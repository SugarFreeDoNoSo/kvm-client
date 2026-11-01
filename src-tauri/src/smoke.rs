//! A real-device, video-only smoke check. Never captures or sends HID input.
//! Requires an explicitly supplied SHA-256 certificate fingerprint.

mod loopback;
use crate::{
    health::{FrameHealth, VideoState},
    media, net, signaling, video_host,
};

use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use futures_util::{SinkExt, StreamExt};
use media::{MediaEngine, MediaEvent};
use serde_json::json;
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async_tls_with_config, tungstenite::Message, Connector};

struct Options {
    address: String,
    fingerprint: String,
    seconds: u64,
    synthetic: bool,
    input_self_test: bool,
    sunshine_self_test: bool,
    dashboard_self_test: bool,
}

fn options() -> Result<Options, String> {
    let mut options = Options {
        address: "https://192.168.100.8/".into(),
        fingerprint: String::new(),
        seconds: 10,
        synthetic: false,
        input_self_test: false,
        sunshine_self_test: false,
        dashboard_self_test: false,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--address" => options.address = args.next().ok_or("Falta --address")?,
            "--fingerprint" => options.fingerprint = args.next().ok_or("Falta --fingerprint")?.to_uppercase(),
            "--seconds" => options.seconds = args.next().ok_or("Falta --seconds")?.parse().map_err(|_| "--seconds debe ser un entero")?,
            "--synthetic" => options.synthetic = true,
            "--sunshine-self-test" => options.sunshine_self_test = true,
            "--dashboard-self-test" => options.dashboard_self_test = true,
            "--input-self-test" => options.input_self_test = true,
            "--help" | "-h" => return Err("Uso: kvm-smoke --fingerprint AA:BB:… [--address https://192.168.100.8/] [--seconds 10], o kvm-smoke --synthetic [--seconds 10]. Contraseña opcional en KVM_PASSWORD; no se envían eventos HID.".into()),
            _ => return Err(format!("Argumento desconocido: {arg}")),
        }
    }
    options.address = net::normalize_address(&options.address)?.to_string();
    if !(1..=3600).contains(&options.seconds) {
        return Err("--seconds debe estar entre 1 y 3600".into());
    }
    if options.dashboard_self_test
        && (options.input_self_test || options.synthetic || options.sunshine_self_test)
    {
        return Err("Usa --dashboard-self-test por separado".into());
    }
    if options.sunshine_self_test && (options.input_self_test || options.synthetic) {
        return Err("Usa --sunshine-self-test por separado".into());
    }
    if options.input_self_test {
        if options.synthetic {
            return Err("Usa --input-self-test o --synthetic por separado".into());
        }
        if !cfg!(target_os = "macos") {
            return Err("--input-self-test requiere una sesión gráfica macOS".into());
        }
        return Ok(options);
    }
    if options.synthetic || options.sunshine_self_test || options.dashboard_self_test {
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
    let mut health = FrameHealth::new(Instant::now());
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
                        no_signal = video["ready"] == false || video["error"] == "no_signal" || video["error"] == "no-signals";
                        if let Some(ready) = video["ready"].as_bool() {health.source_ready(ready, Instant::now());}
                    }
                },
            },
            _ = tick.tick() => {
                if cancel.load(Ordering::SeqCst) {return Err("Prueba cancelada al cerrar la ventana".into());}
                let stats = engine.stats();
                let video_state = health.observe(Instant::now(), engine.frame_counts());
                if video_state == VideoState::Playing && first_frame.is_none() {first_frame = Some(Instant::now());}
                if first_frame.is_some() && video_state != VideoState::Playing {
                    return Err("La transmisión dejó de decodificar o presentar cuadros durante la prueba".into());
                }
                println!("{}",json!({"elapsedSeconds":start.elapsed().as_secs_f64(),"video":stats}));
                if first_frame.is_some_and(|time| time.elapsed() >= Duration::from_secs(options.seconds)) {
                    if !stats.decoded_fps.is_some_and(|fps| fps > 0.0) || !stats.presented_fps.is_some_and(|fps| fps > 0.0) {
                        return Err("La transmisión no mantiene flujo de cuadros al finalizar".into());
                    }
                    socket.close(None).await.map_err(|e|e.to_string())?;
                    if !rpc_connected {return Err("Hubo video pero el canal RPC no abrió".into());}
                    println!("{}",json!({"result":"pass","decodedFrames":engine.frame_counts().0,"presentedFrames":engine.frame_counts().1,"rpcConnected":rpc_connected,"hidReportsSent":0}));
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

/// Video diagnostics never create input capture. The explicit input self-test
/// uses only a simulated transport and cannot connect to a KVM.
pub fn run() -> Result<(), String> {
    crate::runtime_environment::configure()?;
    let options = options()?;
    let mut context = crate::app_context();
    context.config_mut().app.windows.clear();
    let outcome = Arc::new(std::sync::Mutex::new(None));
    let result = outcome.clone();
    let cancellation = Arc::new(AtomicBool::new(false));
    let cancel = cancellation.clone();
    let app = tauri::Builder::default()
        .setup(move |app| {
            // A blank webview uses Tauri's stable window API. The native video
            // host covers it; no application JS, IPC or input engine is loaded.
            let window = tauri::WebviewWindowBuilder::new(
                app,
                "diagnostic",
                tauri::WebviewUrl::External("about:blank".parse()?),
            )
                .title("KVM · prueba de video sin control HID")
                .inner_size(960.0, 600.0)
            .build()?;
            if options.dashboard_self_test {
                video_host::install(&window, Arc::new(|| {}))?;
            } else {
                video_host::install_diagnostic(&window.as_ref().window())?;
            }
            let closed = cancel.clone();
            window.on_window_event(move |event| {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    closed.store(true, Ordering::SeqCst);
                }
            });
            if options.dashboard_self_test {
                let completed = result.clone();
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    let result = video_host::self_test(window).await;
                    *completed.lock().unwrap_or_else(|e| e.into_inner()) = Some(result);
                    let exit_handle = handle.clone();
                    let _ = handle.run_on_main_thread(move || exit_handle.exit(0));
                });
                return Ok(());
            }
            if options.sunshine_self_test {
                let completed = result.clone();
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    let result = crate::sunshine::synthetic_check(&handle).await;
                    *completed.lock().unwrap_or_else(|e| e.into_inner()) = Some(result);
                    let exit_handle = handle.clone();
                    let _ = handle.run_on_main_thread(move || exit_handle.exit(0));
                });
                return Ok(());
            }
            #[cfg(target_os = "macos")]
            if options.input_self_test {
                let completed = result.clone();
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    let result = crate::input::self_test(window).await;
                    if result.is_ok() {
                        println!("{}", json!({"result":"pass","inputSelfTest":true,"kvmConnections":0,"hidReportsSentToKvm":0}));
                    }
                    *completed.lock().unwrap_or_else(|e|e.into_inner())=Some(result);
                    let exit_handle=handle.clone();
                    let _=handle.run_on_main_thread(move ||exit_handle.exit(0));
                });
                return Ok(());
            }
            let (event_tx, event_rx) = mpsc::unbounded_channel();
            let engine = MediaEngine::new(Arc::new(move |event| {
                let _ = event_tx.send(event);
            }))?;
            video_host::attach(&engine)?;
            video_host::show_notice(None);
            let handle = app.handle().clone();
            let completed = result.clone();
            std::thread::Builder::new()
                .name("kvm-smoke-network".into())
                .spawn(move || {
                    let result = tokio::runtime::Builder::new_multi_thread()
                        .worker_threads(2)
                        .enable_all()
                        .build()
                        .map_err(|e| e.to_string())
                        .and_then(|runtime| {
                            if options.synthetic {
                                runtime.block_on(loopback::run(
                                    options.seconds,
                                    engine.clone(),
                                    event_rx,
                                    cancel,
                                ))
                            } else {
                                runtime.block_on(smoke(options, engine.clone(), event_rx, cancel))
                            }
                        });
                    engine.shutdown();
                    *completed.lock().unwrap_or_else(|e| e.into_inner()) = Some(result);
                    let exit_handle = handle.clone();
                    let _ = handle.run_on_main_thread(move || {
                        video_host::show_placeholder();
                        exit_handle.exit(0);
                    });
                })?;
            Ok(())
        })
        .build(context)
        .map_err(|e| e.to_string())?;
    let exit_outcome = outcome.clone();
    // `run` exits the process directly and would turn a failed diagnostic into
    // exit 0. Return to the CLI so its Result controls the actual exit status.
    app.run_return(move |_, event| {
        if let tauri::RunEvent::ExitRequested { api, .. } = event {
            if exit_outcome
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_none()
            {
                api.prevent_exit();
                cancellation.store(true, Ordering::SeqCst);
            }
        }
    });
    let result = outcome
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
        .ok_or("La aplicación de diagnóstico terminó sin resultado")?;
    result
}
