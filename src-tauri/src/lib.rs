mod input;
mod media;
mod net;
mod signaling;

use std::{
    cell::RefCell,
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

use futures_util::{SinkExt, StreamExt};
use gtk::prelude::*;
use serde::Serialize;
use serde_json::{json, Value};
use tauri::{Emitter, Manager, State};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::{connect_async_tls_with_config, tungstenite::Message, Connector};

use input::{InputEngine, InputSettings};
use media::{MediaEngine, MediaEvent, VideoSettings};

thread_local! {
    static VIDEO_HOST: RefCell<Option<gtk::Box>> = const { RefCell::new(None) };
    static VIDEO_NOTICE: RefCell<Option<gtk::Label>> = const { RefCell::new(None) };
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AppStatus {
    state: String,
    message: String,
    address: String,
    capture: bool,
    capture_requested: bool,
    input_backend: String,
    decoder: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Metrics {
    input_hz: f64,
    target_hz: u32,
    received_fps: Option<f64>,
    decoded_fps: Option<f64>,
    presented_fps: Option<f64>,
    rtt_ms: Option<f64>,
    packets_lost: Option<i64>,
    queue_depth: usize,
    congested: bool,
}

struct Connection {
    stop: oneshot::Sender<()>,
    task: tauri::async_runtime::JoinHandle<()>,
}

struct Runtime {
    app: tauri::AppHandle,
    status: Mutex<AppStatus>,
    media: Mutex<Option<MediaEngine>>,
    input: OnceLock<InputEngine>,
    video_settings: Mutex<VideoSettings>,
    connection: Mutex<Option<Connection>>,
    actions: tokio::sync::Mutex<()>,
    capture_lock: Mutex<()>,
    control_rtt: Mutex<Option<f64>>,
}

#[derive(Clone)]
struct AppState(Arc<Runtime>);

impl Runtime {
    fn capture(&self, enabled: bool) -> Result<(), String> {
        let _capture = self.capture_lock.lock().unwrap_or_else(|e| e.into_inner());
        if enabled && self.status.lock().unwrap_or_else(|e| e.into_inner()).state != "connected" {
            return Err("Conecta el KVM antes de capturar la entrada".into());
        }
        if enabled && self.input().is_requested() {
            return Ok(());
        }
        self.input().capture(enabled)
    }
    fn update(&self, change: impl FnOnce(&mut AppStatus)) {
        let snapshot = {
            let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
            change(&mut status);
            status.clone()
        };
        let _ = self.app.emit("kvm://status", snapshot);
    }
    fn input(&self) -> &InputEngine {
        self.input.get().expect("input initialized in setup")
    }
    fn media(&self) -> Option<MediaEngine> {
        self.media.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    fn trust_path(&self) -> Result<PathBuf, String> {
        self.app
            .path()
            .app_config_dir()
            .map(|p| p.join("trusted-devices.json"))
            .map_err(|e| e.to_string())
    }
    fn metrics(&self) -> Option<Metrics> {
        let media = self.media()?;
        let video = media.stats();
        let input = self.input().stats();
        Some(Metrics {
            input_hz: input.input_hz,
            target_hz: input.target_hz,
            received_fps: video.received_fps,
            decoded_fps: video.decoded_fps,
            presented_fps: video.presented_fps,
            rtt_ms: video
                .rtt_ms
                .or_else(|| *self.control_rtt.lock().unwrap_or_else(|e| e.into_inner())),
            packets_lost: video.packets_lost,
            queue_depth: input.queue_depth,
            congested: input.congested,
        })
    }
}

#[tauri::command]
async fn inspect_device(
    state: State<'_, AppState>,
    address: String,
) -> Result<net::DeviceInspection, String> {
    net::inspect(state.0.trust_path()?, &address).await
}

#[tauri::command]
async fn trust_device(
    state: State<'_, AppState>,
    address: String,
    fingerprint: String,
) -> Result<(), String> {
    net::trust(state.0.trust_path()?, &address, &fingerprint).await
}

async fn stop_connection(runtime: &Arc<Runtime>) {
    // Flush release reports while the RPC channel is still alive.
    {
        let _capture = runtime
            .capture_lock
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _ = runtime.input().capture(false);
        runtime.input().release_all();
    }
    let connection = runtime
        .connection
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take();
    if let Some(connection) = connection {
        let _ = connection.stop.send(());
        let _ = connection.task.await;
    }
}

#[tauri::command]
async fn connect_device(
    state: State<'_, AppState>,
    address: String,
    password: String,
) -> Result<(), String> {
    let runtime = state.0.clone();
    let _guard = runtime.actions.lock().await;
    let normalized = net::normalize_address(&address)?.to_string();
    stop_connection(&runtime).await;
    *runtime
        .control_rtt
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;
    runtime.update(|s| {
        s.state = "connecting".into();
        s.message = "Autenticando y preparando video…".into();
        s.address = normalized.clone();
        s.capture = false;
        s.capture_requested = false;
        s.decoder.clear();
    });
    let (stop, rx) = oneshot::channel();
    let worker = runtime.clone();
    let task = tauri::async_runtime::spawn(async move {
        let result = tokio::select! {
            result=run_connection(worker.clone(),&normalized,&password)=>result,
            _=rx=>Ok(()),
        };
        {
            let _capture = worker
                .capture_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let _ = worker.input().capture(false);
            worker.input().release_all();
            worker.update(|s| {
                s.capture = false;
                s.capture_requested = false;
                s.decoder.clear();
                match result {
                    Ok(()) => {
                        s.state = "disconnected".into();
                        s.message = "Desconectado".into();
                    }
                    Err(error) => {
                        s.state = "error".into();
                        s.message = error;
                    }
                }
            });
        }
        let media = worker
            .media
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(media) = media {
            media.shutdown();
        }
        let _ = worker.app.run_on_main_thread(show_placeholder);
    });
    *runtime.connection.lock().unwrap_or_else(|e| e.into_inner()) = Some(Connection { stop, task });
    Ok(())
}

#[tauri::command]
async fn disconnect_device(state: State<'_, AppState>) -> Result<(), String> {
    let _guard = state.0.actions.lock().await;
    stop_connection(&state.0).await;
    state.0.update(|s| {
        s.state = "disconnected".into();
        s.message = "Desconectado".into();
        s.capture = false;
        s.capture_requested = false;
        s.decoder.clear();
    });
    Ok(())
}

#[tauri::command]
fn get_status(state: State<'_, AppState>) -> AppStatus {
    state
        .0
        .status
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

#[tauri::command]
fn set_input_settings(state: State<'_, AppState>, settings: InputSettings) -> Result<(), String> {
    state.0.input().set_settings(settings)
}

#[tauri::command]
fn set_video_settings(state: State<'_, AppState>, settings: VideoSettings) -> Result<(), String> {
    if !settings.sharpness.is_finite() || !(0.0..=1.0).contains(&settings.sharpness) {
        return Err("La nitidez debe estar entre 0 y 1".into());
    }
    if let Some(media) = state.0.media() {
        media.configure(settings)?;
    }
    *state
        .0
        .video_settings
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = settings;
    Ok(())
}

#[tauri::command]
fn set_capture(state: State<'_, AppState>, enabled: bool) -> Result<(), String> {
    state.0.capture(enabled)
}

#[tauri::command]
fn set_fullscreen(window: tauri::WebviewWindow, enabled: bool) -> Result<(), String> {
    window.set_fullscreen(enabled).map_err(|e| e.to_string())
}

fn show_placeholder() {
    VIDEO_NOTICE.with(|notice| *notice.borrow_mut() = None);
    VIDEO_HOST.with(|host| {
        if let Some(host) = host.borrow().as_ref() {
            for child in host.children() {
                host.remove(&child);
            }
            let label = gtk::Label::new(Some("Conecta el KVM para iniciar la transmisión"));
            label.set_hexpand(true);
            label.set_vexpand(true);
            host.pack_start(&label, true, true, 0);
            host.show_all();
        }
    });
}

fn show_video_notice(message: Option<String>) {
    VIDEO_NOTICE.with(|notice| {
        if let Some(label) = notice.borrow().as_ref() {
            label.set_text(message.as_deref().unwrap_or(""));
            label.set_visible(message.is_some());
        }
    });
}

async fn create_media(
    runtime: Arc<Runtime>,
    tx: mpsc::UnboundedSender<MediaEvent>,
) -> Result<MediaEngine, String> {
    let (send, receive) = oneshot::channel();
    let settings = *runtime
        .video_settings
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    runtime
        .app
        .clone()
        .run_on_main_thread(move || {
            if send.is_closed() {
                return;
            }
            let result = (|| {
                let media = MediaEngine::new(Arc::new(move |event| {
                    let _ = tx.send(event);
                }))?;
                media.configure(settings)?;
                // GTK3 and GStreamer use different glib-rs versions; only the
                // GtkWidget C ABI crosses between them, on the GTK main thread.
                let widget: gtk::Widget = unsafe {
                    gtk::glib::translate::from_glib_none(
                        media.widget_ptr() as *mut gtk::ffi::GtkWidget
                    )
                };
                VIDEO_HOST.with(|host| {
                    if let Some(host) = host.borrow().as_ref() {
                        for child in host.children() {
                            host.remove(&child);
                        }
                        let events = gtk::EventBox::new();
                        events.add(&widget);
                        events.add_events(gtk::gdk::EventMask::BUTTON_PRESS_MASK);
                        let weak = Arc::downgrade(&runtime);
                        events.connect_button_press_event(move |_, _| {
                            if let Some(runtime) = weak.upgrade() {
                                if let Err(error) = runtime.capture(true) {
                                    runtime.update(|s| s.message = error);
                                }
                            }
                            gtk::glib::Propagation::Proceed
                        });
                        let overlay = gtk::Overlay::new();
                        overlay.add(&events);
                        let notice = gtk::Label::new(Some("Esperando señal de video…"));
                        notice.set_halign(gtk::Align::Center);
                        notice.set_valign(gtk::Align::Center);
                        notice.set_justify(gtk::Justification::Center);
                        overlay.add_overlay(&notice);
                        overlay.set_overlay_pass_through(&notice, true);
                        VIDEO_NOTICE.with(|value| *value.borrow_mut() = Some(notice));
                        host.pack_start(&overlay, true, true, 0);
                        host.show_all();
                    }
                });
                Ok(media)
            })();
            if let Err(Ok(media)) = send.send(result) {
                media.shutdown();
                show_placeholder();
            }
        })
        .map_err(|e| e.to_string())?;
    receive.await.map_err(|e| e.to_string())?
}

async fn run_connection(
    runtime: Arc<Runtime>,
    address: &str,
    password: &str,
) -> Result<(), String> {
    let session = net::DeviceSession::login(&runtime.trust_path()?, address, password).await?;
    let request = session.signaling_request()?;
    let (mut socket, _) = tokio::time::timeout(
        Duration::from_secs(10),
        connect_async_tls_with_config(
            request,
            None,
            false,
            Some(Connector::Rustls(session.tls.clone())),
        ),
    )
    .await
    .map_err(|_| "Tiempo agotado abriendo la señalización")?
    .map_err(|e| format!("No se pudo abrir WebRTC: {e}"))?;
    let (tx, mut events) = mpsc::unbounded_channel();
    let media = create_media(runtime.clone(), tx).await?;
    *runtime.media.lock().unwrap_or_else(|e| e.into_inner()) = Some(media.clone());
    let mut started = false;
    let mut answer_applied = false;
    let mut candidates: Vec<(u32, String)> = Vec::new();
    let mut local_candidates: Vec<(u32, String)> = Vec::new();
    let mut offer_sent = false;
    let mut ping_at: Option<Instant> = None;
    let mut next_ping = Instant::now();
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            incoming=socket.next()=> {
                match incoming {
                    Some(Ok(Message::Text(text)))=>match signaling::parse(&text)? {
                        signaling::Signal::Metadata=> {if !started {media.start()?;started=true;}},
                        signaling::Signal::Answer(sdp)=> {
                            media.handle_answer(&sdp)?;answer_applied=true;
                            for (index,candidate) in candidates.drain(..){media.add_ice(index,&candidate);}
                            for (index,candidate) in local_candidates.drain(..){socket.send(Message::Text(signaling::ice(&candidate,index).to_string().into())).await.map_err(|e|e.to_string())?;}
                        },
                        signaling::Signal::Ice{candidate,index}=> {if answer_applied{media.add_ice(index,&candidate);}else{candidates.push((index,candidate));}},
                        signaling::Signal::Ping=>socket.send(Message::Text("pong".into())).await.map_err(|e|e.to_string())?,
                        signaling::Signal::Ignore=>{},
                    },
                    Some(Ok(Message::Ping(bytes)))=>socket.send(Message::Pong(bytes)).await.map_err(|e|e.to_string())?,
                    Some(Ok(Message::Close(_)))|None=>return Err("El KVM cerró la conexión. Puedes volver a conectar.".into()),
                    Some(Err(error))=>return Err(format!("Se perdió la señalización: {error}")),
                    _=>{},
                }
            },
            Some(event)=events.recv()=>match event {
                MediaEvent::Offer{sdp}=> {socket.send(Message::Text(signaling::offer(&sdp).to_string().into())).await.map_err(|e|e.to_string())?;offer_sent=true;},
                MediaEvent::Ice{candidate,sdp_m_line_index}=> {
                    // The device drops ICE arriving before currentSession is
                    // created. Its answer confirms the offer was processed.
                    if offer_sent && answer_applied {socket.send(Message::Text(signaling::ice(&candidate,sdp_m_line_index).to_string().into())).await.map_err(|e|e.to_string())?;}
                    else{local_candidates.push((sdp_m_line_index,candidate));}
                },
                MediaEvent::State(state)=>match state.as_str(){
                    "connected"=>{
                        runtime.update(|s|{s.state="connected".into();s.message="Conectado · captura el control para comenzar".into();});
                        media.send_rpc(json!({"jsonrpc":"2.0","method":"getVideoState","params":{},"id":101}))?;
                    },
                    "failed"|"disconnected"|"closed"=>return Err("Se perdió el canal de video o control. Puedes volver a conectar.".into()),
                    _=>{},
                },
                MediaEvent::Decoder(decoder)=>runtime.update(|s|s.decoder=decoder),
                MediaEvent::Error(error)=>return Err(error),
                MediaEvent::Rpc(value)=>{
                    if value["id"]==102 && value["result"]=="pong" {
                        if let Some(sent)=ping_at.take(){*runtime.control_rtt.lock().unwrap_or_else(|e|e.into_inner())=Some(sent.elapsed().as_secs_f64()*1000.0);}
                    }
                    handle_rpc(&runtime,value)?;
                },
            },
            _=tick.tick()=> {
                if runtime.status.lock().unwrap_or_else(|e|e.into_inner()).state=="connected" && Instant::now()>=next_ping {
                    if ping_at.is_none() || ping_at.is_some_and(|sent|sent.elapsed()>Duration::from_secs(3)) {
                        if media.send_rpc(json!({"jsonrpc":"2.0","method":"ping","params":{},"id":102})).is_ok(){ping_at=Some(Instant::now());next_ping=Instant::now()+Duration::from_secs(2);}
                    }
                }
                if let Some(metrics)=runtime.metrics(){let _=runtime.app.emit("kvm://metrics",metrics);}
                if Instant::now()>deadline && runtime.status.lock().unwrap_or_else(|e|e.into_inner()).state!="connected" {
                    return Err("No se completó WebRTC en 20 segundos. Revisa la red y que el KVM use H.264.".into());
                }
            },
        }
    }
}

fn handle_rpc(runtime: &Runtime, value: Value) -> Result<(), String> {
    let video = if value["method"] == "videoInputState" {
        Some(&value["params"])
    } else if value["id"] == 101 {
        Some(&value["result"])
    } else {
        None
    };
    if let Some(video) = video {
        if video["ready"].is_boolean() {
            let message = video_notice(video);
            runtime.update(|s| {
                s.message = message.clone().unwrap_or_else(|| {
                    "Video HDMI disponible · captura el control para comenzar".into()
                })
            });
            let _ = runtime
                .app
                .run_on_main_thread(move || show_video_notice(message));
        }
    }
    match value["method"].as_str() {
        Some("otherSessionConnected" | "sessionInvalidated") => {
            Err("Otra sesión tomó el control del KVM".into())
        }
        Some("hidDeviceMissing") => {
            runtime.input().release_all();
            runtime.update(|s| {
                s.message = "El KVM perdió la conexión USB con el equipo remoto".into()
            });
            Ok(())
        }
        _ => Ok(()),
    }
}

fn video_notice(video: &Value) -> Option<String> {
    if video["ready"] == true {
        return None;
    }
    Some(match video["error"].as_str() {
        Some("no_signal") => {
            "Sin señal HDMI\nCaptura el control y pulsa Shift para despertar la pantalla; revisa el cable si persiste"
                .into()
        }
        Some("no_lock") => "Esperando sincronización de la señal HDMI".into(),
        Some("out_of_range") => {
            "La resolución o frecuencia HDMI está fuera del rango del KVM".into()
        }
        _ => "Esperando señal de video del KVM…".into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_signal_status_distinguishes_missing_input_from_transport_failure() {
        assert!(video_notice(&json!({"ready":false,"error":"no_signal"}))
            .unwrap()
            .contains("Sin señal HDMI"));
        assert!(video_notice(&json!({"ready":false,"error":"out_of_range"}))
            .unwrap()
            .contains("fuera del rango"));
        assert_eq!(video_notice(&json!({"ready":true})), None);
    }
}

pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let window = app
                .get_webview_window("main")
                .ok_or("No se creó la ventana principal")?;
            let container = window.default_vbox()?;
            for child in container.children() {
                child.set_vexpand(false);
                child.set_size_request(-1, 260);
                container.set_child_packing(&child, false, true, 0, gtk::PackType::Start);
            }
            let video_host = gtk::Box::new(gtk::Orientation::Vertical, 0);
            video_host.set_hexpand(true);
            video_host.set_vexpand(true);
            container.pack_start(&video_host, true, true, 0);
            VIDEO_HOST.with(|host| *host.borrow_mut() = Some(video_host));
            show_placeholder();
            let runtime = Arc::new(Runtime {
                app: app.handle().clone(),
                status: Mutex::new(AppStatus {
                    state: "disconnected".into(),
                    message: "Listo para conectar".into(),
                    address: "https://192.168.100.8/".into(),
                    capture: false,
                    capture_requested: false,
                    input_backend: String::new(),
                    decoder: String::new(),
                }),
                media: Mutex::new(None),
                input: OnceLock::new(),
                video_settings: Mutex::new(VideoSettings::default()),
                connection: Mutex::new(None),
                actions: tokio::sync::Mutex::new(()),
                capture_lock: Mutex::new(()),
                control_rtt: Mutex::new(None),
            });
            let sender = Arc::downgrade(&runtime);
            let callback = Arc::downgrade(&runtime);
            let input = InputEngine::new(
                Arc::new(move |value| {
                    sender
                        .upgrade()
                        .and_then(|r| r.media())
                        .ok_or("No hay canal de control".to_string())?
                        .send_rpc(value)
                }),
                Arc::new(move |capture| {
                    if let Some(runtime) = callback.upgrade() {
                        runtime.update(|s| {
                            s.capture = capture.active;
                            s.capture_requested = capture.requested;
                            s.input_backend = capture.backend;
                            s.message = capture.message;
                        });
                    }
                }),
            );
            let _ = runtime.input.set(input);
            let weak = Arc::downgrade(&runtime);
            window.on_window_event(move |event| {
                if let Some(runtime) = weak.upgrade() {
                    match event {
                        tauri::WindowEvent::Focused(false) if runtime.input().is_captured() => {
                            let _ = runtime.capture(false);
                        }
                        tauri::WindowEvent::CloseRequested { .. } => {
                            let _ = runtime.capture(false);
                            runtime.input().release_all();
                            if let Some(media) = runtime.media() {
                                media.shutdown();
                            }
                        }
                        _ => {}
                    }
                }
            });
            app.manage(AppState(runtime));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            inspect_device,
            trust_device,
            connect_device,
            disconnect_device,
            get_status,
            set_input_settings,
            set_video_settings,
            set_capture,
            set_fullscreen
        ])
        .run(tauri::generate_context!())
        .expect("No se pudo iniciar KVM Client");
}
