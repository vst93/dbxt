# dbxt

**English** | [中文](README.zh-CN.md)

A keyboard-and-mouse friendly terminal UI for databases, built on the [DBX](https://github.com/t8y2/dbx) kernel. Configure your connections once — in DBX Desktop, the DBX CLI, or dbxt itself — and use them from any terminal.

## Status

Early but usable. Verified end-to-end against real MySQL 8.4, Redis and MongoDB servers:

- ✅ Launch, connection picker, in-TUI connection creation
- ✅ Connect to MySQL, browse databases/tables
- ✅ **Table data browser**: `Enter` on a table runs a paginated `SELECT *` (50 rows per page). `↑`/`↓` walk rows continuously — hitting the bottom of a page silently loads the next one and puts the cursor on its first row, and vice versa at the top. `n`/`p` and `Ctrl-F`/`Ctrl-B` turn pages while keeping the cursor on the same relative row. A row-number gutter, content-sized columns, the total row count and a live `page / absolute row` indicator are always shown. `COUNT(*)` is cached per table for the session (keyed by the active filter) so paging does not re-count
- ✅ **Cell editing (`e`)**: opens a **diff-style confirmation layer** for the focused cell — old value → new value, the `WHERE` clause, the primary key and the **full generated `UPDATE`** are shown, and the new value is typed inline. `Enter` executes, `Esc` cancels, `Ctrl-V` hands the generated SQL to the editor, `Ctrl-T` queues it for a transactional batch. Nothing is written until you confirm, and the statement still passes the dangerous-statement gate. The primary key is detected from the table metadata; a table without one falls back to matching every column and warns. After a successful write the affected-row count is reported and the current page is refreshed in place
- ✅ **Quick insert (`i`)**: builds an `INSERT INTO table (cols…) VALUES (…)` template from the table's column list (auto-increment columns are skipped) and shows the new row **and the full SQL** in the same diff layer before executing
- ✅ **Row delete (`Delete` / `Ctrl-D`)**: builds a bound `DELETE FROM table WHERE pk = …` (all columns, with a warning, when there is no primary key) and always shows the full statement in the red confirmation layer — nothing is deleted until `Enter`. After success the current page is refreshed in place
- ✅ **Transactional batch edits**: `Ctrl-T` in the edit layer queues a write (`批量 N 待提交` in the status bar); `Ctrl-S` opens a confirmation showing the whole `BEGIN … COMMIT` script and runs it as one transaction on `Enter` (reporting affected rows / errors), `Ctrl-X` discards the queue
- ✅ **Row detail (`o`)**: shows the whole focused row as a scrollable vertical `column = value` list
- ✅ **Filter / sort**: `f` opens a `WHERE` prompt pre-filled with the focused column (`"col" = `) and reloads from page 1 with that predicate; the syntax quick-reference (`= != > < >= <= LIKE IN BETWEEN IS NULL AND/OR`, plus the MySQL/PG quoting differences) is shown inside the prompt. The active filter is shown in the title and status bar **and** as a `⚑` badge on the filtered column header. `Ctrl-R` clears it. `s` sorts by the focused column (ascending ↔ descending) and `Ctrl-K` appends an extra sort key; sorted headers show `▲`/`▼` (with a rank for multi-column sorts). Filter and sort both survive page turns, and the sort is persisted per table
- ✅ **Horizontal scroll progress bar**: a **half-height** track + thumb is drawn along the bottom border of the result grid, showing which slice of the columns is on screen next to a `列 1|3-8/21` label; it hides automatically when every column fits, and clicking the track jumps the column window there. A matching half-width vertical position indicator is drawn on the right border
- ✅ **Table structure**: `r` shows the field list (type / key / nullable / default / comment); `t` toggles the `SHOW CREATE TABLE` DDL (dialect-aware, built by the DBX kernel)
- ✅ **Wide tables / horizontal scrolling**: `←`/`→` (or `h`/`l`) move a cell-level cursor and the column window follows it; `Shift`/`Alt`/`Ctrl`+wheel, the horizontal wheel, a left/right **swipe** (a touch drag) and `Shift`+`←`/`→` pan the window directly, the first data column can be pinned with `z` (the row-number gutter is always pinned); the current column is highlighted in the header and the focused cell is highlighted in the body; `Enter` opens the full, untruncated cell value in a popup. The status bar always shows `列 1|3-8/21`-style horizontal position, and the bottom progress bar makes it obvious at a glance. The same cell cursor and column scrolling work inside a drilled-down script result
- ✅ **Result grid**: the header stays in sync with column scrolling, `NULL` (grey italic) and the empty string (`''`, grey) render differently, and execution time / affected rows are shown
- ✅ **Database switching**: `d` opens a database list (`↑`/`↓` + `Enter` to switch, `Esc` to close, `r` to reload the list in place) — the same gesture works for MySQL/PostgreSQL databases, MongoDB databases and Redis logical DBs. The current database is shown permanently in the sidebar (click it to open the list); `←`/`→` in the sidebar stay as a quick cycle, and `[`/`]` stay as a Redis shortcut. The current table selection is kept across a switch when the new database has a table with the same name
- ✅ **DML**: `INSERT`/`UPDATE`/`DELETE` report affected rows; every generated write (`e` / `i` / `Delete`) and the transactional batch go through a confirmation layer that shows the full SQL, and `DROP`/`TRUNCATE` and `WHERE`-less `UPDATE`/`DELETE` additionally pop the red dangerous-statement confirmation before running
- ✅ **Multi-statement scripts**: `a; b; c;` runs as a batch and shows one row per statement; `Enter` drills into a statement's result set (with the full cell cursor / column scrolling of the main grid)
- ✅ **Help overlay**: `?` opens a keyboard cheat-sheet; every overlay (database list, cell value, row detail, filter prompt, help, confirmation) closes with `Esc`
- ✅ **Mobile efficiency**: compact column widths (`Alt-C` / `w`) share the pane so a wide table fits on a phone screen and the status bar reads `全部 N 列已适配`; `Enter` expands the focused row as a vertical `column = value` list; `Alt-H` (`c`) hides columns (DBX's column-visibility picker, by name, persisted per `database.table`); `/` filters table names as you type; `Alt-R` (`t`) jumps to one of the last five browsed tables. See *Mobile efficiency* below for the recommended phone workflow
- ✅ **Persistent per-table preferences**: compact mode, hidden columns and sort are saved to `~/.config/dbxt/tui.json` (keyed by `database.table`) and restored on reopen; a corrupt or missing config falls back to defaults, and saving merges with the on-disk file so two concurrent sessions do not clobber each other's tables
- ✅ **Copy a row as SQL (`y`)**: `INSERT INTO … VALUES (…)` for the focused row, with NULL / empty-string / quote escaping and binary columns as `X'…'` hex; copied via OSC 52 (tmux passthrough aware) with a `~/.cache/dbxt/clipboard.txt` fallback
- ✅ **Result search (`/`)**: filters the visible rows as you type, highlights matches, shows the hit count, and `n` / `Shift-N` cycle the hits (`Esc` clears)
- ✅ **SQL completion**: `Ctrl-Space` completes the identifier at the cursor and follows the context (`table.` → columns only; `FROM`/`JOIN` → tables first; `WHERE`/`ON` → columns first), tagging candidates `T`/`C`/`K`; `Tab` accepts, typing refines
- ✅ **Query favourites, both ways**: `Ctrl-O` inserts a DBX `saved_sql_files` snippet; `s` saves the editor's SQL back into that shared store (name prompt, `.sql` suffix, RFC3339 timestamp) so DBX Desktop sees it too
- ✅ **No focus stealing**: a background page load updates the results in place and only moves focus to the grid when a table is first opened from the sidebar
- ✅ **SQL editor**: multi-line, shell-style `↑`/`↓` history (seeded from DBX's shared query history), the results pane takes focus after a run
- ✅ MySQL server errors surfaced verbatim in the status line
- ✅ **Responsive layout**: `Ctrl-A` is a single master switch for auto-collapse — off (the default) keeps every pane expanded; on collapses the unfocused sidebar / editor to a one-line strip so the focused pane owns the space. Below 50 columns the panes **stack vertically** (sidebar strip → editor → results). `Ctrl-W` collapses / expands just the focused pane (a manual override that always wins), `Tab`/`Shift-Tab` cycle panes, `Alt-1`/`Alt-2`/`Alt-3` jump straight to one, and clicking a collapsed strip expands and focuses it. The current auto-collapse state is shown in the status bar and the help overlay
- ✅ **Redis key browser**: connecting to a Redis connection opens a paginated `SCAN` key list (never a blocking `KEYS *`) with per-key type + TTL badges, a server-side `MATCH` pattern (`/`), `n` load-more, and logical-DB switching (`d`, `[`/`]`). Selecting a key renders its value **by type** — string (with byte size / truncation), hash (`field`/`value`/TTL), list (`index`/`value`), set, sorted set (`score`/`member`), stream (`id`/`fields`) and RedisJSON — with load-more for collections larger than 200 items. `e` edits a string body or a hash field, `x` sets the TTL, `m` renames, `Del` deletes the key, `y` copies the focused row. `Space` multi-selects keys (`Shift+↑`/`↓` range-select, `a` selects every loaded key) and the selection drives **batch** `Del` / `x` TTL / `m` prefix-rename; every mutation goes through the same red confirmation layer as SQL writes, which shows the affected key count + `MATCH` pattern, and a select-all delete additionally demands a typed key count or `YES`. A selection over 1000 keys is refused with a split-into-batches hint. The raw `redis-cli` console (`Ctrl-L`, then `Tab`) is still there for anything else
- ✅ **MongoDB document browser**: connecting to a MongoDB connection lists collections, `Enter` browses documents as a grid (union of top-level keys, `_id` first) with `n`/`p` paging and a JSON filter (`f`, e.g. `{"age": {"$gt": 30}}`), and `r` shows the collection's indexes. Document CRUD is built in: `e` opens the focused document in a JSON editor (`_id` is immutable, and saving shows a field-level diff in the red confirmation layer), `i` inserts a new document from an empty template, and `Del` deletes one by `_id` — every write is confirmed and the current page reloads in place. `db.col.find({})`, `use <db>` and multi-row output remain available in the Mongo shell console
- ✅ `dbxt --version` / `dbxt --help` answer without starting the TUI
- ⚠️ Prebuilt binaries for Linux (x86_64 / ARM64, glibc + static musl), macOS (Intel / Apple Silicon) and Windows (x86_64) are produced by the release workflow; only the Linux x86_64 build has been exercised on this machine, so the other artifacts are untested. Android/Termux has no dedicated build — see *Installation*

### Redis

A Redis connection (auto-detected from its type) opens the **key browser** in the sidebar instead of a table list. Keys are fetched with `SCAN` in bounded pages — `KEYS *` is never issued, so a large keyspace cannot block the server — and each row shows a one-letter type badge (`S`tring, `H`ash, `L`ist, `s`Et, `Z`set, stream, `J`son) plus the TTL when the key expires. `/` edits the server-side `MATCH` pattern, `n` (or scrolling to the bottom) loads the next page, `r` rescans from the start, and `d` / `←` / `→` switch the 16 logical DBs. `Enter` opens the key: the results pane renders the value **per type** — string, hash, list, set, sorted set, stream and RedisJSON each get the columns that make sense — and `n` pulls the next 200 items when a collection is longer. `e` edits a string body or the focused hash field, `x` sets the TTL in seconds (`-1` persists, `0` deletes), `m` renames, `Del` deletes the key, and `y` copies the focused row as TSV. Every write is shown in the same red confirmation layer as SQL edits and only runs on `Enter`.

**Batch key operations.** `Space` toggles the focused key into a multi-selection (the sidebar shows `[x]` / `[ ]`, the status bar and panel title show the count), `Shift+↑` / `Shift+↓` extend an additive range from the anchor, and `a` selects every loaded key. With a selection active, `Del` batch-deletes, `x` sets one TTL on all of them and `m` rewrites a key-name prefix (`old=new`, prefilled from the current `MATCH` pattern); `y` copies the selected names one per line, and `Esc` clears the selection. Batch writes reuse the red confirmation layer: the summary names the affected key count and the active `MATCH` pattern, and the generated `DEL` / `EXPIRE` / `RENAME` commands are listed before anything runs. A select-all delete is treated as the most dangerous gesture — after the red layer, a second prompt requires typing the key count (or `YES`). A selection larger than 1000 keys is refused with a split-into-batches hint, so a single batch can never fan out unbounded. After a batch the browser rescans the current page.

For everything the browser does not cover, `Ctrl-L` (then `Tab` to the console) still gives you the raw `redis-cli` command line, quoted-argument aware, with `[`/`]` to switch DBs.

### MongoDB

A MongoDB connection lists collections in the sidebar. `Enter` browses the collection as a grid built from the union of each document's top-level keys (`_id` first, nested values as JSON), `n`/`p` page through it and `f` applies a JSON filter (`{"age": {"$gt": 30}}`), with `Ctrl-R`-style clearing when the filter is left blank. `r` shows the collection's indexes (name / columns / unique / primary / type / filter / TTL) as the Mongo analogue of a table structure.

**Document CRUD.** `e` opens the focused document in a JSON editor (`Ctrl-S` validates and previews the change, `Esc` cancels). `_id` is immutable — editing it is rejected with a hint — and a valid edit opens the red confirmation layer showing a top-level field diff (`~ name: "Ada" → "Grace"`, `+ added`, `- removed`) plus the full replacement document before it runs. `i` opens the same editor with an empty `{ }` template for a new document (`_id` may be omitted and MongoDB generates it), and `Del` deletes the focused document after a confirmation that names its `_id`. Invalid JSON is reported inline in the editor and in the status bar and never reaches the server; every successful write reloads the current page in place. The Mongo shell console (`Ctrl-L`, then `Tab`) still handles `db.col.find({})`, `use <db>`, counts and anything else, and `d` switches databases.

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

## Installation

### One-line script (Linux / macOS)

```bash
curl -fsSL https://raw.githubusercontent.com/vst93/dbxt/refs/heads/master/cmd/install.sh | bash
```

China mirror (jsdelivr):

```bash
curl -fsSL https://cdn.jsdelivr.net/gh/vst93/dbxt@master/cmd/install.sh | bash
```

| Option | Description |
| --- | --- |
| `--help` | Show all options |
| `--lang en` | English interface |
| `--lang zh` | 中文界面 |
| `--install-dir <dir>` | Install to a custom directory (default `~/.local/bin`, or `$PREFIX/bin` on Termux) |
| `--force` | Reinstall / upgrade even when already up to date |
| `--skip-github` | Skip the direct GitHub download, use the mirrors only |
| `--preview` | Install the latest pre-release |
| `--musl` | Linux: install the fully static musl build |

The script reads the version from the GitHub release, verifies the `.sha256` and installs the binary. It reports **install / upgrade / already up to date** — and if the installed build is newer than the latest release (for example a binary you built from a checkout) it keeps it rather than downgrading, unless `--force` is given. On a checksum mismatch it asks before continuing (`--force` skips the question). If `github.com` is slow or blocked it retries through `ghfast.top`, `mirror.ghproxy.com`, `gh-proxy.com` and `gh-proxy.net`. The same switches are available as environment variables (`DBXT_INSTALL_DIR`, `DBXT_FORCE_INSTALL=1`, `DBXT_SKIP_GITHUB=1`, `DBXT_PREVIEW=1`, `DBXT_MUSL=1`, `DBXT_LANG=zh`).

### Windows (PowerShell)

```powershell
irm https://raw.githubusercontent.com/vst93/dbxt/master/cmd/install.ps1 | iex
```

China mirror:

```powershell
irm https://cdn.jsdelivr.net/gh/vst93/dbxt@master/cmd/install.ps1 | iex
```

| Environment variable | Description |
| --- | --- |
| `DBXT_INSTALL_DIR=path` | Install to a custom directory (default `%USERPROFILE%\.local\bin`) |
| `DBXT_FORCE_INSTALL=1` | Reinstall / upgrade even when already up to date |
| `DBXT_SKIP_GITHUB=1` | Skip the direct GitHub download, use the mirrors only |
| `DBXT_PREVIEW=1` | Install the latest pre-release |
| `DBXT_LANG=zh` | 中文界面 |

The Windows installer adds the install directory to the user `PATH` when it is missing (restart the terminal to pick it up).

### Manual download

Every release attaches one archive per platform plus a matching `.sha256`:

| File | Platform |
| --- | --- |
| `dbxt-linux-amd64.zip` | Linux, x86_64 (glibc) |
| `dbxt-linux-arm64.zip` | Linux, ARM64 (glibc) |
| `dbxt-linux-amd64-musl.zip` | Linux, x86_64 (static) |
| `dbxt-linux-arm64-musl.zip` | Linux, ARM64 (static) |
| `dbxt-darwin-amd64.zip` | macOS, Intel |
| `dbxt-darwin-arm64.zip` | macOS, Apple Silicon |
| `dbxt-windows-amd64.zip` | Windows, x86_64 |

Unzip the archive and put `dbxt` (`dbxt.exe` on Windows) on your `PATH`. The `gnu` Linux builds link the CI runner's glibc (2.39 on Ubuntu 24.04), so on an older distribution prefer the static `musl` build, which has no glibc requirement at all.

### Android / Termux

There is **no dedicated Android artifact**. `dbxt-linux-arm64-musl.zip` is a fully static aarch64 binary and usually starts under Termux, but Android is bionic rather than glibc, so this is not an officially supported combination. The reliable route is to build from source inside Termux:

```bash
pkg install rust git
curl -fsSL https://raw.githubusercontent.com/vst93/dbxt/refs/heads/master/cmd/install.sh | bash   # tries the static build
# or build locally:
git clone https://github.com/vst93/dbxt && cd dbxt && cargo build --release
```

`dbxt-linux-arm64.zip` (glibc) will **not** run there.

### Build from source

Requires Rust 1.85+ (edition 2021).

```bash
cargo install --git https://github.com/vst93/dbxt
```

Or clone and build:

```bash
git clone https://github.com/vst93/dbxt && cd dbxt
cargo build --release    # target/release/dbxt
```

The first build compiles the full DBX kernel plus several C dependencies (OpenSSL, AWS-LC, SQLite, zstd) from source — expect several minutes. You need a C toolchain (`cc` / `gcc`, `make` and `perl`); no system SQLite or OpenSSL install is required. On Windows x86_64 `aws-lc-sys` additionally needs NASM. The release profile strips symbols and enables thin LTO. Prebuilt binaries are produced by the [`Release` workflow](.github/workflows/release.yml).

`dbxt --version` prints the version and `dbxt --help` the usage, without opening the TUI. Release binaries report the tag they were built from (the version is injected at build time through `DBXT_VERSION`); a local `cargo build` reports the version in `Cargo.toml`. Both commands are pipe-safe: when the reader goes away (`dbxt --help | head -1`) dbxt exits `0` silently instead of panicking on `EPIPE`.

### Exit codes

| Code | Meaning |
| --- | --- |
| `0` | Normal exit — including `--help` / `--version` and a closed stdout pipe |
| `1` | Runtime failure (store could not be opened, stdout is not a terminal, …) |
| `2` | Usage error (unknown option) |

`--version` is the probe `cmd/install.sh` relies on, so it always exits `0` when it can write, and a failure to write (anything other than a closed pipe) still exits non-zero.

## Usage

```bash
# default store (same as DBX Desktop)
dbxt

# explicit store file (a copy you downloaded, a portable dir, etc.)
dbxt /path/to/dbx.db
# a directory is also accepted and joined with dbx.db
dbxt /path/to/dir-containing-dbx.db
# or
DBX_DATA_DIR=/path/to/dir dbxt
```

### Environment variables

| Variable | Effect |
| --- | --- |
| `DBX_DATA_DIR` | directory holding the DBX store (`dbx.db`) |
| `DBXT_LANG` | UI language: `zh` (default) or `en`; when unset, `LANG` / `LC_ALL` / `LC_MESSAGES` decides (`zh*` → Chinese, anything else → English) |
| `DBXT_EVENT_TRACE=<path>` (or `=1`) | append every mouse/resize event, with the exact sequence it arrived in, to that file, and echo the last one in the status bar |
| `DBXT_MOUSE_DEBUG=1` (or `=<path>`) | the same log (`$TMPDIR/dbxt-mouse.log` by default) plus a live floating event panel — how to report what a phone swipe actually sends |
| `DBXT_DRAG_PAN=button\|any\|off` | how a swipe is recognised: a held-button drag (default), also bare motion, or nothing |
| `DBXT_NO_ITALIC=1` | render `NULL` in grey only — never rely on the terminal's italic face |
| `DBXT_CONFIG=<path>` | where the persistent TUI config lives (default `~/.config/dbxt/tui.json`, honouring `XDG_CONFIG_HOME`) |
| `DBXT_NO_PERSIST=1` | never read or write the TUI config |
| `DBXT_CLIPBOARD=off` | do not emit the OSC 52 escape (the file fallback still works) |
| `DBXT_CLIPBOARD_FILE=<path>` | where the copy fallback file is written (default `~/.cache/dbxt/clipboard.txt`) |

### Keys

| Context | Keys | Action |
| --- | --- | --- |
| Global | `Ctrl-C` | quit |
| Global | `Ctrl-L` | cycle command mode: SQL → Redis → MongoDB |
| Global | `F5` / `Ctrl-J` | run current SQL |
| Connection picker | `↑` `↓` / `Enter` | select / connect |
| Connection picker | `c` | new connection form |
| Connection picker | `p` | duplicate the highlighted connection into the form |
| Sidebar (connected) | `↑` `↓` | move in table list |
| Sidebar | `/` | filter table names as you type (`Enter` keeps it, `Esc` clears) |
| Sidebar | `t` | recent tables overlay (`↑` `↓` + `Enter` to jump straight there) |
| Sidebar | `←` `→` | cycle database (shortcut) |
| Sidebar | `d` | open the database list (SQL / MongoDB / Redis) |
| Sidebar | `Enter` | browse table data (paginated `SELECT *`) |
| Sidebar | `r` | table structure (fields + DDL) |
| Sidebar | `o` | back to connection picker |
| Anywhere | `?` | keyboard cheat-sheet (help overlay) |
| Anywhere | `Tab` / `Shift-Tab` | next / previous area (sidebar → editor → results) |
| Anywhere | `Alt-1` / `Alt-2` / `Alt-3` | focus sidebar / editor / results |
| Anywhere | `Ctrl-A` | toggle auto-collapse (on = unfocused panes collapse, off = all expanded) |
| Anywhere | `Ctrl-W` | collapse / expand the focused pane |
| Anywhere | `Ctrl-G` | toggle pan mode — vertical wheel / up-down swipe pans columns (touch fallback) |
| Anywhere | `Alt-C` (or `w` in the results) | toggle **compact column widths** — share the pane so a wide table fits with no horizontal scroll |
| Anywhere | `Alt-H` (or `c` in the results) | **column visibility** picker (`Space` toggles, `a` all, `x` first only; persisted per `database.table`) |
| Anywhere | `Alt-R` (or `t` in the sidebar) | **recent tables** overlay — the last five browsed tables, `Enter` jumps there |
| Anywhere | `Shift`+`←` / `Shift`+`→` | pan the column window one column (hold to repeat; the text inputs keep `Shift`+arrow for selection) |
| Anywhere | `Ctrl-O` | saved SQL snippets (DBX's `saved_sql_files`), insert into the editor |
| Anywhere | `Ctrl-P` | `EXPLAIN` the editor's SQL (SQL backends) |
| Anywhere | `Ctrl-S` / `Ctrl-X` | commit / discard the queued transactional batch |
| Editor | `Enter` | new line |
| Editor | `Ctrl-Space` | SQL prefix completion (tables / columns / keywords, `Tab` accepts) |
| Editor | `↑` / `↓` | history (on the first / last line) |
| Editor | `Esc` | back to sidebar |
| Redis input | `[` `]` | switch Redis database (db 0/1/2…) |
| Redis key browser | `↑` `↓` | move in the key list |
| Redis key browser | `Space` | toggle the focused key in the multi-selection |
| Redis key browser | `Shift+↑` / `Shift+↓` | extend the selection range from the anchor |
| Redis key browser | `a` | select every loaded key (select-all) |
| Redis key browser | `Esc` | clear the selection |
| Redis key browser | `Del` / `x` / `m` | batch delete / set TTL / prefix-rename the selection (all confirmed) |
| Redis key browser | `y` | copy the selected key names (one per line) |
| Redis key browser | `/` | edit the server-side `MATCH` pattern (`Enter` applies, blank = `*`) |
| Redis key browser | `n` / `End` | load the next `SCAN` page |
| Redis key browser | `r` | rescan from the start with the current pattern |
| Redis key browser | `←` `→` (`h` `l`) | switch logical DB (db 0/1/2…) |
| Redis key browser | `Enter` | open the key's value by type |
| Redis value | `e` / `x` / `m` / `Del` | edit string or hash field / set TTL / rename key / delete key (all confirmed) |
| Redis value | `n` | load the next 200 items of a large hash / list / set / zset |
| Redis value | `y` | copy the focused row as TSV |
| Mongo collections | `r` | collection indexes (the Mongo analogue of a table structure) |
| Mongo documents | `n` / `p` | next / previous document page |
| Mongo documents | `f` | JSON filter (`Enter` applies, blank clears) |
| Mongo documents | `e` | edit the focused document in a JSON editor (`_id` immutable, diff preview) |
| Mongo documents | `i` | insert a new document from a `{ }` template |
| Mongo documents | `Del` | delete the focused document (confirmation shows its `_id`) |
| Mongo documents | `y` | copy the focused document row as TSV |
| MongoDB input | `use dbname` + `Enter` | switch database |
| Results | `↑` `↓` `j` `k` | move the row cursor (auto-flips the page at an edge) |
| Results | `PgUp` / `PgDn` | scroll a screen, carrying over the page boundary |
| Results | `n` / `p` | next / previous data page, keeping the relative row |
| Results | `Ctrl-F` / `Ctrl-B` | next / previous data page |
| Results | `←` `→` `h` `l` | move the cell cursor (the column window follows) |
| Results | `Shift`/`Alt`/`Ctrl`+wheel, horizontal wheel, left/right swipe | pan columns |
| Results | `Ctrl-G` | pan mode: the vertical wheel pans columns instead of rows |
| Results | `◀` `▶` on the bottom bar | tap to pan one window of columns (touch-friendly) |
| Results | `[` / `]` | previous / next result tab (successive queries) |
| Results | `Ctrl-Y` | export the focused result to CSV under `$HOME` |
| Results | `y` | copy the focused row as an `INSERT INTO … VALUES (…)` statement (OSC 52 clipboard + file fallback) |
| Results | `/` | search the visible result rows as you type (`Enter` keeps it, `Esc` clears) |
| Results | `n` / `Shift-N` | next / previous search hit (without an active search, `n` is the next page) |
| Results | `Ctrl-N` | load more rows when the result was truncated at the cap |
| Results | `Ctrl-E` | focus the SQL editor |
| Results | `Enter` | open the focused cell in a popup — or, in compact column mode, **expand the whole row** (script view: drill into a statement's result) |
| Results | `v` | open the focused cell in a popup (any mode) |
| Results | `w` / `c` | compact column widths / column visibility picker |
| Results | `o` | open the whole focused row as a vertical list (hidden columns included) |
| Results | `e` | edit the focused cell → diff confirmation layer (full SQL shown) |
| Results | `i` | insert a row → diff confirmation layer (full SQL shown) |
| Results | `Delete` / `Ctrl-D` | delete the focused row → red confirmation layer |
| Results | `f` | `WHERE` filter prompt, pre-filled with the focused column |
| Results | `Ctrl-R` | clear the active filter |
| Results | `s` | sort by the focused column (ascending ↔ descending) |
| Results | `Ctrl-K` | append the focused column as an extra sort key |
| Results | `z` | pin / unpin the first data column |
| Results | click the bottom progress bar | jump the column window to the clicked position |
| Results | `Home` / `End` | first / last row of the page |
| Results | `t` | toggle fields ↔ DDL (structure view) |
| Results | `Esc` | collapse the results / leave the structure view |
| Cell popup | `↑` `↓` / `PgUp` `PgDn` / `Esc` `Enter` | scroll / close |
| Row detail | `↑` `↓` / `PgUp` `PgDn` / `Esc` `Enter` | scroll / close |
| Filter prompt | `Enter` / `Esc` | apply / cancel |
| Edit layer (UPDATE) | `Enter` / `Esc` / `Ctrl-V` / `Ctrl-T` | execute / cancel / hand to editor / queue for batch |
| Edit layer (INSERT) | `Enter` / `Esc` / `Ctrl-V` / `Ctrl-T` | execute / cancel / hand to editor / queue for batch |
| Database list | `↑` `↓` / `Enter` / `r` / `Esc` | select / switch / reload / close |
| SQL snippets (`Ctrl-O`) | `↑` `↓` / `Enter` / `s` / `r` / `Esc` | select / insert into editor / save the editor's SQL as a new favourite / reload / close |
| Table filter (`/`) | typing / `Enter` / `Esc` | filter live / keep the filter / clear it |
| Column picker (`Alt-H`) | `Space` / `a` / `x` / `↑` `↓` / `Esc` | toggle one column / show all / keep only the first / move / close |
| Recent tables (`Alt-R`) | `↑` `↓` / `Enter` / `Esc` | select / jump to the table / close |
| SQL completion (`Ctrl-Space`) | `↑` `↓` / `Tab` `Enter` / `Esc` | select / accept / cancel (typing keeps refining; candidates are tagged `T`/`C`/`K` and follow the cursor context) |
| Help | `↑` `↓` `PgUp` `PgDn` / `Esc` `?` | scroll / close |
| Confirmation | `Enter` `y` / `Esc` `n` | execute / cancel (full SQL shown) |

### Footer & UI language

The bottom line is not a static cheat-sheet: it shows the four to six keys that matter for whatever currently owns the keyboard — the connection picker, the sidebar, the SQL editor, the command line, the results pane, or the overlay that is open (help, filter, column picker, edit confirmation, …). The keycaps are highlighted, and `? 帮助` is always the last item, so the escape hatch is always one keystroke away. On a narrow terminal the least relevant hints are dropped first and the elision is marked with `…` (`↑↓ select connection · … · ? Help` on a 42-column screen), never hiding `?`.

Every user-facing string — the footer, the help overlay, the confirmation layers, status messages and error text — comes from one table (`src/ui_text.rs`). Chinese is the default; set `DBXT_LANG=en` for English:

```sh
DBXT_LANG=en dbxt
```

`DBXT_LANG` wins over the locale; with it unset, `LANG` / `LC_ALL` / `LC_MESSAGES` is consulted (`zh*` → Chinese, otherwise English), and with nothing set the built-in default is Chinese.

### Display conventions

Real SQL `NULL` and the empty string are different values, so dbxt never draws them the same way:

| Value | How it is drawn |
| --- | --- |
| SQL `NULL` | `NULL`, grey (dark grey) and italic |
| Empty string `''` | `''`, grey, upright |
| The literal text `NULL` | `NULL`, in the normal foreground |

The grey is a foreground colour only, so your terminal theme (light or dark) stays in charge of the background. If the terminal or font does not do italics, the italic is simply dropped and `NULL` stays grey — the distinction still holds. Set `DBXT_NO_ITALIC=1` to force the grey-only rendering (e.g. when the italic face is hard to read). The same convention applies everywhere a value is shown: the result grid, the cell popup (`v` / `Enter`), the row detail (`o`), the edit layer's old value and the `INSERT` preview. The empty-string marker is always `''`, so it can never be confused with NULL.

CSV export (`Ctrl-Y`) follows RFC 4180 and DBX: both `NULL` and the empty string become an empty field; only the literal text `NULL` is written as `NULL`.

### Mobile efficiency (recommended phone workflow)

A phone terminal is narrow, and no gesture is reliably delivered. So instead of betting on a swipe, dbxt makes a wide table *fit*: on a narrow terminal the columns are compressed automatically, a row expands to a vertical list on `Enter`, and columns you do not care about can be hidden (and the choice persisted per table). Together these remove the need to scroll sideways at all — horizontal swiping is still there as a bonus for terminals that report it.

**1. Compact column widths (on by default below 50 columns).** Every column shares the pane equally (6–8 cells each, longer values get an ellipsis) so as many columns as possible land on screen at once. On a 42-column terminal a 20-column table goes from 3 visible columns to 5; on a 110-column terminal it goes from 7 to 11. When *every* column fits, the status bar says `全部 N 列已适配` and the bottom scroll bar disappears — you are done scrolling. `Alt-C` toggles it from anywhere, `w` in the results pane.

**2. Row expand (`Enter`).** In compact mode `Enter` opens the focused row as a scrollable `column = value` list — every column, at full width, one per line — which is how a truncated cell is read on a small screen. `o` does the same in any mode, and `v` (or `Enter` outside compact mode) still opens just the focused cell.

**3. Column visibility (`Alt-H`, or `c` in the results).** `Space` ticks a column off, `a` shows them all again, `x` keeps only the first. The choice is remembered for the table (`database.table`, in `~/.config/dbxt/tui.json`) and restored the next time you open it, so a table collapses to just the columns you care about — usually enough to make the horizontal scroll bar disappear entirely.

**4. `/` to filter tables.** With a long sidebar, type a few letters of the table name and only the matches remain (`Enter` keeps the filter, `Esc` clears it). Faster than scrolling on any screen size.

**5. `Alt-R` (or `t`) for recent tables.** The last five browsed `database.table` pairs, newest first — `Enter` jumps straight there, switching database first when needed. Replaces hunting through the sidebar.

**6. `Ctrl-Space` for SQL completion.** Completes the identifier at the cursor and follows the context: after `table.` only that table's columns are offered, after `FROM` / `JOIN` tables come first, after `WHERE` / `ON` columns come first, and matching is case-insensitive. Each candidate carries its type (`T` table / `C` column / `K` keyword); `Tab` accepts, `↑`/`↓` choose, and typing keeps refining the list.

**7. Persistence.** The compact-column toggle, the hidden-column set and the sort are written per `database.table` to `~/.config/dbxt/tui.json` (override with `DBXT_CONFIG`, disable with `DBXT_NO_PERSIST=1`) and restored when you reopen the table. A missing, truncated or corrupt config file is ignored and dbxt starts with defaults. Saving merges with the file on disk, so two dbxt sessions (or a hand-edit) never clobber each other: only the `database.table` entries this session changed are written, and resetting a table to its defaults removes its stored entry instead of silently keeping the stale one.

**8. Copy a row as SQL (`y`).** Builds `INSERT INTO … VALUES (…)` for the focused row (hidden columns included, NULL/empty/quotes escaped, binary columns as `X'…'` hex) and copies it with OSC 52 — wrapped in a tmux DCS passthrough when inside tmux. The same text is always written to `~/.cache/dbxt/clipboard.txt` as a fallback, and the path is reported in the status bar; a terminal that ignores OSC 52 simply gets no clipboard and no error.

**9. Search the result grid (`/`).** Filters the visible rows as you type, highlights the matching cells, and shows the hit count in the title; `n` / `Shift-N` cycle the hits and `Esc` clears the search. It is focus-scoped, so `/` in the results never clashes with the sidebar table filter.

#### A note on `Ctrl-Shift` keys

`Ctrl-Shift-C` / `Ctrl-Shift-H` / `Ctrl-Shift-R` are accepted when the terminal reports the Shift modifier (kitty keyboard protocol, `modifyOtherKeys`, iTerm2). On a legacy terminal — including tmux — `Ctrl-Shift-C` is byte-for-byte `Ctrl-C` (0x03), `Ctrl-Shift-H` is `Ctrl-H` (0x08) and `Ctrl-Shift-R` is `Ctrl-R` (0x12); the Shift is simply not on the wire, so no application can tell them apart. dbxt therefore binds the three view commands to `Alt-C` / `Alt-H` / `Alt-R` (an `Alt` combination *is* delivered distinctly) plus the bare results-pane keys `w` / `c` and the sidebar key `t`.

### Mouse / touch

Touch taps in terminals are delivered as mouse-down events (confirmed on release when the terminal reports releases), so this works on touch devices (including Android Termux) and through tmux mouse passthrough:

- Click a row to select; click the same row again to confirm (connect, browse data)
- Click a cell to move the cell cursor there; click a statement row in a script to drill in
- Click a table in the sidebar to select it, click again to browse its data; click the database row to open the database list
- Click the bottom progress bar to jump the column window; click the `◀` / `▶` at either end to pan one window of columns; click a collapsed pane strip to expand and focus it
- Click an area (editor, command input, results) to focus it
- Wheel scrolls rows (and auto-flips the page at an edge); `Shift`/`Alt`/`Ctrl`+wheel scroll columns; the horizontal wheel and a left/right **swipe** — whatever a touch screen sends for it — scroll columns directly too

**Horizontal scrolling on a phone — optional extra.** The mobile workflow above (compact columns + row expand + column visibility) is designed so that you usually do not need to scroll sideways at all. When a table is still wider than the screen and the terminal does report a swipe, dbxt accepts every encoding it has seen — and also offers fallbacks that need no horizontal wheel:

- a **horizontal wheel** (`ScrollLeft` / `ScrollRight`, SGR buttons 66/67) pans the columns **whatever pane has focus**, so a swipe works even after you tapped the sidebar or the editor;
- a **drag** — a held left button that moves (`Drag(Left)`, SGR button 32) — is read as a swipe and pans by finger travel (two columns of travel per column panned, capped per event). This is what many phone terminals actually send for a left/right swipe, and before R10 it was invisible to dbxt. A mostly-vertical drag is left alone (rows keep moving through the wheel), and a swipe no longer also counts as a tap: inside the results pane the click is confirmed on release and dropped once the finger moves;
- **bare motion** (`Moved`, SGR button 35) is ignored by default, because a desktop mouse emits it continuously; a terminal that reports a touch drag without a press can be served with `DBXT_DRAG_PAN=any`;
- `Shift`+wheel, `Alt`+wheel and `Ctrl`+wheel pan the columns — terminals differ in which modifiers they actually put on the wire, so all three are accepted;
- **`Shift`+`←` / `Shift`+`→` pan one column from any pane** — the keyboard fallback, and holding the key repeats, so it scrolls continuously (in the SQL editor / command line `Shift`+arrow stays a text selection);
- `Ctrl-G` turns on **pan mode**, after which the plain vertical wheel (the one gesture every terminal reports) pans columns instead of rows — the state shows as `横滚 开` in the status bar;
- the terminal decides whether to put a modifier in the wheel event at all (and some swallow `Shift`+wheel for their own horizontal scroll), so dbxt acts on every encoding it can see. Each was verified by injecting the exact sequence under tmux:

| SGR wheel button (up / down) | Modifier sent | Results-pane action |
| --- | --- | --- |
| `64` / `65` | none | scroll rows (pan columns while `Ctrl-G` pan mode is on) |
| `68` / `69` | `Shift` | pan columns |
| `72` / `73` | `Alt` | pan columns |
| `80` / `81` | `Ctrl` | pan columns |
| `66` / `67` | horizontal wheel | pan columns, any pane |
- the `◀` / `▶` buttons on the bottom bar pan a whole window per tap (taps are the one gesture that is always delivered), and the bar itself is clickable to jump.

Under tmux, keep `set -g mouse on` so the outer terminal reports the swipe and tmux forwards it to the pane.

**When a swipe still does nothing.** Start dbxt with `DBXT_MOUSE_DEBUG=1`: a floating panel shows the last six mouse events together with the exact sequence each one arrived in (e.g. `Drag(Left) @(21,11) · SGR \x1b[<32;22;12M`), and the same lines are appended to `$TMPDIR/dbxt-mouse.log` (`DBXT_EVENT_TRACE=<path>` writes the log without the panel). Swipe on the phone and read the panel:

| The panel shows | What it means |
| --- | --- |
| `ScrollLeft` / `ScrollRight` | the terminal sends a horizontal wheel — panning works directly |
| `Drag(Left)` | the swipe is a held-button drag — dbxt pans it (default `DBXT_DRAG_PAN=button`) |
| `Moved` | the terminal reports motion without a press — run dbxt with `DBXT_DRAG_PAN=any` |
| nothing at all | the terminal never reports the swipe (or tmux has `mouse` off) — use `Ctrl`+wheel, `Shift`+`←`/`→`, `Ctrl-G` pan mode or the `◀`/`▶` buttons |

Keystrokes are never traced, so a password typed into the connection form cannot leak into the log or the panel.

### Interaction notes

**Continuous row browsing.** Table data is still fetched 50 rows at a time, but the page boundary is invisible to the keyboard: `↑`/`↓` (and the wheel) load the neighbouring page when the cursor runs off an edge, landing on the row you would have reached anyway. `n`/`p` and `Ctrl-F`/`Ctrl-B` turn a whole page while keeping the cursor on the same relative row, so paging never throws you back to the top. The status bar always shows `第 3/8 页 · 行 102/400`.

**Wide tables.** Each grid has a cell cursor. `←`/`→` (or `h`/`l`) move it and the visible column window follows, with the header of the current column highlighted. `z` pins the first data column next to the always-pinned row-number gutter, so a primary key stays visible while you scroll to the right. `Enter` opens the focused cell in a popup, which is how over-wide values stay readable. The status bar always shows the horizontal position as `列 1|3-8/21` (pinned | scrolled), or `全部 N 列已适配` when nothing is off-screen. `Shift`/`Alt`/`Ctrl`+wheel, the horizontal wheel and a left/right swipe (a drag) move the **window** itself, one column per notch, so the table responds immediately. The same cursor and scrolling work in a drilled-down script result. On a narrow terminal the compact column mode (see *Mobile efficiency*) usually removes the need to scroll at all, and the `Alt-H` column picker removes the columns you do not need.

**Editing without surprises.** `e` opens a diff-style confirmation layer: it shows the column, the old value, the `WHERE` clause, the detected primary key and the **full generated `UPDATE`**, and lets you type the new value inline. `Enter` executes, `Esc` cancels, `Ctrl-V` moves the generated `UPDATE` into the SQL editor for hand-editing, and `Ctrl-T` queues it instead of running it. `i` shows the whole `INSERT` statement the same way, and `Delete` / `Ctrl-D` shows the bound `DELETE … WHERE …` in a red confirmation. The `WHERE` is built from the table's primary key; a keyless table matches every column instead and warns in the layer. A generated write that somehow lacks a bound `WHERE` (or uses `WHERE 1 = 1`) still trips the ordinary dangerous-statement confirmation. After a successful write the affected-row count is reported and the current page is reloaded in place; a failure echoes the server error verbatim.

**Transactional batches.** Queue several edits with `Ctrl-T` — the status bar shows `批量 N 待提交` — then `Ctrl-S` shows the whole `BEGIN … COMMIT` script for confirmation and, on `Enter`, runs it inside a single transaction and reports the affected rows and any error; `Esc` keeps the queue and `Ctrl-X` clears it. This is the cheapest way to make a set of related edits atomic without hand-writing a script.

**Failures are visible, not silent.** Every backend call runs under a watchdog (60 s for metadata / Redis / MongoDB, 3 min for SQL scripts) so a server that accepts the socket but never answers produces an error instead of a spinner that never stops. While a call is in flight the header shows a spinner and, after 3 seconds, the elapsed seconds (`⠋ 7s`), so a slow query reads as working rather than hung. Errors are reported head-first in red (`✗ list tables: …`) and the app stays fully responsive — `?` still opens help, `d` still reconnects. A driver that cannot enumerate databases (but still connects) is not fatal: the configured database is used and a yellow `⚠ …` notice is shown. Copying to the clipboard (OSC 52) degrades silently when the terminal drops it — the file fallback is always written.

**Filtering and sorting.** `f` opens a `WHERE` prompt pre-filled with the focused column (`"col" = `, quoted for the current dialect) so a filter is one keystroke away; when a filter already exists it is loaded for editing instead. The prompt shows a syntax quick-reference and notes the MySQL/PG identifier-quoting differences. Applying a filter reloads from page 1 while keeping the sort; sorting keeps the filter. The active filter is shown in the title and status bar and marked with a `⚑` on each filtered column header; `s` sorts by the focused column (`▲`/`▼` on the header, yellow) and `Ctrl-K` appends an additional key, shown with a rank number; `Ctrl-R` clears the filter. `COUNT(*)` is cached per table for the session, keyed by the filter, so turning pages does not re-run the count; any write clears the cache.

**Responsive layout.** `Ctrl-A` is one master switch for auto-collapse. It is **off by default**, so every pane stays expanded; turn it on and the unfocused sidebar / editor collapse to one-line strips (e.g. `▸ users · shop`), giving the focused pane the space. Below 50 columns the panes stack vertically (sidebar strip → SQL editor → results). Focus changes reflow the layout immediately. `Ctrl-W` pins just the focused pane collapsed or expanded (a manual override that always wins over the master switch), `Tab`/`Shift-Tab` cycle panes, `Alt-1`/`Alt-2`/`Alt-3` jump directly, and clicking a collapsed strip expands and focuses it. The current state (`自动折叠 开/关`) is shown in the status bar.

**Database switching.** `d` opens a list of databases and `Enter` switches — one gesture for MySQL/PostgreSQL schemas, MongoDB databases and Redis logical DBs, instead of a blind `←`/`→` cycle that is invisible on a narrow screen. `r` reloads the list in place (a database created elsewhere in the session shows up without reconnecting). The list is an overlay rather than an always-expanded sidebar tree because the sidebar collapses to a one-line strip on narrow layouts; an overlay works the same at every size and scales to many databases. The current database is still shown permanently in the sidebar (and clicking it opens the same list), while `←`/`→` and Redis `[`/`]` remain as shortcuts.

**Result tabs, EXPLAIN, export and snippets (DBX parity).** Every SQL run keeps its result as a tab, so consecutive `SELECT`s no longer overwrite each other — `[` / `]` flip between them and the title shows `结果 2/3`. `Ctrl-P` wraps the editor's SQL in the dialect's `EXPLAIN` (`EXPLAIN QUERY PLAN` on SQLite, `EXPLAIN` on MySQL/PostgreSQL/DuckDB/…) and shows the plan as a normal grid. `Ctrl-Y` writes the focused result to a CSV file under `$HOME` (`dbxt-export-<table|query>-<epoch>.csv`, RFC 4180 quoting, NULL as an empty field) and reports the path. A result that hit the 500-row cap says `已截断`; `Ctrl-N` re-runs the same statement with a larger cap (500 → 1000 → …, up to 20 000) and replaces the tab in place. `Ctrl-O` lists the SQL snippets DBX saved for this connection (`saved_sql_files`) and inserts the highlighted one into the editor; `s` saves the editor's SQL back as a new favourite (name prompt, written straight into the shared store so DBX Desktop sees it), and `r` reloads the list. Connections are colour-coded by family (mysql / redis / mongo / sqlite …) in the picker and the sidebar, using the connection's own colour when it has one, and `p` copies a connection into the form (new id on save; the copy does not carry SSH transport layers).

## Known issues

- **Multi-layer-encoded CJK identifiers.** If a table/database/column was originally created through a MySQL connection with the wrong charset (latin1/CP1252), MySQL stores each byte of the UTF-8 name as a separate CP1252 character; data written through two such connections carries two layers. DBX's kernel reverses one layer for cell values and table comments but not for identifiers; dbxt reverses identifiers when *rendering* them — repeatedly, until the name stops changing — so a name stored with one *or* two layers shows as `保留表` in the sidebar, the result-grid headers, the table structure, the database list and the SQL completion list. The raw (garbled) name is still what is sent to the server and what the completion inserts, which is why it appears verbatim in a generated `UPDATE`/`INSERT` and in `SHOW CREATE TABLE` output. Names created correctly (as UTF-8) are shown and round-trip unchanged. Server-side `WHERE` filters match the *stored* bytes, so for such data you must filter on the stored value, not the repaired display.
- `COUNT(*)` is a full scan on engines without a cached row count (e.g. InnoDB); the session cache and a 15 s timeout bound the cost but the first count on a very large table can still be slow.
- Column widths are derived from the current page's content, so they can change when a page is turned.

## Roadmap

- [ ] In-TUI connection editing / deletion (duplication is in via `p`)
- [ ] Result export to JSON / XLSX (CSV is in via `Ctrl-Y`)
- [ ] Persist dbxt-run SQL back into DBX's shared query history (it is read today)
- [x] Schema-aware SQL completion — context-aware prefix completion (tables / columns / keywords, tagged `T`/`C`/`K`) is in via `Ctrl-Space`; full JOIN-aware completion is still open
- [x] Persist the compact-column and column-visibility choices across sessions — now stored per `database.table` in `~/.config/dbxt/tui.json` (sort included)
- [ ] In-TUI result-row editing without a primary key (a stable row identity)
- [ ] Search across pages / the whole result set (today `/` filters the loaded page)
- [x] Release builds for Linux (glibc + static musl), macOS (Intel / Apple Silicon) and Windows (x86_64) — the release workflow produces them; only Linux x86_64 is verified so far
- [ ] An official Android/Termux build (today: the static aarch64 binary or a source build)

## License

Apache-2.0, same as DBX. DBX is a project by [t8y2](https://github.com/t8y2); dbxt is an independent client and is not affiliated with or endorsed by the DBX project.
