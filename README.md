# dbxt

**English** | [中文](README.zh-CN.md)

A keyboard-first terminal UI for databases, built on the [DBX](https://github.com/t8y2/dbx) kernel. Configure a connection once — in DBX Desktop, the DBX CLI, or dbxt itself — then use it from any terminal. One static binary: no desktop app, no daemon, no HTTP server.

## Features

**Connections** — shared with DBX Desktop
- Connections live in DBX's own SQLite store (`dbx.db`), so everything you configured there shows up automatically.
- `c` creates a connection in-TUI, `p` duplicates one into the form, `Enter` connects; rows are colour-coded by database family.
- `d` opens a database list and switches — one gesture for MySQL/PostgreSQL schemas, MongoDB databases and Redis logical DBs.
- `o` returns to the picker; `r` reloads the list in place.

**SQL editor & results**
- Multi-line editor with shell-style `↑`/`↓` history (seeded from DBX's shared query history); `F5` / `Ctrl-J` runs.
- `Ctrl-Space` completes identifiers from context (tables / columns / keywords, tagged `T`/`C`/`K`); `Tab` accepts.
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
- `Alt-C` compacts column widths, `Alt-H` hides columns, `Alt-R` jumps to a recent table — choices persist per `database.table`.
- Mouse and touch work: click to select, click again to confirm; the wheel scrolls, `Shift`/`Alt`/`Ctrl`+wheel pans columns.
- Failures are visible: a watchdog turns a dead backend into an error, a spinner with elapsed seconds shows work in flight, and server errors echo verbatim.
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

## Keys (the essentials)

The TUI's `?` overlay and `dbxt --help` carry the complete list; this is the short version.

| Context | Keys |
| --- | --- |
| Global | `?` help · `Tab`/`Shift-Tab` panes · `Alt-1/2/3` focus · `F5`/`Ctrl-J` run · `Ctrl-C` quit |
| Connections | `↑` `↓` move · `Enter` connect · `c` new · `p` duplicate · `d` database list · `o` picker |
| Sidebar | `↑` `↓` tables · `/` filter · `Enter` browse · `r` structure · `I` import CSV · `t` recent |
| Results | `↑` `↓` rows · `←` `→` columns · `n`/`p` pages · `Enter`/`v` cell · `e` edit · `i` insert · `Delete` delete |
| Results (more) | `f` filter · `s` sort · `Ctrl-K` extra sort · `Ctrl-R` clear · `y` copy row · `/` search · `Ctrl-Y` export · `[` `]` tabs |
| Redis | `Space` select · `a` all · `Del`/`x`/`m` batch delete/TTL/rename · `/` MATCH · `n` more · `e` edit · `Enter` value |
| MongoDB | `e` edit · `i` insert · `Del` delete · `f` filter · `n`/`p` pages · `r` indexes |
| Overlays | `Enter`/`y` confirm · `Esc`/`n` cancel · `↑` `↓` scroll |

## Persistence & configuration

Per-table choices (compact widths, hidden columns, sort) are written to `~/.config/dbxt/tui.json`, keyed by `database.table`; `DBXT_CONFIG` overrides the path and `DBXT_NO_PERSIST=1` disables it. `DBXT_LANG=en|zh` selects the UI language (the locale decides when unset), `DBX_DATA_DIR` points dbxt at a different DBX store, and `DBXT_INSTALL_DIR` is the install script's target directory.

## Status & roadmap

Early but usable. Verified end-to-end against real MySQL 8.4, Redis and MongoDB servers.

- [x] SQL browsing, editing, transactions, filter/sort, completion, CSV import/export and result tabs
- [x] Redis key browser with batch key operations
- [x] MongoDB document browser with document CRUD
- [x] Prebuilt archives for seven targets — only the Linux x86_64 build has been exercised locally, the others are untested
- [ ] In-TUI connection editing / deletion (duplication is in via `p`)
- [ ] Result export to XLSX, and search across pages
- [ ] Excel (`.xlsx`) import
- [ ] A dedicated Android/Termux build

## License

Apache-2.0, same as DBX. dbxt is an independent client built on the [DBX](https://github.com/t8y2/dbx) kernel by [t8y2](https://github.com/t8y2); it is not a fork and is not affiliated with or endorsed by the DBX project.
