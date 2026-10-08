//! End-to-end check that `dbxt mcp` serves DBX's own MCP server.
//!
//! The point is reuse: `dbxt mcp` must expose the same `dbx_*` tools the
//! official `dbx-mcp` binary does, because it constructs the same
//! `dbx_mcp::DbxMcpServer`. This test drives a real process over the stdio
//! transport (newline-delimited JSON-RPC, the framing MCP clients use) and
//! asserts the tool catalog comes back.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// A temp data dir with an explicit key file, so the child never touches the
/// developer's real `dbx.db` or the OS keychain.
fn temp_data_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("dbxt-mcp-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("secret.key"),
        format!("{}{}", "a".repeat(32), "b".repeat(32)),
    )
    .unwrap();
    dir
}

fn spawn_mcp(dir: &std::path::Path, extra: &[&str]) -> Child {
    let mut command = Command::new(env!("CARGO_BIN_EXE_dbxt"));
    command
        .arg("mcp")
        .args(extra)
        .env("DBX_DATA_DIR", dir)
        .env("DBX_SECRET_KEY_FILE", dir.join("secret.key"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    command.spawn().expect("spawn `dbxt mcp`")
}

/// Read stdout lines on a thread and hand them to the test, so a missing
/// response fails fast instead of hanging the suite.
fn line_reader(child: &mut Child) -> Receiver<String> {
    let stdout = child.stdout.take().expect("piped stdout");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            match line {
                Ok(line) => {
                    if tx.send(line).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
    rx
}

/// Receive lines until the JSON-RPC response with `id` arrives.
fn response_with_id(rx: &Receiver<String>, id: i64, timeout: Duration) -> Value {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let line = rx
            .recv_timeout(remaining)
            .unwrap_or_else(|_| panic!("no response for id {id} within {timeout:?}"));
        if let Ok(value) = serde_json::from_str::<Value>(&line) {
            if value.get("id").and_then(Value::as_i64) == Some(id) {
                return value;
            }
        }
    }
}

#[test]
fn stdio_lists_the_same_dbx_tools_as_the_official_server() {
    let dir = temp_data_dir("stdio");
    let mut child = spawn_mcp(&dir, &[]);
    let rx = line_reader(&mut child);
    let mut stdin = child.stdin.take().expect("piped stdin");

    // 1. initialize
    writeln!(
        stdin,
        "{}",
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "dbxt-mcp-test", "version": "0.0.0" }
            }
        })
    )
    .unwrap();
    let init = response_with_id(&rx, 1, Duration::from_secs(30));
    assert!(init.get("result").is_some(), "initialize failed: {init}");
    assert!(
        init["result"]["serverInfo"]["name"].as_str().is_some(),
        "no serverInfo: {init}"
    );

    // 2. initialized notification, then tools/list
    writeln!(
        stdin,
        "{}",
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })
    )
    .unwrap();
    writeln!(
        stdin,
        "{}",
        json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" })
    )
    .unwrap();
    let listed = response_with_id(&rx, 2, Duration::from_secs(30));

    let tools: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert!(
        tools.len() >= 20,
        "expected the DBX tool catalog, got {}: {tools:?}",
        tools.len()
    );
    for expected in [
        "dbx_list_connections",
        "dbx_list_tables",
        "dbx_describe_table",
        "dbx_execute_query",
    ] {
        assert!(tools.contains(&expected), "missing {expected} in {tools:?}");
    }

    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn mcp_help_is_answered_without_a_terminal() {
    let output = Command::new(env!("CARGO_BIN_EXE_dbxt"))
        .args(["mcp", "--help"])
        .output()
        .expect("run `dbxt mcp --help`");
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains("dbxt mcp"),
        "help does not describe the subcommand: {text}"
    );
    assert!(
        text.contains("DBX_MCP_HTTP_TOKEN"),
        "help does not name the HTTP token: {text}"
    );
}

#[test]
fn http_refuses_a_non_loopback_bind() {
    let dir = temp_data_dir("http-remote");
    let output = Command::new(env!("CARGO_BIN_EXE_dbxt"))
        .args(["mcp", "--http", "--host", "0.0.0.0"])
        .env("DBX_DATA_DIR", &dir)
        .env("DBX_SECRET_KEY_FILE", dir.join("secret.key"))
        .env("DBX_MCP_HTTP_TOKEN", "test-token")
        .output()
        .expect("run `dbxt mcp --http --host 0.0.0.0`");
    assert!(
        !output.status.success(),
        "non-loopback bind must be refused"
    );
    let text = String::from_utf8_lossy(&output.stderr);
    assert!(
        text.contains("回环") || text.to_ascii_lowercase().contains("loopback"),
        "unexpected error: {text}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
