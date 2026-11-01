//! Deterministic HID queue. No wall clock, network, or GUI code lives here.
use std::{
    collections::{BTreeSet, VecDeque},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const MAX_PENDING: usize = 1024;
// Conservative keyboard pacing, independent of the mouse sampling rate.
const KEYBOARD_INTERVAL: Duration = Duration::from_millis(20);
const MAX_MOTION: i64 = 1_000_000;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct InputSettings {
    pub hz: u32,
    pub sensitivity: f64,
}

impl Default for InputSettings {
    fn default() -> Self {
        Self {
            hz: 125,
            sensitivity: 1.0,
        }
    }
}

impl InputSettings {
    pub fn validate(&self) -> Result<(), String> {
        if ![125, 250, 500, 1000].contains(&self.hz) {
            return Err("La frecuencia debe ser 125, 250, 500 o 1000 Hz".into());
        }
        if !self.sensitivity.is_finite() || !(0.1..=4.0).contains(&self.sensitivity) {
            return Err("La sensibilidad debe estar entre 0.1 y 4".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Pending {
    Motion { x: i64, y: i64, buttons: u8 },
    Buttons(u8),
    Wheel(i32),
    Keyboard { keys: Vec<u8>, modifier: u8 },
}

#[derive(Debug)]
pub struct Scheduler {
    pub settings: InputSettings,
    queue: VecDeque<Pending>,
    fraction_x: f64,
    fraction_y: f64,
    wheel_fraction: f64,
    buttons: u8,
    // USB HID usages, including modifier usages 0xe0..=0xe7. Backends
    // normalize their platform key codes before entering the shared scheduler.
    pressed: BTreeSet<u8>,
    next_keyboard: Duration,
}

fn rpc(method: &str, params: Value) -> Value {
    // Notifications avoid a response for every HID report.
    json!({"jsonrpc":"2.0", "method":method, "params":params})
}

fn motion_chunk(x: i64, y: i64) -> (i64, i64) {
    // Divide both axes by the same factor to preserve the direction of large
    // diagonal movements. Independent clamping would spend the short axis first.
    let parts = ((x.abs().max(y.abs()) + 126) / 127).max(1);
    let divide = |value: i64| value.signum() * ((value.abs() + parts / 2) / parts);
    (divide(x), divide(y))
}

impl Default for Scheduler {
    fn default() -> Self {
        Self {
            settings: InputSettings::default(),
            queue: VecDeque::new(),
            fraction_x: 0.0,
            fraction_y: 0.0,
            wheel_fraction: 0.0,
            buttons: 0,
            pressed: BTreeSet::new(),
            next_keyboard: Duration::ZERO,
        }
    }
}

impl Scheduler {
    pub fn set_settings(&mut self, settings: InputSettings) -> Result<(), String> {
        settings.validate()?;
        self.settings = settings;
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.queue.len()
    }

    fn push(&mut self, item: Pending) -> Result<(), String> {
        if self.queue.len() >= MAX_PENDING {
            return Err("Cola de entrada saturada; se liberará la captura".into());
        }
        self.queue.push_back(item);
        Ok(())
    }

    pub fn motion(&mut self, dx: f64, dy: f64) -> Result<(), String> {
        if !dx.is_finite() || !dy.is_finite() {
            return Err("El movimiento contiene valores no finitos".into());
        }
        let x = dx * self.settings.sensitivity + self.fraction_x;
        let y = dy * self.settings.sensitivity + self.fraction_y;
        if x.abs() > MAX_MOTION as f64 || y.abs() > MAX_MOTION as f64 {
            return Err("Movimiento pendiente excesivo".into());
        }
        let ix = x.trunc() as i64;
        let iy = y.trunc() as i64;
        if ix != 0 || iy != 0 {
            match self.queue.back_mut() {
                Some(Pending::Motion {
                    x: px,
                    y: py,
                    buttons,
                }) if *buttons == self.buttons => {
                    if (*px + ix).abs() > MAX_MOTION || (*py + iy).abs() > MAX_MOTION {
                        return Err("Movimiento pendiente excesivo".into());
                    }
                    *px += ix;
                    *py += iy;
                }
                _ => self.push(Pending::Motion {
                    x: ix,
                    y: iy,
                    buttons: self.buttons,
                })?,
            }
        }
        self.fraction_x = x - ix as f64;
        self.fraction_y = y - iy as f64;
        Ok(())
    }

    pub fn button(&mut self, bit: u8, pressed: bool) -> Result<(), String> {
        let new = if pressed {
            self.buttons | bit
        } else {
            self.buttons & !bit
        };
        if new == self.buttons {
            return Ok(());
        }
        self.push(Pending::Buttons(new))?;
        self.buttons = new;
        Ok(())
    }

    /// Positive values mean wheel up. Fractional input is carried to the next event.
    pub fn wheel(&mut self, steps: f64) -> Result<(), String> {
        if !steps.is_finite() || steps.abs() > 10_000.0 {
            return Err("Desplazamiento de rueda inválido".into());
        }
        let value = steps + self.wheel_fraction;
        let whole = value.trunc() as i32;
        if whole != 0 {
            self.push(Pending::Wheel(whole))?;
        }
        self.wheel_fraction = value - whole as f64;
        Ok(())
    }

    /// Returns true for the local emergency-release shortcut; Escape is never sent.
    pub fn key(&mut self, code: u8, down: bool) -> Result<bool, String> {
        let mut pressed = self.pressed.clone();
        if down {
            pressed.insert(code);
        } else {
            pressed.remove(&code);
        }
        if down
            && code == 41
            && (pressed.contains(&224) || pressed.contains(&228))
            && (pressed.contains(&226) || pressed.contains(&230))
        {
            return Ok(true);
        }
        if pressed == self.pressed {
            return Ok(false);
        }
        let modifier = pressed.iter().fold(0, |mods, code| mods | modifier(*code));
        let mut keys: Vec<u8> = pressed
            .iter()
            .copied()
            .filter(|code| !(224..=231).contains(code))
            .collect();
        keys.sort_unstable();
        keys.dedup();
        if keys.len() > 6 {
            keys = vec![1; 6];
        } // USB boot-keyboard ErrorRollOver.
        self.push(Pending::Keyboard { keys, modifier })?;
        self.pressed = pressed;
        Ok(false)
    }

    /// Release is deliberately a barrier: stale motion must not execute after release.
    pub fn release(&mut self) {
        self.queue.clear();
        self.fraction_x = 0.0;
        self.fraction_y = 0.0;
        self.wheel_fraction = 0.0;
        self.buttons = 0;
        self.pressed.clear();
        self.queue.push_back(Pending::Buttons(0));
        self.queue.push_back(Pending::Keyboard {
            keys: vec![],
            modifier: 0,
        });
    }

    /// Keep keyboard transitions paced and FIFO, but let motion pass a keyboard
    /// cooldown. Mouse buttons and wheel remain ordering barriers.
    /// The worker must use the same timestamp for peek and acknowledgement.
    fn eligible_index(&self, now: Duration) -> Option<usize> {
        let front = self.queue.front()?;
        if !matches!(front, Pending::Keyboard { .. }) || now >= self.next_keyboard {
            return Some(0);
        }
        self.queue
            .iter()
            .position(|item| !matches!(item, Pending::Keyboard { .. }))
            .filter(|index| matches!(self.queue.get(*index), Some(Pending::Motion { .. })))
    }

    pub fn peek_at(&self, now: Duration) -> Option<Value> {
        self.peek_index(self.eligible_index(now)?)
    }

    pub fn acknowledge_at(&mut self, now: Duration) {
        let Some(index) = self.eligible_index(now) else {
            return;
        };
        if matches!(self.queue.get(index), Some(Pending::Keyboard { .. })) {
            self.next_keyboard = now.saturating_add(KEYBOARD_INTERVAL);
        }
        self.acknowledge_index(index);
    }

    /// Inspect a report without consuming it. Caller acknowledges only a successful send.
    pub fn peek(&self) -> Option<Value> {
        self.peek_index(0)
    }

    fn peek_index(&self, index: usize) -> Option<Value> {
        Some(match self.queue.get(index)? {
            Pending::Motion { x, y, buttons } => {
                let (dx, dy) = motion_chunk(*x, *y);
                rpc("relMouseReport", json!({"dx":dx,"dy":dy,"buttons":buttons}))
            }
            Pending::Buttons(buttons) => {
                rpc("relMouseReport", json!({"dx":0,"dy":0,"buttons":buttons}))
            }
            Pending::Wheel(delta) => rpc(
                "wheelReport",
                json!({"wheelY": (*delta).clamp(-127,127), "mouseMode":"relative"}),
            ),
            Pending::Keyboard { keys, modifier } => {
                rpc("keyboardReport", json!({"keys":keys,"modifier":modifier}))
            }
        })
    }

    pub fn acknowledge(&mut self) {
        self.acknowledge_index(0);
    }

    fn acknowledge_index(&mut self, index: usize) {
        let finished = match self.queue.get_mut(index) {
            Some(Pending::Motion { x, y, .. }) => {
                let (dx, dy) = motion_chunk(*x, *y);
                *x -= dx;
                *y -= dy;
                *x == 0 && *y == 0
            }
            Some(Pending::Wheel(delta)) => {
                *delta -= (*delta).clamp(-127, 127);
                *delta == 0
            }
            Some(_) => true,
            None => false,
        };
        if finished {
            self.queue.remove(index);
        }
    }
}

fn modifier(code: u8) -> u8 {
    if (224..=231).contains(&code) {
        1 << (code - 224)
    } else {
        0
    }
}

/// Physical Linux evdev positions to USB HID usage IDs (independent of layout).
#[cfg(any(target_os = "linux", test))]
pub fn evdev_to_hid(code: u32) -> Option<u8> {
    Some(match code {
        1 => 41,
        2..=10 => (code + 28) as u8,
        11 => 39,
        12 => 45,
        13 => 46,
        14 => 42,
        15 => 43,
        16 => 20,
        17 => 26,
        18 => 8,
        19 => 21,
        20 => 23,
        21 => 28,
        22 => 24,
        23 => 12,
        24 => 18,
        25 => 19,
        26 => 47,
        27 => 48,
        28 => 40,
        29 => 224,
        30 => 4,
        31 => 22,
        32 => 7,
        33 => 9,
        34 => 10,
        35 => 11,
        36 => 13,
        37 => 14,
        38 => 15,
        39 => 51,
        40 => 52,
        41 => 53,
        42 => 225,
        43 => 49,
        44 => 29,
        45 => 27,
        46 => 6,
        47 => 25,
        48 => 5,
        49 => 17,
        50 => 16,
        51 => 54,
        52 => 55,
        53 => 56,
        54 => 229,
        55 => 85,
        56 => 226,
        57 => 44,
        58 => 57,
        59..=68 => (code - 1) as u8,
        69 => 83,
        70 => 71,
        71 => 95,
        72 => 96,
        73 => 97,
        74 => 86,
        75 => 92,
        76 => 93,
        77 => 94,
        78 => 87,
        79 => 89,
        80 => 90,
        81 => 91,
        82 => 98,
        83 => 99,
        86 => 100,
        87 => 68,
        88 => 69,
        96 => 88,
        97 => 228,
        98 => 84,
        99 => 70,
        100 => 230,
        102 => 74,
        103 => 82,
        104 => 75,
        105 => 80,
        106 => 79,
        107 => 77,
        108 => 81,
        109 => 78,
        110 => 73,
        111 => 76,
        119 => 72,
        125 => 227,
        126 => 231,
        127 => 101,
        183..=194 => (code - 79) as u8,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn drain(s: &mut Scheduler) -> Vec<Value> {
        let mut reports = vec![];
        while let Some(v) = s.peek() {
            reports.push(v);
            s.acknowledge();
        }
        reports
    }
    #[test]
    fn split_preserves_large_signed_distance() {
        for (x, y) in [(5000, -2071), (-4097, 888), (0, -300), (127, 128)] {
            let mut s = Scheduler::default();
            s.motion(x as f64, y as f64).unwrap();
            let mut sum = (0, 0);
            for v in drain(&mut s) {
                let dx = v["params"]["dx"].as_i64().unwrap();
                let dy = v["params"]["dy"].as_i64().unwrap();
                assert!((-127..=127).contains(&dx));
                assert!((-127..=127).contains(&dy));
                sum.0 += dx;
                sum.1 += dy;
            }
            assert_eq!(sum, (x, y));
        }
    }
    #[test]
    fn fractions_survive_ticks_and_negative_motion() {
        let mut s = Scheduler::default();
        s.set_settings(InputSettings {
            hz: 1000,
            sensitivity: 0.25,
        })
        .unwrap();
        let mut total = (0, 0);
        for _ in 0..400 {
            s.motion(1.0, -1.0).unwrap();
            for v in drain(&mut s) {
                total.0 += v["params"]["dx"].as_i64().unwrap();
                total.1 += v["params"]["dy"].as_i64().unwrap();
            }
        }
        assert_eq!(total, (100, -100));
    }
    #[test]
    fn large_diagonal_motion_keeps_its_direction() {
        let mut s = Scheduler::default();
        s.motion(1000.0, 100.0).unwrap();
        let mut point = (0, 0);
        for v in drain(&mut s) {
            point.0 += v["params"]["dx"].as_i64().unwrap();
            point.1 += v["params"]["dy"].as_i64().unwrap();
            assert!((1000_i64 * point.1 - 100 * point.0).abs() <= 1000);
        }
        assert_eq!(point, (1000, 100));
    }
    #[test]
    fn button_and_wheel_barriers_keep_drag_order() {
        let mut s = Scheduler::default();
        s.motion(300.0, 0.0).unwrap();
        s.button(1, true).unwrap();
        s.motion(10.0, 0.0).unwrap();
        s.wheel(1.0).unwrap();
        s.button(1, false).unwrap();
        let r = drain(&mut s);
        assert_eq!(r.len(), 7);
        assert!(r[..3].iter().all(|v| v["params"]["buttons"] == 0));
        assert_eq!(r[3]["params"]["buttons"], 1);
        assert_eq!(r[4]["params"]["buttons"], 1);
        assert_eq!(r[5]["method"], "wheelReport");
        assert_eq!(r[6]["params"]["buttons"], 0);
    }
    #[test]
    fn failed_send_does_not_consume_pending_motion() {
        let mut s = Scheduler::default();
        s.motion(301.0, -9.0).unwrap();
        assert_eq!(s.peek(), s.peek());
        let r = drain(&mut s);
        assert_eq!(
            r.iter()
                .map(|v| v["params"]["dx"].as_i64().unwrap())
                .sum::<i64>(),
            301
        );
    }
    #[test]
    fn queue_limit_is_explicit_and_release_has_priority() {
        let mut s = Scheduler::default();
        for i in 0..MAX_PENDING {
            s.button(1, i % 2 == 0).unwrap();
        }
        assert!(s.wheel(1.0).is_err());
        assert_eq!(s.len(), MAX_PENDING);
        s.release();
        assert_eq!(s.len(), 2);
        assert_eq!(drain(&mut s)[1]["params"]["keys"], json!([]));
    }
    #[test]
    fn emergency_shortcut_is_local_and_release_resets_modifiers() {
        let mut s = Scheduler::default();
        s.key(224, true).unwrap();
        s.key(226, true).unwrap();
        assert!(s.key(41, true).unwrap());
        s.release();
        let r = drain(&mut s);
        assert_eq!(r[1]["params"]["modifier"], 0);
        assert!(r.iter().all(|v| v["params"]["keys"] != json!([41])));
    }

    #[test]
    fn normalized_hid_preserves_both_modifier_sides_and_boot_rollover() {
        let mut s = Scheduler::default();
        for code in 224..=231 {
            s.key(code, true).unwrap();
        }
        let reports = drain(&mut s);
        assert_eq!(reports.last().unwrap()["params"]["modifier"], 255);
        assert_eq!(reports.last().unwrap()["params"]["keys"], json!([]));
        for code in 4..=10 {
            s.key(code, true).unwrap();
        }
        assert_eq!(
            drain(&mut s).last().unwrap()["params"]["keys"],
            json!(vec![1; 6])
        );
        s.key(10, false).unwrap();
        assert_eq!(
            drain(&mut s)[0]["params"]["keys"],
            json!([4, 5, 6, 7, 8, 9])
        );
    }

    #[test]
    fn keyboard_bursts_keep_press_release_order_at_all_mouse_rates() {
        for hz in [125, 250, 500, 1000] {
            let mut s = Scheduler::default();
            s.set_settings(InputSettings {
                hz,
                sensitivity: 1.0,
            })
            .unwrap();
            for code in [225, 4, 5] {
                s.key(code, true).unwrap();
            }
            for code in [4, 5, 225] {
                s.key(code, false).unwrap();
            }
            let expected = [
                json!([]),
                json!([4]),
                json!([4, 5]),
                json!([5]),
                json!([]),
                json!([]),
            ];
            for (i, keys) in expected.into_iter().enumerate() {
                let now = KEYBOARD_INTERVAL * i as u32;
                if i > 0 {
                    assert!(s.peek_at(now - Duration::from_millis(1)).is_none());
                }
                let report = s.peek_at(now).unwrap();
                assert_eq!(report["params"]["keys"], keys);
                assert_eq!(report["params"]["modifier"], if i < 5 { 2 } else { 0 });
                s.acknowledge_at(now);
            }
            assert_eq!(s.len(), 0);
        }
    }

    #[test]
    fn failed_keyboard_send_keeps_report_and_pacing_starts_on_success() {
        let mut s = Scheduler::default();
        s.key(4, true).unwrap();
        s.key(4, false).unwrap();
        let first = s.peek_at(Duration::ZERO).unwrap();
        let retry = Duration::from_millis(100);
        assert_eq!(s.peek_at(retry), Some(first));
        s.acknowledge_at(retry);
        assert!(s.peek_at(retry).is_none());
        assert_eq!(
            s.peek_at(retry + KEYBOARD_INTERVAL).unwrap()["params"]["keys"],
            json!([])
        );
    }

    #[test]
    fn keyboard_wait_allows_motion_and_release_discards_stale_keys() {
        let mut s = Scheduler::default();
        s.key(4, true).unwrap();
        s.key(4, true).unwrap(); // OS autorepeat must not add reports.
        s.key(4, false).unwrap();
        s.motion(9.0, 0.0).unwrap();
        assert_eq!(s.len(), 3);
        s.acknowledge_at(Duration::ZERO);
        assert_eq!(
            s.peek_at(Duration::from_millis(1)).unwrap()["params"]["dx"],
            9
        );
        s.release();
        assert_eq!(
            s.peek_at(Duration::from_millis(1)).unwrap()["method"],
            "relMouseReport"
        );
        s.acknowledge_at(Duration::from_millis(1));
        assert!(s.peek_at(Duration::from_millis(1)).is_none());
        let released = s.peek_at(KEYBOARD_INTERVAL).unwrap();
        assert_eq!(released["params"]["keys"], json!([]));
        assert_eq!(released["params"]["modifier"], 0);
        s.acknowledge_at(KEYBOARD_INTERVAL);
        assert_eq!(s.len(), 0);
    }

    #[test]
    fn mouse_continues_during_keyboard_cooldown_without_reordering_keys() {
        let mut s = Scheduler::default();
        s.key(4, true).unwrap();
        s.key(5, true).unwrap();
        s.key(4, false).unwrap();
        s.motion(12.0, -4.0).unwrap();
        assert_eq!(
            s.peek_at(Duration::ZERO).unwrap()["params"]["keys"],
            json!([4])
        );
        s.acknowledge_at(Duration::ZERO);

        let moving = Duration::from_millis(1);
        let motion = s.peek_at(moving).unwrap();
        assert_eq!(motion["method"], "relMouseReport");
        assert_eq!(motion["params"]["dx"], 12);
        assert_eq!(motion["params"]["dy"], -4);
        s.acknowledge_at(moving);
        assert!(s.peek_at(moving).is_none());

        assert_eq!(
            s.peek_at(KEYBOARD_INTERVAL).unwrap()["params"]["keys"],
            json!([4, 5])
        );
        s.acknowledge_at(KEYBOARD_INTERVAL);
        assert!(s.peek_at(KEYBOARD_INTERVAL + moving).is_none());
        assert_eq!(
            s.peek_at(KEYBOARD_INTERVAL * 2).unwrap()["params"]["keys"],
            json!([5])
        );
        s.acknowledge_at(KEYBOARD_INTERVAL * 2);
        assert_eq!(s.len(), 0);
    }

    #[test]
    fn keyboard_cooldown_never_bypasses_button_or_wheel_barriers() {
        for wheel in [false, true] {
            let mut s = Scheduler::default();
            s.key(4, true).unwrap();
            s.key(4, false).unwrap();
            s.motion(10.0, 0.0).unwrap();
            if wheel {
                s.wheel(1.0).unwrap();
            } else {
                s.button(1, true).unwrap();
            }
            s.motion(20.0, 0.0).unwrap();
            s.acknowledge_at(Duration::ZERO);

            let moving = Duration::from_millis(1);
            let first_motion = s.peek_at(moving).unwrap();
            assert_eq!(first_motion["params"]["dx"], 10);
            assert_eq!(first_motion["params"]["buttons"], 0);
            s.acknowledge_at(moving);
            assert!(s.peek_at(moving).is_none());

            assert_eq!(
                s.peek_at(KEYBOARD_INTERVAL).unwrap()["method"],
                "keyboardReport"
            );
            s.acknowledge_at(KEYBOARD_INTERVAL);
            let barrier = s.peek_at(KEYBOARD_INTERVAL).unwrap();
            if wheel {
                assert_eq!(barrier["method"], "wheelReport");
            } else {
                assert_eq!(barrier["params"]["dx"], 0);
                assert_eq!(barrier["params"]["buttons"], 1);
            }
            s.acknowledge_at(KEYBOARD_INTERVAL);
            assert_eq!(s.peek_at(KEYBOARD_INTERVAL).unwrap()["params"]["dx"], 20);
            s.acknowledge_at(KEYBOARD_INTERVAL);
            assert_eq!(s.len(), 0);
        }
    }

    #[test]
    fn bypassed_motion_retries_and_preserves_every_partial_delta() {
        let mut s = Scheduler::default();
        s.key(4, true).unwrap();
        s.key(4, false).unwrap();
        s.motion(301.0, -9.0).unwrap();
        s.acknowledge_at(Duration::ZERO);
        let first = s.peek_at(Duration::from_millis(1)).unwrap();
        // A failed transport send must retry the same motion without advancing keys.
        assert_eq!(s.peek_at(Duration::from_millis(2)), Some(first.clone()));
        s.acknowledge_at(Duration::from_millis(2));
        let mut total = (
            first["params"]["dx"].as_i64().unwrap(),
            first["params"]["dy"].as_i64().unwrap(),
        );
        for tick in 3..=4 {
            let now = Duration::from_millis(tick);
            let chunk = s.peek_at(now).unwrap();
            assert_eq!(chunk["method"], "relMouseReport");
            total.0 += chunk["params"]["dx"].as_i64().unwrap();
            total.1 += chunk["params"]["dy"].as_i64().unwrap();
            s.acknowledge_at(now);
        }
        assert_eq!(total, (301, -9));
        assert!(s.peek_at(Duration::from_millis(4)).is_none());
        assert_eq!(
            s.peek_at(KEYBOARD_INTERVAL).unwrap()["params"]["keys"],
            json!([])
        );
        s.acknowledge_at(KEYBOARD_INTERVAL);
        assert_eq!(s.len(), 0);
    }

    #[test]
    fn linux_codes_normalize_to_physical_hid_usages() {
        assert_eq!(evdev_to_hid(30), Some(4));
        assert_eq!(evdev_to_hid(1), Some(41));
        for (code, usage) in [
            (29, 224),
            (42, 225),
            (56, 226),
            (125, 227),
            (97, 228),
            (54, 229),
            (100, 230),
            (126, 231),
        ] {
            assert_eq!(evdev_to_hid(code), Some(usage));
        }
        assert_eq!(evdev_to_hid(u32::MAX), None);
    }
}
