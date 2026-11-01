//! Resolve the private GStreamer runtime before any media threads are started.

#[cfg(any(target_os = "macos", test))]
use std::path::{Path, PathBuf};

/// Call once, on the initial thread, before initializing Tauri or GStreamer.
pub fn configure() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let executable = std::env::current_exe()
            .map_err(|error| format!("No se pudo localizar el ejecutable: {error}"))?;
        if let Some(contents) = bundle_contents(&executable) {
            let plugins = contents.join("Resources/gstreamer-1.0");
            let scanner = contents.join("Resources/libexec/gst-plugin-scanner");
            if !plugins.is_dir() || !scanner.is_file() {
                return Err("El paquete de la aplicación no contiene el runtime GStreamer completo. Vuelve a instalar KVM Client.".into());
            }
            // Keep the packaged app independent of Homebrew installations and
            // incompatible plugins left in the invoking shell's environment.
            std::env::set_var("GST_PLUGIN_SYSTEM_PATH_1_0", &plugins);
            std::env::set_var("GST_PLUGIN_PATH_1_0", &plugins);
            std::env::set_var("GST_PLUGIN_SCANNER_1_0", &scanner);
            std::env::set_var("GST_PLUGIN_SCANNER", &scanner);
        } else if std::env::var_os("GST_PLUGIN_PATH_1_0").is_none()
            && std::env::var_os("GST_PLUGIN_PATH").is_none()
        {
            // Finder does not source scripts/env.sh. Development builds still
            // need Homebrew's separately installed libnice GStreamer plugin.
            for prefix in ["/opt/homebrew", "/usr/local"] {
                let plugins = Path::new(prefix).join("opt/libnice-gstreamer/libexec/gstreamer-1.0");
                if plugins.join("libgstnice.dylib").is_file() {
                    std::env::set_var("GST_PLUGIN_PATH_1_0", plugins);
                    break;
                }
            }
        }
    }
    Ok(())
}

#[cfg(any(target_os = "macos", test))]
fn bundle_contents(executable: &Path) -> Option<PathBuf> {
    let macos = executable.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    (macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && bundle.extension()? == "app")
        .then(|| contents.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_bundle_paths_with_spaces_and_unicode() {
        assert_eq!(
            bundle_contents(Path::new(
                "/Applications/Consola KVM á.app/Contents/MacOS/kvm-client"
            )),
            Some(PathBuf::from("/Applications/Consola KVM á.app/Contents"))
        );
    }

    #[test]
    fn development_and_unrelated_contents_directories_are_not_bundles() {
        for path in [
            "/project/src-tauri/target/release/kvm-client",
            "/project/Contents/MacOS/kvm-client",
            "/Applications/KVM.app/Contents/Resources/kvm-client",
        ] {
            assert_eq!(bundle_contents(Path::new(path)), None);
        }
    }
}
