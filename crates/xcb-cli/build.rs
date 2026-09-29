//! Give `xcb.exe` the 8 MiB main-thread stack Linux and macOS provide.
//! Windows links executables with 1 MiB, which the CLI's startup overflows.
//! A link argument here, unlike `.cargo/config.toml` rustflags, survives a
//! `RUSTFLAGS` override.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let windows = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    if windows && msvc {
        println!("cargo:rustc-link-arg-bins=/STACK:8388608");
    }
}
