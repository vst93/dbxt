# dbxt

**English** | [中文](README.zh-CN.md)

A keyboard-and-mouse friendly terminal UI for databases, built on the [DBX](https://github.com/t8y2/dbx) kernel. Configure your connections once — in DBX Desktop, the DBX CLI, or dbxt itself — and use them from any terminal.

## Status

Early but usable. Verified end-to-end against real MySQL 8.4, Redis and MongoDB servers:

- ✅ Launch, connection picker, in-TUI connection creation
- ✅ Connect to MySQL, browse databases/tables
- ✅ **Table data browser**: `Enter` on a table runs a paginated `SELECT *` (50 rows per page). `↑`/`↓` walk rows continuously — hitting the bottom of a page silently loads the next one and puts the cursor on its first row, and vice versa at the top. `n`/`p` and `Ctrl-F`/`Ctrl-B` turn pages while keeping the cursor on the same relative row. A row-number gutter, content-sized columns, the total row count and a live `page / absolute row` indicator are always shown. `COUNT(*)` is cached per table for the session (keyed by the active filter) so paging does not re-count
- ✅ **Cell editing (`e`)**: builds an `UPDATE table SET col = … WHERE pk = …` for the focused cell and pre-fills it into the editor. The primary key is detected from the table metadata; a table without a primary key falls back to matching every column and warns in a leading SQL comment. Nothing is executed directly — the statement runs through the normal path, including the dangerous-statement confirmation
- ✅ **Quick insert (`i`)**: builds an `INSERT INTO table (cols…) VALUES (…)` template from the table's column list (auto-increment columns are skipped; values are typed `0` / `''` / `NULL`)
- ✅ **Row detail (`o`)**: shows the whole focused row as a scrollable vertical `column = value` list
- ✅ **Filter / sort**: `f` opens a `WHERE` prompt (e.g. `city = 'Beijing'`) and reloads the table with the filter; the filter is shown in the title and status bar. `Shift-F` clears it. `s` sorts by the focused column, toggling ascending / descending. Both survive page turns
- ✅ **Table structure**: `r` shows the field list (type / key / nullable / default / comment); `t` toggles the `SHOW CREATE TABLE` DDL (dialect-aware, built by the DBX kernel)
- ✅ **Wide tables / horizontal scrolling**: `←`/`→` (or `h`/`l`) move a cell-level cursor and the column window follows it; the first data column can be pinned with `z` (the row-number gutter is always pinned); the current column is highlighted in the header and the focused cell is highlighted in the body; `Enter` opens the full, untruncated cell value in a popup. The status bar always shows `列 1|3-8/21`-style horizontal position. The same cell cursor and column scrolling work inside a drilled-down script result
- ✅ **Result grid**: the header stays in sync with column scrolling, `NULL` (italic) and the empty string (`''`) render differently, and execution time / affected rows are shown
- ✅ **Database switching**: `d` opens a database list (`↑`/`↓` + `Enter` to switch, `Esc` to close, `r` to reload the list in place) — the same gesture works for MySQL/PostgreSQL databases, MongoDB databases and Redis logical DBs. The current database is shown permanently in the sidebar (click it to open the list); `←`/`→` in the sidebar stay as a quick cycle, and `[`/`]` stay as a Redis shortcut. The current table selection is kept across a switch when the new database has a table with the same name
- ✅ **DML**: `INSERT`/`UPDATE`/`DELETE` report affected rows; `DROP`/`TRUNCATE` and `WHERE`-less `UPDATE`/`DELETE` pop a red confirmation before running
- ✅ **Multi-statement scripts**: `a; b; c;` runs as a batch and shows one row per statement; `Enter` drills into a statement's result set (with the full cell cursor / column scrolling of the main grid)
- ✅ **Help overlay**: `?` opens a keyboard cheat-sheet; every overlay (database list, cell value, row detail, filter prompt, help, confirmation) closes with `Esc`
- ✅ **No focus stealing**: a background page load updates the results in place and only moves focus to the grid when a table is first opened from the sidebar
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
| Anywhere | `?` | keyboard cheat-sheet (help overlay) |
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
| Results | `o` | open the whole focused row as a vertical list |
| Results | `e` | edit the focused cell → pre-fill `UPDATE … WHERE pk` |
| Results | `i` | pre-fill an `INSERT INTO … VALUES …` template |
| Results | `f` | `WHERE` filter prompt (empty + `Enter` clears) |
| Results | `Shift-F` | clear the active filter |
| Results | `s` | sort by the focused column (ascending ↔ descending) |
| Results | `z` | pin / unpin the first data column |
| Results | `Home` / `End` | first / last row of the page |
| Results | `t` | toggle fields ↔ DDL (structure view) |
| Results | `Esc` | collapse the results / leave the structure view |
| Cell popup | `↑` `↓` / `PgUp` `PgDn` / `Esc` `Enter` | scroll / close |
| Row detail | `↑` `↓` / `PgUp` `PgDn` / `Esc` `Enter` | scroll / close |
| Filter prompt | `Enter` / `Esc` | apply / cancel |
| Database list | `↑` `↓` / `Enter` / `r` / `Esc` | select / switch / reload / close |
| Help | `↑` `↓` `PgUp` `PgDn` / `Esc` `?` | scroll / close |
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

**Wide tables.** Each grid has a cell cursor. `←`/`→` (or `h`/`l`) move it and the visible column window follows, with the header of the current column highlighted. `z` pins the first data column next to the always-pinned row-number gutter, so a primary key stays visible while you scroll to the right. `Enter` opens the focused cell in a popup, which is how over-wide values stay readable. The status bar always shows the horizontal position as `列 1|3-8/21` (pinned | scrolled). The same cursor and scrolling work in a drilled-down script result.

**Editing without surprises.** `e` never writes anything by itself: it builds an `UPDATE … WHERE pk = …` for the focused cell and drops it into the SQL editor, where you review it and run it with `Ctrl-J`/`F5`. The `WHERE` is built from the table's primary key; a keyless table matches every column instead and prepends a `-- ⚠` warning comment. `i` builds an `INSERT` template the same way, skipping auto-increment columns. Because the generated statements go through the ordinary run path, the existing dangerous-statement confirmation still applies.

**Filtering and sorting.** `f` opens a `WHERE` prompt (the leading `WHERE` keyword is optional) and reloads the current table from page 1 with that predicate; the active filter is shown in both the grid title and the status bar, and `Shift-F` clears it. `s` sorts by the focused column and toggles ascending ↔ descending. Filter and sort both survive page turns. `COUNT(*)` is cached per table for the session, keyed by the filter, so turning pages does not re-run the count; any write clears the cache.

**Database switching.** `d` opens a list of databases and `Enter` switches — one gesture for MySQL/PostgreSQL schemas, MongoDB databases and Redis logical DBs, instead of a blind `←`/`→` cycle that is invisible on a narrow screen. `r` reloads the list in place (a database created elsewhere in the session shows up without reconnecting). The list is an overlay rather than an always-expanded sidebar tree because the sidebar collapses to a 7-line strip on narrow layouts; an overlay works the same at every size and scales to many databases. The current database is still shown permanently in the sidebar (and clicking it opens the same list), while `←`/`→` and Redis `[`/`]` remain as shortcuts.

## Known issues

- **Double-encoded CJK identifiers.** If a table/database/column was originally created through a MySQL connection with the wrong charset (latin1/CP1252), MySQL stores each byte of the UTF-8 name as a separate CP1252 character. DBX's kernel already reverses this for cell values and table comments, but not for identifiers; dbxt applies the same reversal when *rendering* table, database and column names, so such a table shows as `保留表` in the sidebar. The raw (garbled) name is still what is sent to the server, which is why it appears verbatim in a generated `UPDATE`/`INSERT` statement. Names created correctly (as UTF-8) are shown and round-trip unchanged. Server-side `WHERE` filters match the *stored* bytes, so for double-encoded data you must filter on the stored value, not the repaired display.
- `COUNT(*)` is a full scan on engines without a cached row count (e.g. InnoDB); the session cache and a 15 s timeout bound the cost but the first count on a very large table can still be slow.
- Column widths are derived from the current page's content, so they can change when a page is turned.

## Roadmap

- [ ] In-TUI connection editing / deletion
- [ ] Result export (CSV / JSON)
- [ ] Persist dbxt-run SQL back into DBX's shared query history (it is read today)
- [ ] Schema-aware SQL editing / autocomplete
- [ ] Release builds for Windows, macOS (Intel/Apple Silicon), Linux (glibc + musl), Android Termux (aarch64 musl, static)

## License

Apache-2.0, same as DBX. DBX is a project by [t8y2](https://github.com/t8y2); dbxt is an independent client and is not affiliated with or endorsed by the DBX project.
