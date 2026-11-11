#[allow(clippy::print_stdout, reason = "Cargo build-script link instruction")]
fn main() {
    mikrio_build::sqlite_runtime();

    match std::env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("macos") => {
            println!("cargo:rustc-link-arg-cdylib=-Wl,-install_name,@rpath/libmikrio.1.dylib");
            println!("cargo:rustc-link-arg-cdylib=-Wl,-compatibility_version,1.0.0");
            println!("cargo:rustc-link-arg-cdylib=-Wl,-current_version,1.0.0");
        }
        Ok("linux") => println!("cargo:rustc-link-arg-cdylib=-Wl,-soname,libmikrio.so.1"),
        _ => {}
    }
}
