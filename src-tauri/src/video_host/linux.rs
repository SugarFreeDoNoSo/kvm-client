use std::{cell::RefCell, sync::Arc};

use gtk::prelude::*;

use super::CaptureCallback;
use crate::media::MediaEngine;

struct Host {
    container: gtk::Box,
    notice: Option<gtk::Label>,
    capture: CaptureCallback,
}

thread_local! {
    static HOST: RefCell<Option<Host>> = const { RefCell::new(None) };
}

pub fn install(window: &tauri::WebviewWindow, capture: CaptureCallback) -> Result<(), String> {
    let container = window.default_vbox().map_err(|e| e.to_string())?;
    for child in container.children() {
        child.set_vexpand(false);
        child.set_size_request(-1, 260);
        container.set_child_packing(&child, false, true, 0, gtk::PackType::Start);
    }
    install_in(&container, capture);
    Ok(())
}

pub fn install_diagnostic(window: &tauri::Window) -> Result<(), String> {
    let container = window.default_vbox().map_err(|e| e.to_string())?;
    // Tauri's stable diagnostic window contains an about:blank webview. Keep
    // Wry's ownership intact while reserving the entire window for the sink.
    for child in container.children() {
        child.set_no_show_all(true);
        child.hide();
    }
    install_in(&container, Arc::new(|| {}));
    Ok(())
}

fn install_in(container: &gtk::Box, capture: CaptureCallback) {
    let host = gtk::Box::new(gtk::Orientation::Vertical, 0);
    host.set_hexpand(true);
    host.set_vexpand(true);
    container.pack_start(&host, true, true, 0);
    HOST.with(|value| {
        *value.borrow_mut() = Some(Host {
            container: host,
            notice: None,
            capture,
        });
    });
    show_placeholder();
}

pub fn attach(media: &MediaEngine) -> Result<(), String> {
    HOST.with(|value| {
        let mut value = value.borrow_mut();
        let host = value
            .as_mut()
            .ok_or("No se creó la superficie de video GTK")?;
        for child in host.container.children() {
            host.container.remove(&child);
        }
        // GTK 0.18 and GStreamer 0.24 use different Rust GLib wrappers; only
        // the borrowed C ABI widget crosses between them, on the GTK thread.
        let widget: gtk::Widget = unsafe {
            gtk::glib::translate::from_glib_none(media.widget_ptr() as *mut gtk::ffi::GtkWidget)
        };
        let events = gtk::EventBox::new();
        events.add(&widget);
        events.add_events(gtk::gdk::EventMask::BUTTON_PRESS_MASK);
        let capture = host.capture.clone();
        events.connect_button_press_event(move |_, _| {
            capture();
            gtk::glib::Propagation::Proceed
        });
        let overlay = gtk::Overlay::new();
        overlay.add(&events);
        let notice = gtk::Label::new(Some("Esperando señal de video…"));
        notice.set_halign(gtk::Align::Center);
        notice.set_valign(gtk::Align::Center);
        notice.set_justify(gtk::Justification::Center);
        overlay.add_overlay(&notice);
        overlay.set_overlay_pass_through(&notice, true);
        host.notice = Some(notice);
        host.container.pack_start(&overlay, true, true, 0);
        host.container.show_all();
        Ok(())
    })
}

pub fn show_notice(message: Option<String>) {
    HOST.with(|value| {
        if let Some(notice) = value
            .borrow()
            .as_ref()
            .and_then(|host| host.notice.as_ref())
        {
            notice.set_text(message.as_deref().unwrap_or(""));
            notice.set_visible(message.is_some());
        }
    });
}

pub fn show_placeholder() {
    HOST.with(|value| {
        if let Some(host) = value.borrow_mut().as_mut() {
            host.notice = None;
            for child in host.container.children() {
                host.container.remove(&child);
            }
            let label = gtk::Label::new(Some("Conecta el KVM para iniciar la transmisión"));
            label.set_hexpand(true);
            label.set_vexpand(true);
            host.container.pack_start(&label, true, true, 0);
            host.container.show_all();
        }
    });
}

pub fn set_toolbar_visible(window: &tauri::WebviewWindow, visible: bool) -> Result<(), String> {
    let container = window.default_vbox().map_err(|e| e.to_string())?;
    HOST.with(|value| {
        if let Some(host) = value.borrow().as_ref() {
            for child in container.children() {
                if child != host.container.clone().upcast::<gtk::Widget>() {
                    child.set_no_show_all(!visible);
                    child.set_visible(visible);
                }
            }
        }
    });
    Ok(())
}
