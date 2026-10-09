// TODO(ai-review): not yet audited. less important for correctness than the rest.
//! Minimal ratatui frontend for the valo client.
//!
//! Three panes top-to-bottom: Status, Events, Hints. Three input sources
//! feed an mpsc channel: keyboard (crossterm), IPC `recv` from the lib,
//! and a `tail -f` thread on `VALO_LOG`. Pause is implemented client-side:
//! when paused, the client withholds `Advance`, so the lib blocks at the
//! frame boundary.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Seek, SeekFrom, Stdout};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use crossterm::event::{self, Event as CtEvent, KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use valo_ipc::ClientIPC;
use valo_ipc::config::Config;
use valo_ipc::messages::{ClientToLib, FrameBoundary, LibToClient};

const MAX_EVENTS: usize = 1000;

struct KeyboardInput {
    suspended: AtomicBool,
    reader_lock: Mutex<()>,
}

#[derive(Debug)]
enum AppEvent {
    Ipc(LibToClient),
    IpcEof,
    IpcError(String),
    Key(KeyEvent),
    LogLine(String),
}

struct App {
    target_pid: u32,
    paused: bool,
    /// If a `FrameBoundary` arrived while paused, store its frame so we
    /// can ack it the moment the user unpauses.
    pending_advance: Option<u64>,
    /// Queued one-shot command to send at the next FrameBoundary, before
    /// `Advance`. Only one slot — overwriting it is fine for now.
    queued_command: Option<ClientToLib>,
    last_frame: Option<u64>,
    last_savestate: Option<u64>,
    last_error: Option<String>,
    events: VecDeque<EventLine>,
    quitting: bool,
}

struct EventLine {
    ts: Instant,
    kind: EventKind,
    text: String,
}

#[derive(Clone, Copy)]
enum EventKind {
    Recv,
    Send,
    User,
    Log,
    Error,
}

impl App {
    fn new(target_pid: u32) -> Self {
        Self {
            target_pid,
            paused: false,
            pending_advance: None,
            queued_command: None,
            last_frame: None,
            last_savestate: None,
            last_error: None,
            events: VecDeque::with_capacity(MAX_EVENTS),
            quitting: false,
        }
    }

    fn push_event(&mut self, kind: EventKind, text: impl Into<String>) {
        if self.events.len() == MAX_EVENTS {
            self.events.pop_front();
        }
        self.events.push_back(EventLine {
            ts: Instant::now(),
            kind,
            text: text.into(),
        });
    }
}

pub fn run(sock: &Path, log_path: Option<&Path>, target_pid: u32, config: Config) -> Result<()> {
    if !std::io::IsTerminal::is_terminal(&std::io::stdout()) {
        bail!("--tui requires stdout to be a tty");
    }

    let mut conn = ClientIPC::establish(sock).context("ipc establish")?;
    conn.init(&config)?;

    let (tx, rx) = mpsc::channel::<AppEvent>();

    let keyboard_input = Arc::new(KeyboardInput {
        suspended: AtomicBool::new(false),
        reader_lock: Mutex::new(()),
    });
    spawn_keyboard_thread(tx.clone(), Arc::clone(&keyboard_input));
    if let Some(p) = log_path {
        spawn_log_tail_thread(tx.clone(), p.to_path_buf());
    }
    spawn_ipc_reader_thread(tx.clone(), conn.try_clone_for_reader()?);

    let mut writer = conn;
    let mut terminal = setup_terminal().context("setup terminal")?;

    // Make sure raw mode is restored even on panic.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        main_loop(&mut terminal, &mut writer, rx, target_pid, &keyboard_input)
    }));
    let _ = restore_terminal(&mut terminal);
    match result {
        Ok(r) => r,
        Err(payload) => std::panic::resume_unwind(payload),
    }
}

fn main_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    conn: &mut ClientIPC,
    rx: Receiver<AppEvent>,
    target_pid: u32,
    keyboard_input: &KeyboardInput,
) -> Result<()> {
    let mut app = App::new(target_pid);
    app.push_event(EventKind::User, "ipc connected; running");

    draw(terminal, &app)?;

    while !app.quitting {
        let ev = match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(e) => Some(e),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };

        if let Some(ev) = ev {
            if let AppEvent::Key(KeyEvent {
                code: KeyCode::Char('g'),
                ..
            }) = ev
            {
                let pid = app.target_pid;
                app.push_event(EventKind::User, format!("gdb attaching to pid {pid}"));
                if let Err(e) = attach_gdb(terminal, keyboard_input, pid) {
                    let message = format!("gdb: {e:#}");
                    app.last_error = Some(message.clone());
                    app.push_event(EventKind::Error, message);
                }
            } else {
                handle_event(&mut app, conn, ev)?;
            }
        }

        draw(terminal, &app)?;
    }

    Ok(())
}

fn handle_event(app: &mut App, conn: &mut ClientIPC, ev: AppEvent) -> Result<()> {
    match ev {
        AppEvent::Ipc(LibToClient::FrameBoundary(FrameBoundary { frame })) => {
            app.last_frame = Some(frame);
            // Don't push every FrameBoundary into the events panel; the
            // status pane shows the current frame already.

            // Send any queued one-shot command before deciding on Advance.
            if let Some(cmd) = app.queued_command.take() {
                send(conn, app, cmd)?;
                // The command (Checkpoint/Restore) responds with its own
                // Done/State message; we still need to Advance afterwards.
                // Wait for the response in the loop, which will arrive
                // before the next FrameBoundary.
            }

            if app.paused {
                app.pending_advance = Some(frame);
            } else {
                send(conn, app, ClientToLib::AdvanceFrames(1))?;
            }
        }
        AppEvent::Ipc(LibToClient::CheckpointDone) => {
            app.last_savestate = app.last_frame;
            app.push_event(EventKind::Recv, "CheckpointDone");
            // Now release the frame.
            release_frame_if_pending(conn, app)?;
        }
        AppEvent::Ipc(LibToClient::RestoreSucceeded) => {
            // Lib's checkpoint call site got setcontext'd back here from
            // a successful Restore. The frame counter is whatever was
            // saved at checkpoint time; the next FrameBoundary will use
            // that older number.
            app.push_event(EventKind::Recv, "RestoreSucceeded");
            release_frame_if_pending(conn, app)?;
        }
        AppEvent::Ipc(LibToClient::State(s)) => {
            app.push_event(EventKind::Recv, format!("State {{ frame: {} }}", s.frame));
        }
        AppEvent::Ipc(LibToClient::Error(e)) => {
            app.last_error = Some(e.message.clone());
            app.push_event(EventKind::Error, format!("Error: {}", e.message));
        }
        AppEvent::Ipc(LibToClient::SetConfigDone) => {
            app.push_event(EventKind::Recv, "SetConfigDone");
        }
        AppEvent::Ipc(LibToClient::Hello(_)) => {
            app.push_event(EventKind::Recv, "Hello (unexpected mid-session)");
        }
        AppEvent::IpcEof => {
            app.push_event(EventKind::User, "lib hung up");
            app.quitting = true;
        }
        AppEvent::IpcError(e) => {
            app.push_event(EventKind::Error, format!("ipc: {e}"));
            app.quitting = true;
        }
        AppEvent::Key(k) => handle_key(app, conn, k)?,
        AppEvent::LogLine(s) => {
            app.push_event(EventKind::Log, s);
        }
    }
    Ok(())
}

fn handle_key(app: &mut App, conn: &mut ClientIPC, k: KeyEvent) -> Result<()> {
    match (k.code, k.modifiers) {
        (KeyCode::Char('q'), _) | (KeyCode::Esc, _) => {
            app.push_event(EventKind::User, "quit");
            // Best-effort: tell the lib too.
            let _ = conn.send(&ClientToLib::Quit);
            app.quitting = true;
        }
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
            app.push_event(EventKind::User, "ctrl-c");
            let _ = conn.send(&ClientToLib::Quit);
            app.quitting = true;
        }
        (KeyCode::Char(' '), _) => {
            app.paused = !app.paused;
            app.push_event(
                EventKind::User,
                if app.paused {
                    "paused".to_string()
                } else {
                    "running".to_string()
                },
            );
            if !app.paused {
                release_frame_if_pending(conn, app)?;
            }
        }
        (KeyCode::Char('s'), _) => {
            app.push_event(EventKind::User, "snapshot queued");
            app.queued_command = Some(ClientToLib::Checkpoint);
        }
        (KeyCode::Char('r'), _) => {
            app.push_event(EventKind::User, "restore queued");
            app.queued_command = Some(ClientToLib::Restore);
        }
        _ => {}
    }
    Ok(())
}

fn release_frame_if_pending(conn: &mut ClientIPC, app: &mut App) -> Result<()> {
    if !app.paused
        && let Some(_frame) = app.pending_advance.take()
    {
        send(conn, app, ClientToLib::AdvanceFrames(1))?;
    }
    Ok(())
}

fn send(conn: &mut ClientIPC, app: &mut App, msg: ClientToLib) -> Result<()> {
    // `Advance` fires once per frame — too noisy for the events panel.
    // Anything else is rare and worth showing.
    if !matches!(msg, ClientToLib::AdvanceFrames(_)) {
        app.push_event(EventKind::Send, format!("{msg:?}"));
    }
    conn.send(&msg).context("ipc send")?;
    Ok(())
}

fn draw(terminal: &mut Terminal<CrosstermBackend<Stdout>>, app: &App) -> Result<()> {
    terminal.draw(|frame| {
        let area = frame.area();
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(3),
                Constraint::Length(3),
            ])
            .split(area);

        // Status pane.
        let state_str = if app.paused { "PAUSED" } else { "RUNNING" };
        let frame_str = app
            .last_frame
            .map(|f| f.to_string())
            .unwrap_or_else(|| "-".to_string());
        let snap_str = app
            .last_savestate
            .map(|f| f.to_string())
            .unwrap_or_else(|| "-".to_string());
        let mut status_lines = vec![Line::from(vec![
            Span::styled("state: ", Style::default().add_modifier(Modifier::DIM)),
            Span::styled(state_str, Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("   "),
            Span::styled("frame: ", Style::default().add_modifier(Modifier::DIM)),
            Span::raw(frame_str),
            Span::raw("   "),
            Span::styled("savestate: ", Style::default().add_modifier(Modifier::DIM)),
            Span::raw(snap_str),
        ])];
        if let Some(err) = &app.last_error {
            status_lines.push(Line::from(Span::raw(format!("error: {err}"))));
        }
        frame.render_widget(
            Paragraph::new(status_lines)
                .block(Block::default().borders(Borders::ALL).title("Status")),
            layout[0],
        );

        // Events pane.
        let height = layout[1].height.saturating_sub(2) as usize;
        let take = app.events.len().min(height.max(1));
        let start = app.events.len().saturating_sub(take);
        let now = Instant::now();
        let lines: Vec<Line> = app
            .events
            .iter()
            .skip(start)
            .map(|e| {
                let secs_ago = now.saturating_duration_since(e.ts).as_secs_f32();
                let prefix = match e.kind {
                    EventKind::Recv => "<-",
                    EventKind::Send => "->",
                    EventKind::User => " *",
                    EventKind::Log => " L",
                    EventKind::Error => " !",
                };
                Line::from(vec![
                    Span::styled(
                        format!("{secs_ago:>5.1}s  "),
                        Style::default().add_modifier(Modifier::DIM),
                    ),
                    Span::raw(format!("{prefix}  ")),
                    Span::raw(e.text.clone()),
                ])
            })
            .collect();
        frame.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .block(Block::default().borders(Borders::ALL).title("Events")),
            layout[1],
        );

        // Hints pane.
        let hints = Line::from(vec![
            Span::styled("[space]", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" play/pause   "),
            Span::styled("[s]", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" snapshot   "),
            Span::styled("[r]", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" restore   "),
            Span::styled("[g]", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" gdb   "),
            Span::styled("[q]", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" quit"),
        ]);
        frame.render_widget(
            Paragraph::new(hints).block(Block::default().borders(Borders::ALL)),
            layout[2],
        );
    })?;
    Ok(())
}

fn spawn_keyboard_thread(tx: Sender<AppEvent>, input: Arc<KeyboardInput>) {
    thread::spawn(move || {
        loop {
            if input.suspended.load(Ordering::Acquire) {
                thread::sleep(Duration::from_millis(10));
                continue;
            }
            let _reader = input.reader_lock.lock().unwrap();
            if input.suspended.load(Ordering::Acquire) {
                continue;
            }
            // 250ms poll keeps the loop responsive to channel-disconnect.
            match event::poll(Duration::from_millis(250)) {
                Ok(true) => {}
                Ok(false) => continue,
                Err(_) => break,
            }
            match event::read() {
                Ok(CtEvent::Key(k)) => {
                    if tx.send(AppEvent::Key(k)).is_err() {
                        break;
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
    });
}

fn spawn_log_tail_thread(tx: Sender<AppEvent>, path: std::path::PathBuf) {
    thread::spawn(move || {
        // Wait for the file to exist (lib creates it shortly after start).
        let mut file = loop {
            match File::open(&path) {
                Ok(f) => break f,
                Err(_) => {
                    thread::sleep(Duration::from_millis(50));
                }
            }
        };
        // Start at end so we don't replay old logs.
        let _ = file.seek(SeekFrom::End(0));
        let mut reader = BufReader::new(file);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => {
                    thread::sleep(Duration::from_millis(100));
                }
                Ok(_) => {
                    let trimmed = line.trim_end_matches(['\n', '\r']);
                    // Filter out DEBUG/TRACE noise; only forward
                    // INFO/WARN/ERROR lines to the TUI events panel.
                    // Lines look like "[pid=N category:LEVEL] msg".
                    let level_is_noisy = trimmed.contains(":DEBUG]") || trimmed.contains(":TRACE]");
                    if level_is_noisy {
                        continue;
                    }
                    if tx.send(AppEvent::LogLine(trimmed.to_string())).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
}

fn spawn_ipc_reader_thread(tx: Sender<AppEvent>, mut reader: ClientIPC) {
    thread::spawn(move || {
        loop {
            match reader.recv() {
                Ok(Some(msg)) => {
                    if tx.send(AppEvent::Ipc(msg)).is_err() {
                        break;
                    }
                }
                Ok(None) => {
                    let _ = tx.send(AppEvent::IpcEof);
                    break;
                }
                Err(e) => {
                    let _ = tx.send(AppEvent::IpcError(e.to_string()));
                    break;
                }
            }
        }
    });
}

fn attach_gdb(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    input: &KeyboardInput,
    pid: u32,
) -> Result<()> {
    input.suspended.store(true, Ordering::Release);
    // Wait for any in-progress poll/read before GDB takes stdin.
    let _reader = input.reader_lock.lock().unwrap();

    let result = restore_terminal(terminal)
        .context("suspend TUI")
        .and_then(|_| run_gdb(pid));

    let restarted = setup_terminal().context("resume TUI");
    input.suspended.store(false, Ordering::Release);
    *terminal = restarted?;
    result
}

fn run_gdb(pid: u32) -> Result<()> {
    let client_group = rustix::process::getpgrp().as_raw_pid();
    let mut command = Command::new("gdb");
    command.arg("-p").arg(pid.to_string()).process_group(0);
    let mut debugger = command.spawn().context("start gdb")?;
    let debugger_group = debugger.id() as libc::pid_t;

    if let Err(e) = foreground_process_group(debugger_group) {
        let _ = debugger.kill();
        let _ = debugger.wait();
        return Err(e).context("give terminal to gdb");
    }
    // GDB may have stopped on SIGTTIN while it was briefly in the background.
    // SAFETY: the negative PID addresses the new process group created above.
    unsafe { libc::kill(-debugger_group, libc::SIGCONT) };

    let status = debugger.wait();
    foreground_process_group(client_group).context("return terminal to TUI")?;
    let status = status.context("wait for gdb")?;
    if !status.success() {
        bail!("gdb exited with {status}");
    }
    Ok(())
}

fn foreground_process_group(group: libc::pid_t) -> Result<()> {
    // A background process must block SIGTTOU while reclaiming its terminal.
    // SAFETY: sigset_t values are initialized before use and stdin is the
    // controlling terminal required by the TUI.
    unsafe {
        let mut blocked: libc::sigset_t = std::mem::zeroed();
        let mut previous: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut blocked);
        libc::sigaddset(&mut blocked, libc::SIGTTOU);
        let error = libc::pthread_sigmask(libc::SIG_BLOCK, &blocked, &mut previous);
        if error != 0 {
            return Err(io::Error::from_raw_os_error(error).into());
        }
        let rc = libc::tcsetpgrp(libc::STDIN_FILENO, group);
        let tty_error = io::Error::last_os_error();
        let restore_error =
            libc::pthread_sigmask(libc::SIG_SETMASK, &previous, std::ptr::null_mut());
        if restore_error != 0 {
            return Err(io::Error::from_raw_os_error(restore_error).into());
        }
        if rc != 0 {
            return Err(tty_error.into());
        }
    }
    Ok(())
}

fn setup_terminal() -> io::Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    Terminal::new(backend)
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}
