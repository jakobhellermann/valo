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

- **Open**: rendezvous succeeds, both ends do a `Hello` handshake (see
  Protocol §). On version mismatch, both ends close cleanly and re-enter
  the rendezvous loop.
- **Steady state**: client sends commands, lib responds at frame
  boundaries. The lib blocks on `recv()` at each frame boundary until the
  client sends an "advance" message.
- **Disconnect**: any `recv`/`send` returning EOF or `EPIPE` ends the
  session. Both ends loop back to rendezvous. The lib does _not_ terminate
  the game — it continues whatever the last advance command implied (e.g.
  keep running freely until the next frame boundary, then block waiting for
  a new client).
- **Reconnect**: state on each side is independent. On reconnect the client
  must `GetState` to learn the current frame counter, savestate slot, etc.
  The lib treats the new client as fresh and assumes nothing about its
  prior state.

A disconnect during a half-completed request (e.g. lib sent a partial reply,
client died) is handled by simply tearing down the stream. No reply is
delivered for the in-flight request; on reconnect the client issues
whatever request it needs anew.

## Wire format

`postcard` (no_std-compatible serde format) over a length-prefixed framing.

```
struct Frame {
    len: u32,        // little-endian, length of `payload` in bytes
    payload: [u8],   // postcard-encoded Message
}
```

`u32` length is enough for any control message we'll need; truly large
payloads (full memory dumps etc.) flow through the existing
`/tmp/valo-snapshot-*` files, not the socket.

Endianness is little-endian regardless of host (host is x86_64 anyway, but
this keeps the format stable if we ever cross-compile).

## Protocol

`crates/valo-ipc/src/lib.rs` defines the message types. Sketch:

```rust
#[derive(Serialize, Deserialize)]
pub struct Hello {
    pub version: u32,
    pub role: Role,    // Lib | Client, informational
}

#[derive(Serialize, Deserialize)]
pub enum ClientToLib {
    Hello(Hello),
    Advance,                  // run until next frame boundary
    Checkpoint,
    Restore,
    GetState,
    Quit,
    // Future: SetInputs(...), Lua eval, etc.
}

#[derive(Serialize, Deserialize)]
pub enum LibToClient {
    Hello(Hello),
    FrameBoundary { frame: u64 },
    State { frame: u64, /* ... */ },
    CheckpointDone,
    RestoreDone,
    Error(ErrorKind),
}
```

Tagging:

- `Hello` is the first message in each direction. Both sides exchange
  versions and refuse the connection if they disagree. Future: include a
  capability bitmap so we can roll out new messages without bumping the
  global version.
- Every other message is part of an implicit request/response pair. We do
  not need correlation IDs because the protocol is strictly sync from the
  client's POV: client sends one command, blocks for the matching response,
  reads it, repeats. If async overlapping commands become useful, add a
  `seq: u32` to both sides.

## Pacing and the frame boundary

The lib enters the IPC loop at every frame boundary (the same hook valo
already uses to count frames):

```
on frame_boundary():
    send(LibToClient::FrameBoundary { frame: N })
    loop:
        msg = recv() // blocks
        match msg:
            Advance     -> break
            Checkpoint  -> do_checkpoint(); send(CheckpointDone)
            Restore     -> do_restore();    send(RestoreDone)
            GetState    -> send(State { ... })
            Quit        -> exit(0)
```

This is the libTAS `frameBoundary()` model. Synchronous, single-threaded,
deterministic.

The client, by contrast, runs an event loop at whatever rate it wants
(>144 fps is fine; it spends most of its time blocked in `recv` waiting
for `FrameBoundary`).

## Stale socket files

If a previous run crashed without `unlink`, the path persists. The
rendezvous algorithm above handles this: the next peer fails
`connect()` with `ECONNREFUSED`, unlinks, then `bind()`s. Best-effort
cleanup on graceful exit (`Drop` impl on the listener wrapper) keeps the
common case tidy.

## Crates

- `crates/valo-ipc/` — wire-format definitions: `Message` enums, serde
  derives via postcard, framing helpers (`encode_frame` / `decode_frame`).
  Used by both lib and client. No socket I/O lives here — this crate is
  just the wire format, kept narrow so it stays testable in isolation.
  Uses `std`; the frame-boundary recv runs on the game's normal thread
  (not in a signal handler), so async-signal-safety is not required.
- Lib: socket I/O lives in `crates/valo/src/ipc.rs` (TBD), called from the
  frame-boundary hook. Uses `rustix` to stay consistent with the rest of
  the lib.
- Client: socket I/O lives in `crates/client/src/ipc.rs` (TBD). Standard
  `std::os::unix::net` is fine; the client has no `no_alloc` constraint.

## Open questions / future work

- **Multiple observers**. A second connection slot for read-only observers
  (e.g. an FPS overlay or a Lua scripting REPL) would need a router on the
  lib side. Doable but not now.
- **Full-duplex async commands**. Some commands (e.g. asynchronous "stop
  the game on next frame") would benefit from out-of-band delivery rather
  than waiting for the next frame boundary. Adding a non-blocking
  `recv()` between frames covers most of this without a full async model.
- **Cross-platform**. macOS support would change the rendezvous path
  (`$TMPDIR`) but otherwise the design holds.
