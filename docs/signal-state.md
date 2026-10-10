<!-- TODO(ai-review): generated, not yet audited -->
# Signal state across checkpoint/restore

## sigaltstack registrations

`sigaltstack` is per-thread kernel state (`sas_ss_sp`/`sas_ss_size`), not memory, so the snapshot does not contain it. It still survives restore — including for threads recreated by `recreate_missing` — through the signal-frame roundtrip:

- At suspension the kernel writes the thread's current altstack registration into `uc_stack` of the `SIGXFSZ` signal frame.
- The frame lives on the thread stack, which is part of the snapshot.
- Recreated threads resume via `setcontext` into the saved suspend-handler context and return through `rt_sigreturn`, which re-registers the altstack from the restored frame.

Neither glibc `getcontext`/`setcontext` (no `sigaltstack` call, `uc_stack` is not filled meaningfully on Linux) nor the `clone3` flags carry the registration; the sigreturn path is the only thing that restores it. Locked in by `tests/threads_altstack.c`.

## Known limitation: suspending a thread that runs on its altstack

`valo_park_on_stack` (crates/valo/native/valo_shim.c) checks the current SP against the thread's pthread-stack bounds. A guest thread executing inside an `SA_ONSTACK` handler has its SP on the altstack, far below those bounds, so the check fails and the suspend handler aborts the whole process at checkpoint time.

This is an accepted loud failure, not a silent corruption. A fix requires parking within the altstack bounds and, because signal frames for handlers running on the altstack carry `SS_ONSTACK` (which `rt_sigreturn` skips re-registering), an explicit per-thread `sigaltstack` save in the suspend handler plus re-registration in `park_recreated`.

Not relevant for Hollow Knight: mono's only `SA_ONSTACK` handler is its SIGSEGV crash handler, and a thread inside it is crashing anyway. Guests with routine `SA_ONSTACK` handlers (SIGPROF profiling, GC suspend) would abort on a checkpoint that hits such a window.
