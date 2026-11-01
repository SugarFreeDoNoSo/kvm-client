use std::{collections::VecDeque, time::Duration};

use bytes::{Bytes, BytesMut};
use thiserror::Error;

use crate::{
    error::Error,
    stream::{
        SunshineEncryption,
        proto::{
            DynCryptoBackend,
            crypto::{CryptoBackend, round_to_pkcs7_safe_len},
            microphone::foundation::packet::{
                FOUNDATION_MAX_MIC_PACKET_SIZE, FOUNDATION_MIC_HEADER_FLAGS, FOUNDATION_MIC_IV_LEN,
                FOUNDATION_MIC_MAGIC, FOUNDATION_MIC_PACKET_TYPE_OPUS, FoundationMicHeader,
            },
        },
    },
};

#[cfg(test)]
#[path = "./payloader_tests.rs"]
mod payloader_tests;

#[derive(Debug, Error)]
pub enum FoundationMicPayloaderError {
    #[error("the mic packet exceeded the maximum size of {FOUNDATION_MAX_MIC_PACKET_SIZE}")]
    PacketTooLarge,
}

impl From<FoundationMicPayloaderError> for Error {
    fn from(value: FoundationMicPayloaderError) -> Self {
        Self::Other(value.into())
    }
}

#[derive(Debug)]
pub struct FoundationMicPayloaderConfig {
    pub encryption: Option<SunshineEncryption>,
}

/// Foundation Extension
#[derive(Debug)]
pub struct FoundationMicPayloader {
    crypto_backend: DynCryptoBackend,
    config: FoundationMicPayloaderConfig,
    packets: VecDeque<Bytes>,
    sequence_number: u16,
}

impl FoundationMicPayloader {
    pub fn new(config: FoundationMicPayloaderConfig, crypto_backend: DynCryptoBackend) -> Self {
        Self {
            crypto_backend,
            config,
            packets: Default::default(),
            sequence_number: 0,
        }
    }

    pub fn push_frame(&mut self, timestamp: Duration, frame: &[u8]) -> Result<(), Error> {
        let safe_len = if self.config.encryption.is_some() {
            FoundationMicHeader::SIZE + round_to_pkcs7_safe_len(frame.len())
        } else {
            FoundationMicHeader::SIZE + frame.len()
        };

        let mut packet = BytesMut::zeroed(safe_len);

        let header = FoundationMicHeader {
            flags: FOUNDATION_MIC_HEADER_FLAGS,
            packet_type: FOUNDATION_MIC_PACKET_TYPE_OPUS,
            sequence_number: self.sequence_number,
            timestamp: timestamp.as_millis() as u32,
            ssrc: FOUNDATION_MIC_MAGIC,
        };

        // This won't panic because the size is correct
        #[allow(clippy::unwrap_used)]
        header.serialize(packet[0..FoundationMicHeader::SIZE].compat_mut_array().unwrap());

        if let Some(SunshineEncryption { aes_key, aes_iv }) = self.config.encryption {
            // See
            // https://github.com/Yundi339/moonlight-common-c/blob/f59424a9f7ad86f2b6278a4e2b07fb2902d8b090/src/MicrophoneStream.c#L96-L116

            let mut iv = [0; FOUNDATION_MIC_IV_LEN];
            iv[0..4].copy_from_slice(
                &aes_iv
                    .wrapping_add(self.sequence_number as u32)
                    .to_be_bytes(),
            );

            let len = self.crypto_backend.encrypt_aes_cbc(
                &aes_key,
                &iv,
                frame,
                &mut packet[FoundationMicHeader::SIZE..],
            )?;
            packet.truncate(FoundationMicHeader::SIZE + len);

            // Check bounds
            if packet.len() > FOUNDATION_MAX_MIC_PACKET_SIZE {
                return Err(FoundationMicPayloaderError::PacketTooLarge.into());
            }
        } else {
            // Check bounds
            if packet.len() > FOUNDATION_MAX_MIC_PACKET_SIZE {
                return Err(FoundationMicPayloaderError::PacketTooLarge.into());
            }

            // just plaintext copy
            packet[FoundationMicHeader::SIZE..].copy_from_slice(frame);
        }

        self.sequence_number = self.sequence_number.wrapping_add(1);

        self.packets.push_back(packet.freeze());

        Ok(())
    }

    pub fn poll_packet(&mut self) -> Option<Bytes> {
        self.packets.pop_front()
    }
}

use crate::compat::SliceArray;
