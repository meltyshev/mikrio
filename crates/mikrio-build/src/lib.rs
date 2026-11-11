//! Internal native loader policy for mikrio executable and library targets.

/// Emit the platform linker search path for an adjacent shared SQLite runtime.
///
/// Call from a Cargo build script. The packaged companion keeps its native
/// loader identity; deployment must retain it beside the executable or library.
#[allow(clippy::print_stdout, reason = "Cargo build-script link instruction")]
pub fn sqlite_runtime() {
    println!("cargo:rerun-if-changed=build.rs");

    match std::env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("macos") => println!("cargo:rustc-link-arg=-Wl,-rpath,@loader_path"),
        Ok("linux") => println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN"),
        // Windows loads the companion from the application directory.
        _ => {}
    }
}
