use futures::sink::SinkExt;
use tokio_stream::StreamExt;

use std::str;
use tokio_util::bytes::{Buf, BytesMut};
use tokio_util::codec::{Decoder, Encoder, FramedRead, FramedWrite};

use crate::util::{AdbError, Result};

const ADB_REQUEST_HEADER_LENGTH: usize = 4;
const ADB_RESPONSE_STATUS_LENGTH: usize = 4;
const ADB_RESPONSE_HEADER_LENGTH: usize = 8;

pub(crate) const MAX_MESSAGE_SIZE: usize = 8 * 1024 * 1024;

#[derive(Debug)]
pub(crate) struct AdbRequest {
    payload: Vec<u8>,
}

impl AdbRequest {
    pub(crate) fn new(cmd: &str) -> AdbRequest {
        AdbRequest {
            payload: cmd.as_bytes().to_vec(),
        }
    }
}

#[derive(Debug, PartialEq)]
pub(crate) enum AdbResponse {
    OKAY { message: String },
    FAIL { message: String },
}

#[derive(Debug)]
pub(crate) enum AdbResponseDecoderImpl {
    Status,
    StatusLengthPayload,
    StatusPayloadNewline,
}

#[derive(Debug)]
pub(crate) struct AdbResponseDecoder {
    pub(crate) decoder_impl: AdbResponseDecoderImpl,
}

impl AdbResponseDecoder {
    fn new() -> AdbResponseDecoder {
        AdbResponseDecoder {
            decoder_impl: AdbResponseDecoderImpl::StatusLengthPayload,
        }
    }
}

impl AdbResponseDecoder {
    fn decode_status(&mut self, src: &mut BytesMut) -> Result<Option<AdbResponse>> {
        if src.len() < ADB_RESPONSE_STATUS_LENGTH {
            // Not enough data to read length marker.
            return Ok(None);
        }

        let status = src[0..4].to_vec();
        let status = str::from_utf8(&status)?;

        match status {
            "OKAY" => {
                src.advance(ADB_RESPONSE_STATUS_LENGTH);
                Ok(Some(AdbResponse::OKAY {
                    message: "".to_string(),
                }))
            },
            "FAIL" => {
                if src.len() < ADB_RESPONSE_HEADER_LENGTH {
                    return Ok(None);
                }

                let length: [u8; 4] = [src[4], src[5], src[6], src[7]];
                let length: usize = usize::from_str_radix(std::str::from_utf8(&length)?, 16)?;

                if src.len() < ADB_RESPONSE_HEADER_LENGTH + length {
                    return Ok(None);
                }

                let message: Vec<u8> = src[ADB_RESPONSE_HEADER_LENGTH..ADB_RESPONSE_HEADER_LENGTH + length].to_vec();
                let message = String::from_utf8_lossy(&message).to_string();
                src.advance(ADB_RESPONSE_HEADER_LENGTH + length);

                Ok(Some(AdbResponse::FAIL { message }))
            }
            _ => Err(AdbError::UnknownResponseStatus(status.into())),
        }
    }

    fn decode_status_and_payload(&mut self, src: &mut BytesMut) -> Result<Option<AdbResponse>> {
        if src.len() < ADB_RESPONSE_STATUS_LENGTH {
            // Not enough data to read length marker.
            return Ok(None);
        }

        // Read length marker.
        let length: [u8; 4] = [src[4], src[5], src[6], src[7]];
        let length: usize = usize::from_str_radix(std::str::from_utf8(&length)?, 16)?;

        // Check that the length is not too large to avoid a denial of
        // service attack where the server runs out of memory.
        if length > MAX_MESSAGE_SIZE {
            return Err(AdbError::IOError(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Frame of length {} is too large.", length),
            )));
        }

        if src.len() < ADB_RESPONSE_HEADER_LENGTH + length {
            // The full string has not yet arrived.
            //
            // We reserve more space in the buffer. This is not strictly
            // necessary, but is a good idea performance-wise.
            src.reserve(ADB_RESPONSE_HEADER_LENGTH + length - src.len());

            // We inform the Framed that we need more bytes to form the next
            // frame.
            return Ok(None);
        }

        // Use advance to modify src such that it no longer contains
        // this frame.
        let status = src[0..4].to_vec();
        let status = str::from_utf8(&status)?;
        let payload: Vec<u8> =
            src[ADB_RESPONSE_HEADER_LENGTH..ADB_RESPONSE_HEADER_LENGTH + length].to_vec();
        let message = String::from_utf8_lossy(&payload).to_string();
        src.advance(ADB_REQUEST_HEADER_LENGTH + length);

        // Read string from src
        match status {
            "OKAY" => Ok(Some(AdbResponse::OKAY { message })),
            "FAIL" => Ok(Some(AdbResponse::FAIL { message })),
            _ => Err(AdbError::UnknownResponseStatus(status.into())),
        }
    }

    fn decode_status_and_read_until_new_line(
        &mut self,
        src: &mut BytesMut,
    ) -> Result<Option<AdbResponse>> {
        if src.len() < ADB_RESPONSE_STATUS_LENGTH {
            // Not enough data to read length marker.
            return Ok(None);
        }

        // Check that the length is not too large to avoid a denial of
        // service attack where the server runs out of memory.
        if src.len() > MAX_MESSAGE_SIZE {
            return Err(AdbError::IOError(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Frame of length {} is too large.", src.len()),
            )));
        }

        let status = src[0..ADB_RESPONSE_STATUS_LENGTH].to_vec();
        let status = str::from_utf8(&status)?;

        let newline_offset = src[ADB_RESPONSE_STATUS_LENGTH..src.len()]
            .iter()
            .position(|b| *b == b'\n');

        match newline_offset {
            Some(offset) => {
                let message =
                    src[ADB_RESPONSE_STATUS_LENGTH..ADB_RESPONSE_STATUS_LENGTH + offset].to_vec();
                let message = String::from_utf8_lossy(&message).to_string();
                src.advance(ADB_RESPONSE_STATUS_LENGTH + offset + 1);

                match status {
                    "OKAY" => Ok(Some(AdbResponse::OKAY { message })),
                    "FAIL" => Ok(Some(AdbResponse::FAIL { message })),
                    _ => Err(AdbError::UnknownResponseStatus(status.into())),
                }
            }
            None => Ok(None),
        }
    }
}

impl Decoder for AdbResponseDecoder {
    type Item = AdbResponse;
    type Error = AdbError;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>> {
        match self.decoder_impl {
            AdbResponseDecoderImpl::Status => self.decode_status(src),
            AdbResponseDecoderImpl::StatusLengthPayload => self.decode_status_and_payload(src),
            AdbResponseDecoderImpl::StatusPayloadNewline => {
                self.decode_status_and_read_until_new_line(src)
            }
        }
    }
}

#[derive(Debug)]
pub(crate) struct AdbRequestEncoder {}

impl AdbRequestEncoder {
    pub fn new() -> AdbRequestEncoder {
        AdbRequestEncoder {}
    }
}

impl Encoder<AdbRequest> for AdbRequestEncoder {
    type Error = AdbError;

    fn encode(&mut self, msg: AdbRequest, dst: &mut BytesMut) -> Result<()> {
        // Don't send a string if it is longer than the other end will
        // accept.
        let length = msg.payload.len();
        if length > MAX_MESSAGE_SIZE {
            return Err(AdbError::IOError(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Frame of length {} is too large.", length),
            )));
        }

        // Reserve space in the buffer.
        dst.reserve(ADB_REQUEST_HEADER_LENGTH + length);

        let length_hex = format!("{:04x}", length);

        // Write the length and string to the buffer.
        dst.extend_from_slice(&length_hex.as_bytes());
        dst.extend_from_slice(&msg.payload);
        Ok(())
    }
}

#[derive(Debug)]
pub(crate)  struct AdbClientConnection<R, W>
where
    R: tokio::io::AsyncRead,
    W: tokio::io::AsyncWrite,
{
    pub(crate) reader: FramedRead<R, AdbResponseDecoder>,
    pub(crate) writer: FramedWrite<W, AdbRequestEncoder>,
}

impl<R, W> AdbClientConnection<R, W>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    pub(crate)  fn wrap(reader: R, writer: W) -> AdbClientConnection<R, W>
    where
        R: tokio::io::AsyncRead,
        W: tokio::io::AsyncWrite,
    {
        let reader = FramedRead::new(reader, AdbResponseDecoder::new());
        let writer = FramedWrite::new(writer, AdbRequestEncoder::new());

        return AdbClientConnection { reader, writer };
    }

    pub(crate) async fn send(&mut self, request: AdbRequest) -> Result<()> {
        self.writer.send(request).await
    }

    pub(crate) async fn next(&mut self) -> Result<String> {
        match self.reader.next().await {
            Some(Ok(AdbResponse::OKAY { message })) => Ok(message),
            Some(Ok(AdbResponse::FAIL { message })) => Err(AdbError::FailedResponseStatus(message)),
            Some(Err(e)) => Err(e),
            None => Err(AdbError::FailedResponseStatus("No response".into())),
        }
    }
}