use std::path::Path;
use crate::shell::{AdbShellProtocol, AdbShellResponseId};
use tokio::net::{TcpStream, UnixStream};
use tokio_stream::StreamExt;
use tokio_util::codec::FramedRead;
use crate::util::{AdbError, Result};
use crate::connection::{AdbClientConnection, AdbRequest, AdbResponseDecoderImpl};
use std::fmt::Display;

/// Specifiying all possibilities to connect to a device.
#[derive(Debug, Clone)]
pub enum Device {
    /// use the only connected device (error if multiple devices connected)
    Default,
    /// use USB device (error if multiple devices connected)
    Usb,
    /// use TCP/IP device (error if multiple TCP/IP devices available)
    TcpIp,
    /// use device with given serial
    Serial(String),
}

impl Display for Device {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Device::Default => write!(f, "host:transport-any"),
            Device::Usb => write!(f, "host:transport-usb"),
            Device::TcpIp => write!(f, "host:transport-local"),
            Device::Serial(serial) => write!(f, "host:transport:{}", serial),
        }
    }
}

/// allow external structs to convert to a device.
pub trait ToDevice {
    /// convert to a device.
    fn to_device(&self) -> Device;
}

impl Into<AdbRequest> for Device {
    fn into(self) -> AdbRequest {
        AdbRequest::new(&self.to_string())
    }
}

impl ToDevice for Device {
    fn to_device(&self) -> Device {
        self.clone()
    }
}

impl ToDevice for &str {
    fn to_device(&self) -> Device {
        Device::Serial(self.to_string())
    }
}

impl ToDevice for &String {
    fn to_device(&self) -> Device {
        Device::Serial(self.to_string())
    }
}

/// ADB client that can connect to ADB server and execute commands.
/// 
/// Example:
/// ```no_run
/// use adb_client_tokio::AdbClient;
/// 
/// #[tokio::main]
/// async fn main() {
///     let mut adb = AdbClient::connect_tcp("127.0.0.1:5037").await.unwrap();
///     let version = adb.get_host_version().await.expect("Failed to get host version");
///     println!("ADB server version: {}", version);
/// }
/// ```
#[derive(Debug)]
pub struct AdbClient<R, W>
where
    R: tokio::io::AsyncRead,
    W: tokio::io::AsyncWrite,
{
    connection: AdbClientConnection<R, W>,
}

impl AdbClient<tokio::net::unix::OwnedReadHalf, tokio::net::unix::OwnedWriteHalf> {
    /// Connect to ADB server using Unix domain socket.
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

impl AdbClient<tokio::net::tcp::OwnedReadHalf, tokio::net::tcp::OwnedWriteHalf>
{
    /// Connect to ADB server using TCP socket.
    pub async fn connect_tcp<A>(addr: A) -> Result<Self>
    where
        A: tokio::net::ToSocketAddrs,
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
    /// Get the version of the ADB server.
    pub async fn get_host_version(&mut self) -> Result<String> {
        self.connection
            .send(AdbRequest::new("host:version"))
            .await?;

        self.connection.next().await
    }

    /// Get the list of devices connected to the ADB server.
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

    /// Restart the adb server on the device in tcp mode on the given port.
    pub async fn tcpip(mut self, device: impl ToDevice, port: u16) -> Result<String> {
        self.connection.send(device.to_device().into()).await?;
        self.connection.reader.decoder_mut().decoder_impl = AdbResponseDecoderImpl::Status;
        self.connection.next().await?;

        let request = AdbRequest::new(&format!("tcpip:{}", port));
        self.connection.send(request).await?;
        self.connection.reader.decoder_mut().decoder_impl = AdbResponseDecoderImpl::StatusPayloadNewline;
        let message = self.connection.next().await?;
        Ok(message)
    }
    

    /// Connect to a tcp port on the give device.
    pub async fn connect_to_device(mut self, device: impl ToDevice, remote: Remote) -> Result<(R, W)> {
        self.connection.send(device.to_device().into()).await?;
        self.connection.reader.decoder_mut().decoder_impl = AdbResponseDecoderImpl::Status;
        self.connection.next().await?;

        let request = AdbRequest::new(remote.to_string().as_str());
        self.connection.send(request).await?;
        self.connection.reader.decoder_mut().decoder_impl = AdbResponseDecoderImpl::Status;
        self.connection.next().await?;

        let reader = self.connection.reader.into_inner();
        let writer: W = self.connection.writer.into_inner();

        Ok((reader, writer))
    }

    /// Connect a device to the ADB server.
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

    /// forward tcp connections from local to remote.
    pub async fn forward_server(&mut self, device: impl ToDevice, local: &str, remote: &str) -> Result<()> {
        self.connection.send(device.to_device().into()).await?;
        self.connection.next().await?;

        let request = AdbRequest::new(&format!("forward:tcp:{};{}", local, remote));
        self.connection.send(request).await?;
        self.connection.next().await?;

        return Ok(());
    }

    /// run a shell command on the device.
    pub async fn shell(mut self, device: impl ToDevice, command: &str) -> Result<String> {
        self.connection.send(device.to_device().into()).await?;
        self.connection.reader.decoder_mut().decoder_impl = AdbResponseDecoderImpl::Status;
        self.connection.next().await?;

        let request = AdbRequest::new(&format!("shell,v2,TERM=xterm-256color,raw:{}", command));
        self.connection.send(request).await?;
        self.connection.reader.decoder_mut().decoder_impl = AdbResponseDecoderImpl::Status;
        self.connection.next().await?;

        let reader = self.connection.reader.into_inner();
        let mut reader = FramedRead::new(reader, AdbShellProtocol::new());

        let mut response = Vec::<u8>::new();
        loop {
            let packet = match reader.next().await {
                Some(Ok(package)) => package,
                Some(Err(e)) => return Err(e.into()),
                None => return Err(AdbError::FailedResponseStatus("No response".into())),
            };

            match packet.id {
                AdbShellResponseId::Stdout => {
                    response.extend_from_slice(&packet.payload);
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

/// represents the options to connect 
#[derive(Debug)]
pub enum Remote {
    /// TCP localhost:<port> on device
    Tcp(u16),
    /// Unix local domain socket on device
    Unix(String),
    /// JDWP thread on VM process <pid>   
    Jwp(u16)
}

impl Display for Remote {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Remote::Tcp(port) => write!(f, "tcp:{}", port),
            Remote::Unix(path) => write!(f, "local:{}", path),
            Remote::Jwp(pid) => write!(f, "jdwp:{}", pid),
        }
    }
}

/// ADB device list item.
#[derive(Debug, Clone)]
pub struct DeviceListItem {
    /// Device serial number.
    pub id: String,
    /// Device type.
    pub device_type: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_failure() -> Result<()> {
        let mut adb_client = AdbClient::connect_tcp("127.0.0.1:5037").await?;
        adb_client
            .connection
            .send(AdbRequest::new("host:abcd"))
            .await?;
        adb_client.connection.reader.decoder_mut().decoder_impl = AdbResponseDecoderImpl::Status;
        let response = adb_client.connection.next().await;

        assert_eq!(
            "FAILED response status: unknown host service".to_string(),
            response.err().unwrap().to_string()
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_get_host_version_command() -> Result<()> {
        let mut adb_client = AdbClient::connect_tcp("127.0.0.1:5037").await?;

        let version: String = adb_client
            .get_host_version()
            .await?;

        assert_eq!(version, "0029");
        Ok(())
    }

    #[tokio::test]
    async fn test_get_devices_command() -> Result<()> {
        let mut adb_client = AdbClient::connect_tcp("127.0.0.1:5037").await?;

        let devices = adb_client
            .get_device_list()
            .await?;

        assert_eq!(devices.len(), 1);
        Ok(())
    }
    
    #[ignore]
    #[tokio::test]
    async fn test_tcpip_command() -> Result<()> {
        let adb_client = AdbClient::connect_tcp("127.0.0.1:5037").await?;
        let response: String = adb_client
            .tcpip("08261JECB10524", 5555)
            .await?;
        
        assert_eq!(response, "restarting in TCP mode port: 5555");
        Ok(())
    }

    #[tokio::test]
    async fn test_get_prop_command() -> Result<()> {
        let adb_client = AdbClient::connect_tcp("127.0.0.1:5038").await?;
        let manufaturer: String = adb_client
            .shell("08261JECB10524", "getprop ro.product.manufacturer")
            .await?;

        let adb_client = AdbClient::connect_tcp("127.0.0.1:5038").await?;
        let model: String = adb_client
            .shell("08261JECB10524", "getprop ro.product.model")
            .await?;

        println!("manufaturer: {:?}", &manufaturer);
        println!("model: {:?}", &model);

        Ok(())
    }
}