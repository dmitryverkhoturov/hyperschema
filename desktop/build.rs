fn main() {
    // The app icon (taskbar, Start menu, Alt+Tab) lives in the exe as resource 1.
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=app.rc");
    embed_resource::compile("app.rc", embed_resource::NONE).manifest_optional().unwrap();
}
