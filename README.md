# dbxt

**English** | [中文](README.zh-CN.md)

A keyboard-first terminal database client in one static binary, built on the [DBX](https://github.com/t8y2/dbx) kernel and sharing the connection store with DBX Desktop.

## Why dbxt

- One static binary — no desktop app, no daemon, no HTTP server.
- Shares DBX Desktop's `dbx.db` connection store, so connections configured anywhere just appear.
- Built-in MCP server (`dbxt mcp`) serves DBX's own `dbx_*` tools to AI agents.
- MySQL, PostgreSQL, Redis, MongoDB, SQLite, plus other engines through the kernel.
- Safety-first writes: read-only mode, red confirmations, and every write shows its SQL first.

## 30-second quick start

1. Run `dbxt`, then `↑` `↓` + `Enter` to pick a connection (`c` creates one).
2. `↑` `↓` + `Enter` opens a table; just type in the sidebar to filter.
3. `Tab` to the SQL editor, type a statement, then `Ctrl-J` (caret statement) or `F5` (whole script).
4. In the results pane `e` edits a cell, `i` inserts, `Delete` removes — the full SQL is shown before anything runs.
5. `?` opens a cheat-sheet for the current pane, `F1` the full key list.
6. The top line of the connection list is the day's tip — press `T` for the next one.

## Install

One-line script — bash (also Termux), or PowerShell:

```bash
curl -fsSL https://raw.githubusercontent.com/vst93/dbxt/refs/heads/master/cmd/install.sh | bash
```

```powershell
irm https://raw.githubusercontent.com/vst93/dbxt/master/cmd/install.ps1 | iex
```

China mirror: use `https://cdn.jsdelivr.net/gh/vst93/dbxt@master/cmd/install.sh` (or `.../install.ps1`) instead.

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

From source (Rust 1.85+ and a C toolchain; the first build compiles the DBX kernel and several C dependencies):

```bash
cargo install --git https://github.com/vst93/dbxt
```

`dbxt --version` and `dbxt --help` answer without opening the TUI; `dbxt [DBX_STORE] --last` reconnects the last session's connection and reopens its `database.schema.table`.

## Feature map

**Connections**
- Connections live in DBX's own SQLite store (`dbx.db`), so everything you configured there shows up automatically.
- `c` creates a connection in-TUI, `e` edits the highlighted one, `p` duplicates it into the form, `x` deletes it behind a red confirmation, `Enter` connects; `s` sorts by name / type / colour and the `color` row sets a stored accent.
- DBX Desktop's connection groups mirror into the tree as foldable `▾ name [n]` nodes.
- Every root carries a status dot — `●` live / `○` disconnected / `◐` connecting — and `x` disconnects it behind a red confirmation.
- The `read_only` flag refuses every write statement at one choke point, while `SELECT` / `SHOW` / `EXPLAIN` still run.
- `query_timeout` takes seconds per connection — blank keeps the kernel default (60 s), `0` means no limit.
- `L` opens a `.db` / `.sqlite` / `.sqlite3` file as a session-only connection that never touches the store.
- `Alt-E` exports every connection as JSON; `Alt-I` imports a dbxt bundle, a DBeaver `data-sources.json` or a Navicat `.ncx` export.
- SSH tunnels pass through DBX's `transport_layers`, accept `~/.ssh/config` aliases (including `ProxyJump`), prompt for an unknown host key, and report failures by stage — auth, host unreachable, or database unreachable.

**SQL editor & results**
- `Alt-/` completes from the caret's context — only tables after `FROM` / `JOIN`, only columns after `WHERE` / `ON` / `SELECT`.
- `Alt-H` opens the history panel — repeated statements merge with `×n`, `f` pins favourites, `Ctrl-Enter` / `p` runs one straight away; `Alt-F` formats and `Ctrl-F` finds in the buffer.
- `Ctrl-O` inserts a saved snippet, `Alt-S` saves the current SQL in one step, `Alt-T` opens built-in templates.
- `Alt-↓` / `Alt-↑` step through statements; `F8` / `Shift-F8` jump to the statements that failed.
- One result tab per run (`[` / `]` switch, `Alt-W` closes); `Ctrl-P` runs `EXPLAIN` and `Ctrl-Y` exports CSV / XLSX / JSON / NDJSON / Markdown / Text / INSERT to the clipboard or a file.
- Multi-statement scripts run as a batch with one row and a timing summary per statement.

**Table data**
- Keyset pagination seeks by primary key, so big tables stay fast on any page; row counts are cached per table and filter.
- `e` / `i` / `Delete` edit, insert and delete with a full SQL preview first.
- `V` enters row-select mode and `Ctrl-U` sets one column to one value for the whole block.
- `f` filters, `s` sorts, `g w` / `g W` auto-fit the focused column / every visible column.
- `Enter` / `o` opens the row popup, `v` the cell popup, `g c` the column popup with an in-place value distribution.
- `Y` copies the focused cell; `y` copies the row as `INSERT`.

**Structure & diff**
- `g d` shows the table structure; `g c` adds the column popup (`D` complete DDL, `y` copies the structure as Markdown); `E` on the connection tree exports a data dictionary.
- `Alt-D` diffs two table structures (also across connections); `Shift+Alt-D` compares two databases' table lists.
- `Alt-K` compares two tables' data, aligned by primary key and working across dialects.
- `Alt-T` moves a table's structure and/or rows to another connection.
- Diffs generate `ALTER` / sync scripts but never execute them.

**Redis**
- A paginated `SCAN` key browser, never `KEYS *`, with type and TTL badges.
- `t` cycles a type filter, `Ctrl-T` sorts by TTL, `T` sets a key's TTL, and `M` samples memory while `Ctrl-M` sorts by it.
- `Space` multi-selects for batch delete / TTL / rename behind the red confirmation layer; `Ctrl-L` opens a raw `redis-cli` console.

**MongoDB**
- A collection browser using the same filter / first-letter-jump keys as the SQL table list.
- `f` applies a JSON filter, `g f` jumps to the first loaded document carrying a field, and `c` copies a dotted sub-path.
- `e` / `i` / `Del` edit, insert and delete documents with a field-level diff; `y` copies one as pretty JSON and `r` shows indexes.

**UI**
- `?` opens a context cheat-sheet (or `F1` in the editor, where `?` is literal); `/` filters it by key or feature.
- The connection tree expands a table's foldable column outline with `>` / `<`.
- Mouse and touch work (click selects, a second click confirms, a double-click opens the row popup); a 20,000 × 12 grid scrolls at ~0.6 ms/frame.
- All UI strings come from one table (Chinese default, `DBXT_LANG=en` switches to English); `F10` opens a static About dialog.

## Keys (essentials)

The TUI's `?` overlay and `dbxt --help` carry the complete list; this is the short version.

| Context | Keys |
| --- | --- |
| Global | `?` context cheat-sheet (again for the full help, `/` filters it) · `F1` full help anywhere · `Tab`/`Shift-Tab` panes · `Alt-←`/`Alt-→` back/forward over tables, collections and Redis keys · `F5` run all · `Ctrl-J` run the caret statement (a selection wins for both) · `Alt-Enter` run only the caret statement · `Ctrl-L` cycle SQL → Redis → MongoDB · `Ctrl-A` auto-collapse unfocused panes (select all rows in row-select mode) · `Ctrl-W` collapse/expand the focused pane · `Ctrl-O` DBX SQL snippets · `Ctrl-P` EXPLAIN (SQL) · `Ctrl-S`/`Ctrl-X` commit/clear the batch queue · `F10` About dialog · `q`/`Ctrl-C` quit (two-stage when the editor holds unrun SQL) |
| Connections | `↑`/`↓` move · `Enter` connect · `Alt-1..9` jump to the Nth connection (smart db/table restore) · `Alt-Tab`/`` Alt-` `` toggle with the previous connection · `Alt-Shift-H` recent connections (this session, up to 8) · `c` new · `e` edit · `p` duplicate · `Y` copy as `xxx-copy` · `x` delete/disconnect (red confirm) · `P` health probe · `Ctrl-P` probe every saved connection at once · `L` open a SQLite file · `s` sort (name/type/colour) · `d` database/schema switcher, or disconnect the highlighted connection in the picker · `!` toggle the read-only flag · `T` next tip of the day |
| Editor | `Alt-H` history panel (`Enter` recall · `Ctrl-Enter`/`p` direct run · `y`/`Y` copy · a cyan `●` marks this session's in-memory runs) · `Alt-G` global search · `Alt-L` run a `.sql` file · `Alt-F` format/compress · `Ctrl-Z`/`Ctrl-U` undo · `Ctrl-Y`/`Ctrl-R` redo · `Alt-/` complete · `Alt-Enter` run the caret statement · `Alt-↓`/`Alt-↑` step statements · `F8`/`Shift-F8` jump to the last run's failures · `Ctrl-F` find in the buffer · `Ctrl-/`/`Alt-C` toggle `-- ` comments · `Ctrl-Shift-V` paste from the session clipboard ring · `Alt-P` paste a snippet at the caret · `Alt-T` built-in SQL templates · `%` jump to the matching bracket · `F2` statement-ordinal gutter · `F5` run all · `Ctrl-J` run the caret statement · `↑`/`↓` history |
| Results | `↑`/`↓` rows · `←`/`→` columns · `Home`/`End`/`gg`/`G` first/last row · `:` jump to a row by number (`:$` last) · `n`/`p` pages · `PgUp`/`PgDn` full page, `Ctrl-U` half a page up · `gd`/`gt` structure/data · `g b` switch table in the same database · `g w`/`g W` fit widths · `g c` column popup · `D` complete DDL · `Alt-F` pin/unpin the pane · `gv` locate a value · `|` jump to a column · `g f`/`z` freeze a column · `g s` pin a reference row · `#` big-number display · `Enter`/`o` row popup · `v` cell popup · `\` find in cells · `*` filter by the focused column · `%` zebra stripes · `S` numeric snapshot · `y` copy row as INSERT · `Y` copy the focused cell · `V` row-select mode (`Ctrl-U` batch set-value, `d`/`c` generate DELETE/UPDATE templates) · `<`/`>` column width · `Ctrl-Y` export · `[`/`]` tabs · `Ctrl-Shift-D` result snapshot |
| Structure / diff | `r` table structure · `t` toggle DDL · `D` complete DDL popup · `Alt-D` table diff (`c` picks another connection) · `Shift+Alt-D` database compare · `Alt-K` data compare (`m` toggles schema/data, `w` WHERE) · `Alt-T` transfer wizard outside the editor · `Tab` switch panes · `y` copy summary · `g` generate a script · `Esc` close |
| Redis | `Space` select · `a` all · `Del`/`x`/`m` batch delete/TTL/rename · `T` set TTL · `t` type filter · `Ctrl-T` TTL sort · `M` sample memory · `Ctrl-M` memory sort · `Shift-M` clear the cache · `/` MATCH · `Alt`+letter first-letter jump · `n` more · `e` edit · `Enter` value |
| MongoDB | `e` edit · `i` insert · `Del` delete · `f` JSON filter · `g f` field jump · `Ctrl-S` size sort · `c` copy a dotted path · `y` copy JSON · `n`/`p` pages · `r` indexes |
| Overlays | `Enter`/`y` confirm · `Esc`/`n` cancel · `↑`/`↓` scroll · every `Esc` flashes a 1.5 s status message |

## MCP server (`dbxt mcp`)

`dbxt mcp` runs DBX's own MCP server out of the dbxt binary. It is not a reimplementation: the server is `dbx_mcp::DbxMcpServer`, the exact struct the official `dbx-mcp` serves, so tools, resources, sessions, transactions and the `Settings → MCP` policy are identical.

| Command | Transport | Use it for |
| --- | --- | --- |
| `dbxt mcp` | stdio (newline-delimited JSON-RPC) | Claude Code, Cursor, Codex — clients that spawn a command |
| `dbxt mcp --http` | Streamable HTTP (loopback + bearer token) | HTTP-capable clients, Docker, long-lived endpoints |

Stdio client entry (use the absolute `dbxt` path; GUI clients may not inherit `PATH`):

```json
{ "mcpServers": { "dbx": { "command": "/absolute/path/to/dbxt", "args": ["mcp"] } } }
```

HTTP is loopback-only and always requires a bearer token; `dbxt mcp --help` lists every option and environment variable (`DBX_DATA_DIR`, `DBX_SECRET_KEY_FILE`, `DBX_MCP_HTTP_TOKEN`, …). MCP policy, allowlists and database scopes are the ones saved in DBX (`Settings → MCP`), re-checked on every call.

## Configuration

- `~/.config/dbxt/tui.json` holds UI preferences and per-table column widths (capped at 200 entries, LRU eviction).
- `~/.config/dbxt/last-session.json` records where the last run left off; `--last` auto-resumes it.
- `DBXT_LANG=en|zh` selects the UI language (the locale decides when unset).
- `DBX_DATA_DIR` points dbxt at a different DBX store.
- `DBXT_NO_PERSIST=1` disables the session and UI files.

## Secret Store compatibility

- dbxt builds against DBX v0.6.34; the kernel encrypts connection / plugin / AI / tunnel secrets at rest since v0.6.27, with a key that lives outside the database.
- On a legacy plaintext store, dbxt runs the kernel's own data-security upgrade at startup — backup → encrypt → verify — after which the store needs DBX v0.6.21+ to be read.
- Key sources, in kernel resolution order: `DBX_SECRET_KEY_FILE` (a key file), `DBX_SECRET_KEY` (the environment), the OS keychain, then the managed `<data-dir>/.dbx/secret.key`.
- The upgrade provisions the key the way the desktop wizard does and never overwrites an existing one; if ciphertext exists without its key, dbxt stops and asks for the original.
- On a host where the keychain is present but locked, dbxt catches `KEYRING_WRITE_FAILED` / `KEYRING_ACCESS_FAILED` and falls back to the managed key file, adopting it on later runs.
- A dbxt built against a pre-encryption kernel (v0.6.20 and earlier) cannot read an upgraded store.

## Status

- Verified end-to-end against real MySQL 8.4, PostgreSQL 16, Redis and MongoDB servers.
- Seven prebuilt archives; only the Linux x86_64 build has been exercised locally, the others build in CI but are untested.
- Roadmap: cross-page search, Excel (`.xlsx`) import, a dedicated Android/Termux build.

## Known issues

- **Table size metadata is MySQL / PostgreSQL only** — `s` on a database row reports unsupported on other engines.
- **Global search (`Alt-G`) is MySQL / PostgreSQL only.**
- **No `.xlsx` import** — deliberately out of scope; use CSV. Export *to* XLSX is supported.
- **Search is page-local** — `/` searches the visible rows, not the whole result set; cross-page result search is not implemented.
- **A large export warns, not stops** — result sets over 10,000 rows warn before `Ctrl-Y` proceeds.
- **Read-only detection fails closed** — a statement with an unrecognised verb counts as a write, so a rare ambiguous read is refused by design.
- **Redis / MongoDB are out of scope** for the `Alt-T` transfer wizard and the `Alt-D` / `Alt-K` diffs.
- **Only the Linux x86_64 prebuilt archive has been exercised locally** — the other six build in CI but are untested.
- **No dedicated Android/Termux artifact** — the static aarch64 build usually runs; otherwise build from source.
- The connection tree reads the desktop group layout **once per session**, so a regrouping in DBX Desktop shows up on the next dbxt launch.

## License

Apache-2.0, same as DBX. dbxt is an independent client built on the [DBX](https://github.com/t8y2/dbx) kernel by [t8y2](https://github.com/t8y2); it is not a fork and is not affiliated with or endorsed by the DBX project.
