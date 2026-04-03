fn main() {
    println!("cargo::rustc-link-arg-cdylib=-Wl,--no-as-needed,-lX11,--as-needed");
}
