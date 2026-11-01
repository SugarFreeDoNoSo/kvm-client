//! This module contains all the necessary parts for the Foundation Sunshine microphone protocol

// Can't read chinese but this seems important
// Rtsp Mic: https://github.com/AlkaidLab/foundation-sunshine/blob/master/src/rtsp.cpp#L916
// Mic receive: https://github.com/AlkaidLab/foundation-sunshine/blob/013388962e547698b34a1e6087f44b1ec2b58d17/src/stream.cpp#L1659-L1925
// Client impl: https://github.com/moonlight-stream/moonlight-common-c/pull/123/changes

use std::{convert::Infallible, net::SocketAddr, time::Duration};

use sans_io_time::Instant;

use tracing::{Level, instrument};

use crate::{
    error::Error,
    stream::{
        SunshineEncryption,
        proto::{
            DynCryptoBackend,
            microphone::foundation::payloader::{
                FoundationMicPayloader, FoundationMicPayloaderConfig,
            },
            runtime::{Receive, Transmit, UdpStream},
        },
    },
};

/// References:
/// - <https://github.com/qiin2333/moonlight-common-c/blob/7ed14144d1aef1d6d234ea98b17eedc083a5ac36/src/RtspConnection.c#L1438-L1439>
pub const FOUNDATION_DEFAULT_MIC_PORT: u16 = 47996;

pub mod packet;
pub mod payloader;
pub mod rtsp;

#[derive(Debug)]
pub struct FoundationMicStreamConfig {
    pub addr: SocketAddr,
    /// If [Some] the mic stream is encrypted.
    pub encryption: Option<SunshineEncryption>,
}

#[derive(Debug)]
pub struct FoundationMicStream {
    addr: SocketAddr,
    payloader: FoundationMicPayloader,
}

impl FoundationMicStream {
    #[instrument(level = Level::DEBUG, skip(crypto_backend))]
    pub fn new(
        now: Instant,
        config: FoundationMicStreamConfig,
        crypto_backend: DynCryptoBackend,
    ) -> Self {
        Self {
            addr: config.addr,
            payloader: FoundationMicPayloader::new(
                FoundationMicPayloaderConfig {
                    encryption: config.encryption,
                },
                crypto_backend,
            ),
        }
    }

    pub fn send_microphone_opus_data(
        &mut self,
        timestamp: Duration,
        frame: &[u8],
    ) -> Result<(), Error> {
        self.payloader.push_frame(timestamp, frame)?;

        Ok(())
    }
}

impl UdpStream for FoundationMicStream {
    type Event = Infallible;

    fn poll_transmit(&mut self) -> Option<Transmit> {
        if let Some(data) = self.payloader.poll_packet() {
            Some(Transmit {
                destination: self.addr,
                data,
            })
        } else {
            None
        }
    }

    fn poll_timeout(&self) -> Option<Instant> {
        None
    }

    fn poll_event(&mut self) -> Option<Self::Event> {
        None
    }

    fn handle_receive(&mut self, _now: Instant, _receive: Receive) -> Result<(), Error> {
        Ok(())
    }

    fn handle_timeout(&mut self, _now: Instant) -> Result<(), Error> {
        Ok(())
    }
}
