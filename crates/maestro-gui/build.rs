//! Embeds the application icon in the Windows executable.

fn main() {
    println!("cargo:rerun-if-changed=assets/windows.rc");
    println!("cargo:rerun-if-changed=assets/app-icon/icon.ico");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // gpui loads icon resource 1 for the window class, so the executable in Explorer and
        // its windows in the taskbar show the same icon.
        embed_resource::compile("assets/windows.rc", embed_resource::NONE)
            .manifest_optional()
            .expect("could not embed the Maestro application icon");
    }
}
