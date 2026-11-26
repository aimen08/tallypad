fn main() {
    // Embed the icon (resource #1, which GPUI uses for the window) and
    // version info into the Windows executable.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=assets/tallypad.rc");
        println!("cargo:rerun-if-changed=assets/tallypad.ico");
        embed_resource::compile("assets/tallypad.rc", embed_resource::NONE)
            .manifest_optional()
            .unwrap();
    }
}
