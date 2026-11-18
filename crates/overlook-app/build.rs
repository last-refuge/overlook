fn main() {
    println!("cargo:rerun-if-changed=Info.plist");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        // AppKit reads the application menu name from this metadata on standalone launches.
        let plist = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Info.plist");
        println!("cargo:rustc-link-arg-bin=overlook=-Wl,-sectcreate,__TEXT,__info_plist");
        println!("cargo:rustc-link-arg-bin=overlook=-Xlinker");
        println!("cargo:rustc-link-arg-bin=overlook={}", plist.display());
    }
}
