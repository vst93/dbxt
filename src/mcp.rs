//! R117: `dbxt mcp` — serve DBX's own MCP server from the dbxt binary.
//!
//! Nothing here implements the Model Context Protocol, a tool, a schema, a
//! policy, a session or a transaction. The server is
//! [`dbx_mcp::DbxMcpServer`] — the exact struct the official `dbx-mcp` binary
//! serves — and the HTTP transport is [`dbx_mcp::serve_streamable_http`]. This
//! module only does what `dbx-mcp/src/main.rs` does: pick a backend, pick a
//! transport, and hand the process over to the kernel's server. So a client
//! talking to `dbxt mcp` gets byte-for-byte the same tools, resources, policy
//! and session semantics as the official server.
//!
//! Two modes:
//! * `dbxt mcp` — stdio (the transport Claude Code, Cursor, Codex and friends
//!   spawn by default).
//! * `dbxt mcp --http` — loopback Streamable HTTP with a bearer token, for
//!   HTTP-capable clients. Remote binding is deliberately not offered here;
//!   use DBX's own `dbx-mcp --http` for that.

use std::net::{IpAddr, SocketAddr};
use std::path::Path;

use dbx_mcp::{DbxMcpServer, HttpAuth, HttpRuntimeConfig, UnavailableBackend};

use crate::prelude::*;

/// Default HTTP port, matching `dbx-mcp`'s own default. If DBX Desktop already
/// serves Streamable HTTP on 5225, pick another with `--port`.
const DEFAULT_HTTP_PORT: u16 = 5225;
const DEFAULT_HTTP_HOST: &str = "127.0.0.1";
const DEFAULT_HTTP_PATH: &str = "/mcp";

/// The parsed `dbxt mcp …` command line.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct McpOptions {
    pub(crate) store: Option<String>,
    pub(crate) http: Option<HttpOptions>,
    pub(crate) help: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct HttpOptions {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) path: String,
}

impl Default for HttpOptions {
    fn default() -> Self {
        Self {
            host: std::env::var("DBX_MCP_HTTP_HOST").unwrap_or_else(|_| DEFAULT_HTTP_HOST.to_string()),
            port: std::env::var("DBX_MCP_HTTP_PORT").ok().and_then(|v| v.parse().ok()).unwrap_or(DEFAULT_HTTP_PORT),
            path: std::env::var("DBX_MCP_HTTP_PATH").unwrap_or_else(|_| DEFAULT_HTTP_PATH.to_string()),
        }
    }
}

/// Parse the arguments after `mcp`. `--http` (or `DBX_MCP_TRANSPORT=http`)
/// selects Streamable HTTP; anything else is the stdio transport.
pub(crate) fn parse_mcp_args(args: &[String]) -> std::result::Result<McpOptions, String> {
    let transport_env = std::env::var("DBX_MCP_TRANSPORT").unwrap_or_default();
    let env_http = matches!(
        transport_env.trim().to_ascii_lowercase().as_str(),
        "http" | "streamable-http" | "streamable_http"
    );
    let mut opts = McpOptions { store: None, http: env_http.then(HttpOptions::default), help: false };

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => opts.help = true,
            "--http" => {
                opts.http.get_or_insert_with(HttpOptions::default);
            }
            "--host" | "--http-host" => {
                let value = next_value(args, &mut i, "--host")?;
                http_opts(&mut opts).host = value;
            }
            "--port" | "--http-port" => {
                let value = next_value(args, &mut i, "--port")?;
                http_opts(&mut opts).port = value
                    .parse()
                    .map_err(|_| tf("{} 需要 1-65535 之间的端口号", &[&"--port"]))?;
            }
            "--path" | "--http-path" => {
                let value = next_value(args, &mut i, "--path")?;
                http_opts(&mut opts).path = value;
            }
            "--store" => {
                opts.store = Some(next_value(args, &mut i, "--store")?);
            }
            other if other.len() > 1 && other.starts_with('-') => {
                return Err(tf("未知选项: {}", &[&other]));
            }
            other => {
                // A bare word is the store path, matching `dbxt <store>`.
                opts.store = Some(other.to_string());
            }
        }
        i += 1;
    }
    Ok(opts)
}

/// Consume the value following a flag.
fn next_value(args: &[String], index: &mut usize, flag: &str) -> std::result::Result<String, String> {
    *index += 1;
    args.get(*index).filter(|v| !v.starts_with("--")).cloned().ok_or_else(|| tf("{} 需要值", &[&flag]))
}

/// The HTTP options, materialised on first use so `--host` works without `--http`.
fn http_opts(opts: &mut McpOptions) -> &mut HttpOptions {
    opts.http.get_or_insert_with(HttpOptions::default)
}

/// `dbxt mcp`: run the kernel's MCP server until the client disconnects (stdio)
/// or the process is stopped (HTTP).
pub(crate) async fn run_mcp(args: &[String]) -> Result<()> {
    if args.iter().any(|a| a == "-V" || a == "--version") {
        crate::write_stdout(&format!("{}\n", crate::version_line()))?;
        return Ok(());
    }
    let opts = match parse_mcp_args(args) {
        Ok(opts) => opts,
        Err(error) => {
            crate::write_stderr(&format!("{error}\n\n{}", mcp_help_text()));
            std::process::exit(2);
        }
    };
    if opts.help {
        crate::write_stdout(&mcp_help_text())?;
        return Ok(());
    }

    let db_path = resolve_store(opts.store.as_deref())?;
    let backend = open_backend(&db_path).await;
    match opts.http {
        None => serve_stdio(backend).await,
        Some(http) => serve_http(backend, http).await,
    }
}

/// Resolve the `dbx.db` path the same way the TUI does: an explicit path/`--store`
/// wins, otherwise `DBX_DATA_DIR` (via [`storage_db_path`]) or the platform default.
fn resolve_store(store: Option<&str>) -> Result<PathBuf> {
    let mut path = match store {
        Some(p) => PathBuf::from(p),
        None => storage_db_path().map_err(|e| anyhow::anyhow!(e))?,
    };
    if path.is_dir() {
        path = path.join("dbx.db");
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(path)
}

/// Build the backend. On any failure serve [`UnavailableBackend`] instead of
/// exiting, so the client gets a real JSON-RPC error on every call rather than
/// a silent EOF — the same contract the official `dbx-mcp` uses.
async fn open_backend(db_path: &Path) -> Arc<dyn DbxBackend> {
    if let Err(reason) = ensure_encrypted_store(db_path).await {
        eprintln!("{}", tf("DBX 存储不可用：{}", &[&reason]));
        return Arc::new(UnavailableBackend::new(reason));
    }
    match LocalBackend::open(db_path).await {
        Ok(backend) => Arc::new(backend),
        Err(reason) => {
            let reason = humanize_backend_error(&reason);
            eprintln!("{}", tf("DBX 存储不可用：{}", &[&reason]));
            Arc::new(UnavailableBackend::new(reason))
        }
    }
}

/// stdio transport: the server reads and writes newline-delimited JSON-RPC on
/// the process's stdin/stdout.
async fn serve_stdio(backend: Arc<dyn DbxBackend>) -> Result<()> {
    use rmcp::ServiceExt;
    let transport = dbx_mcp::with_legacy_discovery_fallback(rmcp::transport::stdio());
    let service = DbxMcpServer::new(backend).serve(transport).await?;
    service.waiting().await?;
    Ok(())
}

/// Loopback Streamable HTTP transport. DBX's own `serve_streamable_http`
/// installs a Ctrl-C shutdown handler, so a plain `dbxt mcp --http` process
/// stops cleanly on interrupt.
async fn serve_http(backend: Arc<dyn DbxBackend>, opts: HttpOptions) -> Result<()> {
    let ip: IpAddr = opts.host.parse().map_err(|_| anyhow::anyhow!(tf("无效的监听地址：{}", &[&opts.host])))?;
    if !ip.is_loopback() {
        return Err(anyhow::anyhow!(t(
            "dbxt 的 MCP HTTP 只监听本机回环地址；需要远程访问请改用 DBX 自带的 dbx-mcp --http",
        )));
    }
    validate_http_path(&opts.path)?;
    let token = http_token().map_err(|e| anyhow::anyhow!(e))?;
    // Loopback + browser origins from localhost; the bearer token is still
    // required for every request, exactly as `dbx-mcp --http` does.
    let auth = HttpAuth::new(token, Vec::<String>::new(), true).map_err(|e| anyhow::anyhow!(e))?;
    let allowed_hosts = vec!["localhost".to_string(), "127.0.0.1".to_string(), "[::1]".to_string()];
    let config = HttpRuntimeConfig::new(SocketAddr::new(ip, opts.port), opts.path, auth, allowed_hosts);
    // `serve_streamable_http` is not re-exported from the crate root, but the
    // `http` module is public, and this is DBX's own listener + Ctrl-C shutdown.
    dbx_mcp::http::serve_streamable_http(backend, config).await.map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(())
}

/// Same shape `dbx-mcp` accepts: `DBX_MCP_HTTP_TOKEN` or `DBX_MCP_HTTP_TOKEN_FILE`.
fn http_token() -> std::result::Result<String, String> {
    let inline = std::env::var("DBX_MCP_HTTP_TOKEN").ok().filter(|v| !v.trim().is_empty());
    let file = std::env::var("DBX_MCP_HTTP_TOKEN_FILE").ok().filter(|v| !v.trim().is_empty());
    match (inline, file) {
        (Some(_), Some(_)) => Err(t("只能设置 DBX_MCP_HTTP_TOKEN 或 DBX_MCP_HTTP_TOKEN_FILE 之一").to_string()),
        (Some(token), None) => Ok(token),
        (None, Some(path)) => std::fs::read_to_string(&path)
            .map_err(|e| tf("读取 DBX_MCP_HTTP_TOKEN_FILE 失败：{}", &[&e]))
            .and_then(|token| {
                let token = token.trim_end_matches(['\r', '\n']).to_string();
                (!token.is_empty()).then_some(token).ok_or_else(|| t("DBX_MCP_HTTP_TOKEN_FILE 为空").to_string())
            }),
        (None, None) => Err(t("Streamable HTTP 需要 DBX_MCP_HTTP_TOKEN 或 DBX_MCP_HTTP_TOKEN_FILE").to_string()),
    }
}

/// Mirror `dbx-mcp`'s path rule: absolute, no trailing slash, no query/fragment.
fn validate_http_path(path: &str) -> Result<()> {
    if path == "/" || !path.starts_with('/') || path.ends_with('/') || path.contains('?') || path.contains('#') {
        return Err(anyhow::anyhow!(t("HTTP 路径必须是 / 开头的绝对路径，且不能以 / 结尾（例如 /mcp）")));
    }
    Ok(())
}

/// `dbxt mcp --help`.
pub(crate) fn mcp_help_text() -> String {
    format!(
        "dbxt {} — {}\n\n{}: dbxt mcp [--store PATH] [--http [--host H] [--port P] [--path P]]\n\n{}:\n{}\n{}\n{}\n{}\n{}\n{}\n\n{}:\n{}\n{}\n{}\n\n{}:\n{}\n{}\n\n{}: https://github.com/vst93/dbxt\n",
        crate::dbxt_version(),
        t("以 DBX 原生 MCP 服务运行"),
        t("用法"),
        t("选项"),
        t("  --store PATH  指定 dbx.db（默认：DBX_DATA_DIR 或平台默认位置）"),
        t("  --http        改用 Streamable HTTP 监听（默认 stdio）"),
        t("  --host H      仅回环地址，默认 127.0.0.1"),
        t("  --port P      HTTP 端口，默认 5225"),
        t("  --path P      HTTP 路径，默认 /mcp"),
        t("  -h, --help    显示本帮助"),
        t("HTTP 环境变量"),
        t("  DBX_MCP_HTTP_TOKEN / DBX_MCP_HTTP_TOKEN_FILE  Bearer 令牌（必填）"),
        t("  DBX_MCP_HTTP_HOST / _PORT / _PATH            监听地址"),
        t("  DBX_MCP_TRANSPORT=http                       等价于 --http"),
        t("说明"),
        t("  工具 / 资源 / 会话 / 事务 / 权限策略全部来自 DBX 的 DbxMcpServer，"),
        t("  与桌面端、dbxt 共用同一个 dbx.db；dbxt 只提供入口，不重写任何 MCP 逻辑。"),
        t("文档"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// `DBX_MCP_TRANSPORT` is process-global, so the cases below take turns
    /// instead of observing each other's value under parallel test threads.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn lock() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn defaults_to_stdio() {
        let _guard = lock();
        std::env::remove_var("DBX_MCP_TRANSPORT");
        let opts = parse_mcp_args(&args(&[])).unwrap();
        assert_eq!(opts.http, None);
        assert_eq!(opts.store, None);
        assert!(!opts.help);
    }

    #[test]
    fn http_flag_and_overrides() {
        let _guard = lock();
        std::env::remove_var("DBX_MCP_TRANSPORT");
        let opts = parse_mcp_args(&args(&["--http", "--host", "127.0.0.1", "--port", "5300", "--path", "/dbx"])).unwrap();
        let http = opts.http.expect("http mode");
        assert_eq!(http.host, "127.0.0.1");
        assert_eq!(http.port, 5300);
        assert_eq!(http.path, "/dbx");
    }

    #[test]
    fn store_can_be_a_flag_or_a_bare_word() {
        let _guard = lock();
        std::env::remove_var("DBX_MCP_TRANSPORT");
        assert_eq!(parse_mcp_args(&args(&["--store", "/tmp/a"])).unwrap().store.as_deref(), Some("/tmp/a"));
        assert_eq!(parse_mcp_args(&args(&["/tmp/b"])).unwrap().store.as_deref(), Some("/tmp/b"));
    }

    #[test]
    fn unknown_flag_and_missing_value_are_errors() {
        let _guard = lock();
        std::env::remove_var("DBX_MCP_TRANSPORT");
        assert!(parse_mcp_args(&args(&["--nope"])).is_err());
        assert!(parse_mcp_args(&args(&["--port"])).is_err());
        assert!(parse_mcp_args(&args(&["--port", "not-a-port"])).is_err());
    }

    #[test]
    fn env_transport_selects_http() {
        let _guard = lock();
        std::env::set_var("DBX_MCP_TRANSPORT", "streamable-http");
        let selected = parse_mcp_args(&args(&[])).unwrap().http.is_some();
        std::env::remove_var("DBX_MCP_TRANSPORT");
        assert!(selected);
    }

    #[test]
    fn http_path_validation_matches_dbx() {
        assert!(validate_http_path("/mcp").is_ok());
        assert!(validate_http_path("/").is_err());
        assert!(validate_http_path("mcp").is_err());
        assert!(validate_http_path("/mcp/").is_err());
        assert!(validate_http_path("/mcp?x=1").is_err());
    }
}
