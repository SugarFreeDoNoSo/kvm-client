use std::{
    cell::{Cell, RefCell},
    sync::{mpsc, Arc},
};

use objc2::{
    define_class, msg_send, rc::Retained, runtime::AnyObject, sel, DefinedClass, MainThreadOnly,
};
use objc2_app_kit::{
    NSButton, NSColor, NSEvent, NSFont, NSLayoutConstraint, NSTextAlignment, NSTextField, NSView,
};
use objc2_foundation::{
    ns_string, MainThreadMarker, NSArray, NSObjectProtocol, NSPoint, NSRect, NSString,
};

use super::{dashboard_width, CaptureCallback};
use crate::media::MediaEngine;
use tauri::Emitter;

struct VideoViewIvars {
    capture: CaptureCallback,
    reveal_dashboard: CaptureCallback,
    viewport_callbacks: RefCell<[Option<ViewportCallback>; 2]>,
    viewport_size: Cell<(u32, u32)>,
}

type ViewportCallback = Arc<dyn Fn(u32, u32) + Send + Sync>;

define_class!(
    // SAFETY: this view and its retained children are confined to AppKit's
    // main thread. The superclass imposes no additional subclass invariants.
    #[unsafe(super = NSView)]
    #[thread_kind = MainThreadOnly]
    #[ivars = VideoViewIvars]
    struct KvmVideoView;

    unsafe impl NSObjectProtocol for KvmVideoView {}

    impl KvmVideoView {
        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method_id(hitTest:))]
        fn hit_test(&self, point: NSPoint) -> Option<Retained<NSView>> {
            // The GL sink adds a child view. Keep native input in our host
            // instead of allowing the sink to consume clicks or navigation.
            let hit: Option<Retained<NSView>> = unsafe { msg_send![super(self), hitTest: point] };
            if hit.is_some() {
                // SAFETY: `self` is a live NSView subclass; retaining it is
                // valid for the Objective-C return ownership convention.
                unsafe { Retained::retain((self as *const Self).cast_mut().cast::<NSView>()) }
            } else {
                None
            }
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, _event: &NSEvent) {
            if let Some(window) = self.window() {
                window.makeFirstResponder(Some(self));
            }
            (self.ivars().capture)();
        }

        #[unsafe(method(revealDashboard:))]
        fn reveal_dashboard(&self, _sender: &AnyObject) {
            (self.ivars().reveal_dashboard)();
        }

        #[unsafe(method(layout))]
        fn layout_viewport(&self) {
            // SAFETY: preserve AppKit's normal layout before reading its bounds.
            unsafe { let _: () = msg_send![super(self), layout]; }
            self.publish_viewport(false);
        }

        #[unsafe(method(viewDidChangeBackingProperties))]
        fn backing_properties_changed(&self) {
            // SAFETY: the superclass owns display/backing state transitions.
            unsafe { let _: () = msg_send![super(self), viewDidChangeBackingProperties]; }
            self.publish_viewport(false);
        }

        #[unsafe(method(viewDidMoveToWindow))]
        fn moved_to_window(&self) {
            // SAFETY: notify the superclass before asking for backing coordinates.
            unsafe { let _: () = msg_send![super(self), viewDidMoveToWindow]; }
            self.publish_viewport(false);
        }
    }
);

impl KvmVideoView {
    fn new(
        mtm: MainThreadMarker,
        capture: CaptureCallback,
        reveal_dashboard: CaptureCallback,
    ) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(VideoViewIvars {
            capture,
            reveal_dashboard,
            viewport_callbacks: RefCell::new([None, None]),
            viewport_size: Cell::new((0, 0)),
        });
        // SAFETY: initializes the NSView superclass after initializing our ivars.
        unsafe { msg_send![super(this), initWithFrame: NSRect::ZERO] }
    }

    fn publish_viewport(&self, force: bool) {
        // Use the video container, not either sink's child view. The latter can
        // change its preferred size when new shader output caps are negotiated.
        let backing = self.convertRectToBacking(self.bounds());
        let width = backing.size.width;
        let height = backing.size.height;
        if !width.is_finite() || !height.is_finite() || width < 1.0 || height < 1.0 {
            return;
        }
        let dimensions = (width.round() as u32, height.round() as u32);
        if !force && self.ivars().viewport_size.replace(dimensions) == dimensions {
            return;
        }
        self.ivars().viewport_size.set(dimensions);
        // A callback holds only a weak media reference. Clone callbacks before
        // invoking them so a resize cannot retain the RefCell borrow across I/O.
        let callbacks = self.ivars().viewport_callbacks.borrow().clone();
        for callback in callbacks.into_iter().flatten() {
            callback(dimensions.0, dimensions.1);
        }
    }
}

struct Host {
    // Keep AppKit ownership until the window closes, including while a worker
    // is bringing the old GStreamer pipeline to NULL during disconnection.
    _container: Retained<KvmVideoView>,
    sidebar_width: Option<Retained<NSLayoutConstraint>>,
    sidebar_full_width: Option<Retained<NSLayoutConstraint>>,
    toolbar: Option<Retained<NSView>>,
    dashboard_fullscreen: bool,
    expanded_before_fullscreen: bool,
    revealer: Option<Retained<NSButton>>,
    dashboard_expanded: bool,
    toolbar_visible: bool,
    surface: Retained<NSView>,
    sunshine: Retained<NSView>,
    sunshine_selected: bool,
    notice: Retained<NSTextField>,
}

thread_local! {
    static HOST: RefCell<Option<Host>> = const { RefCell::new(None) };
}

fn main_thread() -> Result<MainThreadMarker, String> {
    MainThreadMarker::new()
        .ok_or_else(|| "La superficie de video requiere el hilo principal de AppKit".into())
}

pub fn install(window: &tauri::WebviewWindow, capture: CaptureCallback) -> Result<(), String> {
    let mtm = main_thread()?;
    let (send, receive) = mpsc::sync_channel(1);
    // Called during setup on Tauri's UI thread, so with_webview dispatches
    // synchronously. Obtain the actual WKWebView, preserving Wry's parent.
    window
        .with_webview(move |webview| {
            let _ = send.send(webview.inner() as usize);
        })
        .map_err(|e| e.to_string())?;
    let pointer = receive
        .try_recv()
        .map_err(|_| "No se obtuvo la vista WebKit en el hilo principal")?;
    // SAFETY: Tauri owns this WKWebView, a subclass of NSView. We only borrow
    // it during installation on the main thread and retain its native parent.
    let toolbar =
        unsafe { (pointer as *const NSView).as_ref() }.ok_or("WebKit no creó su vista nativa")?;
    let parent = unsafe { toolbar.superview() }.ok_or("WebKit no tiene una vista contenedora")?;
    let reveal_window = window.clone();
    let reveal: CaptureCallback = Arc::new(move || {
        if set_dashboard_expanded(&reveal_window, true).is_ok() {
            let _ = reveal_window.emit("kvm://dashboard", true);
        }
    });
    install_in(&parent, Some(toolbar), mtm, capture, reveal);
    Ok(())
}

pub fn install_diagnostic(window: &tauri::Window) -> Result<(), String> {
    let mtm = main_thread()?;
    let pointer = window.ns_view().map_err(|e| e.to_string())?;
    // SAFETY: this borrowed view belongs to the live Tauri diagnostic window;
    // all AppKit calls and object ownership remain on the main thread.
    let parent = unsafe { (pointer as *const NSView).as_ref() }
        .ok_or("No se creó la vista de diagnóstico")?;
    install_in(parent, None, mtm, Arc::new(|| {}), Arc::new(|| {}));
    Ok(())
}

fn pin_edges(view: &NSView, parent: &NSView) -> Vec<Retained<NSLayoutConstraint>> {
    vec![
        view.leadingAnchor()
            .constraintEqualToAnchor(&parent.leadingAnchor()),
        view.trailingAnchor()
            .constraintEqualToAnchor(&parent.trailingAnchor()),
        view.topAnchor()
            .constraintEqualToAnchor(&parent.topAnchor()),
        view.bottomAnchor()
            .constraintEqualToAnchor(&parent.bottomAnchor()),
    ]
}

fn install_in(
    parent: &NSView,
    toolbar: Option<&NSView>,
    mtm: MainThreadMarker,
    capture: CaptureCallback,
    reveal: CaptureCallback,
) {
    let container = KvmVideoView::new(mtm, capture, reveal);
    container.setTranslatesAutoresizingMaskIntoConstraints(false);
    parent.addSubview(&container);
    let mut constraints = vec![
        container
            .trailingAnchor()
            .constraintEqualToAnchor(&parent.trailingAnchor()),
        container
            .topAnchor()
            .constraintEqualToAnchor(&parent.topAnchor()),
        container
            .bottomAnchor()
            .constraintEqualToAnchor(&parent.bottomAnchor()),
    ];
    let sidebar_width = toolbar.map(|toolbar| {
        toolbar
            .widthAnchor()
            .constraintEqualToConstant(dashboard_width(false) as f64)
    });
    let sidebar_full_width = toolbar.map(|toolbar| {
        toolbar
            .trailingAnchor()
            .constraintEqualToAnchor(&parent.trailingAnchor())
    });
    if let Some(toolbar) = toolbar {
        toolbar.setTranslatesAutoresizingMaskIntoConstraints(false);
        constraints.extend([
            toolbar
                .leadingAnchor()
                .constraintEqualToAnchor(&parent.leadingAnchor()),
            toolbar
                .topAnchor()
                .constraintEqualToAnchor(&parent.topAnchor()),
            toolbar
                .bottomAnchor()
                .constraintEqualToAnchor(&parent.bottomAnchor()),
            sidebar_width.as_ref().expect("sidebar width").clone(),
            container
                .leadingAnchor()
                .constraintEqualToAnchor(&toolbar.trailingAnchor()),
        ]);
    } else {
        constraints.push(
            container
                .leadingAnchor()
                .constraintEqualToAnchor(&parent.leadingAnchor()),
        );
    }

    let surface = NSView::initWithFrame(NSView::alloc(mtm), NSRect::ZERO);
    surface.setTranslatesAutoresizingMaskIntoConstraints(false);
    container.addSubview(&surface);
    constraints.extend(pin_edges(&surface, &container));

    let sunshine = NSView::initWithFrame(NSView::alloc(mtm), NSRect::ZERO);
    sunshine.setTranslatesAutoresizingMaskIntoConstraints(false);
    container.addSubview(&sunshine);
    constraints.extend(pin_edges(&sunshine, &container));
    sunshine.setHidden(true);

    // This is a sibling of the render surface. The GL sink's own child view
    // cannot cover the status notice when it is added asynchronously later.
    let notice = NSTextField::wrappingLabelWithString(
        ns_string!("Conecta el KVM para iniciar la transmisión"),
        mtm,
    );
    notice.setTranslatesAutoresizingMaskIntoConstraints(false);
    notice.setAlignment(NSTextAlignment::Center);
    notice.setTextColor(Some(&NSColor::whiteColor()));
    notice.setFont(Some(&NSFont::systemFontOfSize(16.0)));
    notice.setSelectable(false);
    container.addSubview(&notice);
    constraints.extend([
        notice
            .centerXAnchor()
            .constraintEqualToAnchor(&container.centerXAnchor()),
        notice
            .centerYAnchor()
            .constraintEqualToAnchor(&container.centerYAnchor()),
        notice
            .leadingAnchor()
            .constraintGreaterThanOrEqualToAnchor(&container.leadingAnchor()),
        notice
            .trailingAnchor()
            .constraintLessThanOrEqualToAnchor(&container.trailingAnchor()),
    ]);
    // A sibling of the input-capturing container stays clickable above the GL
    // surface without taking space from the video or relying on the hidden WebView.
    let revealer = toolbar.map(|_| {
        // SAFETY: the retained container implements this exact target action.
        let button = unsafe {
            NSButton::buttonWithTitle_target_action(
                ns_string!("▸ Mostrar panel"),
                Some(&container),
                Some(sel!(revealDashboard:)),
                mtm,
            )
        };
        button.setTranslatesAutoresizingMaskIntoConstraints(false);
        button.setFont(Some(&NSFont::boldSystemFontOfSize(12.0)));
        parent.addSubview(&button);
        constraints.extend([
            button
                .leadingAnchor()
                .constraintEqualToAnchor_constant(&container.leadingAnchor(), 8.0),
            button
                .centerYAnchor()
                .constraintEqualToAnchor(&container.centerYAnchor()),
            button
                .widthAnchor()
                .constraintGreaterThanOrEqualToConstant(146.0),
            button.heightAnchor().constraintEqualToConstant(30.0),
        ]);
        button
    });
    NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&constraints));
    parent.layoutSubtreeIfNeeded();
    HOST.with(|value| {
        *value.borrow_mut() = Some(Host {
            _container: container,
            sidebar_width,
            sidebar_full_width,
            toolbar: toolbar.map(|view| {
                unsafe { Retained::retain((view as *const NSView).cast_mut()) }
                    .expect("live WebView")
            }),
            dashboard_fullscreen: false,
            expanded_before_fullscreen: false,
            revealer,
            dashboard_expanded: false,
            toolbar_visible: true,
            surface,
            sunshine,
            sunshine_selected: false,
            notice,
        });
    });
}

pub fn attach(media: &MediaEngine) -> Result<(), String> {
    attach_source(media, false)
}

pub fn attach_sunshine(media: &MediaEngine) -> Result<(), String> {
    attach_source(media, true)
}

fn attach_source(media: &MediaEngine, sunshine: bool) -> Result<(), String> {
    main_thread()?;
    HOST.with(|value| {
        let value = value.borrow();
        let host = value
            .as_ref()
            .ok_or("No se creó la superficie de video AppKit")?;
        let surface = if sunshine {
            &host.sunshine
        } else {
            &host.surface
        };
        media.set_window_handle(Retained::as_ptr(surface) as usize)?;
        host._container.ivars().viewport_callbacks.borrow_mut()[usize::from(sunshine)] =
            Some(media.viewport_callback());
        host._container.publish_viewport(true);
        host.notice
            .setStringValue(ns_string!("Esperando señal de video…"));
        host.notice.setHidden(host.sunshine_selected);
        Ok(())
    })
}

pub fn show_notice(message: Option<String>) {
    debug_assert!(MainThreadMarker::new().is_some());
    HOST.with(|value| {
        if let Some(host) = value.borrow().as_ref() {
            host.notice
                .setStringValue(&NSString::from_str(message.as_deref().unwrap_or("")));
            host.notice
                .setHidden(host.sunshine_selected || message.is_none());
        }
    });
}

pub fn show_placeholder() {
    HOST.with(|value| {
        if let Some(host) = value.borrow().as_ref() {
            host._container.ivars().viewport_callbacks.borrow_mut()[0] = None;
        }
    });
    show_notice(Some("Conecta el KVM para iniciar la transmisión".into()));
}

pub fn set_toolbar_visible(window: &tauri::WebviewWindow, visible: bool) -> Result<(), String> {
    main_thread()?;
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
    main_thread()?;
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
    main_thread()?;
    if is_dashboard_fullscreen() == enabled {
        return Ok(());
    }
    let previous = HOST.with(|value| {
        let mut value = value.borrow_mut();
        let host = value
            .as_mut()
            .ok_or("No se creó la superficie de video AppKit")?;
        if host.toolbar.is_none() {
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
    // Release HOST before requesting the native transition: AppKit may send
    // resize/focus callbacks while changing the window's fullscreen state.
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
    if let Some(window) = host._container.window() {
        if expanded {
            if let Some(toolbar) = &host.toolbar {
                window.makeFirstResponder(Some(toolbar));
            }
        } else {
            window.makeFirstResponder(Some(&*host._container));
        }
    }
}

fn apply_dashboard(host: &Host) {
    let full = host.dashboard_fullscreen && host.toolbar_visible;
    host._container.setHidden(full);
    if let Some(revealer) = &host.revealer {
        revealer.setHidden(!host.toolbar_visible || host.dashboard_expanded || full);
    }
    if let Some(width) = &host.sidebar_width {
        if let Some(full_width) = &host.sidebar_full_width {
            if full {
                width.setActive(false);
                full_width.setActive(true);
            } else {
                full_width.setActive(false);
                width.setConstant(if host.toolbar_visible {
                    dashboard_width(host.dashboard_expanded) as f64
                } else {
                    0.0
                });
                width.setActive(true);
            }
        }
        // The constraints and views stay on AppKit's main thread. A zero video
        // area in dashboard fullscreen is not sent to the filter as a new size.
        if let Some(parent) = unsafe { host._container.superview() } {
            parent.layoutSubtreeIfNeeded();
        }
        host._container.publish_viewport(false);
    }
}

pub fn select_sunshine(selected: bool) {
    HOST.with(|value| {
        if let Some(host) = value.borrow_mut().as_mut() {
            host.sunshine_selected = selected;
            host.surface.setHidden(selected);
            host.sunshine.setHidden(!selected);
            host.notice.setHidden(true);
        }
    });
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct DashboardGeometry {
    mode: &'static str,
    sidebar_width: f64,
    video_width: f64,
    client_width: f64,
    reveal_visible: bool,
}

fn dashboard_geometry(mode: &'static str) -> Result<DashboardGeometry, String> {
    main_thread()?;
    HOST.with(|value| {
        let value = value.borrow();
        let host = value.as_ref().ok_or("No se instaló el dashboard AppKit")?;
        let toolbar = host
            .toolbar
            .as_ref()
            .ok_or("La prueba requiere un WebView")?;
        // SAFETY: HOST retains these views in this window, on AppKit's thread.
        let parent = unsafe { host._container.superview() }.ok_or("Falta la vista contenedora")?;
        parent.layoutSubtreeIfNeeded();
        Ok(DashboardGeometry {
            mode,
            sidebar_width: toolbar.bounds().size.width,
            video_width: host._container.bounds().size.width,
            client_width: parent.bounds().size.width,
            reveal_visible: host.revealer.as_ref().is_some_and(|tab| !tab.isHidden()),
        })
    })
}

async fn dashboard_on_main<T, F>(window: &tauri::WebviewWindow, action: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(&tauri::WebviewWindow) -> Result<T, String> + Send + 'static,
{
    let (send, receive) = tokio::sync::oneshot::channel();
    let callback_window = window.clone();
    window
        .run_on_main_thread(move || {
            let _ = send.send(action(&callback_window));
        })
        .map_err(|error| error.to_string())?;
    receive
        .await
        .map_err(|_| "La prueba del dashboard perdió el hilo principal".to_string())?
}

async fn wait_dashboard_os_fullscreen(
    window: &tauri::WebviewWindow,
    enabled: bool,
) -> Result<(), String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let actual = dashboard_on_main(window, |window| {
            window.is_fullscreen().map_err(|error| error.to_string())
        })
        .await?;
        if actual == enabled {
            // Allow AppKit to finish the final allocation after reporting the state.
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "La ventana no alcanzó fullscreen={enabled} en cinco segundos"
            ));
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

fn check_dashboard_geometry(
    geometry: &DashboardGeometry,
    full: bool,
    expanded: bool,
) -> Result<(), String> {
    let expected = if full {
        geometry.client_width
    } else {
        dashboard_width(expanded) as f64
    };
    let width_matches = (geometry.sidebar_width - expected).abs() <= 1.0;
    let video_matches = (geometry.video_width - (geometry.client_width - expected)).abs() <= 1.0;
    let reveal_matches = geometry.reveal_visible == !expanded;
    if geometry.client_width < 1.0 || !width_matches || !video_matches || !reveal_matches {
        return Err(format!(
            "Geometría de dashboard inválida: modo={}, panel={}, video={}, cliente={}, pestaña={}",
            geometry.mode,
            geometry.sidebar_width,
            geometry.video_width,
            geometry.client_width,
            geometry.reveal_visible
        ));
    }
    Ok(())
}

/// Local geometry/OS-window check. Never creates a media engine or sends HID.
pub async fn self_test(window: tauri::WebviewWindow) -> Result<(), String> {
    let result = async {
        let closed = dashboard_on_main(&window, |window| {
            set_dashboard_expanded(window, false)?;
            dashboard_geometry("hidden")
        })
        .await?;
        check_dashboard_geometry(&closed, false, false)?;
        let sidebar = dashboard_on_main(&window, |_| {
            // Clone outside HOST before invoking the action: its callback mutates
            // HOST, and holding a RefCell borrow during performClick would panic.
            let tab = HOST
                .with(|value| {
                    value
                        .borrow()
                        .as_ref()
                        .and_then(|host| host.revealer.clone())
                })
                .ok_or("La prueba requiere la pestaña nativa")?;
            // SAFETY: this live NSButton has the retained KvmVideoView target and
            // a valid revealDashboard: action, and this runs on AppKit's thread.
            unsafe {
                tab.performClick(None);
            }
            dashboard_geometry("sidebar")
        })
        .await?;
        check_dashboard_geometry(&sidebar, false, true)?;
        dashboard_on_main(&window, |window| set_dashboard_fullscreen(window, true)).await?;
        wait_dashboard_os_fullscreen(&window, true).await?;
        let full = dashboard_on_main(&window, |_| dashboard_geometry("fullscreen")).await?;
        check_dashboard_geometry(&full, true, true)?;
        dashboard_on_main(&window, |window| set_dashboard_fullscreen(window, false)).await?;
        wait_dashboard_os_fullscreen(&window, false).await?;
        let restored =
            dashboard_on_main(&window, |_| dashboard_geometry("sidebar-restored")).await?;
        check_dashboard_geometry(&restored, false, true)?;
        let hidden = dashboard_on_main(&window, |window| {
            set_dashboard_expanded(window, false)?;
            dashboard_geometry("hidden-restored")
        })
        .await?;
        check_dashboard_geometry(&hidden, false, false)?;
        println!(
            "{}",
            serde_json::json!({
                "result": "pass", "dashboardSelfTest": true,
                "modes": [closed, sidebar, full, restored, hidden],
                "osFullscreenEntered": true, "osFullscreenExited": true,
                "hostConnections": 0, "hidReportsSent": 0,
            })
        );
        Ok(())
    }
    .await;
    if result.is_err() {
        let _ = dashboard_on_main(&window, |window| {
            if is_dashboard_fullscreen() {
                set_dashboard_fullscreen(window, false)?;
            }
            set_dashboard_expanded(window, false)
        })
        .await;
    }
    result
}
