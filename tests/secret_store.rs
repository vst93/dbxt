//! End-to-end check of the DBX Secret Store (v0.6.27+) path that dbxt relies on.
//!
//! dbxt opens the shared `dbx.db` through `dbx_mcp::backend::LocalBackend`,
//! which from v0.6.27 encrypts connection / plugin / AI / tunnel secrets with a
//! key that lives outside the database. dbxt never manages that key — it only
//! passes the kernel's resolution through — so this test drives the kernel the
//! same way dbxt does: a headless CLI with `DBX_SECRET_KEY_FILE` pointing at a
//! key file.
//!
//! The key material is generated at runtime and lives in a throwaway temp dir;
//! nothing secret is committed.

use dbx_core::models::connection::DatabaseType;
use dbx_mcp::backend::{new_connection_config, DbxBackend, LocalBackend};
use uuid::Uuid;

/// A 64-hex-character (32-byte) key, the shape `DBX_SECRET_KEY_FILE` accepts.
fn fresh_key_material() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

#[tokio::test(flavor = "multi_thread")]
async fn secret_store_round_trips_with_an_explicit_key_file() {
    let dir = std::env::temp_dir().join(format!("dbxt-secret-store-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let key_path = dir.join("secret.key");
    std::fs::write(&key_path, fresh_key_material()).unwrap();
    // The integration-test binary is its own process, so setting the process
    // env here cannot leak into the unit-test binary.
    std::env::set_var("DBX_SECRET_KEY_FILE", &key_path);

    let db_path = dir.join("dbx.db");
    let password = "dbxt-secret-store-pw";

    // First open: write a connection carrying a password.
    {
        let backend = LocalBackend::open(&db_path)
            .await
            .expect("open with a key file");
        let cfg = new_connection_config(
            "secret-1".into(),
            "secret store test".into(),
            DatabaseType::Mysql,
            "127.0.0.1".into(),
            3306,
            "dbxt".into(),
            password.into(),
            Some("shop".into()),
            false,
            None,
        )
        .unwrap();
        backend
            .add_connection_for_mcp(cfg)
            .await
            .expect("save connection with a password");
    }

    // The password must be encrypted at rest: an envelope, never the plaintext.
    // SQLite may still hold the latest pages in the `-wal` sidecar, so scan both.
    let mut raw = std::fs::read(&db_path).unwrap();
    let wal_path = db_path.with_extension("db-wal");
    if let Ok(wal) = std::fs::read(&wal_path) {
        raw.extend_from_slice(&wal);
    }
    let needle = password.as_bytes();
    assert!(
        !raw.windows(needle.len()).any(|w| w == needle),
        "the plaintext password leaked into dbx.db"
    );
    assert!(
        raw.windows(7).any(|w| w == b"dbxenc1"),
        "no dbxenc1 envelope found in dbx.db"
    );

    // Second open: the key file must decrypt the saved password back.
    {
        let backend = LocalBackend::open(&db_path)
            .await
            .expect("reopen with the same key file");
        let connections = backend.load_connections().await.expect("load connections");
        let got = connections
            .iter()
            .find(|c| c.id == "secret-1")
            .expect("the saved connection is listed");
        assert_eq!(got.password, password, "the password did not round-trip");
    }

    std::env::remove_var("DBX_SECRET_KEY_FILE");
    let _ = std::fs::remove_dir_all(&dir);
}
