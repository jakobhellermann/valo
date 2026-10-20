fn main() {
    println!("cargo::rerun-if-changed=native/valo_shim.c");
    println!("cargo::rerun-if-changed=native/xcbint.h");
    cc::Build::new()
        .file("native/valo_shim.c")
        .opt_level(2)
        .compile("valo_shim");
    // The shim calls xcb_* functions directly, required in test binaries
    println!("cargo::rustc-link-lib=dylib=xcb");
    println!("cargo::rustc-link-lib=dylib=xcb-present");
    println!("cargo::rustc-link-lib=dylib=X11-xcb");
    println!("cargo::rustc-link-arg-cdylib=-Wl,--no-as-needed,-lX11,--as-needed");
}
