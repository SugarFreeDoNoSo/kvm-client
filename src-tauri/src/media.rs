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

const BILINEAR_SHADER: &str = include_str!("shaders/bilinear.frag");
const SHARP_SHADER: &str = include_str!("shaders/sharp.frag");
const SPATIAL_VERTEX_SHADER: &str = include_str!("shaders/spatial.vert");
const MAX_RPC_BUFFER: u64 = 32 * 1024;

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
    #[default]
    Bilinear,
    Sharp,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoSettings {
    pub filter: VideoFilter,
    pub sharpness: f32,
}

impl Default for VideoSettings {
    fn default() -> Self {
        Self {
            filter: VideoFilter::Bilinear,
            sharpness: 0.35,
        }
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
}

#[derive(Default)]
struct Counters {
    received: AtomicU64,
    decoded: AtomicU64,
}

struct Sample {
    at: Instant,
    received: u64,
    decoded: u64,
    rendered: u64,
}

struct Inner {
    pipeline: gst::Pipeline,
    webrtc: gst::Element,
    parser: gst::Element,
    decoder: Mutex<gst::Element>,
    render_queue: gst::Element,
    shader: gst::Element,
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
    video_settings: Mutex<VideoSettings>,
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
    let factories: &[&str] = if force_software {
        &["avdec_h264", "openh264dec"]
    } else {
        &[
            "vah264dec",
            "vaapih264dec",
            "nvh264dec",
            "v4l2slh264dec",
            "avdec_h264",
            "openh264dec",
        ]
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
    Err("No hay un decodificador H.264. Instala gst-libav o un plugin VA compatible.".into())
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
    /// Must be constructed on the GTK main thread after GTK initialization.
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
            "gtkglsink",
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
        let (decoder, decoder_name) = choose_decoder(false)?;
        let render_queue = element("queue", "kvm-latest-frame")?;
        render_queue.set_property("max-size-buffers", 1u32);
        render_queue.set_property("max-size-bytes", 0u32);
        render_queue.set_property("max-size-time", 0u64);
        render_queue.set_property_from_str("leaky", "downstream");
        let upload = element("glupload", "kvm-upload")?;
        let convert = element("glcolorconvert", "kvm-rgba")?;
        let shader = element("glshader", "kvm-spatial-filter")?;
        // Supply both shader stages. glshader's generated vertex stage can use
        // a newer GLSL version in WebKit's shared GL context, which cannot link
        // with our portable legacy/GLES fragment stages.
        shader.set_property("vertex", SPATIAL_VERTEX_SHADER);
        shader.set_property("fragment", BILINEAR_SHADER);
        let sink = element("gtkglsink", "kvm-screen")?;
        sink.set_property("sync", false);
        sink.set_property("async", false);
        sink.set_property("force-aspect-ratio", true);
        sink.set_property("enable-last-sample", false);
        // Read on this thread so GTK creates its widget on the main thread.
        let _: glib::Object = sink.property("widget");
        let nodes = [
            &webrtc,
            &incoming,
            &depay,
            &parser,
            &decoder,
            &render_queue,
            &upload,
            &convert,
            &shader,
            &sink,
            &audio_discard,
        ];
        pipeline.add_many(nodes).map_err(|e| e.to_string())?;
        gst::Element::link_many([
            &incoming,
            &depay,
            &parser,
            &decoder,
            &render_queue,
            &upload,
            &convert,
            &shader,
            &sink,
        ])
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
            parser,
            decoder: Mutex::new(decoder),
            render_queue,
            shader,
            sink,
            rpc: Mutex::new(None),
            callback,
            counters,
            sample: Mutex::new(Sample {
                at: now,
                received: 0,
                decoded: 0,
                rendered: 0,
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
            video_settings: Mutex::new(VideoSettings::default()),
        });
        let engine = Self(inner);
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

    /// Borrowed GtkWidget pointer, owned by the sink. GTK main thread only.
    /// Keep a gtk::Widget reference in the view while the engine is alive.
    pub fn widget_ptr(&self) -> usize {
        let widget: glib::Object = self.0.sink.property("widget");
        widget.as_ptr() as usize
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

    pub fn configure(&self, settings: VideoSettings) -> Result<(), String> {
        if !settings.sharpness.is_finite() || !(0.0..=1.0).contains(&settings.sharpness) {
            return Err("La nitidez debe estar entre 0 y 1".into());
        }
        let mut previous = self
            .0
            .video_settings
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let changed_filter = previous.filter != settings.filter;
        if changed_filter {
            let fragment = match settings.filter {
                VideoFilter::Bilinear => BILINEAR_SHADER,
                VideoFilter::Sharp => SHARP_SHADER,
            };
            self.0.shader.set_property("fragment", fragment);
        }
        // Slider updates only change uniforms, avoiding a GLSL recompile on
        // every input event while video is playing.
        if settings.filter == VideoFilter::Sharp
            && (changed_filter || previous.sharpness != settings.sharpness)
        {
            self.0.shader.set_property(
                "uniforms",
                gst::Structure::builder("uniforms")
                    .field("amount", settings.sharpness)
                    .build(),
            );
        }
        *previous = settings;
        Ok(())
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
                            (inner.callback)(MediaEvent::Error(format!(
                                "Video ({source}): {}",
                                error.error()
                            )));
                        }
                    }
                    gst::MessageView::Eos(_) => {
                        (inner.callback)(MediaEvent::State("disconnected".into()));
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

    pub fn stats(&self) -> MediaStats {
        let received = self.0.counters.received.load(Ordering::Relaxed);
        let decoded = self.0.counters.decoded.load(Ordering::Relaxed);
        let sink_stats = self.0.sink.property::<gst::Structure>("stats");
        let rendered = sink_stats.get::<u64>("rendered").unwrap_or(0);
        let now = Instant::now();
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
            *sample = Sample {
                at: now,
                received,
                decoded,
                rendered,
            };
        }
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
    }
}
