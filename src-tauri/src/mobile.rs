//! Android uses WebView WebRTC; HTTPS and signaling retain native pinned TLS.
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use std::{path::PathBuf, sync::Mutex, time::Duration};
use tauri::{Emitter, Manager, State};
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async_tls_with_config, tungstenite::Message, Connector};

#[derive(Default)]
struct MobileState {
    session: Mutex<Option<(String, mpsc::Sender<String>)>>,
    connecting: tokio::sync::Mutex<()>,
}

#[derive(Clone, Serialize)]
struct SignalEvent {
    session: String,
    kind: &'static str,
    data: String,
}

fn trust_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join("trusted-devices.json"))
}

#[tauri::command]
async fn mobile_inspect(
    app: tauri::AppHandle,
    address: String,
) -> Result<crate::net::DeviceInspection, String> {
    crate::net::inspect(trust_path(&app)?, &address).await
}

#[tauri::command]
async fn mobile_trust(
    app: tauri::AppHandle,
    address: String,
    fingerprint: String,
) -> Result<(), String> {
    crate::net::trust(trust_path(&app)?, &address, &fingerprint).await
}

#[tauri::command]
async fn mobile_connect(
    app: tauri::AppHandle,
    state: State<'_, MobileState>,
    address: String,
    password: String,
    session: String,
) -> Result<(), String> {
    let _guard = state.connecting.lock().await;
    if session.is_empty() || session.len() > 64 {
        return Err("Sesión inválida".into());
    }
    // Dropping the previous sender closes its socket, including on failed reconnect.
    state.session.lock().map_err(|_| "Sesión bloqueada")?.take();
    let device = crate::net::DeviceSession::login(&trust_path(&app)?, &address, &password).await?;
    let (mut socket, _) = tokio::time::timeout(
        Duration::from_secs(10),
        connect_async_tls_with_config(
            device.signaling_request()?,
            None,
            false,
            Some(Connector::Rustls(device.tls.clone())),
        ),
    )
    .await
    .map_err(|_| "Tiempo agotado abriendo señalización")?
    .map_err(|e| e.to_string())?;
    let (tx, mut rx) = mpsc::channel::<String>(64);
    *state.session.lock().map_err(|_| "Sesión bloqueada")? = Some((session.clone(), tx));
    tauri::async_runtime::spawn(async move {
        let result: Result<(), String> = async {
            loop {
                tokio::select! {
                    outgoing = rx.recv() => match outgoing {
                        Some(text) => socket.send(Message::Text(text.into())).await.map_err(|e| e.to_string())?,
                        None => { let _ = socket.close(None).await; return Ok(()); }
                    },
                    incoming = socket.next() => match incoming {
                        Some(Ok(Message::Text(text))) if text == "ping" => socket.send(Message::Text("pong".into())).await.map_err(|e| e.to_string())?,
                        Some(Ok(Message::Text(text))) => { app.emit("kvm://mobile-signal", SignalEvent { session: session.clone(), kind: "message", data: text.to_string() }).map_err(|e| e.to_string())?; },
                        Some(Ok(Message::Ping(data))) => socket.send(Message::Pong(data)).await.map_err(|e| e.to_string())?,
                        Some(Ok(Message::Close(_))) | None => return Err("El KVM cerró la conexión".into()),
                        Some(Err(error)) => return Err(error.to_string()),
                        _ => {}
                    }
                }
            }
        }.await;
        let _ = app.emit(
            "kvm://mobile-signal",
            SignalEvent {
                session,
                kind: "closed",
                data: result.err().unwrap_or_default(),
            },
        );
    });
    Ok(())
}

#[tauri::command]
async fn mobile_signal(
    state: State<'_, MobileState>,
    session: String,
    message: String,
) -> Result<(), String> {
    if message.len() > 256 * 1024 {
        return Err("Señal demasiado grande".into());
    }
    let value: serde_json::Value = serde_json::from_str(&message).map_err(|e| e.to_string())?;
    if !matches!(value["type"].as_str(), Some("offer" | "new-ice-candidate")) {
        return Err("Señal no permitida".into());
    }
    let tx = state
        .session
        .lock()
        .map_err(|_| "Sesión bloqueada")?
        .as_ref()
        .filter(|(id, _)| id == &session)
        .map(|(_, tx)| tx.clone())
        .ok_or("Sesión cerrada")?;
    tx.try_send(message)
        .map_err(|_| "Cola de señalización no disponible".into())
}

#[tauri::command]
fn mobile_disconnect(state: State<'_, MobileState>, session: String) -> Result<(), String> {
    let mut current = state.session.lock().map_err(|_| "Sesión bloqueada")?;
    if current.as_ref().is_some_and(|(id, _)| id == &session) {
        current.take();
    }
    Ok(())
}

#[tauri::mobile_entry_point]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_kvm_mobile::init())
        .manage(MobileState::default())
        .invoke_handler(tauri::generate_handler![
            mobile_inspect,
            mobile_trust,
            mobile_connect,
            mobile_signal,
            mobile_disconnect
        ])
        .run(tauri::generate_context!())
        .expect("No se pudo iniciar KVM Client para Android");
}
