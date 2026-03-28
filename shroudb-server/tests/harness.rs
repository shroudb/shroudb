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
const TEST_MASTER_KEY: &str = "0000000000000000000000000000000000000000000000000000000000000000";

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
            .env("SHROUDB_MASTER_KEY", TEST_MASTER_KEY)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .ok()?;

        let mut server = Self {
            child,
            addr: addr.clone(),
            data_dir,
            config_dir,
        };

        // Wait for server to be ready (poll with PING)
        if !server.wait_ready(Duration::from_secs(10)).await {
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


    /// Wait for the server to respond to PING.
    async fn wait_ready(&mut self, timeout: Duration) -> bool {
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
}

impl Drop for TestServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Run an offline subcommand (export, import, rekey, doctor) against a data dir.
/// Server must be stopped before calling this.
pub fn run_offline_subcommand(
    data_dir: &Path,
    config_path: &Path,
    args: &[&str],
) -> std::process::Output {
    let binary = find_binary().expect("shroudb binary not found");
    Command::new(&binary)
        .arg("--data-dir")
        .arg(data_dir)
        .arg("--config")
        .arg(config_path)
        .args(args)
        .env("SHROUDB_MASTER_KEY", TEST_MASTER_KEY)
        .output()
        .expect("failed to run subcommand")
}

fn generate_config(bind: &str, config: &TestServerConfig) -> String {
    let mut toml = format!(
        r#"[server]
bind = "{bind}"
"#
    );

    if let Some(limit) = config.rate_limit {
        toml.push_str(&format!("rate_limit_per_second = {limit}\n"));
    }

    toml.push_str("\n[storage]\n");

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
