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
