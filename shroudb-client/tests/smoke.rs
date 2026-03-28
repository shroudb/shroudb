//! End-to-end smoke test: starts a ShrouDB server, connects with the client,
//! and runs through the core KV operations.
//!
//! Requires `cargo build -p shroudb` to have been run first.
//! Skips if the binary is not found.

use std::process::{Child, Command};
use std::time::Duration;

use shroudb_client::ShrouDBClient;

struct ServerGuard {
    child: Child,
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start_server(port: u16) -> Option<ServerGuard> {
    // The binary is built in the workspace target dir.
    // Try both current dir and CARGO_MANIFEST_DIR-relative paths.
    let candidates = [
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("target/debug/shroudb"),
        std::env::current_dir()
            .unwrap_or_default()
            .join("target/debug/shroudb"),
    ];
    let binary = candidates.iter().find(|p| p.exists())?;

    let dir = tempfile::tempdir().ok()?;
    let child = Command::new(binary)
        .arg("--bind")
        .arg(format!("127.0.0.1:{port}"))
        .arg("--data-dir")
        .arg(dir.path())
        .env("SHROUDB_MASTER_KEY", "00".repeat(32))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;

    Some(ServerGuard { child })
}

#[tokio::test]
async fn client_smoke_test() {
    let port = 16499;
    let Some(_guard) = start_server(port) else {
        eprintln!(
            "skipping smoke test: shroudb binary not found (run `cargo build -p shroudb` first)"
        );
        return;
    };

    // Wait for server to be ready
    tokio::time::sleep(Duration::from_secs(2)).await;

    let addr = format!("127.0.0.1:{port}");
    let mut client = ShrouDBClient::connect(&addr).await.unwrap();

    // PING
    client.ping().await.unwrap();

    // NAMESPACE CREATE
    client.namespace_create("smoke.test").await.unwrap();

    // PUT
    let v1 = client.put("smoke.test", b"key1", b"hello").await.unwrap();
    assert_eq!(v1, 1);

    // GET
    let entry = client.get("smoke.test", b"key1").await.unwrap();
    assert_eq!(entry.value, b"hello");
    assert_eq!(entry.version, 1);

    // PUT v2
    let v2 = client.put("smoke.test", b"key1", b"world").await.unwrap();
    assert_eq!(v2, 2);

    // GET latest
    let entry = client.get("smoke.test", b"key1").await.unwrap();
    assert_eq!(entry.value, b"world");
    assert_eq!(entry.version, 2);

    // GET specific version
    let entry = client.get_version("smoke.test", b"key1", 1).await.unwrap();
    assert_eq!(entry.value, b"hello");

    // DELETE
    let v3 = client.delete("smoke.test", b"key1").await.unwrap();
    assert_eq!(v3, 3);

    // GET after delete → error
    let err = client.get("smoke.test", b"key1").await;
    assert!(err.is_err());

    // VERSIONS
    let versions = client.versions("smoke.test", b"key1").await.unwrap();
    assert_eq!(versions.len(), 3);
    assert_eq!(versions[0].state, "Deleted");
    assert_eq!(versions[1].state, "Active");
    assert_eq!(versions[2].state, "Active");

    // LIST (empty after delete)
    let page = client.list("smoke.test").await.unwrap();
    assert!(page.items.is_empty());

    // NAMESPACE LIST
    let ns_page = client.namespace_list().await.unwrap();
    assert!(ns_page.items.contains(&"smoke.test".to_string()));

    // NAMESPACE INFO
    let info = client.namespace_info("smoke.test").await.unwrap();
    assert_eq!(info.name, "smoke.test");
    assert_eq!(info.key_count, 0);

    // HEALTH
    client.health().await.unwrap();

    // COMMAND LIST
    let commands = client.command_list().await.unwrap();
    assert!(commands.contains(&"PUT".to_string()));
    assert!(commands.contains(&"GET".to_string()));
    assert_eq!(commands.len(), 20);
}
