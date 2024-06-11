use crate::{connection::MAX_MESSAGE_SIZE, util::AdbError, util::Result};
use tokio_util::{bytes::{Buf, BytesMut}, codec::Decoder};

const ADB_SHELL_REQUEST_HEADER_LENGTH: usize = 5;
pub(crate) enum AdbShellResponseId {
    Stdin = 0,
    Stdout = 1,
    Stderr = 2,
    Exit = 3,
    // Close subprocess stdin if possible.
    CloseStdin = 4,
    // Window size change (an ASCII version of struct winsize).
    WindowSizeChange = 5,
    // Indicates an invalid or unknown packet.
    Invalid = 255,
}

impl AdbShellResponseId {
    fn from_u8(value: u8) -> Self {
        match value {
            0 => Self::Stdin,
            1 => Self::Stdout,
            2 => Self::Stderr,
            3 => Self::Exit,
            4 => Self::CloseStdin,
            5 => Self::WindowSizeChange,
            _ => Self::Invalid,
        }
    }
}

pub(crate) struct AdbShellPacket {
    pub(crate) id: AdbShellResponseId,
    pub(crate) response: Vec<u8>,
}

pub(crate) struct AdbShellDecoder {}

impl AdbShellDecoder {
    pub fn new() -> AdbShellDecoder {
        AdbShellDecoder {}
    }
}


impl Decoder for AdbShellDecoder {
    type Item = AdbShellPacket;
    type Error = AdbError;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>> {
        if src.len() < ADB_SHELL_REQUEST_HEADER_LENGTH {
            // Not enough data to read length marker.
            return Ok(None);
        }

        // Read length marker.
        let length: [u8; 4] = [src[1], src[2], src[3], src[4]];
        let length: usize = u32::from_le_bytes(length) as usize;

        // Check that the length is not too large to avoid a denial of
        // service attack where the server runs out of memory.
        if length > MAX_MESSAGE_SIZE {
            return Err(AdbError::IOError(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Frame of length {} is too large.", length),
            )));
        }

        if src.len() < ADB_SHELL_REQUEST_HEADER_LENGTH + length {
            // The full string has not yet arrived.
            //
            // We reserve more space in the buffer. This is not strictly
            // necessary, but is a good idea performance-wise.
            src.reserve(ADB_SHELL_REQUEST_HEADER_LENGTH + length - src.len());

            // We inform the Framed that we need more bytes to form the next
            // frame.
            return Ok(None);
        }

        let id = AdbShellResponseId::from_u8(src[0]);
        let response: Vec<u8> =
            src[ADB_SHELL_REQUEST_HEADER_LENGTH..ADB_SHELL_REQUEST_HEADER_LENGTH + length].to_vec();

        src.advance(ADB_SHELL_REQUEST_HEADER_LENGTH + length);

        Ok(Some(AdbShellPacket { id, response }))
    }
}
