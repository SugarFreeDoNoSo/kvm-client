use super::*;

#[test]
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
