use super::*;

#[test]
#[cfg(target_os = "linux")]
#[ignore = "requires an interactive desktop; briefly enables capture without sending input to a KVM"]
fn desktop_capture_stays_requested_until_cancelled() {
    let engine = InputEngine::new(
        Arc::new(|_| Ok(())),
        Arc::new(|status| eprintln!("capture: {status:?}")),
    );
    engine.capture(true).unwrap();
    thread::sleep(Duration::from_secs(3));
    let requested = engine.is_requested();
    engine.capture(false).unwrap();
    assert!(requested, "desktop capture stopped before cancellation");
}

#[cfg(target_os = "macos")]
fn simulated_native_dispatch(engine: &InputEngine, stops: Arc<AtomicU64>) {
    let weak = Arc::downgrade(&engine.shared);
    *engine.shared.native_dispatch.lock().unwrap() = Some(Arc::new(move |enabled, generation| {
        let shared = weak.upgrade().ok_or("input engine dropped")?;
        if shared.generation.load(Ordering::Acquire) == generation {
            if enabled && shared.running(generation) {
                shared.report(true, "simulated AppKit", "captured");
            } else if !enabled {
                stops.fetch_add(1, Ordering::Relaxed);
            }
        }
        Ok(())
    }));
}

#[test]
#[cfg(target_os = "macos")]
fn native_capture_cancellation_invalidates_callbacks_and_releases_held_keys() {
    let reports = Arc::new(Mutex::new(Vec::new()));
    let output = reports.clone();
    let engine = InputEngine::new(
        Arc::new(move |report| {
            output.lock().unwrap().push(report);
            Ok(())
        }),
        Arc::new(|_| {}),
    );
    let stops = Arc::new(AtomicU64::new(0));
    simulated_native_dispatch(&engine, stops.clone());
    engine.capture(true).unwrap();
    let generation = engine.shared.generation.load(Ordering::Acquire);
    assert!(engine.shared.running(generation));
    engine.shared.key(225, true).unwrap();
    engine.capture(false).unwrap();
    assert!(!engine.is_captured());
    assert!(!engine.shared.running(generation));
    assert_eq!(stops.load(Ordering::Relaxed), 1);
    let reports = reports.lock().unwrap();
    assert_eq!(reports.last().unwrap()["params"]["modifier"], 0);
    assert_eq!(
        reports.last().unwrap()["params"]["keys"],
        serde_json::json!([])
    );
}

#[test]
#[cfg(target_os = "macos")]
fn congested_transport_stops_native_capture_without_waiting_for_an_input_event() {
    let engine = InputEngine::new(
        Arc::new(|_| Err("blocked transport".into())),
        Arc::new(|_| {}),
    );
    let stops = Arc::new(AtomicU64::new(0));
    simulated_native_dispatch(&engine, stops.clone());
    engine.capture(true).unwrap();
    engine.shared.motion(100.0, -20.0).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while (engine.is_requested() || stops.load(Ordering::Relaxed) == 0) && Instant::now() < deadline
    {
        thread::sleep(Duration::from_millis(5));
    }
    assert!(!engine.is_requested());
    assert!(!engine.is_captured());
    assert_eq!(stops.load(Ordering::Relaxed), 1);
    assert_eq!(engine.shared.scheduler.lock().unwrap().len(), 2);
}

#[test]
#[cfg(target_os = "macos")]
fn only_the_last_engine_owner_cancels_native_capture() {
    let engine = InputEngine::new(Arc::new(|_| Ok(())), Arc::new(|_| {}));
    let stops = Arc::new(AtomicU64::new(0));
    simulated_native_dispatch(&engine, stops.clone());
    engine.capture(true).unwrap();
    let clone = engine.clone();
    drop(clone);
    assert!(engine.is_captured());
    assert_eq!(stops.load(Ordering::Relaxed), 0);
    drop(engine);
    assert_eq!(stops.load(Ordering::Relaxed), 1);
}

#[test]
fn disconnected_release_retries_do_not_mark_idle_capture_congested() {
    let attempts = Arc::new(AtomicU64::new(0));
    let available = Arc::new(AtomicBool::new(false));
    let send_attempts = attempts.clone();
    let send_available = available.clone();
    let engine = InputEngine::new(
        Arc::new(move |_| {
            send_attempts.fetch_add(1, Ordering::Relaxed);
            if send_available.load(Ordering::Acquire) {
                Ok(())
            } else {
                Err("disconnected".into())
            }
        }),
        Arc::new(|_| {}),
    );
    engine.capture(false).unwrap();
    let deadline = Instant::now() + Duration::from_secs(1);
    while attempts.load(Ordering::Relaxed) < 6 && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(5));
    }
    assert!(attempts.load(Ordering::Relaxed) >= 6);
    assert!(!engine.stats().congested);
    available.store(true, Ordering::Release);
    while engine.stats().queue_depth > 0 && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(engine.stats().queue_depth, 0);
    assert!(!engine.stats().congested);
}
