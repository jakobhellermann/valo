CC := clang
CFLAGS := -O2 -g
RUSTFLAGS= --release
ENV_VARS := GLIBC_TUNABLES=glibc.pthread.rseq=0
VALO_RS_SOURCES := $(shell find crates/valo/src -name '*.rs')

out/libvalo.so: crates/valo/Cargo.toml $(VALO_RS_SOURCES)
	cargo build $(RUSTFLAGS) -Zunstable-options --artifact-dir out -p valo

out/manual:
	@mkdir -p out/manual

out/manual/simple: tests_manual/simple.c out/libvalo.so | out/manual
		$(CC) $(CFLAGS) tests_manual/simple.c -Lout -lvalo -o out/manual/simple
.PHONY: simple
simple: out/manual/simple
	$(ENV_VARS) LD_LIBRARY_PATH=out ./out/manual/simple

out/manual/simple-rs: tests_manual/simple.rs out/libvalo.so | out/manual
		rustc tests_manual/simple.rs -Lout -lvalo -o out/manual/simple-rs
.PHONY: simple-rs
simple-rs: out/manual/simple-rs
	$(ENV_VARS) LD_LIBRARY_PATH=out ./out/manual/simple-rs

out/manual/sdl2-test: tests_manual/window-sdl2.c out/libvalo.so | out/manual
		$(CC) $(CFLAGS) tests_manual/window-sdl2.c -lSDL2 -o out/manual/sdl2-test
.PHONY: sdl2-test
sdl2-test: out/manual/sdl2-test
	$(ENV_VARS) VALO_CHECKPOINT_FRAME=10 VALO_RESTORE_FRAME=50 LD_PRELOAD=out/libvalo.so ./out/manual/sdl2-test

out/manual/x11-test: tests_manual/window-x11.c out/libvalo.so | out/manual
		$(CC) $(CFLAGS) tests_manual/window-x11.c -lX11 -o out/manual/x11-test
.PHONY: x11-test
x11-test: out/manual/x11-test
	$(ENV_VARS) VALO_CHECKPOINT_FRAME=10 VALO_RESTORE_FRAME=50 LD_PRELOAD=out/libvalo.so ./out/manual/x11-test

.PHONY: clean
clean:
	rm out -fr

# old
.PHONY: sdl
sdl: out/libvalo.so
	$(ENV_VARS) LD_PRELOAD=out/libvalo.so /home/jakob/dev/contrib/sdl3-sample/build/sdl-min

.PHONY: sdl-gdb
sdl-gdb: out/libvalo.so
	$(ENV_VARS) LD_PRELOAD=out/libvalo.so gdb /home/jakob/dev/contrib/sdl3-sample/build/sdl-min

.PHONY: breakout
breakout: out/libvalo.so
	$(ENV_VARS) LD_PRELOAD=out/libvalo.so /home/jakob/.cache/rust/release/examples/breakout
