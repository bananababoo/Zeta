//! TCP sockets for the server network layer: a nonblocking listener and framed connections.
//!
//! Rules: `docs/architecture/technical_architecture.md` Section 1. Uses only the standard
//! library and [`zeta_core::wire`], so it is tested against plain sockets without the
//! networking library.

use std::io::{self, ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};

use thiserror::Error;
use zeta_core::wire::{Frame, FrameDecoder, FrameOutbox, WireError};

use crate::settings::MAX_RECEIVE_BYTES_PER_UPDATE;

/// Size of 1 socket read.
const READ_CHUNK_BYTES: usize = 16 * 1024;

/// Why a connection must be closed.
#[derive(Debug, Error)]
pub(crate) enum ConnectionError {
    /// The peer closed its side.
    #[error("the peer closed the connection")]
    Closed,
    /// The socket failed.
    #[error("socket error: {0}")]
    Socket(#[from] io::Error),
    /// The peer sent bytes that are not valid frames, or stopped reading.
    #[error(transparent)]
    Wire(#[from] WireError),
    /// The peer sent a frame on a channel the protocol does not define.
    #[error("the peer used unknown channel {0}")]
    UnknownChannel(u8),
    /// The protocol defines more channels than 1 frame header byte can name.
    #[error("channel {0} does not fit in 1 byte")]
    ChannelOutOfRange(usize),
}

/// A nonblocking listening socket.
#[derive(Debug)]
pub(crate) struct Listener(TcpListener);

impl Listener {
    /// Binds `address` without blocking later accepts.
    pub(crate) fn bind(address: SocketAddr) -> io::Result<Self> {
        let listener = TcpListener::bind(address)?;
        listener.set_nonblocking(true)?;
        Ok(Self(listener))
    }

    /// The bound address, which differs from the requested 1 when port 0 was asked for.
    pub(crate) fn local_address(&self) -> io::Result<SocketAddr> {
        self.0.local_addr()
    }

    /// Accepts every connection waiting right now. A connection that cannot be configured is
    /// dropped and reported, without stopping the listener.
    pub(crate) fn accept_pending(&self) -> (Vec<(Connection, SocketAddr)>, Vec<io::Error>) {
        let mut accepted = Vec::new();
        let mut errors = Vec::new();
        loop {
            match self.0.accept() {
                Ok((stream, peer)) => match Connection::new(stream) {
                    Ok(connection) => accepted.push((connection, peer)),
                    Err(error) => errors.push(error),
                },
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(error) => {
                    // Errors such as a peer resetting before accept concern 1 connection only.
                    errors.push(error);
                    break;
                }
            }
        }
        (accepted, errors)
    }
}

/// 1 framed, nonblocking TCP connection with an outbox for partial writes.
#[derive(Debug)]
pub(crate) struct Connection {
    stream: TcpStream,
    decoder: FrameDecoder,
    outbox: FrameOutbox,
}

impl Connection {
    /// Wraps `stream`, switching it to nonblocking mode with Nagle's delay off, because game
    /// messages are small and latency matters more than packet count.
    pub(crate) fn new(stream: TcpStream) -> io::Result<Self> {
        stream.set_nonblocking(true)?;
        stream.set_nodelay(true)?;
        Ok(Self {
            stream,
            decoder: FrameDecoder::default(),
            outbox: FrameOutbox::default(),
        })
    }

    /// Reads what the socket holds and appends every complete frame to `frames`.
    ///
    /// Frames that arrived before the peer closed are still appended before the error.
    pub(crate) fn receive(&mut self, frames: &mut Vec<Frame>) -> Result<(), ConnectionError> {
        let mut chunk = [0; READ_CHUNK_BYTES];
        let mut received = 0;
        let mut ending = None;
        while received < MAX_RECEIVE_BYTES_PER_UPDATE {
            match self.stream.read(&mut chunk) {
                Ok(0) => {
                    ending = Some(ConnectionError::Closed);
                    break;
                }
                Ok(count) => {
                    received += count;
                    self.decoder.push(&chunk[..count]);
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(error) => {
                    ending = Some(error.into());
                    break;
                }
            }
        }
        while let Some(frame) = self.decoder.next_frame() {
            frames.push(frame?);
        }
        ending.map_or(Ok(()), Err)
    }

    /// Queues 1 frame; [`Self::flush`] sends it.
    pub(crate) fn queue(&mut self, channel: u8, payload: &[u8]) -> Result<(), ConnectionError> {
        Ok(self.outbox.queue(channel, payload)?)
    }

    /// Writes as much of the outbox as the socket accepts without blocking.
    pub(crate) fn flush(&mut self) -> Result<(), ConnectionError> {
        while !self.outbox.is_empty() {
            match self.stream.write(self.outbox.pending()) {
                Ok(0) => return Err(ConnectionError::Closed),
                Ok(count) => self.outbox.advance(count),
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use zeta_core::wire::{MAX_PAYLOAD_BYTES, encode_frame};

    use super::*;

    const DEADLINE: Duration = Duration::from_secs(5);

    fn loopback_listener() -> (Listener, SocketAddr) {
        let listener = Listener::bind(SocketAddr::from(([127, 0, 0, 1], 0))).expect("binds");
        let address = listener.local_address().expect("has an address");
        (listener, address)
    }

    fn accept_one(listener: &Listener) -> Connection {
        let start = Instant::now();
        loop {
            let (mut accepted, errors) = listener.accept_pending();
            assert!(errors.is_empty(), "{errors:?}");
            if let Some((connection, _)) = accepted.pop() {
                return connection;
            }
            assert!(start.elapsed() < DEADLINE, "no connection accepted");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn receive_until(
        connection: &mut Connection,
        count: usize,
    ) -> (Vec<Frame>, Result<(), ConnectionError>) {
        let start = Instant::now();
        let mut frames = Vec::new();
        loop {
            let result = connection.receive(&mut frames);
            if result.is_err() || frames.len() >= count || start.elapsed() > DEADLINE {
                return (frames, result);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn frame(channel: u8, payload: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        encode_frame(channel, payload, MAX_PAYLOAD_BYTES, &mut bytes).expect("fits");
        bytes
    }

    #[test]
    #[ignore = "needs a loopback TCP connect, which the agent sandbox denies; run with --ignored"]
    fn frames_from_a_raw_peer_arrive_whole_even_when_written_in_pieces() {
        let (listener, address) = loopback_listener();
        let mut peer = TcpStream::connect(address).expect("connects");
        let mut connection = accept_one(&listener);

        let bytes = [frame(2, b"hello"), frame(5, b"world")].concat();
        peer.write_all(&bytes[..3]).expect("writes");
        peer.flush().expect("flushes");
        std::thread::sleep(Duration::from_millis(20));
        peer.write_all(&bytes[3..]).expect("writes");

        let (frames, result) = receive_until(&mut connection, 2);
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(
            frames,
            [
                Frame {
                    channel: 2,
                    payload: b"hello".to_vec()
                },
                Frame {
                    channel: 5,
                    payload: b"world".to_vec()
                },
            ]
        );
    }

    #[test]
    #[ignore = "needs a loopback TCP connect, which the agent sandbox denies; run with --ignored"]
    fn queued_frames_reach_the_raw_peer_in_order() {
        let (listener, address) = loopback_listener();
        let mut peer = TcpStream::connect(address).expect("connects");
        let mut connection = accept_one(&listener);

        connection.queue(1, b"first").expect("queues");
        connection.queue(3, b"second").expect("queues");
        connection.flush().expect("flushes");

        let expected = [frame(1, b"first"), frame(3, b"second")].concat();
        let mut received = vec![0; expected.len()];
        peer.set_read_timeout(Some(DEADLINE)).expect("sets timeout");
        peer.read_exact(&mut received).expect("reads");
        assert_eq!(received, expected);
    }

    #[test]
    #[ignore = "needs a loopback TCP connect, which the agent sandbox denies; run with --ignored"]
    fn a_large_payload_survives_partial_writes() {
        let (listener, address) = loopback_listener();
        let mut peer = TcpStream::connect(address).expect("connects");
        let mut connection = accept_one(&listener);

        // Larger than typical socket buffers, so the first flush cannot write it all.
        let payload: Vec<u8> = (0..MAX_PAYLOAD_BYTES)
            .map(|index| index.to_le_bytes()[0])
            .collect();
        connection.queue(4, &payload).expect("queues");
        let reader = std::thread::spawn(move || {
            peer.set_read_timeout(Some(DEADLINE)).expect("sets timeout");
            let mut received = vec![0; payload.len() + zeta_core::wire::HEADER_BYTES];
            peer.read_exact(&mut received).expect("reads");
            (received, payload)
        });
        let start = Instant::now();
        while !connection.outbox.is_empty() {
            connection.flush().expect("flushes");
            assert!(start.elapsed() < DEADLINE, "outbox never drained");
            std::thread::sleep(Duration::from_millis(1));
        }
        let (received, payload) = reader.join().expect("reader finishes");
        assert_eq!(received[0], 4);
        assert_eq!(
            &received[zeta_core::wire::HEADER_BYTES..],
            payload.as_slice()
        );
    }

    #[test]
    #[ignore = "needs a loopback TCP connect, which the agent sandbox denies; run with --ignored"]
    fn peer_closing_reports_closed_after_delivering_earlier_frames() {
        let (listener, address) = loopback_listener();
        let mut peer = TcpStream::connect(address).expect("connects");
        let mut connection = accept_one(&listener);

        peer.write_all(&frame(0, b"last words")).expect("writes");
        drop(peer);

        let (frames, result) = receive_until(&mut connection, usize::MAX);
        assert_eq!(frames.len(), 1);
        assert!(matches!(result, Err(ConnectionError::Closed)), "{result:?}");
    }

    #[test]
    #[ignore = "needs a loopback TCP connect, which the agent sandbox denies; run with --ignored"]
    fn an_oversized_frame_header_is_a_wire_error() {
        let (listener, address) = loopback_listener();
        let mut peer = TcpStream::connect(address).expect("connects");
        let mut connection = accept_one(&listener);

        peer.write_all(&[0, 255, 255, 255, 255]).expect("writes");

        let (frames, result) = receive_until(&mut connection, 1);
        assert_eq!(frames, Vec::new());
        assert!(
            matches!(result, Err(ConnectionError::Wire(_))),
            "{result:?}"
        );
    }
}
