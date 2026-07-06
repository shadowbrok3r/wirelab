//! Frame codec: postcard payload + CRC16 + COBS + 0x00 terminator.

use core::marker::PhantomData;

use crc::{CRC_16_IBM_SDLC, Crc};
use serde::{Serialize, de::DeserializeOwned};

use crate::{MAX_FRAME, MAX_PAYLOAD};

const CRC16: Crc<u16> = Crc::<u16>::new(&CRC_16_IBM_SDLC);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    /// Message too large for the payload buffer.
    Overflow,
    /// Postcard serialization/deserialization failure.
    Codec,
    /// COBS decode failure.
    Cobs,
    /// CRC mismatch.
    Crc,
    /// Frame shorter than the CRC trailer.
    TooShort,
}

/// Encode one message into `out`, returning the number of bytes written
/// (including the trailing 0x00 terminator).
pub fn encode<T: Serialize>(msg: &T, out: &mut [u8]) -> Result<usize, FrameError> {
    let mut payload = [0u8; MAX_PAYLOAD + 2];
    let used = postcard::to_slice(msg, &mut payload[..MAX_PAYLOAD])
        .map_err(|_| FrameError::Overflow)?
        .len();
    let crc = CRC16.checksum(&payload[..used]).to_le_bytes();
    payload[used] = crc[0];
    payload[used + 1] = crc[1];
    let encoded = cobs::encode(&payload[..used + 2], out);
    if encoded + 1 > out.len() {
        return Err(FrameError::Overflow);
    }
    out[encoded] = 0x00;
    Ok(encoded + 1)
}

/// Incremental frame decoder; feed bytes, yields a message per complete frame.
pub struct Decoder<T> {
    buf: [u8; MAX_FRAME],
    len: usize,
    overrun: bool,
    _msg: PhantomData<T>,
}

impl<T: DeserializeOwned> Decoder<T> {
    pub const fn new() -> Self {
        Decoder { buf: [0; MAX_FRAME], len: 0, overrun: false, _msg: PhantomData }
    }

    pub fn push(&mut self, byte: u8) -> Option<Result<T, FrameError>> {
        if byte != 0x00 {
            if self.len < MAX_FRAME {
                self.buf[self.len] = byte;
                self.len += 1;
            } else {
                self.overrun = true;
            }
            return None;
        }
        let len = self.len;
        let overrun = self.overrun;
        self.len = 0;
        self.overrun = false;
        if len == 0 {
            // Bare sentinel: idle line or resync marker.
            return None;
        }
        if overrun {
            return Some(Err(FrameError::Overflow));
        }
        Some(decode_frame(&mut self.buf[..len]))
    }
}

impl<T: DeserializeOwned> Default for Decoder<T> {
    fn default() -> Self {
        Self::new()
    }
}

fn decode_frame<T: DeserializeOwned>(frame: &mut [u8]) -> Result<T, FrameError> {
    let report = cobs::decode_in_place_report(frame).map_err(|_| FrameError::Cobs)?;
    let n = report.frame_size();
    if n < 2 {
        return Err(FrameError::TooShort);
    }
    let (payload, crc_bytes) = frame[..n].split_at(n - 2);
    let expected = u16::from_le_bytes([crc_bytes[0], crc_bytes[1]]);
    if CRC16.checksum(payload) != expected {
        return Err(FrameError::Crc);
    }
    postcard::from_bytes(payload).map_err(|_| FrameError::Codec)
}
