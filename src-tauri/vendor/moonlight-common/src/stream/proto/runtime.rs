use std::net::SocketAddr;

use bytes::Bytes;
use sans_io_time::Instant;

use crate::error::Error;

#[derive(Debug, PartialEq)]
pub struct Transmit {
    pub destination: SocketAddr,
    pub data: Bytes,
}

#[derive(Debug, PartialEq)]
pub struct Receive<'a> {
    pub source: SocketAddr,
    pub data: &'a [u8],
}

pub trait UdpStream: Send + Sync {
    type Event;

    fn poll_transmit(&mut self) -> Option<Transmit>;

    fn poll_timeout(&self) -> Option<Instant>;

    fn poll_event(&mut self) -> Option<Self::Event>;

    fn handle_receive(&mut self, now: Instant, receive: Receive) -> Result<(), Error>;

    fn handle_timeout(&mut self, now: Instant) -> Result<(), Error>;

    /// Allows for setting an os hint for the receive buffer size of the udp socket.
    fn recv_buffer_hint(&self) -> Option<usize> {
        None
    }
}
