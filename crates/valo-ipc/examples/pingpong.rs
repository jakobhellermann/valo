//! Manual ipc test for the establish rendezvous and message round-trip.
//!
//! Run two terminals:
//!     cargo run -p valo-ipc --example pingpong -- --role client /tmp/valo-pp.sock
//!     cargo run -p valo-ipc --example pingpong -- --role lib    /tmp/valo-pp.sock
//!
//! Either side can start first.
//! Each peer exchanges a Hello, then the "lib" side sends three FrameBoundary
//! messages and waits for an Advance after each one. Then both exit.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use valo_ipc::messages::{ClientToLib, FrameBoundary, Hello, LibToClient};
use valo_ipc::{ClientIPC, LibIPC};

#[derive(Parser)]
#[command(about = "valo-ipc rendezvous smoke test")]
struct Args {
    /// Which side of the protocol to play.
    #[arg(long)]
    role: CliRole,

    /// Unix socket path to rendezvous on.
    path: PathBuf,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum CliRole {
    Lib,
    Client,
}

fn main() -> Result<()> {
    let args = Args::parse();

    eprintln!("[{:?}] establishing on {}", args.role, args.path.display());
    match args.role {
        CliRole::Lib => {
            let conn = LibIPC::establish(&args.path).context("establish")?;
            eprintln!("[Lib] connected");
            run_lib(conn)
        }
        CliRole::Client => {
            let conn = ClientIPC::establish(&args.path).context("establish")?;
            eprintln!("[Client] connected");
            run_client(conn)
        }
    }
}

fn run_lib(mut conn: LibIPC) -> Result<()> {
    conn.send(&LibToClient::Hello(Hello::LIB))
        .context("send hello")?;
    let peer = conn
        .recv()
        .context("recv hello")?
        .context("client closed before hello")?;
    eprintln!("[Lib] got hello: {peer:?}");

    for frame in 0..3 {
        conn.send(&LibToClient::FrameBoundary(FrameBoundary { frame }))
            .with_context(|| format!("send frame {frame}"))?;
        eprintln!("[Lib] sent frame {frame}, waiting for client");
        let cmd = conn
            .recv()
            .with_context(|| format!("recv after frame {frame}"))?
            .context("client closed mid-session")?;
        eprintln!("[Lib] client said {cmd:?}");
        match cmd {
            ClientToLib::Advance => {}
            ClientToLib::Quit => break,
            other => eprintln!("[Lib] ignoring {other:?}"),
        }
    }
    eprintln!("[Lib] done");
    Ok(())
}

fn run_client(mut conn: ClientIPC) -> Result<()> {
    conn.send(&ClientToLib::Hello(Hello::CLIENT))
        .context("send hello")?;
    let peer = conn
        .recv()
        .context("recv hello")?
        .context("lib closed before hello")?;
    eprintln!("[Client] got hello: {peer:?}");

    loop {
        let msg = match conn.recv().context("recv")? {
            Some(m) => m,
            None => {
                eprintln!("[Client] lib hung up");
                return Ok(());
            }
        };
        match msg {
            LibToClient::FrameBoundary(FrameBoundary { frame }) => {
                eprintln!("[Client] frame {frame}, advancing in 250ms");
                std::thread::sleep(Duration::from_millis(250));
                conn.send(&ClientToLib::Advance).context("send advance")?;
            }
            other => bail!("[Client] unexpected {other:?}"),
        }
    }
}
