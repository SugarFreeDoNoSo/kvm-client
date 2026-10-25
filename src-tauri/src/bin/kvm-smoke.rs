//! A real-device, video-only smoke check. Never captures or sends HID input.
//! Requires an explicitly supplied SHA-256 certificate fingerprint.

#[path = "support/loopback.rs"]
mod loopback;
#[path = "../media.rs"]
#[allow(dead_code)]
mod media;
#[path = "../net.rs"]
#[allow(dead_code)]
mod net;
#[path = "../signaling.rs"]
#[allow(dead_code)]
mod signaling;

use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc as std_mpsc, Arc,
    },
    time::{Duration, Instant},
};

use futures_util::{SinkExt, StreamExt};
use gtk::prelude::*;
use media::{MediaEngine, MediaEvent};
use serde_json::json;
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async_tls_with_config, tungstenite::Message, Connector};

struct Options {
    address: String,
    fingerprint: String,
    seconds: u64,
    synthetic: bool,
}

fn options() -> Result<Options, String> {
    let mut options = Options {
        address: "https://192.168.100.8/".into(),
        fingerprint: String::new(),
        seconds: 10,
        synthetic: false,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--address" => options.address = args.next().ok_or("Falta --address")?,
            "--fingerprint" => options.fingerprint = args.next().ok_or("Falta --fingerprint")?.to_uppercase(),
            "--seconds" => options.seconds = args.next().ok_or("Falta --seconds")?.parse().map_err(|_| "--seconds debe ser un entero")?,
            "--synthetic" => options.synthetic = true,
            "--help" | "-h" => return Err("Uso: kvm-smoke --fingerprint AA:BB:… [--address https://192.168.100.8/] [--seconds 10], o kvm-smoke --synthetic [--seconds 10]. Contraseña opcional en KVM_PASSWORD; no se envían eventos HID.".into()),
            _ => return Err(format!("Argumento desconocido: {arg}")),
        }
    }
    options.address = net::normalize_address(&options.address)?.to_string();
    if !(1..=120).contains(&options.seconds) {
        return Err("--seconds debe estar entre 1 y 120".into());
    }
    if options.synthetic {
        return Ok(options);
    }
    let fingerprint: Vec<_> = options.fingerprint.split(':').collect();
    if fingerprint.len() != 32
        || fingerprint
            .iter()
            .any(|byte| byte.len() != 2 || u8::from_str_radix(byte, 16).is_err())
    {
        return Err("Se requiere --fingerprint con la huella SHA-256 completa del dispositivo (32 bytes separados por ':')".into());
    }
    Ok(options)
}

struct TempTrust(PathBuf);
impl TempTrust {
    fn new(options: &Options) -> Result<Self, String> {
        let path =
            std::env::temp_dir().join(format!("kvm-client-smoke-{}.json", uuid::Uuid::new_v4()));
        let mut devices = serde_json::Map::new();
        devices.insert(options.address.clone(), json!(options.fingerprint));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| e.to_string())?;
        use std::io::Write;
        file.write_all(json!({"devices":devices}).to_string().as_bytes())
            .map_err(|e| e.to_string())?;
        Ok(Self(path))
    }
}
impl Drop for TempTrust {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

async fn smoke(
    options: Options,
    engine: MediaEngine,
    mut events: mpsc::UnboundedReceiver<MediaEvent>,
    cancel: Arc<AtomicBool>,
) -> Result<(), String> {
    let trust = TempTrust::new(&options)?;
    let password = std::env::var("KVM_PASSWORD").unwrap_or_default();
    let session = net::DeviceSession::login(&trust.0, &options.address, &password).await?;
    drop(password);
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
    .map_err(|e| e.to_string())?;

    let mut started = false;
    let mut answer_applied = false;
    let mut local_ice: Vec<(u32, String)> = Vec::new();
    let mut remote_ice: Vec<(u32, String)> = Vec::new();
    let mut rpc_connected = false;
    let mut first_frame = None;
    let mut decoded = false;
    let mut presented = false;
    let mut no_signal = false;
    let start = Instant::now();
    let connect_deadline = start + Duration::from_secs(20);
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            incoming = socket.next() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => match signaling::parse(&text)? {
                        signaling::Signal::Metadata => { if !started { engine.start()?; started = true; } },
                        signaling::Signal::Answer(sdp) => {
                            engine.handle_answer(&sdp)?;
                            answer_applied = true;
                            for (index,candidate) in remote_ice.drain(..) { engine.add_ice(index,&candidate); }
                            for (index,candidate) in local_ice.drain(..) {
                                socket.send(Message::Text(signaling::ice(&candidate,index).to_string().into())).await.map_err(|e|e.to_string())?;
                            }
                        },
                        signaling::Signal::Ice {candidate,index} => {
                            if answer_applied { engine.add_ice(index,&candidate); } else { remote_ice.push((index,candidate)); }
                        },
                        signaling::Signal::Ping => socket.send(Message::Text("pong".into())).await.map_err(|e|e.to_string())?,
                        signaling::Signal::Ignore => {},
                    },
                    Some(Ok(Message::Ping(bytes))) => socket.send(Message::Pong(bytes)).await.map_err(|e|e.to_string())?,
                    Some(Ok(Message::Close(_))) | None => return Err("La señalización se cerró antes de terminar la prueba".into()),
                    Some(Err(error)) => return Err(format!("Señalización: {error}")),
                    _ => {},
                }
            },
            Some(event) = events.recv() => match event {
                MediaEvent::Offer {sdp} => {socket.send(Message::Text(signaling::offer(&sdp).to_string().into())).await.map_err(|e|e.to_string())?;},
                MediaEvent::Ice {candidate,sdp_m_line_index} => {
                    if answer_applied {socket.send(Message::Text(signaling::ice(&candidate,sdp_m_line_index).to_string().into())).await.map_err(|e|e.to_string())?;}
                    else {local_ice.push((sdp_m_line_index,candidate));}
                },
                MediaEvent::State(state) => {
                    eprintln!("WebRTC: {state}");
                    if state == "connected" {
                        rpc_connected = true;
                        // Read-only diagnostic; never send keyboard/mouse reports.
                        engine.send_rpc(json!({"jsonrpc":"2.0","method":"getVideoState","params":{},"id":900001}))?;
                    }
                    if matches!(state.as_str(),"failed"|"disconnected"|"closed") {return Err(format!("WebRTC: {state}"));}
                },
                MediaEvent::Decoder(name) => eprintln!("Decoder: {name}"),
                MediaEvent::Error(error) => return Err(error),
                MediaEvent::Rpc(value) => {
                    let video = if value["method"] == "videoInputState" {Some(&value["params"])}
                        else if value["id"] == 900001 {Some(&value["result"])} else {None};
                    if let Some(video) = video {
                        eprintln!("HDMI: {video}");
                        no_signal = video["error"] == "no_signal" || video["error"] == "no-signals";
                    }
                },
            },
            _ = tick.tick() => {
                if cancel.load(Ordering::SeqCst) {return Err("Prueba cancelada al cerrar la ventana".into());}
                let stats = engine.stats();
                decoded |= stats.decoded_fps.is_some_and(|fps| fps > 0.0);
                presented |= stats.presented_fps.is_some_and(|fps| fps > 0.0);
                if decoded && presented && first_frame.is_none() {first_frame = Some(Instant::now());}
                println!("{}",json!({"elapsedSeconds":start.elapsed().as_secs_f64(),"video":stats}));
                if first_frame.is_some_and(|time| time.elapsed() >= Duration::from_secs(options.seconds)) {
                    socket.close(None).await.map_err(|e|e.to_string())?;
                    if !rpc_connected {return Err("Hubo video pero el canal RPC no abrió".into());}
                    println!("{}",json!({"result":"pass","decodedFrames":decoded,"presentedFrames":presented,"rpcConnected":rpc_connected,"hidReportsSent":0}));
                    return Ok(());
                }
                if first_frame.is_none() && Instant::now() >= connect_deadline {
                    if no_signal {
                        return Err("WebRTC y RPC conectaron, pero el KVM informa HDMI no_signal: conecta o activa la salida de video del equipo remoto".into());
                    }
                    return Err("No se observaron cuadros decodificados y presentados en 20 segundos".into());
                }
            },
        }
    }
}

fn run() -> Result<(), String> {
    let options = options()?;
    gtk::init().map_err(|e| format!("No se pudo abrir GTK: {e}"))?;
    let window = gtk::Window::new(gtk::WindowType::Toplevel);
    window.set_title("KVM · prueba de video sin control HID");
    window.set_default_size(960, 600);
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let engine = MediaEngine::new(Arc::new(move |event| {
        let _ = event_tx.send(event);
    }))?;
    let widget: gtk::Widget = unsafe {
        gtk::glib::translate::from_glib_none(engine.widget_ptr() as *mut gtk::ffi::GtkWidget)
    };
    window.add(&widget);
    window.show_all();
    let cancel = Arc::new(AtomicBool::new(false));
    let closed = cancel.clone();
    window.connect_delete_event(move |_, _| {
        closed.store(true, Ordering::SeqCst);
        gtk::glib::Propagation::Stop
    });
    let (result_tx, result_rx) = std_mpsc::channel();
    let engine_worker = engine.clone();
    let worker = std::thread::Builder::new()
        .name("kvm-smoke-network".into())
        .spawn(move || {
            let result = (|| {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                    .map_err(|e| e.to_string())?;
                if options.synthetic {
                    runtime.block_on(loopback::run(
                        options.seconds,
                        engine_worker.clone(),
                        event_rx,
                        cancel,
                    ))
                } else {
                    runtime.block_on(smoke(options, engine_worker.clone(), event_rx, cancel))
                }
            })();
            engine_worker.shutdown();
            let _ = result_tx.send(result);
        })
        .map_err(|e| e.to_string())?;
    let outcome = Arc::new(std::sync::Mutex::new(None));
    let final_outcome = outcome.clone();
    gtk::glib::timeout_add_local(Duration::from_millis(100), move || {
        match result_rx.try_recv() {
            Ok(result) => {
                *final_outcome.lock().unwrap() = Some(result);
                gtk::main_quit();
                gtk::glib::ControlFlow::Break
            }
            Err(std_mpsc::TryRecvError::Disconnected) => {
                *final_outcome.lock().unwrap() =
                    Some(Err("El trabajador de red terminó sin resultado".into()));
                gtk::main_quit();
                gtk::glib::ControlFlow::Break
            }
            Err(std_mpsc::TryRecvError::Empty) => gtk::glib::ControlFlow::Continue,
        }
    });
    gtk::main();
    worker
        .join()
        .map_err(|_| "El trabajador de prueba terminó inesperadamente")?;
    window.hide();
    let result = outcome
        .lock()
        .unwrap()
        .take()
        .ok_or("La prueba terminó sin resultado")?;
    result
}

fn main() {
    if let Err(error) = run() {
        eprintln!("kvm-smoke: {error}");
        std::process::exit(1);
    }
}
