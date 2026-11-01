//! Sunshine is a receive-only media source. No input API is exposed here.
mod http_client;
#[cfg(test)]
mod tls_tests;
use crate::{
    media::{MediaEngine, MediaEvent},
    video_host, AppState,
};
use gst::prelude::*;
use gstreamer as gst;
use gstreamer_app::AppSrc;
use moonlight_common::{
    crypto::rustcrypto::RustCryptoBackend,
    high::std::MoonlightHost,
    http::{
        pair::{PairPin, PairingCryptoBackend},
        ClientIdentifier, ClientSecret, ServerIdentifier,
    },
    stream::{
        audio::{AudioConfig, AudioDecoder, AudioFrame, OpusMultistreamConfig},
        control::ActiveGamepads,
        debug::NullListener,
        std::MoonlightStream,
        video::{
            ColorRange, ColorSpace, DecodeResult, VideoDecodeUnit, VideoDecoder, VideoFormat,
            VideoFormats, VideoSetup,
        },
        AesIv, AesKey, EncryptionFlags, MoonlightStreamSettings, StreamingConfig,
    },
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    net::Ipv4Addr,
    path::{Path, PathBuf},
    str::FromStr,
    sync::{mpsc, Arc, Mutex},
    thread,
    time::Duration,
};
use tauri::{Emitter, Manager, State};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Endpoint {
    pub ip: String,
    pub port: u16,
}
impl Endpoint {
    pub(crate) fn validate(&self) -> Result<(), String> {
        let ip = Ipv4Addr::from_str(&self.ip)
            .map_err(|_| "Introduce una dirección IPv4 válida para Sunshine")?;
        if self.port == 0 || ip.is_unspecified() || ip.is_multicast() || ip.is_broadcast() {
            return Err("La dirección o el puerto Sunshine no son válidos".into());
        }
        Ok(())
    }
    fn path(&self, root: &Path) -> PathBuf {
        root.join(format!("sunshine-{}-{}.json", self.ip, self.port))
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SunshineNetwork {
    #[default]
    Auto,
    Local,
    Remote,
}

impl SunshineNetwork {
    fn streaming_config(self) -> StreamingConfig {
        match self {
            Self::Auto => StreamingConfig::Auto,
            Self::Local => StreamingConfig::Local,
            Self::Remote => StreamingConfig::Remote,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct SunshineStreamSettings {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub network: SunshineNetwork,
    pub audio_enabled: bool,
    pub volume: u8,
    pub host_audio: bool,
}

impl Default for SunshineStreamSettings {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            fps: 60,
            bitrate_kbps: 15000,
            network: SunshineNetwork::Auto,
            audio_enabled: true,
            volume: 100,
            host_audio: true,
        }
    }
}

impl SunshineStreamSettings {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if !(128..=7680).contains(&self.width) || !(128..=4320).contains(&self.height) {
            return Err("La resolución Sunshine debe estar entre 128×128 y 7680×4320".into());
        }
        if ![30, 60, 90, 120].contains(&self.fps) {
            return Err("La frecuencia Sunshine debe ser 30, 60, 90 o 120 FPS".into());
        }
        if !(1000..=150000).contains(&self.bitrate_kbps) {
            return Err("El bitrate Sunshine debe estar entre 1 y 150 Mbps".into());
        }
        if self.volume > 100 {
            return Err("El volumen Sunshine debe estar entre 0 y 100 %".into());
        }
        Ok(())
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SunshineApp {
    id: u32,
    title: String,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostInfo {
    name: String,
    paired: bool,
    apps: Vec<SunshineApp>,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SunshineStatus {
    pub state: String,
    pub message: String,
    pub source: String,
}
impl Default for SunshineStatus {
    fn default() -> Self {
        Self {
            state: "disconnected".into(),
            message: String::new(),
            source: "kvm".into(),
        }
    }
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Identity {
    unique_id: String,
    certificate: String,
    private_key: String,
    server_certificate: String,
}
struct Session {
    stop: mpsc::Sender<()>,
    worker: thread::JoinHandle<()>,
    media: MediaEngine,
}
pub struct SunshineState {
    operation: tokio::sync::Mutex<()>,
    session: Mutex<Option<Session>>,
    status: Mutex<SunshineStatus>,
}
impl Default for SunshineState {
    fn default() -> Self {
        Self {
            operation: tokio::sync::Mutex::new(()),
            session: Mutex::new(None),
            status: Mutex::new(SunshineStatus::default()),
        }
    }
}
impl SunshineState {
    pub fn media(&self) -> Option<MediaEngine> {
        self.session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|s| s.media.clone())
    }
    pub fn selected_media(&self) -> Option<MediaEngine> {
        let selected = self.status.lock().unwrap_or_else(|e| e.into_inner()).source == "sunshine";
        if selected {
            self.media()
        } else {
            None
        }
    }
    fn publish(&self, app: &tauri::AppHandle, state: &str, message: &str) {
        let snapshot = {
            let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
            if state == "error" {
                status.source = "kvm".into();
                let _ = app.run_on_main_thread(|| video_host::select_sunshine(false));
            }
            status.state = state.into();
            status.message = message.into();
            status.clone()
        };
        let _ = app.emit("kvm://sunshine", snapshot);
    }
    async fn stop(&self) -> Result<(), String> {
        let session = self
            .session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(session) = session {
            let _ = session.stop.send(());
            tauri::async_runtime::spawn_blocking(move || {
                let _ = session.worker.join();
                session.media.shutdown();
            })
            .await
            .map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}
fn root(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    app.path().app_config_dir().map_err(|e| e.to_string())
}
fn host(
    endpoint: &Endpoint,
    root: &Path,
) -> Result<MoonlightHost<http_client::PinnedClient>, String> {
    endpoint.validate()?;
    let identity = match fs::read(endpoint.path(root)) {
        Ok(bytes) => Some(
            serde_json::from_slice::<Identity>(&bytes)
                .map_err(|_| "La identidad Sunshine guardada no es válida")?,
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("No se pudo leer la identidad Sunshine: {e}")),
    };
    let client = MoonlightHost::<http_client::PinnedClient>::new(
        endpoint.ip.clone(),
        endpoint.port,
        identity.as_ref().map(|i| i.unique_id.clone()),
    )
    .map_err(|e| e.to_string())?;
    if let Some(i) = identity {
        client
            .set_identity(
                ClientIdentifier::from_pem(
                    pem::Pem::from_str(&i.certificate)
                        .map_err(|_| "Certificado Sunshine inválido")?,
                ),
                ClientSecret::from_pem(
                    pem::Pem::from_str(&i.private_key)
                        .map_err(|_| "Identidad Sunshine inválida")?,
                ),
                ServerIdentifier::from_pem(
                    pem::Pem::from_str(&i.server_certificate)
                        .map_err(|_| "Certificado del servidor Sunshine inválido")?,
                ),
            )
            .map_err(|e| format!("No se pudo autenticar Sunshine: {e}"))?;
    }
    Ok(client)
}
fn host_info(client: &MoonlightHost<http_client::PinnedClient>) -> Result<HostInfo, String> {
    let name = client.host_name().map_err(|e| e.to_string())?;
    let paired = client.identity().is_some() && client.is_paired().map_err(|e| e.to_string())?;
    let apps = if paired {
        client
            .app_list()
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|a| SunshineApp {
                id: a.id.0,
                title: a.title,
            })
            .collect()
    } else {
        vec![]
    };
    Ok(HostInfo { name, paired, apps })
}
fn save_identity(path: &Path, identity: &Identity) -> Result<(), String> {
    fs::create_dir_all(path.parent().ok_or("Ruta de identidad inválida")?)
        .map_err(|e| e.to_string())?;
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary).map_err(|e| e.to_string())?;
    use std::io::Write;
    let result = (|| {
        let bytes =
            serde_json::to_vec(identity).map_err(|_| "No se pudo guardar la identidad Sunshine")?;
        file.write_all(&bytes).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        fs::rename(&temporary, path).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}
#[tauri::command]
pub async fn inspect_sunshine(
    app: tauri::AppHandle,
    endpoint: Endpoint,
) -> Result<HostInfo, String> {
    endpoint.validate()?;
    let root = root(&app)?;
    tauri::async_runtime::spawn_blocking(move || host_info(&host(&endpoint, &root)?))
        .await
        .map_err(|e| e.to_string())?
}
#[tauri::command]
pub async fn pair_sunshine(
    app: tauri::AppHandle,
    state: State<'_, SunshineState>,
    endpoint: Endpoint,
    pin: String,
) -> Result<HostInfo, String> {
    endpoint.validate()?;
    if pin.len() != 4 || !pin.bytes().all(|c| c.is_ascii_digit()) {
        return Err("El PIN debe contener cuatro dígitos".into());
    }
    let _guard = state.operation.lock().await;
    if state
        .session
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_some()
    {
        return Err("Desconecta Sunshine antes de emparejar".into());
    }
    let root = root(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        let unique_id = uuid::Uuid::new_v4().to_string();
        let client = MoonlightHost::<http_client::PinnedClient>::new(
            endpoint.ip.clone(),
            endpoint.port,
            Some(unique_id.clone()),
        )
        .map_err(|e| e.to_string())?;
        let crypto = RustCryptoBackend;
        let (cert, secret) = crypto
            .generate_client_identity()
            .map_err(|e| e.to_string())?;
        let digits: Vec<_> = pin.bytes().map(|c| c - b'0').collect();
        let pin = PairPin::new(digits[0], digits[1], digits[2], digits[3]).ok_or("PIN inválido")?;
        let paired = client.pair(&cert, &secret, "KVM Client".into(), pin, crypto);
        // The library sets identity only after all PIN/certificate checks pass,
        // then refreshes serverinfo. Preserve that verified identity even when
        // the subsequent refresh fails, so a retry does not require a new PIN.
        let (_, _, server) = client.identity().ok_or_else(|| match &paired {
            Err(error) => format!("No se pudo emparejar Sunshine: {error}"),
            Ok(()) => "Sunshine no devolvió su certificado".into(),
        })?;
        save_identity(
            &endpoint.path(&root),
            &Identity {
                unique_id,
                certificate: cert.to_pem().to_string(),
                private_key: secret.to_pem().to_string(),
                server_certificate: server.to_pem().to_string(),
            },
        )?;
        paired.map_err(|e| format!(
            "Emparejamiento guardado, pero no se pudo consultar Sunshine. Pulsa Buscar de nuevo: {e}"
        ))?;
        host_info(&client).map_err(|e| format!(
            "Emparejamiento guardado, pero no se pudieron consultar las aplicaciones. Pulsa Buscar de nuevo: {e}"
        ))
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
pub fn get_sunshine_status(state: State<'_, SunshineState>) -> SunshineStatus {
    state
        .status
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

// Two decoders receive media only. There are deliberately no calls to send_input.
struct Video {
    media: MediaEngine,
    source: AppSrc,
}
impl VideoDecoder for Video {
    fn setup(&mut self, setup: VideoSetup) -> i32 {
        if setup.format == VideoFormat::H264 {
            0
        } else {
            -1
        }
    }
    fn start(&mut self) {
        let _ = self.media.start_external();
    }
    fn submit_decode_unit(&mut self, unit: VideoDecodeUnit<&[u8]>) -> DecodeResult {
        if self.source.current_level_bytes() > 8 * 1024 * 1024 {
            return DecodeResult::NeedIdr;
        }
        let mut bytes = Vec::new();
        for buffer in unit.buffers {
            bytes.extend_from_slice(buffer.data);
        }
        match self.source.push_buffer(gst::Buffer::from_slice(bytes)) {
            Ok(_) => DecodeResult::Ok,
            Err(_) => DecodeResult::NeedIdr,
        }
    }
    fn stop(&mut self) {
        self.media.shutdown();
    }
    fn supported_formats(&self) -> VideoFormats {
        VideoFormats::H264
    }
}
struct Audio {
    pipeline: gst::Pipeline,
    source: AppSrc,
}
impl Audio {
    fn new(enabled: bool, volume: u8) -> Result<Self, String> {
        let pipeline = gst::Pipeline::with_name("sunshine-audio");
        let source = AppSrc::builder()
            .is_live(true)
            .format(gst::Format::Time)
            .do_timestamp(true)
            .block(false)
            .max_bytes(512 * 1024)
            .build();
        let mut nodes: Vec<gst::Element> = vec![source.clone().upcast()];
        let factories: &[&str] = if enabled {
            &[
                "opusdec",
                "audioconvert",
                "audioresample",
                "volume",
                "autoaudiosink",
            ]
        } else {
            &["fakesink"]
        };
        for factory in factories {
            let node = gst::ElementFactory::make(*factory)
                .name(if *factory == "volume" {
                    "sunshine-volume"
                } else {
                    *factory
                })
                .build()
                .map_err(|e| format!("Falta audio GStreamer {factory}: {e}"))?;
            if *factory == "volume" {
                node.set_property("volume", f64::from(volume) / 100.0);
            } else if *factory == "fakesink" {
                node.set_property("sync", false);
                node.set_property("async", false);
            }
            nodes.push(node);
        }
        pipeline.add_many(&nodes).map_err(|e| e.to_string())?;
        gst::Element::link_many(&nodes).map_err(|e| e.to_string())?;
        Ok(Self { pipeline, source })
    }
}
impl Drop for Audio {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}
impl AudioDecoder for Audio {
    fn setup(&mut self, _config: AudioConfig, stream: OpusMultistreamConfig) -> i32 {
        if stream.channel_count != 2 || stream.streams != 1 || stream.coupled_streams != 1 {
            return -1;
        }
        self.source.set_caps(Some(
            &gst::Caps::builder("audio/x-opus")
                .field("rate", stream.sample_rate as i32)
                .field("channels", 2i32)
                .field("channel-mapping-family", 0i32)
                .build(),
        ));
        0
    }
    fn start(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Playing);
    }
    fn decode_and_play_sample(&mut self, frame: AudioFrame<&[u8]>) {
        if self.source.current_level_bytes() < 512 * 1024 {
            let _ = self
                .source
                .push_buffer(gst::Buffer::from_slice(frame.buffer.to_vec()));
        }
    }
    fn stop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
    fn config(&self) -> AudioConfig {
        AudioConfig::STEREO
    }
}
fn settings(stream: SunshineStreamSettings) -> MoonlightStreamSettings {
    MoonlightStreamSettings {
        width: stream.width,
        height: stream.height,
        fps: stream.fps,
        fps_x100: stream.fps * 100,
        bitrate: stream.bitrate_kbps,
        packet_size: 1024,
        encryption_flags: EncryptionFlags::all(),
        streaming_remotely: stream.network.streaming_config(),
        sops: false,
        hdr: false,
        supported_video_formats: VideoFormats::H264,
        color_space: ColorSpace::Rec709,
        color_range: ColorRange::Limited,
        local_audio_play_mode: stream.host_audio,
        audio_config: AudioConfig::STEREO,
        gamepads_attached: ActiveGamepads::empty(),
        gamepads_persist_after_disconnect: false,
        enable_mic: false,
    }
}

#[tauri::command]
pub async fn connect_sunshine(
    app: tauri::AppHandle,
    state: State<'_, SunshineState>,
    kvm: State<'_, AppState>,
    endpoint: Endpoint,
    app_id: u32,
    stream: Option<SunshineStreamSettings>,
) -> Result<(), String> {
    endpoint.validate()?;
    let stream = stream.unwrap_or_default();
    stream.validate()?;
    let _guard = state.operation.lock().await;
    if kvm.0.status.lock().unwrap_or_else(|e| e.into_inner()).state != "connected" {
        return Err("Conecta PicoKVM para mantener el teclado y mouse por KVM".into());
    }
    select_source(&app, &state, false).await?;
    state.stop().await?;
    state.publish(
        &app,
        "connecting",
        if stream.audio_enabled {
            "Conectando video y audio Sunshine…"
        } else {
            "Conectando video Sunshine sin audio…"
        },
    );
    let result = connect_impl(&app, &state, endpoint, app_id, stream).await;
    if let Err(error) = &result {
        state.publish(&app, "error", error);
    }
    result
}
async fn connect_impl(
    app: &tauri::AppHandle,
    state: &SunshineState,
    endpoint: Endpoint,
    app_id: u32,
    stream_settings: SunshineStreamSettings,
) -> Result<(), String> {
    let root = root(app)?;
    let (created, receive) = tokio::sync::oneshot::channel();
    let callback_app = app.clone();
    let video_settings = *app
        .state::<AppState>()
        .0
        .video_settings
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    app.run_on_main_thread(move || {
        let callback = Arc::new(move |event| {
            if let MediaEvent::Error(error) = event {
                let state = callback_app.state::<SunshineState>();
                state.publish(&callback_app, "error", &error);
            }
        });
        let result = MediaEngine::external(callback).and_then(|(media, source)| {
            media.configure(video_settings)?;
            video_host::attach_sunshine(&media)?;
            Ok((media, source))
        });
        if let Err(Ok((media, _))) = created.send(result) {
            tauri::async_runtime::spawn_blocking(move || media.shutdown());
        }
    })
    .map_err(|e| e.to_string())?;
    let (media, source) = receive.await.map_err(|e| e.to_string())??;
    let worker_media = media.clone();
    let (stop, cancelled) = mpsc::channel();
    let (ready, started) = tokio::sync::oneshot::channel();
    let worker_app = app.clone();
    let worker = thread::Builder::new()
        .name("sunshine-receiver".into())
        .spawn(move || {
            let result = (|| {
                let client = host(&endpoint, &root)
                    .map_err(|e| format!("No se pudo verificar el equipo Sunshine: {e}"))?;
                if !client.is_paired().map_err(|e| e.to_string())? {
                    return Err("Empareja Sunshine antes de conectar".into());
                }
                if !client
                    .app_list()
                    .map_err(|e| format!("No se pudo consultar la lista de aplicaciones: {e}"))?
                    .iter()
                    .any(|a| a.id.0 == app_id)
                {
                    return Err("La aplicación Sunshine seleccionada ya no está disponible".into());
                }
                let crypto = RustCryptoBackend;
                let mut options = settings(stream_settings);
                options
                    .adjust_for_server(
                        client.version().map_err(|e| e.to_string())?,
                        &client.gfe_version().map_err(|e| e.to_string())?,
                        client
                            .server_codec_mode_support()
                            .map_err(|e| e.to_string())?,
                    )
                    .map_err(|e| e.to_string())?;
                let key = AesKey::new_random(&crypto).map_err(|e| e.to_string())?;
                let iv = AesIv::new_random(&crypto).map_err(|e| e.to_string())?;
                let audio = Audio::new(stream_settings.audio_enabled, stream_settings.volume)
                    .map_err(|e| format!("No se pudo preparar el audio de Sunshine: {e}"))?;
                let config = client
                    .start_stream(
                        moonlight_common::AppId(app_id),
                        &options,
                        key,
                        iv,
                        MoonlightStream::launch_query_parameters(),
                    )
                    .map_err(|e| format!("Sunshine no pudo iniciar la aplicación: {e}"))?;
                let stream = MoonlightStream::connect(
                    config,
                    options,
                    Video {
                        media: worker_media.clone(),
                        source,
                    },
                    audio,
                    NullListener,
                    Arc::new(crypto),
                )
                .map_err(|e| format!("No se pudo iniciar la transmisión Sunshine: {e}"))?;
                Ok(stream)
            })();
            match result {
                Err(error) => {
                    let _ = ready.send(Err(error));
                }
                Ok(stream) => {
                    if ready.send(Ok(())).is_ok() {
                        loop {
                            match cancelled.recv_timeout(Duration::from_millis(100)) {
                                Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                                Err(mpsc::RecvTimeoutError::Timeout) if stream.is_stopped() => {
                                    let state = worker_app.state::<SunshineState>();
                                    state.publish(
                                        &worker_app,
                                        "error",
                                        "Sunshine se desconectó; puedes volver al video PicoKVM",
                                    );
                                    break;
                                }
                                Err(_) => {}
                            }
                        }
                    }
                    stream.stop();
                }
            }
            worker_media.shutdown();
        })
        .map_err(|e| e.to_string())?;
    *state.session.lock().unwrap_or_else(|e| e.into_inner()) = Some(Session {
        stop,
        worker,
        media,
    });
    // A settings change can occur while the stream is being created. Serialize
    // this resync with set_video_settings so the new source gets the latest profile.
    {
        let kvm = app.state::<AppState>();
        let settings = kvm
            .0
            .video_settings
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(media) = state.media() {
            media.configure(*settings)?;
        }
    }
    match started.await.map_err(|e| e.to_string())? {
        Ok(()) => {
            state.publish(
                app,
                "connected",
                if stream_settings.audio_enabled {
                    "Sunshine conectado · teclado y mouse por PicoKVM"
                } else {
                    "Sunshine conectado sin audio · teclado y mouse por PicoKVM"
                },
            );
            Ok(())
        }
        Err(error) => {
            state.stop().await?;
            Err(error)
        }
    }
}
#[tauri::command]
pub async fn disconnect_sunshine(
    app: tauri::AppHandle,
    state: State<'_, SunshineState>,
) -> Result<(), String> {
    let _guard = state.operation.lock().await;
    select_source(&app, &state, false).await?;
    state.stop().await?;
    state.publish(&app, "disconnected", "Sunshine desconectado");
    Ok(())
}
#[tauri::command]
pub async fn select_video_source(
    app: tauri::AppHandle,
    state: State<'_, SunshineState>,
    source: String,
) -> Result<(), String> {
    let _guard = state.operation.lock().await;
    if source != "kvm" && source != "sunshine" {
        return Err("Fuente de video inválida".into());
    }
    let selected = source == "sunshine";
    if selected && state.status.lock().unwrap_or_else(|e| e.into_inner()).state != "connected" {
        return Err("Conecta Sunshine antes de seleccionar su video".into());
    }
    select_source(&app, &state, selected).await
}
async fn select_source(
    app: &tauri::AppHandle,
    state: &SunshineState,
    selected: bool,
) -> Result<(), String> {
    let (done, receive) = tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || {
        video_host::select_sunshine(selected);
        let _ = done.send(());
    })
    .map_err(|e| e.to_string())?;
    receive.await.map_err(|e| e.to_string())?;
    let snapshot = {
        let mut s = state.status.lock().unwrap_or_else(|e| e.into_inner());
        s.source = if selected { "sunshine" } else { "kvm" }.into();
        s.clone()
    };
    let _ = app.emit("kvm://sunshine", snapshot);
    Ok(())
}
pub async fn shutdown(app: &tauri::AppHandle) {
    let state = app.state::<SunshineState>();
    let _guard = state.operation.lock().await;
    let _ = state.stop().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires a paired Sunshine server and explicit environment configuration"]
    fn saved_identity_authenticates_repeated_server_queries() {
        let endpoint = Endpoint {
            ip: std::env::var("KVM_SUNSHINE_HOST").expect("set KVM_SUNSHINE_HOST"),
            port: 47989,
        };
        let root = std::env::var("KVM_SUNSHINE_CONFIG_DIR")
            .expect("set KVM_SUNSHINE_CONFIG_DIR to the existing identity directory");
        assert!(
            endpoint.path(Path::new(&root)).is_file(),
            "no saved identity"
        );
        let client = host(&endpoint, Path::new(&root)).unwrap();
        for _ in 0..3 {
            client.update().unwrap();
            assert!(host_info(&client).unwrap().paired);
        }
    }

    #[test]
    fn endpoint_rejects_urls_and_non_unicast_addresses() {
        for ip in [
            "https://192.168.1.2",
            "localhost/launch",
            "0.0.0.0",
            "255.255.255.255",
            "224.0.0.1",
        ] {
            assert!(Endpoint {
                ip: ip.into(),
                port: 47989
            }
            .validate()
            .is_err());
        }
        assert!(Endpoint {
            ip: "192.168.1.2".into(),
            port: 47989
        }
        .validate()
        .is_ok());
        assert!(Endpoint {
            ip: "192.168.1.2".into(),
            port: 0
        }
        .validate()
        .is_err());
    }
    #[test]
    fn identity_save_is_atomic_and_private() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("identity.json");
        let identity = Identity {
            unique_id: "client".into(),
            certificate: "cert".into(),
            private_key: "secret".into(),
            server_certificate: "server".into(),
        };
        save_identity(&path, &identity).unwrap();
        let read: Identity = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(read.private_key, "secret");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert_eq!(fs::read_dir(folder.path()).unwrap().count(), 1);
    }

    #[test]
    fn stream_is_h264_stereo_without_microphone_or_gamepads() {
        let s = settings(SunshineStreamSettings::default());
        assert_eq!(s.supported_video_formats, VideoFormats::H264);
        assert_eq!(s.audio_config, AudioConfig::STEREO);
        assert!(!s.enable_mic);
        assert!(s.gamepads_attached.is_empty());
        assert_eq!(
            (s.width, s.height, s.fps, s.fps_x100),
            (1920, 1080, 60, 6000)
        );
        assert_eq!(s.bitrate, 15000);
        assert_eq!(s.streaming_remotely, StreamingConfig::Auto);
        assert!(s.local_audio_play_mode);
    }

    #[test]
    fn requested_stream_settings_map_to_the_moonlight_session() {
        let request = SunshineStreamSettings {
            width: 2560,
            height: 1440,
            fps: 120,
            bitrate_kbps: 45000,
            network: SunshineNetwork::Remote,
            audio_enabled: false,
            volume: 30,
            host_audio: false,
        };
        request.validate().unwrap();
        let s = settings(request);
        assert_eq!(
            (s.width, s.height, s.fps, s.fps_x100),
            (2560, 1440, 120, 12000)
        );
        assert_eq!(s.bitrate, 45000);
        assert_eq!(s.streaming_remotely, StreamingConfig::Remote);
        assert!(!s.local_audio_play_mode);
        assert_eq!(
            settings(SunshineStreamSettings {
                network: SunshineNetwork::Local,
                ..request
            })
            .streaming_remotely,
            StreamingConfig::Local
        );
    }

    #[test]
    fn invalid_stream_settings_are_rejected_before_connecting() {
        let valid = SunshineStreamSettings::default();
        for invalid in [
            SunshineStreamSettings {
                width: 127,
                ..valid
            },
            SunshineStreamSettings {
                width: 7681,
                ..valid
            },
            SunshineStreamSettings {
                height: 127,
                ..valid
            },
            SunshineStreamSettings {
                height: 4321,
                ..valid
            },
            SunshineStreamSettings { fps: 59, ..valid },
            SunshineStreamSettings {
                bitrate_kbps: 999,
                ..valid
            },
            SunshineStreamSettings {
                bitrate_kbps: 150001,
                ..valid
            },
            SunshineStreamSettings {
                volume: 101,
                ..valid
            },
        ] {
            assert!(invalid.validate().is_err(), "accepted {invalid:?}");
        }
    }

    #[test]
    fn stream_settings_serde_defaults_are_compatible_with_preferences() {
        let restored: SunshineStreamSettings = serde_json::from_value(serde_json::json!({
            "width": 1280, "height": 720, "network": "local", "audioEnabled": false
        }))
        .unwrap();
        restored.validate().unwrap();
        assert_eq!((restored.width, restored.height), (1280, 720));
        assert_eq!(restored.network, SunshineNetwork::Local);
        assert!(!restored.audio_enabled);
        assert_eq!(
            (restored.fps, restored.bitrate_kbps, restored.volume),
            (60, 15000, 100)
        );
        assert!(restored.host_audio);
        let serialized = serde_json::to_value(restored).unwrap();
        assert_eq!(serialized["network"], "local");
        assert_eq!(serialized["audioEnabled"], false);
        assert_eq!(serialized["bitrateKbps"], 15000);
    }

    #[test]
    fn muted_audio_pipeline_does_not_open_an_audio_output() {
        gst::init().unwrap();
        let audio = Audio::new(false, 100).unwrap();
        assert!(audio.pipeline.by_name("autoaudiosink").is_none());
        assert!(audio.pipeline.by_name("opusdec").is_none());
        let discard = audio.pipeline.by_name("fakesink").unwrap();
        assert!(!discard.property::<bool>("sync"));
        assert!(!discard.property::<bool>("async"));
    }

    #[test]
    fn requested_audio_volume_is_applied_to_the_output_pipeline() {
        gst::init().unwrap();
        let audio = Audio::new(true, 35).unwrap();
        let volume = audio.pipeline.by_name("sunshine-volume").unwrap();
        // The plugin may store volume at single precision internally.
        assert!((volume.property::<f64>("volume") - 0.35).abs() < 1e-6);
        assert!(audio.pipeline.by_name("autoaudiosink").is_some());
    }
}

/// Native render/switch diagnostic. Uses local H.264 frames, no host or HID.
pub async fn synthetic_check(app: &tauri::AppHandle) -> Result<(), String> {
    let errors = Arc::new(Mutex::new(Vec::<String>::new()));
    let callback_errors = errors.clone();
    let (done, receive) = tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || {
        let result = (|| {
            let callback: crate::media::MediaCallback = Arc::new(move |event| {
                if let MediaEvent::Error(error) = event {
                    callback_errors.lock().unwrap().push(error);
                }
            });
            let (kvm, kvm_source) = MediaEngine::external(callback.clone())?;
            let (sunshine, sunshine_source) = MediaEngine::external(callback)?;
            video_host::attach(&kvm)?;
            video_host::attach_sunshine(&sunshine)?;
            video_host::show_notice(None);
            kvm.start_external()?;
            sunshine.start_external()?;
            Ok::<_, String>((kvm, kvm_source, sunshine, sunshine_source))
        })();
        let _ = done.send(result);
    })
    .map_err(|e| e.to_string())?;
    let (kvm, kvm_source, sunshine, sunshine_source) =
        receive.await.map_err(|e| e.to_string())??;
    let publisher = gst::parse::launch("videotestsrc is-live=true ! video/x-raw,width=640,height=360,framerate=30/1 ! x264enc tune=zerolatency speed-preset=ultrafast key-int-max=30 byte-stream=true ! h264parse ! video/x-h264,stream-format=byte-stream,alignment=au ! appsink name=encoded sync=false")
        .map_err(|e| e.to_string())?.dynamic_cast::<gst::Pipeline>().map_err(|_| "No se creó el emisor sintético")?;
    let sink = publisher
        .by_name("encoded")
        .ok_or("No se creó appsink")?
        .dynamic_cast::<gstreamer_app::AppSink>()
        .map_err(|_| "Appsink inválido")?;
    sink.set_callbacks(
        gstreamer_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                let bytes = sample
                    .buffer()
                    .ok_or(gst::FlowError::Error)?
                    .map_readable()
                    .map_err(|_| gst::FlowError::Error)?;
                kvm_source.push_buffer(gst::Buffer::from_slice(bytes.as_slice().to_vec()))?;
                sunshine_source.push_buffer(gst::Buffer::from_slice(bytes.as_slice().to_vec()))?;
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );
    let result = async {
        publisher.set_state(gst::State::Playing).map_err(|e| e.to_string())?;
        for selected in [false, true, false, true, false] {
            let (sent, applied) = tokio::sync::oneshot::channel();
            app.run_on_main_thread(move || { video_host::select_sunshine(selected); let _ = sent.send(()); }).map_err(|e| e.to_string())?;
            applied.await.map_err(|e| e.to_string())?;
            tokio::time::sleep(Duration::from_millis(800)).await;
        }
        let kvm_frames = kvm.frame_counts(); let sunshine_frames = sunshine.frame_counts();
        let failures = errors.lock().unwrap().clone();
        if !failures.is_empty() { return Err(format!("Errores de render: {}", failures.join("; "))); }
        if kvm_frames.0 < 20 || sunshine_frames.0 < 20 || kvm_frames.1 == 0 || sunshine_frames.1 == 0 {
            return Err(format!("No se presentaron ambas fuentes: KVM={kvm_frames:?}, Sunshine={sunshine_frames:?}"));
        }
        println!("{}", serde_json::json!({"result":"pass","sunshineSynthetic":true,"switches":4,"kvmDecoded":kvm_frames.0,"kvmPresented":kvm_frames.1,"sunshineDecoded":sunshine_frames.0,"sunshinePresented":sunshine_frames.1,"hostConnections":0,"hidReportsSentToKvm":0}));
        Ok(())
    }.await;
    let _ = publisher.set_state(gst::State::Null);
    tauri::async_runtime::spawn_blocking(move || {
        kvm.shutdown();
        sunshine.shutdown();
    })
    .await
    .map_err(|e| e.to_string())?;
    result
}
