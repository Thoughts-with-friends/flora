//! Puts `assets/shocora.ico` into the Windows executable, for Explorer and the taskbar.
//! Other targets have no such resource: `embed-resource` does nothing there.
fn main() {
    println!("cargo:rerun-if-changed=assets/shocora.rc");
    println!("cargo:rerun-if-changed=assets/shocora.ico");
    if let Err(error) =
        embed_resource::compile("assets/shocora.rc", embed_resource::NONE).manifest_optional()
    {
        // The program still works without it, only with the default icon.
        println!("cargo:warning=executable icon not embedded: {error}");
    }
}
