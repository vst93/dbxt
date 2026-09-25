# dbxt

**English** | [中文](README.zh-CN.md)

A keyboard-and-mouse friendly terminal UI for databases, built on the [DBX](https://github.com/t8y2/dbx) kernel. Configure your connections once — in DBX Desktop, the DBX CLI, or dbxt itself — and use them from any terminal.

## Status

Early but usable. Verified end-to-end against real MySQL 8.4, Redis and MongoDB servers:

- ✅ Launch, connection picker, in-TUI connection creation
- ✅ Connect to MySQL, browse databases/tables
- ✅ **Table data browser**: `Enter` on a table runs a paginated `SELECT *` (50 rows per page). `↑`/`↓` walk rows continuously — hitting the bottom of a page silently loads the next one and puts the cursor on its first row, and vice versa at the top. `n`/`p` and `Ctrl-F`/`Ctrl-B` turn pages while keeping the cursor on the same relative row. A row-number gutter, content-sized columns, the total row count and a live `page / absolute row` indicator are always shown
- ✅ **Table structure**: `r` shows the field list (type / key / nullable / default / comment); `t` toggles the `SHOW CREATE TABLE` DDL (dialect-aware, built by the DBX kernel)
- ✅ **Wide tables / horizontal scrolling**: `←`/`→` (or `h`/`l`) move a cell-level cursor and the column window follows it; the first data column can be pinned with `f` (the row-number gutter is always pinned); the current column is highlighted in the header and the focused cell is highlighted in the body; `Enter` opens the full, untruncated cell value in a popup. The status bar always shows `列 1|3-8/21`-style horizontal position
- ✅ **Result grid**: the header stays in sync with column scrolling, `NULL` (italic) and the empty string (`''`) render differently, and execution time / affected rows are shown
- ✅ **Database switching**: `d` opens a database list (`↑`/`↓` + `Enter` to switch, `Esc` to close) — the same gesture works for MySQL/PostgreSQL databases, MongoDB databases and Redis logical DBs. The current database is shown permanently in the sidebar (click it to open the list); `←`/`→` in the sidebar stay as a quick cycle, and `[`/`]` stay as a Redis shortcut. The current table selection is kept across a switch when the new database has a table with the same name
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
| Sidebar | `←` `→` | cycle database (shortcut) |
| Sidebar | `d` | open the database list (SQL / MongoDB / Redis) |
| Sidebar | `Enter` | browse table data (paginated `SELECT *`) |
| Sidebar | `r` | table structure (fields + DDL) |
| Sidebar | `o` | back to connection picker |
| Anywhere | `Tab` | next area (sidebar → editor → results) |
| Editor | `Enter` | new line |
| Editor | `↑` / `↓` | history (on the first / last line) |
| Editor | `Esc` | back to sidebar |
| Redis input | `[` `]` | switch Redis database (db 0/1/2…) |
| MongoDB input | `use dbname` + `Enter` | switch database |
| Results | `↑` `↓` `j` `k` | move the row cursor (auto-flips the page at an edge) |
| Results | `PgUp` / `PgDn` | scroll a screen, carrying over the page boundary |
| Results | `n` / `p` | next / previous data page, keeping the relative row |
| Results | `Ctrl-F` / `Ctrl-B` | next / previous data page |
| Results | `←` `→` `h` `l` | move the cell cursor (the column window follows) |
| Results | `Enter` | open the focused cell in a popup / open a statement's result (script view) |
| Results | `f` | pin / unpin the first data column |
| Results | `Home` / `End` | first / last row of the page |
| Results | `t` | toggle fields ↔ DDL (structure view) |
| Results | `e` / `Esc` | back to editor / collapse |
| Cell popup | `↑` `↓` / `PgUp` `PgDn` / `Esc` `Enter` | scroll / close |
| Database list | `↑` `↓` / `Enter` / `Esc` | select / switch / close |
| Confirmation | `Enter` `y` / `Esc` `n` | run / cancel a dangerous statement |

### Mouse / touch

Touch taps in terminals are delivered as mouse-down events, so this works on touch devices (including Android Termux) and through tmux mouse passthrough:

- Click a row to select; click the same row again to confirm (connect, browse data)
- Click a cell to move the cell cursor there; click a statement row in a script to drill in
- Click a table in the sidebar to select it, click again to browse its data; click the database row to open the database list
- Click an area (editor, command input, results) to focus it
- Wheel scrolls rows (and auto-flips the page at an edge); `Shift`+wheel scrolls columns; horizontal wheel also scrolls columns

### Interaction notes

**Continuous row browsing.** Table data is still fetched 50 rows at a time, but the page boundary is invisible to the keyboard: `↑`/`↓` (and the wheel) load the neighbouring page when the cursor runs off an edge, landing on the row you would have reached anyway. `n`/`p` and `Ctrl-F`/`Ctrl-B` turn a whole page while keeping the cursor on the same relative row, so paging never throws you back to the top. The status bar always shows `第 3/8 页 · 行 102/400`.

**Wide tables.** Each grid has a cell cursor. `←`/`→` (or `h`/`l`) move it and the visible column window follows, with the header of the current column highlighted. `f` pins the first data column next to the always-pinned row-number gutter, so a primary key stays visible while you scroll to the right. `Enter` opens the focused cell in a popup, which is how over-wide values stay readable. The status bar always shows the horizontal position as `列 1|3-8/21` (pinned | scrolled).

**Database switching.** `d` opens a list of databases and `Enter` switches — one gesture for MySQL/PostgreSQL schemas, MongoDB databases and Redis logical DBs, instead of a blind `←`/`→` cycle that is invisible on a narrow screen. The list is an overlay rather than an always-expanded sidebar tree because the sidebar collapses to a 7-line strip on narrow layouts; an overlay works the same at every size and scales to many databases. The current database is still shown permanently in the sidebar (and clicking it opens the same list), while `←`/`→` and Redis `[`/`]` remain as shortcuts.

## Roadmap

- [ ] In-TUI connection editing / deletion
- [ ] Filtering and sorting in the data grid
- [ ] Result export (CSV / JSON)
- [ ] Persist dbxt-run SQL back into DBX's shared query history (it is read today)
- [ ] Schema-aware SQL editing / autocomplete
- [ ] Release builds for Windows, macOS (Intel/Apple Silicon), Linux (glibc + musl), Android Termux (aarch64 musl, static)

## License

Apache-2.0, same as DBX. DBX is a project by [t8y2](https://github.com/t8y2); dbxt is an independent client and is not affiliated with or endorsed by the DBX project.
