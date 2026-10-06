//! R116: first-run encryption upgrade for the shared DBX store.
//!
//! Older DBX releases persisted connection / plugin / AI / tunnel secrets as
//! plaintext inside `dbx.db`. Recent kernels store an AES-GCM envelope instead
//! and keep the key *outside* the database (OS keychain, or
//! `DBX_SECRET_KEY_FILE` / `DBX_SECRET_KEY`). The desktop app offers a "data
//! security upgrade" wizard, but a user who only has dbxt — desktop already
//! uninstalled — used to hit `DATA_MIGRATION_REQUIRED` with no way forward.
//!
//! This module runs that same kernel migration for dbxt: it inspects the store
//! read-only, and only when legacy plaintext data is present does it provision
//! a key (mirroring the desktop's `PlatformDefault` lifecycle) and call
//! [`Storage::start_data_migration`], which backs the database up first and
//! verifies every secret afterwards before the TUI opens the store.

use std::path::Path;

use dbx_core::persistence::secret_codec::SecretKeyPolicy;
use dbx_core::storage::{MigrationPreflight, Storage};

use crate::prelude::*;

/// The first DBX release that introduced the `dbxenc1` secret store this
/// migration produces (commit `250e0c0a1`, "encrypt local credentials and guide
/// data migration"). The envelope prefix, AAD and `secret-store-v1` id are
/// byte-for-byte unchanged from here through the kernel dbxt builds against, so
/// any DBX at or above this version can read the upgraded store.
const MIN_DBX_VERSION: &str = "v0.6.21";

/// Ensure the shared store is encrypted before the TUI reads it.
///
/// Returns `Ok(true)` when this call performed the upgrade and `Ok(false)`
/// when the store was already encrypted (or a fresh, empty profile). Any
/// failure leaves the kernel's own backup in place and is reported as a
/// user-actionable string.
pub(crate) async fn ensure_encrypted_store(db_path: &Path) -> Result<bool, String> {
    // `PlatformDefault` + lazy creation is the same key lifecycle the desktop
    // wizard uses: an OS-keychain entry where one is reachable, otherwise the
    // managed per-user fallback file. A key created here is exactly the key the
    // desktop app would later read, so installing it again keeps working.
    let storage = Storage::open_unmigrated(db_path)
        .await?
        .with_secret_key_policy(SecretKeyPolicy::PlatformDefault)
        .with_secret_key_creation(true);
    let preflight = storage.inspect_data_migration().await?;
    if preflight.is_ready() || !preflight.needs_migration {
        return Ok(false);
    }
    // A key is required, and for plaintext-only data the kernel may create one.
    // If neither is possible the user has to supply the original key.
    if !preflight.key_provider_available && !preflight.key_creation_allowed {
        return Err(migration_blocked_message(&preflight));
    }
    if !confirm_migration(&preflight) {
        return Err(t(
            "已取消 DBX 数据加密升级：库仍为明文，dbxt 无法打开。请重新运行并确认；若在脚本 / 无终端环境，请设置 DBXT_ASSUME_YES=1 后重试。",
        )
        .to_string());
    }

    eprintln!("{}", t("正在升级为加密存储…"));
    let report = storage.start_data_migration().await.map_err(|error| {
        tf(
            "DBX 数据加密升级失败：{}（原始数据已保留，可修正后重试）",
            &[&error],
        )
    })?;
    match report.backup_path.as_deref() {
        Some(backup) => eprintln!(
            "{}",
            tf(
                "升级完成：已加密 {} 项密钥；备份位于 {}",
                &[&report.verified_secret_count, &backup],
            )
        ),
        None => eprintln!(
            "{}",
            tf("升级完成：已加密 {} 项密钥", &[&report.verified_secret_count])
        ),
    }
    Ok(true)
}

/// Print what the upgrade will do — and which DBX version is needed to read
/// the result — then ask for confirmation. A non-interactive stdin declines
/// unless `DBXT_ASSUME_YES` opts in, so a piped run never rewrites the store by
/// surprise.
fn confirm_migration(preflight: &MigrationPreflight) -> bool {
    let legacy_files = preflight.legacy_json_files.iter().filter(|file| file.exists).count();
    eprintln!(
        "{}",
        tf(
            "检测到 DBX 数据尚未加密：{} 项明文凭据、{} 个旧版明文文件。",
            &[&preflight.database_plaintext_count, &legacy_files],
        )
    );
    eprintln!("{}", t("升级会先自动备份 dbx.db，再加密全部敏感字段并校验。"));
    eprintln!(
        "{}",
        tf(
            "升级后该库需要 DBX {} 或更高版本（桌面端 / CLI / dbxt）才能读取。",
            &[&MIN_DBX_VERSION],
        )
    );
    if assume_yes() {
        return true;
    }
    if !std::io::stdin().is_terminal() {
        return false;
    }
    eprint!("{}", t("是否现在升级？[Y/n] "));
    let _ = std::io::stderr().flush();
    let mut answer = String::new();
    let confirmed = match std::io::stdin().read_line(&mut answer) {
        Ok(0) | Err(_) => false,
        Ok(_) => {
            let answer = answer.trim().to_ascii_lowercase();
            answer.is_empty() || matches!(answer.as_str(), "y" | "yes" | "是")
        }
    };
    // Leave the cursor on a fresh line so the caller's message does not run
    // into the prompt when stdin is piped (no terminal echo).
    eprintln!();
    confirmed
}

/// `DBXT_ASSUME_YES=1|true|yes` skips the interactive confirmation. Intended
/// for provisioning scripts; the upgrade still takes its own backup first.
fn assume_yes() -> bool {
    std::env::var("DBXT_ASSUME_YES")
        .map(|value| matches!(value.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false)
}

/// Turn the kernel's preflight into the one instruction the user can act on:
/// either "supply the original key" (ciphertext already exists) or "no key
/// provider is reachable" (keychain unavailable and nothing to encrypt yet).
fn migration_blocked_message(preflight: &MigrationPreflight) -> String {
    match preflight.error_code.as_deref() {
        Some("ENCRYPTED_DATA_KEY_MISSING")
        | Some("SECRET_KEY_MISMATCH")
        | Some("SECRET_KEY_INVALID")
        | Some("MISSING_EXTERNAL_KEY")
        | Some("KEY_FILE_UNAVAILABLE")
        | Some("KEYRING_ACCESS_FAILED") => tf(
            "DBX 数据需要加密升级，但读不到正确的数据加密密钥（{}）：请用 DBX_SECRET_KEY_FILE / DBX_SECRET_KEY 提供创建该库时所用的密钥。",
            &[&preflight.error_code.as_deref().unwrap_or("KEY_UNAVAILABLE")],
        ),
        _ => t(
            "DBX 数据需要加密升级，但当前进程既无法访问系统钥匙串，也无法创建本地密钥：请使用带系统钥匙串支持的构建，或用 DBX_SECRET_KEY_FILE / DBX_SECRET_KEY 提供密钥。",
        )
        .to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use dbx_core::models::connection::DatabaseType;
    use dbx_mcp::backend::{new_connection_config, DbxBackend, LocalBackend};

    /// A 64-hex-character (32-byte) key, the shape `DBX_SECRET_KEY_FILE`
    /// accepts. Generated per test run; nothing secret is committed.
    fn fresh_key_material() -> String {
        format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple())
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn migrates_legacy_plaintext_connections_and_is_idempotent() {
        let dir = std::env::temp_dir().join(format!("dbxt-migrate-{}", uuid::Uuid::new_v4()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let key_path = dir.join("secret.key");
        std::fs::write(&key_path, fresh_key_material()).unwrap();
        // The unit-test binary is its own process; pointing the kernel at an
        // explicit key file keeps the migration hermetic (no real keychain),
        // and `DBXT_ASSUME_YES` skips the interactive confirmation (stdin is
        // not a terminal under `cargo test`). This test is the only one that
        // touches these variables, so the two scenarios run in sequence rather
        // than racing each other.
        std::env::set_var("DBX_SECRET_KEY_FILE", &key_path);
        std::env::set_var("DBXT_ASSUME_YES", "1");

        // A fresh, empty profile has nothing to upgrade.
        let fresh = dir.join("fresh").join("dbx.db");
        std::fs::create_dir_all(fresh.parent().unwrap()).unwrap();
        assert!(!ensure_encrypted_store(&fresh).await.unwrap(), "an empty profile has nothing to migrate");

        // A legacy profile: `connections.json` still carries the plaintext
        // password, and no encrypted rows exist yet.
        let password = "dbxt-legacy-plaintext-pw";
        let cfg = new_connection_config(
            "legacy-1".into(),
            "legacy connection".into(),
            DatabaseType::Mysql,
            "127.0.0.1".into(),
            3306,
            "legacy".into(),
            password.into(),
            None,
            false,
            None,
        )
        .unwrap();
        std::fs::write(dir.join("connections.json"), serde_json::to_string(&vec![cfg]).unwrap()).unwrap();

        let db_path = dir.join("dbx.db");
        assert!(
            ensure_encrypted_store(&db_path).await.unwrap(),
            "the legacy profile should have been upgraded"
        );

        // The upgrade left a backup and the legacy file renamed aside.
        assert!(dir.join("connections.json.bak").exists(), "legacy file not finalized");

        // The password is now stored as an envelope, never as plaintext.
        let mut raw = std::fs::read(&db_path).unwrap();
        if let Ok(wal) = std::fs::read(db_path.with_extension("db-wal")) {
            raw.extend_from_slice(&wal);
        }
        assert!(
            !contains(&raw, password.as_bytes()),
            "the plaintext password survived the upgrade"
        );
        assert!(contains(&raw, b"dbxenc1"), "no dbxenc1 envelope was written");

        // The migrated store opens through the normal dbxt path and the secret
        // round-trips.
        let backend = LocalBackend::open(&db_path).await.expect("open after upgrade");
        let connections = backend.load_connections().await.unwrap();
        let got = connections.iter().find(|c| c.id == "legacy-1").expect("connection listed");
        assert_eq!(got.password, password, "the password did not round-trip");

        // A second startup is a no-op.
        assert!(!ensure_encrypted_store(&db_path).await.unwrap(), "second run must not re-migrate");

        std::env::remove_var("DBX_SECRET_KEY_FILE");
        std::env::remove_var("DBXT_ASSUME_YES");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
