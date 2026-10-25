//! Narrow, owned wrapper around libei's stable receiver C API.
//! The context and its borrowed events never leave the capture thread.
use std::{
    ffi::{c_char, c_int, c_void},
    os::fd::{FromRawFd, IntoRawFd, OwnedFd},
    ptr,
};

#[repr(C)]
struct Ei {
    _private: [u8; 0],
}
#[repr(C)]
struct EiEvent {
    _private: [u8; 0],
}
#[repr(C)]
struct EiSeat {
    _private: [u8; 0],
}

#[link(name = "ei")]
unsafe extern "C" {
    fn ei_new_receiver(user_data: *mut c_void) -> *mut Ei;
    fn ei_unref(context: *mut Ei) -> *mut Ei;
    fn ei_configure_name(context: *mut Ei, name: *const c_char);
    fn ei_setup_backend_fd(context: *mut Ei, fd: c_int) -> c_int;
    fn ei_get_fd(context: *mut Ei) -> c_int;
    fn ei_dispatch(context: *mut Ei);
    fn ei_get_event(context: *mut Ei) -> *mut EiEvent;
    fn ei_event_unref(event: *mut EiEvent) -> *mut EiEvent;
    fn ei_event_get_type(event: *mut EiEvent) -> c_int;
    fn ei_event_get_seat(event: *mut EiEvent) -> *mut EiSeat;
    fn ei_seat_bind_capabilities(seat: *mut EiSeat, ...);
    fn ei_event_pointer_get_dx(event: *mut EiEvent) -> f64;
    fn ei_event_pointer_get_dy(event: *mut EiEvent) -> f64;
    fn ei_event_button_get_button(event: *mut EiEvent) -> u32;
    fn ei_event_button_get_is_press(event: *mut EiEvent) -> bool;
    fn ei_event_scroll_get_dy(event: *mut EiEvent) -> f64;
    fn ei_event_scroll_get_discrete_dy(event: *mut EiEvent) -> i32;
    fn ei_event_keyboard_get_key(event: *mut EiEvent) -> u32;
    fn ei_event_keyboard_get_key_is_press(event: *mut EiEvent) -> bool;
}

pub(super) enum Event {
    Motion(f64, f64),
    Button(u8, bool),
    Wheel(f64),
    Key(u32, bool),
    Stopped,
}

pub(super) struct Receiver {
    context: *mut Ei,
    continuous_scroll: f64,
    discrete_scroll: Option<f64>,
}

impl Receiver {
    pub(super) fn readiness_fd(&self) -> Result<OwnedFd, String> {
        // Duplicate only for event-loop registration. libei owns and consumes the
        // original descriptor; both refer to the same socket readiness state.
        let fd = unsafe { libc::fcntl(ei_get_fd(self.context), libc::F_DUPFD_CLOEXEC, 0) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    pub(super) fn new(fd: OwnedFd) -> Result<Self, String> {
        // SAFETY: every pointer is owned here, and libei takes ownership of the fd.
        unsafe {
            let context = ei_new_receiver(ptr::null_mut());
            if context.is_null() {
                return Err("libei no pudo crear el receptor".into());
            }
            ei_configure_name(context, c"kvm-client".as_ptr());
            let result = ei_setup_backend_fd(context, fd.into_raw_fd());
            if result < 0 {
                ei_unref(context);
                return Err(format!(
                    "libei: {}",
                    std::io::Error::from_raw_os_error(-result)
                ));
            }
            ei_dispatch(context);
            Ok(Self {
                context,
                continuous_scroll: 0.0,
                discrete_scroll: None,
            })
        }
    }

    pub(super) fn events(&mut self) -> Result<Vec<Event>, String> {
        let mut result = Vec::new();
        // SAFETY: context is live and used only on its owning thread. Event fields
        // are queried only for their corresponding type, then each event is unref'd.
        unsafe {
            let mut poll = libc::pollfd {
                fd: ei_get_fd(self.context),
                events: libc::POLLIN,
                revents: 0,
            };
            if libc::poll(&mut poll, 1, 0) < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() != std::io::ErrorKind::Interrupted {
                    return Err(error.to_string());
                }
            }
            ei_dispatch(self.context);
            loop {
                let event = ei_get_event(self.context);
                if event.is_null() {
                    break;
                }
                let kind = ei_event_get_type(event);
                match kind {
                    2 => {
                        ei_event_unref(event);
                        return Err("El compositor cerró la conexión libei".into());
                    }
                    3 => {
                        let seat = ei_event_get_seat(event);
                        if !seat.is_null() {
                            // POINTER, KEYBOARD, SCROLL, BUTTON; terminated by NULL.
                            ei_seat_bind_capabilities(
                                seat,
                                1 as c_int,
                                4 as c_int,
                                16 as c_int,
                                32 as c_int,
                                ptr::null::<c_void>(),
                            );
                        }
                    }
                    100 => {
                        let scroll = self
                            .discrete_scroll
                            .take()
                            .unwrap_or(self.continuous_scroll / 15.0);
                        self.continuous_scroll = 0.0;
                        if scroll != 0.0 {
                            result.push(Event::Wheel(scroll));
                        }
                    }
                    // Stable libei enum values: SEAT_REMOVED, DEVICE_REMOVED,
                    // DEVICE_PAUSED, DEVICE_STOP_EMULATING. Each invalidates held
                    // input. Clear scroll fragments so they cannot replay later.
                    4 | 6 | 7 | 201 => {
                        self.continuous_scroll = 0.0;
                        self.discrete_scroll = None;
                        result.push(Event::Stopped);
                    }
                    300 => result.push(Event::Motion(
                        ei_event_pointer_get_dx(event),
                        ei_event_pointer_get_dy(event),
                    )),
                    500 => {
                        let bit = match ei_event_button_get_button(event) {
                            0x110 => 1,
                            0x111 => 2,
                            0x112 => 4,
                            0x113 => 8,
                            0x114 => 16,
                            _ => 0,
                        };
                        if bit != 0 {
                            result.push(Event::Button(bit, ei_event_button_get_is_press(event)));
                        }
                    }
                    600 => self.continuous_scroll -= ei_event_scroll_get_dy(event),
                    603 => {
                        let value = -(ei_event_scroll_get_discrete_dy(event) as f64) / 120.0;
                        *self.discrete_scroll.get_or_insert(0.0) += value;
                    }
                    700 => result.push(Event::Key(
                        ei_event_keyboard_get_key(event),
                        ei_event_keyboard_get_key_is_press(event),
                    )),
                    _ => {}
                }
                ei_event_unref(event);
            }
        }
        Ok(result)
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        // SAFETY: this releases our only owning reference; it also closes the fd.
        unsafe {
            ei_unref(self.context);
        }
    }
}
