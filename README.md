# valo

Linux snapshot/restore tool heavily inspired by/ported from [libTAS](https://github.com/clementgallet/libTAS/).

Goals:

- learning about linux internals
- building a set of tests with good code coverage for various savestate features
- having a documented IPC interface which is restart-safe and allows for scripting and testability
- possible libTAS / celesteTAS movie file compatibility

# Development

```sh
cargo valo test # run tests in ./tests
cargo valo test --coverage
```

# Environment variables

`libvalo.so` reads the following environment variables at process startup. All
are optional; a missing or unparseable value is treated as the default.

| Variable                | Default         | Effect                                                                          |
| ----------------------- | --------------- | ------------------------------------------------------------------------------- |
| `VALO_LOG`              | `/tmp/valo.log` | Path of the log file (truncated on init). Use `/dev/stderr` to log to stderr.   |
| `VALO_MAX_RESTORES`     | `0` (unlimited) | After this many restores, `valo_restore` calls `exit(0)`. `0` disables the cap. |
| `VALO_CHECKPOINT_FRAME` | `0` (disabled)  | Auto-checkpoint when the frame counter reaches this value. `0` disables.        |
| `VALO_RESTORE_FRAME`    | `0` (disabled)  | Auto-restore when the frame counter reaches this value. `0` disables.           |
| `VALO_SOCK`             | unset           | If set, `init` blocks until a peer rendezvous on the path. See `docs/ipc.md`.   |

The `valo` client also recognises:

| Variable     | Default | Effect                                                                             |
| ------------ | ------- | ---------------------------------------------------------------------------------- |
| `LD_PRELOAD` | unset   | If already set, `valo run` prepends `libvalo.so:` to it instead of overwriting it. |
