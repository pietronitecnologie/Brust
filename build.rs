//! Embeds the application icon and version information into the Windows executable.
fn main() {
    println!("cargo:rerun-if-changed=assets/icon/brust.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon/brust.ico")
            .set("ProductName", "Brust")
            .set("FileDescription", "Brust – desktop client for web apps")
            .set("LegalCopyright", "GPL-3.0-or-later");
        if let Err(e) = res.compile() {
            // Missing resource compiler: build without the embedded icon instead of failing.
            println!("cargo:warning=icon not embedded: {e}");
        }
    }
}
