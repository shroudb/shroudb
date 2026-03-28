//! Lightweight RESP3 client connection.
//!
//! Handles TCP (and optionally TLS) connectivity and RESP3 frame encoding/decoding.

use std::io;
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::net::TcpStream;

use crate::error::ClientError;
use crate::response::Response;

/// A connection to a ShrouDB server that speaks RESP3.
///
/// Supports both plain TCP and TLS transports via boxed trait objects.
pub struct Connection {
    reader: BufReader<Box<dyn tokio::io::AsyncRead + Unpin + Send>>,
    writer: BufWriter<Box<dyn tokio::io::AsyncWrite + Unpin + Send>>,
}

impl Connection {
    /// Connect to a ShrouDB server at the given address (e.g. `"127.0.0.1:6399"`).
    pub async fn connect(addr: &str) -> Result<Self, ClientError> {
        let stream = TcpStream::connect(addr).await?;
        let (r, w) = tokio::io::split(stream);
        Ok(Self {
            reader: BufReader::new(Box::new(r)),
            writer: BufWriter::new(Box::new(w)),
        })
    }

    /// Connect to a ShrouDB server over TLS at the given address (e.g. `"127.0.0.1:6399"`).
    ///
    /// Uses the system's native root certificate store for server verification.
    pub async fn connect_tls(addr: &str) -> Result<Self, ClientError> {
        let mut root_store = rustls::RootCertStore::empty();
        let native_certs = rustls_native_certs::load_native_certs();
        for cert in native_certs.certs {
            root_store
                .add(cert)
                .map_err(|e| ClientError::Protocol(format!("failed to add root cert: {e}")))?;
        }

        let config = rustls::ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth();

        let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
        let stream = TcpStream::connect(addr).await?;

        let host = if addr.starts_with('[') {
            // IPv6: [::1]:6399
            addr.split(']')
                .next()
                .unwrap_or("localhost")
                .trim_start_matches('[')
        } else {
            // IPv4 or hostname: 127.0.0.1:6399
            addr.rsplit_once(':').map(|(h, _)| h).unwrap_or(addr)
        };
        let domain = rustls_pki_types::ServerName::try_from(host.to_string())
            .map_err(|e| ClientError::Protocol(format!("invalid server name: {e}")))?;

        let tls_stream = connector
            .connect(domain, stream)
            .await
            .map_err(|e| ClientError::Protocol(format!("TLS handshake failed: {e}")))?;

        let (r, w) = tokio::io::split(tls_stream);
        Ok(Self {
            reader: BufReader::new(Box::new(r)),
            writer: BufWriter::new(Box::new(w)),
        })
    }

    /// Send a command (as a list of string arguments) and read the response.
    ///
    /// Times out after 30 seconds to prevent hanging on unresponsive servers.
    pub async fn send_command(&mut self, args: &[String]) -> Result<Response, ClientError> {
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            self.write_command(args).await?;
            self.writer.flush().await?;
            self.read_response().await
        })
        .await
        .map_err(|_| ClientError::Timeout)?
    }

    /// Send a command from `&str` slices.
    pub async fn send_command_strs(&mut self, args: &[&str]) -> Result<Response, ClientError> {
        let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        self.send_command(&owned).await
    }

    async fn write_command(&mut self, args: &[String]) -> io::Result<()> {
        // *N\r\n followed by N bulk strings
        self.writer
            .write_all(format!("*{}\r\n", args.len()).as_bytes())
            .await?;
        for arg in args {
            let bytes = arg.as_bytes();
            self.writer
                .write_all(format!("${}\r\n", bytes.len()).as_bytes())
                .await?;
            self.writer.write_all(bytes).await?;
            self.writer.write_all(b"\r\n").await?;
        }
        Ok(())
    }

    async fn read_response(&mut self) -> Result<Response, ClientError> {
        read_value(&mut self.reader).await
    }
}

fn read_value(
    reader: &mut BufReader<Box<dyn tokio::io::AsyncRead + Unpin + Send>>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Response, ClientError>> + Send + '_>>
{
    Box::pin(async move {
        let mut line = String::new();
        let n = reader
            .read_line(&mut line)
            .await
            .map_err(ClientError::Connection)?;
        if n == 0 {
            return Err(ClientError::Protocol("connection closed".into()));
        }
        let line = line.trim_end_matches("\r\n").trim_end_matches('\n');

        if line.is_empty() {
            return Err(ClientError::Protocol("empty response".into()));
        }

        let type_byte = line.as_bytes()[0];
        let payload = &line[1..];

        match type_byte {
            b'+' => Ok(Response::String(payload.to_string())),
            b'-' => Ok(Response::Error(payload.to_string())),
            b':' => {
                let n: i64 = payload
                    .parse()
                    .map_err(|e| ClientError::Protocol(format!("invalid integer: {e}")))?;
                Ok(Response::Integer(n))
            }
            b'_' => Ok(Response::Null),
            b'$' => {
                let len: i64 = payload
                    .parse()
                    .map_err(|e| ClientError::Protocol(format!("invalid bulk length: {e}")))?;
                if len < 0 {
                    return Ok(Response::Null);
                }
                let len = len as usize;
                let mut buf = vec![0u8; len + 2]; // +2 for \r\n
                reader
                    .read_exact(&mut buf)
                    .await
                    .map_err(ClientError::Connection)?;
                let s = String::from_utf8_lossy(&buf[..len]).to_string();
                Ok(Response::String(s))
            }
            b'*' => {
                let count: i64 = payload
                    .parse()
                    .map_err(|e| ClientError::Protocol(format!("invalid array length: {e}")))?;
                if count < 0 {
                    return Ok(Response::Null);
                }
                let mut items = Vec::with_capacity(count as usize);
                for _ in 0..count {
                    items.push(read_value(reader).await?);
                }
                Ok(Response::Array(items))
            }
            b'%' => {
                let count: i64 = payload
                    .parse()
                    .map_err(|e| ClientError::Protocol(format!("invalid map length: {e}")))?;
                let mut entries = Vec::with_capacity(count as usize);
                for _ in 0..count {
                    let key = read_value(reader).await?;
                    let val = read_value(reader).await?;
                    entries.push((key, val));
                }
                Ok(Response::Map(entries))
            }
            // RESP3 Boolean: #t\r\n or #f\r\n
            b'#' => match payload {
                "t" => Ok(Response::String("true".to_string())),
                "f" => Ok(Response::String("false".to_string())),
                _ => Err(ClientError::Protocol(format!(
                    "invalid boolean value: {payload}"
                ))),
            },
            // RESP3 Double: ,3.14\r\n
            b',' => Ok(Response::String(payload.to_string())),
            // RESP3 Push (server-initiated): treat like Array
            b'>' => {
                let count: i64 = payload
                    .parse()
                    .map_err(|e| ClientError::Protocol(format!("invalid push length: {e}")))?;
                let mut items = Vec::with_capacity(count as usize);
                for _ in 0..count {
                    items.push(read_value(reader).await?);
                }
                Ok(Response::Array(items))
            }
            // RESP3 Set: treat like Array
            b'~' => {
                let count: i64 = payload
                    .parse()
                    .map_err(|e| ClientError::Protocol(format!("invalid set length: {e}")))?;
                let mut items = Vec::with_capacity(count as usize);
                for _ in 0..count {
                    items.push(read_value(reader).await?);
                }
                Ok(Response::Array(items))
            }
            _ => Err(ClientError::Protocol(format!(
                "unsupported RESP3 type: '{}' (0x{:02x})",
                type_byte as char, type_byte
            ))),
        }
    })
}
