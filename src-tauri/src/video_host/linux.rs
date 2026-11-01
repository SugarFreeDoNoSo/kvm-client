use std::{cell::RefCell, rc::Rc, sync::Arc};

use gtk::prelude::*;

use super::{dashboard_width, CaptureCallback};
use crate::media::MediaEngine;
use tauri::Emitter;

type ViewportCallback = Arc<dyn Fn(u32, u32) + Send + Sync>;
type ViewportCallbacks = Rc<RefCell<[Option<ViewportCallback>; 2]>>;

struct Host {
    container: gtk::Box,
    layout: gtk::Box,
    video_overlay: gtk::Overlay,
    dashboard_fullscreen: bool,
    expanded_before_fullscreen: bool,
    notice: Option<gtk::Label>,
    capture: CaptureCallback,
    kvm: Option<gtk::Widget>,
    sunshine: Option<gtk::Widget>,
    sunshine_selected: bool,
    viewport_callbacks: ViewportCallbacks,
    toolbar: Vec<gtk::Widget>,
    revealer: Option<gtk::Button>,
    dashboard_expanded: bool,
    toolbar_visible: bool,
}

thread_local! {
    static HOST: RefCell<Option<Host>> = const { RefCell::new(None) };
}

pub fn install(window: &tauri::WebviewWindow, capture: CaptureCallback) -> Result<(), String> {
    let container = window.default_vbox().map_err(|e| e.to_string())?;
    // Keep strong GtkWidget references while moving Wry's existing WebView
    // into a horizontal row. Its ownership remains with the same window.
    let toolbar_children = container.children();
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    row.set_hexpand(true);
    row.set_vexpand(true);
    let sidebar = gtk::Box::new(gtk::Orientation::Vertical, 0);
    for child in &toolbar_children {
        container.remove(child);
        child.set_hexpand(true);
        child.set_vexpand(true);
        child.set_size_request(-1, -1);
        child.set_no_show_all(false);
        child.show();
        sidebar.pack_start(child, true, true, 0);
    }
    sidebar.set_size_request(dashboard_width(false), -1);
    sidebar.set_no_show_all(true);
    sidebar.hide();
    row.pack_start(&sidebar, false, true, 0);
    container.pack_start(&row, true, true, 0);
    let toolbar = vec![sidebar.upcast()];
    let reveal_window = window.clone();
    let reveal: CaptureCallback = Arc::new(move || {
        if set_dashboard_expanded(&reveal_window, true).is_ok() {
            let _ = reveal_window.emit("kvm://dashboard", true);
        }
    });
    install_in(&row, capture, toolbar, Some(reveal));
    row.show_all();
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
    install_in(&container, Arc::new(|| {}), Vec::new(), None);
    Ok(())
}

fn install_in(
    container: &gtk::Box,
    capture: CaptureCallback,
    toolbar: Vec<gtk::Widget>,
    reveal: Option<CaptureCallback>,
) {
    let host = gtk::Box::new(gtk::Orientation::Vertical, 0);
    host.set_hexpand(true);
    host.set_vexpand(true);
    host.set_can_focus(true);
    let viewport_callbacks: ViewportCallbacks = Rc::new(RefCell::new([None, None]));
    let resize_callbacks = viewport_callbacks.clone();
    host.connect_size_allocate(move |container, _| {
        publish_viewport(container, &resize_callbacks);
    });
    let scale_callbacks = viewport_callbacks.clone();
    host.connect_scale_factor_notify(move |container| {
        publish_viewport(container, &scale_callbacks);
    });
    let video_overlay = gtk::Overlay::new();
    video_overlay.add(&host);
    let revealer = reveal.map(|reveal| {
        let button = gtk::Button::with_label("▸ Mostrar panel");
        button.set_halign(gtk::Align::Start);
        button.set_valign(gtk::Align::Center);
        button.set_margin_start(8);
        button.set_size_request(146, 30);
        button.connect_clicked(move |_| reveal());
        let style = gtk::CssProvider::new();
        let _ = style.load_from_data(b"button { color: #161616; background: #fff4d6; border: 2px solid #161616; border-radius: 0; font-weight: bold; box-shadow: 3px 3px #161616; }");
        button.style_context().add_provider(&style, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        video_overlay.add_overlay(&button);
        button
    });
    container.pack_start(&video_overlay, true, true, 0);
    HOST.with(|value| {
        *value.borrow_mut() = Some(Host {
            container: host,
            layout: container.clone(),
            video_overlay: video_overlay.clone(),
            dashboard_fullscreen: false,
            expanded_before_fullscreen: false,
            notice: None,
            capture,
            kvm: None,
            sunshine: None,
            sunshine_selected: false,
            viewport_callbacks,
            toolbar,
            revealer,
            dashboard_expanded: false,
            toolbar_visible: true,
        });
    });
    show_placeholder();
    video_overlay.show_all();
}

fn publish_viewport(container: &gtk::Box, callbacks: &ViewportCallbacks) {
    // Parent allocation stays independent of the negotiated size of either sink,
    // including the currently hidden source. GTK allocations are logical pixels.
    let width = container.allocated_width();
    let height = container.allocated_height();
    let scale = container.scale_factor().max(1) as u32;
    if width <= 0 || height <= 0 {
        return;
    }
    let callbacks = callbacks.borrow().clone();
    for callback in callbacks.into_iter().flatten() {
        callback(
            (width as u32).saturating_mul(scale),
            (height as u32).saturating_mul(scale),
        );
    }
}

pub fn attach(media: &MediaEngine) -> Result<(), String> {
    attach_source(media, false)
}
pub fn attach_sunshine(media: &MediaEngine) -> Result<(), String> {
    attach_source(media, true)
}
fn attach_source(media: &MediaEngine, sunshine: bool) -> Result<(), String> {
    HOST.with(|value| {
        let mut value = value.borrow_mut();
        let host = value
            .as_mut()
            .ok_or("No se creó la superficie de video GTK")?;
        host.viewport_callbacks.borrow_mut()[usize::from(sunshine)] =
            Some(media.viewport_callback());
        let slot = if sunshine {
            &mut host.sunshine
        } else {
            &mut host.kvm
        };
        if let Some(child) = slot.take() {
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
        if !sunshine {
            host.notice = Some(notice);
        }
        host.container.pack_start(&overlay, true, true, 0);
        if sunshine {
            host.sunshine = Some(overlay.upcast());
        } else {
            host.kvm = Some(overlay.upcast());
        }
        host.container.show_all();
        apply_source(host);
        publish_viewport(&host.container, &host.viewport_callbacks);
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
            host.viewport_callbacks.borrow_mut()[0] = None;
            if let Some(child) = host.kvm.take() {
                host.container.remove(&child);
            }
            let label = gtk::Label::new(Some("Conecta el KVM para iniciar la transmisión"));
            label.set_hexpand(true);
            label.set_vexpand(true);
            host.container.pack_start(&label, true, true, 0);
            host.kvm = Some(label.upcast());
            host.container.show_all();
            apply_source(host);
        }
    });
}

pub fn set_toolbar_visible(window: &tauri::WebviewWindow, visible: bool) -> Result<(), String> {
    let was_fullscreen = is_dashboard_fullscreen();
    HOST.with(|value| {
        if let Some(host) = value.borrow_mut().as_mut() {
            host.toolbar_visible = visible;
            if !visible {
                host.dashboard_fullscreen = false;
            }
            apply_dashboard(host);
        }
    });
    if was_fullscreen && !visible {
        let _ = window.emit("kvm://dashboard-fullscreen", false);
    }
    Ok(())
}

pub fn set_dashboard_expanded(window: &tauri::WebviewWindow, expanded: bool) -> Result<(), String> {
    if !expanded && is_dashboard_fullscreen() {
        set_dashboard_fullscreen(window, false)?;
    }
    HOST.with(|value| {
        if let Some(host) = value.borrow_mut().as_mut() {
            host.dashboard_expanded = expanded;
            apply_dashboard(host);
            focus_dashboard(host, expanded);
        }
    });
    Ok(())
}

pub fn is_dashboard_fullscreen() -> bool {
    HOST.with(|value| {
        value
            .borrow()
            .as_ref()
            .is_some_and(|host| host.dashboard_fullscreen)
    })
}

pub fn set_dashboard_fullscreen(
    window: &tauri::WebviewWindow,
    enabled: bool,
) -> Result<(), String> {
    if is_dashboard_fullscreen() == enabled {
        return Ok(());
    }
    let previous = HOST.with(|value| {
        let mut value = value.borrow_mut();
        let host = value
            .as_mut()
            .ok_or("No se creó la superficie de video GTK")?;
        if host.toolbar.is_empty() {
            return Err("La ventana de diagnóstico no tiene dashboard".into());
        }
        let previous = (
            host.dashboard_fullscreen,
            host.dashboard_expanded,
            host.toolbar_visible,
            host.expanded_before_fullscreen,
        );
        if enabled && !host.dashboard_fullscreen {
            host.expanded_before_fullscreen = host.dashboard_expanded;
        }
        host.dashboard_fullscreen = enabled;
        host.dashboard_expanded = if enabled {
            true
        } else {
            host.expanded_before_fullscreen
        };
        host.toolbar_visible = true;
        apply_dashboard(host);
        Ok::<_, String>(previous)
    })?;
    if let Err(error) = window.set_fullscreen(enabled) {
        HOST.with(|value| {
            if let Some(host) = value.borrow_mut().as_mut() {
                (
                    host.dashboard_fullscreen,
                    host.dashboard_expanded,
                    host.toolbar_visible,
                    host.expanded_before_fullscreen,
                ) = previous;
                apply_dashboard(host);
            }
        });
        return Err(error.to_string());
    }
    HOST.with(|value| {
        if let Some(host) = value.borrow().as_ref() {
            focus_dashboard(host, host.dashboard_expanded);
        }
    });
    let _ = window.emit("kvm://dashboard-fullscreen", enabled);
    Ok(())
}

fn focus_dashboard(host: &Host, expanded: bool) {
    if expanded {
        for sidebar in &host.toolbar {
            let _ = sidebar.child_focus(gtk::DirectionType::TabForward);
        }
    } else {
        host.container.grab_focus();
    }
}

fn apply_dashboard(host: &Host) {
    let expanded = host.toolbar_visible && host.dashboard_expanded;
    let full = host.dashboard_fullscreen && host.toolbar_visible;
    if let Some(revealer) = &host.revealer {
        revealer.set_no_show_all(expanded || !host.toolbar_visible);
        revealer.set_visible(host.toolbar_visible && !host.dashboard_expanded);
    }
    for child in &host.toolbar {
        child.set_hexpand(full);
        child.set_size_request(
            if full {
                -1
            } else {
                dashboard_width(host.dashboard_expanded)
            },
            -1,
        );
        host.layout
            .set_child_packing(child, full, true, 0, gtk::PackType::Start);
        child.set_no_show_all(!expanded);
        child.set_visible(expanded);
    }
    host.video_overlay.set_no_show_all(full);
    host.video_overlay.set_visible(!full);
    // GTK relays the resulting allocation change to both viewport callbacks.
    host.layout.queue_resize();
}

fn apply_source(host: &Host) {
    if let Some(widget) = &host.kvm {
        widget.set_visible(!host.sunshine_selected);
    }
    if let Some(widget) = &host.sunshine {
        widget.set_visible(host.sunshine_selected);
    }
}
pub fn select_sunshine(selected: bool) {
    HOST.with(|value| {
        if let Some(host) = value.borrow_mut().as_mut() {
            host.sunshine_selected = selected;
            apply_source(host);
        }
    });
}

pub async fn self_test(_window: tauri::WebviewWindow) -> Result<(), String> {
    Err("La comprobación del dashboard con fullscreen del sistema requiere macOS por ahora".into())
}
