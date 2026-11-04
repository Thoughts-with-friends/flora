//! Puts `assets/shocora.ico` and the version information into the Windows executable, for
//! Explorer and the taskbar. Other targets have no such resource: `embed-resource` does nothing
//! there.
fn main() {
    println!("cargo:rerun-if-changed=assets/shocora.rc");
    println!("cargo:rerun-if-changed=assets/shocora.ico");
    let macros = [
        format!("VER_MAJOR={}", env!("CARGO_PKG_VERSION_MAJOR")),
        format!("VER_MINOR={}", env!("CARGO_PKG_VERSION_MINOR")),
        format!("VER_PATCH={}", env!("CARGO_PKG_VERSION_PATCH")),
        format!("VER_STRING=\"{}\"", env!("CARGO_PKG_VERSION")),
    ];
    if let Err(error) = embed_resource::compile("assets/shocora.rc", &macros).manifest_optional() {
        // The program still works without it, only with the default icon.
        println!("cargo:warning=executable resources not embedded: {error}");
    }
}
