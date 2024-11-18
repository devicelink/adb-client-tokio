use futures::sink::SinkExt;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpStream, UnixStream};
use tokio_stream::StreamExt;

use std::pin::Pin;
use std::str;
use std::task::{Context, Poll};
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
    pub(crate) fn new() -> AdbResponseDecoder {
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
        if src.len() < ADB_RESPONSE_HEADER_LENGTH {
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
        src.advance(ADB_RESPONSE_HEADER_LENGTH + length);

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
        if src.len() == 0
        {
            return Ok(None);
        }

        let response = match self.decoder_impl {
            AdbResponseDecoderImpl::Status => self.decode_status(src),
            AdbResponseDecoderImpl::StatusLengthPayload => self.decode_status_and_payload(src),
            AdbResponseDecoderImpl::StatusPayloadNewline => {
                self.decode_status_and_read_until_new_line(src)
            }
        };

        println!("decofing:\n{}", pretty_hex::pretty_hex(&src));

        response
    }
}

#[derive(Debug)]
pub(crate) struct AdbRequestEncoder {}

impl AdbRequestEncoder {
    pub(crate) fn new() -> AdbRequestEncoder {
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

        println!("sending {}", pretty_hex::pretty_hex(&dst));

        Ok(())
    }
}

/// AdbClientStream represents a stream that can be used to communicate with an ADB server.
#[derive(Debug)]
pub enum AdbClientStream {
    /// A TCP stream
    Tcp(TcpStream),
    /// A Unix stream
    Unix(UnixStream),
}

impl AsyncRead for AdbClientStream {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            AdbClientStream::Tcp(s) => Pin::new(s).poll_read(cx, buf),
            AdbClientStream::Unix(s) => Pin::new(s).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for AdbClientStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::result::Result<usize, std::io::Error>> {
        match self.get_mut() {
            AdbClientStream::Tcp(s) => Pin::new(s).poll_write(cx, buf),
            AdbClientStream::Unix(s) => Pin::new(s).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::result::Result<(), std::io::Error>> {
        match self.get_mut() {
            AdbClientStream::Tcp(s) => Pin::new(s).poll_flush(cx),
            AdbClientStream::Unix(s) => Pin::new(s).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::result::Result<(), std::io::Error>> {
        match self.get_mut() {
            AdbClientStream::Tcp(s) => Pin::new(s).poll_shutdown(cx),
            AdbClientStream::Unix(s) => Pin::new(s).poll_shutdown(cx),
        }
    }
}

/// AdbClientStreamOwnedReadHalf represents the read half of an AdbClientStream.
#[derive(Debug)]
pub enum AdbClientStreamOwnedReadHalf {
    /// A TCP read half
    Tcp(tokio::net::tcp::OwnedReadHalf),
    /// A Unix read half
    Unix(tokio::net::unix::OwnedReadHalf),
}

/// AdbClientStreamOwnedWriteHalf represents the write half of an AdbClientStream.
#[derive(Debug)]
pub enum AdbClientStreamOwnedWriteHalf {
    /// A TCP write half
    Tcp(tokio::net::tcp::OwnedWriteHalf),
    /// A Unix write half
    Unix(tokio::net::unix::OwnedWriteHalf),
}

impl AdbClientStream {

    /// Split the stream into a read half and a write half.
    pub fn into_split(self) -> (AdbClientStreamOwnedReadHalf, AdbClientStreamOwnedWriteHalf) {
        match self {
            AdbClientStream::Tcp(s) => {
                let (r, w) = s.into_split();
                (AdbClientStreamOwnedReadHalf::Tcp(r), AdbClientStreamOwnedWriteHalf::Tcp(w))
            }
            AdbClientStream::Unix(s) => {
                let (r, w) = s.into_split();
                (AdbClientStreamOwnedReadHalf::Unix(r), AdbClientStreamOwnedWriteHalf::Unix(w))
            }
        }
    }
}

impl AdbClientStreamOwnedReadHalf {
    /// Reunite the read half with the write half to recreate the original stream.
    pub fn reunite(self, w: AdbClientStreamOwnedWriteHalf) -> Result<AdbClientStream> {
        match self {
            AdbClientStreamOwnedReadHalf::Tcp(r) => {
                let w = match w {
                    AdbClientStreamOwnedWriteHalf::Tcp(w) => w,
                    _ => panic!("Invalid write half"),
                };
                Ok(AdbClientStream::Tcp(r.reunite(w).unwrap()))
            }
            AdbClientStreamOwnedReadHalf::Unix(r) => {
                let w = match w {
                    AdbClientStreamOwnedWriteHalf::Unix(w) => w,
                    _ => panic!("Invalid write half"),
                };
                Ok(AdbClientStream::Unix(r.reunite(w).unwrap()))
            }
        }

    }
}

impl AsyncRead for AdbClientStreamOwnedReadHalf {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            AdbClientStreamOwnedReadHalf::Tcp(s) => Pin::new(s).poll_read(cx, buf),
            AdbClientStreamOwnedReadHalf::Unix(s) => Pin::new(s).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for AdbClientStreamOwnedWriteHalf {
    #[inline]
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match self.get_mut() {
            AdbClientStreamOwnedWriteHalf::Unix(x) => Pin::new(x).poll_write(cx, buf),
            AdbClientStreamOwnedWriteHalf::Tcp(x) => Pin::new(x).poll_write(cx, buf),
        }
    }

    #[inline]
    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<std::io::Result<usize>> {
        match self.get_mut() {
            AdbClientStreamOwnedWriteHalf::Unix(x) => Pin::new(x).poll_write_vectored(cx, bufs),
            AdbClientStreamOwnedWriteHalf::Tcp(x) => Pin::new(x).poll_write_vectored(cx, bufs),
        }
    }

    #[inline]
    fn is_write_vectored(&self) -> bool {
        match self {
            AdbClientStreamOwnedWriteHalf::Unix(x) => x.is_write_vectored(),
            AdbClientStreamOwnedWriteHalf::Tcp(x) => x.is_write_vectored(),
        }
    }

    #[inline]
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            AdbClientStreamOwnedWriteHalf::Unix(x) => Pin::new(x).poll_flush(cx),
            AdbClientStreamOwnedWriteHalf::Tcp(x) => Pin::new(x).poll_flush(cx),
        }
    }

    #[inline]
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            AdbClientStreamOwnedWriteHalf::Unix(x) => Pin::new(x).poll_shutdown(cx),
            AdbClientStreamOwnedWriteHalf::Tcp(x) => Pin::new(x).poll_shutdown(cx),
        }
    }
}

#[derive(Debug)]
pub(crate)  struct AdbClientConnection
{
    pub(crate) reader: FramedRead<AdbClientStreamOwnedReadHalf, AdbResponseDecoder>,
    pub(crate) writer: FramedWrite<AdbClientStreamOwnedWriteHalf, AdbRequestEncoder>,
}

impl<'a> AdbClientConnection
{
    pub(crate) fn new(socket: AdbClientStream) -> AdbClientConnection
    {
        let (r, w) = socket.into_split();

        let reader = FramedRead::new(r, AdbResponseDecoder::new());
        let writer = FramedWrite::new(w, AdbRequestEncoder::new());

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