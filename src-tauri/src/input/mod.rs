//! Native input is captured and scheduled without crossing the JavaScript bridge.
#[cfg(target_os = "linux")]
mod ei;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "linux")]
mod portal;
#[cfg(test)]
mod runtime_tests;
mod scheduler;
#[cfg(target_os = "linux")]
mod x11;

pub use scheduler::InputSettings;

/// Exercises the AppKit capture lifecycle with an isolated in-memory HID
/// transport. This diagnostic never creates a device or network connection.
#[cfg(target_os = "macos")]
pub(crate) async fn self_test(window: tauri::WebviewWindow) -> Result<(), String> {
    macos::self_test(window).await
}

use scheduler::Scheduler;
use serde::Serialize;
use serde_json::Value;
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

pub type RpcSender = Arc<dyn Fn(Value) -> Result<(), String> + Send + Sync>;
pub type CaptureCallback = Arc<dyn Fn(CaptureStatus) + Send + Sync>;
#[cfg(target_os = "macos")]
type NativeDispatch = Arc<dyn Fn(bool, u64) -> Result<(), String> + Send + Sync>;
const INPUT_DISABLED_MESSAGE: &str =
    "El mouse y teclado del KVM están desactivados; habilítalos antes de capturar";

#[derive(Debug, Clone, Serialize)]
pub struct CaptureStatus {
    pub active: bool,
    pub requested: bool,
    pub backend: String,
    pub message: String,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InputStats {
    pub input_hz: f64,
    pub target_hz: u32,
    pub queue_depth: usize,
    pub congested: bool,
}

pub(super) struct Shared {
    scheduler: Mutex<Scheduler>,
    send: RpcSender,
    callback: CaptureCallback,
    generation: AtomicU64,
    enabled: AtomicBool,
    requested: AtomicBool,
    active: AtomicBool,
    stats: Mutex<InputStats>,
    #[cfg(target_os = "macos")]
    native_dispatch: Mutex<Option<NativeDispatch>>,
}

impl Shared {
    pub(super) fn running(&self, generation: u64) -> bool {
        self.enabled.load(Ordering::Acquire)
            && self.requested.load(Ordering::Acquire)
            && self.generation.load(Ordering::Acquire) == generation
    }

    pub(super) fn report(&self, active: bool, backend: &str, message: impl Into<String>) {
        // Publish capture transitions under the same lock as HID enablement. A
        // delayed native activation must not overtake the disabled notification.
        let _scheduler = self.scheduler.lock().unwrap_or_else(|e| e.into_inner());
        let requested =
            self.enabled.load(Ordering::Acquire) && self.requested.load(Ordering::Acquire);
        if active && !requested {
            return;
        }
        self.active.store(active, Ordering::Release);
        (self.callback)(CaptureStatus {
            active,
            requested,
            backend: backend.into(),
            message: message.into(),
        });
    }

    pub(super) fn motion(&self, dx: f64, dy: f64) -> Result<(), String> {
        let mut scheduler = self.scheduler.lock().map_err(|e| e.to_string())?;
        if self.enabled.load(Ordering::Acquire) {
            scheduler.motion(dx, dy)?;
        }
        Ok(())
    }

    pub(super) fn button(&self, bit: u8, pressed: bool) -> Result<(), String> {
        let mut scheduler = self.scheduler.lock().map_err(|e| e.to_string())?;
        if self.enabled.load(Ordering::Acquire) {
            scheduler.button(bit, pressed)?;
        }
        Ok(())
    }

    pub(super) fn wheel(&self, steps: f64) -> Result<(), String> {
        let mut scheduler = self.scheduler.lock().map_err(|e| e.to_string())?;
        if self.enabled.load(Ordering::Acquire) {
            scheduler.wheel(steps)?;
        }
        Ok(())
    }

    pub(super) fn key(&self, usage: u8, pressed: bool) -> Result<bool, String> {
        let mut scheduler = self.scheduler.lock().map_err(|e| e.to_string())?;
        if self.enabled.load(Ordering::Acquire) {
            scheduler.key(usage, pressed)
        } else {
            Ok(false)
        }
    }

    #[cfg(target_os = "linux")]
    pub(super) fn evdev_key(&self, code: u32, pressed: bool) -> Result<bool, String> {
        match scheduler::evdev_to_hid(code) {
            Some(usage) => self.key(usage, pressed),
            None => Ok(false),
        }
    }

    #[cfg(target_os = "macos")]
    fn dispatch_native(&self, enabled: bool, generation: u64) -> Result<(), String> {
        let dispatch = self
            .native_dispatch
            .lock()
            .map_err(|e| e.to_string())?
            .clone();
        match dispatch {
            Some(dispatch) => dispatch(enabled, generation),
            None if !enabled => Ok(()),
            None => Err("La captura macOS todavía no se ha inicializado".into()),
        }
    }

    #[cfg(target_os = "macos")]
    fn stop_native(&self, message: impl Into<String>) {
        self.requested.store(false, Ordering::Release);
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        self.release();
        self.report(false, "macOS · AppKit", message);
        let _ = self.dispatch_native(false, generation);
    }

    pub(super) fn release(&self) {
        if let Ok(mut scheduler) = self.scheduler.lock() {
            scheduler.release();
        }
    }
}

#[derive(Clone)]
pub struct InputEngine {
    shared: Arc<Shared>,
    // Capture workers hold Shared as well. Count engine handles independently so
    // dropping the final owner also cancels a worker that is still capturing.
    owners: Arc<()>,
}

impl InputEngine {
    pub fn new(send: RpcSender, callback: CaptureCallback) -> Self {
        let shared = Arc::new(Shared {
            scheduler: Mutex::new(Scheduler::default()),
            send,
            callback,
            generation: AtomicU64::new(0),
            enabled: AtomicBool::new(true),
            requested: AtomicBool::new(false),
            active: AtomicBool::new(false),
            stats: Mutex::new(InputStats {
                target_hz: 125,
                ..Default::default()
            }),
            #[cfg(target_os = "macos")]
            native_dispatch: Mutex::new(None),
        });
        let weak = Arc::downgrade(&shared);
        thread::Builder::new()
            .name("kvm-hid-scheduler".into())
            .spawn(move || {
                let input_clock = Instant::now();
                let mut next = input_clock;
                let mut sample = Instant::now();
                let mut reports = 0u32;
                let mut failures = 0u32;
                let mut successes = 0u32;
                let mut backoff = 1u32;
                let mut blocked_since: Option<Instant> = None;
                loop {
                    let Some(shared) = weak.upgrade() else {
                        break;
                    };
                    let mut scheduler = match shared.scheduler.lock() {
                        Ok(s) => s,
                        Err(_) => break,
                    };
                    let hz = scheduler.settings.hz;
                    let mut congested = false;
                    // At most one HID report per tick: even a large delta stays paced.
                    // Keep report selection stable if the keyboard deadline
                    // passes between peeking and acknowledging a mouse report.
                    let report_time = input_clock.elapsed();
                    if let Some(report) = scheduler.peek_at(report_time) {
                        match (shared.send)(report) {
                            Ok(()) => {
                                scheduler.acknowledge_at(report_time);
                                reports += 1;
                                successes += 1;
                                failures = 0;
                                blocked_since = None;
                            }
                            Err(_) => {
                                failures += 1;
                                successes = 0;
                                congested = true;
                                blocked_since.get_or_insert_with(Instant::now);
                            }
                        }
                    }
                    if failures >= 3 {
                        backoff = (backoff * 2).min(8);
                        failures = 0;
                    }
                    if successes >= 32 {
                        backoff = (backoff / 2).max(1);
                        successes = 0;
                    }
                    // Retry release reports while disconnected without carrying
                    // backoff or a stale congestion warning into a later capture.
                    if !shared.active.load(Ordering::Acquire) || scheduler.len() == 0 {
                        backoff = 1;
                        failures = 0;
                        successes = 0;
                        blocked_since = None;
                        congested = false;
                    }
                    let effective = (hz / backoff).max(125);
                    // Stop a failing transport before old motion can execute long after
                    // the user made it. Release intentionally discards stale movement.
                    let stalled = blocked_since
                        .is_some_and(|since| since.elapsed() > Duration::from_millis(250))
                        && shared.active.load(Ordering::Acquire);
                    let stalled_generation = if stalled {
                        let generation = shared.generation.load(Ordering::Acquire);
                        shared.requested.store(false, Ordering::Release);
                        scheduler.release();
                        blocked_since = None;
                        Some(generation)
                    } else {
                        None
                    };
                    if let Ok(mut stats) = shared.stats.lock() {
                        stats.target_hz = hz;
                        stats.queue_depth = scheduler.len();
                        stats.congested = congested || backoff > 1;
                        if sample.elapsed() >= Duration::from_secs(1) {
                            stats.input_hz = reports as f64 / sample.elapsed().as_secs_f64();
                            reports = 0;
                            sample = Instant::now();
                        }
                    }
                    drop(scheduler);
                    if let Some(generation) = stalled_generation.filter(|generation| {
                        shared.generation.load(Ordering::Acquire) == *generation
                    }) {
                        #[cfg(target_os = "macos")]
                        let _ = shared.dispatch_native(false, generation);
                        #[cfg(not(target_os = "macos"))]
                        let _ = generation;
                        shared.report(
                            false,
                            "",
                            "Captura liberada: el transporte de control está congestionado",
                        );
                    }
                    drop(shared);
                    let interval = Duration::from_nanos(1_000_000_000 / effective as u64);
                    next += interval;
                    let now = Instant::now();
                    if next > now {
                        thread::sleep(next - now);
                    } else {
                        next = now;
                    }
                }
            })
            .expect("failed to start input scheduler");
        Self {
            shared,
            owners: Arc::new(()),
        }
    }

    #[cfg(target_os = "macos")]
    pub fn initialize_macos(&self, window: tauri::WebviewWindow) -> Result<(), String> {
        macos::initialize(&self.shared, window)
    }

    pub fn set_settings(&self, settings: InputSettings) -> Result<(), String> {
        self.shared
            .scheduler
            .lock()
            .map_err(|e| e.to_string())?
            .set_settings(settings)
    }

    pub fn stats(&self) -> InputStats {
        self.shared
            .stats
            .lock()
            .map(|s| s.clone())
            .unwrap_or_default()
    }

    pub fn is_captured(&self) -> bool {
        self.is_enabled() && self.is_requested() && self.shared.active.load(Ordering::Acquire)
    }
    pub fn is_requested(&self) -> bool {
        self.shared.requested.load(Ordering::Acquire)
    }

    pub fn is_enabled(&self) -> bool {
        self.shared.enabled.load(Ordering::Acquire)
    }

    /// Disable HID independently of the media transport. The scheduler lock also
    /// gates native callbacks already in flight when capture is cancelled.
    pub fn set_enabled(&self, enabled: bool) -> Result<(), String> {
        let generation = {
            let mut scheduler = self.shared.scheduler.lock().map_err(|e| e.to_string())?;
            if self.shared.enabled.swap(enabled, Ordering::AcqRel) == enabled {
                return Ok(());
            }
            if enabled {
                return Ok(());
            }
            self.shared.requested.store(false, Ordering::Release);
            let generation = self.shared.generation.fetch_add(1, Ordering::AcqRel) + 1;
            scheduler.release();
            self.send_pending_releases(&mut scheduler);
            generation
        };
        self.shared.report(
            false,
            "",
            "Mouse y teclado del KVM desactivados; control local",
        );
        #[cfg(target_os = "macos")]
        return self.shared.dispatch_native(false, generation);
        #[cfg(not(target_os = "macos"))]
        {
            let _ = generation;
            Ok(())
        }
    }

    pub fn capture(&self, enabled: bool) -> Result<(), String> {
        if !enabled {
            self.shared.requested.store(false, Ordering::Release);
            let generation = self.shared.generation.fetch_add(1, Ordering::AcqRel) + 1;
            #[cfg(target_os = "macos")]
            let native_result = self.shared.dispatch_native(false, generation);
            #[cfg(not(target_os = "macos"))]
            let _ = generation;
            self.release_all();
            self.shared
                .report(false, "", "Control local; captura liberada");
            #[cfg(target_os = "macos")]
            return native_result;
            #[cfg(not(target_os = "macos"))]
            return Ok(());
        }
        let generation = {
            let _scheduler = self.shared.scheduler.lock().map_err(|e| e.to_string())?;
            if !self.is_enabled() {
                return Err(INPUT_DISABLED_MESSAGE.into());
            }
            if self.shared.requested.swap(true, Ordering::AcqRel) {
                return Ok(());
            }
            self.shared.generation.fetch_add(1, Ordering::AcqRel) + 1
        };
        #[cfg(target_os = "macos")]
        {
            self.shared
                .report(false, "macOS · AppKit", "Preparando captura nativa…");
            return self
                .shared
                .dispatch_native(true, generation)
                .inspect_err(|_| {
                    self.shared.requested.store(false, Ordering::Release);
                });
        }
        #[cfg(target_os = "linux")]
        {
            self.capture_linux(generation)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = generation;
            self.shared.requested.store(false, Ordering::Release);
            Err("La captura nativa sólo está disponible en Linux y macOS".into())
        }
    }

    #[cfg(target_os = "linux")]
    fn capture_linux(&self, generation: u64) -> Result<(), String> {
        let shared = self.shared.clone();
        let force_x11 = std::env::var("KVM_INPUT_BACKEND").as_deref() == Ok("x11");
        let wayland = !force_x11
            && std::env::var_os("WAYLAND_DISPLAY").is_some()
            && std::env::var("XDG_SESSION_TYPE").unwrap_or_default() != "x11";
        self.shared.report(
            false,
            if wayland {
                "Wayland · libei"
            } else {
                "XInput2"
            },
            "Preparando captura nativa…",
        );
        thread::Builder::new()
            .name("kvm-native-capture".into())
            .spawn(move || {
                let backend = if wayland {
                    "Wayland · libei"
                } else {
                    "XInput2"
                };
                let result = if wayland {
                    tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|e| e.to_string())
                        .and_then(|runtime| {
                            runtime.block_on(portal::run(shared.clone(), generation))
                        })
                } else {
                    x11::run(shared.clone(), generation)
                };
                if shared.generation.load(Ordering::Acquire) == generation {
                    shared.requested.store(false, Ordering::Release);
                    shared.release();
                    shared.report(
                        false,
                        backend,
                        match result {
                            Ok(()) => "Captura liberada; control local".into(),
                            Err(error) => format!("Captura no disponible: {error}"),
                        },
                    );
                }
            })
            .map_err(|e| {
                self.shared.requested.store(false, Ordering::Release);
                e.to_string()
            })?;
        Ok(())
    }

    /// Call before closing the RPC transport. Failed sends remain queued for retry.
    pub fn release_all(&self) {
        let Ok(mut scheduler) = self.shared.scheduler.lock() else {
            return;
        };
        scheduler.release();
        self.send_pending_releases(&mut scheduler);
    }

    fn send_pending_releases(&self, scheduler: &mut Scheduler) {
        for _ in 0..2 {
            let Some(report) = scheduler.peek() else {
                break;
            };
            if (self.shared.send)(report).is_err() {
                break;
            }
            scheduler.acknowledge();
        }
    }
}

impl Drop for InputEngine {
    fn drop(&mut self) {
        if Arc::strong_count(&self.owners) != 1 {
            return;
        }
        self.shared.requested.store(false, Ordering::Release);
        let generation = self.shared.generation.fetch_add(1, Ordering::AcqRel) + 1;
        self.shared.release();
        #[cfg(target_os = "macos")]
        let _ = self.shared.dispatch_native(false, generation);
        #[cfg(not(target_os = "macos"))]
        let _ = generation;
    }
}

#[cfg(test)]
mod enabled_tests {
    use super::*;
    use serde_json::json;

    fn isolated_engine(reports: Arc<Mutex<Vec<Value>>>) -> InputEngine {
        InputEngine {
            shared: Arc::new(Shared {
                scheduler: Mutex::new(Scheduler::default()),
                send: Arc::new(move |report| {
                    reports.lock().unwrap().push(report);
                    Ok(())
                }),
                callback: Arc::new(|_| {}),
                generation: AtomicU64::new(0),
                enabled: AtomicBool::new(true),
                requested: AtomicBool::new(false),
                active: AtomicBool::new(false),
                stats: Mutex::new(InputStats::default()),
                #[cfg(target_os = "macos")]
                native_dispatch: Mutex::new(None),
            }),
            owners: Arc::new(()),
        }
    }

    fn assert_release_reports(reports: &[Value]) {
        assert_eq!(reports.len(), 2, "one mouse and one keyboard release");
        assert_eq!(reports[0]["method"], "relMouseReport");
        assert_eq!(
            reports[0]["params"],
            json!({"dx": 0, "dy": 0, "buttons": 0})
        );
        assert_eq!(reports[1]["method"], "keyboardReport");
        assert_eq!(reports[1]["params"], json!({"keys": [], "modifier": 0}));
    }

    #[test]
    fn disabling_releases_held_input_and_discards_all_late_native_events() {
        let reports = Arc::new(Mutex::new(Vec::new()));
        let engine = isolated_engine(reports.clone());
        engine.shared.requested.store(true, Ordering::Release);
        engine.shared.active.store(true, Ordering::Release);
        let generation = engine.shared.generation.load(Ordering::Acquire);
        engine.shared.key(4, true).unwrap();
        engine.shared.key(225, true).unwrap();
        engine.shared.button(1, true).unwrap();
        {
            let mut scheduler = engine.shared.scheduler.lock().unwrap();
            while let Some(report) = scheduler.peek() {
                (engine.shared.send)(report).unwrap();
                scheduler.acknowledge();
            }
        }
        reports.lock().unwrap().clear();
        engine.shared.motion(500.75, -90.25).unwrap();
        engine.shared.wheel(3.5).unwrap();
        engine.shared.key(5, true).unwrap();

        engine.set_enabled(false).unwrap();
        assert!(!engine.is_enabled());
        assert!(!engine.is_requested());
        assert!(!engine.is_captured());
        assert!(!engine.shared.running(generation));
        assert_release_reports(&reports.lock().unwrap());

        engine.shared.motion(100.0, 20.0).unwrap();
        engine.shared.button(1, true).unwrap();
        engine.shared.wheel(4.0).unwrap();
        assert!(!engine.shared.key(6, true).unwrap());
        assert_eq!(engine.shared.scheduler.lock().unwrap().len(), 0);
        assert_release_reports(&reports.lock().unwrap());
    }

    #[test]
    fn disabled_capture_is_rejected_and_enabling_requires_a_new_capture() {
        let reports = Arc::new(Mutex::new(Vec::new()));
        let engine = isolated_engine(reports);
        engine.shared.motion(0.75, 0.75).unwrap();
        engine.shared.wheel(0.75).unwrap();
        engine.set_enabled(false).unwrap();
        assert_eq!(engine.capture(true).unwrap_err(), INPUT_DISABLED_MESSAGE);
        engine.shared.motion(100.0, 100.0).unwrap();
        engine.set_enabled(true).unwrap();
        assert!(engine.is_enabled());
        assert!(!engine.is_requested());
        assert!(!engine.is_captured());
        assert_eq!(engine.shared.scheduler.lock().unwrap().len(), 0);
        engine.shared.motion(0.5, 0.5).unwrap();
        engine.shared.wheel(0.5).unwrap();
        assert_eq!(engine.shared.scheduler.lock().unwrap().len(), 0);
        engine.shared.motion(0.5, 0.5).unwrap();
        let scheduler = engine.shared.scheduler.lock().unwrap();
        let report = scheduler.peek().unwrap();
        assert_eq!(report["params"], json!({"dx": 1, "dy": 1, "buttons": 0}));
    }

    #[test]
    fn stale_native_activation_is_ignored_while_disabled_and_after_reenabling() {
        let mut engine = isolated_engine(Arc::new(Mutex::new(Vec::new())));
        let statuses = Arc::new(Mutex::new(Vec::new()));
        let capture_statuses = statuses.clone();
        Arc::get_mut(&mut engine.shared).unwrap().callback = Arc::new(move |status| {
            capture_statuses.lock().unwrap().push(status);
        });
        engine.shared.requested.store(true, Ordering::Release);
        engine.set_enabled(false).unwrap();
        engine.shared.report(true, "stale capture", "captured");
        engine.set_enabled(true).unwrap();
        engine.shared.report(true, "stale capture", "captured");
        assert!(!engine.is_captured());
        assert!(!engine.is_requested());
        let statuses = statuses.lock().unwrap();
        assert_eq!(statuses.len(), 1);
        assert!(!statuses[0].active);
        assert!(!statuses[0].requested);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn disabling_cancels_native_capture_and_invalidates_its_generation() {
        let engine = isolated_engine(Arc::new(Mutex::new(Vec::new())));
        let stops = Arc::new(AtomicU64::new(0));
        let native_stops = stops.clone();
        let weak = Arc::downgrade(&engine.shared);
        *engine.shared.native_dispatch.lock().unwrap() =
            Some(Arc::new(move |enabled, generation| {
                let shared = weak.upgrade().ok_or("input engine dropped")?;
                if enabled && shared.running(generation) {
                    shared.report(true, "simulated AppKit", "captured");
                } else if !enabled {
                    native_stops.fetch_add(1, Ordering::Relaxed);
                }
                Ok(())
            }));
        engine.capture(true).unwrap();
        let generation = engine.shared.generation.load(Ordering::Acquire);
        assert!(engine.is_captured());
        engine.set_enabled(false).unwrap();
        assert_eq!(stops.load(Ordering::Relaxed), 1);
        assert!(!engine.shared.running(generation));
        assert!(!engine.is_captured());
        engine.set_enabled(true).unwrap();
        assert!(!engine.is_requested());
        assert!(!engine.is_captured());
    }

    #[test]
    fn disabled_release_retries_only_zero_hid_reports_when_transport_recovers() {
        let available = Arc::new(AtomicBool::new(false));
        let reports = Arc::new(Mutex::new(Vec::new()));
        let send_available = available.clone();
        let send_reports = reports.clone();
        let engine = InputEngine::new(
            Arc::new(move |report| {
                if !send_available.load(Ordering::Acquire) {
                    return Err("temporarily unavailable".into());
                }
                send_reports.lock().unwrap().push(report);
                Ok(())
            }),
            Arc::new(|_| {}),
        );
        engine.shared.key(225, true).unwrap();
        engine.shared.button(1, true).unwrap();
        engine.shared.motion(1000.0, -1000.0).unwrap();
        engine.set_enabled(false).unwrap();
        assert_eq!(engine.shared.scheduler.lock().unwrap().len(), 2);
        engine.shared.motion(50.0, 50.0).unwrap();
        engine.shared.wheel(2.0).unwrap();
        engine.shared.key(4, true).unwrap();
        available.store(true, Ordering::Release);
        let deadline = Instant::now() + Duration::from_secs(2);
        while engine.shared.scheduler.lock().unwrap().len() > 0 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(engine.shared.scheduler.lock().unwrap().len(), 0);
        assert_release_reports(&reports.lock().unwrap());
        assert!(!engine.is_enabled());
        assert!(!engine.is_captured());
    }
}
