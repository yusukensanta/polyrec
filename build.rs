fn main() {
    // The DLLs polyrec.exe imports (d3d11, mfplat, dwmapi, ...) load from
    // System32 only (LOAD_LIBRARY_SEARCH_SYSTEM32), not from the exe's own
    // folder first -- where a planted one would otherwise win. See
    // .cargo/config.toml for the C runtime.
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-bins=/DEPENDENTLOADFLAG:0x800");
    }
    if std::env::var("CARGO_CFG_WINDOWS").is_ok() {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.compile().expect("failed to embed icon resource");
    }
}
