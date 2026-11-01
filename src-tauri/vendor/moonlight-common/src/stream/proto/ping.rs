use std::{collections::VecDeque, time::Duration};

use bytes::Bytes;
use sans_io_time::Instant;

use tracing::{Level, instrument, trace};

use crate::stream::proto::packet::{SunshinePing, SunshinePingPacket};

const PING_RETRY_TIMEOUT: Duration = Duration::from_millis(500);
const LEGACY_PING: &[u8] = &[0x50, 0x49, 0x4E, 0x47];

#[cfg(test)]
#[path = "./ping_tests.rs"]
mod ping_tests;

#[derive(Debug)]
pub struct PingSenderConfig {
    pub sunshine_ping: Option<SunshinePing>,
}

#[derive(Debug)]
pub struct PingSender {
    now: Instant,
    next_ping_send: Instant,
    config: PingSenderConfig,
    next_attempt: u32,
    transmits: VecDeque<Bytes>,
}

impl PingSender {
    #[instrument(level = Level::DEBUG)]
    pub fn new(now: Instant, config: PingSenderConfig) -> Self {
        let mut this = Self {
            now,
            next_ping_send: now,
            config,
            next_attempt: 0,
            transmits: Default::default(),
        };
        this.advance_packet();

        this
    }

    fn push_packet(&mut self, sequence_number: u32) {
        let mut bytes = [0; SunshinePingPacket::SIZE];

        let packet_len = if let Some(ping) = self.config.sunshine_ping.as_ref() {
            // Use Sunshine ping
            let packet = SunshinePingPacket {
                payload: ping.clone(),
                sequence_number,
            };

            packet.serialize(&mut bytes);
            SunshinePingPacket::SIZE
        } else {
            // Just some magic bytes
            let ping = LEGACY_PING;

            bytes[0..ping.len()].copy_from_slice(ping);
            ping.len()
        };

        let transmit = &bytes[0..packet_len];
        self.transmits.push_back(Bytes::copy_from_slice(transmit));

        trace!(packet = ?bytes, "sending ping");
    }

    fn advance_packet(&mut self) {
        // Advance next ping send
        self.next_ping_send += PING_RETRY_TIMEOUT;

        // Overwrite current ping buffer with the new packet
        self.push_packet(self.next_attempt);

        // Advance attempt
        self.next_attempt += 1;
    }

    pub fn poll_timeout(&self) -> Option<Instant> {
        Some(self.next_ping_send)
    }

    pub fn poll_transmit(&mut self) -> Option<Bytes> {
        self.transmits.pop_front()
    }

    pub fn handle_timeout(&mut self, now: Instant) {
        self.now = now;

        // Check if we've reached the timeout
        if self.now < self.next_ping_send {
            return;
        }

        self.advance_packet();
    }
}
