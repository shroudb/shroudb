//! Integration test harness for ShrouDB.
//!
//! Starts a real `shroudb` server process, waits for readiness via PING,
//! and provides connected clients for testing.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use shroudb_client::ShrouDBClient;

/// Master key used by all tests (32 zero bytes, hex-encoded).
pub const TEST_MASTER_KEY: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";

/// Find a free TCP port by binding to :0.
fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

/// Find the shroudb binary in the workspace target directory.
fn find_binary() -> Option<PathBuf> {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let candidates = [
        PathBuf::from(manifest_dir).join("../target/debug/shroudb"),
        PathBuf::from(manifest_dir).join("target/debug/shroudb"),
    ];
    candidates.into_iter().find(|p| p.exists())
}

/// Configuration for a test server instance.
#[derive(Default)]
pub struct TestServerConfig {
    pub auth_tokens: Vec<TestToken>,
    pub rate_limit: Option<u32>,
    pub webhooks: Vec<TestWebhook>,
    pub tls_cert: Option<PathBuf>,
    pub tls_key: Option<PathBuf>,
    pub master_key: Option<String>,
    /// Cache memory budget (e.g., "1kb", "256mb", "70%"). None = unlimited.
    pub cache_memory_budget: Option<String>,
}

pub struct TestToken {
    pub raw: String,
    pub tenant: String,
    pub actor: String,
    pub platform: bool,
    pub grants: Vec<TestGrant>,
}

pub struct TestGrant {
    pub namespace: String,
    pub scopes: Vec<String>,
}

pub struct TestWebhook {
    pub url: String,
    pub secret: String,
    pub events: Vec<String>,
    pub namespaces: Vec<String>,
}

/// A running test server instance. Killed on drop.
pub struct TestServer {
    child: Child,
    pub addr: String,
    pub data_dir: tempfile::TempDir,
    config_dir: tempfile::TempDir,
    pub master_key: String,
}

impl TestServer {
    /// Start a server with default config (no auth, no TLS).
    pub async fn start() -> Option<Self> {
        Self::start_with_config(TestServerConfig::default()).await
    }

    /// Start a server with custom config.
    pub async fn start_with_config(config: TestServerConfig) -> Option<Self> {
        let binary = find_binary()?;
        let port = free_port();
        let addr = format!("127.0.0.1:{port}");
        let data_dir = tempfile::tempdir().ok()?;
        let config_dir = tempfile::tempdir().ok()?;
        let master_key = config
            .master_key
            .clone()
            .unwrap_or_else(|| TEST_MASTER_KEY.to_string());

        // Generate config TOML
        let config_path = config_dir.path().join("config.toml");
        let toml = generate_config(&addr, &config);
        std::fs::write(&config_path, toml).ok()?;

        let child = Command::new(&binary)
            .arg("--config")
            .arg(&config_path)
            .arg("--data-dir")
            .arg(data_dir.path())
            .arg("--log-level")
            .arg("warn")
            .env("SHROUDB_MASTER_KEY", &master_key)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .ok()?;

        let mut server = Self {
            child,
            addr: addr.clone(),
            data_dir,
            config_dir,
            master_key,
        };

        // Wait for server to be ready (poll with PING, or TCP for TLS servers)
        let is_tls = config.tls_cert.is_some();
        let ready = if is_tls {
            server.wait_ready_tls(Duration::from_secs(10)).await
        } else {
            server.wait_ready(Duration::from_secs(10)).await
        };
        if !ready {
            eprintln!("server failed to start on port {port}");
            return None;
        }

        Some(server)
    }

    /// Connect a client to this server.
    pub async fn client(&self) -> ShrouDBClient {
        ShrouDBClient::connect(&self.addr).await.unwrap()
    }

    /// Connect a client and authenticate.
    pub async fn authed_client(&self, token: &str) -> ShrouDBClient {
        let mut client = self.client().await;
        client.auth(token).await.unwrap();
        client
    }

    /// Get the config file path (for hot-reload tests).
    pub fn config_path(&self) -> PathBuf {
        self.config_dir.path().join("config.toml")
    }

    /// Take ownership of the config dir (for offline subcommand tests).
    /// Prevents cleanup when the server is dropped.
    pub fn _config_dir_keep(&self) -> &tempfile::TempDir {
        &self.config_dir
    }

    /// Stop the server process without dropping temp dirs.
    pub fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// Send SIGKILL (hard crash, no graceful shutdown) then wait.
    #[cfg(unix)]
    pub fn kill_hard(&mut self) {
        unsafe {
            libc::kill(self.child.id() as libc::pid_t, libc::SIGKILL);
        }
        let _ = self.child.wait();
    }

    /// Wait for the server to respond to PING.
    async fn wait_ready(&mut self, timeout: Duration) -> bool {
        self.wait_ready_tcp(timeout).await
    }

    /// Wait for the server to accept a TCP connection and respond to PING.
    async fn wait_ready_tcp(&mut self, timeout: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if tokio::time::Instant::now() > deadline {
                return false;
            }
            if let Ok(mut client) = ShrouDBClient::connect(&self.addr).await
                && client.ping().await.is_ok()
            {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;

            // Check if process exited
            if let Some(status) = self.child.try_wait().ok().flatten() {
                eprintln!("server exited with status: {status}");
                return false;
            }
        }
    }

    /// Wait for a TLS server to be ready by checking if TCP connect succeeds
    /// (the server is listening). We cannot do a plain PING since TLS is required.
    async fn wait_ready_tls(&mut self, timeout: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if tokio::time::Instant::now() > deadline {
                return false;
            }
            // Just check if TCP connect succeeds (server is listening)
            if tokio::net::TcpStream::connect(&self.addr).await.is_ok() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;

            if let Some(status) = self.child.try_wait().ok().flatten() {
                eprintln!("server exited with status: {status}");
                return false;
            }
        }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Start a new server on an existing data directory.
/// Takes ownership of a pre-existing `TempDir` so it is not cleaned up.
pub async fn start_on_data_dir(
    data_dir: tempfile::TempDir,
    master_key: &str,
) -> Option<TestServer> {
    let binary = find_binary()?;
    let port = free_port();
    let addr = format!("127.0.0.1:{port}");
    let config_dir = tempfile::tempdir().ok()?;

    let config_path = config_dir.path().join("config.toml");
    let toml = generate_config(&addr, &TestServerConfig::default());
    std::fs::write(&config_path, toml).ok()?;

    let child = Command::new(&binary)
        .arg("--config")
        .arg(&config_path)
        .arg("--data-dir")
        .arg(data_dir.path())
        .arg("--log-level")
        .arg("warn")
        .env("SHROUDB_MASTER_KEY", master_key)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;

    let mut server = TestServer {
        child,
        addr: addr.clone(),
        data_dir,
        config_dir,
        master_key: master_key.to_string(),
    };

    if !server.wait_ready(Duration::from_secs(10)).await {
        eprintln!("server failed to start on port {port}");
        return None;
    }

    Some(server)
}

/// Start a server on an existing data directory path (for crash recovery tests).
/// The caller manages the directory lifetime.
pub async fn start_on_data_dir_path(data_dir: &Path, master_key: &str) -> Option<TestServer> {
    let binary = find_binary()?;
    let port = free_port();
    let addr = format!("127.0.0.1:{port}");

    // We need a config_dir TempDir for the struct, but the config already exists.
    // Create a new temp dir and copy the config into it.
    let config_dir = tempfile::tempdir().ok()?;
    let new_config_path = config_dir.path().join("config.toml");
    let toml = generate_config(&addr, &TestServerConfig::default());
    std::fs::write(&new_config_path, toml).ok()?;

    let child = Command::new(&binary)
        .arg("--config")
        .arg(&new_config_path)
        .arg("--data-dir")
        .arg(data_dir)
        .arg("--log-level")
        .arg("warn")
        .env("SHROUDB_MASTER_KEY", master_key)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;

    // We need a TempDir for data_dir field but we don't own it.
    // Create a dummy TempDir that points nowhere — the actual data lives at data_dir.
    let dummy_data_dir = tempfile::tempdir().ok()?;

    let mut server = TestServer {
        child,
        addr: addr.clone(),
        data_dir: dummy_data_dir,
        config_dir,
        master_key: master_key.to_string(),
    };

    if !server.wait_ready(Duration::from_secs(10)).await {
        eprintln!("server failed to start on port {port}");
        return None;
    }

    Some(server)
}

/// Run an offline subcommand (export, import, rekey, doctor) against a data dir.
/// Server must be stopped before calling this.
pub fn run_offline_subcommand(
    data_dir: &Path,
    config_path: &Path,
    args: &[&str],
) -> std::process::Output {
    run_offline_subcommand_with_key(data_dir, config_path, args, TEST_MASTER_KEY)
}

/// Run an offline subcommand with a custom master key.
pub fn run_offline_subcommand_with_key(
    data_dir: &Path,
    config_path: &Path,
    args: &[&str],
    master_key: &str,
) -> std::process::Output {
    let binary = find_binary().expect("shroudb binary not found");
    Command::new(&binary)
        .arg("--data-dir")
        .arg(data_dir)
        .arg("--config")
        .arg(config_path)
        .args(args)
        .env("SHROUDB_MASTER_KEY", master_key)
        .output()
        .expect("failed to run subcommand")
}

/// Start a server on an existing data directory path with cache config.
pub async fn start_on_data_dir_path_with_cache(
    data_dir: &Path,
    master_key: &str,
    cache_budget: &str,
) -> Option<TestServer> {
    let binary = find_binary()?;
    let port = free_port();
    let addr = format!("127.0.0.1:{port}");

    let config_dir = tempfile::tempdir().ok()?;
    let config = TestServerConfig {
        cache_memory_budget: Some(cache_budget.to_string()),
        ..Default::default()
    };
    let new_config_path = config_dir.path().join("config.toml");
    let toml = generate_config(&addr, &config);
    std::fs::write(&new_config_path, toml).ok()?;

    let child = Command::new(&binary)
        .arg("--config")
        .arg(&new_config_path)
        .arg("--data-dir")
        .arg(data_dir)
        .arg("--log-level")
        .arg("warn")
        .env("SHROUDB_MASTER_KEY", master_key)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;

    let dummy_data_dir = tempfile::tempdir().ok()?;

    let mut server = TestServer {
        child,
        addr: addr.clone(),
        data_dir: dummy_data_dir,
        config_dir,
        master_key: master_key.to_string(),
    };

    if !server.wait_ready(std::time::Duration::from_secs(10)).await {
        eprintln!("server failed to start on port {port} (cache restart)");
        return None;
    }

    Some(server)
}

fn generate_config(bind: &str, config: &TestServerConfig) -> String {
    let mut toml = format!(
        r#"[server]
bind = "{bind}"
"#
    );

    if let Some(ref cert_path) = config.tls_cert {
        toml.push_str(&format!("tls_cert = \"{}\"\n", cert_path.display()));
    }
    if let Some(ref key_path) = config.tls_key {
        toml.push_str(&format!("tls_key = \"{}\"\n", key_path.display()));
    }

    if let Some(limit) = config.rate_limit {
        toml.push_str(&format!("rate_limit_per_second = {limit}\n"));
    }

    toml.push_str("\n[storage]\n");

    if let Some(ref budget) = config.cache_memory_budget {
        toml.push_str(&format!(
            "\n[storage.cache]\nmemory_budget = \"{budget}\"\n"
        ));
    }

    if !config.auth_tokens.is_empty() {
        toml.push_str("\n[auth]\nmethod = \"token\"\n\n");
        for token in &config.auth_tokens {
            toml.push_str(&format!(
                "[auth.tokens.\"{}\"]\ntenant = \"{}\"\nactor = \"{}\"\nplatform = {}\n",
                token.raw, token.tenant, token.actor, token.platform
            ));
            if !token.grants.is_empty() {
                toml.push_str("grants = [\n");
                for grant in &token.grants {
                    let scopes: Vec<String> =
                        grant.scopes.iter().map(|s| format!("\"{s}\"")).collect();
                    toml.push_str(&format!(
                        "  {{ namespace = \"{}\", scopes = [{}] }},\n",
                        grant.namespace,
                        scopes.join(", ")
                    ));
                }
                toml.push_str("]\n");
            }
            toml.push('\n');
        }
    }

    for webhook in &config.webhooks {
        toml.push_str(&format!(
            "[[webhooks]]\nurl = \"{}\"\nsecret = \"{}\"\n",
            webhook.url, webhook.secret
        ));
        if !webhook.events.is_empty() {
            let events: Vec<String> = webhook.events.iter().map(|e| format!("\"{e}\"")).collect();
            toml.push_str(&format!("events = [{}]\n", events.join(", ")));
        }
        if !webhook.namespaces.is_empty() {
            let ns: Vec<String> = webhook
                .namespaces
                .iter()
                .map(|n| format!("\"{n}\""))
                .collect();
            toml.push_str(&format!("namespaces = [{}]\n", ns.join(", ")));
        }
        toml.push('\n');
    }

    toml
}
