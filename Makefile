CC := clang
CFLAGS := -O2 -g
RUSTFLAGS= --release

out/libvalo.so: crates/valo/Cargo.toml crates/valo/src/lib.rs
	cargo build $(RUSTFLAGS) -Zunstable-options --artifact-dir out

out/simple: tests/simple.c out/libvalo.so
		$(CC) $(CFLAGS) tests/simple.c -Lout -lvalo -o out/simple

out/simple-rs: tests/simple.rs out/libvalo.so
		rustc tests/simple.rs -Lout -lvalo -o out/simple-rs

.PHONY: simple
simple: out/simple
	LD_LIBRARY_PATH=out ./out/simple

.PHONY: simple-rs
simple-rs: out/simple-rs
	LD_LIBRARY_PATH=out ./out/simple-rs
