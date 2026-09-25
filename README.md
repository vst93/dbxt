# dbxt

**English** | [中文](README.zh-CN.md)

A keyboard-and-mouse friendly terminal UI for databases, built on the [DBX](https://github.com/t8y2/dbx) kernel. Configure your connections once — in DBX Desktop, the DBX CLI, or dbxt itself — and use them from any terminal.

## Status

Early but usable. Verified end-to-end against real MySQL 8.4, Redis and MongoDB servers:

- ✅ Launch, connection picker, in-TUI connection creation
- ✅ Connect to MySQL, browse databases/tables
- ✅ **Table data browser**: `Enter` on a table runs a paginated `SELECT *` (50 rows per page, `n`/`p` to page), with a row-number gutter, content-sized columns, and the total row count
- ✅ **Table structure**: `r` shows the field list (type / key / nullable / default / comment); `t` toggles the `SHOW CREATE TABLE` DDL (dialect-aware, built by the DBX kernel)
- ✅ **Result grid**: the header stays in sync with `h`/`l` column scrolling, `NULL` (italic) and the empty string (`''`) render differently, and execution time / affected rows are shown
- ✅ **DML**: `INSERT`/`UPDATE`/`DELETE` report affected rows; `DROP`/`TRUNCATE` and `WHERE`-less `UPDATE`/`DELETE` pop a red confirmation before running
- ✅ **Multi-statement scripts**: `a; b; c;` runs as a batch and shows one row per statement; `Enter` drills into a statement's result set
- ✅ **SQL editor**: multi-line, shell-style `↑`/`↓` history (seeded from DBX's shared query history), the results pane takes focus after a run
- ✅ MySQL server errors surfaced verbatim in the status line
- ✅ Adaptive layout at 120×32 (desktop) and 42×22 (narrow pane / phone portrait)
- ✅ Redis: connect, `SET`/`GET`/`KEYS`/`DBSIZE`, quoted args, and `[`/`]` db switching verified end-to-end
- ✅ MongoDB: `db.col.find({})`, `use <db>` database switching, and multi-row output verified end-to-end
- ⚠️ Cross-platform release builds (Windows / macOS / Android-Termux) planned, not yet verified

## Relationship to DBX

This project would not exist without [DBX](https://github.com/t8y2/dbx) by t8y2 (Apache-2.0). It is **not** a fork and not affiliated — it *embeds* DBX's Rust kernel as library dependencies and adds a TUI front-end on top.

```
DBX Desktop (Tauri)   DBX CLI   DBX MCP   dbxt (this project)
        │                 │         │            │
        └────────────┬────┴─────────┴─────┬──────┘
                     ▼                    ▼
              dbx-core (business layer)  dbx-mcp (LocalBackend)
                     │
              native drivers for 90+ databases
                     │
              shared connection store: dbx.db
```

- `dbx-core` + `dbx-mcp` are used as **git dependencies pinned to tag `v0.6.9`**. dbxt calls `dbx_mcp::backend::LocalBackend` directly, in-process: connection CRUD, metadata, SQL execution, batch, transactions, Redis and MongoDB commands all go through the same code paths DBX Desktop uses.
- **No desktop app, no Node.js, no daemon, no HTTP server.** One static binary; everything runs in your terminal.
- The `[patch.crates-io]` section in `Cargo.toml` mirrors DBX's own workspace patches (a gaussdb-compatible `tokio-postgres` fork and a `mysql_async` fork). Cargo does not propagate `[patch]` sections from git dependencies, so dbxt must re-declare them — if you bump the DBX tag, re-check this section against the corresponding DBX `Cargo.toml`.

### Database coverage

Follows DBX's own execution model:

| Tier | Databases | Works in dbxt? |
| --- | --- | --- |
| Native drivers (compiled in) | MySQL, PostgreSQL, SQLite, Redis, MongoDB, SQL Server, ClickHouse, Elasticsearch, Doris, StarRocks, and more | ✅ headless, no desktop needed |
| Official CLI "direct" whitelist | postgres, mysql, sqlite, redshift, doris, starrocks, manticoresearch, rqlite, kwdb, questdb | ✅ |
| Agent / JDBC types | Oracle, Dameng, DB2, Hive, Snowflake, SAP HANA, … | ❌ needs the DBX Agent runtime (Java), out of scope |

dbxt is not limited by the official CLI's static whitelist — that list is a product decision in `dbx-cli`, not a kernel limitation. Everything `LocalBackend` executes natively works here.

### Connection store (shared with DBX)

All connections live in a single SQLite file, `dbx.db`, shared by DBX Desktop, the DBX CLI, DBX MCP, and dbxt. Connections you already configured are picked up automatically.

| Platform | Default path |
| --- | --- |
| Linux | `~/.local/share/com.dbx.app/dbx.db` |
| macOS | `~/Library/Application Support/com.dbx.app/dbx.db` |
| Windows | `%APPDATA%\com.dbx.app\dbx.db` |
| Portable (all platforms) | `$DBX_DATA_DIR/dbx.db` |

Backup notes (from DBX's `storage.rs`, verified against v0.6.9):

- One file holds everything: connections, passwords, query history, settings.
- Passwords are stored **in plaintext** in the `connection_secrets` table; the file's owner-only permissions (600) are the protection. There is no external key, so a copied file works as-is on another machine — `chmod 600` it after copying.
- If DBX is running, the file may have `-wal`/`-shm` sidecars. Either quit DBX before copying, or take a consistent snapshot: `sqlite3 dbx.db ".backup '/backup/path/dbx.db'"`.

## Build

Requires Rust 1.85+ (edition 2021).

```bash
cargo build --release
```

The first build compiles the full DBX kernel (several minutes; sqlite is bundled, so no system sqlite is needed). The release profile strips symbols and enables thin LTO.

## Usage

```bash
# default store (same as DBX Desktop)
dbxt

# explicit store directory (a copy you downloaded, a portable dir, etc.)
dbxt /path/to/dir-containing-dbx.db
# or
DBX_DATA_DIR=/path/to/dir dbxt
```

### Keys

| Context | Keys | Action |
| --- | --- | --- |
| Global | `Ctrl-C` | quit |
| Global | `Ctrl-L` | cycle command mode: SQL → Redis → MongoDB |
| Global | `F5` / `Ctrl-J` | run current SQL |
| Connection picker | `↑` `↓` / `Enter` | select / connect |
| Connection picker | `c` | new connection form |
| Sidebar (connected) | `↑` `↓` | move in table list |
| Sidebar | `←` `→` | switch database |
| Sidebar | `Enter` | browse table data (paginated `SELECT *`) |
| Sidebar | `r` | table structure (fields + DDL) |
| Sidebar | `o` | back to connection picker |
| Anywhere | `Tab` | next area (sidebar → editor → results) |
| Editor | `Enter` | new line |
| Editor | `↑` / `↓` | history (on the first / last line) |
| Editor | `Esc` | back to sidebar |
| Redis input | `[` `]` | switch Redis database (db 0/1/2…) |
| MongoDB input | `use dbname` + `Enter` | switch database |
| Results | `↑` `↓` `j` `k` | scroll rows |
| Results | `PgUp` / `PgDn` | scroll a screen of rows |
| Results | `n` / `p` | next / previous data page (table data) |
| Results | `h` `l` | scroll columns |
| Results | `t` | toggle fields ↔ DDL (structure view) |
| Results | `Enter` | open a statement's result (script view) |
| Results | `e` / `Esc` | back to editor / collapse |
| Confirmation | `Enter` `y` / `Esc` `n` | run / cancel a dangerous statement |

### Mouse / touch

Touch taps in terminals are delivered as mouse-down events, so this works on touch devices (including Android Termux) and through tmux mouse passthrough:

- Click a row to select; click the same row again to confirm (connect, browse data)
- Click an area (editor, command input, results) to focus it
- Wheel scrolls rows; horizontal wheel scrolls result columns

## Roadmap

- [ ] In-TUI connection editing / deletion
- [ ] Filtering and sorting in the data grid
- [ ] Result export (CSV / JSON)
- [ ] Persist dbxt-run SQL back into DBX's shared query history (it is read today)
- [ ] Schema-aware SQL editing / autocomplete
- [ ] Release builds for Windows, macOS (Intel/Apple Silicon), Linux (glibc + musl), Android Termux (aarch64 musl, static)

## License

Apache-2.0, same as DBX. DBX is a project by [t8y2](https://github.com/t8y2); dbxt is an independent client and is not affiliated with or endorsed by the DBX project.
