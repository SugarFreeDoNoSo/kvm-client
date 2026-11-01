use std::{
    collections::VecDeque,
    convert::Infallible,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};

use bytes::Bytes;
use rusty_enet::{
    Address, Event, Host, HostSettings, MTU_MAX, PacketReceived, Peer, PeerID, ReadWrite, Socket,
    SocketOptions,
};
use sans_io_time::Instant;
use tracing::{debug, trace};

use crate::{
    error::Error,
    stream::{control::PacketSendError, proto::runtime::Transmit},
};

// TODO: dynamically set timeout, see https://github.com/jabuwu/rusty_enet/issues/4
// TODO: this seems interesting: https://github.com/zpl-c/enet/blob/8647b6eaea881c86471ae29f732620d299fc20d7/include/enet.h#L296-L488

impl From<rusty_enet::error::HostNewError<ReadWrite<SocketAddr, Infallible>>> for Error {
    fn from(value: rusty_enet::error::HostNewError<ReadWrite<SocketAddr, Infallible>>) -> Self {
        match value {
            rusty_enet::error::HostNewError::BadParameter(parameter) => {
                Error::Other(parameter.into())
            }
            rusty_enet::error::HostNewError::FailedToInitializeSocket(_) => unreachable!(),
        }
    }
}

impl From<rusty_enet::error::PeerSendError> for Error {
    fn from(value: rusty_enet::error::PeerSendError) -> Self {
        use rusty_enet::error::PeerSendError;

        match value {
            PeerSendError::NotConnected => Error::PacketSend(PacketSendError::PeerNotConnected),
            value => Error::Other(value.into()),
        }
    }
}

impl From<rusty_enet::error::NoAvailablePeers> for Error {
    fn from(value: rusty_enet::error::NoAvailablePeers) -> Self {
        Self::Other(value.into())
    }
}

pub struct EnetConfig {
    pub peer_count: usize,
    pub channel_limit: usize,
    pub incoming_bandwidth: Option<usize>,
    pub outgoing_bandwidth: Option<usize>,
}

#[derive(Debug)]
pub enum EnetEvent {
    Connect {
        peer: PeerID,
        data: u32,
    },
    Receive {
        peer: PeerID,
        channel_id: u8,
        data: Vec<u8>,
    },
    Disconnect {
        peer: PeerID,
        #[allow(unused)]
        data: u32,
    },
}

pub struct EnetHost {
    last_now: Arc<Mutex<Instant>>,
    pub(crate) enet: Host<Io<SocketAddr>>,
    events: VecDeque<EnetEvent>,
}

impl EnetHost {
    pub fn new(now: Instant, config: EnetConfig) -> Self {
        let last_now = Arc::new(Mutex::new(now));

        // This unwrap is safe because those settings don't fail, which would be the only error source
        #[allow(clippy::unwrap_used)]
        let enet = Host::new(
            Io::<SocketAddr>::default(),
            HostSettings {
                peer_limit: config.peer_count,
                channel_limit: config.channel_limit,
                incoming_bandwidth_limit: config.incoming_bandwidth.map(|x| x as u32),
                outgoing_bandwidth_limit: config.outgoing_bandwidth.map(|x| x as u32),
                time: {
                    let start = now;
                    let last_now = last_now.clone();
                    Box::new(move || {
                        let last_now = {
                            // This is allowed because we:
                            // 1. only we have got access to this mutex
                            // 2. don't panic inside this implementation
                            #[allow(clippy::unwrap_used)]
                            let last_now = last_now.lock().unwrap();

                            *last_now
                        };

                        last_now - start
                    })
                },
                ..Default::default()
            },
        )
        .unwrap();

        Self {
            last_now,
            enet,
            events: Default::default(),
        }
    }

    pub fn connect(
        &mut self,
        addr: SocketAddr,
        channel_count: usize,
        data: u32,
    ) -> Result<PeerID, Error> {
        debug!(remote_addr = ?addr, connect_data = ?data, "enet starting connect");

        let peer = self.enet.connect(addr, channel_count, data)?;

        Ok(peer.id())
    }

    pub fn disconnect_later(&mut self, id: PeerID, data: u32) -> Result<(), Error> {
        self.enet
            .get_peer_mut(id)
            .ok_or(Error::PacketSend(PacketSendError::PeerNotFound))?
            .disconnect_later(data);

        Ok(())
    }
    pub fn disconnect_now(&mut self, id: PeerID, data: u32) -> Result<(), Error> {
        self.enet
            .get_peer_mut(id)
            .ok_or(Error::PacketSend(PacketSendError::PeerNotFound))?
            .disconnect_now(data);

        Ok(())
    }

    pub fn peer_mut(&mut self, id: PeerID) -> Option<&mut Peer<impl Socket<Address = SocketAddr>>> {
        self.enet.get_peer_mut(id)
    }
    pub fn peer(&self, id: PeerID) -> Option<&Peer<impl Socket<Address = SocketAddr>>> {
        self.enet.get_peer(id)
    }

    pub fn poll_transmit(&mut self) -> Option<Transmit> {
        self.enet
            .socket_mut()
            .outbound
            .pop_front()
            .map(|(destination, data)| Transmit { destination, data })
    }

    pub fn poll_timeout(&self) -> Instant {
        self.last_now() + Duration::from_millis(50)
    }

    pub fn poll_event(&mut self) -> Option<EnetEvent> {
        self.events.pop_front()
    }

    pub fn handle_receive(&mut self, now: Instant, addr: SocketAddr, data: &[u8]) {
        self.enet
            .socket_mut()
            .inbound
            .push_back((addr, data.to_vec()));

        self.handle_timeout(now);
    }

    pub fn handle_timeout(&mut self, now: Instant) {
        self.set_last_now(now);

        self.service();
    }

    /// Must be called after each modification to update all state
    pub fn service(&mut self) {
        // The error is infallible and cannot be constructed
        // -> we are allowed to unwrap
        #[allow(clippy::unwrap_used)]
        while let Some(event) = self.enet.service().unwrap() {
            trace!(event = ?event, "enet service event");

            let event = match event {
                Event::Connect { peer, data } => {
                    debug!(peer_id = ?peer.id(), connect_data = ?data, "enet peer connected");

                    EnetEvent::Connect {
                        peer: peer.id(),
                        data,
                    }
                }
                Event::Receive {
                    peer,
                    channel_id,
                    packet,
                } => {
                    trace!(peer_id = ?peer.id(), channel_id = ?channel_id, packet = ?packet, "enet received packet");

                    EnetEvent::Receive {
                        peer: peer.id(),
                        channel_id,
                        data: packet.data().to_vec(),
                    }
                }
                Event::Disconnect { peer, data } => {
                    debug!(peer_id = ?peer.id(), disconnect_data = ?data, "enet peer disconnected");

                    EnetEvent::Disconnect {
                        peer: peer.id(),
                        data,
                    }
                }
            };

            self.events.push_back(event);
        }
    }

    fn set_last_now(&self, now: Instant) {
        // This is allowed because we:
        // 1. only we have got access to this mutex
        // 2. don't panic inside this implementation
        #[allow(clippy::unwrap_used)]
        let mut last_now = self.last_now.lock().unwrap();

        *last_now = now;
    }
    fn last_now(&self) -> Instant {
        // This is allowed because we:
        // 1. only we have got access to this mutex
        // 2. don't panic inside this implementation
        #[allow(clippy::unwrap_used)]
        let last_now = self.last_now.lock().unwrap();

        *last_now
    }
}

#[derive(Debug)]
pub(crate) struct Io<A> {
    inbound: VecDeque<(A, Vec<u8>)>,
    outbound: VecDeque<(A, Bytes)>,
}

impl<A> Default for Io<A> {
    fn default() -> Self {
        Self {
            inbound: Default::default(),
            outbound: Default::default(),
        }
    }
}

impl<A: Address + 'static> Socket for Io<A> {
    type Address = A;
    type Error = Infallible;

    fn init(&mut self, _socket_options: SocketOptions) -> Result<(), Self::Error> {
        // NOTE: this implementation must not become fallable
        Ok(())
    }

    fn send(&mut self, address: A, buffer: &[u8]) -> Result<usize, Infallible> {
        self.outbound
            .push_back((address, Bytes::copy_from_slice(buffer)));
        Ok(buffer.len())
    }

    fn receive(
        &mut self,
        buffer: &mut [u8; MTU_MAX],
    ) -> Result<Option<(A, PacketReceived)>, Infallible> {
        if let Some((address, inbound)) = self.inbound.pop_front() {
            let bytes = inbound.len();
            if bytes <= MTU_MAX {
                #[cfg(feature = "std")]
                {
                    use std::io::{Cursor, copy};
                    copy(&mut Cursor::new(inbound), &mut Cursor::new(&mut buffer[..]))
                        .expect("Buffer copy should not fail.");
                }
                #[cfg(not(feature = "std"))]
                unsafe {
                    use core::ptr::copy_nonoverlapping;
                    copy_nonoverlapping(inbound.as_ptr(), buffer.as_mut_ptr(), bytes);
                }
                Ok(Some((address, PacketReceived::Complete(bytes))))
            } else {
                Ok(None)
            }
        } else {
            Ok(None)
        }
    }
}
