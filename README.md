# dbxt

**English** | [中文](README.zh-CN.md)

A keyboard-first terminal UI for databases, built on the [DBX](https://github.com/t8y2/dbx) kernel. Configure a connection once — in DBX Desktop, the DBX CLI, or dbxt itself — then use it from any terminal. One static binary: no desktop app, no daemon, no HTTP server.

## Features

**Connections** — shared with DBX Desktop
- Connections live in DBX's own SQLite store (`dbx.db`), so everything you configured there shows up automatically.
- `c` creates a connection in-TUI, `e` edits the highlighted one (prefilled, including its SSH tunnel), `p` duplicates one into the form, `x` deletes one behind a red confirmation, `Enter` connects; rows are colour-coded by database family.
- The connection form has a `color` row: `Space` cycles a preset palette (none → 10 colours → custom), `Enter` types a free-form `#RRGGBB`, and a live swatch previews the result. The colour is stored in DBX's own `color` field, so it round-trips with the desktop app, survives editing and duplication, and tints the sidebar, picker, title bar, status line and the `d` switcher header (the name stays fully readable — colour is only an accent).
- `s` in the picker cycles the sort order: **name** (default) → **type** → **colour**, so all your production (red) connections line up together.
- `d` opens a switcher — MySQL databases, PostgreSQL (and other schema-aware engines') **schemas then databases**, MongoDB databases and Redis logical DBs, all one gesture.
- `o` returns to the picker; `r` reloads the list in place.

**SSH tunnels (jump hosts)**
- A tunneled connection configured in DBX Desktop works in dbxt unchanged: dbxt passes `transport_layers` straight through to the kernel, so no re-entry is needed.
- The connection form has an `ssh_tunnel` section: `ssh_host` / `ssh_port` (22) / `ssh_user`, and an `ssh_auth` login method of `password`, `key` (key path + passphrase) or `agent` (SSH agent, optional socket path).
- `ssh_host` also accepts a `~/.ssh/config` **alias**; the kernel resolves it, including `ProxyJump` (which expands into a multi-hop chain).
- The tunnel forwards to the connection's own `host:port`; the form shows that target as `远端目标` / `remote target`. To forward somewhere else, change the connection's `host`/`port`.
- The first connection to an unknown jump host shows a host-key fingerprint dialog (`y`/`Enter` accept & remember, `s` trust for this session only, `n`/`Esc` reject). Accepted keys go to DBX's own `<store-dir>/known_hosts`; `~/.ssh/known_hosts` is read but never written.
- Tunnel failures are reported by stage: **SSH authentication failed**, **SSH host unreachable**, or **tunnel up but remote database unreachable** — so a wrong password, a wrong bastion address and a closed far-side port are told apart.

**SQL editor & results**
- Multi-line editor with shell-style `↑`/`↓` history (seeded from DBX's shared query history); `F5` / `Ctrl-J` runs. Every run is written back into that shared history (connection, database, timing, success flag), so the recall list and the `Alt-H` panel also cover the statements you ran here.
- `Alt-/` completes identifiers from context (tables / columns / keywords, tagged `T`/`C`/`K`); `Tab` accepts. (`Ctrl-Space` still works as a compatibility alias, but it clashes with input-method switching, so it is no longer advertised.)
- `Alt-H` opens the **query-history panel** (latest 300 statements, newest first, unique SQL): each row shows the time, the statement's first line and the source connection. `↑`/`↓`/`PgUp`/`PgDn` move, `Enter` recalls the statement into the editor (cursor at the end), `f` favourites / unfavourites it in DBX's `saved_sql_files`, `y` copies the whole statement, `Del` deletes one entry behind a red confirmation (history only — never database data), and `/` filters by statement text (case-insensitive substring). The focused statement is previewed, wrapped, below the list.
- `Alt-F` formats the editor's SQL — keywords upper-cased, main clauses on their own line, `JOIN` on its own line, two-space indent, whitespace collapsed — while leaving string literals, quoted identifiers, comments and function names (`count(`) untouched. Pressing it again compresses a formatted statement back to one line (idempotent toggle); `Ctrl-U` undoes the reformat.
- `Alt-G` opens **global search**: it scans the `char` / `varchar` / `text` columns of every table on the current database (and PostgreSQL schema) for a term, case-insensitively, and lists each hit as `table.column → value` with the match highlighted. One bounded `SELECT … LIMIT` runs per table (`DBXT_SEARCH_SCAN_LIMIT`, default 1000), and a table estimated above `DBXT_SEARCH_MAX_ROWS` (default 1,000,000) is skipped and reported. `↑`/`↓` move, `Enter` jumps to the table and lands on the matching row, `y` copies the matched value, `r` re-runs, and `Esc` aborts a running scan (keeping partial results). MySQL / PostgreSQL only.
- `Alt-L` loads and runs a **`.sql` file**: type a path (a leading `~` expands), then confirm a preview showing the file size, statement count and target connection/database, with the script wrapped below. A file over 2 MB warns, and one containing `DROP`, `TRUNCATE`, or an `UPDATE`/`DELETE` without `WHERE` routes through the same red confirmation layer before anything runs. The whole file is one history entry, and the result is the usual per-statement script summary with error messages inline; `e` loads the file into the editor instead.
- Every run keeps its own result tab (`[` / `]`); execution time and affected rows are shown.
- `Ctrl-P` runs `EXPLAIN`, `Ctrl-Y` exports the result set (CSV / JSON / NDJSON / Markdown / INSERT), `Ctrl-N` loads more when a result hit the row cap.
- `Ctrl-O` inserts a DBX saved snippet; `s` saves the editor's SQL back into that shared store.
- Multi-statement scripts (`a; b; c;`) run as a batch, one row per statement; `Enter` drills into one.

**Table data & editing**
- `Enter` on a table runs a paginated `SELECT *`; `↑`/`↓` walk rows continuously across page edges, `n`/`p` turn pages keeping the relative row.
- `←`/`→` move a cell cursor, `z` pins the first data column, `Enter` opens the full cell; a bottom bar shows horizontal position.
- `e` edits the focused cell in a diff layer showing old → new, the `WHERE` clause, the primary key and the full `UPDATE`.
- `i` inserts from a column template and `Delete` / `Ctrl-D` deletes with a bound `WHERE` — every write is confirmed first.
- `Ctrl-T` queues writes and `Ctrl-S` runs them as one `BEGIN … COMMIT`; `f` filters, `s` sorts, `Ctrl-K` adds a sort key, `Ctrl-R` clears.
- `y` copies the focused row as `INSERT INTO … VALUES (…)`; `/` searches the visible rows.

**Table structure**
- `r` shows fields (type / key / nullable / default / comment); `t` toggles the dialect-aware `SHOW CREATE TABLE` DDL.
- `Alt-D` **diffs two table structures**: the focused table is the source, then pick a target table (or `c` to pick *another connection* and diff across databases / dialects). The overlay shows, for every column, `+` (missing in the target — add), `-` (extra in the target — drop) and `~` (attributes differ: type / nullable / default / comment / charset / collation / extra / PK / unique), plus a second tab for indexes (name, columns, unique, filter) and a `⚠ cross-dialect` badge when the two sides use different engines. Type comparison is dialect-aware: within one engine it ignores cosmetic display width (`int(11)` = `int`), and across engines it maps the common types (`varchar(255)` ≈ `character varying(255)`, `int` ≈ `integer`, `tinyint(1)` ≈ `smallint`, `jsonb` ≈ `json`, …); a type outside the map is shown with `?` and both raw spellings.
- `y` copies a plain-text diff summary (paste it into a ticket), `g` generates the `ALTER` script that would rewrite the **target** to match the source (`ADD` / `DROP` / `MODIFY`, indexes included, plus `COMMENT ON COLUMN` for PostgreSQL) — dbxt **never executes it**: it is shown only in a preview tab so you can copy it into another client or the editor.
- `Shift+Alt-D` compares two **databases' table lists** (only-source / only-target / shared); `Enter` on a shared table opens its table diff. The generated `ALTER` is dialect-quoted (MySQL backticks, PostgreSQL `"double quotes"`).
- `Alt-K` **compares two tables' data** by primary key — the schema diff's row-level sibling (everywhere the picker is open, `m` toggles schema ↔ data). Pick a target table (or `c` for another connection, so MySQL ↔ PostgreSQL works) and optionally type a `WHERE` (`w`) that both sides share. Both tables must have a primary key: it is the alignment key, and only the name-intersection columns are compared, so column order does not matter. The compare streams 1000-row chunks in the background (a `COUNT(*)` forecasts the scale; `Esc` aborts and keeps what it has, and a result past 5,000 difference rows is capped with a note to narrow the scope). The overlay's summary shows the source / target row counts and the source-only `<` / target-only `>` / differing `≠` counts; `Tab` switches to those lists and `Enter` expands a `≠` row into a column-level side-by-side. Values compare as text, with the cross-dialect canonical mapping (`1` = `1.0`, `true` = `1`, `2026-01-01T…` = `2026-01-01 …`); an unmapped type keeps its raw spelling and is flagged `?`. `y` copies a plain-text summary, and `g` generates the source→target sync script (`INSERT` / `UPDATE` / `DELETE`, values escaped for the target dialect) in a preview tab — dbxt **never executes it**.

**Import & export**
- `I` imports a CSV into the focused table (the browsed table, else the sidebar selection). Enter a path (a leading `~` expands to `$HOME`) and confirm the preview: encoding, delimiter, row count and file size, the first five parsed rows, and the column mapping with per-column type inference (`int` / `float` / `bool` / `date` / `datetime` / `text`).
- CSV headers match table columns by name (case-insensitive); a table column absent from the CSV keeps its default (usually `NULL`), and an extra CSV column blocks the import with a clear message.
- `m` toggles append / overwrite (overwrite clears the table first and turns the preview border red), `s` toggles stop-on-error (default, reports the failing row) / skip-and-continue (reports every skipped row). Rows are written in transactional batches of 500 with per-batch progress.
- Encoding is auto-detected — UTF-8, otherwise GB18030/GBK (the common Chinese encoding) — and the delimiter is sniffed from the header (`,` / `;` / TAB). Excel `.xlsx` is deliberately not supported.
- `Ctrl-Y` exports the focused result set as CSV, JSON (array), NDJSON, Markdown, `INSERT` (one statement per row) or batched `INSERT` (multi-row `VALUES`). Pick a format, then a destination: blank copies via OSC 52, a path writes a file. Results over 10,000 rows warn that generation may take a moment.

**Redis**
- Connecting to Redis opens a paginated `SCAN` key browser (never `KEYS *`) with type + TTL badges, a server-side `MATCH` pattern (`/`) and logical-DB switching.
- `Enter` renders the value by type — string, hash, list, set, sorted set, stream, RedisJSON — with load-more for large collections.
- `e` / `x` / `m` / `Del` edit, set TTL, rename and delete; `Space` multi-selects and drives batch delete / TTL / prefix-rename behind the red confirmation layer.
- The raw `redis-cli` console (`Ctrl-L`) remains for anything else.

**MongoDB**
- Connecting to MongoDB lists collections; `Enter` browses documents as a grid over the union of top-level keys (`_id` first).
- `n`/`p` page, `f` applies a JSON filter, `r` shows the collection's indexes.
- `e` edits a document in a JSON editor (`_id` immutable, field-level diff), `i` inserts, `Del` deletes by `_id` — every write confirmed, page reloaded in place.

**Efficiency & experience**
- `?` opens a keyboard cheat-sheet from anywhere; every overlay closes with `Esc`.
- `Ctrl-A` auto-collapses unfocused panes and `Ctrl-W` pins one; below 50 columns the panes stack vertically.
- `Alt-C` compacts column widths, `Alt-V` hides columns, `Alt-R` jumps to a recent table, `Alt-H` opens the query history — choices persist per `database.table`.
- Mouse and touch work: click to select, click again to confirm; the wheel scrolls, `Shift`/`Alt`/`Ctrl`+wheel pans columns.
- Failures are visible: a watchdog turns a dead backend into an error, a spinner with elapsed seconds shows work in flight, and server errors echo verbatim.
- Large results stay smooth: a 20,000×12 grid scrolls at ~0.6 ms/frame (column widths are cached and only the visible window is sliced out, instead of re-scanning every row each frame), and file export runs on a background worker that streams straight to disk — 20,000 rows × 12 columns in well under a second, with peak memory near a single row (~36 MB whole-process, versus ~96 MB when the document was built in memory).
- All UI strings come from one table (`src/ui_text.rs`); Chinese is the default, `DBXT_LANG=en` switches to English.

## Installation

One-line script — bash (also Termux), or PowerShell:

```bash
curl -fsSL https://raw.githubusercontent.com/vst93/dbxt/refs/heads/master/cmd/install.sh | bash
```

```powershell
irm https://raw.githubusercontent.com/vst93/dbxt/master/cmd/install.ps1 | iex
```

China mirror: use `https://cdn.jsdelivr.net/gh/vst93/dbxt@master/cmd/install.sh` (or `.../install.ps1`) instead. The script reads the version from the GitHub release, verifies the `.sha256` and installs to `~/.local/bin` (`$PREFIX/bin` on Termux, `%USERPROFILE%\.local\bin` on Windows), reporting install / upgrade / already up to date. It keeps a newer local build instead of downgrading it. Options: `--force`, `--preview`, `--musl`, `--skip-github`, `--install-dir <dir>`, `--lang en|zh`, and the matching `DBXT_*` environment variables (`--help` lists them).

Every release attaches one archive per platform plus a `.sha256`:

| File | Platform |
| --- | --- |
| `dbxt-linux-amd64.zip` | Linux x86_64 (glibc) |
| `dbxt-linux-arm64.zip` | Linux ARM64 (glibc) |
| `dbxt-linux-amd64-musl.zip` | Linux x86_64 (static) |
| `dbxt-linux-arm64-musl.zip` | Linux ARM64 (static) |
| `dbxt-darwin-amd64.zip` | macOS Intel |
| `dbxt-darwin-arm64.zip` | macOS Apple Silicon |
| `dbxt-windows-amd64.zip` | Windows x86_64 |

Unzip and put `dbxt` on your `PATH`. The glibc builds link the CI runner's glibc; on older distributions prefer the static `musl` build. Android/Termux has no dedicated artifact — the static aarch64 build usually runs, otherwise `pkg install rust` and build from source.

From source (Rust 1.85+ and a C toolchain; the first build compiles the DBX kernel and several C dependencies):

```bash
cargo install --git https://github.com/vst93/dbxt
# or: git clone https://github.com/vst93/dbxt && cd dbxt && cargo build --release
```

Releases are cut from GitHub Actions: `gh workflow run release.yml` (optionally `-f version=0.2.0`) bumps the patch of the latest tag, tags, creates the release and builds all seven archives. `dbxt --version` and `dbxt --help` answer without opening the TUI.

## Quick start

1. Run `dbxt` — it reads the same store as DBX Desktop.
2. Pick a connection (`↑` `↓` + `Enter`), or press `c` to create one.
3. `Enter` on a table to browse it; `/` filters the table list, `d` switches database.
4. `e` edits a cell, `i` inserts, `Delete` deletes — each shows the full SQL before it runs.
5. `F5` runs the editor's SQL; `?` opens the full keyboard help.

For a database behind a bastion, press `c`, set `ssh_tunnel` to `y`, fill `ssh_host` / `ssh_user` and the login method, then `Enter` on the save row. The connection now goes dbxt → jump host → database.

## Keys (the essentials)

The TUI's `?` overlay and `dbxt --help` carry the complete list; this is the short version.

| Context | Keys |
| --- | --- |
| Global | `?` help · `Tab`/`Shift-Tab` panes · `Alt-1/2/3` focus · `F5`/`Ctrl-J` run · `Ctrl-C` quit |
| Connections | `↑` `↓` move · `Enter` connect · `c` new · `e` edit · `p` duplicate · `s` sort (name/type/colour) · `x` delete · `d` database/schema switcher · `o` picker |
| Connection form | `↑` `↓`/`Tab` fields · `Enter` edit/toggle/save · `Space` toggle `ssh_tunnel`/`ssl`/`ssh_auth`, cycle the `color` palette · `Esc` back |
| SSH host key | `y`/`Enter` accept & remember · `s` this session only · `n`/`Esc` reject |
| Sidebar | `↑` `↓` tables · `/` filter · `Enter` browse · `r` structure · `I` import CSV · `t` recent |
| Editor | `Alt-H` history panel · `Alt-G` global search · `Alt-L` run `.sql` file · `Alt-F` format/compress · `Ctrl-U` undo format · `Alt-/` complete · `F5`/`Ctrl-J` run · `↑` `↓` history |
| Schema diff | `Alt-D` diff current table vs a chosen table (`c` picks another connection) · `Shift+Alt-D` diff two databases' tables · `Tab` columns/indexes/ALTER · `y` copy summary · `g` generate ALTER · `Esc` close |
| Data compare | `Alt-K` compare two tables' rows by primary key (`c` picks another connection) · `m` switch schema/data · `w` WHERE · `Tab` summary/only-src/only-tgt/diff · `Enter` expand a diff row · `y` summary · `g` sync SQL · `Esc` close |
| Results | `↑` `↓` rows · `←` `→` columns · `n`/`p` pages · `Enter`/`v` cell · `e` edit · `i` insert · `Delete` delete |
| Results (more) | `f` filter · `s` sort · `Ctrl-K` extra sort · `Ctrl-R` clear · `y` copy row · `/` search · `Ctrl-Y` export · `[` `]` tabs |
| Redis | `Space` select · `a` all · `Del`/`x`/`m` batch delete/TTL/rename · `/` MATCH · `n` more · `e` edit · `Enter` value |
| MongoDB | `e` edit · `i` insert · `Del` delete · `f` filter · `n`/`p` pages · `r` indexes |
| Overlays | `Enter`/`y` confirm · `Esc`/`n` cancel · `↑` `↓` scroll |

## Persistence & configuration

Per-table choices (compact widths, hidden columns, sort) are written to `~/.config/dbxt/tui.json`, keyed by `database.table`; `DBXT_CONFIG` overrides the path and `DBXT_NO_PERSIST=1` disables it. `DBXT_LANG=en|zh` selects the UI language (the locale decides when unset), `DBX_DATA_DIR` points dbxt at a different DBX store, and `DBXT_INSTALL_DIR` is the install script's target directory.

The SSH-tunnel unit tests (serialization shape, form mapping, auth/error classification, host-key prompt) run with the normal `cargo test`. Two extra end-to-end tests drive a real tunnel (dbxt → local `sshd` → MySQL) and are skipped unless `DBXT_SSH_TEST=1`; they read `DBXT_SSH_TEST_USER` / `_PASSWORD` / `_KEY`, `DBXT_SSH_TEST_MYSQL_PORT` (default 13306) and `DBXT_SSH_TEST_MYSQL_USER` / `_PASSWORD`.

## Status & roadmap

Early but usable. Verified end-to-end against real MySQL 8.4, PostgreSQL 16, Redis and MongoDB servers.

**PostgreSQL** — the SQL backend speaks the PostgreSQL dialect end to end. Identifiers are double-quoted (`"schema"."table"`, reserved words included); `bytea` (`0x…` cells round-trip as `'\x…'::bytea`), `uuid`, `jsonb`, arrays (`ARRAY[…]`) and enums render in full and copy/export as valid SQL; `serial` / identity columns are skipped by the insert template so the sequence stays authoritative; and `Ctrl-P` issues a plain `EXPLAIN` (never `EXPLAIN ANALYZE`), so a write statement is planned without being executed. Structure and DDL read `pg_catalog` with an `information_schema` fallback, and the DDL resolves the relation's visible schema (`CREATE TABLE "public"."accounts"`, plus indexes, constraints and comments). Browsing is schema-aware: `d` lists the database's schemas (schemas first, then databases), the sidebar shows `inv.items`-style qualified names, and table data, structure, DDL, cell edits, inserts, deletes, filters, sorts, CSV import/export and INSERT export all carry the selected schema — so `public.orders` and `inv.orders` never mix. Column visibility, the saved sort and the `COUNT(*)` cache are keyed by `database.schema.table`.

- [x] SQL browsing, editing, transactions, filter/sort, completion, CSV import/export and result tabs
- [x] Redis key browser with batch key operations
- [x] MongoDB document browser with document CRUD
- [x] SSH tunnels (password / key / agent, `~/.ssh/config` aliases and `ProxyJump`) with in-TUI create/edit
- [x] Prebuilt archives for seven targets — only the Linux x86_64 build has been exercised locally, the others are untested
- [x] In-TUI connection deletion (red confirmation; config only, never database data)
- [ ] Result export to XLSX, and search across pages
- [ ] Excel (`.xlsx`) import
- [ ] A dedicated Android/Termux build

## License

Apache-2.0, same as DBX. dbxt is an independent client built on the [DBX](https://github.com/t8y2/dbx) kernel by [t8y2](https://github.com/t8y2); it is not a fork and is not affiliated with or endorsed by the DBX project.
