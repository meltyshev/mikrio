fn main() {
    napi_build::setup();
    mikrio_build::sqlite_runtime();

    // napi-build applies its macOS flags to the library cdylib only. Cargo's
    // rustc-link-arg-examples also omits library examples. The
    // separately built observation addon is an example cdylib with the same
    // host-provided Node-API symbols; it is never packed with the library.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        configure_example_linking();
    }
}

#[allow(clippy::print_stdout, reason = "Cargo build-script link instruction")]
fn configure_example_linking() {
    println!("cargo:rustc-link-arg=-Wl,-undefined,dynamic_lookup");
}
