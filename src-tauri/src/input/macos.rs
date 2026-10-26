//! App-local input capture. AppKit objects remain on the main thread and the
//! existing Rust scheduler owns all HID ordering and transport pacing.
use super::{InputEngine, NativeDispatch, Shared};
use block2::RcBlock;
use objc2::{rc::Retained, runtime::AnyObject, MainThreadMarker};
use objc2_app_kit::{
    NSCursor, NSEvent, NSEventMask, NSEventModifierFlags, NSEventType, NSWindow,
    NSWindowDidResignKeyNotification,
};
use objc2_core_graphics::{
    CGAssociateMouseAndMouseCursorPosition, CGError, CGEventSource, CGEventSourceStateID,
};
use objc2_foundation::{NSNotification, NSNotificationCenter};
use std::{
    cell::RefCell,
    collections::{BTreeSet, HashMap},
    ptr::{self, NonNull},
    rc::Rc,
    sync::{atomic::Ordering, Arc, Mutex, Weak},
    time::{Duration, Instant},
};

thread_local! {
    static CAPTURES: RefCell<HashMap<usize, Capture>> = RefCell::new(HashMap::new());
    static AUDITS: RefCell<HashMap<usize, Arc<Mutex<Option<Restoration>>>>> = RefCell::new(HashMap::new());
}

pub(super) fn initialize(shared: &Arc<Shared>, window: tauri::WebviewWindow) -> Result<(), String> {
    MainThreadMarker::new().ok_or("La captura AppKit debe inicializarse en el hilo principal")?;
    window.ns_window().map_err(|e| e.to_string())?;
    let weak = Arc::downgrade(shared);
    let dispatch: NativeDispatch = Arc::new(move |enabled, generation| {
        let callback_window = window.clone();
        let weak = weak.clone();
        if MainThreadMarker::new().is_some() {
            set_capture(weak, callback_window, enabled, generation);
            return Ok(());
        }
        window
            .run_on_main_thread(move || {
                set_capture(weak, callback_window, enabled, generation);
            })
            .map_err(|e| e.to_string())
    });
    *shared.native_dispatch.lock().map_err(|e| e.to_string())? = Some(dispatch);
    Ok(())
}

fn set_capture(weak: Weak<Shared>, window: tauri::WebviewWindow, enabled: bool, generation: u64) {
    let id = weak.as_ptr() as usize;
    let Some(shared) = weak.upgrade() else {
        remove_capture(id);
        return;
    };
    // A cancellation/new request may overtake an installation dispatched from
    // another thread. Never let an obsolete callback replace a newer capture.
    if shared.generation.load(Ordering::Acquire) != generation {
        return;
    }
    remove_capture(id);
    if !enabled || !shared.running(generation) {
        return;
    }
    let result = (|| {
        let mtm = MainThreadMarker::new().ok_or("AppKit requiere el hilo principal")?;
        let pointer = window.ns_window().map_err(|e| e.to_string())?;
        // SAFETY: Tauri exposes its live NSWindow; retain it for the lifetime of
        // this main-thread capture, without taking ownership of Tauri's reference.
        let window = unsafe { Retained::retain(pointer.cast::<NSWindow>()) }
            .ok_or("La ventana nativa ya no está disponible")?;
        let capture = Capture::new(window, &shared, generation, mtm)?;
        CAPTURES.with(|captures| captures.borrow_mut().insert(id, capture));
        Ok::<_, String>(())
    })();
    match result {
        Ok(()) if shared.running(generation) => shared.report(
            true,
            "macOS · AppKit",
            "Control remoto activo · Ctrl+Option+Esc para liberar",
        ),
        Ok(()) => remove_capture(id),
        Err(error) => shared.stop_native(format!("Captura no disponible: {error}")),
    }
}

fn remove_capture(id: usize) {
    // Drop outside the RefCell borrow: removing an AppKit monitor may release its
    // callback and must not leave thread-local state borrowed during teardown.
    let previous = CAPTURES.with(|captures| captures.borrow_mut().remove(&id));
    drop(previous);
}

struct Capture {
    window: Retained<NSWindow>,
    monitor: Option<Retained<AnyObject>>,
    focus_observer: Option<Retained<AnyObject>>,
    old_mouse_moved: bool,
    old_coalescing: bool,
    disconnected_cursor: bool,
    hidden_cursor: bool,
    audit: Option<Arc<Mutex<Option<Restoration>>>>,
}

impl Capture {
    fn new(
        window: Retained<NSWindow>,
        shared: &Arc<Shared>,
        generation: u64,
        _mtm: MainThreadMarker,
    ) -> Result<Self, String> {
        if !window.isKeyWindow() {
            return Err("Enfoca la ventana del KVM antes de capturar".into());
        }
        let mut capture = Self {
            old_mouse_moved: window.acceptsMouseMovedEvents(),
            old_coalescing: NSEvent::isMouseCoalescingEnabled(),
            window: window.clone(),
            monitor: None,
            focus_observer: None,
            disconnected_cursor: false,
            hidden_cursor: false,
            audit: AUDITS.with(|audits| {
                audits
                    .borrow()
                    .get(&(Arc::as_ptr(shared) as usize))
                    .cloned()
            }),
        };
        window.setAcceptsMouseMovedEvents(true);
        NSEvent::setMouseCoalescingEnabled(false);
        let result = CGAssociateMouseAndMouseCursorPosition(false);
        if result != CGError::Success {
            return Err(format!("No se pudo capturar el cursor ({})", result.0));
        }
        capture.disconnected_cursor = true;
        NSCursor::hide();
        capture.hidden_cursor = true;
        let modifiers = Rc::new(RefCell::new(Modifiers::current()));
        for usage in &modifiers.borrow().pressed {
            shared.key(*usage, true)?;
        }
        let weak = Arc::downgrade(shared);
        let handler = RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
            // SAFETY: AppKit keeps the NSEvent alive throughout this callback.
            let native_event = unsafe { event.as_ref() };
            let Some(shared) = weak.upgrade() else {
                return event.as_ptr();
            };
            if !shared.running(generation) {
                return event.as_ptr();
            }
            if !window.isKeyWindow() {
                shared.stop_native("Control local; la ventana perdió el foco");
                return event.as_ptr();
            }
            if native_event.windowNumber() != 0
                && native_event.windowNumber() != window.windowNumber()
            {
                return event.as_ptr();
            }
            if native_event.r#type() == NSEventType::KeyDown
                && emergency_shortcut(
                    native_event.r#type(),
                    native_event.keyCode(),
                    native_event.modifierFlags(),
                )
            {
                shared.stop_native("Control local; captura liberada con Ctrl+Option+Esc");
                return ptr::null_mut();
            }
            let result = handle_event(&shared, native_event, &modifiers);
            match result {
                Ok(true) => ptr::null_mut(),
                Ok(false) => event.as_ptr(),
                Err(error) => {
                    shared.stop_native(format!("Captura liberada: {error}"));
                    ptr::null_mut()
                }
            }
        });
        let mask = NSEventMask::MouseMoved
            | NSEventMask::LeftMouseDragged
            | NSEventMask::RightMouseDragged
            | NSEventMask::OtherMouseDragged
            | NSEventMask::LeftMouseDown
            | NSEventMask::LeftMouseUp
            | NSEventMask::RightMouseDown
            | NSEventMask::RightMouseUp
            | NSEventMask::OtherMouseDown
            | NSEventMask::OtherMouseUp
            | NSEventMask::ScrollWheel
            | NSEventMask::KeyDown
            | NSEventMask::KeyUp
            | NSEventMask::FlagsChanged;
        // SAFETY: the callback returns either the incoming, live event or null.
        capture.monitor =
            unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(mask, &handler) };
        if capture.monitor.is_none() {
            return Err("AppKit no pudo instalar el monitor local de entrada".into());
        }
        let weak = Arc::downgrade(shared);
        let focus_changed = RcBlock::new(move |_notification: NonNull<NSNotification>| {
            if let Some(shared) = weak.upgrade().filter(|shared| shared.running(generation)) {
                shared.stop_native("Control local; la ventana perdió el foco");
            }
        });
        // SAFETY: this named AppKit notification is process-lifetime constant;
        // observing our own NSWindow neither monitors nor focuses other apps.
        capture.focus_observer = Some(unsafe {
            NSNotificationCenter::defaultCenter()
                .addObserverForName_object_queue_usingBlock(
                    Some(NSWindowDidResignKeyNotification),
                    Some(&capture.window),
                    None,
                    &focus_changed,
                )
                .into()
        });
        Ok(capture)
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        if let Some(observer) = self.focus_observer.take() {
            // SAFETY: this is the token returned by the notification center.
            unsafe {
                NSNotificationCenter::defaultCenter().removeObserver(&observer);
            }
        }
        if let Some(monitor) = self.monitor.take() {
            // SAFETY: this token was returned by addLocalMonitor above and is
            // removed exactly once, on the AppKit main thread.
            unsafe {
                NSEvent::removeMonitor(&monitor);
            }
        }
        let cursor_restored = !self.disconnected_cursor
            || CGAssociateMouseAndMouseCursorPosition(true) == CGError::Success;
        if self.hidden_cursor {
            NSCursor::unhide();
        }
        self.window.setAcceptsMouseMovedEvents(self.old_mouse_moved);
        NSEvent::setMouseCoalescingEnabled(self.old_coalescing);
        if let Some(audit) = &self.audit {
            if let Ok(mut audit) = audit.lock() {
                *audit = Some(Restoration {
                    cursor_reassociated: cursor_restored,
                    cursor_unhidden: self.hidden_cursor,
                });
            }
        }
    }
}

fn emergency_shortcut(kind: NSEventType, code: u16, flags: NSEventModifierFlags) -> bool {
    kind == NSEventType::KeyDown
        && code == 0x35
        && flags.contains(NSEventModifierFlags::Control | NSEventModifierFlags::Option)
}

fn handle_event(
    shared: &Shared,
    event: &NSEvent,
    modifiers: &RefCell<Modifiers>,
) -> Result<bool, String> {
    match event.r#type() {
        NSEventType::MouseMoved
        | NSEventType::LeftMouseDragged
        | NSEventType::RightMouseDragged
        | NSEventType::OtherMouseDragged => {
            shared.motion(event.deltaX(), event.deltaY())?;
        }
        NSEventType::LeftMouseDown
        | NSEventType::RightMouseDown
        | NSEventType::OtherMouseDown
        | NSEventType::LeftMouseUp
        | NSEventType::RightMouseUp
        | NSEventType::OtherMouseUp => {
            if let Some(bit) = button_bit(event.buttonNumber()) {
                let pressed = matches!(
                    event.r#type(),
                    NSEventType::LeftMouseDown
                        | NSEventType::RightMouseDown
                        | NSEventType::OtherMouseDown
                );
                shared.button(bit, pressed)?;
            }
        }
        NSEventType::ScrollWheel => {
            shared.wheel(wheel_steps(
                event.scrollingDeltaY(),
                event.hasPreciseScrollingDeltas(),
            ))?;
        }
        NSEventType::KeyDown | NSEventType::KeyUp => {
            if !event.isARepeat() {
                if let Some(usage) = macos_to_hid(event.keyCode()) {
                    shared.key(usage, event.r#type() == NSEventType::KeyDown)?;
                }
            }
        }
        NSEventType::FlagsChanged => {
            for (usage, pressed) in modifiers
                .borrow_mut()
                .changed(event.keyCode(), event.modifierFlags())
            {
                shared.key(usage, pressed)?;
            }
        }
        _ => return Ok(false),
    }
    Ok(true)
}

fn button_bit(button: isize) -> Option<u8> {
    match button {
        0 => Some(1),
        1 => Some(2),
        2 => Some(4),
        3 => Some(8),
        4 => Some(16),
        _ => None,
    }
}

fn wheel_steps(delta: f64, precise: bool) -> f64 {
    // AppKit supplies wheel notches or trackpad points. Match the continuous
    // scroll scale used by the libei adapter and preserve fractional notches.
    if precise {
        delta / 15.0
    } else {
        delta
    }
}

#[derive(Default)]
struct Modifiers {
    pressed: BTreeSet<u8>,
    caps_lock: bool,
}

impl Modifiers {
    fn current() -> Self {
        let pressed = [0x3b, 0x38, 0x3a, 0x37, 0x3e, 0x3c, 0x3d, 0x36]
            .into_iter()
            .filter(|code| {
                CGEventSource::key_state(CGEventSourceStateID::CombinedSessionState, *code)
            })
            .filter_map(macos_to_hid)
            .collect();
        Self {
            pressed,
            caps_lock: NSEvent::modifierFlags_class().contains(NSEventModifierFlags::CapsLock),
        }
    }

    fn changed(&mut self, code: u16, flags: NSEventModifierFlags) -> Vec<(u8, bool)> {
        if code == 0x39 {
            let caps_lock = flags.contains(NSEventModifierFlags::CapsLock);
            if caps_lock == self.caps_lock {
                return vec![];
            }
            self.caps_lock = caps_lock;
            // Caps Lock is a latched AppKit flag, not a held USB key.
            return vec![(57, true), (57, false)];
        }
        let Some(usage @ 224..=231) = macos_to_hid(code) else {
            return vec![];
        };
        let flag = match usage {
            224 | 228 => NSEventModifierFlags::Control,
            225 | 229 => NSEventModifierFlags::Shift,
            226 | 230 => NSEventModifierFlags::Option,
            _ => NSEventModifierFlags::Command,
        };
        // Each flagsChanged identifies the physical side. The aggregate flag
        // remains set when one side is released while the other is still held.
        let pressed = flags.contains(flag) && !self.pressed.contains(&usage);
        if pressed {
            self.pressed.insert(usage);
        } else {
            self.pressed.remove(&usage);
        }
        vec![(usage, pressed)]
    }
}

/// Carbon virtual key positions to USB HID usages. The remote OS chooses its
/// layout; characters/Unicode and the local input method never enter this map.
fn macos_to_hid(code: u16) -> Option<u8> {
    Some(match code {
        0x00 => 4,
        0x01 => 22,
        0x02 => 7,
        0x03 => 9,
        0x04 => 11,
        0x05 => 10,
        0x06 => 29,
        0x07 => 27,
        0x08 => 6,
        0x09 => 25,
        0x0a => 100,
        0x0b => 5,
        0x0c => 20,
        0x0d => 26,
        0x0e => 8,
        0x0f => 21,
        0x10 => 28,
        0x11 => 23,
        0x12 => 30,
        0x13 => 31,
        0x14 => 32,
        0x15 => 33,
        0x16 => 35,
        0x17 => 34,
        0x18 => 46,
        0x19 => 38,
        0x1a => 36,
        0x1b => 45,
        0x1c => 37,
        0x1d => 39,
        0x1e => 48,
        0x1f => 18,
        0x20 => 24,
        0x21 => 47,
        0x22 => 12,
        0x23 => 19,
        0x24 => 40,
        0x25 => 15,
        0x26 => 13,
        0x27 => 52,
        0x28 => 14,
        0x29 => 51,
        0x2a => 49,
        0x2b => 54,
        0x2c => 56,
        0x2d => 17,
        0x2e => 16,
        0x2f => 55,
        0x30 => 43,
        0x31 => 44,
        0x32 => 53,
        0x33 => 42,
        0x35 => 41,
        0x36 => 231,
        0x37 => 227,
        0x38 => 225,
        0x39 => 57,
        0x3a => 226,
        0x3b => 224,
        0x3c => 229,
        0x3d => 230,
        0x3e => 228,
        0x40 => 108,
        0x41 => 99,
        0x43 => 85,
        0x45 => 87,
        0x47 => 83,
        0x4b => 84,
        0x4c => 88,
        0x4e => 86,
        0x4f => 109,
        0x50 => 110,
        0x51 => 103,
        0x52 => 98,
        0x53 => 89,
        0x54 => 90,
        0x55 => 91,
        0x56 => 92,
        0x57 => 93,
        0x58 => 94,
        0x59 => 95,
        0x5a => 111,
        0x5b => 96,
        0x5c => 97,
        0x5d => 137,
        0x5e => 135,
        0x5f => 133,
        0x60 => 62,
        0x61 => 63,
        0x62 => 64,
        0x63 => 60,
        0x64 => 65,
        0x65 => 66,
        0x66 => 145,
        0x67 => 68,
        0x68 => 144,
        0x69 => 104,
        0x6a => 107,
        0x6b => 105,
        0x6d => 67,
        0x6f => 69,
        0x71 => 106,
        0x72 => 73,
        0x73 => 74,
        0x74 => 75,
        0x75 => 76,
        0x76 => 61,
        0x77 => 77,
        0x78 => 59,
        0x79 => 78,
        0x7a => 58,
        0x7b => 80,
        0x7c => 79,
        0x7d => 81,
        0x7e => 82,
        _ => return None,
    })
}

#[derive(Clone, Copy)]
struct Restoration {
    cursor_reassociated: bool,
    cursor_unhidden: bool,
}

#[derive(Clone, Copy)]
struct NativeSnapshot {
    mouse_moved: bool,
    coalescing: bool,
    focused: bool,
    installed: bool,
    hidden_cursor: bool,
    disconnected_cursor: bool,
}

fn native_snapshot(window: &tauri::WebviewWindow, id: usize) -> Result<NativeSnapshot, String> {
    MainThreadMarker::new().ok_or("El diagnóstico AppKit requiere el hilo principal")?;
    let pointer = window.ns_window().map_err(|e| e.to_string())?;
    // SAFETY: retain the live NSWindow exposed by Tauri only on its main thread.
    let window = unsafe { Retained::retain(pointer.cast::<NSWindow>()) }
        .ok_or("La ventana del diagnóstico ya no está disponible")?;
    let (installed, hidden_cursor, disconnected_cursor) = CAPTURES.with(|captures| {
        let captures = captures.borrow();
        match captures.get(&id) {
            Some(capture) => (
                capture.monitor.is_some(),
                capture.hidden_cursor,
                capture.disconnected_cursor,
            ),
            None => (false, false, false),
        }
    });
    Ok(NativeSnapshot {
        mouse_moved: window.acceptsMouseMovedEvents(),
        coalescing: NSEvent::isMouseCoalescingEnabled(),
        focused: window.isKeyWindow(),
        installed,
        hidden_cursor,
        disconnected_cursor,
    })
}

async fn on_main<T: Send + 'static>(
    window: &tauri::WebviewWindow,
    callback: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let (send, receive) = tokio::sync::oneshot::channel();
    window
        .run_on_main_thread(move || {
            let _ = send.send(callback());
        })
        .map_err(|e| e.to_string())?;
    tokio::time::timeout(Duration::from_secs(3), receive)
        .await
        .map_err(|_| "El hilo AppKit no respondió al diagnóstico en 3 segundos".to_string())?
        .map_err(|_| "El hilo AppKit cerró la respuesta del diagnóstico".to_string())?
}

struct SelfTestCleanup {
    engine: InputEngine,
    window: tauri::WebviewWindow,
    id: usize,
}

impl Drop for SelfTestCleanup {
    fn drop(&mut self) {
        // This also runs if the diagnostic future is cancelled. The engine's
        // generation makes any still-queued installation harmless.
        let _ = self.engine.capture(false);
        let id = self.id;
        let _ = self.window.run_on_main_thread(move || {
            remove_capture(id);
            AUDITS.with(|audits| audits.borrow_mut().remove(&id));
        });
    }
}

async fn wait_for_capture(engine: &InputEngine, active: bool) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if engine.is_captured() == active && (active || !engine.is_requested()) {
            return Ok(());
        }
        if Instant::now() >= deadline || (active && !engine.is_requested()) {
            return Err(format!(
                "La captura AppKit no alcanzó el estado esperado: active={active}"
            ));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn verify_restored(
    engine: &InputEngine,
    window: &tauri::WebviewWindow,
    id: usize,
    baseline: NativeSnapshot,
    audit: &Arc<Mutex<Option<Restoration>>>,
) -> Result<(), String> {
    let snapshot_window = window.clone();
    let restored = on_main(window, move || native_snapshot(&snapshot_window, id)).await?;
    if engine.is_captured()
        || engine.is_requested()
        || restored.installed
        || restored.hidden_cursor
        || restored.disconnected_cursor
        || restored.mouse_moved != baseline.mouse_moved
        || restored.coalescing != baseline.coalescing
    {
        return Err("La captura AppKit no restauró el estado nativo de la ventana".into());
    }
    let restoration = audit
        .lock()
        .map_err(|e| e.to_string())?
        .ok_or("No se recibió la confirmación de limpieza AppKit")?;
    if !restoration.cursor_reassociated || !restoration.cursor_unhidden {
        return Err("AppKit no confirmó la reasociación y liberación del cursor".into());
    }
    Ok(())
}

pub(super) async fn self_test(window: tauri::WebviewWindow) -> Result<(), String> {
    let reports = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let output = reports.clone();
    let engine = InputEngine::new(
        Arc::new(move |report| {
            output.lock().map_err(|e| e.to_string())?.push(report);
            Ok(())
        }),
        Arc::new(|status| eprintln!("input-self-test: {}", status.message)),
    );
    let id = Arc::as_ptr(&engine.shared) as usize;
    let audit = Arc::new(Mutex::new(None));
    let _cleanup = SelfTestCleanup {
        engine: engine.clone(),
        window: window.clone(),
        id,
    };
    window.show().map_err(|e| e.to_string())?;
    window.set_focus().map_err(|e| e.to_string())?;
    let setup_engine = engine.clone();
    let setup_window = window.clone();
    let setup_audit = audit.clone();
    let baseline = on_main(&window, move || {
        setup_engine.initialize_macos(setup_window.clone())?;
        AUDITS.with(|audits| audits.borrow_mut().insert(id, setup_audit));
        native_snapshot(&setup_window, id)
    })
    .await?;
    let result = async {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let focus_window = window.clone();
            if on_main(&window, move || native_snapshot(&focus_window, id))
                .await?
                .focused
            {
                break;
            }
            if Instant::now() >= deadline {
                return Err("La ventana del diagnóstico no obtuvo el foco".into());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        engine.capture(true)?;
        wait_for_capture(&engine, true).await?;
        let active_window = window.clone();
        let active = on_main(&window, move || native_snapshot(&active_window, id)).await?;
        if !active.installed
            || !active.hidden_cursor
            || !active.disconnected_cursor
            || !active.mouse_moved
            || active.coalescing
        {
            return Err("La captura AppKit no instaló todos sus recursos nativos".into());
        }
        engine.shared.motion(300.0, -120.0)?;
        engine.shared.button(1, true)?;
        engine.shared.wheel(1.0)?;
        engine.shared.key(225, true)?;
        engine.shared.key(4, true)?;
        engine.shared.key(4, false)?;
        engine.shared.key(225, false)?;
        engine.shared.button(1, false)?;
        let deadline = Instant::now() + Duration::from_secs(3);
        while engine
            .shared
            .scheduler
            .lock()
            .map_err(|e| e.to_string())?
            .len()
            > 0
        {
            if Instant::now() >= deadline {
                return Err("El scheduler no vació los reportes sintéticos".into());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        {
            let reports = reports.lock().map_err(|e| e.to_string())?;
            let motion = reports.iter().any(|report| {
                report["params"]["dx"].as_i64().is_some_and(|dx| dx > 0)
                    && report["params"]["dy"].as_i64().is_some_and(|dy| dy < 0)
            });
            let button = reports
                .iter()
                .any(|report| report["params"]["buttons"] == 1);
            let wheel = reports
                .iter()
                .any(|report| report["method"] == "wheelReport" && report["params"]["wheelY"] == 1);
            let key = reports.iter().any(|report| {
                report["method"] == "keyboardReport"
                    && report["params"]["keys"] == serde_json::json!([4])
                    && report["params"]["modifier"]
                        .as_u64()
                        .is_some_and(|modifier| modifier & 2 != 0)
            });
            if !motion || !button || !wheel || !key {
                return Err("Faltan reportes sintéticos de mouse, rueda o teclado".into());
            }
        }
        engine.capture(false)?;
        verify_restored(&engine, &window, id, baseline, &audit).await?;
        engine.capture(true)?;
        wait_for_capture(&engine, true).await?;
        // Hiding our own window produces a real native focus-loss notification;
        // no other app is activated and no KVM transport exists in this mode.
        window.hide().map_err(|e| e.to_string())?;
        wait_for_capture(&engine, false).await?;
        verify_restored(&engine, &window, id, baseline, &audit).await?;
        window.show().map_err(|e| e.to_string())?;
        // Exercise cancellation before the queued main-thread installation runs.
        engine.capture(true)?;
        engine.capture(false)?;
        verify_restored(&engine, &window, id, baseline, &audit).await?;
        Ok::<_, String>(())
    }
    .await;
    let cancel = engine.capture(false);
    let cleanup_window = window.clone();
    let restored = on_main(&window, move || {
        remove_capture(id);
        let snapshot = native_snapshot(&cleanup_window, id);
        AUDITS.with(|audits| audits.borrow_mut().remove(&id));
        snapshot
    })
    .await;
    let _ = window.show();
    result?;
    cancel?;
    let restored = restored?;
    if restored.installed
        || restored.mouse_moved != baseline.mouse_moved
        || restored.coalescing != baseline.coalescing
    {
        return Err("La limpieza final del diagnóstico no restauró la ventana".into());
    }
    eprintln!("input-self-test: captura, HID simulado, cancelación y pérdida de foco aprobados; cero conexiones KVM");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn physical_key_map_covers_letters_navigation_modifiers_and_layout_keys() {
        for (code, usage) in [
            (0, 4),
            (0x35, 41),
            (0x24, 40),
            (0x7b, 80),
            (0x7e, 82),
            (0xa, 100),
            (0x5d, 137),
            (0x68, 144),
        ] {
            assert_eq!(macos_to_hid(code), Some(usage));
        }
        for (code, usage) in [
            (0x3b, 224),
            (0x38, 225),
            (0x3a, 226),
            (0x37, 227),
            (0x3e, 228),
            (0x3c, 229),
            (0x3d, 230),
            (0x36, 231),
        ] {
            assert_eq!(macos_to_hid(code), Some(usage));
        }
        assert_eq!(macos_to_hid(0x3f), None); // Fn has no boot-keyboard usage.
        assert_eq!(macos_to_hid(u16::MAX), None);
    }

    #[test]
    fn flags_changed_preserves_modifier_sides_and_pulses_caps_lock() {
        let mut state = Modifiers::default();
        assert_eq!(
            state.changed(0x38, NSEventModifierFlags::Shift),
            vec![(225, true)]
        );
        assert_eq!(
            state.changed(0x3c, NSEventModifierFlags::Shift),
            vec![(229, true)]
        );
        assert_eq!(
            state.changed(0x38, NSEventModifierFlags::Shift),
            vec![(225, false)]
        );
        assert!(state.pressed.contains(&229));
        assert_eq!(
            state.changed(0x3c, NSEventModifierFlags::empty()),
            vec![(229, false)]
        );
        assert_eq!(
            state.changed(0x39, NSEventModifierFlags::CapsLock),
            vec![(57, true), (57, false)]
        );
        assert!(state
            .changed(0x39, NSEventModifierFlags::CapsLock)
            .is_empty());
        assert_eq!(
            state.changed(0x39, NSEventModifierFlags::empty()),
            vec![(57, true), (57, false)]
        );
    }

    #[test]
    fn shortcut_and_mouse_adapters_keep_local_release_and_fractional_wheel() {
        let flags = NSEventModifierFlags::Control | NSEventModifierFlags::Option;
        assert!(emergency_shortcut(NSEventType::KeyDown, 0x35, flags));
        assert!(!emergency_shortcut(NSEventType::KeyUp, 0x35, flags));
        assert!(!emergency_shortcut(
            NSEventType::KeyDown,
            0x35,
            NSEventModifierFlags::Command
        ));
        assert_eq!(
            (0..5).map(button_bit).collect::<Vec<_>>(),
            vec![Some(1), Some(2), Some(4), Some(8), Some(16)]
        );
        assert_eq!(wheel_steps(-3.75, true), -0.25);
        assert_eq!(wheel_steps(1.0, false), 1.0);
    }
}
