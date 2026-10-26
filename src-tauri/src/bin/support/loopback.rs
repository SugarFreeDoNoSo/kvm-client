//! Local encoded-video test. Both sides use real WebRTC/DTLS/SRTP; the receiver
//! is the production MediaEngine and no real KVM connection is created.

use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use base64::{engine::general_purpose::STANDARD, Engine};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_sdp as gst_sdp;
use gstreamer_webrtc as gst_webrtc;
use serde_json::json;
use tokio::sync::mpsc;

use crate::{
    health::{FrameHealth, VideoState},
    media::{MediaEngine, MediaEvent, VideoFilter, VideoSettings},
};

enum SenderEvent {
    Answer(String),
    Ice(u32, String),
    RemoteReady,
    Error(String),
}

struct Sender(gst::Pipeline);
impl Drop for Sender {
    fn drop(&mut self) {
        let _ = self.0.set_state(gst::State::Null);
    }
}

fn validate_synthetic_sps(parameter_sets: &str) -> Result<(), String> {
    let sps = STANDARD
        .decode(parameter_sets.split(',').next().unwrap_or_default())
        .map_err(|_| "El emisor sintético no produjo un SPS H.264 válido")?;
    // RFC 6184 §8.1, table 5: profile 42 with profile-iop x1xx0000
    // denotes Constrained Baseline. 42c0 and 42e0 are the same sub-profile.
    if sps.len() < 4
        || sps[0] & 0x1f != 7
        || sps[1] != 0x42
        || sps[2] & 0x4f != 0x40
        || sps[3] != 0x1f
    {
        return Err(
            "El emisor sintético debe producir H.264 Constrained Baseline nivel 3.1".into(),
        );
    }
    Ok(())
}

fn answer(
    peer: &gst::Element,
    sdp: &str,
    events: mpsc::UnboundedSender<SenderEvent>,
) -> Result<(), String> {
    let message = gst_sdp::SDPMessage::parse_buffer(sdp.as_bytes()).map_err(|e| e.to_string())?;
    let offer =
        gst_webrtc::WebRTCSessionDescription::new(gst_webrtc::WebRTCSDPType::Offer, message);
    let sender = peer.clone();
    let promise = gst::Promise::with_change_func(move |reply| {
        match reply {
            Err(error) => {
                let _ = events.send(SenderEvent::Error(format!("SDP local remoto: {error:?}")));
                return;
            }
            Ok(reply) => {
                if let Some(error) = reply.and_then(|s| s.get::<glib::Error>("error").ok()) {
                    let _ = events.send(SenderEvent::Error(error.to_string()));
                    return;
                }
            }
        }
        let _ = events.send(SenderEvent::RemoteReady);
        let peer_for_answer = sender.clone();
        let result = gst::Promise::with_change_func(move |reply| {
            let result = (|| -> Result<String, String> {
                let reply = reply
                    .map_err(|e| format!("Respuesta SDP local: {e:?}"))?
                    .ok_or("Respuesta SDP local vacía")?;
                let description = reply
                    .get::<gst_webrtc::WebRTCSessionDescription>("answer")
                    .map_err(|e| e.to_string())?;
                let sdp = description.sdp().as_text().map_err(|e| e.to_string())?;
                peer_for_answer.emit_by_name::<()>(
                    "set-local-description",
                    &[&description, &None::<gst::Promise>],
                );
                Ok(sdp)
            })();
            let _ = events.send(match result {
                Ok(sdp) => SenderEvent::Answer(sdp),
                Err(error) => SenderEvent::Error(error),
            });
        });
        sender.emit_by_name::<()>("create-answer", &[&None::<gst::Structure>, &result]);
    });
    peer.emit_by_name::<()>("set-remote-description", &[&offer, &promise]);
    Ok(())
}

pub async fn run(
    seconds: u64,
    engine: MediaEngine,
    mut receiver_events: mpsc::UnboundedReceiver<MediaEvent>,
    cancel: Arc<AtomicBool>,
) -> Result<(), String> {
    if gst::ElementFactory::find("capssetter").is_none() {
        return Err("La prueba sintética requiere el componente GStreamer capssetter".into());
    }
    let encoder = if gst::ElementFactory::find("openh264enc").is_some() {
        "openh264enc bitrate=1000000 gop-size=30 complexity=low"
    } else if gst::ElementFactory::find("x264enc").is_some() {
        "x264enc bitrate=1000 key-int-max=30 tune=zerolatency speed-preset=ultrafast"
    } else {
        return Err("La prueba sintética requiere openh264enc o x264enc".into());
    };
    let pipeline = gst::parse::launch(&format!(
        // GStreamer's caps matching compares the profile bytes literally.
        // Normalize the equivalent CB flags after validating the real SPS;
        // this changes only the diagnostic sender, never the production offer.
        "videotestsrc is-live=true pattern=ball ! video/x-raw,format=I420,width=640,height=360,framerate=30/1 ! {encoder} ! video/x-h264,profile=constrained-baseline,level=(string)3.1 ! h264parse ! rtph264pay name=synthetic-pay pt=96 config-interval=-1 ! capssetter caps=\"application/x-rtp,profile-level-id=(string)42e01f,level-asymmetry-allowed=(string)1\" ! application/x-rtp,media=video,encoding-name=H264,payload=96,clock-rate=90000 ! webrtcbin name=sender bundle-policy=max-bundle"
    ))
    .map_err(|e| e.to_string())?
    .downcast::<gst::Pipeline>()
    .map_err(|_| "El emisor local no es un pipeline")?;
    let sender = Sender(pipeline);
    let peer = sender.0.by_name("sender").ok_or("Falta webrtcbin emisor")?;
    let bus = sender.0.bus().ok_or("Falta el bus emisor")?;
    let (events, mut sender_events) = mpsc::unbounded_channel();
    let validation_events = events.clone();
    sender
        .0
        .by_name("synthetic-pay")
        .and_then(|pay| pay.static_pad("src"))
        .ok_or("Falta el payloader sintético")?
        .add_probe(gst::PadProbeType::EVENT_DOWNSTREAM, move |_, info| {
            if let Some(event) = info.event() {
                if let gst::EventView::Caps(caps) = event.view() {
                    let result = caps
                        .caps()
                        .structure(0)
                        .and_then(|caps| caps.get::<String>("sprop-parameter-sets").ok())
                        .ok_or("El emisor sintético no publicó sus parámetros SPS".to_string())
                        .and_then(|parameters| validate_synthetic_sps(&parameters));
                    if let Err(error) = result {
                        let _ = validation_events.send(SenderEvent::Error(error));
                        return gst::PadProbeReturn::Drop;
                    }
                }
            }
            gst::PadProbeReturn::Ok
        });
    let ice_events = events.clone();
    peer.connect("on-ice-candidate", false, move |values| {
        if let (Ok(index), Ok(candidate)) = (values[1].get::<u32>(), values[2].get::<String>()) {
            let _ = ice_events.send(SenderEvent::Ice(index, candidate));
        }
        None
    });
    // Accept the receiver's RPC DataChannel but never send any HID RPC.
    peer.connect("on-data-channel", false, move |values| {
        if let Ok(channel) = values[1].get::<gst_webrtc::WebRTCDataChannel>() {
            channel.connect_on_open(|_| eprintln!("Synthetic sender RPC connected"));
        }
        None
    });
    sender
        .0
        .set_state(gst::State::Playing)
        .map_err(|e| e.to_string())?;
    engine.start()?;
    let mut remote_ready = false;
    let mut pending: Vec<(u32, String)> = Vec::new();
    let mut rpc = false;
    let mut health = FrameHealth::new(Instant::now());
    let mut first_frame = None;
    let mut sharp_enabled = false;
    let start = Instant::now();
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            Some(event)=receiver_events.recv()=>match event {
                MediaEvent::Offer{sdp}=>answer(&peer,&sdp,events.clone())?,
                MediaEvent::Ice{candidate,sdp_m_line_index}=>{
                    if remote_ready {peer.emit_by_name::<()>("add-ice-candidate",&[&sdp_m_line_index,&candidate]);}
                    else{pending.push((sdp_m_line_index,candidate));}
                },
                MediaEvent::State(state)=>{
                    eprintln!("Synthetic receiver: {state}");
                    if state=="connected" {rpc=true;}
                    if matches!(state.as_str(),"failed"|"closed"|"disconnected") {return Err(format!("Receptor sintético: {state}"));}
                },
                MediaEvent::Decoder(name)=>eprintln!("Synthetic decoder: {name}"),
                MediaEvent::Error(error)=>return Err(error),
                MediaEvent::Rpc(_)=>{},
            },
            Some(event)=sender_events.recv()=>match event {
                SenderEvent::Answer(sdp)=>engine.handle_answer(&sdp)?,
                SenderEvent::Ice(index,candidate)=>engine.add_ice(index,&candidate),
                SenderEvent::RemoteReady=>{
                    remote_ready=true;
                    for (index,candidate) in pending.drain(..) {peer.emit_by_name::<()>("add-ice-candidate",&[&index,&candidate]);}
                },
                SenderEvent::Error(error)=>return Err(error),
            },
            _=tick.tick()=>{
                if cancel.load(Ordering::SeqCst) {return Err("Prueba sintética cancelada".into());}
                while let Some(message)=bus.pop() {
                    if let gst::MessageView::Error(error)=message.view(){return Err(format!("Emisor sintético: {} ({:?})",error.error(),error.debug()));}
                }
                let stats=engine.stats();
                let video_state=health.observe(Instant::now(),engine.frame_counts());
                if video_state==VideoState::Playing && first_frame.is_none(){first_frame=Some(Instant::now());}
                if first_frame.is_some() && video_state!=VideoState::Playing {
                    return Err("La transmisión sintética se congeló durante la prueba".into());
                }
                println!("{}",json!({"synthetic":true,"elapsedSeconds":start.elapsed().as_secs_f64(),"video":stats}));
                if let Some(first)=first_frame {
                    if !sharp_enabled && first.elapsed()>=Duration::from_secs(seconds/2) {
                        engine.configure(VideoSettings{filter:VideoFilter::Sharp,sharpness:0.35})?;
                        sharp_enabled=true;
                        eprintln!("Synthetic: spatial sharpness enabled during playback");
                    }
                    if first.elapsed()>=Duration::from_secs(seconds) {
                        if !rpc{return Err("La prueba sintética no abrió RPC".into());}
                        if !stats.decoded_fps.is_some_and(|fps|fps>0.0) || !stats.presented_fps.is_some_and(|fps|fps>0.0) {
                            return Err("La transmisión sintética dejó de decodificar o presentar cuadros después de activar el filtro".into());
                        }
                        println!("{}",json!({"result":"pass","synthetic":true,"decodedFrames":engine.frame_counts().0,"presentedFrames":engine.frame_counts().1,"rpcConnected":rpc,"hidReportsSent":0,"sharpFilterTested":sharp_enabled}));
                        return Ok(());
                    }
                } else if start.elapsed()>Duration::from_secs(20) {return Err("La prueba WebRTC sintética no produjo cuadros en 20 segundos".into());}
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_sps_accepts_equivalent_constrained_baseline_flags() {
        // Actual x264 SPS observed at 640×360, Constrained Baseline 3.1.
        assert!(
            validate_synthetic_sps("Z0LAH9oCgL/lwFqDAwNSgAAAAwCAAAAeR4wZUA==,aM48gA==").is_ok()
        );
        assert!(validate_synthetic_sps(&STANDARD.encode([0x67, 0x42, 0xe0, 0x1f])).is_ok());
    }

    #[test]
    fn synthetic_sps_rejects_incompatible_profile_level_and_invalid_data() {
        for bytes in [
            vec![0x67, 0x64, 0x00, 0x1f], // High profile.
            vec![0x67, 0x42, 0xc0, 0x2a], // Level 4.2.
            vec![0x67, 0x42, 0x80, 0x1f], // Baseline without CB constraints.
            vec![0x68, 0x42, 0xc0, 0x1f], // PPS, not SPS.
            vec![0x67],
        ] {
            assert!(validate_synthetic_sps(&STANDARD.encode(bytes)).is_err());
        }
        assert!(validate_synthetic_sps("invalid!").is_err());
    }
}
