use crate::prelude::*;

// ─── Redis / Mongo value rendering ───────────────────────────────────────────

/// Decode a standard base64 string without pulling in a dependency. Returns
/// `None` on any malformed input, so a corrupt blob degrades to a placeholder
/// instead of panicking.
pub(crate) fn b64_decode(s: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let bytes: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    let mut acc: u32 = 0;
    let mut bits: u32 = 0;
    for &b in &bytes {
        if b == b'=' {
            break;
        }
        let v = val(b)?;
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

/// Human-readable form of a Redis blob. UTF-8 values are decoded; binary values
/// are shown as hex so a terminal never receives raw control bytes.
pub(crate) fn redis_blob_text(b: &RedisBlob) -> String {
    let bytes = b64_decode(&b.raw_base64).unwrap_or_default();
    match b.encoding {
        RedisBlobEncoding::Utf8 => String::from_utf8(bytes)
            .map(|s| sanitize_cell(&s))
            .unwrap_or_else(|_| format!("<binary {} bytes>", b.raw_base64.len())),
        RedisBlobEncoding::Binary => {
            if bytes.is_empty() {
                String::new()
            } else {
                let hex: String = bytes.iter().take(64).map(|b| format!("{b:02x}")).collect();
                if bytes.len() > 64 {
                    format!("0x{hex}… ({} bytes)", bytes.len())
                } else {
                    format!("0x{hex}")
                }
            }
        }
    }
}

/// Raw (unsanitized) text of a Redis blob, used to prefill an edit dialog. Only
/// UTF-8 blobs are editable; binary ones return `None`.
pub(crate) fn redis_blob_editable_text(b: &RedisBlob) -> Option<String> {
    if b.encoding != RedisBlobEncoding::Utf8 {
        return None;
    }
    b64_decode(&b.raw_base64).and_then(|bytes| String::from_utf8(bytes).ok())
}

/// Turn a fetched [`RedisValue`] into a grid the existing results pane can draw.
pub(crate) fn redis_value_view(v: RedisValue) -> RedisValueView {
    let mut row_keys: Vec<String> = Vec::new();
    let (columns, rows, note): (Vec<String>, Vec<Vec<Val>>, String) = match &v.data {
        RedisValueData::String {
            content,
            total_bytes,
            truncated,
        } => {
            row_keys.push(String::new());
            let size = total_bytes.unwrap_or(content.raw_base64.len() as u64);
            let mut note = tf("{} 字节", &[&(size)]);
            if *truncated {
                note.push_str(t(" · 已截断"));
            }
            (
                vec![t("value").into()],
                vec![vec![Val::Text(redis_blob_text(content))]],
                note,
            )
        }
        RedisValueData::Bitmap {
            content,
            total_bytes,
            truncated,
            set_bits,
        } => {
            row_keys.push(String::new());
            let size = total_bytes.unwrap_or(content.raw_base64.len() as u64);
            let mut note = tf("{} 字节", &[&(size)]);
            if let Some(bits) = set_bits {
                note.push_str(&tf(" · {} 位置位", &[&(bits)]));
            }
            if *truncated {
                note.push_str(t(" · 已截断"));
            }
            (
                vec![t("value").into()],
                vec![vec![Val::Text(redis_blob_text(content))]],
                note,
            )
        }
        // kvrocks-style HyperLogLog: the raw bytes are unreadable, so only the
        // PFCOUNT cardinality estimate is shown.
        RedisValueData::HyperLogLog { count } => {
            row_keys.push(String::new());
            let text = match count {
                Some(c) => tf("基数估计 {}", &[&(c)]),
                None => t("（不可读）").to_string(),
            };
            (
                vec![t("value").into()],
                vec![vec![Val::Text(text)]],
                String::new(),
            )
        }
        RedisValueData::Json { value } => {
            row_keys.push(String::new());
            (
                vec![t("value").into()],
                vec![vec![Val::Text(sanitize_cell(value))]],
                String::new(),
            )
        }
        RedisValueData::List { items, total, .. } => {
            let rows = items
                .iter()
                .map(|it| {
                    row_keys.push(it.index.to_string());
                    vec![
                        Val::Text(it.index.to_string()),
                        Val::Text(redis_blob_text(&it.value)),
                    ]
                })
                .collect();
            (
                vec![t("index").into(), t("value").into()],
                rows,
                tf("{} 个元素", &[&(total)]),
            )
        }
        RedisValueData::Set { items, total, .. } => {
            let rows = items
                .iter()
                .map(|it| {
                    let m = redis_blob_text(&it.member);
                    row_keys.push(m.clone());
                    vec![Val::Text(m)]
                })
                .collect();
            (vec![t("member").into()], rows, tf("{} 个成员", &[&(total)]))
        }
        RedisValueData::Hash { items, total, .. } => {
            let rows = items
                .iter()
                .map(|it| {
                    let f = redis_blob_text(&it.field);
                    row_keys.push(f.clone());
                    let ttl = match it.field_ttl {
                        Some(-1) | None => Val::Null,
                        Some(t) => Val::Text(format!("{t}s")),
                    };
                    vec![Val::Text(f), Val::Text(redis_blob_text(&it.value)), ttl]
                })
                .collect();
            (
                vec![t("field").into(), t("value").into(), "TTL".into()],
                rows,
                tf("{} 个字段", &[&(total)]),
            )
        }
        RedisValueData::Zset { items, total, .. } => {
            let rows = items
                .iter()
                .map(|it| {
                    let m = redis_blob_text(&it.member);
                    row_keys.push(m.clone());
                    vec![Val::Text(it.score.clone()), Val::Text(m)]
                })
                .collect();
            (
                vec![t("score").into(), t("member").into()],
                rows,
                tf("{} 个成员", &[&(total)]),
            )
        }
        RedisValueData::Stream {
            entries,
            total,
            next_cursor,
        } => {
            let rows = entries
                .iter()
                .map(|e| {
                    row_keys.push(e.id.clone());
                    let fields = e
                        .fields
                        .iter()
                        .map(|f| format!("{}={}", f.field, f.value))
                        .collect::<Vec<_>>()
                        .join(", ");
                    vec![Val::Text(e.id.clone()), Val::Text(fields)]
                })
                .collect();
            let mut note = total
                .map(|t| tf("{} 条", &[&(t)]))
                .unwrap_or_else(|| tf("{} 条", &[&(entries.len())]));
            if next_cursor.is_some() {
                note.push_str(t(" · 更多"));
            }
            (vec![t("id").into(), t("fields").into()], rows, note)
        }
        RedisValueData::Unknown { redis_type } => (
            vec![t("value").into()],
            vec![vec![Val::Text(tf(
                "（暂不支持的类型：{}）",
                &[&(redis_type)],
            ))]],
            String::new(),
        ),
    };
    let scan_cursor = redis_value_cursor(&v.data);
    let mut note = note;
    if scan_cursor.is_some() && !note.contains("更多") {
        note.push_str(t(" · 更多（n 加载）"));
    }
    RedisValueView {
        key_display: v.key_display.clone(),
        key_raw: v.key_raw.clone(),
        redis_type: v.redis_type.clone(),
        ttl: v.ttl,
        grid: Grid {
            columns,
            types: Vec::new(),
            rows,
            note,
        },
        row_keys,
        scan_cursor,
        raw: v,
    }
}

/// The continuation cursor of a collection value (None = complete).
pub(crate) fn redis_value_cursor(data: &RedisValueData) -> Option<u64> {
    match data {
        RedisValueData::List { scan_cursor, .. }
        | RedisValueData::Set { scan_cursor, .. }
        | RedisValueData::Hash { scan_cursor, .. }
        | RedisValueData::Zset { scan_cursor, .. } => *scan_cursor,
        _ => None,
    }
}

/// Turn one `LOAD MORE` collection page into grid rows + row keys + next cursor.
pub(crate) fn redis_collection_page_rows(
    page: &RedisCollectionPage,
) -> (Vec<Vec<Val>>, Vec<String>, Option<u64>) {
    let mut row_keys: Vec<String> = Vec::new();
    match page {
        RedisCollectionPage::List { items, scan_cursor } => {
            let rows = items
                .iter()
                .map(|it| {
                    row_keys.push(it.index.to_string());
                    vec![
                        Val::Text(it.index.to_string()),
                        Val::Text(redis_blob_text(&it.value)),
                    ]
                })
                .collect();
            (rows, row_keys, *scan_cursor)
        }
        RedisCollectionPage::Set { items, scan_cursor } => {
            let rows = items
                .iter()
                .map(|it| {
                    let m = redis_blob_text(&it.member);
                    row_keys.push(m.clone());
                    vec![Val::Text(m)]
                })
                .collect();
            (rows, row_keys, *scan_cursor)
        }
        RedisCollectionPage::Hash { items, scan_cursor } => {
            let rows = items
                .iter()
                .map(|it| {
                    let f = redis_blob_text(&it.field);
                    row_keys.push(f.clone());
                    let ttl = match it.field_ttl {
                        Some(-1) | None => Val::Null,
                        Some(t) => Val::Text(format!("{t}s")),
                    };
                    vec![Val::Text(f), Val::Text(redis_blob_text(&it.value)), ttl]
                })
                .collect();
            (rows, row_keys, *scan_cursor)
        }
        RedisCollectionPage::Zset { items, scan_cursor } => {
            let rows = items
                .iter()
                .map(|it| {
                    let m = redis_blob_text(&it.member);
                    row_keys.push(m.clone());
                    vec![Val::Text(it.score.clone()), Val::Text(m)]
                })
                .collect();
            (rows, row_keys, *scan_cursor)
        }
    }
}

/// Human TTL label for a key: `-1` never expires, `-2` key missing.
pub(crate) fn redis_ttl_label(ttl: i64) -> String {
    match ttl {
        -1 => t("永不过期").to_string(),
        -2 => t("不存在").to_string(),
        n if n >= 0 => format!("{n}s"),
        n => n.to_string(),
    }
}

/// R57: advance a locally-displayed Redis TTL by `secs` for the key browser's
/// countdown. `-1` (persistent) and `-2` (missing) stay put; a positive TTL
/// floors at `0` so the badge can never read a negative countdown.
pub(crate) fn redis_ttl_advance(ttl: i64, secs: i64) -> i64 {
    if ttl <= 0 {
        ttl
    } else {
        (ttl - secs.max(0)).max(0)
    }
}

/// R81: compact TTL for the key-browser row — `45s` / `5m` / `2h` / `3d`, with
/// `-1` permanent and `-2` missing shown verbatim. Purely a rendering of the
/// `TTL` value already loaded by SCAN, so the list stays query-free.
pub(crate) fn redis_ttl_short(ttl: i64) -> String {
    if ttl < 0 {
        return ttl.to_string();
    }
    match ttl {
        0..=59 => format!("{ttl}s"),
        60..=3599 => format!("{}m", ttl / 60),
        3600..=86399 => format!("{}h", ttl / 3600),
        n => format!("{}d", n / 86400),
    }
}

/// R81: the parsed result of a key-list TTL input (`T`): the exact Redis
/// command, a human label and the TTL in seconds for a local, query-free list
/// refresh.
#[derive(Clone, PartialEq, Debug)]
pub struct RedisTtlPlan {
    pub command: String,
    pub label: String,
    pub ttl_secs: i64,
}

/// R81: parse a TTL argument for the key browser's `T` prompt. A bare integer
/// is seconds; an explicit `s` / `ms` / `m` / `h` / `d` suffix picks the unit
/// (`ms` needs `PEXPIRE`, everything else `EXPIRE`). `-1` persists the key and
/// `0` deletes it immediately, exactly like `EXPIRE`.
pub(crate) fn redis_ttl_command(key: &str, input: &str) -> Result<RedisTtlPlan, String> {
    let raw = input.trim();
    if raw.is_empty() {
        return Err(t("TTL 不能为空：秒数，可加 s/ms/m/h/d 后缀").to_string());
    }
    let lower = raw.to_ascii_lowercase();
    let (num, unit_ms) = if let Some(p) = lower.strip_suffix("ms") {
        (p, 1i64)
    } else if let Some(p) = lower.strip_suffix('s') {
        (p, 1_000)
    } else if let Some(p) = lower.strip_suffix('m') {
        (p, 60_000)
    } else if let Some(p) = lower.strip_suffix('h') {
        (p, 3_600_000)
    } else if let Some(p) = lower.strip_suffix('d') {
        (p, 86_400_000)
    } else {
        (lower.as_str(), 1_000)
    };
    let n: i64 = num
        .trim()
        .parse()
        .map_err(|_| t("TTL 需为整数（可加 s/ms/m/h/d 后缀，-1 持久化）").to_string())?;
    let ms = n
        .checked_mul(unit_ms)
        .ok_or_else(|| t("TTL 超出范围").to_string())?;
    // Prefer EXPIRE (whole seconds) so the common case is a plain second count;
    // a sub-second / millisecond input needs PEXPIRE.
    let command = if ms % 1_000 == 0 {
        format!("EXPIRE {} {}", redis_quote(key), ms / 1_000)
    } else {
        format!("PEXPIRE {} {}", redis_quote(key), ms)
    };
    let ttl_secs = if ms % 1_000 == 0 {
        ms / 1_000
    } else {
        ms.div_euclid(1_000) + 1
    };
    let label = if unit_ms == 1 {
        format!("{n}ms")
    } else if n == -1 {
        t("永久（-1）").to_string()
    } else {
        redis_ttl_short(ttl_secs)
    };
    Ok(RedisTtlPlan {
        command,
        label,
        ttl_secs,
    })
}

/// How many keys one batch command may carry. A multi-key `DEL` with thousands
/// of arguments risks a huge line and a slow single round trip, so the batch is
/// split into chunks of this size (also the per-batch safety ceiling).
pub const REDIS_BATCH_LIMIT: usize = 1000;
/// Keys per generated `DEL` command.
pub const REDIS_BATCH_CHUNK: usize = 100;

/// Quote one key / value for the redis-cli tokenizer the backend uses.
pub(crate) fn redis_quote(s: &str) -> String {
    let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// Build the `DEL` commands for a batch of display key names, chunked so one
/// command never grows unbounded.
pub(crate) fn redis_batch_del_commands(displays: &[String]) -> Vec<String> {
    if displays.is_empty() {
        return Vec::new();
    }
    displays
        .chunks(REDIS_BATCH_CHUNK)
        .map(|chunk| {
            let keys: Vec<String> = chunk.iter().map(|k| redis_quote(k)).collect();
            format!("DEL {}", keys.join(" "))
        })
        .collect()
}

/// Build one `EXPIRE key seconds` command per key. `ttl` is validated by the
/// caller; a non-numeric value yields an empty plan.
pub(crate) fn redis_batch_ttl_commands(displays: &[String], ttl: &str) -> Vec<String> {
    let ttl = ttl.trim();
    if ttl.parse::<i64>().is_err() {
        return Vec::new();
    }
    displays
        .iter()
        .map(|k| format!("EXPIRE {} {}", redis_quote(k), ttl))
        .collect()
}

/// Plan a prefix replacement: every key starting with `old_prefix` maps to
/// `new_prefix` + the remainder. Keys that do not match are left untouched.
pub(crate) fn redis_prefix_rename_plan(
    displays: &[String],
    old_prefix: &str,
    new_prefix: &str,
) -> Vec<(String, String)> {
    displays
        .iter()
        .filter_map(|k| {
            let rest = k.strip_prefix(old_prefix)?;
            let new_name = format!("{new_prefix}{rest}");
            (new_name != *k).then(|| (k.clone(), new_name))
        })
        .collect()
}

/// Build the `RENAME old new` commands for a prefix replacement.
pub(crate) fn redis_batch_rename_commands(
    displays: &[String],
    old_prefix: &str,
    new_prefix: &str,
) -> Vec<String> {
    redis_prefix_rename_plan(displays, old_prefix, new_prefix)
        .into_iter()
        .map(|(old, new)| format!("RENAME {} {}", redis_quote(&old), redis_quote(&new)))
        .collect()
}

/// Which batch write a key-browser gesture generates.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum RedisBatchKind {
    Delete,
    Ttl,
    RenamePrefix,
}

/// A generated batch, ready to be shown in the red confirmation layer.
#[derive(Debug)]
pub struct RedisBatchPlan {
    pub commands: Vec<String>,
    /// When set, the red layer must ask for a typed count / `YES` first.
    pub typed_confirm: Option<usize>,
    pub summary: String,
}

/// Pure batch planner: turn a gesture + targets into commands, a typed-confirm
/// requirement and a human summary. Errors are translated status strings.
pub(crate) fn redis_plan_batch(
    kind: RedisBatchKind,
    targets: &[(String, String)],
    all_loaded: bool,
    arg: &str,
) -> Result<RedisBatchPlan, String> {
    let displays: Vec<String> = targets.iter().map(|(_, d)| d.clone()).collect();
    match kind {
        RedisBatchKind::Delete => Ok(RedisBatchPlan {
            commands: redis_batch_del_commands(&displays),
            typed_confirm: all_loaded.then_some(displays.len()),
            summary: tf("批量删除 {} 个 key", &[&displays.len()]),
        }),
        RedisBatchKind::Ttl => {
            let ttl = arg.trim();
            if ttl.parse::<i64>().is_err() {
                return Err(t("TTL 需为整数秒（-1 持久化，0 立即删除）").to_string());
            }
            let commands = redis_batch_ttl_commands(&displays, ttl);
            if commands.is_empty() {
                return Err(t("没有可操作的 key").to_string());
            }
            Ok(RedisBatchPlan {
                commands,
                typed_confirm: None,
                summary: tf("批量设置 TTL={}s · {} 个 key", &[&ttl, &displays.len()]),
            })
        }
        RedisBatchKind::RenamePrefix => {
            let Some((old, new)) = arg.split_once('=') else {
                return Err(t("格式：旧前缀=新前缀，例 app: = new:").to_string());
            };
            let plan = redis_prefix_rename_plan(&displays, old, new);
            if plan.is_empty() {
                return Err(t("没有 key 匹配该前缀（未改名）").to_string());
            }
            Ok(RedisBatchPlan {
                commands: redis_batch_rename_commands(&displays, old, new),
                typed_confirm: None,
                summary: tf(
                    "批量前缀重命名 {} → {} · {} 个 key",
                    &[&old, &new, &plan.len()],
                ),
            })
        }
    }
}

/// Flatten a page of MongoDB documents into a grid: the union of top-level keys
/// (with `_id` first) becomes the columns, and each document is one row.
pub(crate) fn mongo_docs_grid(docs: &[serde_json::Value]) -> Grid {
    let mut keys: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for doc in docs {
        if let serde_json::Value::Object(map) = doc {
            for k in map.keys() {
                if seen.insert(k.clone()) {
                    keys.push(k.clone());
                }
            }
        }
    }
    if let Some(i) = keys.iter().position(|k| k == "_id") {
        let id = keys.remove(i);
        keys.insert(0, id);
    }
    let rows: Vec<Vec<Val>> = docs
        .iter()
        .map(|doc| {
            keys.iter()
                .map(|k| match doc.get(k) {
                    Some(serde_json::Value::Null) | None => Val::Null,
                    Some(serde_json::Value::String(s)) => Val::Text(sanitize_cell(s)),
                    Some(other) => Val::Text(sanitize_cell(&other.to_string())),
                })
                .collect()
        })
        .collect();
    Grid {
        columns: keys,
        types: Vec::new(),
        rows,
        note: tf("{} 个文档", &[&(docs.len())]),
    }
}

/// R82: the trailing grid column carrying each document's byte size (compact
/// `serde_json` length of the already-loaded value). The name carries the unit
/// and cannot collide with a real field called `size`.
pub(crate) const MONGO_SIZE_COLUMN: &str = "size(B)";

/// R82: the loaded document's serialized byte length. Computed from the value
/// already in memory — never a server round-trip.
pub(crate) fn mongo_doc_size_bytes(doc: &serde_json::Value) -> usize {
    serde_json::to_string(doc)
        .map(|s| s.len())
        .unwrap_or_default()
}

/// R82: [`mongo_docs_grid`] plus the trailing `size(B)` column. Kept as a
/// wrapper so the pure grid function stays the single source of truth for the
/// document's own fields.
pub(crate) fn mongo_docs_grid_with_sizes(docs: &[serde_json::Value]) -> Grid {
    let mut grid = mongo_docs_grid(docs);
    // An empty page keeps zero columns so the empty-state hint can render.
    if docs.is_empty() {
        return grid;
    }
    grid.columns.push(MONGO_SIZE_COLUMN.to_string());
    for (row, doc) in grid.rows.iter_mut().zip(docs.iter()) {
        row.push(Val::Text(mongo_doc_size_bytes(doc).to_string()));
    }
    grid
}

/// R82: re-order the loaded documents by compact serialized size. `Natural`
/// returns the arrival order untouched; the size modes are stable so equal
/// sizes keep their relative order.
pub(crate) fn mongo_docs_sorted_by_size(
    docs: &[serde_json::Value],
    sort: MongoSizeSort,
) -> Vec<serde_json::Value> {
    let mut out = docs.to_vec();
    match sort {
        MongoSizeSort::Natural => {}
        MongoSizeSort::SizeAsc => out.sort_by_key(mongo_doc_size_bytes),
        MongoSizeSort::SizeDesc => {
            out.sort_by_key(|d| std::cmp::Reverse(mongo_doc_size_bytes(d)));
        }
    }
    out
}

/// R82: true when a document has the named field. A dotted name is treated as a
/// nested path (`a.b.0.name`); a bare name matches a top-level key,
/// case-insensitively. Client-side over the already-loaded page.
pub(crate) fn mongo_doc_has_field(doc: &serde_json::Value, name: &str) -> bool {
    let name = name.trim();
    if name.is_empty() {
        return false;
    }
    if name.contains('.') {
        return mongo_path_lookup(doc, name).is_some();
    }
    match doc {
        serde_json::Value::Object(map) => map.keys().any(|k| k.eq_ignore_ascii_case(name)),
        _ => false,
    }
}

/// R82: walk a dotted path into a JSON value. Object segments are field names;
/// array segments are decimal indices. Any missing segment (or a non-numeric
/// index into an array) yields `None`.
pub(crate) fn mongo_path_lookup<'a>(
    doc: &'a serde_json::Value,
    path: &str,
) -> Option<&'a serde_json::Value> {
    if path.trim().is_empty() {
        return None;
    }
    let mut cur = doc;
    for seg in path.split('.') {
        if seg.is_empty() {
            return None;
        }
        cur = match cur {
            serde_json::Value::Object(map) => map.get(seg)?,
            serde_json::Value::Array(arr) => arr.get(seg.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(cur)
}

/// R82: the clipboard text for an extracted sub-value. A JSON string copies as
/// the raw string (the same convention as a cell value); anything else copies
/// its compact JSON.
pub(crate) fn mongo_path_copy_text(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        other => serde_json::to_string(other).unwrap_or_else(|_| other.to_string()),
    }
}

/// True when `s` looks like a 24-char hex ObjectId. A genuine string `_id` with
/// that shape must be marked so the driver does not reinterpret it.
pub(crate) fn is_object_id_hex(s: &str) -> bool {
    s.len() == 24 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Convert a document's `_id` value (in the driver's `bson_to_json` shape) into
/// the `id` argument the MongoDB document APIs expect.
pub(crate) fn mongo_id_arg(id: &serde_json::Value) -> String {
    match id {
        serde_json::Value::String(s) => {
            if is_object_id_hex(s) {
                // `__dbx_mongo_string_id__` + a JSON string tells the driver this
                // is an explicitly typed BSON string, not an ObjectId.
                format!(
                    "__dbx_mongo_string_id__{}",
                    serde_json::Value::String(s.clone())
                )
            } else {
                s.clone()
            }
        }
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Object(map) => {
            if let Some(oid) = map.get("$oid").and_then(|v| v.as_str()) {
                oid.to_string()
            } else {
                serde_json::to_string(id).unwrap_or_default()
            }
        }
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// A short, human-readable rendering of an `_id` for a confirmation prompt.
pub(crate) fn mongo_id_label(id: &serde_json::Value) -> String {
    match id {
        serde_json::Value::Object(map) => {
            if let Some(oid) = map.get("$oid").and_then(|v| v.as_str()) {
                oid.to_string()
            } else if let Some(n) = map.get("$numberLong").and_then(|v| v.as_str()) {
                n.to_string()
            } else {
                serde_json::to_string(id).unwrap_or_default()
            }
        }
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Top-level field diff between two documents, capped for the confirmation
/// layer. Each line is `field: old -> new` (or `+` / `-` for added / removed).
pub(crate) fn mongo_doc_diff(
    old: &serde_json::Value,
    new: &serde_json::Value,
    cap: usize,
) -> Vec<String> {
    let empty = serde_json::Map::new();
    let old_map = old.as_object().unwrap_or(&empty);
    let new_map = new.as_object().unwrap_or(&empty);
    let mut keys: Vec<&String> = Vec::new();
    for k in old_map.keys().chain(new_map.keys()) {
        if !keys.contains(&k) {
            keys.push(k);
        }
    }
    let mut out: Vec<String> = Vec::new();
    for k in keys {
        if k == "_id" {
            continue;
        }
        let before = old_map.get(k);
        let after = new_map.get(k);
        if before == after {
            continue;
        }
        let show = |v: Option<&serde_json::Value>| match v {
            None => "—".to_string(),
            Some(v) => truncate_disp(&one_line(&v.to_string()), 60),
        };
        let mark = if before.is_none() {
            "+ "
        } else if after.is_none() {
            "- "
        } else {
            "~ "
        };
        out.push(format!("{mark}{k}: {} → {}", show(before), show(after)));
        if out.len() >= cap {
            out.push(t("…（更多字段已省略）").to_string());
            break;
        }
    }
    out
}

/// One statement inside a multi-statement script run.
#[derive(Clone)]
pub(crate) struct StmtOutcome {
    pub(crate) sql: String,
    pub(crate) grid: Grid,
    pub(crate) error: Option<String>,
    pub(crate) affected: u64,
    pub(crate) ms: u128,
}

#[derive(Clone)]
pub(crate) struct ScriptView {
    pub(crate) outcomes: Vec<StmtOutcome>,
    pub(crate) sel: usize,
    pub(crate) drilled: Option<usize>,
}

/// A frozen snapshot of the results pane (R48). `Alt-F` in the results pane
/// pins the current grid so it keeps showing above whatever comes next: switch
/// to another table / database and the pinned grid stays on top for an up/down
/// comparison. Pressing `Alt-F` again releases it.
#[derive(Clone)]
pub(crate) struct PinnedResult {
    pub(crate) title: String,
    pub(crate) grid: Grid,
    pub(crate) kind: GridKind,
}

/// One saved query result the user can flip back to with `[` / `]` (DBX keeps
/// a result tab per run; this is the TUI equivalent for query results).
#[derive(Clone)]
pub(crate) struct ResultTab {
    /// Short label (the first line of the SQL, trimmed).
    pub(crate) title: String,
    pub(crate) grid: Option<Grid>,
    /// Unfiltered grid, so the column-visibility filter can be re-applied.
    pub(crate) grid_full: Option<Grid>,
    pub(crate) script: Option<ScriptView>,
    pub(crate) kind: GridKind,
    pub(crate) sel: usize,
    pub(crate) col_offset: usize,
    pub(crate) col_cursor: usize,
}

#[derive(Clone)]
pub(crate) struct PageState {
    pub(crate) table: String,
    /// Schema the table lives in (empty for engines without one, e.g. MySQL).
    pub(crate) schema: String,
    pub(crate) table_type: Option<String>,
    pub(crate) page: usize,
    pub(crate) page_size: usize,
    pub(crate) total: Option<u64>,
    /// True when `total` is only a lower bound: the row count hit the sample
    /// cap, so the real table is larger. Rendered as `>N`.
    pub(crate) total_lower_bound: bool,
    pub(crate) has_next: bool,
    /// Active WHERE predicate (without the `WHERE` keyword); empty = no filter.
    pub(crate) filter: String,
    /// Active ORDER BY expression (without the `ORDER BY` keyword).
    pub(crate) order_by: Option<String>,
    /// Primary-key cursor enabling keyset pagination for `n`/`p` and edge
    /// crossings. `None` while the view orders by something else (custom sort)
    /// or the table has no usable primary key, in which case `n`/`p` fall back
    /// to `LIMIT … OFFSET`.
    pub(crate) keyset: Option<KeysetCursor>,
}

/// A primary-key cursor for keyset pagination: the key tuple of the first and
/// last row of the current page. `n`/`p` then read `WHERE pk > last ORDER BY pk
/// LIMIT n` instead of `LIMIT n OFFSET page*n`, whose cost grows with the page
/// number.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct KeysetCursor {
    /// Primary-key columns, in key order (single or composite).
    pub(crate) pk: Vec<String>,
    /// The display order on the key (true = ascending).
    pub(crate) ascending: bool,
    /// Key tuple of the first row on the current page.
    pub(crate) first: Vec<serde_json::Value>,
    /// Key tuple of the last row on the current page.
    pub(crate) last: Vec<serde_json::Value>,
}

/// Where a table page read starts.
#[derive(Clone, Debug, PartialEq, Default)]
pub(crate) enum PageSeek {
    /// Classic `LIMIT n OFFSET m` — used for the first page and for any jump
    /// that is not one page forward or back.
    #[default]
    Offset,
    /// Rows strictly after this key tuple, in the view's display order.
    After(Vec<serde_json::Value>),
    /// Rows strictly before this key tuple, in the view's display order.
    Before(Vec<serde_json::Value>),
}

/// Column metadata for the table currently open in the data browser. Used to
/// build `UPDATE`/`INSERT` templates (primary-key detection, value typing).
#[derive(Clone, Default)]
pub(crate) struct TableMeta {
    pub(crate) table: String,
    /// Schema the metadata was read from; matched alongside the table name so
    /// `public.orders` and `inv.orders` never swap column metadata.
    pub(crate) schema: String,
    pub(crate) columns: Vec<ColumnInfo>,
    /// R56: the table's indexes, fetched alongside the columns (best effort) so
    /// the `g c` popup can mark a non-unique index column as `MUL` without a
    /// query of its own. Empty when the backend could not list them.
    pub(crate) indexes: Vec<IndexInfo>,
}

/// One paginated table-data request (first load, page turn, filter or sort).
pub(crate) struct TableDataReq {
    pub(crate) cfg: Box<ConnectionConfig>,
    pub(crate) db: String,
    pub(crate) schema: String,
    pub(crate) table: String,
    pub(crate) table_type: Option<String>,
    pub(crate) page: usize,
    pub(crate) page_size: usize,
    pub(crate) filter: String,
    pub(crate) order_by: Option<String>,
    /// Reuse a session-cached total instead of running COUNT(*) again; the bool
    /// marks a lower bound (the sample cap was hit).
    pub(crate) known_total: Option<(u64, bool)>,
    /// Primary-key columns to browse by (empty = plain OFFSET).
    pub(crate) keyset_pk: Vec<String>,
    /// Whether the keyset browse order is ascending.
    pub(crate) keyset_asc: bool,
    /// Where the read starts (first page / keyset cursor / arbitrary OFFSET).
    pub(crate) seek: PageSeek,
    /// Monotonic request id; a reply whose id is not the latest is discarded.
    pub(crate) gen: u64,
}

// ─── text helpers ────────────────────────────────────────────────────────────

pub(crate) fn disp_width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// Truncate to `max` display columns, appending `…` when content was dropped.
pub(crate) fn truncate_disp(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if disp_width(s) <= max {
        return s.to_string();
    }
    let mut out = String::new();
    let mut w = 0usize;
    for c in s.chars() {
        let cw = UnicodeWidthChar::width(c).unwrap_or(0);
        if w + cw > max.saturating_sub(1) {
            break;
        }
        out.push(c);
        w += cw;
    }
    out.push('…');
    out
}

pub(crate) fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Reverse CP1252→UTF-8 double-encoding in an identifier, for display only.
///
/// A MySQL client that writes through the wrong connection charset
/// (latin1/CP1252) stores each byte of the correct UTF-8 sequence as a separate
/// CP1252 character. dbx-core already reverses this for cell values and table
/// comments (`fix_potential_double_encoding` in `db/db/mysql.rs`) but not for
/// table / database / column names, so dbxt applies the same reversal when
/// *rendering* identifiers. The raw name is always what is sent to the server,
/// and correctly stored CJK names (chars > U+00FF) pass through untouched.
///
/// Real data can carry more than one such layer (a name written through a latin1
/// connection twice, or a latin1 dump imported into a latin1 connection). One
/// pass is not enough there: the intermediate string already contains non-Latin-1
/// characters (`•`, `™`) and is therefore mistaken for a successful decode. So
/// repeat the reversal until it stops changing (bounded, so a pathological input
/// can never loop) and peel every layer off.
pub(crate) fn fix_double_encoding(s: &str) -> String {
    let mut current = s.to_string();
    // Two layers is the realistic worst case; 4 leaves headroom and still
    // terminates immediately for clean names (first pass is a no-op).
    for _ in 0..4 {
        let next = reverse_double_encoding_once(&current);
        if next == current {
            break;
        }
        current = next;
    }
    current
}

/// One CP1252→UTF-8 reversal pass. Returns the input unchanged when the bytes
/// are not valid UTF-8 or the result carries no char above U+00FF (i.e. the
/// reversal did not reveal CJK, so it is assumed to have been a false positive).
pub(crate) fn reverse_double_encoding_once(s: &str) -> String {
    let mut bytes = Vec::with_capacity(s.len());
    for c in s.chars() {
        let byte = match c as u32 {
            0x20AC => 0x80,
            0x201A => 0x82,
            0x0192 => 0x83,
            0x201E => 0x84,
            0x2026 => 0x85,
            0x2020 => 0x86,
            0x2021 => 0x87,
            0x02C6 => 0x88,
            0x2030 => 0x89,
            0x0160 => 0x8A,
            0x2039 => 0x8B,
            0x0152 => 0x8C,
            0x017D => 0x8E,
            0x2018 => 0x91,
            0x2019 => 0x92,
            0x201C => 0x93,
            0x201D => 0x94,
            0x2022 => 0x95,
            0x2013 => 0x96,
            0x2014 => 0x97,
            0x02DC => 0x98,
            0x2122 => 0x99,
            0x0161 => 0x9A,
            0x203A => 0x9B,
            0x0153 => 0x9C,
            0x017E => 0x9E,
            0x0178 => 0x9F,
            v if v <= 0xFF => v as u8,
            _ => return s.to_string(),
        };
        bytes.push(byte);
    }
    match String::from_utf8(bytes) {
        Ok(decoded) if decoded.chars().any(|c| c > '\u{00FF}') => decoded,
        _ => s.to_string(),
    }
}

/// Hard-wrap text to `width` display columns, returning physical lines.
pub(crate) fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    for line in text.split('\n') {
        if line.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut cur = String::new();
        let mut w = 0usize;
        for c in line.chars() {
            let cw = UnicodeWidthChar::width(c).unwrap_or(0).max(1);
            if w + cw > width && !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
                w = 0;
            }
            cur.push(c);
            w += cw;
        }
        out.push(cur);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// Wrap a multi-line SQL statement for display, dropping blank lines so the
/// preview stays compact.
pub(crate) fn wrap_sql_lines(sql: &str, width: usize) -> Vec<String> {
    wrap_text(sql, width)
        .into_iter()
        .filter(|l| !l.trim().is_empty())
        .collect()
}
