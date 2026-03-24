use std::net::SocketAddr;
use std::path::Path;
use std::process::{Child, Command as StdCommand};

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

/// A test server that spawns the actual `shroudb` binary on a random port.
pub struct TestServer {
    child: Child,
    pub addr: SocketAddr,
    _data_dir: Option<tempfile::TempDir>,
}

impl TestServer {
    pub async fn start() -> Self {
        Self::start_with_config("").await
    }

    pub async fn start_with_config(extra_config: &str) -> Self {
        let data_dir = tempfile::tempdir().unwrap();
        let mut server = Self::start_inner(data_dir.path(), &"ab".repeat(32), extra_config).await;
        server._data_dir = Some(data_dir);
        server
    }

    /// Start a server with a specific data directory and master key.
    /// The caller owns the data directory (it is NOT cleaned up on drop).
    pub async fn start_with_dir_and_key(data_dir: &Path, master_key: &str) -> Self {
        Self::start_inner(data_dir, master_key, "").await
    }

    async fn start_inner(data_dir: &Path, master_key: &str, extra_config: &str) -> Self {
        let port = find_free_port().await;
        let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();

        // Write config into the data dir (or a temp location next to it)
        let config_path = data_dir.join("config.toml");
        let config = format!(
            r#"
[server]
bind = "127.0.0.1:{port}"

[storage]
data_dir = "{}"

[keyspaces.test-jwt]
type = "jwt"
algorithm = "ES256"
default_ttl = "15m"

[keyspaces.test-apikeys]
type = "api_key"
prefix = "sk"

[keyspaces.test-hmac]
type = "hmac"
algorithm = "sha256"

[keyspaces.test-refresh]
type = "refresh_token"
token_ttl = "30d"
max_chain_length = 10

{extra_config}
"#,
            data_dir.display()
        );
        std::fs::create_dir_all(data_dir).unwrap();
        std::fs::write(&config_path, &config).unwrap();

        // Start the binary
        let child = StdCommand::new(env!("CARGO_BIN_EXE_shroudb"))
            .arg("--config")
            .arg(&config_path)
            .env("SHROUDB_MASTER_KEY", master_key)
            .env("RUST_LOG", "warn")
            .spawn()
            .expect("failed to start shroudb");

        // Wait for the server to be ready (up to 5 seconds)
        let mut connected = false;
        for _ in 0..50 {
            if tokio::net::TcpStream::connect(&addr).await.is_ok() {
                connected = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        if !connected {
            panic!("shroudb server did not start within 5 seconds on {addr}");
        }

        Self {
            child,
            addr,
            _data_dir: None,
        }
    }

    pub fn stop(&mut self) {
        // Send SIGTERM for graceful shutdown, then wait
        #[cfg(unix)]
        {
            unsafe {
                libc::kill(self.child.id() as libc::pid_t, libc::SIGTERM);
            }
            // Give the process a moment to shut down gracefully
            let _ = self.child.wait();
        }
        #[cfg(not(unix))]
        {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.stop();
    }
}

async fn find_free_port() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    listener.local_addr().unwrap().port()
}

// ---------------------------------------------------------------------------
// TCP client
// ---------------------------------------------------------------------------

pub struct TestClient {
    reader: BufReader<tokio::io::ReadHalf<tokio::net::TcpStream>>,
    writer: tokio::io::WriteHalf<tokio::net::TcpStream>,
}

impl TestClient {
    pub async fn connect(addr: SocketAddr) -> Self {
        let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let (r, w) = tokio::io::split(stream);
        Self {
            reader: BufReader::new(r),
            writer: w,
        }
    }

    /// Send a command as a RESP3 array and return the parsed response.
    pub async fn cmd(&mut self, args: &[&str]) -> TestResponse {
        // Write RESP3 array
        let header = format!("*{}\r\n", args.len());
        self.writer.write_all(header.as_bytes()).await.unwrap();
        for arg in args {
            let bulk = format!("${}\r\n{}\r\n", arg.len(), arg);
            self.writer.write_all(bulk.as_bytes()).await.unwrap();
        }
        self.writer.flush().await.unwrap();

        // Read response
        self.read_value().await
    }

    fn read_value(
        &mut self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = TestResponse> + '_>> {
        Box::pin(async move {
            let type_byte = self.read_byte().await;
            match type_byte {
                b'+' => {
                    let line = self.read_line().await;
                    TestResponse::String(line)
                }
                b'-' => {
                    let line = self.read_line().await;
                    TestResponse::Error(line)
                }
                b':' => {
                    let line = self.read_line().await;
                    let n: i64 = line.parse().unwrap();
                    TestResponse::Integer(n)
                }
                b'$' => {
                    let line = self.read_line().await;
                    let len: usize = line.parse().unwrap();
                    let mut buf = vec![0u8; len];
                    self.reader.read_exact(&mut buf).await.unwrap();
                    // consume trailing \r\n
                    let mut crlf = [0u8; 2];
                    self.reader.read_exact(&mut crlf).await.unwrap();
                    TestResponse::String(String::from_utf8(buf).unwrap())
                }
                b'*' => {
                    let line = self.read_line().await;
                    let count: usize = line.parse().unwrap();
                    let mut items = Vec::with_capacity(count);
                    for _ in 0..count {
                        items.push(self.read_value().await);
                    }
                    TestResponse::Array(items)
                }
                b'%' => {
                    let line = self.read_line().await;
                    let count: usize = line.parse().unwrap();
                    let mut entries = Vec::with_capacity(count);
                    for _ in 0..count {
                        let key = self.read_value().await;
                        let key_str = match key {
                            TestResponse::String(s) => s,
                            other => {
                                panic!("expected string map key, got {:?}", other.variant_name())
                            }
                        };
                        let value = self.read_value().await;
                        entries.push((key_str, value));
                    }
                    TestResponse::Map(entries)
                }
                b'_' => {
                    // Null: consume trailing \r\n
                    let mut crlf = [0u8; 2];
                    self.reader.read_exact(&mut crlf).await.unwrap();
                    TestResponse::Null
                }
                other => panic!("unexpected RESP3 type byte: 0x{other:02x}"),
            }
        })
    }

    async fn read_byte(&mut self) -> u8 {
        let mut b = [0u8; 1];
        self.reader.read_exact(&mut b).await.unwrap();
        b[0]
    }

    async fn read_line(&mut self) -> String {
        let mut line = String::new();
        self.reader.read_line(&mut line).await.unwrap();
        if line.ends_with('\n') {
            line.pop();
            if line.ends_with('\r') {
                line.pop();
            }
        }
        line
    }
}

// ---------------------------------------------------------------------------
// Response type
// ---------------------------------------------------------------------------

#[derive(Debug)]
#[allow(dead_code)]
pub enum TestResponse {
    String(String),
    Error(String),
    Integer(i64),
    Null,
    Array(Vec<TestResponse>),
    Map(Vec<(String, TestResponse)>),
}

impl TestResponse {
    pub fn as_str(&self) -> &str {
        match self {
            TestResponse::String(s) => s,
            _ => panic!("expected string, got {:?}", self.variant_name()),
        }
    }

    pub fn as_error(&self) -> &str {
        match self {
            TestResponse::Error(s) => s,
            _ => panic!("expected error, got {:?}", self.variant_name()),
        }
    }

    #[allow(dead_code)]
    pub fn as_int(&self) -> i64 {
        match self {
            TestResponse::Integer(n) => *n,
            _ => panic!("expected integer, got {:?}", self.variant_name()),
        }
    }

    #[allow(dead_code)]
    pub fn as_map(&self) -> &[(String, TestResponse)] {
        match self {
            TestResponse::Map(m) => m,
            _ => panic!("expected map, got {:?}", self.variant_name()),
        }
    }

    pub fn get(&self, key: &str) -> &TestResponse {
        match self {
            TestResponse::Map(entries) => entries
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v)
                .unwrap_or_else(|| panic!("key '{key}' not found in map: {:?}", self.map_keys())),
            _ => panic!("expected map, got {:?}", self.variant_name()),
        }
    }

    pub fn is_error(&self) -> bool {
        matches!(self, TestResponse::Error(_))
    }

    #[allow(dead_code)]
    pub fn is_null(&self) -> bool {
        matches!(self, TestResponse::Null)
    }

    pub fn variant_name(&self) -> &'static str {
        match self {
            TestResponse::String(_) => "String",
            TestResponse::Error(_) => "Error",
            TestResponse::Integer(_) => "Integer",
            TestResponse::Null => "Null",
            TestResponse::Array(_) => "Array",
            TestResponse::Map(_) => "Map",
        }
    }

    fn map_keys(&self) -> Vec<&str> {
        match self {
            TestResponse::Map(entries) => entries.iter().map(|(k, _)| k.as_str()).collect(),
            _ => vec![],
        }
    }
}
