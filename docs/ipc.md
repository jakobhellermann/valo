# IPC design

This document describes the communication between `libvalo.so` (lib) and the `valo` client.

It is heavily inspired by the libTAS communication model.

## Goals

- Synchronous frame control:
  - the lib pauses at frame boundaries and waits for instructions by the client.
- Both lib and client can restart independently and reconnect for iteration speed
- There is only one controlling client at any point in time
- Simple Unix-domain socket

---

## Implementation

The IPC runs over a single unix domain socket. Whichever side starts first creates the socket and listens for connections, for the messages it is irrelevant if which party you are.

Path: `$VALO_SOCK` if set, else `/tmp/valo.sock`.

### Establish

Both lib and client run identical logic on startup and disconnect:

```rs
loop {
    match try_connect(path) {
        Ok(stream)            => return Connector(stream);
        Err(not found)        => /* no socket file yet, be listener */
        Err(refused)          => remove(path); /* stale, be listener  */
        Err(other)            => sleep_backoff(); continue;
    }
    match try_bind_listen(path) {
        Ok(listener)          => {
            let stream = listener.accept()?;
            return Listener(stream);
        }
        Err(addrinuse)        => continue; /* peer just won; retry connect */
        Err(other)            => sleep_backoff(); continue;
    }
}
```

// TOOD(ai-review): AI slop from here on

## Connection lifecycle

### Init

Every session starts with the same init exchange, run while the game is
still in its constructor — no frame runs before it completes:

1. `Hello` handshake: `ClientHello { version }` →
   `LibHello { version, config }`. The lib echoes the config it currently
   has, so any client can learn the session state on connect.
2. Exactly one `SetConfig(config)`, fire-and-forget: the lib applies it
   before anything runs, no ack. The init client is the source of truth
   for runtime config; there is no config env (`VALO_SOCK` /
   `VALO_LOG` are bootstrap-only).
3. With `use_gpu`, `GpuControl`, followed by one byte that carries the
   GPU host's control socket (`SCM_RIGHTS`). The client starts the GPU
   host outside the target's launcher chain; inherited descriptors do not
   survive wrappers such as Steam's, the rendezvous socket does.

The session then starts held with the init connection, and the first
frame boundary's notification drives it:

| init client | first boundary |
|---|---|
| answers `FrameBoundary` with `AdvanceFrames`, keeps gating every boundary (`valo run`) | **gated** |
| queued `Detach` drains (its client may have closed already) | **free**: frames run by default |
| closed | **held** until the next client connects (`valo spawn --paused`) |

Gated and held are the same state machine state (`Release::Held`); the
only difference is whether a client is connected at the boundary.

### Steady state

Commands are processed at frame boundaries (see below). The lib is
strictly synchronous: one command in, one reply out, no pipelining.

### Disconnect and reconnect

The lib always keeps a listener bound on the socket path. When the
controlling client dies (`recv` EOF / `EPIPE`), the game does **not** run
free: it holds at the next frame boundary and the listener accepts the
next client. `valo msg` one-shot clients can drive any held session this
way, and a restarted `valo run` reconnects transparently. A freshly
accepted client gets no `FrameBoundary` notification — it initiates with
a command; a client that is already connected when a boundary is held
gets one notification per held boundary.

A disconnect during a half-completed request tears down the stream. No
reply is delivered for the in-flight request; on reconnect the client
re-issues whatever request it needs.

## Wire format

`postcard` (no_std-compatible serde format) over a length-prefixed framing.

```
struct Frame {
    len: u32,        // little-endian, length of `payload` in bytes
    payload: [u8],   // postcard-encoded Message
}
```

`u32` length is enough for any control message we'll need; truly large
payloads (full memory dumps etc.) flow through the snapshot files, not
the socket.

## Protocol

`crates/valo-ipc/src/messages.rs` defines the message types:

```rust
pub enum ClientToLib {
    Hello(ClientHello),
    AdvanceFrames(u64),   // 0 = query, n = run n frames, then hold
    SetConfig(Config),    // full replacement of the runtime config
    Detach,               // held → free
    Checkpoint(u32),      // snapshot into savestate slot N
    Restore(u32),         // rewind to the snapshot in slot N
    SetInputs(InputState), // full keyboard state for input feeding
    GetState,
    Quit,
    GpuControl,           // init only: the control socket follows on the next byte
    Agent(Vec<u8>),       // opaque request for the in-target agent (held only)
}

pub enum LibToClient {
    Hello(LibHello),      // version + echo of the lib's current config
    FrameBoundary(FrameBoundary), // a boundary is held; give a command
    State(State),         // reply to GetState / AdvanceFrames(0)
    SetConfigDone,
    CheckpointDone,
    RestoreSucceeded(RestoreSucceeded), // the lib is held in the restored frame
    Error(Error),
    Agent(Vec<u8>),       // the agent's opaque reply
}
```

`FrameBoundary` carries the frame number and the game's current top-level X
window as tracked by the lib's window hooks; the client's overlay and hotkeys
attach to that window.

`Config` (`crates/valo-ipc/src/config.rs`): log level,
auto-checkpoint/restore frames, restore cap, GPU orchestration flag,
input mode (`input.uncontrolled_input`).
`SetConfig` replaces it wholesale (the GPU flag is fixed at init —
runtime attempts to change it are refused); the current value is always
readable from the `LibHello` echo.

`SetInputs` carries the full keyboard state. Unless input is uncontrolled,
the lib turns each frame boundary's change into xcb key events on the game's
connections and drops real input events, so only this state reaches the game.
A restore rewinds the fed state with the process.

Tagging: `Hello` is the first message in each direction; version
mismatch refuses the connection. Every other message is an implicit
request/response pair — the protocol is strictly sync from the client's
POV, so no correlation IDs.

## The session state machine

The lib's IPC state is a single `Release` enum (in normal static memory —
it rewinds with a restore, so the timeline's config is deterministic):

```
enum Release {
    Held,        // every boundary waits for a command
    Until(u64),  // frames run until this frame number, then Held again
    Free,        // frames run without stopping
}
```

At every frame boundary:

- `Free`: the frame runs. A connected client gets a ~100 ms command
  window per boundary (checkpoint/state/detach mid-run); a non-blocking
  accept picks up new one-shot clients.
- `Until(n)`, `frame < n`: the frame runs.
- `Until(n)` expired, or `Held`: the boundary is held. With a connected
  client: `FrameBoundary` notification, then a blocking command loop.
  Without: blocking accept for the next client.

Command semantics inside a held boundary:

- `AdvanceFrames(n > 0)` → `Release::Until(frame + n)`, the held frame
  runs. On expiry the session is held again and the connected client is
  notified.
- `AdvanceFrames(0)` → `State` reply, stays held.
- `Detach` → `Release::Free`, the held frame runs.
- `Checkpoint(slot)` → `valo_checkpoint_slot(slot)`. It returns twice:
  on the save return the reply is `CheckpointDone`, when a later restore
  jumps back into this call it is `RestoreSucceeded` with the restored frame, and the session stays held in it. The session lock is held across the
  call; that is consistent through a restore (the lock state rewinds
  with everything else).
- `Restore(slot)` → a slot that cannot restore (missing snapshot, gpu-mode
  slot restriction) is answered with an `Error` and stays held. Otherwise
  `valo_restore_slot(slot)` jumps into the saved context and never returns
  (it aborts on error); the client observes EOF.

Slots persist under `$VALO_HOME/slots/<n>/`.

## Agent channel

A client can talk to code running inside the target — an *agent*, e.g. a
mod injected next to libvalo — without valo knowing what they say. The lib
only moves opaque bytes:

- The agent runs its own thread and serves requests through the C ABI
  exported by libvalo (`crates/valo/src/agent.rs`):
  - `void valo_agent_register(void)` announces the agent. Called before the
    first frame boundary, so the client's first request already waits for
    the agent instead of failing.
  - `ssize_t valo_agent_recv(uint8_t *buf, size_t cap)` blocks until a
    request arrives and returns its length. A request longer than `cap`
    stays queued and only its length is returned, so the agent retries with
    a larger buffer. Returns -1 on the frame thread, which would deadlock
    the boundary it waits for.
  - `int valo_agent_send(const uint8_t *buf, size_t len)` replies to the
    request last received; -1 when no request awaits a reply.
- `ClientToLib::Agent(bytes)` is answered by exactly one
  `LibToClient::Agent(reply)` or `Error`, and only inside a held boundary:
  the frame thread stands still, so the agent sees one consistent frame.
  Outside a held boundary, before an agent registered, or
  without a reply within 10 s of real time, the answer is an `Error`; a late
  reply is refused with -1.
- The channel is plain static memory: a restore rewinds it together with
  the agent thread. Between turns no request is in flight, so a checkpoint
  always captures an idle channel.
- Agent threads wait in real time, but the target's clocks are virtual and
  stand still while a boundary is held: a wait whose timeout is measured
  with them never ends during the hold (Mono's `Thread.Sleep` hangs there).

## Stale socket files

If a previous run crashed without `unlink`, the path persists. The
rendezvous algorithm handles this: the next peer fails `connect()` with
`ECONNREFUSED`, unlinks, then `bind()`s. The lib's init also rebinds the
path for its listener after the rendezvous consumed it.

## File offsets across restores

`valo_restore` rewinds the guest's file offsets to the checkpoint state
(fd_sync). A file *description* shared with another process — most notably an
inherited stdout both redirect into the same log file — is rewound for that
process too: every byte it wrote between checkpoint and restore is silently
overwritten from the rewound offset, and there is no way for the lib to
detect the sharing. Drivers that log alongside their target should give it
piped stdio and relay the output themselves; `O_APPEND` redirection also
writes position-independently.

## Crates

- `crates/valo-ipc/` — wire format, message and config types, framing,
  and the `ClientIPC`/`LibIPC` handles including the client-side
  `hello()` / `init()` exchange helpers. Used by lib, client and
  harness. No game-side socket I/O lives here.
- Lib: `crates/valo/src/ipc.rs` — rendezvous, session state machine,
  command handling. Called from the frame-boundary hook; runs on the
  game's normal thread (not in a signal handler), so async-signal-safety
  is not required.
- Client: `crates/client/src/{main,msg,tui}.rs` connect via `ClientIPC`.
- Harness: acts as the init client for every test (`SetConfig` +
  `Detach`), so tests need no config env.

## Open questions / future work

- **Pause command**: `Free → Held` mid-run is one command away on the
  state machine (`release = Held`); no client surface for it yet.
- **Multiple observers**. A second connection slot for read-only
  observers would need a router on the lib side.
- **Cross-platform**. macOS support would change the rendezvous path
  (`$TMPDIR`) but otherwise the design holds.
