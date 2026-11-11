fn main() {
    mikrio_build::sqlite_runtime();

    // The extension and private observation cdylibs obtain Python symbols from
    // their host. Cargo's cdylib-only flags omit example libraries on macOS.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        extension_link_mode();
    }
}

#[allow(clippy::print_stdout, reason = "Cargo build-script link instruction")]
fn extension_link_mode() {
    println!("cargo:rustc-link-arg=-Wl,-undefined,dynamic_lookup");
}
