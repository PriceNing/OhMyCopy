fn main() {
    #[cfg(windows)]
    {
        let mut res = winres::WindowsResource::new();
        res.set_icon("assets/ohmycopy.ico");
        // Product metadata shown in Explorer properties
        res.set("ProductName", "OhMyCopy");
        res.set("FileDescription", "OhMyCopy — LAN clipboard sync");
        res.set("LegalCopyright", "MIT");
        if let Err(e) = res.compile() {
            // Don't hard-fail cross-compiles without winres tools; warn instead.
            println!("cargo:warning=winres icon embed failed: {e}");
        }
    }
    println!("cargo:rerun-if-changed=assets/ohmycopy.ico");
    println!("cargo:rerun-if-changed=assets/icon.png");
    println!("cargo:rerun-if-changed=assets/tray.png");
}
