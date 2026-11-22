//! Frame format for the network transport: channel, length, and payload. Performs no input or output.
//!
//! Rules: `docs/architecture/technical_architecture.md` Section 1. The server and client
//! network modules move these frames over TCP sockets. Each frame is 1 channel byte, the
//! payload length as a little-endian `u32`, then the payload. This module only turns bytes
//! into frames and frames into bytes, so both sides share 1 tested codec.

use thiserror::Error;

/// Bytes before the payload: 1 channel byte and a 4-byte length.
pub const HEADER_BYTES: usize = 5;

/// Largest payload 1 frame may carry.
///
/// Placeholder: 1,048,576 bytes, far above any Milestone 1 message. Replaced when hosting is set up
/// (`docs/architecture/technical_architecture.md` Section 1).
pub const MAX_PAYLOAD_BYTES: u32 = 1 << 20;

/// Largest number of encoded bytes waiting in 1 [`FrameOutbox`] before the peer counts as stuck.
///
/// Placeholder: 8 times [`MAX_PAYLOAD_BYTES`]. Replaced when hosting is set up
/// (`docs/architecture/technical_architecture.md` Section 1).
pub const MAX_OUTBOX_BYTES: usize = 8 << 20;

/// 1 decoded frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    /// Channel the payload travels on, as numbered by the networking library.
    pub channel: u8,
    /// The message bytes.
    pub payload: Vec<u8>,
}

/// Why bytes could not become a frame, or a frame could not be queued.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum WireError {
    /// A frame declared or carried more payload than allowed.
    #[error("frame payload of {length} bytes exceeds the maximum of {maximum} bytes")]
    PayloadTooLarge {
        /// Payload length that was declared or offered.
        length: u64,
        /// Largest payload allowed.
        maximum: u32,
    },
    /// The outbox would grow past its limit because the peer is not reading.
    #[error("outbox would hold {length} unsent bytes, over the maximum of {maximum} bytes")]
    OutboxFull {
        /// Bytes the outbox would hold after the frame.
        length: usize,
        /// Largest number of bytes allowed.
        maximum: usize,
    },
}

/// Appends 1 encoded frame to `output`.
///
/// # Errors
///
/// Returns [`WireError::PayloadTooLarge`] when `payload` is longer than `max_payload_bytes`;
/// `output` is then unchanged.
pub fn encode_frame(
    channel: u8,
    payload: &[u8],
    max_payload_bytes: u32,
    output: &mut Vec<u8>,
) -> Result<(), WireError> {
    let too_large = || WireError::PayloadTooLarge {
        length: u64::try_from(payload.len()).unwrap_or(u64::MAX),
        maximum: max_payload_bytes,
    };
    let length = u32::try_from(payload.len()).map_err(|_| too_large())?;
    if length > max_payload_bytes {
        return Err(too_large());
    }
    output.reserve(HEADER_BYTES + payload.len());
    output.push(channel);
    output.extend_from_slice(&length.to_le_bytes());
    output.extend_from_slice(payload);
    Ok(())
}

/// Turns a byte stream that arrives in arbitrary pieces back into frames.
///
/// After the first error the stream position is lost, so every later call returns the same
/// error and the caller closes the connection.
#[derive(Clone, Debug)]
pub struct FrameDecoder {
    buffer: Vec<u8>,
    start: usize,
    max_payload_bytes: u32,
    failure: Option<WireError>,
}

impl FrameDecoder {
    /// Creates a decoder that rejects payloads longer than `max_payload_bytes`.
    pub fn new(max_payload_bytes: u32) -> Self {
        Self {
            buffer: Vec::new(),
            start: 0,
            max_payload_bytes,
            failure: None,
        }
    }

    /// Adds bytes received from the peer.
    pub fn push(&mut self, bytes: &[u8]) {
        if self.failure.is_some() {
            return;
        }
        // Dropping consumed bytes only here keeps decoding many small frames linear.
        if self.start > 0 {
            self.buffer.drain(..self.start);
            self.start = 0;
        }
        self.buffer.extend_from_slice(bytes);
    }

    /// Returns the next complete frame, or `None` until more bytes arrive.
    ///
    /// # Errors
    ///
    /// Yields [`WireError::PayloadTooLarge`] when a header declares a payload longer than the
    /// maximum, without waiting for that payload.
    pub fn next_frame(&mut self) -> Option<Result<Frame, WireError>> {
        if let Some(failure) = &self.failure {
            return Some(Err(failure.clone()));
        }
        let unread = &self.buffer[self.start..];
        let (&channel, rest) = unread.split_first()?;
        let length_bytes: [u8; 4] = rest.get(..4)?.try_into().ok()?;
        let length = u32::from_le_bytes(length_bytes);
        if length > self.max_payload_bytes {
            let failure = WireError::PayloadTooLarge {
                length: u64::from(length),
                maximum: self.max_payload_bytes,
            };
            self.failure = Some(failure.clone());
            self.buffer = Vec::new();
            self.start = 0;
            return Some(Err(failure));
        }
        let length = usize::try_from(length).ok()?;
        let payload = rest.get(4..length.checked_add(4)?)?.to_vec();
        self.start += HEADER_BYTES + length;
        Some(Ok(Frame { channel, payload }))
    }

    /// Bytes received but not yet returned as frames.
    pub fn buffered_bytes(&self) -> usize {
        self.buffer.len() - self.start
    }
}

impl Default for FrameDecoder {
    fn default() -> Self {
        Self::new(MAX_PAYLOAD_BYTES)
    }
}

/// Encoded frames waiting for the socket to accept them, so a partial write never splits or
/// drops a frame.
#[derive(Clone, Debug)]
pub struct FrameOutbox {
    buffer: Vec<u8>,
    start: usize,
    max_payload_bytes: u32,
    max_outbox_bytes: usize,
}

impl FrameOutbox {
    /// Creates an empty outbox with the given limits.
    pub fn new(max_payload_bytes: u32, max_outbox_bytes: usize) -> Self {
        Self {
            buffer: Vec::new(),
            start: 0,
            max_payload_bytes,
            max_outbox_bytes,
        }
    }

    /// Encodes 1 frame at the end of the outbox.
    ///
    /// # Errors
    ///
    /// Returns [`WireError::PayloadTooLarge`] for an oversized payload, or
    /// [`WireError::OutboxFull`] when the unsent bytes would exceed the outbox limit. The
    /// outbox is unchanged on error.
    pub fn queue(&mut self, channel: u8, payload: &[u8]) -> Result<(), WireError> {
        let length = self
            .pending()
            .len()
            .saturating_add(HEADER_BYTES)
            .saturating_add(payload.len());
        if length > self.max_outbox_bytes {
            return Err(WireError::OutboxFull {
                length,
                maximum: self.max_outbox_bytes,
            });
        }
        if self.start > 0 {
            self.buffer.drain(..self.start);
            self.start = 0;
        }
        encode_frame(channel, payload, self.max_payload_bytes, &mut self.buffer)
    }

    /// Bytes not yet written to the socket, oldest first.
    pub fn pending(&self) -> &[u8] {
        &self.buffer[self.start..]
    }

    /// Marks the first `written` pending bytes as sent.
    pub fn advance(&mut self, written: usize) {
        self.start = self.start.saturating_add(written).min(self.buffer.len());
        if self.start == self.buffer.len() {
            self.buffer.clear();
            self.start = 0;
        }
    }

    /// Whether every queued byte has been sent.
    pub fn is_empty(&self) -> bool {
        self.pending().is_empty()
    }
}

impl Default for FrameOutbox {
    fn default() -> Self {
        Self::new(MAX_PAYLOAD_BYTES, MAX_OUTBOX_BYTES)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encoded(channel: u8, payload: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        encode_frame(channel, payload, MAX_PAYLOAD_BYTES, &mut bytes).expect("payload fits");
        bytes
    }

    #[test]
    fn frame_layout_is_channel_then_little_endian_length_then_payload() {
        assert_eq!(encoded(3, b"abc"), [3, 3, 0, 0, 0, b'a', b'b', b'c']);
    }

    #[test]
    fn empty_payload_round_trips() {
        let mut decoder = FrameDecoder::default();
        decoder.push(&encoded(7, &[]));
        let frame = decoder.next_frame().expect("complete").expect("valid");
        assert_eq!(
            frame,
            Frame {
                channel: 7,
                payload: Vec::new()
            }
        );
        assert!(decoder.next_frame().is_none());
    }

    #[test]
    fn frames_split_at_every_byte_still_decode_in_order() {
        let mut stream = encoded(1, b"first");
        stream.extend(encoded(2, b"second frame"));
        let mut decoder = FrameDecoder::default();
        let mut frames = Vec::new();
        for byte in &stream {
            decoder.push(std::slice::from_ref(byte));
            while let Some(frame) = decoder.next_frame() {
                frames.push(frame.expect("valid"));
            }
        }
        assert_eq!(
            frames,
            [
                Frame {
                    channel: 1,
                    payload: b"first".to_vec()
                },
                Frame {
                    channel: 2,
                    payload: b"second frame".to_vec()
                },
            ]
        );
        assert_eq!(decoder.buffered_bytes(), 0);
    }

    #[test]
    fn several_frames_in_1_push_all_decode() {
        let mut stream = Vec::new();
        for channel in 0..10 {
            stream.extend(encoded(channel, &[channel; 3]));
        }
        let mut decoder = FrameDecoder::default();
        decoder.push(&stream);
        let channels: Vec<u8> = std::iter::from_fn(|| decoder.next_frame())
            .map(|frame| frame.expect("valid").channel)
            .collect();
        assert_eq!(channels, (0..10).collect::<Vec<u8>>());
    }

    #[test]
    fn incomplete_header_and_payload_wait_for_more_bytes() {
        let stream = encoded(4, b"payload");
        let mut decoder = FrameDecoder::default();
        decoder.push(&stream[..3]);
        assert!(decoder.next_frame().is_none());
        decoder.push(&stream[3..8]);
        assert!(decoder.next_frame().is_none());
        assert_eq!(decoder.buffered_bytes(), 8);
        decoder.push(&stream[8..]);
        assert!(decoder.next_frame().is_some());
    }

    #[test]
    fn oversized_declared_length_fails_without_waiting_for_the_payload() {
        let mut decoder = FrameDecoder::new(4);
        decoder.push(&[0, 5, 0, 0, 0]);
        let error = decoder.next_frame().expect("header complete");
        assert_eq!(
            error,
            Err(WireError::PayloadTooLarge {
                length: 5,
                maximum: 4
            })
        );
    }

    #[test]
    fn decoder_stays_failed_after_an_error() {
        let mut decoder = FrameDecoder::new(4);
        decoder.push(&[0, 255, 255, 255, 255]);
        assert!(matches!(decoder.next_frame(), Some(Err(_))));
        decoder.push(&encoded(1, b"ok"));
        assert!(matches!(decoder.next_frame(), Some(Err(_))));
    }

    #[test]
    fn encoding_an_oversized_payload_fails_and_leaves_output_unchanged() {
        let mut output = vec![9];
        let result = encode_frame(0, &[0; 5], 4, &mut output);
        assert_eq!(
            result,
            Err(WireError::PayloadTooLarge {
                length: 5,
                maximum: 4
            })
        );
        assert_eq!(output, [9]);
    }

    #[test]
    fn outbox_keeps_unsent_bytes_after_a_partial_write() {
        let mut outbox = FrameOutbox::default();
        outbox.queue(1, b"ab").expect("fits");
        outbox.advance(3);
        assert_eq!(outbox.pending(), [0, 0, b'a', b'b']);
        outbox.queue(2, b"c").expect("fits");
        assert_eq!(outbox.pending(), [0, 0, b'a', b'b', 2, 1, 0, 0, 0, b'c']);
        outbox.advance(10);
        assert!(outbox.is_empty());
    }

    #[test]
    fn outbox_rejects_frames_past_its_limit_and_stays_unchanged() {
        let mut outbox = FrameOutbox::new(MAX_PAYLOAD_BYTES, 12);
        outbox.queue(0, b"abc").expect("8 bytes fit");
        let result = outbox.queue(0, b"d");
        assert_eq!(
            result,
            Err(WireError::OutboxFull {
                length: 14,
                maximum: 12
            })
        );
        assert_eq!(outbox.pending().len(), 8);
    }

    #[test]
    fn outbox_advance_past_the_end_empties_it() {
        let mut outbox = FrameOutbox::default();
        outbox.queue(0, b"x").expect("fits");
        outbox.advance(100);
        assert!(outbox.is_empty());
    }
}
