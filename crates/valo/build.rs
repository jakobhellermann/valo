fn main() {
    println!("cargo::rerun-if-changed=native/valo_shim.c");
    println!("cargo::rerun-if-changed=native/xcbint.h");
    cc::Build::new()
        .file("native/valo_shim.c")
        .compile("valo_shim");
    println!("cargo::rustc-link-lib=dylib=X11-xcb");
    println!("cargo::rustc-link-arg-cdylib=-Wl,--no-as-needed,-lX11,--as-needed");
}
