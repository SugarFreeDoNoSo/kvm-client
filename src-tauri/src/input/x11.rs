use super::Shared;
use std::{sync::Arc, thread, time::Duration};
use x11rb::{
    connection::Connection,
    protocol::{
        xinput::{ConnectionExt as _, EventMask as XiMask, XIEventMask},
        xproto::{ConnectionExt as _, CreateGCAux, EventMask, GrabMode, GrabStatus, Rectangle},
        Event,
    },
    CURRENT_TIME,
};

pub(super) fn run(shared: Arc<Shared>, generation: u64) -> Result<(), String> {
    let (connection, screen) = x11rb::connect(None).map_err(|e| e.to_string())?;
    let root = connection.setup().roots[screen].root;
    connection
        .xinput_xi_query_version(2, 2)
        .map_err(|e| e.to_string())?
        .reply()
        .map_err(|e| format!("XInput2: {e}"))?;
    let focused = connection
        .get_input_focus()
        .map_err(|e| e.to_string())?
        .reply()
        .map_err(|e| e.to_string())?
        .focus;
    let window = if focused > 1 { focused } else { root };
    connection
        .xinput_xi_select_events(
            root,
            &[XiMask {
                deviceid: 1, // XIAllMasterDevices avoids duplicate master+slave motion.
                mask: vec![
                    XIEventMask::RAW_MOTION
                        | XIEventMask::RAW_BUTTON_PRESS
                        | XIEventMask::RAW_BUTTON_RELEASE
                        | XIEventMask::RAW_KEY_PRESS
                        | XIEventMask::RAW_KEY_RELEASE,
                ],
            }],
        )
        .map_err(|e| e.to_string())?
        .check()
        .map_err(|e| e.to_string())?;

    // A transparent cursor while the local pointer is confined to the viewer.
    let pixmap = connection.generate_id().map_err(|e| e.to_string())?;
    let gc = connection.generate_id().map_err(|e| e.to_string())?;
    let cursor = connection.generate_id().map_err(|e| e.to_string())?;
    connection
        .create_pixmap(1, pixmap, window, 1, 1)
        .map_err(|e| e.to_string())?;
    connection
        .create_gc(gc, pixmap, &CreateGCAux::new().foreground(0))
        .map_err(|e| e.to_string())?;
    connection
        .poly_fill_rectangle(
            pixmap,
            gc,
            &[Rectangle {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            }],
        )
        .map_err(|e| e.to_string())?;
    connection
        .create_cursor(cursor, pixmap, pixmap, 0, 0, 0, 0, 0, 0, 0, 0)
        .map_err(|e| e.to_string())?
        .check()
        .map_err(|e| e.to_string())?;
    connection.free_gc(gc).map_err(|e| e.to_string())?;
    connection.free_pixmap(pixmap).map_err(|e| e.to_string())?;

    let pointer = connection
        .grab_pointer(
            false,
            window,
            EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE | EventMask::POINTER_MOTION,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
            window,
            cursor,
            CURRENT_TIME,
        )
        .map_err(|e| e.to_string())?
        .reply()
        .map_err(|e| e.to_string())?;
    if pointer.status != GrabStatus::SUCCESS {
        return Err(format!(
            "El servidor X11 rechazó la captura del mouse: {:?}",
            pointer.status
        ));
    }
    let result = (|| {
        let keyboard = connection
            .grab_keyboard(
                false,
                window,
                CURRENT_TIME,
                GrabMode::ASYNC,
                GrabMode::ASYNC,
            )
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| e.to_string())?;
        if keyboard.status != GrabStatus::SUCCESS {
            return Err(format!(
                "El servidor X11 rechazó la captura del teclado: {:?}",
                keyboard.status
            ));
        }
        connection.flush().map_err(|e| e.to_string())?;
        if !shared.running(generation) {
            return Ok(());
        }
        shared.report(
            true,
            "XInput2",
            "Control remoto activo · Ctrl+Alt+Esc para liberar",
        );
        while shared.running(generation) {
            // poll_for_event never blocks release; the connection itself owns the
            // grabs, so disconnecting/crashing also restores local input.
            if let Some(event) = connection.poll_for_event().map_err(|e| e.to_string())? {
                match event {
                    Event::XinputRawMotion(event) => {
                        let mut dx = 0.0;
                        let mut dy = 0.0;
                        let mut value = 0;
                        for (block, mask) in event.valuator_mask.iter().enumerate() {
                            for bit in 0..32 {
                                if mask & (1 << bit) == 0 {
                                    continue;
                                }
                                let Some(axis) = event.axisvalues_raw.get(value) else {
                                    break;
                                };
                                let movement =
                                    axis.integral as f64 + axis.frac as f64 / 4294967296.0;
                                match block * 32 + bit {
                                    0 => dx = movement,
                                    1 => dy = movement,
                                    _ => {}
                                }
                                value += 1;
                            }
                        }
                        shared.motion(dx, dy)?;
                    }
                    Event::XinputRawButtonPress(event) => {
                        if event.detail == 4 {
                            shared.wheel(1.0)?;
                        } else if event.detail == 5 {
                            shared.wheel(-1.0)?;
                        } else if let Some(bit) = button_bit(event.detail) {
                            shared.button(bit, true)?;
                        }
                    }
                    Event::XinputRawButtonRelease(event) => {
                        if let Some(bit) = button_bit(event.detail) {
                            shared.button(bit, false)?;
                        }
                    }
                    Event::XinputRawKeyPress(event) => {
                        if shared.key(event.detail.saturating_sub(8), true)? {
                            break;
                        }
                    }
                    Event::XinputRawKeyRelease(event) => {
                        shared.key(event.detail.saturating_sub(8), false)?;
                    }
                    _ => {}
                }
            } else {
                thread::sleep(Duration::from_millis(1));
            }
        }
        Ok(())
    })();
    let _ = connection.ungrab_keyboard(CURRENT_TIME);
    let _ = connection.ungrab_pointer(CURRENT_TIME);
    let _ = connection.free_cursor(cursor);
    let _ = connection.xinput_xi_select_events(
        root,
        &[XiMask {
            deviceid: 1,
            mask: vec![],
        }],
    );
    let _ = connection.flush();
    result
}

fn button_bit(detail: u32) -> Option<u8> {
    match detail {
        1 => Some(1),
        2 => Some(4),
        3 => Some(2),
        8 => Some(8),
        9 => Some(16),
        _ => None,
    }
}
