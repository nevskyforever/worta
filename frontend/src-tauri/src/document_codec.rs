//! Codec10: title, Tiptap content and scope are one complete document version.
use crate::project_metadata_sync as metadata;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
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
    pub document: Value,
    pub deleted_at: Value,
}
type Result<T> = std::result::Result<T, String>;
fn fail<T>() -> Result<T> {
    Err("invalid_document_payload".into())
}
fn exact(v: &Value, fields: &[&str], required: &[&str]) -> Result<()> {
    let o = v.as_object().ok_or("invalid_document_payload")?;
    if required.iter().any(|k| !o.contains_key(*k))
        || o.keys().any(|k| !fields.contains(&k.as_str()))
    {
        return Err("document_unsupported_structure".into());
    }
    Ok(())
}
fn id(v: &Value) -> bool {
    v.as_str()
        .is_some_and(|s| !s.is_empty() && s.len() <= 512 && !s.contains('\0'))
}
fn attrs(v: &Value, allowed: &Value) -> Result<()> {
    let o = v.as_object().ok_or("invalid_document_payload")?;
    let keys = allowed.as_array().unwrap();
    for (k, v) in o {
        if !keys.iter().any(|a| a == k) || (!v.is_null() && !v.is_string() && !v.is_number()) {
            return Err("document_unsupported_structure".into());
        }
        if !matches!(k.as_str(), "level" | "start") && !v.is_null() && !v.is_string() {
            return fail();
        }
        if k == "textAlign"
            && !v.is_null()
            && !v
                .as_str()
                .is_some_and(|s| matches!(s, "left" | "center" | "right" | "justify"))
        {
            return fail();
        }
        if v.as_str()
            .is_some_and(|s| s.len() > 2048 || s.contains('\0'))
        {
            return Err("document_resource_limit".into());
        }
        if k == "level" && !v.as_i64().is_some_and(|i| (1..=6).contains(&i)) {
            return fail();
        }
        if k == "start"
            && !v
                .as_i64()
                .is_some_and(|i| (1..=9007199254740991).contains(&i))
        {
            return fail();
        }
        if k == "href"
            && v.as_str().is_some_and(|s| {
                !(s.starts_with("https://")
                    || s.starts_with("http://")
                    || s.starts_with("mailto:")
                    || s.starts_with('#'))
            })
        {
            return Err("document_unsupported_structure".into());
        }
    }
    Ok(())
}
pub fn validate_content(v: &Value) -> Result<()> {
    let contract: Value = serde_json::from_str(include_str!(
        "../../src/cloud/documentCodecV1.contract.json"
    ))
    .unwrap();
    let mut stack = vec![(v, 0usize, "", false)];
    let mut count = 0;
    while let Some((n, depth, parent, is_mark)) = stack.pop() {
        count += 1;
        if count > 50000 || depth > 60 {
            return Err("document_resource_limit".into());
        }
        exact(n, &["type", "attrs", "content", "text", "marks"], &["type"])?;
        let kind = n["type"].as_str().ok_or("invalid_document_payload")?;
        let allowed = &contract[if is_mark { "marks" } else { "nodes" }][kind];
        if allowed.is_null() {
            return Err("document_unsupported_structure".into());
        }
        if depth == 0 && kind != "doc" || depth > 0 && !is_mark && kind == "doc" {
            return fail();
        }
        if let Some(a) = n.get("attrs") {
            attrs(a, allowed)?
        }
        if is_mark {
            if n.get("content").is_some() || n.get("text").is_some() || n.get("marks").is_some() {
                return fail();
            }
            continue;
        }
        let inline = matches!(kind, "text" | "hardBreak");
        if !parent.is_empty() {
            let permitted = match parent {
                "paragraph" | "heading" => inline,
                "codeBlock" => kind == "text",
                "bulletList" | "orderedList" => kind == "listItem",
                "listItem" | "blockquote" | "doc" => !inline && kind != "listItem",
                _ => false,
            };
            if !permitted {
                return fail();
            }
        }
        if kind == "text" {
            let s = n["text"].as_str().ok_or("invalid_document_payload")?;
            if s.is_empty() || s.len() > 1048576 || s.contains('\0') {
                return Err("document_resource_limit".into());
            }
            if n.get("content").is_some() {
                return fail();
            }
        } else if n.get("text").is_some() {
            return fail();
        }
        if let Some(marks) = n.get("marks") {
            if !inline || kind != "text" {
                return fail();
            }
            let marks = marks.as_array().ok_or("invalid_document_payload")?;
            let mut names = std::collections::HashSet::new();
            for mark in marks {
                if !names.insert(mark["type"].as_str()) {
                    return fail();
                }
                stack.push((mark, depth + 1, "", true))
            }
        }
        if kind == "heading" && n["attrs"]["level"].as_i64().is_none() {
            return fail();
        }
        if matches!(
            kind,
            "doc" | "blockquote" | "bulletList" | "orderedList" | "listItem"
        ) && n
            .get("content")
            .and_then(Value::as_array)
            .is_none_or(|c| c.is_empty())
        {
            return fail();
        }
        if kind == "listItem" && n["content"][0]["type"] != "paragraph" {
            return fail();
        }
        if parent == "codeBlock"
            && n.get("marks")
                .is_some_and(|m| m.as_array().is_none_or(|m| !m.is_empty()))
        {
            return fail();
        }
        if let Some(content) = n.get("content") {
            let content = content.as_array().ok_or("invalid_document_payload")?;
            if matches!(kind, "text" | "hardBreak" | "horizontalRule") {
                return fail();
            }
            for child in content {
                stack.push((child, depth + 1, kind, false))
            }
        }
    }
    Ok(())
}
pub fn validate_document(v: &Value) -> Result<()> {
    exact(
        v,
        &[
            "id",
            "project_id",
            "stage_id",
            "title",
            "content_json",
            "content_format",
            "created_at",
            "extensions",
        ],
        &[
            "id",
            "project_id",
            "stage_id",
            "title",
            "content_json",
            "content_format",
            "created_at",
            "extensions",
        ],
    )?;
    if !id(&v["id"])
        || !id(&v["project_id"])
        || !v["stage_id"].is_null() && !id(&v["stage_id"])
        || v["title"]
            .as_str()
            .is_none_or(|s| s.len() > 4096 || s.contains('\0'))
        || v["content_format"] != "tiptap-json/v1"
        || !v["created_at"].is_null()
            && v["created_at"]
                .as_str()
                .is_none_or(|s| !metadata::timestamp(s))
    {
        return fail();
    }
    if v["extensions"] != json!({}) {
        return Err("document_unsupported_extension".into());
    }
    validate_content(&v["content_json"])
}
pub fn validate(e: &Event) -> Result<()> {
    let h = &e.header;
    if e.version != 1
        || ![
            &h.account_id,
            &h.device_id,
            &h.event_id,
            &h.bootstrap_id,
            &h.metadata_event_id,
        ]
        .iter()
        .all(|s| metadata::uuid(s))
        || !id(&json!(h.project_id))
        || !id(&json!(h.entity_id))
        || h.stage_id.as_ref().is_some_and(|s| !id(&json!(s)))
        || !metadata::timestamp(&h.updated_at)
        || !(1..=9007199254740991).contains(&h.revision)
        || h.generation != h.revision
        || h.parents.len() > 64
        || h.stage_event_ids.len() > 64
        || h.stage_id.is_none() != h.stage_event_ids.is_empty()
    {
        return fail();
    }
    for ids in [&h.parents, &h.stage_event_ids] {
        if ids.iter().any(|s| !metadata::uuid(s) || s == &h.event_id)
            || ids.windows(2).any(|p| p[0] >= p[1])
        {
            return fail();
        }
    }
    if match h.operation.as_str() {
        "create" => !h.parents.is_empty() || h.revision != 1,
        "update" | "delete" => h.parents.len() != 1 || h.revision < 2,
        "resolution" => h.parents.len() < 2 || h.revision < 2,
        _ => true,
    } {
        return fail();
    }
    if e.mutation == "upsert" {
        validate_document(&e.document)?;
        if !e.deleted_at.is_null()
            || h.operation == "delete"
            || e.document["id"] != h.entity_id
            || e.document["project_id"] != h.project_id
            || e.document["stage_id"] != json!(h.stage_id)
        {
            return fail();
        }
    } else if e.mutation != "delete"
        || !e.document.is_null()
        || e.deleted_at != json!(h.updated_at)
        || !matches!(h.operation.as_str(), "delete" | "resolution")
    {
        return fail();
    }
    Ok(())
}
pub fn dependencies_ready(
    db: &Connection,
    a: &str,
    user: &str,
    source: &str,
    e: &Event,
) -> Result<()> {
    let h = &e.header;
    if h.account_id != user || h.device_id != source {
        return Err("document_scope_mismatch".into());
    }
    crate::account_catalog::project_reference_ready(
        db,
        a,
        &h.project_id,
        &h.bootstrap_id,
        &h.metadata_event_id,
    )
    .map_err(|_| "project_metadata_authority_unresolved".to_string())?;
    if let Some(s) = &h.stage_id {
        crate::stage_sync::content_reference_ready(db, a, &h.project_id, s, &h.stage_event_ids)?
    }
    Ok(())
}
pub fn canonical(v: &Value) -> String {
    crate::map_codec::canonical(v)
}
fn preflight(v: &Value) -> Result<()> {
    let mut stack = vec![(v, 0usize)];
    let mut size = 0;
    while let Some((v, d)) = stack.pop() {
        if d > 160 {
            return Err("document_resource_limit".into());
        }
        size += match v {
            Value::String(s) => serde_json::to_string(s).unwrap().len(),
            Value::Object(o) => {
                stack.extend(o.values().map(|v| (v, d + 1)));
                2 + o.len() + o.keys().map(|k| k.len() + 3).sum::<usize>()
            }
            Value::Array(a) => {
                stack.extend(a.iter().map(|v| (v, d + 1)));
                2 + a.len()
            }
            _ => 32,
        };
        if size > MAX_FRAME_BYTES - 20 {
            return Err("document_resource_limit".into());
        }
    }
    Ok(())
}
pub fn encode(e: &Event) -> Result<Vec<u8>> {
    let v = serde_json::to_value(e).map_err(|_| "invalid_document_payload")?;
    preflight(&v)?;
    validate(e)?;
    let body = canonical(&v).into_bytes();
    if body.len() + 20 > MAX_FRAME_BYTES {
        return Err("document_resource_limit".into());
    }
    let mut out = b"WORTA-C1".to_vec();
    out.extend([1, 10, 1, 0]);
    out.extend((body.len() as u32).to_be_bytes());
    out.extend((body.len() as u32).to_be_bytes());
    out.extend(body);
    Ok(out)
}
pub fn decode(frame: &[u8]) -> Result<Event> {
    let normalized;
    let frame = if frame.len() >= 20
        && frame[11] != 0
        && [10].contains(&frame[9])
        && [1].contains(&frame[10])
    {
        normalized = crate::frame_compression::normalize_authenticated_frame(
            frame,
            &[10],
            &[1],
            MAX_FRAME_BYTES - 20,
        )
        .map_err(str::to_string)?;
        normalized.as_slice()
    } else {
        frame
    };

    if frame.len() > MAX_FRAME_BYTES {
        return Err("document_resource_limit".into());
    }
    if frame.len() < 20 || &frame[..8] != b"WORTA-C1" || frame[8..12] != [1, 10, 1, 0] {
        return Err("document_codec_unsupported".into());
    }
    if frame[12..16] != frame[16..20]
        || u32::from_be_bytes(frame[12..16].try_into().unwrap()) as usize != frame.len() - 20
    {
        return fail();
    }
    let mut d = 0usize;
    let mut quoted = false;
    let mut escape = false;
    for b in &frame[20..] {
        if quoted {
            if escape {
                escape = false
            } else if *b == b'\\' {
                escape = true
            } else if *b == b'"' {
                quoted = false
            }
        } else {
            match b {
                b'"' => quoted = true,
                b'{' | b'[' => {
                    d += 1;
                    if d > 160 {
                        return Err("document_resource_limit".into());
                    }
                }
                b'}' | b']' => d = d.saturating_sub(1),
                _ => {}
            }
        }
    }
    crate::map_codec::bounded_stack(|| {
        let mut p = serde_json::Deserializer::from_slice(&frame[20..]);
        p.disable_recursion_limit();
        let v = Value::deserialize(&mut p).map_err(|_| "invalid_document_payload")?;
        p.end().map_err(|_| "invalid_document_payload")?;
        if canonical(&v).as_bytes() != &frame[20..] {
            return fail();
        }
        let e = serde_json::from_value(v).map_err(|_| "invalid_document_payload")?;
        validate(&e)?;
        Ok(e)
    })
}
pub(crate) fn bounded_stack<T: Send>(work: impl FnOnce() -> Result<T> + Send) -> Result<T> {
    crate::map_codec::bounded_stack(work)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn document_codec_golden_and_frame_negatives() {
        let f: Value = serde_json::from_str(include_str!(
            "../../src/cloud/__fixtures__/documentCodecV1.json"
        ))
        .unwrap();
        for v in f["examples"].as_array().unwrap() {
            let e: Event = serde_json::from_value(v["event"].clone()).unwrap();
            let bytes = encode(&e).unwrap();
            assert_eq!(
                bytes.iter().map(|b| format!("{b:02x}")).collect::<String>(),
                v["frame_hex"].as_str().unwrap()
            );
            assert_eq!(
                canonical(&serde_json::to_value(decode(&bytes).unwrap()).unwrap()),
                v["canonical_json"].as_str().unwrap()
            );
            for i in [9, 10, 11, 12] {
                let mut bad = bytes.clone();
                bad[i] ^= 1;
                assert!(decode(&bad).is_err())
            }
        }
    }
    #[test]
    fn document_codec_extensions_structures_and_identity() {
        let f: Value = serde_json::from_str(include_str!(
            "../../src/cloud/__fixtures__/documentCodecV1.json"
        ))
        .unwrap();
        let mut e: Event = serde_json::from_value(f["examples"][0]["event"].clone()).unwrap();
        e.document["extensions"] = json!({"legacy_flag":true});
        assert_eq!(encode(&e).unwrap_err(), "document_unsupported_extension");
        e.document["extensions"] = json!({});
        e.header.entity_id = "foreign".into();
        assert!(encode(&e).is_err());
        assert!(validate_content(&json!({"type":"doc","content":[{"type":"image"}]})).is_err());
    }
}
