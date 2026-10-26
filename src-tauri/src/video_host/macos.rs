use std::{cell::RefCell, sync::mpsc};

use objc2::{define_class, msg_send, rc::Retained, DefinedClass, MainThreadOnly};
use objc2_app_kit::{
    NSColor, NSEvent, NSFont, NSLayoutConstraint, NSTextAlignment, NSTextField, NSView,
};
use objc2_foundation::{
    ns_string, MainThreadMarker, NSArray, NSObjectProtocol, NSPoint, NSRect, NSString,
};

use super::CaptureCallback;
use crate::media::MediaEngine;

struct VideoViewIvars {
    capture: CaptureCallback,
}

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
    }
);

impl KvmVideoView {
    fn new(mtm: MainThreadMarker, capture: CaptureCallback) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(VideoViewIvars { capture });
        // SAFETY: initializes the NSView superclass after initializing our ivars.
        unsafe { msg_send![super(this), initWithFrame: NSRect::ZERO] }
    }
}

struct Host {
    // Keep AppKit ownership until the window closes, including while a worker
    // is bringing the old GStreamer pipeline to NULL during disconnection.
    _container: Retained<KvmVideoView>,
    toolbar_height: Option<Retained<NSLayoutConstraint>>,
    surface: Retained<NSView>,
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
    install_in(&parent, Some(toolbar), mtm, capture);
    Ok(())
}

pub fn install_diagnostic(window: &tauri::Window) -> Result<(), String> {
    let mtm = main_thread()?;
    let pointer = window.ns_view().map_err(|e| e.to_string())?;
    // SAFETY: this borrowed view belongs to the live Tauri diagnostic window;
    // all AppKit calls and object ownership remain on the main thread.
    let parent = unsafe { (pointer as *const NSView).as_ref() }
        .ok_or("No se creó la vista de diagnóstico")?;
    install_in(parent, None, mtm, std::sync::Arc::new(|| {}));
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
) {
    let container = KvmVideoView::new(mtm, capture);
    container.setTranslatesAutoresizingMaskIntoConstraints(false);
    parent.addSubview(&container);
    let mut constraints = vec![
        container
            .leadingAnchor()
            .constraintEqualToAnchor(&parent.leadingAnchor()),
        container
            .trailingAnchor()
            .constraintEqualToAnchor(&parent.trailingAnchor()),
        container
            .bottomAnchor()
            .constraintEqualToAnchor(&parent.bottomAnchor()),
    ];
    let toolbar_height =
        toolbar.map(|toolbar| toolbar.heightAnchor().constraintEqualToConstant(260.0));
    if let Some(toolbar) = toolbar {
        toolbar.setTranslatesAutoresizingMaskIntoConstraints(false);
        constraints.extend([
            toolbar
                .leadingAnchor()
                .constraintEqualToAnchor(&parent.leadingAnchor()),
            toolbar
                .trailingAnchor()
                .constraintEqualToAnchor(&parent.trailingAnchor()),
            toolbar
                .topAnchor()
                .constraintEqualToAnchor(&parent.topAnchor()),
            toolbar_height.as_ref().expect("toolbar height").clone(),
            container
                .topAnchor()
                .constraintEqualToAnchor(&toolbar.bottomAnchor()),
        ]);
    } else {
        constraints.push(
            container
                .topAnchor()
                .constraintEqualToAnchor(&parent.topAnchor()),
        );
    }

    let surface = NSView::initWithFrame(NSView::alloc(mtm), NSRect::ZERO);
    surface.setTranslatesAutoresizingMaskIntoConstraints(false);
    container.addSubview(&surface);
    constraints.extend(pin_edges(&surface, &container));

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
    NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&constraints));
    parent.layoutSubtreeIfNeeded();
    HOST.with(|value| {
        *value.borrow_mut() = Some(Host {
            _container: container,
            toolbar_height,
            surface,
            notice,
        });
    });
}

pub fn attach(media: &MediaEngine) -> Result<(), String> {
    main_thread()?;
    HOST.with(|value| {
        let value = value.borrow();
        let host = value
            .as_ref()
            .ok_or("No se creó la superficie de video AppKit")?;
        media.set_window_handle(Retained::as_ptr(&host.surface) as usize)?;
        host.notice
            .setStringValue(ns_string!("Esperando señal de video…"));
        host.notice.setHidden(false);
        Ok(())
    })
}

pub fn show_notice(message: Option<String>) {
    debug_assert!(MainThreadMarker::new().is_some());
    HOST.with(|value| {
        if let Some(host) = value.borrow().as_ref() {
            host.notice
                .setStringValue(&NSString::from_str(message.as_deref().unwrap_or("")));
            host.notice.setHidden(message.is_none());
        }
    });
}

pub fn show_placeholder() {
    show_notice(Some("Conecta el KVM para iniciar la transmisión".into()));
}

pub fn set_toolbar_visible(_window: &tauri::WebviewWindow, visible: bool) -> Result<(), String> {
    main_thread()?;
    HOST.with(|value| {
        if let Some(host) = value.borrow().as_ref() {
            if let Some(height) = &host.toolbar_height {
                height.setConstant(if visible { 260.0 } else { 0.0 });
            }
        }
    });
    Ok(())
}
