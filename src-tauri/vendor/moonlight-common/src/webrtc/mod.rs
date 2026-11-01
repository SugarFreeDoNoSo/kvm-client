//! This module contains common types and functions needed for moonlight game streaming over webrtc.
//! It doesn't contain a full webrtc implementation.
//!
#![doc = include_str!("./protocol.md")]

use sdp_types::{Attribute, Session};

pub use sdp_types as sdp;

pub mod answer;
pub mod header;
pub mod offer;

fn push(session: &mut Session, attribute: impl Into<String>, value: impl Into<String>) {
    session.attributes.push(Attribute {
        attribute: attribute.into(),
        value: Some(value.into()),
    });
}

fn bool_to_number_str(v: bool) -> &'static str {
    if v { "1" } else { "0" }
}
