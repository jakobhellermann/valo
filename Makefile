CC := clang
CFLAGS := -O2 -g
RUSTFLAGS= --release

out/libvalo.so: crates/valo/Cargo.toml crates/valo/src/lib.rs
	cargo build $(RUSTFLAGS) -Zunstable-options --artifact-dir out -p valo

out/manual:
	@mkdir -p out/manual

out/manual/simple: tests_manual/simple.c out/libvalo.so | out/manual
		$(CC) $(CFLAGS) tests_manual/simple.c -Lout -lvalo -o out/manual/simple
.PHONY: simple
simple: out/manual/simple
	LD_LIBRARY_PATH=out ./out/manual/simple

out/manual/simple-rs: tests_manual/simple.rs out/libvalo.so | out/manual
		rustc tests_manual/simple.rs -Lout -lvalo -o out/manual/simple-rs
.PHONY: simple-rs
simple-rs: out/manual/simple-rs
	LD_LIBRARY_PATH=out ./out/manual/simple-rs

out/manual/sdl2-test: tests_manual/window-sdl2.c out/libvalo.so | out/manual
		$(CC) $(CFLAGS) tests_manual/window-sdl2.c -lSDL2 -o out/manual/sdl2-test
.PHONY: sdl2-test
sdl2-test: out/manual/sdl2-test
	VALO_CHECKPOINT_FRAME=10 VALO_RESTORE_FRAME=50 LD_PRELOAD=out/libvalo.so ./out/manual/sdl2-test

out/manual/x11-test: tests_manual/window-x11.c out/libvalo.so | out/manual
		$(CC) $(CFLAGS) tests_manual/window-x11.c -lX11 -o out/manual/x11-test
.PHONY: x11-test
x11-test: out/manual/x11-test
	VALO_CHECKPOINT_FRAME=10 VALO_RESTORE_FRAME=50 LD_PRELOAD=out/libvalo.so ./out/manual/x11-test

.PHONY: clean
clean:
	rm out -fr

# old
.PHONY: sdl
sdl: out/libvalo.so
	LD_PRELOAD=out/libvalo.so /home/jakob/dev/contrib/sdl3-sample/build/sdl-min

.PHONY: sdl-gdb
sdl-gdb: out/libvalo.so
	LD_PRELOAD=out/libvalo.so gdb /home/jakob/dev/contrib/sdl3-sample/build/sdl-min

.PHONY: breakout
breakout: out/libvalo.so
	LD_PRELOAD=out/libvalo.so /home/jakob/.cache/rust/release/examples/breakout
