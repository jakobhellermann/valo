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

| Variable    | Default               | Effect                                                                        |
| ----------- | --------------------- | ----------------------------------------------------------------------------- |
| `VALO_HOME` | `/tmp/valo-$PID`      | Base directory for all other paths.                                           |
| `VALO_LOG`  | `$VALO_HOME/valo.log` | Path of the log file (truncated on init). Use `/dev/stderr` to log to stderr. |
| `VALO_SOCK` | unset                 | If set, init rendezvous with a client on the path. See `docs/ipc.md`.         |

And for gpu checkpoint support/debugging:

| Variable                   | Default   | Effect                                                                                          |
| -------------------------- | --------- | ----------------------------------------------------------------------------------------------- |
| `VALO_KUMQUAT_BIN`         | `kumquat` | Path to the kumquat executable                                                                  |
| `VALO_GFXSTREAM_ICD`       | unset     | Override the path to the `gfxtream` guest driver icd, defaults to selecting `gfxstream_vk_icd*` |
| `ANDROID_EMU_VK_LOG_CALLS` | `false`   | Include vulkan commands in kumquat log                                                          |
| `GFXSTREAM_LOG_LEVEL`      | `info`    | Log level for gfxstream                                                                         |

# Paths

| Path                            | Usage                                              |
| ------------------------------- | -------------------------------------------------- |
| `$VALO_HOME/valo.log`           | Log output of the valo session                     |
| `$VALO_HOME/valo-struct.jsonl`  | Structured machine readable info about the session |
| `$VALO_HOME/ipc.sock`           | Control socket for the valo library                |
| `$VALO_HOME/slots/$i/meta`      | Savestate pages, threads                           |
| `$VALO_HOME/slots/$i/pages`     | Savestate page data                                |
| `$VALO_HOME/slots/$i/gpu/`      | Savestate gfxstream snapshot                       |
| `$VALO_HOME/gpu-$n/gpu.sock`    | Kumquat communication socket                       |
| `$VALO_HOME/gpu-$n/kumquat.log` | Logs of the host-side kumquat media server         |
