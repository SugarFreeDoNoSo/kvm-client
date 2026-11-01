//! Native video surfaces live on the UI thread; decoded frames stay in GStreamer.
use std::sync::Arc;

pub type CaptureCallback = Arc<dyn Fn() + Send + Sync>;

const DASHBOARD_COLLAPSED_WIDTH: i32 = 0;
const DASHBOARD_EXPANDED_WIDTH: i32 = 360;

fn dashboard_width(expanded: bool) -> i32 {
    if expanded {
        DASHBOARD_EXPANDED_WIDTH
    } else {
        DASHBOARD_COLLAPSED_WIDTH
    }
}

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "linux")]
pub use linux::{
    attach, attach_sunshine, install, install_diagnostic, is_dashboard_fullscreen, select_sunshine,
    self_test, set_dashboard_expanded, set_dashboard_fullscreen, set_toolbar_visible, show_notice,
    show_placeholder,
};
#[cfg(target_os = "macos")]
pub use macos::{
    attach, attach_sunshine, install, install_diagnostic, is_dashboard_fullscreen, select_sunshine,
    self_test, set_dashboard_expanded, set_dashboard_fullscreen, set_toolbar_visible, show_notice,
    show_placeholder,
};
