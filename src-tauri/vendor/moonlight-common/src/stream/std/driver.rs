use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant as StdInstant};
use std::{io, thread};

use sans_io_time::Instant;
use tracing::{Level, Span, debug, instrument, trace};

use crate::error::Error;
use crate::stream::proto::runtime::{Receive, UdpStream};
use crate::stream::sockets::new_udp_socket;
use crate::stream::std::{finalize_errors, join_run_thread};

const UDP_BUFFER_CAPACITY: usize = 4096;

pub struct SyncUdpDriver<Stream> {
    socket: UdpSocket,
    base_time: StdInstant,
    stream_condvar: Condvar,
    stream: Mutex<Stream>,
    stopped: AtomicBool,
}

impl<Stream> SyncUdpDriver<Stream>
where
    Stream: UdpStream,
{
    pub fn bind(base_time: StdInstant, stream: Stream) -> Result<Self, io::Error> {
        let socket = new_udp_socket(false, stream.recv_buffer_hint())?;

        Ok(Self {
            socket,
            base_time,
            stream_condvar: Condvar::new(),
            stream: Mutex::new(stream),
            stopped: AtomicBool::new(false),
        })
    }

    #[instrument(level = Level::TRACE, skip(self, f))]
    pub fn stream_mut<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut Stream) -> R,
    {
        let mut guard = self.stream.lock().expect("lock stream failed");
        let res = f(&mut guard);
        self.stream_condvar.notify_all();
        res
    }
    #[instrument(level = Level::TRACE, skip(self, f))]
    pub fn stream<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&Stream) -> R,
    {
        let mut guard = self.stream.lock().expect("lock stream failed");
        f(&mut guard)
    }

    pub fn run(&self) -> Result<(), Error> {
        if self.is_stopped() {
            return Ok(());
        }

        let span = Span::current();

        thread::scope::<_, Result<(), Error>>(|scope| {
            debug!("starting udp driver threads");

            let send =
                scope.spawn(|| span.in_scope(|| self.blocking_send().inspect_err(|_| self.stop())));
            let recv =
                scope.spawn(|| span.in_scope(|| self.blocking_recv().inspect_err(|_| self.stop())));
            let timeout = scope
                .spawn(|| span.in_scope(|| self.blocking_timeout().inspect_err(|_| self.stop())));

            let mut errors = Default::default();

            join_run_thread(&mut errors, send.join());
            join_run_thread(&mut errors, recv.join());
            join_run_thread(&mut errors, timeout.join());

            finalize_errors(errors)?;

            Ok(())
        })?;

        self.stop();

        Ok(())
    }

    #[instrument(level = Level::TRACE, skip(self))]
    fn blocking_send(&self) -> Result<(), Error> {
        debug!("started sending thread");

        // This handles sending packets

        let mut stream = self.stream.lock().expect("lock stream failed");

        self.socket
            .set_write_timeout(Some(Duration::from_secs(1)))?;
        loop {
            if self.is_stopped() {
                break;
            }

            let transmit = if let Some(transmit) = stream.poll_transmit() {
                #[cfg(debug_assertions)]
                trace!(transmit = ?transmit, "got pending sending buffer");

                transmit
            } else {
                // Wait for pending packet
                #[cfg(debug_assertions)]
                trace!("waiting for change of stream");
                stream = self.stream_condvar.wait(stream).expect("wait on stream");
                continue;
            };
            drop(stream);

            #[cfg(debug_assertions)]
            trace!("sending packet");
            // Send packet
            match self.socket.send_to(&transmit.data, transmit.destination) {
                Ok(_) => {
                    #[cfg(debug_assertions)]
                    trace!(transmit = ?transmit, "successfully sent packet");
                }
                Err(err)
                    if matches!(
                        err.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    if self.is_stopped() {
                        return Ok(());
                    }
                }
                Err(err) => return Err(err.into()),
            }
            #[cfg(debug_assertions)]
            trace!("finished sending packet");

            // Re-lock stream
            stream = self.stream.lock().expect("lock stream failed");
        }

        Ok(())
    }

    #[instrument(level = Level::TRACE, skip(self))]
    fn blocking_recv(&self) -> Result<(), Error> {
        debug!("started receiving thread");

        // This handles receiving packets

        let mut buffer = vec![0; UDP_BUFFER_CAPACITY];

        self.socket.set_read_timeout(Some(Duration::from_secs(1)))?;
        loop {
            let (len, addr) = match self.socket.recv_from(&mut buffer) {
                Ok(value) => value,
                Err(err)
                    if matches!(
                        err.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    if self.is_stopped() {
                        return Ok(());
                    }
                    continue;
                }
                Err(err) => return Err(err.into()),
            };

            if self.is_stopped() {
                break;
            }

            #[cfg(debug_assertions)]
            trace!(packet = ?buffer[0..len], "received packet");

            let mut stream = self.stream.lock().expect("lock stream failed");
            stream.handle_receive(
                Instant::from_std(self.base_time),
                Receive {
                    source: addr,
                    data: &buffer[0..len],
                },
            )?;

            self.stream_condvar.notify_all();
        }

        Ok(())
    }

    #[instrument(level = Level::TRACE, skip(self))]
    fn blocking_timeout(&self) -> Result<(), Error> {
        debug!("started timeout thread");

        // This handles timeouts

        let mut stream = self.stream.lock().expect("lock stream failed");
        loop {
            if self.is_stopped() {
                break;
            }

            let deadline = stream.poll_timeout().map(|x| x.to_std(self.base_time));

            if let Some(deadline) = deadline {
                let timeout = deadline
                    .checked_duration_since(StdInstant::now())
                    // Make sure that the other threads can have access
                    .unwrap_or(Duration::from_millis(1));

                #[cfg(debug_assertions)]
                trace!(timeout = ?timeout, "waiting on stream");

                let (new_stream, result) = self
                    .stream_condvar
                    .wait_timeout(stream, timeout)
                    .expect("wait on stream");
                stream = new_stream;

                if result.timed_out() {
                    #[cfg(debug_assertions)]
                    trace!("handling timeout");

                    stream.handle_timeout(Instant::from_std(self.base_time))?;
                    self.stream_condvar.notify_all();
                } else {
                    #[cfg(debug_assertions)]
                    trace!("not handling timeout because the timeout wasn't reached");
                }
            } else {
                #[cfg(debug_assertions)]
                trace!("waiting on stream without timeout");

                stream = self.stream_condvar.wait(stream).expect("wait on stream");
            }
        }

        Ok(())
    }

    /// Returns [None] if the stream was stopped
    pub fn blocking_next_event(&self) -> Option<Stream::Event> {
        let mut stream = self.stream.lock().expect("lock stream failed");

        loop {
            if self.is_stopped() {
                return None;
            }

            if let Some(event) = stream.poll_event() {
                return Some(event);
            } else {
                #[cfg(debug_assertions)]
                trace!("no events found");
            }

            #[cfg(debug_assertions)]
            trace!("waiting on stream for events");
            stream = self
                .stream_condvar
                .wait(stream)
                .expect("wait on stream failed");
        }
    }
}

impl<Stream> SyncUdpDriver<Stream> {
    pub fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }

    pub fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
        self.stream_condvar.notify_all();
    }
}
