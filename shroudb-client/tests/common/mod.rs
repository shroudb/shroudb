use std::net::SocketAddr;
use std::path::Path;
use std::process::{Child, Command as StdCommand};

/// A test server that spawns the actual `shroudb` binary on a random port.
pub struct TestServer {
    child: Child,
    pub addr: SocketAddr,
    _data_dir: Option<tempfile::TempDir>,
}

impl TestServer {
    pub async fn start() -> Self {
        let data_dir = tempfile::tempdir().unwrap();
        let mut server = Self::start_inner(data_dir.path(), &"ab".repeat(32)).await;
        server._data_dir = Some(data_dir);
        server
    }

    async fn start_inner(data_dir: &Path, master_key: &str) -> Self {
        let port = find_free_port().await;
        let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();

        let config_path = data_dir.join("config.toml");
        let config = format!(
            r#"
[server]
bind = "127.0.0.1:{port}"
metrics_bind = "127.0.0.1:0"

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
"#,
            data_dir.display()
        );
        std::fs::create_dir_all(data_dir).unwrap();
        std::fs::write(&config_path, &config).unwrap();

        // Find the shroudb binary in the workspace target directory.
        // When run via `cargo test --workspace`, CARGO_BIN_EXE is not available
        // for binaries from other crates. We locate it via cargo metadata.
        let binary = find_shroudb_binary();

        let child = StdCommand::new(&binary)
            .arg("--config")
            .arg(&config_path)
            .env("SHROUDB_MASTER_KEY", master_key)
            .env("RUST_LOG", "warn")
            .spawn()
            .unwrap_or_else(|e| panic!("failed to start shroudb at {}: {e}", binary.display()));

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
}

impl Drop for TestServer {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            unsafe {
                libc::kill(self.child.id() as libc::pid_t, libc::SIGTERM);
            }
            let _ = self.child.wait();
        }
        #[cfg(not(unix))]
        {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

async fn find_free_port() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    listener.local_addr().unwrap().port()
}

/// Locate the `shroudb` binary in the workspace target directory.
fn find_shroudb_binary() -> std::path::PathBuf {
    // Try the standard cargo-built location relative to the test binary
    let mut path = std::env::current_exe().unwrap();
    // Go up from target/debug/deps/<test-binary> to target/debug/
    path.pop(); // deps
    path.pop(); // debug (or release)
    path.push("shroudb");

    if path.exists() {
        return path;
    }

    // Fallback: try with .exe extension on Windows
    path.set_extension("exe");
    if path.exists() {
        return path;
    }

    panic!(
        "Could not find shroudb binary. Build it first with `cargo build -p shroudb`. \
         Searched at: {}",
        path.display()
    );
}
