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
    requested: AtomicBool,
    active: AtomicBool,
    stats: Mutex<InputStats>,
    #[cfg(target_os = "macos")]
    native_dispatch: Mutex<Option<NativeDispatch>>,
}

impl Shared {
    pub(super) fn running(&self, generation: u64) -> bool {
        self.requested.load(Ordering::Acquire)
            && self.generation.load(Ordering::Acquire) == generation
    }

    pub(super) fn report(&self, active: bool, backend: &str, message: impl Into<String>) {
        self.active.store(active, Ordering::Release);
        (self.callback)(CaptureStatus {
            active,
            requested: self.requested.load(Ordering::Acquire),
            backend: backend.into(),
            message: message.into(),
        });
    }

    pub(super) fn motion(&self, dx: f64, dy: f64) -> Result<(), String> {
        self.scheduler
            .lock()
            .map_err(|e| e.to_string())?
            .motion(dx, dy)
    }

    pub(super) fn button(&self, bit: u8, pressed: bool) -> Result<(), String> {
        self.scheduler
            .lock()
            .map_err(|e| e.to_string())?
            .button(bit, pressed)
    }

    pub(super) fn wheel(&self, steps: f64) -> Result<(), String> {
        self.scheduler
            .lock()
            .map_err(|e| e.to_string())?
            .wheel(steps)
    }

    pub(super) fn key(&self, usage: u8, pressed: bool) -> Result<bool, String> {
        self.scheduler
            .lock()
            .map_err(|e| e.to_string())?
            .key(usage, pressed)
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
                let mut next = Instant::now();
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
                    if let Some(report) = scheduler.peek() {
                        match (shared.send)(report) {
                            Ok(()) => {
                                scheduler.acknowledge();
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
        self.shared.active.load(Ordering::Acquire)
    }
    pub fn is_requested(&self) -> bool {
        self.shared.requested.load(Ordering::Acquire)
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
        if self.shared.requested.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let generation = self.shared.generation.fetch_add(1, Ordering::AcqRel) + 1;
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
