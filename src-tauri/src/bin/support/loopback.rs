//! Local encoded-video test. Both sides use real WebRTC/DTLS/SRTP; the receiver
//! is the production MediaEngine and no real KVM connection is created.

use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_sdp as gst_sdp;
use gstreamer_webrtc as gst_webrtc;
use serde_json::json;
use tokio::sync::mpsc;

use crate::media::{MediaEngine, MediaEvent, VideoFilter, VideoSettings};

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
    if gst::ElementFactory::find("openh264enc").is_none() {
        return Err("La prueba sintética requiere el encoder GStreamer openh264enc".into());
    }
    let pipeline = gst::parse::launch(concat!(
        "videotestsrc is-live=true pattern=ball ! ",
        "video/x-raw,format=I420,width=640,height=360,framerate=30/1 ! ",
        "openh264enc bitrate=1000000 gop-size=30 complexity=low ! ",
        "video/x-h264,profile=constrained-baseline ! ",
        "rtph264pay pt=96 config-interval=-1 ! ",
        "application/x-rtp,media=video,encoding-name=H264,payload=96,clock-rate=90000 ! ",
        "webrtcbin name=sender bundle-policy=max-bundle"
    ))
    .map_err(|e| e.to_string())?
    .downcast::<gst::Pipeline>()
    .map_err(|_| "El emisor local no es un pipeline")?;
    let sender = Sender(pipeline);
    let peer = sender.0.by_name("sender").ok_or("Falta webrtcbin emisor")?;
    let bus = sender.0.bus().ok_or("Falta el bus emisor")?;
    let (events, mut sender_events) = mpsc::unbounded_channel();
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
    let mut decoded = false;
    let mut presented = false;
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
                decoded |= stats.decoded_fps.is_some_and(|fps|fps>0.0);
                presented |= stats.presented_fps.is_some_and(|fps|fps>0.0);
                if decoded && presented && first_frame.is_none(){first_frame=Some(Instant::now());}
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
                        println!("{}",json!({"result":"pass","synthetic":true,"decodedFrames":decoded,"presentedFrames":presented,"rpcConnected":rpc,"hidReportsSent":0,"sharpFilterTested":sharp_enabled}));
                        return Ok(());
                    }
                } else if start.elapsed()>Duration::from_secs(20) {return Err("La prueba WebRTC sintética no produjo cuadros en 20 segundos".into());}
            },
        }
    }
}
