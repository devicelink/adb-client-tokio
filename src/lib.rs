pub mod util;

use futures::sink::SinkExt;
use std::path::Path;
use tokio_stream::StreamExt;

use std::str;
use tokio::net::{TcpStream, ToSocketAddrs, UnixStream};
use tokio_util::bytes::{Buf, BytesMut};
use tokio_util::codec::{Decoder, Encoder, FramedRead, FramedWrite};

use crate::util::{AdbError, Result};

const ADB_REQUEST_HEADER_LENGTH: usize = 4;
const ADB_RESPONSE_STATUS_LENGTH: usize = 4;
const ADB_RESPONSE_HEADER_LENGTH: usize = 8;

const MAX: usize = 8 * 1024 * 1024;

#[derive(Debug)]
pub struct AdbRequest {
    payload: Vec<u8>,
}

impl AdbRequest {
    pub fn new(cmd: &str) -> AdbRequest {
        AdbRequest {
            payload: cmd.as_bytes().to_vec(),
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum AdbResponse {
    OKAY { message: String },
    FAIL { message: String },
}

enum AdbResponseDecoderImpl {
    Status,
    StatusLengthPayload,
    StatusPayloadNewline,
}

pub struct AdbResponseDecoder {
    decoder_impl: AdbResponseDecoderImpl,
}

impl AdbResponseDecoder {
    pub fn new() -> AdbResponseDecoder {
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
        if length > MAX {
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
        if src.len() > MAX {
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

pub struct AdbRequestEncoder {}

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
        if length > MAX {
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

pub struct AdbClientConnection<R, W>
where
    R: tokio::io::AsyncRead,
    W: tokio::io::AsyncWrite,
{
    reader: FramedRead<R, AdbResponseDecoder>,
    writer: FramedWrite<W, AdbRequestEncoder>,
}

impl<R, W> AdbClientConnection<R, W>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    pub fn wrap(reader: R, writer: W) -> AdbClientConnection<R, W>
    where
        R: tokio::io::AsyncRead,
        W: tokio::io::AsyncWrite,
    {
        let reader = FramedRead::new(reader, AdbResponseDecoder::new());
        let writer = FramedWrite::new(writer, AdbRequestEncoder::new());

        return AdbClientConnection { reader, writer };
    }

    async fn send(&mut self, request: AdbRequest) -> Result<()> {
        self.writer.send(request).await
    }

    async fn next(&mut self) -> Result<String> {
        match self.reader.next().await {
            Some(Ok(AdbResponse::OKAY { message })) => Ok(message),
            Some(Ok(AdbResponse::FAIL { message })) => Err(AdbError::FailedResponseStatus(message)),
            Some(Err(e)) => Err(e),
            None => Err(AdbError::FailedResponseStatus("No response".into())),
        }
    }
}

pub struct AdbClient<R, W>
where
    R: tokio::io::AsyncRead,
    W: tokio::io::AsyncWrite,
{
    connection: AdbClientConnection<R, W>,
}

impl AdbClient<tokio::net::unix::OwnedReadHalf, tokio::net::unix::OwnedWriteHalf> {
    pub async fn connect_unix<P>(path: P) -> Result<Self>
    where
        P: AsRef<Path>,
    {
        let stream = UnixStream::connect(path).await?;
        let (reader, writer) = stream.into_split();
        let connection = AdbClientConnection::wrap(reader, writer);

        Ok(Self { connection })
    }
}

impl AdbClient<tokio::net::tcp::OwnedReadHalf, tokio::net::tcp::OwnedWriteHalf> {
    pub async fn connect_tcp<A>(addr: A) -> Result<Self>
    where
        A: ToSocketAddrs,
    {
        let stream = TcpStream::connect(addr).await?;
        let (reader, writer) = stream.into_split();
        let connection = AdbClientConnection::wrap(reader, writer);

        Ok(Self { connection })
    }
}

impl<R, W> AdbClient<R, W>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    pub async fn get_host_version(&mut self) -> Result<String> {
        self.connection
            .send(AdbRequest::new("host:version"))
            .await?;

        self.connection.next().await
    }

    pub async fn get_device_list(&mut self) -> Result<Vec<DeviceListItem>> {
        self.connection
            .send(AdbRequest::new("host:devices"))
            .await?;
        let devices = self.connection.next().await?;

        let devices = devices
            .trim()
            .split('\n')
            .filter_map(|line| {
                let mut split = line.split("\t");
                let id = match split.next() {
                    Some(id) => id,
                    None => return None,
                };
                let device_type = match split.next() {
                    Some(id) => id,
                    None => return None,
                };

                Some(DeviceListItem {
                    id: id.into(),
                    device_type: device_type.into(),
                })
            })
            .collect();

        Ok(devices)
    }

    pub async fn tcpip(mut self, serial: &str, port: u16) -> Result<String> {
        let request: AdbRequest = AdbRequest::new(&format!("host:transport:{}", serial));
        self.connection.send(request).await?;
        self.connection.reader.decoder_mut().decoder_impl = AdbResponseDecoderImpl::Status;
        self.connection.next().await?;

        let request = AdbRequest::new(&format!("tcpip:{}", port));
        self.connection.send(request).await?;
        self.connection.reader.decoder_mut().decoder_impl = AdbResponseDecoderImpl::StatusPayloadNewline;
        let message = self.connection.next().await?;
        Ok(message)
    }

    pub async fn connect_to_tcp_port(mut self, serial: &str, port: u16) -> Result<(R, W)> {
        let request: AdbRequest = AdbRequest::new(&format!("host:transport:{}", serial));
        self.connection.send(request).await?;
        self.connection.reader.decoder_mut().decoder_impl = AdbResponseDecoderImpl::Status;
        self.connection.next().await?;

        let request = AdbRequest::new(&format!("tcp:{}", port));
        self.connection.send(request).await?;
        self.connection.reader.decoder_mut().decoder_impl = AdbResponseDecoderImpl::Status;
        self.connection.next().await?;

        let reader = self.connection.reader.into_inner();
        let writer: W = self.connection.writer.into_inner();

        Ok((reader, writer))
    }

    pub async fn connect_device_to_server(&mut self, connection_string: &str) -> Result<()> {
        let request = AdbRequest::new(&format!("host:connect:{}", connection_string));
        self.connection.send(request).await?;

        let response = self.connection.next().await?;
        if response.starts_with("already connected to") || response.starts_with("connected to") {
            Ok(())
        } else {
            Err(AdbError::FailedResponseStatus(response))
        }
    }

    pub async fn forward_server(&mut self, serial: &str, local: &str, remote: &str) -> Result<()> {
        let request = AdbRequest::new(&format!("host:transport:{}", serial));
        self.connection.send(request).await?;
        self.connection.next().await?;

        let request = AdbRequest::new(&format!("forward:tcp:{};{}", local, remote));
        self.connection.send(request).await?;
        self.connection.next().await?;

        return Ok(());
    }

    pub async fn shell(mut self, serial: &str, command: &str) -> Result<String> {
        self.connection.reader.decoder_mut().decoder_impl = AdbResponseDecoderImpl::StatusLengthPayload;

        let request = AdbRequest::new(&format!("host:transport:{}", serial));
        self.connection.send(request).await?;
        self.connection.next().await?;

        let request = AdbRequest::new(&format!("shell,v2,TEM=xterm-256color,raw:{}", command));
        self.connection.send(request).await?;
        self.connection.next().await?;

        let reader = self.connection.reader.into_inner();
        let mut reader = FramedRead::new(reader, AdbShellDecoder::new());

        let mut response = Vec::<u8>::new();
        loop {
            let packet = match reader.next().await {
                Some(Ok(package)) => package,
                Some(Err(e)) => return Err(e.into()),
                None => return Err(AdbError::FailedResponseStatus("No response".into())),
            };

            match packet.id {
                AdbShellResponseId::Stdout => {
                    response.extend_from_slice(&packet.response);
                }
                AdbShellResponseId::Exit => {
                    break;
                }
                _ => {}
            }
        }

        let real_response = std::str::from_utf8(&response)?;
        Ok(real_response.into())
    }
}

#[derive(Debug, Clone)]
pub struct DeviceListItem {
    pub id: String,
    pub device_type: String,
}

const ADB_SHELL_REQUEST_HEADER_LENGTH: usize = 5;
struct AdbShellDecoder {}

impl AdbShellDecoder {
    pub fn new() -> AdbShellDecoder {
        AdbShellDecoder {}
    }
}

enum AdbShellResponseId {
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

struct AdbShellPacket {
    id: AdbShellResponseId,
    response: Vec<u8>,
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
        if length > MAX {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_failure() {
        let mut adb_client = AdbClient::connect_tcp("127.0.0.1:5038").await.unwrap();
        adb_client
            .connection
            .send(AdbRequest::new("host:abcd"))
            .await
            .unwrap();
        let response = adb_client.connection.next().await;

        assert_eq!(
            "FAILED response status: unknown host service".to_string(),
            response.err().unwrap().to_string()
        );
    }

    #[tokio::test]
    async fn test_get_host_version_command() {
        let mut adb_client = AdbClient::connect_tcp("127.0.0.1:5037").await.unwrap();

        let version: String = adb_client
            .get_host_version()
            .await
            .expect("Failed to get host version");

        assert_eq!(version, "0029");
    }

    #[tokio::test]
    async fn test_get_devices_command() {
        let mut adb_client = AdbClient::connect_tcp("127.0.0.1:5037").await.unwrap();

        let devices = adb_client
            .get_device_list()
            .await
            .expect("Failed to get host version");

        assert_eq!(devices.len(), 1);
    }
    
    #[tokio::test]
    async fn test_tcpip_command() {
        let adb_client = AdbClient::connect_tcp("127.0.0.1:5037").await.unwrap();
        let response: String = adb_client
            .tcpip("08261JECB10524", 5555)
            .await
            .expect("Failed to execute tcpip command");
        assert_eq!(response, "restarting in TCP mode port: 5555");
    }

    #[tokio::test]
    async fn test_get_prop_command() {
        let adb_client = AdbClient::connect_tcp("127.0.0.1:5037").await.unwrap();
        let manufaturer: String = adb_client
            .shell("08261JECB10524", "getprop ro.product.manufacturer")
            .await
            .expect("Failed to get manufacturer");

        let adb_client = AdbClient::connect_tcp("127.0.0.1:5037").await.unwrap();
        let model: String = adb_client
            .shell("08261JECB10524", "getprop ro.product.model")
            .await
            .expect("Failed to get model");

        println!("manufaturer: {:?}", &manufaturer);
        println!("model: {:?}", &model);
    }
}
