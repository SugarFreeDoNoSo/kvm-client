mod health;
mod input;
mod media;
mod net;
mod preferences;
mod runtime_environment;
mod signaling;
pub mod smoke;
mod sunshine;
mod video_host;

use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU8, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::{Duration, Instant},
};

use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use serde_json::{json, Value};
use tauri::{Emitter, Manager, State};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::{connect_async_tls_with_config, tungstenite::Message, Connector};

use input::{InputEngine, InputSettings};
use media::{MediaEngine, MediaEvent, VideoSettings};

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AppStatus {
    state: String,
    phase: String,
    video_state: String,
    video_paused: bool,
    message: String,
    address: String,
    capture: bool,
    capture_requested: bool,
    input_enabled: bool,
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
    input_width: u32,
    input_height: u32,
    output_width: u32,
    output_height: u32,
    filter_processing_ms: Option<f64>,
    sink_dropped_frames: Option<u64>,
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
    video_health: Mutex<health::FrameHealth>,
    video_count_offset: Mutex<(u64, u64)>,
    source_notice: Mutex<Option<String>>,
    shutdown: AtomicU8,
}

#[derive(Clone)]
struct AppState(Arc<Runtime>);

impl Runtime {
    fn capture(&self, enabled: bool) -> Result<(), String> {
        let _capture = self.capture_lock.lock().unwrap_or_else(|e| e.into_inner());
        if enabled && !self.input().is_enabled() {
            return Err(
                "El mouse y teclado del KVM están desactivados; habilítalos antes de capturar"
                    .into(),
            );
        }
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
            let previous = (
                status.state.clone(),
                status.phase.clone(),
                status.video_state.clone(),
            );
            change(&mut status);
            if previous
                != (
                    status.state.clone(),
                    status.phase.clone(),
                    status.video_state.clone(),
                )
            {
                eprintln!(
                    "[kvm] state={} phase={} video={} {}",
                    status.state, status.phase, status.video_state, status.message
                );
            }
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
        let media = self
            .app
            .state::<sunshine::SunshineState>()
            .selected_media()
            .or_else(|| self.media())?;
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
            input_width: video.input_width,
            input_height: video.input_height,
            output_width: video.output_width,
            output_height: video.output_height,
            filter_processing_ms: video.filter_processing_ms,
            sink_dropped_frames: video.sink_dropped_frames,
        })
    }
}

#[tauri::command]
async fn inspect_device(
    state: State<'_, AppState>,
    address: String,
) -> Result<net::DeviceInspection, String> {
    let _guard = state.0.actions.lock().await;
    if matches!(
        state
            .0
            .status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .state
            .as_str(),
        "connected" | "connecting"
    ) {
        return Err("Desconecta la sesión antes de inspeccionar otro certificado".into());
    }
    state.0.update(|s| {
        s.state = "disconnected".into();
        s.phase = "tls".into();
        s.message = "Inspeccionando certificado TLS…".into();
    });
    let result = net::inspect(state.0.trust_path()?, &address).await;
    state.0.update(|s| {
        s.phase = "idle".into();
        match &result {
            Ok(inspection) => {
                s.message = if inspection.trusted {
                    "Certificado verificado".into()
                } else {
                    "Revisa y acepta la huella del certificado".into()
                };
            }
            Err(error) => {
                s.state = "error".into();
                s.message = error.clone();
            }
        }
    });
    result
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

fn request_shutdown(runtime: Arc<Runtime>) {
    if runtime
        .shutdown
        .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    let _ = runtime.capture(false);
    tauri::async_runtime::spawn(async move {
        let _guard = runtime.actions.lock().await;
        sunshine::shutdown(&runtime.app).await;
        stop_connection(&runtime).await;
        let completed = runtime.clone();
        let _ = runtime.app.run_on_main_thread(move || {
            video_host::show_placeholder();
            completed.shutdown.store(2, Ordering::Release);
            completed.app.exit(0);
        });
    });
}

#[tauri::command]
async fn connect_device(
    state: State<'_, AppState>,
    address: String,
    password: String,
    remember_password: Option<bool>,
) -> Result<(), String> {
    let runtime = state.0.clone();
    let _guard = runtime.actions.lock().await;
    if runtime.shutdown.load(Ordering::Acquire) != 0 {
        return Err("La aplicación se está cerrando".into());
    }
    let normalized = net::normalize_address(&address)?.to_string();
    stop_connection(&runtime).await;
    *runtime
        .control_rtt
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;
    *runtime
        .video_health
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = health::FrameHealth::new(Instant::now());
    *runtime
        .video_count_offset
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = (0, 0);
    *runtime
        .source_notice
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;
    runtime.update(|s| {
        s.phase = "auth".into();
        s.video_state = "waiting".into();
        s.video_paused = false;
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
            result=run_connection(worker.clone(),&normalized,&password,remember_password.unwrap_or(false))=>result,
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
                s.video_state = "waiting".into();
                s.video_paused = false;
                match result {
                    Ok(()) => {
                        s.state = "disconnected".into();
                        s.phase = "idle".into();
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
        let _ = worker.app.run_on_main_thread(video_host::show_placeholder);
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
        s.phase = "idle".into();
        s.video_state = "waiting".into();
        s.video_paused = false;
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
fn set_video_settings(
    state: State<'_, AppState>,
    sunshine: State<'_, sunshine::SunshineState>,
    settings: VideoSettings,
) -> Result<(), String> {
    settings.validate()?;
    let mut current = state
        .0
        .video_settings
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(media) = state.0.media() {
        media.configure(settings)?;
    }
    if let Some(media) = sunshine.media() {
        media.configure(settings)?;
    }
    *current = settings;
    Ok(())
}

#[tauri::command]
async fn set_kvm_video_paused(state: State<'_, AppState>, paused: bool) -> Result<(), String> {
    let runtime = &state.0;
    let _operation = runtime.actions.lock().await;
    if runtime
        .status
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .state
        != "connected"
    {
        return Err("Conecta PicoKVM antes de pausar su video".into());
    }
    let media = runtime.media().ok_or("No hay un receptor PicoKVM activo")?;
    if media.is_video_paused() == paused {
        return Ok(());
    }
    media.set_video_paused(paused)?;
    if !paused {
        *runtime
            .video_count_offset
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = media.frame_counts();
        let mut health = health::FrameHealth::new(Instant::now());
        if runtime
            .source_notice
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
        {
            health.source_ready(false, Instant::now());
        }
        *runtime
            .video_health
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = health;
    }
    let message = if paused {
        "Video de PicoKVM pausado · teclado y mouse disponibles"
    } else {
        "Reanudando video de PicoKVM…"
    };
    runtime.update(|status| {
        status.video_paused = paused;
        status.video_state = "waiting".into();
        status.message = message.into();
    });
    let notice = Some(message.into());
    let _ = runtime
        .app
        .run_on_main_thread(move || video_host::show_notice(notice));
    Ok(())
}

#[tauri::command]
fn set_capture(state: State<'_, AppState>, enabled: bool) -> Result<(), String> {
    state.0.capture(enabled)
}

#[tauri::command]
fn set_kvm_input_enabled(state: State<'_, AppState>, enabled: bool) -> Result<(), String> {
    let runtime = &state.0;
    let _capture = runtime
        .capture_lock
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let result = runtime.input().set_enabled(enabled);
    runtime.update(|s| {
        s.input_enabled = runtime.input().is_enabled();
        if enabled && result.is_ok() {
            s.message = "Mouse y teclado del KVM habilitados; pulsa Capturar para usarlos".into();
        }
    });
    result
}

#[tauri::command]
fn set_fullscreen(window: tauri::WebviewWindow, enabled: bool) -> Result<(), String> {
    window.set_fullscreen(enabled).map_err(|e| e.to_string())?;
    video_host::set_toolbar_visible(&window, !enabled)
}

#[tauri::command]
fn set_dashboard_expanded(
    window: tauri::WebviewWindow,
    state: State<'_, AppState>,
    expanded: bool,
) -> Result<(), String> {
    if expanded {
        state.0.capture(false)?;
    }
    video_host::set_dashboard_expanded(&window, expanded)
}

#[tauri::command]
fn set_dashboard_fullscreen(
    window: tauri::WebviewWindow,
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<(), String> {
    if enabled {
        state.0.capture(false)?;
    }
    video_host::set_dashboard_fullscreen(&window, enabled)
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
                video_host::attach(&media)?;
                Ok(media)
            })();
            if let Err(Ok(media)) = send.send(result) {
                // Cocoa teardown can synchronously dispatch to AppKit; never block its thread.
                tauri::async_runtime::spawn_blocking(move || {
                    media.shutdown();
                });
            }
        })
        .map_err(|e| e.to_string())?;
    receive.await.map_err(|e| e.to_string())?
}

async fn run_connection(
    runtime: Arc<Runtime>,
    address: &str,
    password: &str,
    remember_password: bool,
) -> Result<(), String> {
    let session = net::DeviceSession::login(&runtime.trust_path()?, address, password).await?;
    if remember_password {
        let app = runtime.app.clone();
        let verified_address = address.to_owned();
        let verified_password = password.to_owned();
        // Only store a credential after the pinned HTTPS login accepts it.
        // A locked/unavailable keychain must not interrupt an authenticated session.
        tauri::async_runtime::spawn(async move {
            if let Err(error) = preferences::save_saved_password(
                app.state::<preferences::PreferenceState>(),
                verified_address,
                verified_password,
            )
            .await
            {
                let _ = app.emit("kvm://credential-error", error);
            }
        });
    }
    runtime.update(|s| {
        s.phase = "signaling".into();
        s.message = "Abriendo señalización segura…".into();
    });
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
    runtime.update(|s| {
        s.phase = "webrtc".into();
        s.message = "Negociando video y canal de control…".into();
    });
    let (tx, mut events) = mpsc::unbounded_channel();
    let media = create_media(runtime.clone(), tx).await?;
    {
        let settings = runtime
            .video_settings
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        media.configure(*settings)?;
        *runtime.media.lock().unwrap_or_else(|e| e.into_inner()) = Some(media.clone());
    }
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
                MediaEvent::State(state)=>{eprintln!("[kvm] WebRTC {state}");match state.as_str(){
                    "connected"=>{
                        runtime.update(|s|{s.state="connected".into();s.phase="ready".into();s.message="Control conectado · esperando imagen".into();});
                        media.send_rpc(json!({"jsonrpc":"2.0","method":"getVideoState","params":{},"id":101}))?;
                    },
                    "failed"|"disconnected"|"closed"=>return Err("Se perdió el canal de video o control. Puedes volver a conectar.".into()),
                    _=>{},
                }},
                MediaEvent::Decoder(decoder)=>{eprintln!("[kvm] decoder={decoder}");runtime.update(|s|s.decoder=decoder);},
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
                update_video_health(&runtime);
                if let Some(metrics)=runtime.metrics(){let _=runtime.app.emit("kvm://metrics",metrics);}
                if Instant::now()>deadline && runtime.status.lock().unwrap_or_else(|e|e.into_inner()).state!="connected" {
                    return Err("No se completó WebRTC en 20 segundos. Revisa la red y que el KVM use H.264.".into());
                }
            },
        }
    }
}

fn resumed_frame_counts(counts: (u64, u64), baseline: (u64, u64)) -> (u64, u64) {
    (
        counts.0.saturating_sub(baseline.0),
        counts.1.saturating_sub(baseline.1),
    )
}

fn update_video_health(runtime: &Runtime) {
    let Some(media) = runtime.media() else {
        return;
    };
    if media.is_video_paused()
        || runtime
            .status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .video_paused
    {
        return;
    }
    let counts = media.frame_counts();
    let offset = *runtime
        .video_count_offset
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let state = runtime
        .video_health
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .observe(Instant::now(), resumed_frame_counts(counts, offset));
    let changed = runtime
        .status
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .video_state
        != state.as_str();
    if !changed {
        return;
    }
    let notice = match state {
        health::VideoState::Playing => None,
        health::VideoState::Waiting => Some("Esperando señal de video…".to_string()),
        health::VideoState::NoSignal => runtime
            .source_notice
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone(),
        health::VideoState::Stalled => Some(
            "El video no está avanzando · el canal de control sigue disponible si está conectado"
                .to_string(),
        ),
    };
    runtime.update(|s| {
        s.video_state = state.as_str().into();
        s.message = notice
            .clone()
            .unwrap_or_else(|| "Video activo · captura el control para comenzar".into());
    });
    let _ = runtime
        .app
        .run_on_main_thread(move || video_host::show_notice(notice));
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
            runtime
                .video_health
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .source_ready(video["ready"] == true, Instant::now());
            let message = video_notice(video);
            *runtime
                .source_notice
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = message.clone();
            if runtime
                .status
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .video_paused
            {
                return Ok(());
            }
            runtime.update(|s| {
                s.message = message.clone().unwrap_or_else(|| {
                    "Video HDMI disponible · captura el control para comenzar".into()
                })
            });
            let _ = runtime
                .app
                .run_on_main_thread(move || video_host::show_notice(message));
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
    fn resume_waits_for_new_frames_instead_of_old_cumulative_totals() {
        let now = Instant::now();
        let baseline = (1200, 1198);
        let mut health = health::FrameHealth::new(now);
        assert_eq!(
            health.observe(now, resumed_frame_counts(baseline, baseline)),
            health::VideoState::Waiting
        );
        assert_eq!(
            health.observe(
                now + Duration::from_secs(5),
                resumed_frame_counts(baseline, baseline)
            ),
            health::VideoState::Waiting
        );
        assert_eq!(
            health.observe(
                now + Duration::from_secs(6),
                resumed_frame_counts((1201, 1199), baseline)
            ),
            health::VideoState::Playing
        );
    }

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

fn app_context() -> tauri::Context<tauri::Wry> {
    // Generate once: macOS embeds a single Info.plist symbol per crate.
    tauri::generate_context!()
}

/// Offline release check: do not open a window, preferences, credentials or peers.
fn check_runtime() -> Result<(), String> {
    gstreamer::init().map_err(|error| error.to_string())?;
    let required = [
        "webrtcbin",
        "nicesrc",
        "nicesink",
        "dtlssrtpdec",
        "dtlssrtpenc",
        "sctpenc",
        "sctpdec",
        "rtph264depay",
        "h264parse",
        "glupload",
        "glcolorconvert",
        "glshader",
        "rtpbin",
        "queue",
        "valve",
        "appsrc",
        "capsfilter",
        "fakesink",
        "opusdec",
        "audioconvert",
        "audioresample",
        "volume",
        "autoaudiosink",
        #[cfg(target_os = "macos")]
        "glimagesinkelement",
        #[cfg(target_os = "macos")]
        "osxaudiosink",
        #[cfg(target_os = "linux")]
        "gtkglsink",
    ];
    let missing: Vec<_> = required
        .iter()
        .filter(|factory| gstreamer::ElementFactory::find(factory).is_none())
        .copied()
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "Faltan elementos GStreamer: {}",
            missing.join(", ")
        ));
    }
    let decoders: Vec<_> = ["vtdec_hw", "vtdec", "avdec_h264", "openh264dec"]
        .into_iter()
        .filter(|factory| gstreamer::ElementFactory::find(factory).is_some())
        .collect();
    if decoders.is_empty() {
        return Err("No hay un decodificador H.264 disponible".into());
    }
    println!(
        "{}",
        json!({
            "result": "pass", "version": env!("CARGO_PKG_VERSION"),
            "architecture": std::env::consts::ARCH,
        "gstreamer": gstreamer::version_string().to_string(),
            "requiredFactories": required.len(), "decoders": decoders,
            "networkConnections": 0, "hidReportsSent": 0
        })
    );
    Ok(())
}

pub fn run() {
    if let Err(error) = runtime_environment::configure() {
        eprintln!("No se pudo preparar el motor multimedia: {error}");
        std::process::exit(1);
    }
    if std::env::args().any(|argument| argument == "--check-runtime") {
        if let Err(error) = check_runtime() {
            eprintln!("Runtime: {error}");
            std::process::exit(1);
        }
        return;
    }
    tauri::Builder::default()
        .setup(|app| {
            let window = app
                .get_webview_window("main")
                .ok_or("No se creó la ventana principal")?;
            let runtime = Arc::new(Runtime {
                app: app.handle().clone(),
                status: Mutex::new(AppStatus {
                    state: "disconnected".into(),
                    phase: "idle".into(),
                    video_state: "waiting".into(),
                    video_paused: false,
                    message: "Listo para conectar".into(),
                    address: "https://192.168.100.8/".into(),
                    capture: false,
                    capture_requested: false,
                    input_enabled: true,
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
                video_health: Mutex::new(health::FrameHealth::new(Instant::now())),
                video_count_offset: Mutex::new((0, 0)),
                source_notice: Mutex::new(None),
                shutdown: AtomicU8::new(0),
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
                            s.input_enabled = runtime.input().is_enabled();
                            s.input_backend = capture.backend;
                            s.message = capture.message;
                        });
                    }
                }),
            );
            #[cfg(target_os = "macos")]
            input.initialize_macos(window.clone())?;
            let _ = runtime.input.set(input);
            let capture = Arc::downgrade(&runtime);
            video_host::install(
                &window,
                Arc::new(move || {
                    if let Some(runtime) = capture.upgrade() {
                        if let Err(error) = runtime.capture(true) {
                            runtime.update(|s| s.message = error);
                        }
                    }
                }),
            )?;
            video_host::show_placeholder();
            let weak = Arc::downgrade(&runtime);
            let layout_window = window.clone();
            let previous_fullscreen = AtomicBool::new(window.is_fullscreen().unwrap_or(false));
            window.on_window_event(move |event| {
                if let Some(runtime) = weak.upgrade() {
                    match event {
                        tauri::WindowEvent::Focused(false) if runtime.input().is_captured() => {
                            let _ = runtime.capture(false);
                        }
                        tauri::WindowEvent::Resized(_) => {
                            if let Ok(fullscreen) = layout_window.is_fullscreen() {
                                let was_fullscreen =
                                    previous_fullscreen.swap(fullscreen, Ordering::AcqRel);
                                let _ = layout_window.emit("kvm://fullscreen", fullscreen);
                                if video_host::is_dashboard_fullscreen() {
                                    if was_fullscreen && !fullscreen {
                                        let _ = video_host::set_dashboard_fullscreen(
                                            &layout_window,
                                            false,
                                        );
                                    }
                                } else {
                                    let _ = video_host::set_toolbar_visible(
                                        &layout_window,
                                        !fullscreen,
                                    );
                                }
                            }
                        }
                        tauri::WindowEvent::CloseRequested { api, .. } => {
                            api.prevent_close();
                            request_shutdown(runtime);
                        }
                        _ => {}
                    }
                }
            });
            app.manage(sunshine::SunshineState::default());
            app.manage(preferences::PreferenceState::default());
            app.manage(AppState(runtime));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            preferences::get_preferences,
            preferences::save_preferences,
            preferences::load_saved_password,
            preferences::save_saved_password,
            preferences::delete_saved_password,
            sunshine::inspect_sunshine,
            sunshine::pair_sunshine,
            sunshine::get_sunshine_status,
            sunshine::connect_sunshine,
            sunshine::disconnect_sunshine,
            sunshine::select_video_source,
            inspect_device,
            trust_device,
            connect_device,
            disconnect_device,
            get_status,
            set_input_settings,
            set_video_settings,
            set_kvm_video_paused,
            set_capture,
            set_kvm_input_enabled,
            set_fullscreen,
            set_dashboard_expanded,
            set_dashboard_fullscreen
        ])
        .build(app_context())
        .expect("No se pudo iniciar KVM Client")
        .run(|app, event| {
            if let tauri::RunEvent::ExitRequested { api, .. } = event {
                if let Some(state) = app.try_state::<AppState>() {
                    if state.0.shutdown.load(Ordering::Acquire) != 2 {
                        api.prevent_exit();
                        request_shutdown(state.0.clone());
                    }
                }
            }
        });
}
