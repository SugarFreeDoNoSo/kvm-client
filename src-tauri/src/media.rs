//! Native WebRTC/H.264 receiver. GTK objects are only exposed on the GTK thread;
//! frame buffers never cross Tauri's JSON bridge.

use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex,
};
use std::time::Instant;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_sdp as gst_sdp;
use gstreamer_webrtc as gst_webrtc;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[cfg(target_os = "linux")]
const SHARP_SHADER: &str = include_str!("shaders/sharp.frag");
#[cfg(target_os = "linux")]
const SPATIAL_VERTEX_SHADER: &str = include_str!("shaders/spatial.vert");
#[cfg(target_os = "macos")]
const SHARP_SHADER: &str = include_str!("shaders/sharp-core.frag");
#[cfg(target_os = "macos")]
const SPATIAL_VERTEX_SHADER: &str = include_str!("shaders/spatial-core.vert");
const MAX_RPC_BUFFER: u64 = 32 * 1024;
#[cfg(target_os = "macos")]
const NEAREST_SHADER: &str = include_str!("shaders/nearest-core.frag");
#[cfg(target_os = "linux")]
const NEAREST_SHADER: &str = include_str!("shaders/nearest.frag");
#[cfg(target_os = "macos")]
const ORIGINAL_SHADER: &str = include_str!("shaders/original-core.frag");
#[cfg(target_os = "linux")]
const ORIGINAL_SHADER: &str = include_str!("shaders/original.frag");
#[cfg(target_os = "macos")]
const DESKTOP_SHADER: &str = include_str!("shaders/desktop-core.frag");
#[cfg(target_os = "linux")]
const DESKTOP_SHADER: &str = include_str!("shaders/desktop.frag");
#[cfg(target_os = "macos")]
const GAMING_SHADER: &str = include_str!("shaders/gaming-core.frag");
#[cfg(target_os = "linux")]
const GAMING_SHADER: &str = include_str!("shaders/gaming.frag");

pub type MediaCallback = Arc<dyn Fn(MediaEvent) + Send + Sync + 'static>;

#[derive(Debug, Clone)]
pub enum MediaEvent {
    Offer {
        sdp: String,
    },
    Ice {
        candidate: String,
        sdp_m_line_index: u32,
    },
    Rpc(Value),
    State(String),
    Decoder(String),
    Error(String),
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum VideoFilter {
    #[serde(rename = "original", alias = "bilinear")]
    Bilinear,
    Sharp,
    #[default]
    Desktop,
    Gaming,
    Nearest,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoSettings {
    pub filter: VideoFilter,
    pub sharpness: f32,
    #[serde(default)]
    pub native_size: bool,
    #[serde(default)]
    pub scale_factor: u8,
    #[serde(default = "default_anti_ringing")]
    pub anti_ringing: f32,
    #[serde(default = "default_edge_threshold")]
    pub edge_threshold: f32,
    #[serde(default = "default_true")]
    pub downscale_anti_aliasing: bool,
}

fn default_anti_ringing() -> f32 {
    1.0
}
fn default_edge_threshold() -> f32 {
    8.0 / 255.0
}
fn default_true() -> bool {
    true
}

impl Default for VideoSettings {
    fn default() -> Self {
        Self {
            filter: VideoFilter::Desktop,
            sharpness: 0.2,
            native_size: false,
            scale_factor: 0,
            anti_ringing: default_anti_ringing(),
            edge_threshold: default_edge_threshold(),
            downscale_anti_aliasing: true,
        }
    }
}

impl VideoSettings {
    pub fn validate(&self) -> Result<(), String> {
        if !self.sharpness.is_finite() || !(0.0..=1.0).contains(&self.sharpness) {
            return Err("La nitidez debe estar entre 0 y 1".into());
        }
        if ![0, 2, 3, 4].contains(&self.scale_factor)
            || (self.native_size && self.scale_factor != 0)
        {
            return Err("La escala debe ser Ajustar, 1:1, 2×, 3× o 4×".into());
        }
        if !self.anti_ringing.is_finite() || !(0.0..=1.0).contains(&self.anti_ringing) {
            return Err("El control de halos debe estar entre 0 y 1".into());
        }
        if !self.edge_threshold.is_finite()
            || !(1.0 / 255.0..=32.0 / 255.0).contains(&self.edge_threshold)
        {
            return Err("El umbral de bordes debe estar entre 1 y 32".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaStats {
    pub received_fps: Option<f64>,
    pub decoded_fps: Option<f64>,
    pub presented_fps: Option<f64>,
    pub rtt_ms: Option<f64>,
    pub packets_lost: Option<i64>,
    pub decoder: String,
    pub rpc_buffered_bytes: u64,
    pub input_width: u32,
    pub input_height: u32,
    pub output_width: u32,
    pub output_height: u32,
    pub filter_processing_ms: Option<f64>,
    pub sink_dropped_frames: Option<u64>,
}

#[derive(Default)]
struct Counters {
    received: AtomicU64,
    decoded: AtomicU64,
    filter_nanos: AtomicU64,
    filter_frames: AtomicU64,
}

struct Sample {
    at: Instant,
    received: u64,
    decoded: u64,
    rendered: u64,
    filter_nanos: u64,
    filter_frames: u64,
}

#[derive(Default)]
struct SpatialState {
    input: (u32, u32),
    pixel_aspect: (u32, u32),
    viewport: (u32, u32),
    negotiated: (u32, u32),
    requested: (u32, u32),
    settings: VideoSettings,
    applied_filter: Option<VideoFilter>,
}

fn target_size(
    input: (u32, u32),
    par: (u32, u32),
    viewport: (u32, u32),
    native: bool,
) -> (u32, u32) {
    if viewport.0 == 0 || viewport.1 == 0 {
        return input;
    }
    if native || input.0 == 0 || input.1 == 0 {
        return viewport;
    }
    let aspect = input.0 as f64 * par.0.max(1) as f64 / (input.1 as f64 * par.1.max(1) as f64);
    if viewport.0 as f64 / viewport.1 as f64 > aspect {
        (
            ((viewport.1 as f64 * aspect).round() as u32).max(1),
            viewport.1,
        )
    } else {
        (
            viewport.0,
            ((viewport.0 as f64 / aspect).round() as u32).max(1),
        )
    }
}

struct Inner {
    pipeline: gst::Pipeline,
    webrtc: gst::Element,
    video_gate: gst::Element,
    parser: gst::Element,
    decoder: Mutex<gst::Element>,
    render_queue: gst::Element,
    shader: gst::Element,
    output_caps: gst::Element,
    sink: gst::Element,
    rpc: Mutex<Option<gst_webrtc::WebRTCDataChannel>>,
    callback: MediaCallback,
    counters: Arc<Counters>,
    sample: Mutex<Sample>,
    cached_stats: Mutex<MediaStats>,
    alive: AtomicBool,
    started: AtomicBool,
    offered: AtomicBool,
    software_fallback: AtomicBool,
    stats_pending: AtomicBool,
    remote_ready: AtomicBool,
    pending_remote_ice: Mutex<Vec<(u32, String)>>,
    spatial: Mutex<SpatialState>,
    spatial_update_pending: AtomicBool,
    spatial_apply: Mutex<()>,
    filter_started: Mutex<Option<Instant>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::SeqCst);
        if let Some(channel) = self.rpc.get_mut().unwrap_or_else(|e| e.into_inner()).take() {
            channel.close();
        }
        // Cancellation may drop the last engine before the session owns it.
        // GStreamer elements must reach NULL before their final reference goes.
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

#[derive(Clone)]
pub struct MediaEngine(Arc<Inner>);

fn element(factory: &str, name: &str) -> Result<gst::Element, String> {
    gst::ElementFactory::make(factory)
        .name(name)
        .build()
        .map_err(|e| format!("Falta el componente GStreamer «{factory}» o no se pudo crear: {e}"))
}

fn choose_decoder(force_software: bool) -> Result<(gst::Element, String), String> {
    #[cfg(target_os = "linux")]
    let hardware_factories: &[&str] = &[
        "vah264dec",
        "vaapih264dec",
        "nvh264dec",
        "v4l2slh264dec",
        "avdec_h264",
        "openh264dec",
    ];
    #[cfg(target_os = "macos")]
    let hardware_factories: &[&str] = &["vtdec_hw", "vtdec", "avdec_h264", "openh264dec"];
    let factories: &[&str] = if force_software {
        &["avdec_h264", "openh264dec"]
    } else {
        hardware_factories
    };
    for factory in factories {
        if gst::ElementFactory::find(factory).is_none() {
            continue;
        }
        if let Ok(decoder) = element(factory, "kvm-decoder") {
            // Fast decoding is preferable to waiting for a decoder's private
            // output queue. Only set options the selected backend implements.
            if decoder.find_property("low-latency").is_some() {
                decoder.set_property("low-latency", true);
            }
            return Ok((decoder, factory.to_string()));
        }
    }
    Err("No hay un decodificador H.264 disponible. Ejecuta npm run doctor para revisar las dependencias.".into())
}

fn count_buffers(pad: &gst::Pad, counters: Arc<Counters>, decoded: bool) {
    pad.add_probe(gst::PadProbeType::BUFFER, move |_, _| {
        let counter = if decoded {
            &counters.decoded
        } else {
            &counters.received
        };
        counter.fetch_add(1, Ordering::Relaxed);
        gst::PadProbeReturn::Ok
    });
}

impl MediaEngine {
    /// Construct on the application's main thread after the native host exists.
    pub fn new(callback: MediaCallback) -> Result<Self, String> {
        gst::init().map_err(|e| format!("No se pudo iniciar GStreamer: {e}"))?;
        for required in [
            "webrtcbin",
            "nicesrc",
            "dtlssrtpdec",
            "sctpenc",
            "rtph264depay",
            "h264parse",
            "glupload",
            "glcolorconvert",
            "glshader",
            #[cfg(target_os = "linux")]
            "gtkglsink",
            #[cfg(target_os = "macos")]
            "glimagesinkelement",
        ] {
            if gst::ElementFactory::find(required).is_none() {
                return Err(format!("Falta el componente GStreamer «{required}». Ejecuta npm run doctor para revisar las dependencias."));
            }
        }

        let pipeline = gst::Pipeline::with_name("kvm-native-video");
        let webrtc = element("webrtcbin", "kvm-peer")?;
        webrtc.set_property_from_str("bundle-policy", "max-bundle");
        webrtc.set_property("latency", 10u32);
        let incoming = element("queue", "kvm-incoming")?;
        incoming.set_property("max-size-buffers", 128u32);
        incoming.set_property("max-size-bytes", 0u32);
        incoming.set_property("max-size-time", 100_000_000u64);
        // Keep the shared WebRTC peer/DataChannel alive while allowing the
        // compressed video branch to stop before depayloading and decoding.
        let video_gate = element("valve", "kvm-video-gate")?;
        video_gate.set_property_from_str("drop-mode", "forward-sticky-events");
        // Do not leak compressed RTP packets: that would corrupt H.264 frames.
        let depay = element("rtph264depay", "kvm-depay")?;
        if depay.find_property("request-keyframe").is_some() {
            depay.set_property("request-keyframe", true);
            depay.set_property("wait-for-keyframe", true);
        }
        let parser = element("h264parse", "kvm-parser")?;
        parser.set_property("config-interval", -1i32);
        let audio_discard = element("fakesink", "kvm-audio-discard")?;
        audio_discard.set_property("sync", false);
        audio_discard.set_property("async", false);
        let force_software = std::env::var("KVM_FORCE_SOFTWARE").as_deref() == Ok("1");
        let (decoder, decoder_name) = choose_decoder(force_software)?;
        let render_queue = element("queue", "kvm-latest-frame")?;
        render_queue.set_property("max-size-buffers", 1u32);
        render_queue.set_property("max-size-bytes", 0u32);
        render_queue.set_property("max-size-time", 0u64);
        render_queue.set_property_from_str("leaky", "downstream");
        let upload = element("glupload", "kvm-upload")?;
        let convert = element("glcolorconvert", "kvm-rgba")?;
        #[cfg(target_os = "macos")]
        let texture_caps = {
            // VideoToolbox may expose rectangle textures. Our spatial shaders
            // sample normalized 2D coordinates on every platform.
            let filter = element("capsfilter", "kvm-texture-format")?;
            let caps = gst::Caps::builder("video/x-raw")
                .features(["memory:GLMemory"])
                .field("format", "RGBA")
                .field("texture-target", "2D")
                .build();
            filter.set_property("caps", &caps);
            filter
        };
        let shader = element("glshader", "kvm-spatial-filter")?;
        // Supply both shader stages. glshader's generated vertex stage can use
        // a newer GLSL version in WebKit's shared GL context, which cannot link
        // with our portable legacy/GLES fragment stages.
        shader.set_property("vertex", SPATIAL_VERTEX_SHADER);
        shader.set_property("fragment", DESKTOP_SHADER);
        let output_caps = element("capsfilter", "kvm-spatial-output")?;
        output_caps.set_property(
            "caps",
            gst::Caps::builder("video/x-raw")
                .features(["memory:GLMemory"])
                .field("format", "RGBA")
                .field("texture-target", "2D")
                .field("pixel-aspect-ratio", gst::Fraction::new(1, 1))
                .build(),
        );
        #[cfg(target_os = "linux")]
        let sink = element("gtkglsink", "kvm-screen")?;
        #[cfg(target_os = "macos")]
        let sink = element("glimagesinkelement", "kvm-screen")?;
        sink.set_property("sync", false);
        sink.set_property("async", false);
        sink.set_property("force-aspect-ratio", true);
        sink.set_property("enable-last-sample", false);
        // Read on this thread so GTK creates its widget on the main thread.
        #[cfg(target_os = "linux")]
        let _: glib::Object = sink.property("widget");
        let nodes = vec![
            &webrtc,
            &incoming,
            &video_gate,
            &depay,
            &parser,
            &decoder,
            &render_queue,
            &upload,
            &convert,
            &shader,
            &output_caps,
            &sink,
            &audio_discard,
        ];
        pipeline.add_many(nodes).map_err(|e| e.to_string())?;
        #[cfg(target_os = "macos")]
        pipeline.add(&texture_caps).map_err(|e| e.to_string())?;
        let mut video_nodes = vec![
            &incoming,
            &video_gate,
            &depay,
            &parser,
            &decoder,
            &render_queue,
            &upload,
            &convert,
        ];
        #[cfg(target_os = "macos")]
        video_nodes.push(&texture_caps);
        video_nodes.extend([&shader, &output_caps, &sink]);
        gst::Element::link_many(video_nodes)
            .map_err(|e| format!("No se pudo enlazar el receptor H.264: {e}"))?;

        let counters = Arc::new(Counters::default());
        count_buffers(
            &parser
                .static_pad("src")
                .ok_or("h264parse no tiene pad src")?,
            counters.clone(),
            false,
        );
        count_buffers(
            &decoder
                .static_pad("src")
                .ok_or("El decodificador no tiene pad src")?,
            counters.clone(),
            true,
        );
        let now = Instant::now();
        let inner = Arc::new(Inner {
            pipeline,
            webrtc,
            video_gate,
            parser,
            decoder: Mutex::new(decoder),
            render_queue,
            shader,
            output_caps,
            sink,
            rpc: Mutex::new(None),
            callback,
            counters,
            sample: Mutex::new(Sample {
                at: now,
                received: 0,
                decoded: 0,
                rendered: 0,
                filter_nanos: 0,
                filter_frames: 0,
            }),
            cached_stats: Mutex::new(MediaStats {
                decoder: decoder_name.clone(),
                ..Default::default()
            }),
            alive: AtomicBool::new(true),
            started: AtomicBool::new(false),
            offered: AtomicBool::new(false),
            software_fallback: AtomicBool::new(false),
            stats_pending: AtomicBool::new(false),
            remote_ready: AtomicBool::new(false),
            pending_remote_ice: Mutex::new(Vec::new()),
            spatial: Mutex::new(SpatialState::default()),
            spatial_update_pending: AtomicBool::new(false),
            spatial_apply: Mutex::new(()),
            filter_started: Mutex::new(None),
        });
        let engine = Self(inner);
        engine.connect_spatial_probes();
        engine.connect_signals(incoming, audio_discard);
        (engine.0.callback)(MediaEvent::Decoder(decoder_name));
        Ok(engine)
    }

    fn connect_signals(&self, incoming: gst::Element, audio_discard: gst::Element) {
        let weak = Arc::downgrade(&self.0);
        self.0.webrtc.connect_pad_added(move |_, pad| {
            let Some(inner) = weak.upgrade() else {
                return;
            };
            if pad.direction() != gst::PadDirection::Src {
                return;
            }
            let caps = pad.current_caps().unwrap_or_else(|| pad.query_caps(None));
            let media = caps
                .structure(0)
                .and_then(|s| s.get::<String>("media").ok())
                .unwrap_or_default();
            if media == "audio" {
                if let Some(sink_pad) = audio_discard.static_pad("sink") {
                    if !sink_pad.is_linked() {
                        let _ = pad.link(&sink_pad);
                    }
                }
                return;
            }
            if media != "video" {
                return;
            }
            let codec = caps
                .structure(0)
                .and_then(|s| s.get::<String>("encoding-name").ok())
                .unwrap_or_default();
            if codec != "H264" {
                (inner.callback)(MediaEvent::Error(format!(
                    "El dispositivo negoció {codec}; esta versión necesita una transmisión H.264."
                )));
                return;
            }
            if let Some(sink_pad) = incoming.static_pad("sink") {
                if !sink_pad.is_linked() {
                    if let Err(e) = pad.link(&sink_pad) {
                        (inner.callback)(MediaEvent::Error(format!(
                            "No se pudo conectar el flujo RTP: {e}"
                        )));
                    }
                }
            }
        });

        let weak = Arc::downgrade(&self.0);
        self.0
            .webrtc
            .connect("on-ice-candidate", false, move |values| {
                if let Some(inner) = weak.upgrade() {
                    if let (Ok(index), Ok(candidate)) =
                        (values[1].get::<u32>(), values[2].get::<String>())
                    {
                        (inner.callback)(MediaEvent::Ice {
                            candidate,
                            sdp_m_line_index: index,
                        });
                    }
                }
                None
            });
        let weak = Arc::downgrade(&self.0);
        self.0
            .webrtc
            .connect_notify(Some("connection-state"), move |peer, _| {
                if let Some(inner) = weak.upgrade() {
                    let state =
                        peer.property::<gst_webrtc::WebRTCPeerConnectionState>("connection-state");
                    let state = if state == gst_webrtc::WebRTCPeerConnectionState::Connected {
                        "transport-connected".to_string()
                    } else {
                        format!("{state:?}").to_lowercase()
                    };
                    (inner.callback)(MediaEvent::State(state));
                }
            });
    }

    /// A Sunshine decoder shares our native rendering path, without a WebRTC peer.
    pub fn external(callback: MediaCallback) -> Result<(Self, gstreamer_app::AppSrc), String> {
        let media = Self::new(callback)?;
        let pipeline = &media.0.pipeline;
        // Self::new prepares the common H.264 render chain. Replace its RTP
        // transport before starting it; PicoKVM keeps its own, separate engine.
        for name in [
            "kvm-peer",
            "kvm-incoming",
            "kvm-video-gate",
            "kvm-depay",
            "kvm-audio-discard",
        ] {
            if let Some(node) = pipeline.by_name(name) {
                pipeline.remove(&node).map_err(|e| e.to_string())?;
            }
        }
        let source = gstreamer_app::AppSrc::builder()
            .name("sunshine-h264")
            .is_live(true)
            .format(gst::Format::Time)
            .do_timestamp(true)
            .block(false)
            .max_bytes(8 * 1024 * 1024)
            .build();
        source.set_caps(Some(
            &gst::Caps::builder("video/x-h264")
                .field("stream-format", "byte-stream")
                .field("alignment", "au")
                .build(),
        ));
        pipeline.add(&source).map_err(|e| e.to_string())?;
        let parser = pipeline
            .by_name("kvm-parser")
            .ok_or("No se creó el parser H.264")?;
        source.link(&parser).map_err(|e| e.to_string())?;
        Ok((media, source))
    }

    pub fn start_external(&self) -> Result<(), String> {
        if !self.0.started.swap(true, Ordering::SeqCst) {
            self.spawn_bus();
        }
        self.0
            .pipeline
            .set_state(gst::State::Playing)
            .map(|_| ())
            .map_err(|e| format!("No se pudo iniciar el video Sunshine: {e}"))
    }

    /// Borrowed GtkWidget pointer, owned by the sink. GTK main thread only.
    /// Keep a gtk::Widget reference in the view while the engine is alive.
    #[cfg(target_os = "linux")]
    pub fn widget_ptr(&self) -> usize {
        let widget: glib::Object = self.0.sink.property("widget");
        widget.as_ptr() as usize
    }

    /// The host retains this NSView until the pipeline has reached NULL.
    /// Call on the AppKit main thread before starting the pipeline.
    #[cfg(target_os = "macos")]
    pub fn set_window_handle(&self, handle: usize) -> Result<(), String> {
        use gstreamer_video::prelude::*;
        if handle == 0 {
            return Err("No se creó la superficie nativa de video".into());
        }
        let overlay = self
            .0
            .sink
            .clone()
            .dynamic_cast::<gstreamer_video::VideoOverlay>()
            .map_err(|_| "El sink de video no admite una superficie nativa")?;
        // SAFETY: video_host owns a retained NSView on the main thread and
        // keeps it alive through shutdown. No AppKit object crosses to JS.
        unsafe { overlay.set_window_handle(handle) };
        overlay.handle_events(false);
        Ok(())
    }

    /// Called once signaling is ready to accept Offer and Ice events.
    pub fn start(&self) -> Result<(), String> {
        if self.0.started.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        self.0
            .pipeline
            .set_state(gst::State::Ready)
            .map_err(|e| format!("No se pudo preparar el receptor: {e}"))?;
        let caps = gst::Caps::builder("application/x-rtp")
            .field("media", "video")
            .field("encoding-name", "H264")
            .field("payload", 96i32)
            .field("clock-rate", 90_000i32)
            .field("packetization-mode", "1")
            .field("profile-level-id", "42e01f")
            .field("level-asymmetry-allowed", "1")
            .build();
        self.0
            .webrtc
            .emit_by_name::<gst_webrtc::WebRTCRTPTransceiver>(
                "add-transceiver",
                &[&gst_webrtc::WebRTCRTPTransceiverDirection::Recvonly, &caps],
            );
        let audio_caps = gst::Caps::builder("application/x-rtp")
            .field("media", "audio")
            .field("encoding-name", "OPUS")
            .field("payload", 111i32)
            .field("clock-rate", 48_000i32)
            .field("encoding-params", "2")
            .build();
        self.0
            .webrtc
            .emit_by_name::<gst_webrtc::WebRTCRTPTransceiver>(
                "add-transceiver",
                &[
                    &gst_webrtc::WebRTCRTPTransceiverDirection::Recvonly,
                    &audio_caps,
                ],
            );
        let channel = self
            .0
            .webrtc
            .emit_by_name::<Option<gst_webrtc::WebRTCDataChannel>>(
                "create-data-channel",
                &[&"rpc", &None::<gst::Structure>],
            )
            .ok_or("No se pudo crear el canal de control WebRTC (revisa SCTP en GStreamer)")?;
        let weak = Arc::downgrade(&self.0);
        channel.connect_on_open(move |_| {
            if let Some(inner) = weak.upgrade() {
                (inner.callback)(MediaEvent::State("connected".into()));
            }
        });
        let weak = Arc::downgrade(&self.0);
        channel.connect_on_close(move |_| {
            if let Some(inner) = weak.upgrade() {
                if inner.alive.load(Ordering::SeqCst) {
                    (inner.callback)(MediaEvent::State("disconnected".into()));
                }
            }
        });
        let weak = Arc::downgrade(&self.0);
        channel.connect_on_message_string(move |_, message| {
            if let (Some(inner), Some(message)) = (weak.upgrade(), message) {
                match serde_json::from_str::<Value>(message) {
                    Ok(value) => (inner.callback)(MediaEvent::Rpc(value)),
                    Err(error) => (inner.callback)(MediaEvent::Error(format!(
                        "RPC no válido recibido del KVM: {error}"
                    ))),
                }
            }
        });
        let weak = Arc::downgrade(&self.0);
        channel.connect_on_error(move |_, error| {
            if let Some(inner) = weak.upgrade() {
                (inner.callback)(MediaEvent::Error(format!("Canal RPC: {error}")));
            }
        });
        *self.0.rpc.lock().map_err(|_| "Bloqueo RPC invalidado")? = Some(channel);
        self.0
            .pipeline
            .set_state(gst::State::Playing)
            .map_err(|e| format!("No se pudo iniciar video: {e}"))?;
        self.spawn_bus();
        self.create_offer();
        Ok(())
    }

    fn create_offer(&self) {
        if self.0.offered.swap(true, Ordering::SeqCst) {
            return;
        }
        let weak = Arc::downgrade(&self.0);
        let promise = gst::Promise::with_change_func(move |reply| {
            let Some(inner) = weak.upgrade() else {
                return;
            };
            let result = (|| -> Result<(), String> {
                let reply = reply
                    .map_err(|e| format!("No se pudo generar SDP: {e:?}"))?
                    .ok_or("Oferta SDP vacía")?;
                let offer = reply
                    .get::<gst_webrtc::WebRTCSessionDescription>("offer")
                    .map_err(|e| e.to_string())?;
                let sdp = offer.sdp().as_text().map_err(|e| e.to_string())?;
                inner
                    .webrtc
                    .emit_by_name::<()>("set-local-description", &[&offer, &None::<gst::Promise>]);
                (inner.callback)(MediaEvent::Offer { sdp });
                Ok(())
            })();
            if let Err(error) = result {
                (inner.callback)(MediaEvent::Error(error));
            }
        });
        self.0
            .webrtc
            .emit_by_name::<()>("create-offer", &[&None::<gst::Structure>, &promise]);
    }

    pub fn handle_answer(&self, sdp: &str) -> Result<(), String> {
        let message = gst_sdp::SDPMessage::parse_buffer(sdp.as_bytes())
            .map_err(|e| format!("SDP remoto no válido: {e}"))?;
        let answer =
            gst_webrtc::WebRTCSessionDescription::new(gst_webrtc::WebRTCSDPType::Answer, message);
        let weak = Arc::downgrade(&self.0);
        let promise = gst::Promise::with_change_func(move |reply| {
            if let Some(inner) = weak.upgrade() {
                match reply {
                    Err(error) => {
                        (inner.callback)(MediaEvent::Error(format!(
                            "No se pudo aplicar SDP remoto: {error:?}"
                        )));
                    }
                    Ok(reply) => {
                        if let Some(error) = reply.and_then(|s| s.get::<glib::Error>("error").ok())
                        {
                            (inner.callback)(MediaEvent::Error(format!(
                                "No se pudo aplicar SDP remoto: {error}"
                            )));
                            return;
                        }
                        let queued = {
                            let mut pending = inner
                                .pending_remote_ice
                                .lock()
                                .unwrap_or_else(|e| e.into_inner());
                            inner.remote_ready.store(true, Ordering::SeqCst);
                            std::mem::take(&mut *pending)
                        };
                        for (index, candidate) in queued {
                            inner
                                .webrtc
                                .emit_by_name::<()>("add-ice-candidate", &[&index, &candidate]);
                        }
                    }
                }
            }
        });
        self.0
            .webrtc
            .emit_by_name::<()>("set-remote-description", &[&answer, &promise]);
        Ok(())
    }

    pub fn add_ice(&self, sdp_m_line_index: u32, candidate: &str) {
        let mut pending = self
            .0
            .pending_remote_ice
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if !self.0.remote_ready.load(Ordering::SeqCst) {
            if pending.len() >= 128 {
                drop(pending);
                (self.0.callback)(MediaEvent::Error(
                    "Demasiados candidatos ICE pendientes".into(),
                ));
                return;
            }
            pending.push((sdp_m_line_index, candidate.to_string()));
            return;
        }
        drop(pending);
        self.0
            .webrtc
            .emit_by_name::<()>("add-ice-candidate", &[&sdp_m_line_index, &candidate]);
    }

    /// A notification/request already wrapped as JSON-RPC. Never queues motion
    /// behind a large SCTP buffer: the input scheduler retains it for retry.
    pub fn send_rpc(&self, value: Value) -> Result<(), String> {
        let rpc = self.0.rpc.lock().map_err(|_| "Bloqueo RPC invalidado")?;
        let channel = rpc.as_ref().ok_or("El canal RPC todavía no está abierto")?;
        if channel.ready_state() != gst_webrtc::WebRTCDataChannelState::Open {
            return Err("El canal RPC no está conectado".into());
        }
        if channel.buffered_amount() > MAX_RPC_BUFFER {
            return Err("El canal RPC está congestionado".into());
        }
        channel
            .send_string_full(Some(&value.to_string()))
            .map_err(|e| format!("No se pudo enviar RPC: {e}"))
    }

    /// Pause only compressed PicoKVM video. The WebRTC transport and reliable
    /// RPC DataChannel remain connected so keyboard/mouse reports can continue.
    pub fn set_video_paused(&self, paused: bool) -> Result<(), String> {
        if !self.0.alive.load(Ordering::Acquire) {
            return Err("El receptor de video está cerrado".into());
        }
        if self.0.video_gate.parent().is_none() {
            return Err("Esta opción solo pausa el video de PicoKVM".into());
        }
        let was_paused = self.is_video_paused();
        self.0.video_gate.set_property("drop", paused);
        if was_paused && !paused {
            // Lost RTP packets require a fresh IDR. The depayloader also waits
            // for keyframes and sends loss feedback; don't reopen the peer.
            let _ = self.0.parser.send_event(
                gstreamer_video::UpstreamForceKeyUnitEvent::builder()
                    .all_headers(true)
                    .build(),
            );
        }
        Ok(())
    }

    pub fn is_video_paused(&self) -> bool {
        self.0.video_gate.property::<bool>("drop")
    }

    pub fn configure(&self, settings: VideoSettings) -> Result<(), String> {
        settings.validate()?;
        self.0
            .spatial
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .settings = settings;
        self.schedule_spatial_update();
        Ok(())
    }

    /// Called with native backing pixels, never CSS pixels or AppKit points.
    pub fn set_viewport_size(&self, width: u32, height: u32) -> Result<(), String> {
        if width == 0 || height == 0 {
            return Ok(());
        }
        if width > 16384 || height > 16384 {
            return Err("El área de video supera 16384 píxeles por eje".into());
        }
        let mut spatial = self.0.spatial.lock().unwrap_or_else(|e| e.into_inner());
        if spatial.viewport == (width, height) {
            return Ok(());
        }
        spatial.viewport = (width, height);
        drop(spatial);
        self.schedule_spatial_update();
        Ok(())
    }

    pub fn viewport_callback(&self) -> Arc<dyn Fn(u32, u32) + Send + Sync> {
        let weak = Arc::downgrade(&self.0);
        Arc::new(move |width, height| {
            if let Some(inner) = weak.upgrade().filter(|i| i.alive.load(Ordering::Acquire)) {
                let engine = Self(inner);
                if let Err(error) = engine.set_viewport_size(width, height) {
                    (engine.0.callback)(MediaEvent::Error(error));
                }
            }
        })
    }

    fn schedule_spatial_update(&self) {
        if self.0.spatial_update_pending.swap(true, Ordering::AcqRel) {
            return;
        }
        let weak = Arc::downgrade(&self.0);
        // Caps renegotiation must not block a native UI callback or a streaming pad probe.
        self.0.pipeline.call_async(move |_| {
            if let Some(inner) = weak.upgrade() {
                let _apply = inner
                    .spatial_apply
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                inner.spatial_update_pending.store(false, Ordering::Release);
                if !inner.alive.load(Ordering::Acquire) {
                    return;
                }
                let mut spatial = inner.spatial.lock().unwrap_or_else(|e| e.into_inner());
                if spatial.input.0 == 0 || spatial.input.1 == 0 {
                    return;
                }
                let settings = spatial.settings;
                let output = target_size(
                    spatial.input,
                    spatial.pixel_aspect,
                    spatial.viewport,
                    settings.native_size || settings.scale_factor != 0,
                );
                let changed = spatial.applied_filter != Some(settings.filter);
                let input = spatial.input;
                let resized = output != spatial.requested;
                spatial.applied_filter = Some(settings.filter);
                spatial.requested = output;
                drop(spatial);
                if changed {
                    let shader = match settings.filter {
                        VideoFilter::Bilinear => ORIGINAL_SHADER,
                        VideoFilter::Sharp => SHARP_SHADER,
                        VideoFilter::Desktop => DESKTOP_SHADER,
                        VideoFilter::Gaming => GAMING_SHADER,
                        VideoFilter::Nearest => NEAREST_SHADER,
                    };
                    inner.shader.set_property("fragment", shader);
                }
                inner.shader.set_property(
                    "uniforms",
                    gst::Structure::builder("uniforms")
                        .field("input_width", input.0 as f32)
                        .field("input_height", input.1 as f32)
                        .field("output_width", output.0 as f32)
                        .field("output_height", output.1 as f32)
                        .field(
                            "native_size",
                            if settings.native_size || settings.scale_factor != 0 {
                                1.0f32
                            } else {
                                0.0f32
                            },
                        )
                        .field(
                            "zoom",
                            if settings.native_size {
                                1.0f32
                            } else {
                                settings.scale_factor.max(1) as f32
                            },
                        )
                        .field("anti_ringing", settings.anti_ringing)
                        .field("edge_threshold", settings.edge_threshold)
                        .field(
                            "downscale_aa",
                            if settings.downscale_anti_aliasing {
                                1.0f32
                            } else {
                                0.0f32
                            },
                        )
                        .field("amount", settings.sharpness)
                        .build(),
                );
                if resized {
                    // The shader's FBO now has the destination resolution. The sink
                    // only letterboxes it instead of doing the final enlargement.
                    inner.output_caps.set_property(
                        "caps",
                        gst::Caps::builder("video/x-raw")
                            .features(["memory:GLMemory"])
                            .field("format", "RGBA")
                            .field("texture-target", "2D")
                            .field("pixel-aspect-ratio", gst::Fraction::new(1, 1))
                            .field("width", output.0 as i32)
                            .field("height", output.1 as i32)
                            .build(),
                    );
                }
            }
        });
    }

    fn connect_spatial_probes(&self) {
        let weak = Arc::downgrade(&self.0);
        self.0
            .shader
            .static_pad("sink")
            .expect("shader sink pad")
            .add_probe(
                gst::PadProbeType::EVENT_DOWNSTREAM | gst::PadProbeType::BUFFER,
                move |_, probe| {
                    let Some(inner) = weak.upgrade() else {
                        return gst::PadProbeReturn::Remove;
                    };
                    if let Some(event) = probe.event() {
                        if let gst::EventView::Caps(caps) = event.view() {
                            if let Some(s) = caps.caps().structure(0) {
                                if let (Ok(width), Ok(height)) =
                                    (s.get::<i32>("width"), s.get::<i32>("height"))
                                {
                                    let par = s
                                        .get::<gst::Fraction>("pixel-aspect-ratio")
                                        .unwrap_or(gst::Fraction::new(1, 1));
                                    let mut spatial =
                                        inner.spatial.lock().unwrap_or_else(|e| e.into_inner());
                                    spatial.input = (width.max(1) as u32, height.max(1) as u32);
                                    spatial.pixel_aspect =
                                        (par.numer().max(1) as u32, par.denom().max(1) as u32);
                                    drop(spatial);
                                    Self(inner.clone()).schedule_spatial_update();
                                }
                            }
                        }
                    }
                    if probe.buffer().is_some() {
                        *inner
                            .filter_started
                            .lock()
                            .unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
                    }
                    gst::PadProbeReturn::Ok
                },
            );
        let weak = Arc::downgrade(&self.0);
        self.0
            .shader
            .static_pad("src")
            .expect("shader src pad")
            .add_probe(
                gst::PadProbeType::EVENT_DOWNSTREAM | gst::PadProbeType::BUFFER,
                move |_, probe| {
                    let Some(inner) = weak.upgrade() else {
                        return gst::PadProbeReturn::Remove;
                    };
                    if let Some(event) = probe.event() {
                        if let gst::EventView::Caps(caps) = event.view() {
                            if let Some(s) = caps.caps().structure(0) {
                                if let (Ok(width), Ok(height)) =
                                    (s.get::<i32>("width"), s.get::<i32>("height"))
                                {
                                    inner
                                        .spatial
                                        .lock()
                                        .unwrap_or_else(|e| e.into_inner())
                                        .negotiated = (width.max(1) as u32, height.max(1) as u32);
                                }
                            }
                        }
                    }
                    if probe.buffer().is_some() {
                        if let Some(start) = inner
                            .filter_started
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .take()
                        {
                            inner.counters.filter_nanos.fetch_add(
                                start.elapsed().as_nanos().min(u64::MAX as u128) as u64,
                                Ordering::Relaxed,
                            );
                            inner.counters.filter_frames.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    gst::PadProbeReturn::Ok
                },
            );
    }

    fn spawn_bus(&self) {
        let weak = Arc::downgrade(&self.0);
        let bus = self
            .0
            .pipeline
            .bus()
            .expect("GStreamer pipelines have a bus");
        std::thread::Builder::new()
            .name("kvm-media-bus".into())
            .spawn(move || loop {
                let Some(inner) = weak.upgrade() else {
                    break;
                };
                if !inner.alive.load(Ordering::SeqCst) {
                    break;
                }
                drop(inner);
                let Some(message) = bus.timed_pop(gst::ClockTime::from_mseconds(100)) else {
                    continue;
                };
                let Some(inner) = weak.upgrade() else {
                    break;
                };
                match message.view() {
                    gst::MessageView::Error(error) => {
                        let is_hardware = inner
                            .cached_stats
                            .lock()
                            .map(|s| !is_software_decoder(&s.decoder))
                            .unwrap_or(false);
                        let decoder_source = error
                            .src()
                            .map(|source| source.name().contains("kvm-decoder"))
                            .unwrap_or(false);
                        if is_hardware
                            && decoder_source
                            && !inner.software_fallback.swap(true, Ordering::SeqCst)
                        {
                            MediaEngine::fallback_to_software(inner.clone());
                        } else {
                            let source = error
                                .src()
                                .map(|s| s.name().to_string())
                                .unwrap_or_default();
                            // Avoid dumping raw debug/SDP, which may contain
                            // ephemeral ICE credentials. Keep element context.
                            eprintln!("GStreamer error ({source}): {}", error.error());
                            (inner.callback)(MediaEvent::Error(format!(
                                "Video ({source}): {}",
                                error.error()
                            )));
                        }
                    }
                    gst::MessageView::Eos(_) => {
                        (inner.callback)(MediaEvent::State("disconnected".into()));
                    }
                    gst::MessageView::Warning(warning) => {
                        let source = warning
                            .src()
                            .map(|s| s.name().to_string())
                            .unwrap_or_default();
                        eprintln!("GStreamer warning ({source}): {}", warning.error());
                    }
                    _ => {}
                }
            })
            .expect("failed to start media bus worker");
    }

    fn fallback_to_software(inner: Arc<Inner>) {
        // An IDLE blocking probe stops the parser before replacing the decoder.
        // It is removed after rebuilding so no compressed buffer is discarded.
        let Some(pad) = inner.parser.static_pad("src") else {
            return;
        };
        pad.add_probe(gst::PadProbeType::IDLE, move |_, _| {
            let result = (|| -> Result<(), String> {
                let (replacement, name) = choose_decoder(true)?;
                let mut decoder = inner
                    .decoder
                    .lock()
                    .map_err(|_| "Bloqueo del decodificador invalidado")?;
                inner.parser.unlink(&*decoder);
                decoder.unlink(&inner.render_queue);
                decoder
                    .set_state(gst::State::Null)
                    .map_err(|e| e.to_string())?;
                inner
                    .pipeline
                    .remove(&*decoder)
                    .map_err(|e| e.to_string())?;
                inner
                    .pipeline
                    .add(&replacement)
                    .map_err(|e| e.to_string())?;
                gst::Element::link_many([&inner.parser, &replacement, &inner.render_queue])
                    .map_err(|e| e.to_string())?;
                count_buffers(
                    &replacement
                        .static_pad("src")
                        .ok_or("No hay salida del decodificador")?,
                    inner.counters.clone(),
                    true,
                );
                replacement
                    .sync_state_with_parent()
                    .map_err(|e| e.to_string())?;
                *decoder = replacement;
                if let Ok(mut stats) = inner.cached_stats.lock() {
                    stats.decoder = name.clone();
                }
                (inner.callback)(MediaEvent::Decoder(name));
                Ok(())
            })();
            if let Err(error) = result {
                (inner.callback)(MediaEvent::Error(format!(
                    "No se pudo activar decodificación por software: {error}"
                )));
            }
            gst::PadProbeReturn::Remove
        });
    }

    /// Monotonic decoded/presented counters, independent of FPS sampling.
    pub fn frame_counts(&self) -> (u64, u64) {
        let decoded = self.0.counters.decoded.load(Ordering::Relaxed);
        let sink_stats = self.0.sink.property::<gst::Structure>("stats");
        (decoded, sink_stats.get::<u64>("rendered").unwrap_or(0))
    }

    pub fn stats(&self) -> MediaStats {
        let received = self.0.counters.received.load(Ordering::Relaxed);
        let decoded = self.0.counters.decoded.load(Ordering::Relaxed);
        let sink_stats = self.0.sink.property::<gst::Structure>("stats");
        let rendered = sink_stats.get::<u64>("rendered").unwrap_or(0);
        let now = Instant::now();
        let filter_nanos = self.0.counters.filter_nanos.load(Ordering::Relaxed);
        let filter_frames = self.0.counters.filter_frames.load(Ordering::Relaxed);
        let mut sample = self.0.sample.lock().unwrap_or_else(|e| e.into_inner());
        let elapsed = now.duration_since(sample.at).as_secs_f64();
        let mut stats = self
            .0
            .cached_stats
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if elapsed >= 0.25 {
            stats.received_fps =
                (received > 0).then_some(received.saturating_sub(sample.received) as f64 / elapsed);
            stats.decoded_fps =
                (decoded > 0).then_some(decoded.saturating_sub(sample.decoded) as f64 / elapsed);
            stats.presented_fps =
                (rendered > 0).then_some(rendered.saturating_sub(sample.rendered) as f64 / elapsed);
            let processed = filter_frames.saturating_sub(sample.filter_frames);
            stats.filter_processing_ms = (processed > 0).then(|| {
                filter_nanos.saturating_sub(sample.filter_nanos) as f64
                    / processed as f64
                    / 1_000_000.0
            });
            *sample = Sample {
                at: now,
                received,
                decoded,
                rendered,
                filter_nanos,
                filter_frames,
            };
        }
        let spatial = self.0.spatial.lock().unwrap_or_else(|e| e.into_inner());
        stats.input_width = spatial.input.0;
        stats.input_height = spatial.input.1;
        stats.output_width = spatial.negotiated.0;
        stats.output_height = spatial.negotiated.1;
        stats.sink_dropped_frames = sink_stats.get::<u64>("dropped").ok();
        drop(spatial);
        stats.rpc_buffered_bytes = self
            .0
            .rpc
            .lock()
            .ok()
            .and_then(|rpc| rpc.as_ref().map(|channel| channel.buffered_amount()))
            .unwrap_or(0);
        let result = stats.clone();
        drop(stats);
        drop(sample);
        self.request_stats();
        result
    }

    fn request_stats(&self) {
        if !self.0.started.load(Ordering::SeqCst)
            || self.0.stats_pending.swap(true, Ordering::SeqCst)
        {
            return;
        }
        let weak = Arc::downgrade(&self.0);
        let promise = gst::Promise::with_change_func(move |reply| {
            let Some(inner) = weak.upgrade() else {
                return;
            };
            if let Ok(Some(reply)) = reply {
                let mut rtt = None;
                let mut lost = None;
                read_webrtc_stats(reply, &mut rtt, &mut lost);
                if let Ok(mut stats) = inner.cached_stats.lock() {
                    stats.rtt_ms = rtt;
                    stats.packets_lost = lost;
                }
            }
            inner.stats_pending.store(false, Ordering::SeqCst);
        });
        self.0
            .webrtc
            .emit_by_name::<()>("get-stats", &[&None::<gst::Pad>, &promise]);
    }

    pub fn shutdown(&self) {
        self.0.alive.store(false, Ordering::SeqCst);
        if let Ok(mut rpc) = self.0.rpc.lock() {
            if let Some(channel) = rpc.take() {
                channel.close();
            }
        }
        let _ = self.0.pipeline.set_state(gst::State::Null);
    }
}

fn is_software_decoder(name: &str) -> bool {
    matches!(name, "avdec_h264" | "openh264dec")
}

fn read_webrtc_stats(structure: &gst::StructureRef, rtt: &mut Option<f64>, lost: &mut Option<i64>) {
    if let Ok(seconds) = structure
        .get::<f64>("current-round-trip-time")
        .or_else(|_| structure.get::<f64>("round-trip-time"))
    {
        if seconds.is_finite() && seconds >= 0.0 {
            *rtt = Some(seconds * 1000.0);
        }
    }
    if structure.get::<String>("kind").ok().as_deref() != Some("audio") {
        if let Ok(value) = structure.get::<i64>("packets-lost") {
            *lost = Some(value);
        }
        if let Ok(value) = structure.get::<i32>("packets-lost") {
            *lost = Some(i64::from(value));
        }
    }
    for (_, value) in structure.iter() {
        if let Ok(child) = value.get::<gst::Structure>() {
            read_webrtc_stats(&child, rtt, lost);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_webrtc_stats_and_converts_seconds() {
        gst::init().unwrap();
        let pair = gst::Structure::builder("candidate-pair")
            .field("current-round-trip-time", 0.004f64)
            .build();
        let inbound = gst::Structure::builder("inbound-rtp")
            .field("kind", "video")
            .field("packets-lost", 3i64)
            .build();
        let audio = gst::Structure::builder("inbound-rtp")
            .field("kind", "audio")
            .field("packets-lost", 99i64)
            .build();
        let root = gst::Structure::builder("application/x-webrtc-stats")
            .field("pair", pair)
            .field("video", inbound)
            .field("audio", audio)
            .build();
        let (mut rtt, mut lost) = (None, None);
        read_webrtc_stats(&root, &mut rtt, &mut lost);
        assert_eq!(rtt, Some(4.0));
        assert_eq!(lost, Some(3));
    }

    #[test]
    fn unknown_stats_stay_unknown() {
        gst::init().unwrap();
        let root = gst::Structure::new_empty("application/x-webrtc-stats");
        let (mut rtt, mut lost) = (None, None);
        read_webrtc_stats(&root, &mut rtt, &mut lost);
        assert_eq!((rtt, lost), (None, None));
    }

    #[test]
    fn video_settings_serde_matches_frontend() {
        let settings: VideoSettings =
            serde_json::from_value(serde_json::json!({"filter":"sharp", "sharpness":0.25}))
                .unwrap();
        assert_eq!(settings.filter, VideoFilter::Sharp);
        assert_eq!(settings.sharpness, 0.25);
        assert!(!settings.native_size);
        assert_eq!(settings.scale_factor, 0);
        assert_eq!(settings.anti_ringing, 1.0);
        assert_eq!(settings.edge_threshold, 8.0 / 255.0);
        assert!(settings.downscale_anti_aliasing);
    }

    #[test]
    fn spatial_target_uses_physical_viewport_and_preserves_display_aspect() {
        assert_eq!(
            target_size((1920, 1080), (1, 1), (2560, 1600), false),
            (2560, 1440)
        );
        assert_eq!(
            target_size((1920, 1080), (1, 1), (1280, 800), false),
            (1280, 720)
        );
        assert_eq!(
            target_size((1920, 1080), (1, 1), (2560, 1600), true),
            (2560, 1600)
        );
        assert_eq!(
            target_size((720, 576), (16, 15), (1920, 1080), false),
            (1440, 1080)
        );
        assert_eq!(
            target_size((1920, 1080), (1, 1), (0, 0), false),
            (1920, 1080)
        );
    }

    #[test]
    fn video_profiles_accept_new_names_and_legacy_original() {
        for name in ["desktop", "gaming", "original", "bilinear", "nearest"] {
            let settings: VideoSettings = serde_json::from_value(serde_json::json!({
                "filter": name, "sharpness":0.2, "nativeSize":true
            }))
            .unwrap();
            assert!(settings.native_size);
        }
        assert_eq!(
            serde_json::to_value(VideoFilter::Bilinear).unwrap(),
            "original"
        );
    }

    #[test]
    fn video_quality_controls_reject_invalid_or_conflicting_values() {
        let defaults = VideoSettings::default();
        for settings in [
            VideoSettings {
                sharpness: f32::NAN,
                ..defaults
            },
            VideoSettings {
                anti_ringing: -0.1,
                ..defaults
            },
            VideoSettings {
                anti_ringing: f32::INFINITY,
                ..defaults
            },
            VideoSettings {
                edge_threshold: 0.0,
                ..defaults
            },
            VideoSettings {
                edge_threshold: 33.0 / 255.0,
                ..defaults
            },
            VideoSettings {
                scale_factor: 1,
                ..defaults
            },
            VideoSettings {
                scale_factor: 5,
                ..defaults
            },
            VideoSettings {
                native_size: true,
                scale_factor: 2,
                ..defaults
            },
        ] {
            assert!(settings.validate().is_err());
        }
        for factor in [0, 2, 3, 4] {
            assert!(VideoSettings {
                scale_factor: factor,
                ..defaults
            }
            .validate()
            .is_ok());
        }
        assert!(VideoSettings {
            native_size: true,
            ..defaults
        }
        .validate()
        .is_ok());
    }
}
