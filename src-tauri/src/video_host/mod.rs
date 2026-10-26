//! Native video surfaces live on the UI thread; decoded frames stay in GStreamer.
use std::sync::Arc;

pub type CaptureCallback = Arc<dyn Fn() + Send + Sync>;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "linux")]
pub use linux::{
    attach, install, install_diagnostic, set_toolbar_visible, show_notice, show_placeholder,
};
#[cfg(target_os = "macos")]
pub use macos::{
    attach, install, install_diagnostic, set_toolbar_visible, show_notice, show_placeholder,
};
