//! Codec9: complete owning maps and their Note annotations, never combined views.
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::sync::OnceLock;

pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_NODES: usize = 50_000;
pub const MAX_DEPTH: usize = 512;
pub type Result<T> = std::result::Result<T, String>;
fn invalid<T>() -> Result<T> {
    Err("invalid_map_payload".into())
}
fn text(v: &Value, max: usize) -> bool {
    v.as_str()
        .is_some_and(|s| s.chars().count() <= max && !s.contains('\0'))
}
fn id(v: &Value) -> bool {
    v.as_str()
        .is_some_and(|s| !s.is_empty() && s.len() <= 512 && !s.contains('\0'))
}
fn fields(v: &Value, kind: &str, required: &[&str]) -> Result<()> {
    static CONTRACT: OnceLock<Value> = OnceLock::new();
    let contract = CONTRACT.get_or_init(|| {
        serde_json::from_str(include_str!("../../src/cloud/mapCodecV1.contract.json")).unwrap()
    });
    let allowed = contract["fields"][kind].as_array().unwrap();
    let o = v.as_object().ok_or("invalid_map_payload")?;
    if required.iter().any(|k| !o.contains_key(*k)) {
        return invalid();
    }
    if o.keys()
        .any(|k| !allowed.iter().any(|a| a.as_str() == Some(k)))
    {
        return Err("map_unsupported_extension".into());
    }
    Ok(())
}
fn exact(v: &Value, keys: &[&str]) -> Result<()> {
    let o = v.as_object().ok_or("invalid_map_payload")?;
    if o.len() != keys.len() || keys.iter().any(|k| !o.contains_key(*k)) {
        return invalid();
    }
    Ok(())
}
fn string_bytes(s: &str) -> usize {
    2 + s
        .chars()
        .map(|c| match c {
            '\"' | '\\' | '\n' | '\r' | '\t' | '\u{0008}' | '\u{000c}' => 2,
            c if c < '\u{0020}' => 6,
            c => c.len_utf8(),
        })
        .sum::<usize>()
}
// Count canonical UTF-8 bytes before recursive validation or serialization.
// This bounds the aggregate, not just each otherwise valid topic/image string.
fn resource_preflight(values: &[&Value]) -> Result<()> {
    let mut stack = values.iter().map(|v| (*v, 0usize)).collect::<Vec<_>>();
    let mut bytes = 0usize;
    while let Some((value, depth)) = stack.pop() {
        if depth > MAX_DEPTH * 2 + 32 {
            return Err("map_resource_limit".into());
        }
        bytes += match value {
            Value::String(s) => string_bytes(s),
            Value::Array(a) => {
                stack.extend(a.iter().map(|v| (v, depth + 1)));
                2 + a.len().saturating_sub(1)
            }
            Value::Object(o) => {
                stack.extend(o.values().map(|v| (v, depth + 1)));
                2 + o.len().saturating_sub(1) + o.keys().map(|k| string_bytes(k) + 1).sum::<usize>()
            }
            Value::Number(n) => number(n).len(),
            Value::Bool(true) => 4,
            Value::Bool(false) => 5,
            Value::Null => 4,
        };
        if bytes > MAX_FRAME_BYTES - 20 {
            return Err("map_resource_limit".into());
        }
    }
    Ok(())
}
fn strings(v: &Value, count: usize, length: usize) -> Result<()> {
    let a = v.as_array().ok_or("invalid_map_payload")?;
    if a.len() > count || a.iter().any(|v| !text(v, length)) {
        return invalid();
    }
    Ok(())
}
fn point(v: &Value) -> Result<()> {
    fields(v, "position", &["x", "y"])?;
    if ["x", "y"].iter().any(|k| {
        !v[k]
            .as_f64()
            .is_some_and(|n| n.is_finite() && n.abs() <= 1_000_000_000.0)
    }) {
        return invalid();
    }
    Ok(())
}
fn style(v: &Value) -> Result<()> {
    fields(v, "style", &[])?;
    if v.as_object().unwrap().values().any(|v| {
        !(text(v, 2048)
            || v.as_f64()
                .is_some_and(|n| n.is_finite() && n.abs() <= 1_000_000_000.0))
    }) {
        return invalid();
    }
    Ok(())
}
fn node(
    v: &Value,
    depth: usize,
    ids: &mut HashSet<String>,
    notes: &mut BTreeMap<String, String>,
    count: &mut usize,
) -> Result<()> {
    if depth > MAX_DEPTH || *count >= MAX_NODES {
        return Err("map_resource_limit".into());
    }
    *count += 1;
    fields(v, "node", &["id", "topic", "children"])?;
    if !id(&v["id"]) || !text(&v["topic"], 300_000) || !ids.insert(v["id"].as_str().unwrap().into())
    {
        return invalid();
    }
    for k in ["root", "expanded", "nfprogressFreeRoot", "nfprogressNote"] {
        if let Some(n) = v.get(k) {
            if !n.is_boolean() {
                return invalid();
            }
        }
    }
    if let Some(n) = v.get("direction") {
        if !n.as_i64().is_some_and(|n| (0..=2).contains(&n)) {
            return invalid();
        }
    }
    if let Some(n) = v.get("style") {
        style(n)?
    }
    if let Some(n) = v.get("position") {
        point(n)?
    }
    for k in ["hyperLink", "branchColor"] {
        if let Some(n) = v.get(k) {
            if !text(n, 8192) {
                return invalid();
            }
        }
    }
    for k in ["tags", "icons"] {
        if let Some(n) = v.get(k) {
            strings(n, 100, 512)?
        }
    }
    if let Some(n) = v.get("image") {
        fields(n, "image", &["url", "width", "height"])?;
        if !text(&n["url"], MAX_FRAME_BYTES)
            || ["width", "height"]
                .iter()
                .any(|k| !n[k].as_f64().is_some_and(|x| x > 0.0 && x <= 1_000_000.0))
            || n.get("fit").is_some_and(|f| !text(f, 32))
        {
            return invalid();
        }
    }
    if v["nfprogressNote"] == true {
        notes.insert(
            v["id"].as_str().unwrap().into(),
            v["topic"].as_str().unwrap().into(),
        );
    }
    for c in v["children"].as_array().ok_or("invalid_map_payload")? {
        node(c, depth + 1, ids, notes, count)?
    }
    Ok(())
}
/// Validate directly. Normalization is never used to discard unsupported source data.
pub fn validate_map(data: &Value, annotations: &Value) -> Result<BTreeMap<String, String>> {
    resource_preflight(&[data, annotations])?;
    fields(data, "map", &["nodeData"])?;
    let mut ids = HashSet::new();
    let mut notes = BTreeMap::new();
    let mut count = 0;
    let mut tree_notes = BTreeMap::new();
    node(&data["nodeData"], 0, &mut ids, &mut tree_notes, &mut count)?;
    // Production map Notes are native free nodes or legacy floating Note items.
    if !tree_notes.is_empty() {
        return Err("map_note_link_invalid".into());
    }
    if let Some(free) = data.get("freeNodes") {
        for n in free.as_array().ok_or("invalid_map_payload")? {
            node(n, 0, &mut ids, &mut notes, &mut count)?
        }
    }
    let mut floating = HashSet::new();
    if let Some(items) = data.get("nfprogressFloatingItems") {
        let items = items.as_array().ok_or("invalid_map_payload")?;
        if items.len() + count > MAX_NODES {
            return Err("map_resource_limit".into());
        }
        for item in items {
            fields(item, "floating", &["id", "kind", "text", "x", "y"])?;
            if !id(&item["id"])
                || !text(&item["text"], 300_000)
                || !matches!(item["kind"].as_str(), Some("node" | "note"))
                || !floating.insert(item["id"].as_str().unwrap().to_string())
            {
                return invalid();
            }
            point(&json!({"x":item["x"],"y":item["y"]}))?;
            if ["x", "y"]
                .iter()
                .any(|k| !item[k].as_f64().is_some_and(|n| (0.0..=100.0).contains(&n)))
            {
                return invalid();
            }
            let key = item["id"].as_str().unwrap();
            if ids.contains(key) {
                if item["kind"] != "note"
                    || notes.get(key).map(String::as_str) != item["text"].as_str()
                {
                    return Err("map_note_link_invalid".into());
                }
            } else if item["kind"] == "note" {
                notes.insert(key.into(), item["text"].as_str().unwrap().into());
            }
        }
        // Parent links are a forest. Ordinary arrows may legitimately cycle.
        let parents: BTreeMap<&str, &str> = items
            .iter()
            .filter_map(|item| {
                item.get("parentId")
                    .and_then(Value::as_str)
                    .map(|parent| (item["id"].as_str().unwrap(), parent))
            })
            .collect();
        let mut done = HashSet::new();
        for start in parents.keys() {
            let mut path = HashSet::new();
            let mut current = *start;
            while !done.contains(current) {
                if !path.insert(current) {
                    return invalid();
                }
                let Some(parent) = parents.get(current) else {
                    break;
                };
                current = parent;
            }
            done.extend(path);
        }
        for item in items {
            if let Some(p) = item.get("parentId") {
                if item["kind"] != "node"
                    || !id(p)
                    || p == &item["id"]
                    || !floating.contains(p.as_str().unwrap())
                {
                    return invalid();
                }
            }
        }
    }
    let mut children = BTreeMap::new();
    let mut stack = vec![&data["nodeData"]];
    if let Some(free) = data.get("freeNodes").and_then(Value::as_array) {
        stack.extend(free);
    }
    while let Some(n) = stack.pop() {
        let list = n["children"].as_array().unwrap();
        children.insert(n["id"].as_str().unwrap(), list.len());
        stack.extend(list);
    }
    let mut refs = HashSet::new();
    let mut references = 0;
    for (key, kind, required) in [
        ("arrows", "arrow", vec!["id", "from", "to"]),
        (
            "summaries",
            "summary",
            vec!["id", "parent", "start", "end", "label"],
        ),
        (
            "nfprogressFloatingLinks",
            "link",
            vec!["id", "fromType", "from", "toType", "to"],
        ),
    ] {
        if let Some(items) = data.get(key) {
            for item in items.as_array().ok_or("invalid_map_payload")? {
                references += 1;
                if references > 50_000 {
                    return Err("map_resource_limit".into());
                }
                fields(item, kind, &required)?;
                if !id(&item["id"]) || !refs.insert(item["id"].as_str().unwrap().to_string()) {
                    return invalid();
                }
                if let Some(s) = item.get("style") {
                    style(s)?
                }
                if let Some(l) = item.get("label") {
                    if !text(l, 300_000) {
                        return invalid();
                    }
                }
                if kind == "summary" {
                    if !id(&item["parent"])
                        || !ids.contains(item["parent"].as_str().unwrap())
                        || !item["start"].as_u64().is_some_and(|n| n < 50_000)
                        || !item["end"].as_u64().is_some_and(|n| n < 50_000)
                        || item["start"].as_u64() > item["end"].as_u64()
                    {
                        return invalid();
                    }
                    if item
                        .get("nfprogressFreeSelf")
                        .is_some_and(|v| !v.is_boolean())
                    {
                        return invalid();
                    }
                    if item["nfprogressFreeSelf"] == true {
                        if item["start"] != 0 || item["end"] != 0 {
                            return invalid();
                        }
                    } else if item["end"].as_u64().unwrap()
                        >= *children.get(item["parent"].as_str().unwrap()).unwrap() as u64
                    {
                        return Err("map_reference_missing".into());
                    }
                } else {
                    for end in ["from", "to"] {
                        if !id(&item[end]) {
                            return invalid();
                        }
                        let known = if kind == "link" {
                            match item[format!("{end}Type")].as_str() {
                                Some("floating") => floating.contains(item[end].as_str().unwrap()),
                                Some("node") => ids.contains(item[end].as_str().unwrap()),
                                _ => false,
                            }
                        } else {
                            ids.contains(item[end].as_str().unwrap())
                        };
                        if !known {
                            return Err("map_reference_missing".into());
                        }
                    }
                    for k in ["delta1", "delta2"] {
                        if let Some(p) = item.get(k) {
                            point(p)?
                        }
                    }
                    if item.get("bidirectional").is_some_and(|v| !v.is_boolean()) {
                        return invalid();
                    }
                }
            }
        }
    }
    if let Some(n) = data.get("direction") {
        if !n.as_i64().is_some_and(|n| (0..=2).contains(&n)) {
            return invalid();
        }
    }
    if let Some(n) = data.get("compact") {
        if !n.is_boolean() {
            return invalid();
        }
    }
    if let Some(n) = data.get("meta") {
        fields(n, "meta", &[])?
    }
    if let Some(t) = data.get("theme") {
        fields(t, "theme", &["name", "type", "palette", "cssVar"])?;
        if !text(&t["name"], 512) || !text(&t["type"], 64) {
            return invalid();
        }
        strings(&t["palette"], 256, 512)?;
        let css = t["cssVar"].as_object().ok_or("invalid_map_payload")?;
        if css.len() > 128
            || css
                .iter()
                .any(|(k, v)| !k.starts_with("--") || k.len() > 128 || !text(v, 2048))
        {
            return invalid();
        }
    }
    let a = annotations.as_object().ok_or("invalid_map_payload")?;
    if a.len() > MAX_NODES || a.len() != notes.len() {
        return Err("map_note_link_invalid".into());
    }
    let mut note_ids = HashSet::new();
    for (source, v) in a {
        if !notes.contains_key(source) {
            return Err("map_note_link_invalid".into());
        }
        fields(
            v,
            "annotation",
            &[
                "note_id",
                "title",
                "checklist",
                "tags",
                "color",
                "pinned",
                "archived",
                "sort_order",
                "metadata",
                "created_at",
            ],
        )?;
        fields(&v["metadata"], "meta", &[])?;
        if !id(&v["note_id"])
            || !note_ids.insert(v["note_id"].as_str().unwrap())
            || !text(&v["title"], 500)
            || !text(&v["color"], 32)
            || !v["pinned"].is_boolean()
            || !v["archived"].is_boolean()
            || !v["sort_order"]
                .as_i64()
                .is_some_and(|n| (0..=9_007_199_254_740_991).contains(&n))
            || !v["created_at"]
                .as_str()
                .is_some_and(crate::project_metadata_sync::timestamp)
        {
            return invalid();
        }
        strings(&v["tags"], 100, 512)?;
        let list = v["checklist"].as_array().ok_or("invalid_map_payload")?;
        if list.len() > 500 {
            return Err("map_resource_limit".into());
        }
        let mut checks = HashSet::new();
        for c in list {
            fields(c, "checklist", &["id", "text", "checked"])?;
            if !id(&c["id"])
                || !checks.insert(c["id"].as_str().unwrap())
                || !text(&c["text"], 1000)
                || !c["checked"].is_boolean()
            {
                return invalid();
            }
        }
    }
    Ok(notes)
}

pub fn entity_id(stage: Option<&str>) -> String {
    stage
        .map(|s| format!("stage-map-{:x}", Sha256::digest(s.as_bytes())))
        .unwrap_or_else(|| "project-map".into())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Header {
    pub account_id: String,
    pub device_id: String,
    pub project_id: String,
    pub stage_id: Option<String>,
    pub entity_id: String,
    pub event_id: String,
    pub bootstrap_id: String,
    pub metadata_event_id: String,
    pub stage_event_ids: Vec<String>,
    pub parents: Vec<String>,
    pub revision: i64,
    pub generation: i64,
    pub operation: String,
    pub updated_at: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub version: i64,
    pub header: Header,
    pub mutation: String,
    pub map: Value,
    pub deleted_at: Value,
}
pub fn validate(event: &Event) -> Result<()> {
    let h = &event.header;
    if event.version != 1
        || ![
            &h.account_id,
            &h.device_id,
            &h.event_id,
            &h.bootstrap_id,
            &h.metadata_event_id,
        ]
        .iter()
        .all(|s| crate::project_metadata_sync::uuid(s))
        || !id(&json!(h.project_id))
        || h.stage_id.as_ref().is_some_and(|s| !id(&json!(s)))
        || h.entity_id != entity_id(h.stage_id.as_deref())
        || !crate::project_metadata_sync::timestamp(&h.updated_at)
        || !(1..=9_007_199_254_740_991).contains(&h.revision)
        || h.generation != h.revision
        || h.parents.len() > 64
        || h.stage_event_ids.len() > 64
        || h.stage_id.is_none() != h.stage_event_ids.is_empty()
    {
        return invalid();
    }
    for ids in [&h.parents, &h.stage_event_ids] {
        if ids
            .iter()
            .any(|s| !crate::project_metadata_sync::uuid(s) || s == &h.event_id)
            || ids.windows(2).any(|p| p[0] >= p[1])
        {
            return invalid();
        }
    }
    if match h.operation.as_str() {
        "create" => !h.parents.is_empty() || h.revision != 1,
        "update" | "delete" => h.parents.len() != 1 || h.revision < 2,
        "resolution" => h.parents.len() < 2 || h.revision < 2,
        _ => true,
    } {
        return invalid();
    }
    match event.mutation.as_str() {
        "upsert" => {
            if !event.deleted_at.is_null() || h.operation == "delete" {
                return invalid();
            }
            exact(&event.map, &["data", "annotations"])?;
            validate_map(&event.map["data"], &event.map["annotations"])?;
        }
        "delete" => {
            if !event.map.is_null()
                || event.deleted_at != json!(h.updated_at)
                || !matches!(h.operation.as_str(), "delete" | "resolution")
            {
                return invalid();
            }
        }
        _ => return invalid(),
    }
    Ok(())
}
pub fn dependencies_ready(
    db: &Connection,
    a: &str,
    user: &str,
    source: &str,
    event: &Event,
) -> Result<()> {
    let h = &event.header;
    if h.account_id != user || h.device_id != source {
        return Err("map_scope_mismatch".into());
    }
    crate::account_catalog::project_reference_ready(
        db,
        a,
        &h.project_id,
        &h.bootstrap_id,
        &h.metadata_event_id,
    )
    .map_err(|_| "project_metadata_authority_unresolved".to_string())?;
    if let Some(stage) = &h.stage_id {
        crate::stage_sync::content_reference_ready(db, a, &h.project_id, stage, &h.stage_event_ids)?
    }
    Ok(())
}

// JSON numbers use ECMAScript's fixed notation in [1e-6,1e21), matching the
// existing TS canonical serializer for fractional map coordinates/styles.
fn number(n: &serde_json::Number) -> String {
    let raw = n.to_string();
    if n.as_f64() == Some(0.0) {
        return "0".into();
    }
    let negative = raw.starts_with('-');
    let unsigned = raw.trim_start_matches('-');
    let (mantissa, exponent) = unsigned
        .split_once(['e', 'E'])
        .map(|(m, e)| (m, e.parse::<i32>().unwrap()))
        .unwrap_or((unsigned, 0));
    let dot = mantissa.find('.').unwrap_or(mantissa.len()) as i32;
    let mut digits = mantissa.replace('.', "");
    let mut position = dot + exponent;
    while digits.starts_with('0') && digits.len() > 1 {
        digits.remove(0);
        position -= 1;
    }
    while digits.ends_with('0') && digits.len() > 1 {
        digits.pop();
    }
    let sign = if negative { "-" } else { "" };
    if position > 0 && position <= 21 {
        if position as usize >= digits.len() {
            format!(
                "{sign}{digits}{}",
                "0".repeat(position as usize - digits.len())
            )
        } else {
            format!(
                "{sign}{}.{}",
                &digits[..position as usize],
                &digits[position as usize..]
            )
        }
    } else if position <= 0 && position >= -5 {
        format!("{sign}0.{}{digits}", "0".repeat((-position) as usize))
    } else {
        let e = position - 1;
        format!(
            "{sign}{}{}e{}{e}",
            &digits[..1],
            if digits.len() > 1 {
                format!(".{}", &digits[1..])
            } else {
                String::new()
            },
            if e >= 0 { "+" } else { "" }
        )
    }
}
pub fn canonical(v: &Value) -> String {
    match v {
        Value::Number(n) => number(n),
        Value::Array(a) => format!(
            "[{}]",
            a.iter().map(canonical).collect::<Vec<_>>().join(",")
        ),
        Value::Object(o) => {
            let mut keys = o.keys().collect::<Vec<_>>();
            keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
            format!(
                "{{{}}}",
                keys.iter()
                    .map(|k| format!(
                        "{}:{}",
                        serde_json::to_string(k).unwrap(),
                        canonical(&o[*k])
                    ))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        }
        _ => serde_json::to_string(v).unwrap(),
    }
}
pub fn encode(event: &Event) -> Result<Vec<u8>> {
    bounded_stack(|| encode_inner(event))
}
// The existing editor allows depth 512. Serde's default recursion limit and
// platform worker stack are insufficient for that valid, bounded representation.
pub(crate) fn bounded_stack<T: Send>(work: impl FnOnce() -> Result<T> + Send) -> Result<T> {
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name("map-codec".into())
            .stack_size(16 * 1024 * 1024)
            .spawn_scoped(scope, work)
            .map_err(|_| "map_codec_unavailable".to_string())?
            .join()
            .map_err(|_| "invalid_map_payload".to_string())?
    })
}
fn encode_inner(event: &Event) -> Result<Vec<u8>> {
    validate(event)?;
    let value = serde_json::to_value(event).map_err(|_| "invalid_map_payload")?;
    resource_preflight(&[&value])?;
    let body = canonical(&value).into_bytes();
    if body.len() + 20 > MAX_FRAME_BYTES {
        return Err("map_resource_limit".into());
    }
    let mut frame = b"WORTA-C1".to_vec();
    frame.extend([1, 9, 1, 0]);
    frame.extend((body.len() as u32).to_be_bytes());
    frame.extend((body.len() as u32).to_be_bytes());
    frame.extend(body);
    Ok(frame)
}
pub fn decode(frame: &[u8]) -> Result<Event> {
    let normalized;
    let frame = if frame.len() >= 20
        && frame[11] != 0
        && [9].contains(&frame[9])
        && [1].contains(&frame[10])
    {
        normalized = crate::frame_compression::normalize_authenticated_frame(
            frame,
            &[9],
            &[1],
            MAX_FRAME_BYTES - 20,
        )
        .map_err(str::to_string)?;
        normalized.as_slice()
    } else {
        frame
    };

    if frame.len() > MAX_FRAME_BYTES {
        return Err("map_resource_limit".into());
    }
    if frame.len() < 20 || &frame[..8] != b"WORTA-C1" || frame[8..12] != [1, 9, 1, 0] {
        return Err("map_codec_unsupported".into());
    }
    let length = u32::from_be_bytes(frame[12..16].try_into().unwrap()) as usize;
    if frame[12..16] != frame[16..20] || length != frame.len() - 20 {
        return invalid();
    }
    // Bound nesting before enabling deep parsing, ignoring quoted brackets.
    let mut depth = 0usize;
    let mut quoted = false;
    let mut escaped = false;
    for b in &frame[20..] {
        if quoted {
            if escaped {
                escaped = false;
            } else if *b == b'\\' {
                escaped = true;
            } else if *b == b'"' {
                quoted = false;
            }
        } else {
            match b {
                b'"' => quoted = true,
                b'{' | b'[' => {
                    depth += 1;
                    if depth > MAX_DEPTH * 2 + 32 {
                        return Err("map_resource_limit".into());
                    }
                }
                b'}' | b']' => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
    }
    bounded_stack(|| decode_body(frame))
}
fn decode_body(frame: &[u8]) -> Result<Event> {
    let mut parser = serde_json::Deserializer::from_slice(&frame[20..]);
    parser.disable_recursion_limit();
    let value = Value::deserialize(&mut parser).map_err(|_| "invalid_map_payload")?;
    parser.end().map_err(|_| "invalid_map_payload")?;
    if canonical(&value).as_bytes() != &frame[20..] {
        return invalid();
    }
    let event: Event = serde_json::from_value(value).map_err(|_| "invalid_map_payload")?;
    validate(&event)?;
    Ok(event)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Value {
        serde_json::from_str(include_str!("../../src/cloud/__fixtures__/mapCodecV1.json")).unwrap()
    }
    #[test]
    fn map_codec_existing_editor_depth_and_parent_cycle_bounds() {
        let mut e: Event =
            serde_json::from_value(fixture()["examples"][0]["event"].clone()).unwrap();
        let mut tree = json!({"id":"deep-512","topic":"leaf","children":[]});
        for depth in (0..512).rev() {
            tree = json!({"id":format!("deep-{depth}"),"topic":"node","children":[tree]});
        }
        e.map = json!({"data":{"nodeData":tree},"annotations":{}});
        let frame = encode(&e).unwrap();
        assert_eq!(encode(&decode(&frame).unwrap()).unwrap(), frame);
        e.map["data"]["nodeData"] =
            json!({"id":"too-deep","topic":"node","children":[e.map["data"]["nodeData"].take()]});
        assert_eq!(encode(&e).unwrap_err(), "map_resource_limit");
        e.map = json!({"data":{"nodeData":{"id":"root","topic":"root","children":[]},
            "nfprogressFloatingItems":[
                {"id":"a","kind":"node","text":"A","x":1,"y":1,"parentId":"b"},
                {"id":"b","kind":"node","text":"B","x":2,"y":2,"parentId":"a"}
            ]},"annotations":{}});
        assert_eq!(encode(&e).unwrap_err(), "invalid_map_payload");
    }
    #[test]
    fn map_codec_aggregate_utf8_bound_precedes_serialization() {
        let children = (0..30)
            .map(|i| {
                json!({"id":format!("large-{i}"),
            "topic":"a".repeat(300_000),"children":[]})
            })
            .collect::<Vec<_>>();
        let data = json!({"nodeData":{"id":"root","topic":"root","children":children}});
        assert_eq!(
            validate_map(&data, &json!({})).unwrap_err(),
            "map_resource_limit"
        );
        let data = json!({"nodeData":{"id":"root","topic":"a".repeat(300_000),"children":[]}});
        assert!(validate_map(&data, &json!({})).is_ok());
    }
    #[test]
    fn map_codec_cross_language_vectors_and_fractional_coordinates() {
        for v in fixture()["examples"].as_array().unwrap() {
            let e: Event = serde_json::from_value(v["event"].clone()).unwrap();
            let frame = encode(&e).unwrap();
            let hex = frame.iter().map(|b| format!("{b:02x}")).collect::<String>();
            assert_eq!(hex, v["frame_hex"].as_str().unwrap());
            assert_eq!(
                canonical(&serde_json::to_value(decode(&frame).unwrap()).unwrap()),
                v["canonical_json"].as_str().unwrap()
            );
        }
    }
    #[test]
    fn map_codec_extensions_references_and_duplicate_note_identity_fail_closed() {
        let base = fixture()["examples"][2]["event"].clone();
        let mut e: Event = serde_json::from_value(base).unwrap();
        e.map["data"]["nfprogressFloatingItems"] =
            json!([{"id":"note-1","kind":"note","text":"Map Note text","x":10,"y":20}]);
        assert_eq!(
            validate_map(&e.map["data"], &e.map["annotations"])
                .unwrap()
                .len(),
            1
        );
        e.map["data"]["nfprogressFloatingItems"][0]["text"] = json!("Different text");
        assert_eq!(encode(&e).unwrap_err(), "map_note_link_invalid");
        e.map["data"]
            .as_object_mut()
            .unwrap()
            .remove("nfprogressFloatingItems");
        e.map["data"]["customExtension"] = json!({"text":"retain source"});
        assert_eq!(encode(&e).unwrap_err(), "map_unsupported_extension");
        e.map["data"]
            .as_object_mut()
            .unwrap()
            .remove("customExtension");
        e.map["data"]["arrows"][0]["to"] = json!("foreign");
        assert_eq!(encode(&e).unwrap_err(), "map_reference_missing");
    }
    #[test]
    fn map_codec_owner_and_frame_substitution_rejected() {
        let mut e: Event =
            serde_json::from_value(fixture()["examples"][0]["event"].clone()).unwrap();
        let frame = encode(&e).unwrap();
        for i in [9, 10, 11, 12] {
            let mut corrupt = frame.clone();
            corrupt[i] ^= 1;
            assert!(decode(&corrupt).is_err());
        }
        e.header.stage_id = Some("S1".into());
        e.header.stage_event_ids = vec!["123e4567-e89b-42d3-a456-426614174004".into()];
        assert!(encode(&e).is_err());
        assert_ne!(entity_id(Some("S1")), entity_id(Some("S2")));
        assert_eq!(
            canonical(&json!({"a":1.0,"b":0.000001,"c":0.0000001,"d":-0.0})),
            "{\"a\":1,\"b\":0.000001,\"c\":1e-7,\"d\":0}"
        );
    }
}
