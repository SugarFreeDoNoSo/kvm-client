//! Non-secret preferences are stored atomically; passwords live only in the OS keystore.
use crate::{
    input::InputSettings,
    media::VideoSettings,
    net,
    sunshine::{Endpoint, SunshineStreamSettings},
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};
use tauri::{Manager, State};

const SERVICE: &str = "local.kvm.client";
const MAX_RECENT: usize = 8;
const MAX_PREFERENCES_BYTES: u64 = 64 * 1024;

#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct Preferences {
    pub address: String,
    pub recent_addresses: Vec<String>,
    pub sunshine_endpoint: Option<Endpoint>,
    pub video: VideoSettings,
    pub input: InputSettings,
    pub stream: SunshineStreamSettings,
    pub dashboard_expanded: bool,
    pub remember_password: bool,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            address: "https://192.168.100.8/".into(),
            recent_addresses: Vec::new(),
            sunshine_endpoint: None,
            video: VideoSettings::default(),
            input: InputSettings::default(),
            stream: SunshineStreamSettings::default(),
            dashboard_expanded: false,
            remember_password: true,
        }
    }
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct PreferencePatch {
    pub address: Option<String>,
    pub sunshine_endpoint: Option<Endpoint>,
    pub video: Option<VideoSettings>,
    pub input: Option<InputSettings>,
    pub stream: Option<SunshineStreamSettings>,
    pub dashboard_expanded: Option<bool>,
    pub remember_password: Option<bool>,
}

#[derive(Default)]
pub struct PreferenceState {
    operation: tokio::sync::Mutex<()>,
    credentials: tokio::sync::Mutex<()>,
}

fn normalize_origin(address: &str) -> Result<String, String> {
    if address.len() > 2048 {
        return Err("La dirección del KVM es demasiado larga".into());
    }
    net::normalize_address(address).map(|url| url.to_string())
}

impl Preferences {
    fn validate(mut self) -> Result<Self, String> {
        self.address = normalize_origin(&self.address)?;
        self.input.validate()?;
        self.stream.validate()?;
        self.video.validate()?;
        if self.video.filter == crate::media::VideoFilter::Sharp {
            self.video.filter = crate::media::VideoFilter::Desktop;
        }
        if let Some(endpoint) = &self.sunshine_endpoint {
            endpoint.validate()?;
        }
        let mut recent = vec![self.address.clone()];
        for address in &self.recent_addresses {
            let address = normalize_origin(address)?;
            if !recent.contains(&address) {
                recent.push(address);
            }
            if recent.len() == MAX_RECENT {
                break;
            }
        }
        self.recent_addresses = recent;
        Ok(self)
    }

    fn merge(mut self, patch: PreferencePatch) -> Result<Self, String> {
        if let Some(address) = patch.address {
            self.address = address;
        }
        if let Some(endpoint) = patch.sunshine_endpoint {
            self.sunshine_endpoint = Some(endpoint);
        }
        if let Some(video) = patch.video {
            self.video = video;
        }
        if let Some(input) = patch.input {
            self.input = input;
        }
        if let Some(stream) = patch.stream {
            self.stream = stream;
        }
        if let Some(expanded) = patch.dashboard_expanded {
            self.dashboard_expanded = expanded;
        }
        if let Some(remember) = patch.remember_password {
            self.remember_password = remember;
        }
        self.validate()
    }
}

fn preferences_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_config_dir()
        .map(|root| root.join("preferences.json"))
        .map_err(|_| "No se pudo localizar la carpeta de preferencias".into())
}

fn read_preferences(path: &Path) -> Result<Preferences, String> {
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Preferences::default().validate();
        }
        Err(_) => return Err("No se pudieron leer las preferencias guardadas".into()),
    };
    use std::io::Read;
    let mut bytes = Vec::new();
    (&mut file)
        .take(MAX_PREFERENCES_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "No se pudieron leer las preferencias guardadas")?;
    if bytes.len() as u64 > MAX_PREFERENCES_BYTES {
        return Err("El archivo de preferencias supera el tamaño permitido".into());
    }
    // Do not include parser diagnostics, which might echo unexpected file contents.
    let preferences: Preferences = serde_json::from_slice(&bytes)
        .map_err(|_| "Las preferencias guardadas no son válidas; no se han sobrescrito")?;
    preferences.validate()
}

fn write_preferences(path: &Path, preferences: &Preferences) -> Result<(), String> {
    let parent = path.parent().ok_or("Ruta de preferencias inválida")?;
    fs::create_dir_all(parent).map_err(|_| "No se pudo crear la carpeta de preferencias")?;
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .map_err(|_| "No se pudieron guardar las preferencias")?;
        let bytes = serde_json::to_vec(preferences)
            .map_err(|_| "No se pudieron preparar las preferencias")?;
        file.write_all(&bytes)
            .map_err(|_| "No se pudieron guardar las preferencias")?;
        file.sync_all()
            .map_err(|_| "No se pudieron guardar las preferencias")?;
        fs::rename(&temporary, path).map_err(|_| "No se pudieron guardar las preferencias")
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(String::from)
}

fn update_preferences(path: &Path, patch: PreferencePatch) -> Result<Preferences, String> {
    let preferences = read_preferences(path)?.merge(patch)?;
    write_preferences(path, &preferences)?;
    Ok(preferences)
}

#[tauri::command]
pub async fn get_preferences(
    app: tauri::AppHandle,
    state: State<'_, PreferenceState>,
) -> Result<Preferences, String> {
    let _operation = state.operation.lock().await;
    let path = preferences_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || read_preferences(&path))
        .await
        .map_err(|_| "No se pudieron cargar las preferencias".to_string())?
}

#[tauri::command]
pub async fn save_preferences(
    app: tauri::AppHandle,
    state: State<'_, PreferenceState>,
    patch: PreferencePatch,
) -> Result<Preferences, String> {
    let _operation = state.operation.lock().await;
    let path = preferences_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || update_preferences(&path, patch))
        .await
        .map_err(|_| "No se pudieron guardar las preferencias".to_string())?
}

fn password_entry(address: &str) -> Result<keyring::Entry, String> {
    let origin = normalize_origin(address)?;
    keyring::Entry::new(SERVICE, &origin)
        .map_err(|_| "No se pudo abrir el almacén seguro de contraseñas".into())
}

fn delete_entry(entry: keyring::Entry) -> Result<(), String> {
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(_) => Err("No se pudo eliminar la contraseña del almacén seguro".into()),
    }
}

#[tauri::command]
pub async fn load_saved_password(
    state: State<'_, PreferenceState>,
    address: String,
) -> Result<Option<String>, String> {
    let origin = normalize_origin(&address)?;
    let _credentials = state.credentials.lock().await;
    tauri::async_runtime::spawn_blocking(move || match password_entry(&origin)?.get_password() {
        Ok(password) => Ok(Some(password)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(_) => {
            Err("No se pudo leer la contraseña. Desbloquea el almacén seguro del sistema".into())
        }
    })
    .await
    .map_err(|_| "No se pudo leer la contraseña guardada".to_string())?
}

#[tauri::command]
pub async fn save_saved_password(
    state: State<'_, PreferenceState>,
    address: String,
    password: String,
) -> Result<(), String> {
    let origin = normalize_origin(&address)?;
    if password.len() > 64 * 1024 {
        return Err("La contraseña supera el tamaño permitido".into());
    }
    let _credentials = state.credentials.lock().await;
    tauri::async_runtime::spawn_blocking(move || {
        let entry = password_entry(&origin)?;
        if password.is_empty() {
            return delete_entry(entry);
        }
        entry.set_password(&password).map_err(|_| {
            "No se pudo guardar la contraseña. Desbloquea el almacén seguro del sistema".into()
        })
    })
    .await
    .map_err(|_| "No se pudo guardar la contraseña".to_string())?
}

#[tauri::command]
pub async fn delete_saved_password(
    state: State<'_, PreferenceState>,
    address: String,
) -> Result<(), String> {
    let origin = normalize_origin(&address)?;
    let _credentials = state.credentials.lock().await;
    tauri::async_runtime::spawn_blocking(move || delete_entry(password_entry(&origin)?))
        .await
        .map_err(|_| "No se pudo eliminar la contraseña".to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_updates_preserve_other_fields_and_normalize_history() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("preferences.json");
        update_preferences(
            &path,
            PreferencePatch {
                address: Some(" https://192.168.1.10 ".into()),
                dashboard_expanded: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
        let result = update_preferences(
            &path,
            PreferencePatch {
                sunshine_endpoint: Some(Endpoint {
                    ip: "192.168.1.20".into(),
                    port: 47989,
                }),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(result.address, "https://192.168.1.10/");
        assert!(result.dashboard_expanded);
        assert_eq!(result.sunshine_endpoint.unwrap().ip, "192.168.1.20");
        for number in 11..24 {
            update_preferences(
                &path,
                PreferencePatch {
                    address: Some(format!("https://192.168.1.{number}/")),
                    ..Default::default()
                },
            )
            .unwrap();
        }
        let result = read_preferences(&path).unwrap();
        assert_eq!(result.recent_addresses.len(), MAX_RECENT);
        assert_eq!(result.recent_addresses[0], "https://192.168.1.23/");
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[test]
    fn preferences_never_serialize_passwords_and_file_is_private() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("preferences.json");
        update_preferences(&path, PreferencePatch::default()).unwrap();
        let bytes = fs::read(&path).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(value.get("password").is_none());
        assert!(value.get("privateKey").is_none());
        assert!(serde_json::from_value::<PreferencePatch>(
            serde_json::json!({"password":"must-not-be-written"})
        )
        .is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn malformed_preferences_are_not_overwritten() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("preferences.json");
        let original = b"{broken json";
        fs::write(&path, original).unwrap();
        assert!(update_preferences(&path, PreferencePatch::default()).is_err());
        assert_eq!(fs::read(&path).unwrap(), original);
    }

    #[test]
    fn invalid_origins_and_settings_leave_saved_preferences_unchanged() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("preferences.json");
        update_preferences(&path, PreferencePatch::default()).unwrap();
        let original = fs::read(&path).unwrap();
        for address in [
            "http://host/",
            "https://user:secret@host/",
            "https://host/path",
        ] {
            assert!(update_preferences(
                &path,
                PreferencePatch {
                    address: Some(address.into()),
                    ..Default::default()
                }
            )
            .is_err());
        }
        assert!(update_preferences(
            &path,
            PreferencePatch {
                input: Some(InputSettings {
                    hz: 99,
                    sensitivity: 1.0
                }),
                ..Default::default()
            }
        )
        .is_err());
        assert_eq!(fs::read(&path).unwrap(), original);
        assert_eq!(
            normalize_origin(" https://HOST:443 ").unwrap(),
            "https://host/"
        );
    }
}
