//! Embeds the application icon and file details in the Windows executable.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/icon/gesso.ico");
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|t| t == "windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon/gesso.ico");
        res.set("ProductName", "Gesso");
        res.set("FileDescription", "Gesso");
        if let Err(e) = res.compile() {
            eprintln!("cannot embed the application icon: {e}");
            std::process::exit(1);
        }
    }
}
