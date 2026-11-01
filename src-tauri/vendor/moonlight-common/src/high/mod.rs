//!
//! The high level api of this crate for easy usage.
//!

#[cfg(feature = "std")]
pub mod std;

#[cfg(feature = "tokio")]
pub mod tokio;
