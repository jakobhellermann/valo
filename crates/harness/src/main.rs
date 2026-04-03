use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;
use std::{fs, thread};
use wait_timeout::ChildExt;

#[derive(Parser)]
#[command(bin_name = "cargo valo")]
struct Args {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the test harness against tests/*.c
    Test(TestArgs),
}

#[derive(Parser)]
struct TestArgs {
    /// Regenerate snapshot files
    #[arg(long)]
    update: bool,
    /// Show stderr even for passing tests
    #[arg(long)]
    nocapture: bool,
    /// Filter tests by name
    filter: Option<String>,
}

const SAFETY_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Default)]
struct Config {
    max_restores: u32,
    link_mode: LinkMode,
    link_flags: Vec<String>,
}

#[derive(Default)]
enum LinkMode {
    /// Compile with -Lout -lvalo, run with LD_LIBRARY_PATH=out
    #[default]
    Link,
    /// Compile standalone, run with LD_PRELOAD=out/libvalo.so
    Preload,
}

struct Output {
    stdout: String,
    stderr: String,
}

/// Parse `//# key: value` directives from the source file.
fn parse_config(source: &Path) -> Config {
    let mut config = Config {
        max_restores: 1,
        ..Config::default()
    };

    let text = fs::read_to_string(source).unwrap_or_default();
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("//#") {
            let rest = rest.trim();
            if let Some((key, value)) = rest.split_once(':') {
                let key = key.trim();
                let value = value.trim();
                match key {
                    "max_restores" => {
                        config.max_restores = value.parse().unwrap_or(1);
                    }
                    "link_mode" => {
                        config.link_mode = match value {
                            "preload" => LinkMode::Preload,
                            _ => LinkMode::Link,
                        };
                    }
                    "link_flags" => {
                        config
                            .link_flags
                            .extend(value.split_whitespace().map(String::from));
                    }
                    _ => {}
                }
            }
        }
    }

    config
}

fn discover_tests(tests_dir: &Path) -> Vec<PathBuf> {
    let mut tests: Vec<PathBuf> = fs::read_dir(tests_dir)
        .expect("cannot read tests/")
        .filter_map(|e| {
            let e = e.ok()?;
            let path = e.path();
            if path.extension().is_some_and(|e| e == "c") {
                Some(path)
            } else {
                None
            }
        })
        .collect();
    tests.sort();
    tests
}

fn compile(
    name: &str,
    source: &Path,
    config: &Config,
    tests_dir: &Path,
    out_dir: &Path,
    tests_out_dir: &Path,
) -> Result<PathBuf> {
    let binary = tests_out_dir.join(name);

    let mut cmd = Command::new("clang");
    cmd.args(["-O2", "-g"]);
    cmd.arg(format!("-I{}", tests_dir.display()));
    cmd.arg(source);

    match config.link_mode {
        LinkMode::Link => {
            cmd.arg("-L").arg(out_dir);
            cmd.arg("-lvalo");
        }
        LinkMode::Preload => {}
    }

    cmd.args(&config.link_flags);
    cmd.arg("-o").arg(&binary);

    let output = cmd.output().context("failed to run clang")?;
    if !output.status.success() {
        bail!(
            "compilation failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(binary)
}

fn run(
    binary: &Path,
    config: &Config,
    out_dir: &Path,
    nocapture: bool,
    log_path: &Path,
) -> Result<Output> {
    let mut cmd = Command::new(binary);
    match config.link_mode {
        LinkMode::Link => {
            cmd.env("LD_LIBRARY_PATH", out_dir);
        }
        LinkMode::Preload => {
            cmd.env("LD_PRELOAD", out_dir.join("libvalo.so"));
        }
    }
    cmd.env("VALO_MAX_RESTORES", config.max_restores.to_string());
    cmd.env("VALO_LOG", log_path);

    if nocapture {
        cmd.stdout(Stdio::inherit());
        cmd.stderr(Stdio::inherit());
    } else {
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
    }

    let mut child = cmd.spawn().context("failed to spawn test")?;

    let stdout_thread = child.stdout.take().map(|child_stdout| {
        thread::spawn(move || {
            let mut buf = String::new();
            let mut r = child_stdout;
            let _ = r.read_to_string(&mut buf);
            buf
        })
    });
    let stderr_thread = child.stderr.take().map(|child_stderr| {
        thread::spawn(move || {
            let mut buf = String::new();
            let mut r = child_stderr;
            let _ = r.read_to_string(&mut buf);
            buf
        })
    });

    let status = child.wait_timeout(SAFETY_TIMEOUT)?;

    let status = match status {
        Some(s) => s,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            let stderr = stderr_thread
                .and_then(|t| t.join().ok())
                .unwrap_or_default();
            bail!("timed out\nstderr:\n{stderr}");
        }
    };

    let stdout = stdout_thread
        .and_then(|t| t.join().ok())
        .unwrap_or_default();
    let stderr = stderr_thread
        .and_then(|t| t.join().ok())
        .unwrap_or_default();

    if !status.success() {
        bail!("exit status: {status}\nstdout:\n{stdout}\nstderr:\n{stderr}");
    }

    Ok(Output { stdout, stderr })
}

/// Returns Ok(stderr) on success.
fn run_test(
    source: &Path,
    tests_dir: &Path,
    out_dir: &Path,
    tests_out_dir: &Path,
    logs_dir: &Path,
    update: bool,
    nocapture: bool,
) -> Result<String> {
    let name = source.file_stem().unwrap().to_str().unwrap();
    let config = parse_config(source);
    let snapshot_path = tests_dir.join("snapshots").join(format!("{name}.txt"));
    let log_path = logs_dir.join(format!("{name}.log"));

    let binary = compile(name, source, &config, tests_dir, out_dir, tests_out_dir)?;
    let output = run(&binary, &config, out_dir, nocapture, &log_path)?;

    if update {
        fs::write(&snapshot_path, &output.stdout)?;
        return Ok(output.stderr);
    }

    if nocapture {
        return Ok(output.stderr);
    }

    let expected = fs::read_to_string(&snapshot_path)
        .context("missing snapshot (run with --update to generate)")?;

    if output.stdout != expected {
        let actual_lines: Vec<&str> = output.stdout.lines().collect();
        let expected_lines: Vec<&str> = expected.lines().collect();

        let mut msg = String::new();
        for (i, (a, e)) in actual_lines.iter().zip(expected_lines.iter()).enumerate() {
            if a != e {
                msg = format!("line {}: expected {:?}, got {:?}", i + 1, e, a);
                break;
            }
        }
        if msg.is_empty() {
            msg = format!(
                "expected {} lines, got {}",
                expected_lines.len(),
                actual_lines.len(),
            );
        }
        if !output.stderr.is_empty() {
            msg.push_str(&format!("\nstderr:\n{}", output.stderr));
        }
        msg.push_str("\n  run with --update to regenerate snapshot");
        bail!("{msg}");
    }

    Ok(output.stderr)
}

fn main() {
    // Disable core dumps for child processes (inherited across fork)
    let rlimit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    unsafe { libc::setrlimit(libc::RLIMIT_CORE, &rlimit) };

    let args = Args::parse();

    match args.command {
        Cmd::Test(test_args) => run_test_command(test_args),
    }
}

fn build_libvalo(root: &Path) -> Result<()> {
    let status = Command::new("cargo")
        .args([
            "build",
            "--release",
            "-Zunstable-options",
            "--artifact-dir",
            "out",
            "-p",
            "valo",
        ])
        .current_dir(root)
        .status()
        .context("failed to spawn cargo build")?;
    if !status.success() {
        bail!("cargo build exited with {status}");
    }
    Ok(())
}

fn run_test_command(args: TestArgs) {
    let root = find_workspace_root();
    let out_dir = root.join("out");
    let tests_out_dir = out_dir.join("tests");
    let tests_dir = root.join("tests");

    let lib = out_dir.join("libvalo.so");
    if let Err(e) = build_libvalo(&root) {
        eprintln!("ERROR: failed to build libvalo.so: {e}");
        std::process::exit(1);
    }
    if !lib.exists() {
        eprintln!("ERROR: out/libvalo.so not found after build.");
        std::process::exit(1);
    }
    if let Err(e) = fs::create_dir_all(&tests_out_dir) {
        eprintln!("ERROR: failed to create {}: {e}", tests_out_dir.display());
        std::process::exit(1);
    }

    let logs_dir = root.join("out").join("test_logs");
    if let Err(e) = fs::create_dir_all(&logs_dir) {
        eprintln!("ERROR: failed to create {}: {e}", logs_dir.display());
        std::process::exit(1);
    }

    let tests = discover_tests(&tests_dir);
    let mut passed = 0;
    let mut failed = 0;

    for source in &tests {
        let name = source.file_stem().unwrap().to_str().unwrap();
        if let Some(ref f) = args.filter
            && !name.contains(f.as_str())
        {
            continue;
        }

        eprint!("  {:<30}", name);
        let start = std::time::Instant::now();
        match run_test(
            source,
            &tests_dir,
            &out_dir,
            &tests_out_dir,
            &logs_dir,
            args.update,
            args.nocapture,
        ) {
            Ok(_stderr) => {
                let elapsed = start.elapsed();
                if args.update {
                    eprintln!("UPDATED ({elapsed:.1?})");
                } else {
                    eprintln!("PASS    ({elapsed:.1?})");
                }
                passed += 1;
            }
            Err(e) => {
                let elapsed = start.elapsed();
                eprintln!("FAIL    ({elapsed:.1?})");
                eprintln!("    {e}");
                failed += 1;
            }
        }
    }

    eprintln!("\n{passed} passed, {failed} failed");
    std::process::exit(if failed > 0 { 1 } else { 0 });
}

fn find_workspace_root() -> PathBuf {
    let mut dir = std::env::current_dir().expect("no cwd");
    loop {
        if dir.join("Cargo.toml").exists() && dir.join("crates").exists() {
            return dir;
        }
        if !dir.pop() {
            panic!("could not find workspace root");
        }
    }
}
