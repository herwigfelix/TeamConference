// Bettet unter Windows ein Anwendungs-Manifest ein. Ohne Manifest warnt
// wxWidgets beim Start („no manifest", künftig deprecated) und es fehlen
// moderne Common Controls (Theming) sowie DPI-Awareness.
fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "windows" {
        use embed_manifest::manifest::{DpiAwareness, SupportedOS};
        use embed_manifest::{embed_manifest, new_manifest};

        let manifest = new_manifest("TeamConference.Client")
            .supported_os(SupportedOS::Windows7..=SupportedOS::Windows10)
            .dpi_awareness(DpiAwareness::PerMonitorV2);

        if let Err(e) = embed_manifest(manifest) {
            println!("cargo::warning=Manifest konnte nicht eingebettet werden: {e}");
        }
    }
}
