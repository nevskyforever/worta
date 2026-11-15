//! C18 structural substrate and explicit migration integration.
//! No background migration discovery/capture writer.
//! Authentication/unframing starts in stageCodec; native repeats frame, scope,
//! immutable object, dependencies and causal CAS inside BEGIN IMMEDIATE.
use crate::project_metadata_sync::{self as metadata, MetadataError as Error};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub(crate) const FIELDS: &[&str] = &[
    "name",
    "goal",
    "infinite",
    "unit",
    "status",
    "deadline",
    "personal_goal",
    "auto_freeze",
    "streak_enabled",
    "work_method",
    "created_at",
    "completed_at",
];
const LEGACY: &[&str] = &[
    "id",
    "total",
    "progress",
    "updated_at",
    "notes_updated_at",
    "mindmap_updated_at",
    "map_note_annotations",
    "today_goal",
    "planning_date",
    "plan_daily_goal",
    "added_today",
    "remaining",
    "streak_status",
    "streak_length",
    "max_streak",
    "progress_entries",
    "project_notes",
    "mindmap",
    "stages",
    "stages_enabled",
    "combine_stage_mindmaps",
    "cover_image",
    "folder_id",
    "synch",
    "last_synch",
    "sync_available",
    "parent_project_name",
    "parent_project_id",
];
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Header {
    pub account_id: String,
    pub project_id: String,
    pub bootstrap_id: String,
    pub device_id: String,
    pub entity_id: String,
    pub entity_type: String,
    pub event_id: String,
    pub operation: String,
    pub revision: i64,
    pub generation: i64,
    pub parent_event_ids: Vec<String>,
    pub updated_at: String,
    pub metadata_event_id: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Event {
    pub version: i64,
    pub header: Header,
    pub stage: Value,
    pub stage_ids: Value,
    pub stage_heads: Value,
    pub deleted_at: Value,
}
fn text(v: &Value) -> bool {
    v.as_str().is_some_and(|s| !s.is_empty() && s.len() <= 512)
}
fn stage_valid(v: &Value) -> bool {
    let Some(o) = v.as_object() else { return false };
    o.len() == FIELDS.len()
        && FIELDS.iter().all(|k| o.contains_key(*k))
        && ["name", "unit", "status", "work_method"]
            .iter()
            .all(|k| text(&o[*k]))
        && ["infinite", "auto_freeze", "streak_enabled"]
            .iter()
            .all(|k| o[*k].is_boolean())
        && (o["goal"].is_null()
            || o["goal"]
                .as_f64()
                .is_some_and(|n| n.is_finite() && n >= 0.0 && n <= 9_007_199_254_740_991.0))
        && o["personal_goal"]
            .as_f64()
            .is_some_and(|n| n.is_finite() && n >= 0.0 && n <= 9_007_199_254_740_991.0)
        && ["deadline", "created_at", "completed_at"]
            .iter()
            .all(|k| o[*k].is_null() || text(&o[*k]))
}
fn validate(e: &Event) -> Result<(), Error> {
    let h = &e.header;
    if !matches!(e.version, 1 | 2)
        || ![
            &h.account_id,
            &h.bootstrap_id,
            &h.device_id,
            &h.event_id,
            &h.metadata_event_id,
        ]
        .iter()
        .all(|s| metadata::uuid(s))
        || !text(&json!(h.project_id))
        || !text(&json!(h.entity_id))
        || !matches!(h.entity_type.as_str(), "stage" | "stage_order")
        || !metadata::timestamp(&h.updated_at)
        || !(1..=9_007_199_254_740_991).contains(&h.revision)
        || !(1..=9_007_199_254_740_991).contains(&h.generation)
        || !matches!(h.operation.as_str(), "create" | "update" | "delete")
        || if e.version == 1 {
            h.parent_event_ids.len() != if h.operation == "create" { 0 } else { 1 }
        } else {
            h.operation == "create"
                || h.parent_event_ids.is_empty()
                || h.parent_event_ids.len() > 64
                || !h.parent_event_ids.windows(2).all(|w| w[0] < w[1])
        }
        || h.parent_event_ids
            .iter()
            .any(|p| !metadata::uuid(p) || p == &h.event_id)
        || (h.operation == "create" && (h.revision != 1 || h.generation != 1))
        || (h.operation != "create" && (h.revision < 2 || h.generation < 2))
    {
        return Err(Error::Invalid);
    }
    if h.entity_type == "stage" {
        if !e.stage_ids.is_null()
            || !e.stage_heads.is_null()
            || if h.operation == "delete" {
                !e.stage.is_null() || e.deleted_at != json!(h.updated_at)
            } else {
                !stage_valid(&e.stage) || !e.deleted_at.is_null()
            }
        {
            return Err(Error::Invalid);
        }
    } else {
        let ids = e.stage_ids.as_array().ok_or(Error::Invalid)?;
        let heads = e.stage_heads.as_object().ok_or(Error::Invalid)?;
        if h.entity_id != "stage_order"
            || h.operation == "delete"
            || !e.stage.is_null()
            || !e.deleted_at.is_null()
            || ids.len() > 4096
            || heads.len() != ids.len()
            || ids.iter().any(|s| !text(s))
            || ids.iter().any(|s| {
                !heads
                    .get(s.as_str().unwrap())
                    .and_then(Value::as_array)
                    .is_some_and(|a| {
                        !a.is_empty()
                            && a.len() <= 64
                            && a.iter().all(|v| v.as_str().is_some_and(metadata::uuid))
                            && a.windows(2).all(|w| w[0].as_str() < w[1].as_str())
                    })
            })
            || ids.iter().collect::<std::collections::HashSet<_>>().len() != ids.len()
        {
            return Err(Error::Invalid);
        }
    }
    Ok(())
}
// ECMAScript notation and UTF-16 key order match the TS canonical codec.
pub(crate) fn canonical(v: &Value) -> Result<String, Error> {
    match v {
        Value::Array(a) => Ok(format!(
            "[{}]",
            a.iter()
                .map(canonical)
                .collect::<Result<Vec<_>, _>>()?
                .join(",")
        )),
        Value::Object(o) => {
            let mut keys = o.keys().collect::<Vec<_>>();
            keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
            Ok(format!(
                "{{{}}}",
                keys.into_iter()
                    .map(|k| Ok(format!(
                        "{}:{}",
                        serde_json::to_string(k).map_err(|_| Error::Invalid)?,
                        canonical(&o[k])?
                    )))
                    .collect::<Result<Vec<_>, Error>>()?
                    .join(",")
            ))
        }
        Value::Number(n) => {
            let f = n.as_f64().ok_or(Error::Invalid)?;
            if f == 0.0 {
                return Ok("0".into());
            }
            if f.fract() == 0.0 && f.abs() <= 9_007_199_254_740_991.0 {
                return Ok(format!("{:.0}", f));
            }
            let raw = n.to_string();
            if let Some((mantissa, exponent)) = raw.split_once('e') {
                let exp: i32 = exponent.parse().map_err(|_| Error::Invalid)?;
                if exp >= -6 && exp < 21 {
                    let digits = mantissa.replace('.', "");
                    let point = mantissa.find('.').unwrap_or(mantissa.len()) as i32 + exp;
                    if point <= 0 {
                        return Ok(format!("0.{}{}", "0".repeat((-point) as usize), digits));
                    }
                    if point as usize >= digits.len() {
                        return Ok(format!(
                            "{}{}",
                            digits,
                            "0".repeat(point as usize - digits.len())
                        ));
                    }
                    return Ok(format!(
                        "{}.{}",
                        &digits[..point as usize],
                        &digits[point as usize..]
                    ));
                }
                return Ok(format!(
                    "{}e{}{}",
                    mantissa,
                    if exp >= 0 { "+" } else { "" },
                    exp
                ));
            }
            Ok(raw)
        }
        _ => serde_json::to_string(v).map_err(|_| Error::Invalid),
    }
}
/// Native canonical bytes use serde's sorted map keys and normalized integral
/// numbers; matches stageCodec JSON.stringify for the bounded portable numbers.
pub(crate) fn frame(e: &Event) -> Result<Vec<u8>, Error> {
    validate(e)?;
    let mut v = serde_json::to_value(e).map_err(|_| Error::Invalid)?;
    if !v["stage"].is_null() {
        v["stage"] = metadata::normalize_metadata_numbers(v["stage"].clone());
    }
    let bytes = canonical(&v)?.into_bytes();
    if bytes.len() > 1024 * 1024 {
        return Err(Error::Invalid);
    }
    let mut out = b"WORTA-C1".to_vec();
    out.extend([
        1,
        if e.header.entity_type == "stage" {
            2
        } else {
            3
        },
        e.version as u8,
        0,
    ]);
    out.extend((bytes.len() as u32).to_be_bytes());
    out.extend((bytes.len() as u32).to_be_bytes());
    out.extend(bytes);
    Ok(out)
}
pub(crate) fn unframe(bytes: &[u8]) -> Result<Event, Error> {
    let normalized;
    let bytes = if bytes.len() >= 20
        && bytes[11] != 0
        && [2, 3].contains(&bytes[9])
        && [1, 2].contains(&bytes[10])
    {
        normalized = crate::frame_compression::normalize_authenticated_frame(
            bytes,
            &[2, 3],
            &[1, 2],
            1024 * 1024,
        )
        .map_err(Error::Compression)?;
        normalized.as_slice()
    } else {
        bytes
    };

    if bytes.len() < 20
        || bytes.len() > 1024 * 1024 + 20
        || &bytes[..8] != b"WORTA-C1"
        || bytes[8] != 1
        || !matches!(bytes[9], 2 | 3)
        || !matches!(bytes[10], 1 | 2)
        || bytes[11] != 0
        || u32::from_be_bytes(bytes[12..16].try_into().unwrap()) as usize != bytes.len() - 20
        || bytes[12..16] != bytes[16..20]
    {
        return Err(Error::Invalid);
    }
    let e: Event = serde_json::from_slice(&bytes[20..]).map_err(|_| Error::Invalid)?;
    if frame(&e)? != bytes {
        return Err(Error::Invalid);
    }
    Ok(e)
}
fn tips(db: &Connection, a: &str, p: &str, t: &str, id: &str) -> Result<Vec<String>, Error> {
    let mut q=db.prepare("SELECT event_id FROM cloud_sync_structural_tips WHERE account_id=?1 AND project_id=?2 AND entity_type=?3 AND entity_id=?4 ORDER BY event_id")?;
    let rows = q
        .query_map(params![a, p, t, id], |r| r.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}
/// Active includes reconciliation with visible local metadata; unresolved genesis,
/// differing local shell and tombstoned metadata cannot authorize structural work.
fn dependency(db: &Connection, a: &str, h: &Header) -> Result<Option<&'static str>, Error> {
    let binding:Option<(String,String)>=db.query_row("SELECT u.canonical_user_id,b.bootstrap_id FROM cloud_sync_project_bindings p JOIN cloud_account_bindings u ON u.local_account_id=p.account_id JOIN cloud_sync_project_bootstraps b ON b.account_id=p.account_id AND b.project_id=p.project_id WHERE p.account_id=?1 AND p.project_id=?2",params![a,h.project_id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    let Some((user, boot)) = binding else {
        return Ok(Some("project_metadata_dependency_missing"));
    };
    if user != h.account_id || boot != h.bootstrap_id {
        return Err(Error::Scope);
    }
    let proven:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_metadata_events e JOIN cloud_sync_metadata_apply_ledger l ON l.account_id=e.account_id AND l.event_id=e.event_id WHERE e.account_id=?1 AND e.project_id=?2 AND e.event_id=?3 AND e.bootstrap_id=?4 AND e.state IN ('applied','conflict_preserved') AND e.operation!='delete')",params![a,h.project_id,h.metadata_event_id,h.bootstrap_id],|r|r.get(0))?;
    if !proven {
        return Ok(Some("project_metadata_head_unknown"));
    }
    if metadata::authority_view(db, a, &h.project_id)?.state != "active" {
        return Ok(Some("project_metadata_authority_unresolved"));
    }
    Ok(None)
}
fn snapshot(
    db: &Connection,
    project: &str,
    id: &str,
) -> Result<(Value, Value, Vec<String>), Error> {
    let (name,goal,infinite,unit,status,created,raw):(String,Option<f64>,bool,String,String,Option<String>,String)=db.query_row("SELECT name,goal,infinite,unit,status,created_at,payload_json FROM stages WHERE id=?1 AND project_id=?2",params![id,project],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?)))?;
    let raw: Value = serde_json::from_str(&raw).map_err(|_| Error::Invalid)?;
    let o = raw.as_object().ok_or(Error::Invalid)?;
    let mut s = json!({"name":name,"goal":goal,"infinite":infinite,"unit":unit,"status":status,"created_at":created,"completed_at":null,"deadline":null,"personal_goal":0,"auto_freeze":true,"streak_enabled":true,"work_method":"manual"});
    for k in [
        "completed_at",
        "deadline",
        "personal_goal",
        "auto_freeze",
        "streak_enabled",
        "work_method",
    ] {
        if let Some(v) = o.get(k) {
            s[k] = v.clone();
        }
    }
    s = metadata::normalize_metadata_numbers(s);
    let ext:Option<String>=db.query_row("SELECT payload_json FROM project_extensions WHERE entity_type='stage' AND entity_id=?1",[id],|r|r.get(0)).optional()?;
    let mut unsupported: Vec<String> = o
        .keys()
        .filter(|k| !FIELDS.contains(&k.as_str()) && !LEGACY.contains(&k.as_str()))
        .cloned()
        .collect();
    let extension: Value = ext
        .map(|s| serde_json::from_str(&s))
        .transpose()
        .map_err(|_| Error::Invalid)?
        .unwrap_or(json!({}));
    if extension.as_object().is_none_or(|o| !o.is_empty()) {
        unsupported.push("stage_extensions".into());
    }
    if !stage_valid(&s) {
        unsupported.push("invalid_stage_portable_fields".into());
    }
    for key in ["name", "goal", "infinite", "unit", "status", "created_at"] {
        if let Some(value) = o.get(key) {
            let same = value == &s[key]
                || (value.is_number() && s[key].is_number() && value.as_f64() == s[key].as_f64());
            if !same {
                unsupported.push(format!("stage_column_payload_mismatch:{key}"));
            }
        }
    }
    if o.get("parent_project_id")
        .is_some_and(|v| !v.is_null() && v.as_str() != Some(project))
    {
        unsupported.push("stage_parent_identity_mismatch".into());
    }
    if o.get("id").is_some_and(|v| v.as_str() != Some(id)) {
        unsupported.push("stage_identity_mismatch".into());
    }
    for (key, expected) in [
        ("stages", json!([])),
        ("stages_enabled", json!(false)),
        ("combine_stage_mindmaps", json!(false)),
        ("cover_image", Value::Null),
        ("folder_id", Value::Null),
    ] {
        if o.get(key).is_some_and(|v| v != &expected) {
            unsupported.push(format!("unsupported_stage_legacy_field:{key}"));
        }
    }
    unsupported.sort();
    // Raw source includes extension contents, not just the name of a blocker.
    Ok((
        s,
        json!({"payload":raw,"extensions":extension}),
        unsupported,
    ))
}
pub(crate) fn capture_candidate(
    db: &mut Connection,
    a: &str,
    p: &str,
    id: &str,
    now: &str,
) -> Result<String, Error> {
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let result = capture_candidate_tx(&tx, a, p, id, now)?;
    tx.commit()?;
    Ok(result)
}
fn capture_candidate_tx(
    tx: &Transaction<'_>,
    a: &str,
    p: &str,
    id: &str,
    now: &str,
) -> Result<String, Error> {
    let view = metadata::authority_view(&tx, a, p)?;
    if view.state != "active" {
        return Err(Error::Conflict);
    }
    let head = view.head_event_id.ok_or(Error::Scope)?;
    let (device,boot):(String,String)=tx.query_row("SELECT s.device_id,b.bootstrap_id FROM cloud_sync_state s JOIN cloud_sync_project_bootstraps b ON b.account_id=s.account_id WHERE s.account_id=?1 AND b.project_id=?2",params![a,p],|r|Ok((r.get(0)?,r.get(1)?)))?;
    let (s, raw, unsupported) = snapshot(&tx, p, id)?;
    let prior:Option<(String,String,String,String,String)>=tx.query_row("SELECT candidate_id,snapshot_json,source_json,unsupported_json,metadata_event_id FROM cloud_sync_stage_candidates WHERE account_id=?1 AND project_id=?2 AND stage_id=?3 AND device_id=?4 ORDER BY generation DESC LIMIT 1",params![a,p,id,device],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
    let blocks = json!(unsupported).to_string();
    if let Some((cid, old, source, b, dep)) = prior {
        if old == s.to_string() && source == raw.to_string() && b == blocks && dep == head {
            return Ok(cid);
        }
    }
    let generation:i64=tx.query_row("SELECT COALESCE(MAX(generation),0)+1 FROM cloud_sync_stage_candidates WHERE account_id=?1 AND project_id=?2 AND stage_id=?3 AND device_id=?4",params![a,p,id,device],|r|r.get(0))?;
    let cid = metadata::new_event_id()?;
    tx.execute(
        "INSERT INTO cloud_sync_stage_candidates VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
        params![
            cid,
            a,
            p,
            id,
            device,
            boot,
            head,
            generation,
            s.to_string(),
            raw.to_string(),
            blocks,
            now
        ],
    )?;
    Ok(cid)
}
fn store(
    tx: &Transaction<'_>,
    a: &str,
    e: &Event,
    bytes: &[u8],
    state: &str,
    seq: Option<i64>,
) -> Result<(), Error> {
    let h = &e.header;
    tx.execute("INSERT INTO cloud_sync_structural_events(account_id,event_id,project_id,entity_type,entity_id,canonical_frame,parent_event_id,metadata_event_id,revision,generation,operation,state,server_sequence) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",params![a,h.event_id,h.project_id,h.entity_type,h.entity_id,bytes,h.parent_event_ids.first(),h.metadata_event_id,h.revision,h.generation,h.operation,state,seq])?;
    Ok(())
}
/// Explicit internal publication only. Exact expected heads avoid capturing a
/// stale local edit as a descendant of a newly received peer branch.
pub(crate) fn prepare(
    db: &mut Connection,
    a: &str,
    e: &Event,
    expected_tips: &[String],
) -> Result<(), Error> {
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    prepare_tx(&tx, a, e, expected_tips)?;
    tx.commit()?;
    Ok(())
}
fn prepare_tx(
    tx: &Transaction<'_>,
    a: &str,
    e: &Event,
    expected_tips: &[String],
) -> Result<(), Error> {
    let bytes = frame(e)?;
    let h = &e.header;
    let device: String = tx.query_row(
        "SELECT device_id FROM cloud_sync_state WHERE account_id=?1",
        [a],
        |r| r.get(0),
    )?;
    if device != h.device_id {
        return Err(Error::Scope);
    }
    let previous:Option<Vec<u8>>=tx.query_row("SELECT e.canonical_frame FROM cloud_sync_structural_events e JOIN cloud_sync_outbox o ON o.account_id=e.account_id AND o.event_id=e.event_id WHERE e.account_id=?1 AND e.event_id=?2 AND o.device_id=?3",params![a,h.event_id,h.device_id],|r|r.get(0)).optional()?;
    if let Some(previous) = previous {
        if previous != bytes {
            return Err(Error::Conflict);
        }
        return Ok(());
    }
    let pending:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_structural_events WHERE account_id=?1 AND project_id=?2 AND entity_type=?3 AND entity_id=?4 AND state IN ('unsealed','sealed'))",params![a,h.project_id,h.entity_type,h.entity_id],|r|r.get(0))?;
    if pending {
        return Err(Error::Conflict);
    }
    if dependency(&tx, a, h)?.is_some() {
        return Err(Error::Scope);
    }
    if tips(&tx, a, &h.project_id, &h.entity_type, &h.entity_id)? != expected_tips
        || h.parent_event_ids != expected_tips
    {
        return Err(Error::Conflict);
    }
    if h.entity_type == "stage" && h.operation == "create" {
        let candidate:Option<(String,String)>=tx.query_row("SELECT snapshot_json,unsupported_json FROM cloud_sync_stage_candidates WHERE account_id=?1 AND project_id=?2 AND stage_id=?3 AND device_id=?4 AND metadata_event_id=?5 ORDER BY generation DESC LIMIT 1",params![a,h.project_id,h.entity_id,h.device_id,h.metadata_event_id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        let Some((s, b)) = candidate else {
            return Err(Error::Scope);
        };
        let (current, _, current_blockers) = snapshot(&tx, &h.project_id, &h.entity_id)?;
        if b != "[]"
            || !current_blockers.is_empty()
            || serde_json::from_str::<Value>(&s).map_err(|_| Error::Invalid)? != e.stage
            || current != e.stage
        {
            return Err(Error::Conflict);
        }
    }
    let mut max_revision = 0;
    let mut max_generation = 0;
    for parent in &h.parent_event_ids {
        let (revision,generation,operation):(i64,i64,String)=tx.query_row("SELECT revision,generation,operation FROM cloud_sync_structural_events WHERE account_id=?1 AND event_id=?2 AND project_id=?3 AND entity_type=?4 AND entity_id=?5 AND state IN ('applied','conflict_preserved','tombstone_blocked')",params![a,parent,h.project_id,h.entity_type,h.entity_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        if e.version == 1 && operation == "delete" {
            return Err(Error::Invalid);
        }
        max_revision = max_revision.max(revision);
        max_generation = max_generation.max(generation);
    }
    if h.revision != max_revision + 1 || h.generation != max_generation + 1 {
        return Err(Error::Invalid);
    }
    if h.entity_type == "stage_order" && order_dependency(&tx, a, e, true)?.is_some() {
        return Err(Error::Conflict);
    }
    store(&tx, a, e, &bytes, "unsealed", None)?;
    tx.execute("INSERT INTO cloud_sync_outbox(event_id,account_id,device_id,project_id,entity_id,entity_type,operation,revision,updated_at,deleted_at,created_at,parent_event_id,local_ordinal,lifecycle) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?9,?11,(SELECT COALESCE(MAX(local_ordinal),0)+1 FROM cloud_sync_outbox WHERE account_id=?2),'unsealed')",params![h.event_id,a,h.device_id,h.project_id,h.entity_id,h.entity_type,if h.operation=="delete"{"delete"}else{"upsert"},h.revision,h.updated_at,e.deleted_at.as_str(),h.parent_event_ids.first()])?;
    Ok(())
}
pub(crate) fn seal(
    db: &mut Connection,
    a: &str,
    event: &str,
    expected_frame: &[u8],
    nonce: &[u8],
    ciphertext: &[u8],
) -> Result<(), Error> {
    if nonce.len() != 24 || ciphertext.len() < 16 || ciphertext.len() > 1024 * 1024 + 36 {
        return Err(Error::Invalid);
    }
    let compression_view =
        crate::frame_compression::canonical_view(expected_frame).map_err(|_| Error::Invalid)?;
    let expected_frame = compression_view.as_ref();
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let bytes:Vec<u8>=tx.query_row("SELECT e.canonical_frame FROM cloud_sync_structural_events e JOIN cloud_sync_outbox o ON o.account_id=e.account_id AND o.event_id=e.event_id WHERE e.account_id=?1 AND e.event_id=?2 AND o.lifecycle IN ('unsealed','sealed','accepted')",params![a,event],|r|r.get(0))?;
    if bytes != expected_frame {
        return Err(Error::Conflict);
    }
    let existing:Option<(Vec<u8>,Vec<u8>)>=tx.query_row("SELECT nonce,ciphertext FROM cloud_sync_event_objects WHERE account_id=?1 AND event_id=?2",params![a,event],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    if let Some((n, c)) = existing {
        if n != nonce || c != ciphertext {
            return Err(Error::Conflict);
        }
    } else {
        tx.execute(
            "INSERT INTO cloud_sync_event_objects VALUES(?1,?2,1,1,?3,?4,?5)",
            params![
                a,
                event,
                nonce,
                ciphertext,
                unframe(&bytes)?.header.updated_at
            ],
        )?;
        tx.execute("UPDATE cloud_sync_structural_events SET state='sealed' WHERE account_id=?1 AND event_id=?2",params![a,event])?;
        tx.execute("UPDATE cloud_sync_outbox SET lifecycle='sealed' WHERE account_id=?1 AND event_id=?2 AND lifecycle='unsealed'",params![a,event])?;
    }
    tx.commit()?;
    Ok(())
}
/// All current local stages must have authenticated live structural tips.
/// A tombstone has no complete child manifest in this slice and cannot remove
/// membership. Delete/edit conflict blocks membership; all-live edit branches prove membership.
/// A frozen order reference can remain valid after a portable Stage edit.
/// Prove live membership through authenticated causal ancestry, never sequence
/// or timestamps. Every current tip and every referenced tip must be covered.
fn reference_coverage(
    db: &Connection,
    a: &str,
    p: &str,
    id: &str,
    current: &[String],
    refs: &[String],
) -> Result<Option<bool>, Error> {
    use std::collections::{HashMap, HashSet};
    for reference in refs {
        let known:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_structural_events e JOIN cloud_sync_structural_apply_ledger l ON l.account_id=e.account_id AND l.event_id=e.event_id WHERE e.account_id=?1 AND e.project_id=?2 AND e.entity_type='stage' AND e.entity_id=?3 AND e.event_id=?4 AND e.operation!='delete' AND e.state IN ('applied','conflict_preserved'))",params![a,p,id,reference],|r|r.get(0))?;
        if !known {
            return Ok(Some(false));
        }
    }
    let mut cache: HashMap<String, Vec<String>> = HashMap::new();
    let mut covered = HashSet::new();
    for tip in current {
        let mut stack = vec![tip.clone()];
        let mut visited = HashSet::new();
        let mut matched = false;
        while let Some(event) = stack.pop() {
            if !visited.insert(event.clone()) {
                continue;
            }
            if refs.contains(&event) {
                covered.insert(event.clone());
                matched = true;
                continue;
            }
            if !cache.contains_key(&event) {
                if cache.len() >= 256 {
                    return Ok(None);
                }
                let raw:Option<Vec<u8>>=db.query_row("SELECT canonical_frame FROM cloud_sync_structural_events WHERE account_id=?1 AND project_id=?2 AND entity_type='stage' AND entity_id=?3 AND event_id=?4 AND state IN ('applied','conflict_preserved','tombstone_blocked')",params![a,p,id,event],|r|r.get(0)).optional()?;
                let Some(raw) = raw else {
                    return Ok(Some(false));
                };
                cache.insert(event.clone(), unframe(&raw)?.header.parent_event_ids);
            }
            stack.extend(cache[&event].iter().cloned());
        }
        if !matched {
            return Ok(Some(false));
        }
    }
    Ok(Some(covered.len() == refs.len()))
}
// Read-only C18 content consumer. Accepted Stage-order coverage is unchanged.
pub(crate) fn content_reference_ready(
    db: &Connection,
    a: &str,
    p: &str,
    id: &str,
    refs: &[String],
) -> Result<(), String> {
    let current = tips(db, a, p, "stage", id).map_err(|_| "stage_dependency_missing")?;
    if current.len() != 1 {
        return Err("stage_dependency_missing".into());
    }
    let raw: Vec<u8> = db.query_row("SELECT canonical_frame FROM cloud_sync_structural_events WHERE account_id=?1 AND project_id=?2 AND entity_type='stage' AND entity_id=?3 AND event_id=?4", params![a,p,id,current[0]], |r|r.get(0)).map_err(|_| "stage_dependency_missing")?;
    let event = unframe(&raw).map_err(|_| "stage_dependency_missing")?;
    if event.header.operation == "delete" {
        return Err("stage_tombstone_child_manifest_incomplete".into());
    }
    // Consume the same authority facts for this Stage only. Do not materialize
    // every Stage/Stage-order branch just to verify one bounded Note dependency.
    let (local, _, unsupported) = snapshot(db, p, id).map_err(|_| "stage_dependency_missing")?;
    let projection: Option<String> = db.query_row("SELECT head_event_id FROM cloud_sync_structural_projection WHERE account_id=?1 AND project_id=?2 AND entity_type='stage' AND entity_id=?3", params![a,p,id], |r|r.get(0)).optional().map_err(|_| "stage_dependency_missing")?;
    if !unsupported.is_empty() || projection.as_ref() != current.first() || event.stage != local {
        return Err("stage_dependency_missing".into());
    }
    match reference_coverage(db, a, p, id, &current, refs)
        .map_err(|_| "stage_dependency_missing")?
    {
        Some(true) => Ok(()),
        None => Err("stage_dependency_proof_limit".into()),
        _ => Err("stage_dependency_missing".into()),
    }
}
// Remote decisions may reconcile authenticated history, but must not adopt an
// unrelated legacy/local candidate on behalf of this device. Prove the visible
// snapshot belongs to the decision's lineage, or require a local explicit CAS.
fn known_local_in_lineage(
    db: &Connection,
    a: &str,
    e: &Event,
    local: &Value,
) -> Result<Option<bool>, Error> {
    let h = &e.header;
    let mut stack = h.parent_event_ids.clone();
    let mut visited = std::collections::HashSet::new();
    while let Some(id) = stack.pop() {
        if !visited.insert(id.clone()) {
            continue;
        }
        if visited.len() > 256 {
            return Ok(None);
        }
        let raw:Option<Vec<u8>>=db.query_row("SELECT canonical_frame FROM cloud_sync_structural_events WHERE account_id=?1 AND project_id=?2 AND entity_type=?3 AND entity_id=?4 AND event_id=?5 AND state IN ('applied','conflict_preserved','tombstone_blocked')",params![a,h.project_id,h.entity_type,h.entity_id,id],|r|r.get(0)).optional()?;
        let Some(raw) = raw else {
            return Ok(Some(false));
        };
        let prior = unframe(&raw)?;
        let value = if h.entity_type == "stage" {
            &prior.stage
        } else {
            &prior.stage_ids
        };
        if prior.header.operation != "delete" && value == local {
            return Ok(Some(true));
        }
        stack.extend(prior.header.parent_event_ids);
    }
    Ok(Some(false))
}
fn order_dependency(
    db: &Connection,
    a: &str,
    e: &Event,
    writer: bool,
) -> Result<Option<&'static str>, Error> {
    let h = &e.header;
    let ids = e.stage_ids.as_array().ok_or(Error::Invalid)?;
    for id in ids {
        let owner: Option<String> = db
            .query_row(
                "SELECT project_id FROM stages WHERE id=?1",
                [id.as_str().unwrap()],
                |r| r.get(0),
            )
            .optional()?;
        if owner.as_deref().is_some_and(|p| p != h.project_id) {
            return Ok(Some("stage_order_foreign_stage"));
        }
        let ts = tips(db, a, &h.project_id, "stage", id.as_str().unwrap())?;
        if ts.is_empty() {
            return Ok(Some("stage_dependency_missing"));
        }
        for tip in &ts {
            let op:String=db.query_row("SELECT operation FROM cloud_sync_structural_events WHERE account_id=?1 AND event_id=?2",params![a,tip],|r|r.get(0))?;
            if op == "delete" {
                return Ok(Some("stage_tombstone_child_manifest_incomplete"));
            }
        }
        if e.stage_heads[id.as_str().unwrap()] != json!(ts) {
            if writer {
                return Ok(Some("stage_membership_head_changed"));
            }
            let refs: Vec<String> =
                serde_json::from_value(e.stage_heads[id.as_str().unwrap()].clone())
                    .map_err(|_| Error::Invalid)?;
            match reference_coverage(db, a, &h.project_id, id.as_str().unwrap(), &ts, &refs)? {
                Some(true) => {}
                Some(false) => return Ok(Some("stage_membership_head_changed")),
                None => return Ok(Some("stage_dependency_proof_limit")),
            }
        }
    }
    let mut q = db.prepare("SELECT id FROM stages WHERE project_id=?1 ORDER BY id")?;
    let local: Vec<String> = q
        .query_map([&h.project_id], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    let mut members: Vec<String> = ids
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    members.sort();
    if local != members {
        return Ok(Some("stage_order_membership_mismatch"));
    }
    Ok(None)
}
fn write_stage(tx: &Transaction<'_>, e: &Event) -> Result<(), Error> {
    let h = &e.header;
    let s = &e.stage;
    let existing: Option<String> = tx
        .query_row(
            "SELECT payload_json FROM stages WHERE id=?1 AND project_id=?2",
            params![h.entity_id, h.project_id],
            |r| r.get(0),
        )
        .optional()?;
    let is_new = existing.is_none();
    let mut payload: Value = existing
        .map(|s| serde_json::from_str(&s))
        .transpose()
        .map_err(|_| Error::Invalid)?
        .unwrap_or(json!({"id":h.entity_id}));
    let o = payload.as_object_mut().ok_or(Error::Invalid)?;
    if is_new {
        // A newly imported Stage has no local Progress history. Initialize the
        // empty local read model so its separate Progress genesis can attach.
        // Existing totals/history remain owned by Progress and are preserved.
        o.insert("total".into(), json!(0));
        o.insert("progress_entries".into(), json!([]));
    }
    for k in FIELDS {
        o.insert(k.to_string(), s[*k].clone());
    }
    // D, not an independent portable authority.
    let parent_name: String = tx.query_row(
        "SELECT name FROM projects WHERE id=?1",
        [&h.project_id],
        |r| r.get(0),
    )?;
    o.insert("parent_project_name".into(), json!(parent_name));
    tx.execute("INSERT INTO stages(id,project_id,name,goal,infinite,unit,status,created_at,updated_at,payload_json) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10) ON CONFLICT(id) DO UPDATE SET name=excluded.name,goal=excluded.goal,infinite=excluded.infinite,unit=excluded.unit,status=excluded.status,created_at=excluded.created_at,updated_at=excluded.updated_at,payload_json=excluded.payload_json",params![h.entity_id,h.project_id,s["name"].as_str(),s["goal"].as_f64(),s["infinite"].as_bool(),s["unit"].as_str(),s["status"].as_str(),s["created_at"].as_str(),h.updated_at,payload.to_string()])?;
    if is_new {
        // Compatibility placement only, never an authenticated order authority.
        // A received Stage-order permutation replaces it in its own transaction.
        tx.execute("INSERT INTO stage_order(stage_id,project_id,position) VALUES(?1,?2,(SELECT COALESCE(MAX(position),-1)+1 FROM stage_order WHERE project_id=?2))",params![h.entity_id,h.project_id])?;
    }
    Ok(())
}
fn block(tx: Transaction<'_>, a: &str, e: &Event, reason: &str) -> Result<&'static str, Error> {
    tx.execute("UPDATE cloud_sync_structural_events SET state='orphan',blocker=?1,retry_ordinal=(SELECT COALESCE(MAX(retry_ordinal),0)+1 FROM cloud_sync_structural_events WHERE account_id=?2) WHERE account_id=?2 AND event_id=?3",params![reason,a,e.header.event_id])?;
    tx.execute("UPDATE cloud_sync_inbox SET state='orphan',error_code=?1 WHERE account_id=?2 AND event_id=?3",params![reason,a,e.header.event_id])?;
    tx.commit()?;
    Ok("orphan")
}
/// expected object is mandatory: binds native verified plaintext to the exact
/// durable inbox object used by the trusted authenticated codec caller.
pub(crate) fn apply(
    db: &mut Connection,
    a: &str,
    bytes: &[u8],
    nonce: &[u8],
    ciphertext: &[u8],
) -> Result<&'static str, Error> {
    let compression_view =
        crate::frame_compression::canonical_view(bytes).map_err(|_| Error::Invalid)?;
    let bytes = compression_view.as_ref();
    let e = unframe(bytes)?;
    let h = &e.header;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let user: Option<String> = tx
        .query_row(
            "SELECT canonical_user_id FROM cloud_account_bindings WHERE local_account_id=?1",
            [a],
            |r| r.get(0),
        )
        .optional()?;
    if user.as_deref() != Some(&h.account_id) {
        return Err(Error::Scope);
    }
    let object:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_event_objects WHERE account_id=?1 AND event_id=?2 AND crypto_version=1 AND aad_version=1 AND nonce=?3 AND ciphertext=?4)",params![a,h.event_id,nonce,ciphertext],|r|r.get(0))?;
    if !object {
        return Err(Error::Scope);
    }
    let inbox:Option<(i64,String)>=tx.query_row("SELECT server_sequence,state FROM cloud_sync_inbox WHERE account_id=?1 AND event_id=?2 AND project_id=?3 AND entity_id=?4 AND entity_type=?5 AND device_id=?6 AND sync_revision=?7 AND operation=?8 AND updated_at=?9 AND deleted_at IS ?10",params![a,h.event_id,h.project_id,h.entity_id,h.entity_type,h.device_id,h.revision,if h.operation=="delete"{"delete"}else{"upsert"},h.updated_at,e.deleted_at.as_str()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    let Some((seq, _)) = inbox else {
        return Err(Error::Scope);
    };
    let existing:Option<(Vec<u8>,String)>=tx.query_row("SELECT canonical_frame,state FROM cloud_sync_structural_events WHERE account_id=?1 AND event_id=?2",params![a,h.event_id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    if let Some((old, _)) = &existing {
        if old != bytes {
            return Err(Error::Conflict);
        }
    }
    let own_event:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_outbox o JOIN cloud_sync_state s ON s.account_id=o.account_id WHERE o.account_id=?1 AND o.event_id=?2 AND o.device_id=?3 AND s.device_id=o.device_id AND o.lifecycle IN ('sealed','accepted') AND o.entity_type=?4 AND o.entity_id=?5 AND o.project_id=?6)",params![a,h.event_id,h.device_id,h.entity_type,h.entity_id,h.project_id],|r|r.get(0))?;
    if let Some(outcome)=tx.query_row("SELECT outcome FROM cloud_sync_structural_apply_ledger WHERE account_id=?1 AND event_id=?2 AND nonce=?3 AND ciphertext=?4 AND server_sequence=?5",params![a,h.event_id,nonce,ciphertext,seq],|r|r.get::<_,String>(0)).optional()?{
        return Ok(if outcome=="applied"{"applied"}else{"conflict_preserved"})
    }
    if existing.is_none() {
        store(&tx, a, &e, bytes, "orphan", Some(seq))?;
    } else {
        tx.execute("UPDATE cloud_sync_structural_events SET server_sequence=?1 WHERE account_id=?2 AND event_id=?3",params![seq,a,h.event_id])?;
    }
    if let Some(reason) = dependency(&tx, a, h)? {
        return block(tx, a, &e, reason);
    }
    // A blocked tombstone has already established its causal tip. Retry must
    // never resurrect it after a later explicit resolution consumed that tip.
    if existing
        .as_ref()
        .is_some_and(|(_, state)| state == "tombstone_blocked")
    {
        tx.commit()?;
        return Ok("tombstone_blocked");
    }
    let mut max_revision = 0;
    let mut max_generation = 0;
    for parent in &h.parent_event_ids {
        let prior:Option<(i64,i64,String)>=tx.query_row("SELECT revision,generation,operation FROM cloud_sync_structural_events WHERE account_id=?1 AND event_id=?2 AND project_id=?3 AND entity_type=?4 AND entity_id=?5 AND state IN ('applied','conflict_preserved','tombstone_blocked')",params![a,parent,h.project_id,h.entity_type,h.entity_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        let Some((r, g, op)) = prior else {
            return block(tx, a, &e, "structural_parent_unknown");
        };
        if e.version == 1 && op == "delete" {
            return block(tx, a, &e, "structural_parent_revision_invalid");
        }
        max_revision = max_revision.max(r);
        max_generation = max_generation.max(g);
    }
    if h.revision != max_revision + 1 || h.generation != max_generation + 1 {
        return block(tx, a, &e, "structural_parent_revision_invalid");
    }
    if h.entity_type == "stage_order" {
        if let Some(reason) = order_dependency(&tx, a, &e, false)? {
            return block(tx, a, &e, reason);
        }
    }
    if h.entity_type == "stage" {
        let owner: Option<String> = tx
            .query_row(
                "SELECT project_id FROM stages WHERE id=?1",
                [&h.entity_id],
                |r| r.get(0),
            )
            .optional()?;
        if owner.as_deref().is_some_and(|p| p != h.project_id) {
            return block(tx, a, &e, "stage_scope_collision");
        }
    }
    if h.operation == "delete" && h.parent_event_ids.is_empty() {
        return Err(Error::Invalid);
    }
    let old_tips = tips(&tx, a, &h.project_id, &h.entity_type, &h.entity_id)?;
    let mut conflict = old_tips != h.parent_event_ids;
    if e.version == 2 {
        let expected:Option<String>=tx.query_row("SELECT expected_local_json FROM cloud_sync_structural_decisions WHERE account_id=?1 AND event_id=?2",params![a,h.event_id],|r|r.get(0)).optional()?;
        if let Some(expected) = expected {
            let local = if h.entity_type == "stage" {
                snapshot(&tx, &h.project_id, &h.entity_id)?.0
            } else {
                json!(stage_ids(&tx, &h.project_id)?)
            };
            conflict |=
                local != serde_json::from_str::<Value>(&expected).map_err(|_| Error::Invalid)?;
        } else {
            let local = if h.entity_type == "stage" {
                let (local, _, unsupported) = snapshot(&tx, &h.project_id, &h.entity_id)?;
                conflict |= !unsupported.is_empty();
                local
            } else {
                json!(stage_ids(&tx, &h.project_id)?)
            };
            match known_local_in_lineage(&tx, a, &e, &local)? {
                Some(known) => conflict |= !known,
                None => return block(tx, a, &e, "structural_local_lineage_limit"),
            }
        }
    }
    // A distinct existing legacy value is never overwritten by remote genesis.
    // Preserve its exact raw source as a durable candidate before ACKing conflict.
    if h.entity_type == "stage" && h.operation == "create" && old_tips.is_empty() {
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM stages WHERE id=?1)",
            [&h.entity_id],
            |r| r.get(0),
        )?;
        if exists {
            let (local, raw, unsupported) = snapshot(&tx, &h.project_id, &h.entity_id)?;
            let self_echo = own_event;
            if !self_echo || local != e.stage || !unsupported.is_empty() {
                conflict = true;
                let device: String = tx.query_row(
                    "SELECT device_id FROM cloud_sync_state WHERE account_id=?1",
                    [a],
                    |r| r.get(0),
                )?;
                let generation:i64=tx.query_row("SELECT COALESCE(MAX(generation),0)+1 FROM cloud_sync_stage_candidates WHERE account_id=?1 AND project_id=?2 AND stage_id=?3 AND device_id=?4",params![a,h.project_id,h.entity_id,device],|r|r.get(0))?;
                tx.execute("INSERT INTO cloud_sync_stage_candidates VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",params![metadata::new_event_id()?,a,h.project_id,h.entity_id,device,h.bootstrap_id,h.metadata_event_id,generation,local.to_string(),raw.to_string(),json!(unsupported).to_string(),h.updated_at])?;
            }
        }
    }
    if h.entity_type == "stage" && h.operation == "update" && e.version == 1 {
        let prior_frame:Option<Vec<u8>>=tx.query_row("SELECT e.canonical_frame FROM cloud_sync_structural_projection p JOIN cloud_sync_structural_events e ON e.account_id=p.account_id AND e.event_id=p.head_event_id WHERE p.account_id=?1 AND p.project_id=?2 AND p.entity_type='stage' AND p.entity_id=?3",params![a,h.project_id,h.entity_id],|r|r.get(0)).optional()?;
        if let Some(prior) = prior_frame {
            let (local, raw, unsupported) = snapshot(&tx, &h.project_id, &h.entity_id)?;
            if local != unframe(&prior)?.stage || !unsupported.is_empty() {
                conflict = true;
                let device: String = tx.query_row(
                    "SELECT device_id FROM cloud_sync_state WHERE account_id=?1",
                    [a],
                    |r| r.get(0),
                )?;
                let generation:i64=tx.query_row("SELECT COALESCE(MAX(generation),0)+1 FROM cloud_sync_stage_candidates WHERE account_id=?1 AND project_id=?2 AND stage_id=?3 AND device_id=?4",params![a,h.project_id,h.entity_id,device],|r|r.get(0))?;
                tx.execute("INSERT INTO cloud_sync_stage_candidates VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",params![metadata::new_event_id()?,a,h.project_id,h.entity_id,device,h.bootstrap_id,h.metadata_event_id,generation,local.to_string(),raw.to_string(),json!(unsupported).to_string(),h.updated_at])?;
            }
        }
    }
    if h.entity_type == "stage_order" && h.operation == "create" && old_tips.is_empty() {
        let legacy:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_stage_candidates c WHERE c.account_id=?1 AND c.project_id=?2 AND NOT EXISTS(SELECT 1 FROM cloud_sync_structural_projection p WHERE p.account_id=c.account_id AND p.project_id=c.project_id AND p.entity_type='stage' AND p.entity_id=c.stage_id))",params![a,h.project_id],|r|r.get(0))?;
        if legacy && !own_event {
            conflict = true;
            tx.execute(
                "INSERT OR IGNORE INTO cloud_sync_structural_local_orders VALUES(?1,?2,?3,?4)",
                params![
                    a,
                    h.project_id,
                    h.event_id,
                    json!(stage_ids(&tx, &h.project_id)?).to_string()
                ],
            )?;
        }
    }
    let pending:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_structural_events WHERE account_id=?1 AND project_id=?2 AND entity_type=?3 AND entity_id=?4 AND event_id!=?5 AND state IN ('unsealed','sealed'))",params![a,h.project_id,h.entity_type,h.entity_id,h.event_id],|r|r.get(0))?;
    conflict |= pending;
    tx.execute(
        "INSERT OR IGNORE INTO cloud_sync_structural_tips VALUES(?1,?2,?3,?4,?5)",
        params![a, h.project_id, h.entity_type, h.entity_id, h.event_id],
    )?;
    for parent in &h.parent_event_ids {
        tx.execute("DELETE FROM cloud_sync_structural_tips WHERE account_id=?1 AND project_id=?2 AND entity_type=?3 AND entity_id=?4 AND event_id=?5",params![a,h.project_id,h.entity_type,h.entity_id,parent])?;
    }
    if conflict || h.operation == "delete" {
        tx.execute("DELETE FROM cloud_sync_structural_projection WHERE account_id=?1 AND project_id=?2 AND entity_type=?3 AND entity_id=?4",params![a,h.project_id,h.entity_type,h.entity_id])?;
    }
    if h.operation == "delete" {
        tx.execute("UPDATE cloud_sync_structural_events SET state='tombstone_blocked',blocker='stage_tombstone_child_manifest_incomplete' WHERE account_id=?1 AND event_id=?2",params![a,h.event_id])?;
        tx.execute("UPDATE cloud_sync_inbox SET state='orphan',error_code='stage_tombstone_child_manifest_incomplete' WHERE account_id=?1 AND event_id=?2",params![a,h.event_id])?;
        tx.commit()?;
        return Ok("tombstone_blocked");
    }
    let outcome = if conflict {
        "conflict_preserved"
    } else {
        "applied"
    };
    if !conflict {
        if h.entity_type == "stage" {
            write_stage(&tx, &e)?;
        } else {
            tx.execute(
                "DELETE FROM stage_order WHERE project_id=?1",
                [&h.project_id],
            )?;
            for (pos, id) in e.stage_ids.as_array().unwrap().iter().enumerate() {
                tx.execute(
                    "INSERT INTO stage_order VALUES(?1,?2,?3)",
                    params![id.as_str(), h.project_id, pos as i64],
                )?;
            }
        }
        tx.execute("INSERT INTO cloud_sync_structural_projection VALUES(?1,?2,?3,?4,?5) ON CONFLICT(account_id,project_id,entity_type,entity_id) DO UPDATE SET head_event_id=excluded.head_event_id",params![a,h.project_id,h.entity_type,h.entity_id,h.event_id])?;
    }
    tx.execute("UPDATE cloud_sync_structural_events SET state=?1,blocker=NULL WHERE account_id=?2 AND event_id=?3",params![outcome,a,h.event_id])?;
    if conflict && e.version == 2 {
        tx.execute("UPDATE cloud_sync_structural_events SET blocker='stale_structural_resolution' WHERE account_id=?1 AND event_id=?2",params![a,h.event_id])?;
    }
    tx.execute(
        "INSERT INTO cloud_sync_structural_apply_ledger VALUES(?1,?2,?3,?4,?5,?6)",
        params![a, h.event_id, outcome, seq, nonce, ciphertext],
    )?;
    tx.execute("UPDATE cloud_sync_inbox SET state=?1,applied_at=?2,error_code=NULL WHERE account_id=?3 AND event_id=?4",params![if conflict{"conflict"}else{"applied"},h.updated_at,a,h.event_id])?;
    if own_event {
        tx.execute(
            "INSERT OR IGNORE INTO cloud_sync_upload_receipts VALUES(?1,?2,?3,?4,0,?5)",
            params![a, h.event_id, h.device_id, seq, h.updated_at],
        )?;
        tx.execute("UPDATE cloud_sync_outbox SET lifecycle='accepted' WHERE account_id=?1 AND event_id=?2 AND lifecycle='sealed'",params![a,h.event_id])?;
    }
    tx.commit()?;
    Ok(outcome)
}
/// Entry point for an authenticated caller, including failed codec/frame opens.
/// Rejecting a malformed frame retains inbox/ciphertext and never creates a head.
pub(crate) fn apply_received(
    db: &mut Connection,
    a: &str,
    event_id: &str,
    bytes: &[u8],
    nonce: &[u8],
    ciphertext: &[u8],
) -> Result<&'static str, Error> {
    match unframe(bytes) {
        Ok(e) if e.header.event_id == event_id => apply(db, a, bytes, nonce, ciphertext),
        Ok(_) => Err(Error::Scope),
        Err(_) => {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let changed=tx.execute("UPDATE cloud_sync_inbox SET state='orphan',error_code='invalid_stage_frame' WHERE account_id=?1 AND event_id=?2 AND entity_type IN ('stage','stage_order') AND state IN ('received','orphan') AND EXISTS(SELECT 1 FROM cloud_sync_event_objects o WHERE o.account_id=?1 AND o.event_id=?2 AND o.crypto_version=1 AND o.aad_version=1 AND o.nonce=?3 AND o.ciphertext=?4)",params![a,event_id,nonce,ciphertext])?;
            if changed != 1 {
                return Err(Error::Scope);
            }
            tx.commit()?;
            Ok("invalid_stage_frame")
        }
    }
}

/// Fair bounded replay of already authenticated durable orphans; missing
/// dependencies rotate to the back. New inbox events are handled independently.
pub(crate) fn retry(db: &mut Connection, a: &str, limit: usize) -> Result<Vec<String>, Error> {
    if !(1..=32).contains(&limit) {
        return Err(Error::Invalid);
    }
    let rows = {
        let mut q=db.prepare("SELECT e.canonical_frame,o.nonce,o.ciphertext FROM cloud_sync_structural_events e JOIN cloud_sync_event_objects o ON o.account_id=e.account_id AND o.event_id=e.event_id WHERE e.account_id=?1 AND e.state='orphan' ORDER BY e.retry_ordinal,e.event_id LIMIT ?2")?;
        let rows = q
            .query_map(params![a, limit as i64], |r| {
                Ok((
                    r.get::<_, Vec<u8>>(0)?,
                    r.get::<_, Vec<u8>>(1)?,
                    r.get::<_, Vec<u8>>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows
    };
    rows.iter()
        .map(|(b, n, c)| apply(db, a, b, n, c).map(str::to_string))
        .collect()
}

// C18.4.02 production integration. Only begin() creates a migration snapshot;
// inbound readers and ordinary cycles never discover/capture legacy structure.
pub(crate) fn event_now(db: &Connection) -> Result<String, Error> {
    Ok(db.query_row(
        "SELECT strftime('%Y-%m-%dT%H:%M:%f','now') || '000Z'",
        [],
        |r| r.get(0),
    )?)
}
pub(crate) fn portable_from_payload(raw: &Value) -> Result<Value, Error> {
    let mut out = serde_json::Map::new();
    for field in FIELDS {
        out.insert(
            (*field).into(),
            raw.get(*field).cloned().unwrap_or(Value::Null),
        );
    }
    let value = metadata::normalize_metadata_numbers(Value::Object(out));
    Ok(value)
}
fn stage_ids(db: &Connection, p: &str) -> Result<Vec<String>, Error> {
    let mut q =
        db.prepare("SELECT stage_id FROM stage_order WHERE project_id=?1 ORDER BY position")?;
    let ids = q
        .query_map([p], |r| r.get(0))?
        .collect::<Result<Vec<String>, _>>()?;
    let count: i64 = db.query_row(
        "SELECT count(*) FROM stages WHERE project_id=?1",
        [p],
        |r| r.get(0),
    )?;
    if ids.len() as i64 != count {
        return Err(Error::Conflict);
    }
    Ok(ids)
}
fn header(
    db: &Connection,
    a: &str,
    p: &str,
    t: &str,
    id: &str,
    now: &str,
) -> Result<Header, Error> {
    let view = metadata::authority_view(db, a, p)?;
    if view.state != "active" {
        return Err(Error::Conflict);
    }
    let (user,device,boot):(String,String,String)=db.query_row("SELECT u.canonical_user_id,s.device_id,b.bootstrap_id FROM cloud_account_bindings u JOIN cloud_sync_state s ON s.account_id=u.local_account_id JOIN cloud_sync_project_bootstraps b ON b.account_id=s.account_id WHERE s.account_id=?1 AND b.project_id=?2",params![a,p],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    Ok(Header {
        account_id: user,
        project_id: p.into(),
        bootstrap_id: boot,
        device_id: device,
        entity_id: id.into(),
        entity_type: t.into(),
        event_id: metadata::new_event_id()?,
        operation: "create".into(),
        revision: 1,
        generation: 1,
        parent_event_ids: vec![],
        updated_at: now.into(),
        metadata_event_id: view.head_event_id.ok_or(Error::Scope)?,
    })
}
fn empty_event(header: Header) -> Event {
    Event {
        version: 1,
        header,
        stage: Value::Null,
        stage_ids: Value::Null,
        stage_heads: Value::Null,
        deleted_at: Value::Null,
    }
}
fn heads(db: &Connection, a: &str, p: &str, ids: &[String]) -> Result<Value, Error> {
    let mut out = serde_json::Map::new();
    for id in ids {
        out.insert(id.clone(), json!(tips(db, a, p, "stage", id)?));
    }
    Ok(Value::Object(out))
}
fn descend(
    db: &Connection,
    a: &str,
    e: &mut Event,
    parents: &[String],
    resolution: bool,
) -> Result<(), Error> {
    e.header.parent_event_ids = parents.to_vec();
    if !parents.is_empty() {
        e.header.operation = "update".into();
        let mut revision = 0;
        let mut generation = 0;
        for id in parents {
            let (r,g):(i64,i64)=db.query_row("SELECT revision,generation FROM cloud_sync_structural_events WHERE account_id=?1 AND event_id=?2",params![a,id],|r|Ok((r.get(0)?,r.get(1)?)))?;
            revision = revision.max(r);
            generation = generation.max(g);
        }
        e.header.revision = revision + 1;
        e.header.generation = generation + 1;
    }
    if resolution {
        e.version = 2;
    }
    Ok(())
}
#[derive(Serialize)]
pub(crate) struct StructuralView {
    pub state: String,
    pub migration_id: Option<String>,
    pub blockers: Vec<String>,
    pub entities: Vec<Value>,
    pub order: Vec<String>,
}
pub(crate) fn authority(db: &Connection, a: &str, p: &str) -> Result<StructuralView, Error> {
    let mut blockers = vec![];
    if metadata::authority_view(db, a, p)?.state != "active" {
        blockers.push("project_metadata_authority_unresolved".into());
    }
    let ids = stage_ids(db, p)?;
    let manifest:Option<(String,String)>=db.query_row("SELECT migration_id,manifest_json FROM cloud_sync_structural_migrations WHERE account_id=?1 AND project_id=?2 ORDER BY generation DESC LIMIT 1",params![a,p],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    let mut entities = vec![];
    let mut conflict = false;
    let mut all_active = true;
    for (t, id) in ids
        .iter()
        .map(|id| ("stage", id.as_str()))
        .chain(std::iter::once(("stage_order", "stage_order")))
    {
        let current = tips(db, a, p, t, id)?;
        let mut branches = vec![];
        for tip in &current {
            let raw:Vec<u8>=db.query_row("SELECT canonical_frame FROM cloud_sync_structural_events WHERE account_id=?1 AND event_id=?2",params![a,tip],|r|r.get(0))?;
            branches.push(serde_json::to_value(unframe(&raw)?).map_err(|_| Error::Invalid)?);
        }
        let local = if t == "stage" {
            let (local, _, unsupported) = snapshot(db, p, id)?;
            blockers.extend(
                unsupported
                    .into_iter()
                    .map(|s| format!("unsupported_stage_source:{id}:{s}")),
            );
            local
        } else {
            json!(ids)
        };
        let projection:Option<String>=db.query_row("SELECT head_event_id FROM cloud_sync_structural_projection WHERE account_id=?1 AND project_id=?2 AND entity_type=?3 AND entity_id=?4",params![a,p,t,id],|r|r.get(0)).optional()?;
        let matches = projection
            .as_ref()
            .is_some_and(|h| current.len() == 1 && &current[0] == h)
            && branches.first().is_some_and(|b| {
                if t == "stage" {
                    b["stage"] == local
                } else {
                    b["stage_ids"] == local
                }
            });
        let causal_tombstone_selected = current.len() == 1
            && branches
                .first()
                .is_some_and(|b| b["header"]["operation"] == "delete");
        let entity_conflict =
            current.len() > 1 || (!current.is_empty() && !matches && !causal_tombstone_selected);
        conflict |= entity_conflict;
        all_active &= matches;
        entities.push(json!({"entity_type":t,"entity_id":id,"tips":current,"branches":branches,"local":local,"conflict":entity_conflict,"causal_tombstone_selected":causal_tombstone_selected}));
    }
    let mut q=db.prepare("SELECT DISTINCT blocker FROM cloud_sync_structural_events WHERE account_id=?1 AND project_id=?2 AND blocker IS NOT NULL AND (state IN ('orphan','tombstone_blocked') OR EXISTS(SELECT 1 FROM cloud_sync_structural_tips t WHERE t.account_id=cloud_sync_structural_events.account_id AND t.event_id=cloud_sync_structural_events.event_id))")?;
    blockers.extend(
        q.query_map(params![a, p], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?,
    );
    let mut q=db.prepare("SELECT DISTINCT error_code FROM cloud_sync_inbox WHERE account_id=?1 AND project_id=?2 AND entity_type IN ('stage','stage_order') AND state IN ('received','orphan') AND error_code IS NOT NULL")?;
    blockers.extend(
        q.query_map(params![a, p], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?,
    );
    let pending:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_structural_events WHERE account_id=?1 AND project_id=?2 AND state IN ('unsealed','sealed'))",params![a,p],|r|r.get(0))?;
    let sealed:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_structural_events WHERE account_id=?1 AND project_id=?2 AND state='sealed')",params![a,p],|r|r.get(0))?;
    if conflict {
        blockers.push("unresolved_structural_conflict".into());
    }
    blockers.sort();
    blockers.dedup();
    let state = if blockers
        .iter()
        .any(|b| b == "project_metadata_authority_unresolved")
    {
        "blocked"
    } else if pending {
        if sealed {
            "published_self_echo_pending"
        } else {
            "publication_pending"
        }
    } else if conflict {
        "conflict"
    } else if !blockers.is_empty() {
        "blocked"
    } else if all_active {
        "active"
    } else if manifest.is_some() {
        "candidate_captured"
    } else {
        "structural_local"
    };
    if manifest.is_some() {
        db.execute("UPDATE cloud_sync_structural_migrations SET state=?1,blockers_json=?2 WHERE account_id=?3 AND migration_id=?4",params![state,json!(blockers).to_string(),a,manifest.as_ref().unwrap().0])?;
    }
    Ok(StructuralView {
        state: state.into(),
        migration_id: manifest.map(|r| r.0),
        blockers,
        entities,
        order: ids,
    })
}
pub(crate) fn begin(
    db: &mut Connection,
    a: &str,
    p: &str,
    now: &str,
) -> Result<StructuralView, Error> {
    if !metadata::timestamp(now) {
        return Err(Error::Invalid);
    }
    let view = authority(db, a, p)?;
    if view.state == "active" {
        return Ok(view);
    }
    if view.migration_id.is_some() {
        let prepared:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_structural_events WHERE account_id=?1 AND project_id=?2)",params![a,p],|r|r.get(0))?;
        if prepared || !view.blockers.is_empty() {
            return Ok(view);
        }
        // Explicit recapture after unsupported source repair; retain all older
        // frozen snapshots and allocate a new generation, never alter a seal.
    }
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let ids = stage_ids(&tx, p)?;
    if ids.len() > 32 {
        return Err(Error::Invalid);
    }
    let h = header(&tx, a, p, "stage_order", "stage_order", now)?;
    // Never publish a second genesis over existing authenticated structure.
    if !tips(&tx, a, p, "stage_order", "stage_order")?.is_empty() {
        return Err(Error::Conflict);
    }
    let mut items = vec![];
    let mut blockers = vec![];
    let mut events = vec![];
    for id in &ids {
        if !tips(&tx, a, p, "stage", id)?.is_empty() {
            return Err(Error::Conflict);
        }
        let cid = capture_candidate_tx(&tx, a, p, id, now)?;
        let (portable, raw, unsupported) = snapshot(&tx, p, id)?;
        blockers.extend(
            unsupported
                .into_iter()
                .map(|s| format!("unsupported_stage_source:{id}:{s}")),
        );
        let mut e = empty_event(header(&tx, a, p, "stage", id, now)?);
        e.stage = portable;
        items.push(
            json!({"stage_id":id,"candidate_id":cid,"event_id":e.header.event_id,"source":raw}),
        );
        events.push(e);
    }
    let manifest = json!({"items":items,"stage_ids":ids,"order_event_id":h.event_id,"order_header":h,"metadata_event_id":h.metadata_event_id});
    tx.execute("INSERT INTO cloud_sync_structural_migrations(account_id,project_id,migration_id,generation,manifest_json,state,blockers_json,created_at) VALUES(?1,?2,?3,(SELECT COALESCE(MAX(generation),0)+1 FROM cloud_sync_structural_migrations WHERE account_id=?1 AND project_id=?2),?4,?5,?6,?7)",params![a,p,metadata::new_event_id()?,manifest.to_string(),if blockers.is_empty(){"candidate_captured"}else{"blocked"},json!(blockers).to_string(),now])?;
    if blockers.is_empty() {
        for e in &events {
            prepare_tx(&tx, a, e, &[])?;
        }
    }
    tx.commit()?;
    advance(db, a, p)?;
    authority(db, a, p)
}
/// May only complete a previously explicit frozen migration. Dependencies must
/// have exact applied projections/self echoes before the order is prepared.
pub(crate) fn advance(db: &mut Connection, a: &str, p: &str) -> Result<(), Error> {
    let intents = {
        let mut q=db.prepare("SELECT i.order_event_json FROM cloud_sync_structural_order_intents i JOIN cloud_sync_structural_apply_ledger l ON l.account_id=i.account_id AND l.event_id=i.stage_event_id WHERE i.account_id=?1 AND i.project_id=?2 AND l.outcome='applied' LIMIT 8")?;
        let rows = q
            .query_map(params![a, p], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        rows
    };
    for raw in intents {
        let e: Event = serde_json::from_str(&raw).map_err(|_| Error::Invalid)?;
        let exists:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_structural_events WHERE account_id=?1 AND event_id=?2)",params![a,e.header.event_id],|r|r.get(0))?;
        if !exists {
            // A later explicit full-tip order decision can reconcile a companion
            // whose frozen Stage references became stale. Retain its immutable
            // intent, but never republish it over the proven current order.
            if authority(db, a, p)?.state != "active" {
                prepare(db, a, &e, &e.header.parent_event_ids)?;
            }
        }
    }

    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let raw:Option<String>=tx.query_row("SELECT manifest_json FROM cloud_sync_structural_migrations WHERE account_id=?1 AND project_id=?2 ORDER BY generation DESC LIMIT 1",params![a,p],|r|r.get(0)).optional()?;
    let Some(raw) = raw else {
        return Ok(());
    };
    let m: Value = serde_json::from_str(&raw).map_err(|_| Error::Invalid)?;
    let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_structural_events WHERE account_id=?1 AND event_id=?2)",params![a,m["order_event_id"].as_str()],|r|r.get(0))?;
    if exists {
        return Ok(());
    }
    for item in m["items"].as_array().ok_or(Error::Invalid)? {
        let proven:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_structural_projection p JOIN cloud_sync_structural_apply_ledger l ON l.account_id=p.account_id AND l.event_id=p.head_event_id JOIN cloud_sync_structural_apply_ledger genesis ON genesis.account_id=p.account_id AND genesis.event_id=?4 WHERE p.account_id=?1 AND p.project_id=?2 AND p.entity_type='stage' AND p.entity_id=?3 AND l.outcome='applied')",params![a,p,item["stage_id"].as_str(),item["event_id"].as_str()],|r|r.get(0))?;
        if !proven {
            return Ok(());
        }
        let id = item["stage_id"].as_str().ok_or(Error::Invalid)?;
        let raw:Vec<u8>=tx.query_row("SELECT e.canonical_frame FROM cloud_sync_structural_projection p JOIN cloud_sync_structural_events e ON e.account_id=p.account_id AND e.event_id=p.head_event_id WHERE p.account_id=?1 AND p.project_id=?2 AND p.entity_type='stage' AND p.entity_id=?3",params![a,p,id],|r|r.get(0))?;
        let (local, _, unsupported) = snapshot(&tx, p, id)?;
        if local != unframe(&raw)?.stage || !unsupported.is_empty() {
            return Ok(());
        }
    }
    let ids: Vec<String> =
        serde_json::from_value(m["stage_ids"].clone()).map_err(|_| Error::Invalid)?;
    if ids != stage_ids(&tx, p)? {
        return Err(Error::Conflict);
    }
    let mut e =
        empty_event(serde_json::from_value(m["order_header"].clone()).map_err(|_| Error::Invalid)?);
    e.stage_ids = json!(ids);
    e.stage_heads = heads(&tx, a, p, &ids)?;
    prepare_tx(&tx, a, &e, &[])?;
    tx.commit()?;
    Ok(())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Decision {
    pub entity_type: String,
    pub entity_id: String,
    pub expected_tips: Vec<String>,
    pub expected_local: Value,
    pub proposed: Value,
    pub selected_event_id: Option<String>,
}
pub(crate) fn decide(
    db: &mut Connection,
    a: &str,
    p: &str,
    d: &Decision,
    now: &str,
) -> Result<String, Error> {
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if !matches!(d.entity_type.as_str(), "stage" | "stage_order") {
        return Err(Error::Invalid);
    }
    let current = tips(&tx, a, p, &d.entity_type, &d.entity_id)?;
    if current.is_empty() || current != d.expected_tips {
        return Err(Error::Conflict);
    }
    let local = if d.entity_type == "stage" {
        snapshot(&tx, p, &d.entity_id)?.0
    } else {
        json!(stage_ids(&tx, p)?)
    };
    if local != d.expected_local {
        return Err(Error::Conflict);
    }
    let mut e = empty_event(header(&tx, a, p, &d.entity_type, &d.entity_id, now)?);
    descend(&tx, a, &mut e, &current, true)?;
    if let Some(id) = &d.selected_event_id {
        if !current.contains(id) {
            return Err(Error::Conflict);
        }
        let raw:Vec<u8>=tx.query_row("SELECT canonical_frame FROM cloud_sync_structural_events WHERE account_id=?1 AND event_id=?2",params![a,id],|r|r.get(0))?;
        let selected = unframe(&raw)?;
        e.stage = selected.stage;
        e.stage_ids = selected.stage_ids;
        if selected.header.operation == "delete" {
            e.header.operation = "delete".into();
            e.deleted_at = json!(now);
        }
    } else if d.entity_type == "stage" {
        e.stage = metadata::normalize_metadata_numbers(d.proposed.clone());
    } else {
        e.stage_ids = d.proposed.clone();
    }
    if d.entity_type == "stage_order" {
        let ids: Vec<String> =
            serde_json::from_value(e.stage_ids.clone()).map_err(|_| Error::Invalid)?;
        e.stage_heads = heads(&tx, a, p, &ids)?;
    }
    let prior:Option<(Vec<u8>,String)>=tx.query_row("SELECT e.canonical_frame,d.expected_local_json FROM cloud_sync_structural_events e JOIN cloud_sync_structural_decisions d ON d.account_id=e.account_id AND d.event_id=e.event_id WHERE e.account_id=?1 AND e.project_id=?2 AND e.entity_type=?3 AND e.entity_id=?4 AND e.state IN ('unsealed','sealed')",params![a,p,d.entity_type,d.entity_id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    if let Some((raw, expected)) = prior {
        let old = unframe(&raw)?;
        if old.version == 2
            && old.header.parent_event_ids == current
            && old.header.operation == e.header.operation
            && old.stage == e.stage
            && old.stage_ids == e.stage_ids
            && old.stage_heads == e.stage_heads
            && serde_json::from_str::<Value>(&expected).map_err(|_| Error::Invalid)?
                == d.expected_local
        {
            tx.commit()?;
            return Ok(old.header.event_id);
        }
        return Err(Error::Conflict);
    }
    prepare_tx(&tx, a, &e, &current)?;
    tx.execute(
        "INSERT INTO cloud_sync_structural_decisions VALUES(?1,?2,?3)",
        params![a, e.header.event_id, d.expected_local.to_string()],
    )?;
    let id = e.header.event_id;
    tx.commit()?;
    Ok(id)
}
/// Ordinary production edits are durable intent first; visible portable values
/// change only with authenticated self echo. Local-only payload stays on device.
pub(crate) fn normal_edit(
    db: &mut Connection,
    p: &str,
    id: &str,
    proposed: Value,
    now: &str,
) -> Result<bool, Error> {
    let a: Option<String> = db
        .query_row(
            "SELECT account_id FROM cloud_sync_project_bindings WHERE project_id=?1",
            [p],
            |r| r.get(0),
        )
        .optional()?;
    let Some(a) = a else {
        return Ok(false);
    };
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if !structural_started(&tx, &a, p)? {
        return Ok(false);
    }
    // Local snapshot, current tips and intent share the same write transaction.
    if authority(&tx, &a, p)?.state != "active" {
        return Err(Error::Conflict);
    }
    let portable = portable_from_payload(&proposed)?;
    let mut e = empty_event(header(&tx, &a, p, "stage", id, now)?);
    let parents = tips(&tx, &a, p, "stage", id)?;
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM stages WHERE id=?1 AND project_id=?2)",
        params![id, p],
        |r| r.get(0),
    )?;
    if exists {
        if parents.len() != 1 {
            return Err(Error::Conflict);
        }
        e.stage = portable;
        descend(&tx, &a, &mut e, &parents, false)?;
    } else {
        // New stage and its frozen genesis intent are committed atomically.
        e.stage = portable;
        write_stage(&tx, &e)?;
        let mut local = e.stage.clone();
        if let Some(raw) = proposed.as_object() {
            for (k, v) in raw {
                if !FIELDS.contains(&k.as_str()) {
                    local[k] = v.clone();
                }
            }
        }
        tx.execute(
            "UPDATE stages SET payload_json=?1 WHERE id=?2 AND project_id=?3",
            params![local.to_string(), id, p],
        )?;
        capture_candidate_tx(&tx, &a, p, id, now)?;
        let ids = stage_ids(&tx, p)?;
        let mut order = empty_event(header(&tx, &a, p, "stage_order", "stage_order", now)?);
        let order_parents = tips(&tx, &a, p, "stage_order", "stage_order")?;
        descend(&tx, &a, &mut order, &order_parents, false)?;
        order.stage_ids = json!(ids);
        order.stage_heads = heads(&tx, &a, p, &ids)?;
        order.stage_heads[id] = json!([e.header.event_id]);
        tx.execute(
            "INSERT INTO cloud_sync_structural_order_intents VALUES(?1,?2,?3,?4)",
            params![
                a,
                p,
                e.header.event_id,
                serde_json::to_string(&order).map_err(|_| Error::Invalid)?
            ],
        )?;
    }
    if exists {
        let raw: String = tx.query_row(
            "SELECT payload_json FROM stages WHERE id=?1 AND project_id=?2",
            params![id, p],
            |r| r.get(0),
        )?;
        let mut raw: Value = serde_json::from_str(&raw).map_err(|_| Error::Invalid)?;
        if let Some(source) = proposed.as_object() {
            for (k, v) in source {
                if !FIELDS.contains(&k.as_str()) {
                    raw[k] = v.clone();
                }
            }
        }
        tx.execute(
            "UPDATE stages SET payload_json=?1 WHERE id=?2 AND project_id=?3",
            params![raw.to_string(), id, p],
        )?;
    }
    prepare_tx(&tx, &a, &e, &parents)?;
    if !exists {
        // Creating the first enabled Stage must use the existing metadata writer
        // for that portable preference, never a cache write behind its authority.
        let metadata_view = metadata::authority_view(&tx, &a, p)?;
        let local = metadata_view.local.ok_or(Error::Invalid)?;
        if local["stages_enabled"] != true {
            let mut desired = local.clone();
            desired["stages_enabled"] = json!(true);
            let parents: Vec<String> = metadata_view
                .branches
                .iter()
                .map(|b| b.event_id.clone())
                .collect();
            metadata::prepare_change_in_transaction(
                &tx,
                &a,
                p,
                &e.header.device_id,
                "edit",
                None,
                Some(&desired),
                &local,
                &parents,
                now,
            )?;
        }
    }
    tx.commit()?;
    Ok(true)
}
pub(crate) fn normal_order(
    db: &mut Connection,
    p: &str,
    ids: &[String],
    now: &str,
) -> Result<bool, Error> {
    let a: Option<String> = db
        .query_row(
            "SELECT account_id FROM cloud_sync_project_bindings WHERE project_id=?1",
            [p],
            |r| r.get(0),
        )
        .optional()?;
    let Some(a) = a else {
        return Ok(false);
    };
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if !structural_started(&tx, &a, p)? {
        return Ok(false);
    }
    // Local snapshot, current tips and intent share the same write transaction.
    if authority(&tx, &a, p)?.state != "active" {
        return Err(Error::Conflict);
    }
    let mut e = empty_event(header(&tx, &a, p, "stage_order", "stage_order", now)?);
    let parents = tips(&tx, &a, p, "stage_order", "stage_order")?;
    if parents.len() != 1 {
        return Err(Error::Conflict);
    }
    descend(&tx, &a, &mut e, &parents, false)?;
    e.stage_ids = json!(ids);
    e.stage_heads = heads(&tx, &a, p, ids)?;
    prepare_tx(&tx, &a, &e, &parents)?;
    tx.commit()?;
    Ok(true)
}
fn structural_started(db: &Connection, a: &str, p: &str) -> Result<bool, Error> {
    Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_structural_migrations WHERE account_id=?1 AND project_id=?2) OR EXISTS(SELECT 1 FROM cloud_sync_structural_events WHERE account_id=?1 AND project_id=?2) OR EXISTS(SELECT 1 FROM cloud_sync_inbox WHERE account_id=?1 AND project_id=?2 AND entity_type IN ('stage','stage_order'))", params![a,p], |r| r.get(0))?)
}
pub(crate) fn guard_delete(db: &Connection, p: &str) -> Result<(), Error> {
    let a: Option<String> = db
        .query_row(
            "SELECT account_id FROM cloud_sync_project_bindings WHERE project_id=?1",
            [p],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(a) = a {
        if structural_started(db, &a, p)? {
            return Err(Error::Conflict);
        }
    }
    Ok(())
}
pub(crate) fn pending_events(
    db: &Connection,
    a: &str,
    device: &str,
    sealed: bool,
) -> Result<Vec<Value>, Error> {
    let mut q=db.prepare("SELECT e.canonical_frame,obj.nonce,obj.ciphertext FROM cloud_sync_structural_events e JOIN cloud_sync_outbox o ON o.account_id=e.account_id AND o.event_id=e.event_id LEFT JOIN cloud_sync_event_objects obj ON obj.account_id=e.account_id AND obj.event_id=e.event_id WHERE e.account_id=?1 AND o.device_id=?2 AND e.state=?3 AND o.lifecycle=?3 ORDER BY o.local_ordinal LIMIT 8")?;
    let rows = q
        .query_map(
            params![a, device, if sealed { "sealed" } else { "unsealed" }],
            |r| {
                Ok((
                    r.get::<_, Vec<u8>>(0)?,
                    r.get::<_, Option<Vec<u8>>>(1)?,
                    r.get::<_, Option<Vec<u8>>>(2)?,
                ))
            },
        )?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(|(raw, n, c)| Ok(json!({"event":unframe(&raw)?,"nonce":n,"ciphertext":c})))
        .collect()
}
pub(crate) fn received(
    db: &Connection,
    a: &str,
    limit: i64,
    after: i64,
) -> Result<Vec<Value>, Error> {
    if !(1..=32).contains(&limit) || after < 0 {
        return Err(Error::Invalid);
    }
    let mut q=db.prepare("SELECT i.event_id,i.server_sequence,i.device_id,i.project_id,i.entity_id,i.entity_type,i.sync_revision,i.updated_at,i.deleted_at,i.operation,o.crypto_version,o.aad_version,o.nonce,o.ciphertext FROM cloud_sync_inbox i JOIN cloud_sync_event_objects o ON o.account_id=i.account_id AND o.event_id=i.event_id WHERE i.account_id=?1 AND i.entity_type IN ('stage','stage_order') AND i.state IN ('received','orphan') AND i.server_sequence>?3 ORDER BY i.server_sequence LIMIT ?2")?;
    let rows=q.query_map(params![a,limit,after],|r|Ok(json!({"event_id":r.get::<_,String>(0)?,"server_sequence":r.get::<_,i64>(1)?,"source_device_id":r.get::<_,String>(2)?,"project_id":r.get::<_,String>(3)?,"entity_id":r.get::<_,String>(4)?,"entity_type":r.get::<_,String>(5)?,"revision":r.get::<_,i64>(6)?,"updated_at":r.get::<_,String>(7)?,"deleted_at":r.get::<_,Option<String>>(8)?,"operation":r.get::<_,String>(9)?,"crypto_version":r.get::<_,i64>(10)?,"aad_version":r.get::<_,i64>(11)?,"nonce":r.get::<_,Vec<u8>>(12)?,"ciphertext":r.get::<_,Vec<u8>>(13)?})))?.collect::<Result<Vec<_>,_>>()?;
    Ok(rows)
}
pub(crate) fn commit_receipt(
    db: &mut Connection,
    a: &str,
    device: &str,
    event: &str,
    sequence: i64,
    duplicate: bool,
    now: &str,
) -> Result<(), Error> {
    if sequence < 1 || sequence > 9_007_199_254_740_991 || !metadata::timestamp(now) {
        return Err(Error::Invalid);
    }
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let state:Option<String>=tx.query_row("SELECT lifecycle FROM cloud_sync_outbox WHERE account_id=?1 AND device_id=?2 AND event_id=?3 AND entity_type IN ('stage','stage_order')",params![a,device,event],|r|r.get(0)).optional()?;
    if !matches!(state.as_deref(), Some("sealed" | "accepted")) {
        return Err(Error::Scope);
    }
    let prior:Option<i64>=tx.query_row("SELECT server_sequence FROM cloud_sync_upload_receipts WHERE account_id=?1 AND event_id=?2",params![a,event],|r|r.get(0)).optional()?;
    if let Some(prior) = prior {
        if prior != sequence {
            return Err(Error::Conflict);
        }
    } else {
        tx.execute(
            "INSERT INTO cloud_sync_upload_receipts VALUES(?1,?2,?3,?4,?5,?6)",
            params![a, event, device, sequence, i64::from(duplicate), now],
        )?;
    }
    tx.execute(
        "UPDATE cloud_sync_outbox SET lifecycle='accepted' WHERE account_id=?1 AND event_id=?2",
        params![a, event],
    )?;
    tx.commit()?;
    Ok(())
}
pub(crate) fn reader_blocker(
    db: &Connection,
    a: &str,
    event: &str,
    reason: &str,
) -> Result<(), Error> {
    if !crate::frame_compression::CODES.contains(&reason)
        && !matches!(
            reason,
            "invalid_stage_frame"
                | "structural_authentication_failed"
                | "structural_scope_mismatch"
        )
    {
        return Err(Error::Invalid);
    }
    db.execute("UPDATE cloud_sync_inbox SET state='orphan',error_code=?1 WHERE account_id=?2 AND event_id=?3 AND entity_type IN ('stage','stage_order') AND state IN ('received','orphan')",params![reason,a,event])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{note_sync, sqlite};
    const ACCOUNT: &str = "local-account";
    const NOW: &str = "2026-10-01T00:00:00.000000Z";
    fn event(n: u32) -> Event {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../src/cloud/__fixtures__/stageCodecV1.json"
        ))
        .unwrap();
        let mut e: Event = serde_json::from_value(fixture["event"].clone()).unwrap();
        e.header.event_id = id(n);
        e
    }
    fn id(n: u32) -> String {
        format!("123e4567-e89b-42d3-a456-426614174{n:03}")
    }
    fn database() -> (Connection, std::path::PathBuf) {
        database_with_note(false)
    }
    fn database_with_note(seed_note: bool) -> (Connection, std::path::PathBuf) {
        let path =
            std::env::temp_dir().join(format!("c184-{}.db", metadata::new_event_id().unwrap()));
        let db = sqlite::open_database(&path).unwrap();
        let h = event(101).header;
        db.execute("INSERT INTO projects(id,name,infinite,unit,status,payload_json) VALUES('project','Project',1,'symbols','active','{}')",[]).unwrap();
        db.execute("INSERT INTO project_order VALUES('project',0)", [])
            .unwrap();
        if seed_note {
            legacy_stage(&db, "S1");
            db.execute("INSERT INTO notes(id,project_id,stage_id,payload_json) VALUES('child-note','project','S1','{}')",[]).unwrap();
        }
        db.execute("INSERT INTO cloud_sync_state(account_id,device_id,created_at,updated_at) VALUES(?1,?2,?3,?3)",params![ACCOUNT,h.device_id,NOW]).unwrap();
        db.execute(
            "INSERT INTO cloud_account_bindings VALUES(?1,?2,?3,?3)",
            params![ACCOUNT, h.account_id, NOW],
        )
        .unwrap();
        db.execute(
            "INSERT INTO cloud_sync_project_bindings VALUES('project',?1,?2,?2)",
            params![ACCOUNT, NOW],
        )
        .unwrap();
        db.execute("INSERT INTO cloud_sync_project_bootstraps(project_id,account_id,device_id,bootstrap_id,mode,phase,created_at,updated_at) VALUES('project',?1,?2,?3,'upload_existing','prepared',?4,?4)",params![ACCOUNT,h.device_id,h.bootstrap_id,NOW]).unwrap();
        (db, path)
    }
    fn metadata_active(db: &mut Connection) {
        let h = event(101).header;
        let e = json!({"version":1,"header":{"account_id":h.account_id,"project_id":"project","entity_id":"project","device_id":h.device_id,"bootstrap_id":h.bootstrap_id,"event_id":id(100),"revision":1,"generation":1,"operation":"create","parent_event_ids":[],"updated_at":NOW},"metadata":{"name":"Project","goal":null,"infinite":true,"unit":"symbols","status":"active","deadline":null,"personal_goal":0,"auto_freeze":true,"streak_enabled":true,"work_method":"manual","stages_enabled":false,"combine_stage_mindmaps":false},"deleted_at":null});
        db.execute("INSERT INTO cloud_sync_inbox(account_id,event_id,server_sequence,device_id,project_id,entity_id,entity_type,operation,sync_revision,updated_at,state,received_at) VALUES(?1,?2,1,?3,'project','project','project_metadata','upsert',1,?4,'received',?4)",params![ACCOUNT,id(100),h.device_id,NOW]).unwrap();
        db.execute(
            "INSERT INTO cloud_sync_event_objects VALUES(?1,?2,1,1,zeroblob(24),zeroblob(16),?3)",
            params![ACCOUNT, id(100), NOW],
        )
        .unwrap();
        metadata::preserve_authenticated_event(
            db,
            ACCOUNT,
            "project",
            &serde_json::to_vec(&e).unwrap(),
            NOW,
        )
        .unwrap();
        assert_eq!(
            metadata::authority_view(db, ACCOUNT, "project")
                .unwrap()
                .state,
            "active"
        );
    }
    fn inbox(db: &Connection, e: &Event, seq: i64) {
        let h = &e.header;
        db.execute("INSERT INTO cloud_sync_inbox(account_id,event_id,server_sequence,device_id,project_id,entity_id,entity_type,operation,sync_revision,updated_at,deleted_at,state,received_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'received',?10)",params![ACCOUNT,h.event_id,seq,h.device_id,h.project_id,h.entity_id,h.entity_type,if h.operation=="delete"{"delete"}else{"upsert"},h.revision,NOW,e.deleted_at.as_str()]).unwrap();
        db.execute("INSERT OR IGNORE INTO cloud_sync_event_objects VALUES(?1,?2,1,1,zeroblob(24),zeroblob(16),?3)",params![ACCOUNT,h.event_id,NOW]).unwrap();
        db.execute(
            "UPDATE cloud_sync_state SET pull_cursor=MAX(pull_cursor,?1) WHERE account_id=?2",
            params![seq, ACCOUNT],
        )
        .unwrap();
    }
    fn apply_event(db: &mut Connection, e: &Event, seq: i64) -> String {
        inbox(db, e, seq);
        apply(db, ACCOUNT, &frame(e).unwrap(), &[0; 24], &[0; 16])
            .unwrap()
            .into()
    }
    fn update(base: &Event, n: u32, name: &str) -> Event {
        let mut e = base.clone();
        e.header.event_id = id(n);
        e.header.operation = "update".into();
        e.header.revision += 1;
        e.header.generation += 1;
        e.header.parent_event_ids = vec![base.header.event_id.clone()];
        e.stage["name"] = json!(name);
        e
    }
    fn order(n: u32, stages: &[Event]) -> Event {
        let mut e = event(n);
        e.header.entity_id = "stage_order".into();
        e.header.entity_type = "stage_order".into();
        e.stage = Value::Null;
        e.stage_ids = json!(stages
            .iter()
            .map(|s| s.header.entity_id.clone())
            .collect::<Vec<_>>());
        e.stage_heads = json!(stages
            .iter()
            .map(|s| (s.header.entity_id.clone(), vec![s.header.event_id.clone()]))
            .collect::<std::collections::BTreeMap<_, _>>());
        e
    }
    fn ack(db: &mut Connection) -> i64 {
        let h = event(101).header;
        note_sync::prepare_note_sync_ack(
            db,
            &note_sync::PrepareNoteSyncAckCommand {
                account_id: ACCOUNT.into(),
                device_id: h.device_id,
                canonical_user_id: h.account_id,
            },
        )
        .unwrap()
        .candidate_cursor
    }
    #[test]
    fn stage_sync_codec_golden_and_unknown_frame_keys() {
        let e = event(101);
        let f = frame(&e).unwrap();
        let fixture: Value = serde_json::from_str(include_str!(
            "../../src/cloud/__fixtures__/stageCodecV1.json"
        ))
        .unwrap();
        assert_eq!(
            &f[20..],
            fixture["canonical_json"].as_str().unwrap().as_bytes()
        );
        for sample in fixture["numeric_cases"].as_array().unwrap() {
            let mut e = event(101);
            e.stage["goal"] = sample["goal"].clone();
            let bytes = frame(&e).unwrap();
            assert_eq!(
                &bytes[20..],
                sample["canonical_json"].as_str().unwrap().as_bytes()
            );
            assert!(unframe(&bytes).is_ok());
        }
        for offset in [0, 8, 9, 10, 11, 12, 16] {
            let mut bad = f.clone();
            bad[offset] = 255;
            assert!(unframe(&bad).is_err());
        }
        let mut v = serde_json::to_value(e).unwrap();
        v["stage"]["path"] = json!("/secret");
        assert!(frame(&serde_json::from_value(v).unwrap()).is_err());
        for n in [0.000001, 0.0000001, 1234.5, 9007199254740991.0] {
            let mut e = event(101);
            e.stage["goal"] = json!(n);
            assert!(unframe(&frame(&e).unwrap()).is_ok());
        }
    }
    #[test]
    fn stage_sync_candidates_restart_generation_blocker_and_no_publication() {
        let (mut db, path) = database();
        metadata_active(&mut db);
        let e = event(101);
        db.execute("INSERT INTO stages(id,project_id,name,goal,infinite,unit,status,created_at,payload_json) VALUES('S1','project',?1,1234.5,0,'symbols','активен','2026-09-01',?2)",params![e.stage["name"].as_str(),e.stage.to_string()]).unwrap();
        db.execute("INSERT INTO stage_order VALUES('S1','project',0)", [])
            .unwrap();
        let cid = capture_candidate(&mut db, ACCOUNT, "project", "S1", NOW).unwrap();
        drop(db);
        let mut db = sqlite::open_database(&path).unwrap();
        assert_eq!(
            capture_candidate(&mut db, ACCOUNT, "project", "S1", NOW).unwrap(),
            cid
        );
        let before: String = db
            .query_row("SELECT payload_json FROM stages", [], |r| r.get(0))
            .unwrap();
        prepare(&mut db, ACCOUNT, &e, &[]).unwrap();
        prepare(&mut db, ACCOUNT, &e, &[]).unwrap();
        let mut duplicate = e.clone();
        duplicate.header.event_id = id(198);
        assert!(prepare(&mut db, ACCOUNT, &duplicate, &[]).is_err());
        let bytes = frame(&e).unwrap();
        seal(
            &mut db,
            ACCOUNT,
            &e.header.event_id,
            &bytes,
            &[0; 24],
            &[0; 16],
        )
        .unwrap();
        assert_eq!(apply_event(&mut db, &e, 2), "applied");
        prepare(&mut db, ACCOUNT, &e, &[]).unwrap();
        seal(
            &mut db,
            ACCOUNT,
            &e.header.event_id,
            &bytes,
            &[0; 24],
            &[0; 16],
        )
        .unwrap();
        assert_eq!(ack(&mut db), 2);
        assert_eq!(
            db.query_row("SELECT payload_json FROM stages", [], |r| r
                .get::<_, String>(0))
                .unwrap()
                .contains("Stage Alpha"),
            true
        );
        db.execute(
            "UPDATE stages SET payload_json=json_set(payload_json,'$.unknown',1)",
            [],
        )
        .unwrap();
        let cid2 = capture_candidate(&mut db, ACCOUNT, "project", "S1", NOW).unwrap();
        assert_ne!(cid, cid2);
        assert_eq!(
            db.query_row(
                "SELECT generation FROM cloud_sync_stage_candidates WHERE candidate_id=?1",
                [cid2],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            2
        );
        assert!(!before.is_empty());
        assert_eq!(
            db.query_row("SELECT count(*) FROM cloud_sync_outbox", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        let mut later = event(199);
        later.header.entity_id = "S1".into();
        assert!(prepare(&mut db, ACCOUNT, &later, &[]).is_err());
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn stage_sync_concurrent_edit_delete_and_tombstone_ack_guard() {
        let (mut db, path) = database();
        metadata_active(&mut db);
        let e = event(101);
        assert_eq!(apply_event(&mut db, &e, 2), "applied");
        let beta = update(&e, 102, "Stage Beta");
        let gamma = update(&e, 103, "Stage Gamma");
        assert_eq!(apply_event(&mut db, &beta, 3), "applied");
        assert_eq!(apply_event(&mut db, &gamma, 4), "conflict_preserved");
        assert_eq!(
            tips(&db, ACCOUNT, "project", "stage", "S1").unwrap(),
            vec![id(102), id(103)]
        );
        assert_eq!(ack(&mut db), 4);
        let mut second = event(104);
        second.header.entity_id = "S2".into();
        apply_event(&mut db, &second, 5);
        let mut delete = update(&second, 105, "unused");
        delete.header.operation = "delete".into();
        delete.stage = Value::Null;
        delete.deleted_at = json!(NOW);
        assert_eq!(apply_event(&mut db, &delete, 6), "tombstone_blocked");
        let edit = update(&second, 106, "Surviving edit");
        assert_eq!(apply_event(&mut db, &edit, 7), "conflict_preserved");
        assert_eq!(ack(&mut db), 5);
        assert_eq!(
            tips(&db, ACCOUNT, "project", "stage", "S2").unwrap().len(),
            2
        );
        assert_eq!(
            db.query_row("SELECT name FROM stages WHERE id='S2'", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "Stage Alpha"
        );
        drop(db);
        let mut db = sqlite::open_database(&path).unwrap();
        assert_eq!(ack(&mut db), 5);
        assert_eq!(
            apply(
                &mut db,
                ACCOUNT,
                &frame(&delete).unwrap(),
                &[0; 24],
                &[0; 16]
            )
            .unwrap(),
            "tombstone_blocked"
        );
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn stage_sync_metadata_and_stage_order_orphans_retry_after_restart() {
        let (mut db, path) = database();
        let e = event(101);
        assert_eq!(apply_event(&mut db, &e, 2), "orphan");
        assert_eq!(ack(&mut db), 0);
        drop(db);
        let mut db = sqlite::open_database(&path).unwrap();
        metadata_active(&mut db);
        assert_eq!(retry(&mut db, ACCOUNT, 8).unwrap(), vec!["applied"]);
        let initial: String = db
            .query_row("SELECT payload_json FROM stages WHERE id='S1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let initial: Value = serde_json::from_str(&initial).unwrap();
        assert_eq!(initial["total"], json!(0));
        assert_eq!(initial["progress_entries"], json!([]));
        assert_eq!(
            crate::progress_sync::source(&db, "project", Some("S1")).unwrap()["chain"]
                ["base_total"],
            json!("0.000000")
        );
        let mut s2 = event(102);
        s2.header.entity_id = "S2".into();
        let o = order(103, &[e.clone(), s2.clone()]);
        assert_eq!(apply_event(&mut db, &o, 3), "orphan");
        assert_eq!(apply_event(&mut db, &s2, 4), "applied");
        assert_eq!(ack(&mut db), 2);
        drop(db);
        let mut db = sqlite::open_database(&path).unwrap();
        assert_eq!(retry(&mut db, ACCOUNT, 8).unwrap(), vec!["applied"]);
        assert_eq!(ack(&mut db), 4);
        assert_eq!(
            apply(&mut db, ACCOUNT, &frame(&e).unwrap(), &[0; 24], &[0; 16]).unwrap(),
            "applied"
        );
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM cloud_sync_structural_apply_ledger",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            3
        );
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn stage_sync_concurrent_permutation_and_invalid_membership() {
        let (mut db, path) = database();
        metadata_active(&mut db);
        let mut stages = Vec::new();
        for i in 1..=3 {
            let mut e = event(100 + i);
            e.header.entity_id = format!("S{i}");
            apply_event(&mut db, &e, 1 + i as i64);
            stages.push(e);
        }
        let base = order(104, &stages);
        assert_eq!(apply_event(&mut db, &base, 5), "applied");
        let mut a = base.clone();
        a.header.event_id = id(105);
        a.header.operation = "update".into();
        a.header.revision = 2;
        a.header.generation = 2;
        a.header.parent_event_ids = vec![id(104)];
        a.stage_ids = json!(["S2", "S1", "S3"]);
        let mut b = a.clone();
        b.header.event_id = id(106);
        b.stage_ids = json!(["S1", "S3", "S2"]);
        assert_eq!(apply_event(&mut db, &a, 6), "applied");
        assert_eq!(apply_event(&mut db, &b, 7), "conflict_preserved");
        assert_eq!(
            tips(&db, ACCOUNT, "project", "stage_order", "stage_order")
                .unwrap()
                .len(),
            2
        );
        let mut bad = order(107, &stages);
        bad.stage_ids = json!(["S1", "S1", "S3"]);
        assert!(frame(&bad).is_err());
        bad = order(108, &stages[..2]);
        assert_eq!(apply_event(&mut db, &bad, 8), "orphan");
        let mut foreign = event(109);
        foreign.header.entity_id = "unknown".into();
        bad = order(110, &[foreign]);
        assert_eq!(apply_event(&mut db, &bad, 9), "orphan");
        assert_eq!(ack(&mut db), 7);
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn stage_sync_atomic_rollback_and_scope_guards() {
        let (mut db, path) = database();
        metadata_active(&mut db);
        let e = event(101);
        inbox(&db, &e, 2);
        db.execute_batch("CREATE TRIGGER fail_ledger BEFORE INSERT ON cloud_sync_structural_apply_ledger BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
        assert!(apply(&mut db, ACCOUNT, &frame(&e).unwrap(), &[0; 24], &[0; 16]).is_err());
        assert_eq!(
            db.query_row("SELECT count(*) FROM stages", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(ack(&mut db), 1);
        db.execute_batch("DROP TRIGGER fail_ledger").unwrap();
        assert!(apply(&mut db, ACCOUNT, &frame(&e).unwrap(), &[1; 24], &[0; 16]).is_err());
        assert_eq!(
            apply(&mut db, ACCOUNT, &frame(&e).unwrap(), &[0; 24], &[0; 16]).unwrap(),
            "applied"
        );
        let mut altered = e.clone();
        altered.stage["name"] = json!("Changed replay");
        assert!(apply(
            &mut db,
            ACCOUNT,
            &frame(&altered).unwrap(),
            &[0; 24],
            &[0; 16]
        )
        .is_err());
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn stage_sync_tombstone_preserves_children_and_blocks_order_removal() {
        let (mut db, path) = database();
        metadata_active(&mut db);
        let e = event(101);
        apply_event(&mut db, &e, 2);
        db.execute("UPDATE stages SET payload_json=json_set(payload_json,'$.mindmap',json('{\"private\":1}'),'$.total',42)",[]).unwrap();
        db.execute("INSERT INTO progress_entries(id,project_id,stage_id,payload_json) VALUES('progress','project','S1','{}')",[]).unwrap();
        db.execute("INSERT INTO progress_order VALUES('progress',0)", [])
            .unwrap();
        // Note triggers retain their local-only behavior even on a connected stage.
        let before: String = db
            .query_row("SELECT payload_json FROM stages WHERE id='S1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let mut delete = update(&e, 102, "unused");
        delete.header.operation = "delete".into();
        delete.stage = Value::Null;
        delete.deleted_at = json!(NOW);
        assert_eq!(apply_event(&mut db, &delete, 3), "tombstone_blocked");
        assert_eq!(
            db.query_row("SELECT payload_json FROM stages WHERE id='S1'", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap(),
            before
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM progress_entries", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        let included = order(103, &[delete]);
        assert_eq!(apply_event(&mut db, &included, 4), "orphan");
        let removed = order(104, &[]);
        assert_eq!(apply_event(&mut db, &removed, 5), "orphan");
        assert_eq!(ack(&mut db), 2);
        assert_eq!(
            db.query_row("SELECT count(*) FROM stage_order", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn stage_sync_fair_orphans_foreign_stage_and_invalid_frame_block_shared_ack() {
        let (mut db, path) = database();
        metadata_active(&mut db);
        let mut unknown = event(101);
        unknown.header.project_id = "missing-project".into();
        assert_eq!(apply_event(&mut db, &unknown, 2), "orphan");
        let good = event(102);
        assert_eq!(apply_event(&mut db, &good, 3), "applied");
        assert_eq!(ack(&mut db), 1);
        assert_eq!(retry(&mut db, ACCOUNT, 1).unwrap(), vec!["orphan"]);
        let bad = event(103);
        inbox(&db, &bad, 4);
        let mut bytes = frame(&bad).unwrap();
        bytes[11] = 1;
        assert_eq!(
            apply_received(
                &mut db,
                ACCOUNT,
                &bad.header.event_id,
                &bytes,
                &[0; 24],
                &[0; 16]
            )
            .unwrap(),
            "invalid_stage_frame"
        );
        assert_eq!(
            db.query_row(
                "SELECT error_code FROM cloud_sync_inbox WHERE event_id=?1",
                [id(103)],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "invalid_stage_frame"
        );
        db.execute("INSERT INTO projects(id,name,infinite,unit,status,payload_json) VALUES('foreign','Foreign',1,'symbols','active','{}')",[]).unwrap();
        db.execute("INSERT INTO project_order VALUES('foreign',1)", [])
            .unwrap();
        db.execute("INSERT INTO stages(id,project_id,name,infinite,unit,status,payload_json) VALUES('foreign-stage','foreign','Foreign',1,'symbols','active','{}')",[]).unwrap();
        db.execute(
            "INSERT INTO stage_order VALUES('foreign-stage','foreign',0)",
            [],
        )
        .unwrap();
        let mut foreign = event(104);
        foreign.header.entity_id = "foreign-stage".into();
        let o = order(105, &[foreign]);
        assert_eq!(apply_event(&mut db, &o, 5), "orphan");
        assert_eq!(
            db.query_row(
                "SELECT blocker FROM cloud_sync_structural_events WHERE event_id=?1",
                [id(105)],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "stage_order_foreign_stage"
        );
        drop(db);
        let mut db = sqlite::open_database(&path).unwrap();
        assert_eq!(ack(&mut db), 1);
        assert_eq!(
            db.query_row("SELECT count(*) FROM cloud_sync_inbox", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            5
        );
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn stage_sync_metadata_genesis_conflict_and_untracked_local_edit_are_preserved() {
        let (mut db, path) = database();
        metadata_active(&mut db);
        let e = event(101);
        apply_event(&mut db, &e, 2);
        db.execute("UPDATE stages SET name='Local untracked',payload_json=json_set(payload_json,'$.private_path','/device/only') WHERE id='S1'",[]).unwrap();
        let remote = update(&e, 102, "Remote edit");
        assert_eq!(apply_event(&mut db, &remote, 3), "conflict_preserved");
        assert_eq!(
            db.query_row("SELECT name FROM stages", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "Local untracked"
        );
        let source: String = db
            .query_row(
                "SELECT source_json FROM cloud_sync_stage_candidates",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(source.contains("/device/only"));
        let raw: Vec<u8> = db
            .query_row(
                "SELECT canonical_payload FROM cloud_sync_metadata_apply_ledger",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let mut meta: Value = serde_json::from_slice(&raw).unwrap();
        meta["header"]["event_id"] = json!(id(199));
        meta["metadata"]["name"] = json!("Other genesis");
        let mut descriptor = e.clone();
        descriptor.header.event_id = id(199);
        descriptor.header.entity_id = "project".into();
        descriptor.header.entity_type = "project_metadata".into();
        inbox(&db, &descriptor, 4);
        metadata::preserve_authenticated_event(
            &mut db,
            ACCOUNT,
            "project",
            &serde_json::to_vec(&meta).unwrap(),
            NOW,
        )
        .unwrap();
        let mut later = event(103);
        later.header.entity_id = "S2".into();
        assert_eq!(apply_event(&mut db, &later, 5), "orphan");
        assert_eq!(ack(&mut db), 4);
        assert_eq!(
            db.query_row(
                "SELECT blocker FROM cloud_sync_structural_events WHERE event_id=?1",
                [id(103)],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "project_metadata_authority_unresolved"
        );
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    fn legacy_stage(db: &Connection, id: &str) {
        let s = event(101).stage;
        db.execute("INSERT INTO stages(id,project_id,name,goal,infinite,unit,status,created_at,payload_json) VALUES(?1,'project',?2,1234.5,0,'symbols','активен','2026-09-01',?3)",params![id,s["name"].as_str(),s.to_string()]).unwrap();
        db.execute(
            "INSERT INTO stage_order VALUES(?1,'project',(SELECT count(*) FROM stage_order))",
            [id],
        )
        .unwrap();
    }
    fn pending(db: &Connection) -> Vec<Event> {
        pending_events(db, ACCOUNT, &event(101).header.device_id, false)
            .unwrap()
            .into_iter()
            .map(|v| serde_json::from_value(v["event"].clone()).unwrap())
            .collect()
    }
    fn echo(db: &mut Connection, e: &Event, seq: i64) -> String {
        seal(
            db,
            ACCOUNT,
            &e.header.event_id,
            &frame(e).unwrap(),
            &[0; 24],
            &[0; 16],
        )
        .unwrap();
        apply_event(db, e, seq)
    }
    fn activate(db: &mut Connection) -> (Event, Event) {
        metadata_active(db);
        if !db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM stages WHERE id='S1')",
                [],
                |r| r.get::<_, bool>(0),
            )
            .unwrap()
        {
            legacy_stage(db, "S1");
        }
        assert_eq!(
            authority(db, ACCOUNT, "project").unwrap().state,
            "structural_local"
        );
        begin(db, ACCOUNT, "project", NOW).unwrap();
        let stage = pending(db).pop().unwrap();
        assert_eq!(echo(db, &stage, 2), "applied");
        advance(db, ACCOUNT, "project").unwrap();
        let order = pending(db).pop().unwrap();
        assert_eq!(echo(db, &order, 3), "applied");
        assert_eq!(authority(db, ACCOUNT, "project").unwrap().state, "active");
        (stage, order)
    }
    fn decision(
        db: &mut Connection,
        t: &str,
        id: &str,
        selected: Option<String>,
        proposed: Value,
    ) -> Event {
        let view = authority(db, ACCOUNT, "project").unwrap();
        let row = view
            .entities
            .iter()
            .find(|r| r["entity_type"] == t && r["entity_id"] == id)
            .unwrap();
        let d = Decision {
            entity_type: t.into(),
            entity_id: id.into(),
            expected_tips: serde_json::from_value(row["tips"].clone()).unwrap(),
            expected_local: row["local"].clone(),
            proposed,
            selected_event_id: selected,
        };
        let result = decide(db, ACCOUNT, "project", &d, NOW).unwrap();
        pending(db)
            .into_iter()
            .find(|e| e.header.event_id == result)
            .unwrap()
    }
    #[test]
    fn stage_sync_explicit_migration_frozen_restart_receipts_and_order() {
        let (mut db, path) = database();
        metadata_active(&mut db);
        legacy_stage(&db, "S1");
        legacy_stage(&db, "S2");
        // Reading/retrying cannot start migration or capture source candidates.
        authority(&db, ACCOUNT, "project").unwrap();
        advance(&mut db, ACCOUNT, "project").unwrap();
        retry(&mut db, ACCOUNT, 8).unwrap();
        assert!(pending(&db).is_empty());
        let v = begin(&mut db, ACCOUNT, "project", NOW).unwrap();
        assert_eq!(v.state, "publication_pending");
        let events = pending(&db);
        assert_eq!(events.len(), 2);
        drop(db);
        let mut db = sqlite::open_database(&path).unwrap();
        assert_eq!(
            begin(&mut db, ACCOUNT, "project", NOW)
                .unwrap()
                .migration_id,
            v.migration_id
        );
        assert_eq!(frame(&pending(&db)[0]).unwrap(), frame(&events[0]).unwrap());
        for (i, e) in events.iter().enumerate() {
            seal(
                &mut db,
                ACCOUNT,
                &e.header.event_id,
                &frame(e).unwrap(),
                &[0; 24],
                &[0; 16],
            )
            .unwrap();
        }
        drop(db);
        let mut db = sqlite::open_database(&path).unwrap();
        assert_eq!(
            pending_events(&db, ACCOUNT, &event(101).header.device_id, true)
                .unwrap()
                .len(),
            2
        );
        assert_eq!(apply_event(&mut db, &events[0], 2), "applied");
        advance(&mut db, ACCOUNT, "project").unwrap();
        assert!(pending(&db).is_empty());
        let device = event(101).header.device_id;
        commit_receipt(
            &mut db,
            ACCOUNT,
            &device,
            &events[1].header.event_id,
            3,
            true,
            NOW,
        )
        .unwrap();
        assert_eq!(apply_event(&mut db, &events[1], 3), "applied");
        advance(&mut db, ACCOUNT, "project").unwrap();
        let order = pending(&db).pop().unwrap();
        assert_eq!(order.stage_ids, json!(["S1", "S2"]));
        assert_eq!(echo(&mut db, &order, 4), "applied");
        assert_eq!(ack(&mut db), 4);
        assert_eq!(authority(&db, ACCOUNT, "project").unwrap().state, "active");
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    fn echo_enabled_metadata(db: &mut Connection, stage: &Event, seq: i64) {
        let metadata = metadata::unsealed_genesis(db, ACCOUNT, &stage.header.device_id)
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(metadata["metadata"]["stages_enabled"], true);
        let metadata_id = metadata["header"]["event_id"].as_str().unwrap();
        metadata::commit_sealed_genesis(
            db,
            ACCOUNT,
            &stage.header.device_id,
            metadata_id,
            &[0; 24],
            &[0; 16],
        )
        .unwrap();
        let mut descriptor = stage.clone();
        descriptor.header.event_id = metadata_id.into();
        descriptor.header.entity_id = "project".into();
        descriptor.header.entity_type = "project_metadata".into();
        descriptor.header.revision = metadata["header"]["revision"].as_i64().unwrap();
        inbox(db, &descriptor, seq);
        metadata::preserve_authenticated_event(
            db,
            ACCOUNT,
            "project",
            &serde_json::to_vec(&metadata).unwrap(),
            NOW,
        )
        .unwrap();
    }
    #[test]
    fn stage_sync_normal_writer_pending_atomic_local_fields_creation_and_order() {
        let (mut db, path) = database();
        let (stage, order) = activate(&mut db);
        let mut proposed = stage.stage.clone();
        proposed["name"] = json!("Rename");
        proposed["total"] = json!(42);
        assert!(normal_edit(&mut db, "project", "S1", proposed, NOW).unwrap());
        let edit = pending(&db).pop().unwrap();
        assert_eq!(edit.header.parent_event_ids, vec![stage.header.event_id]);
        assert_eq!(
            db.query_row("SELECT name FROM stages", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "Stage Alpha"
        );
        assert_eq!(echo(&mut db, &edit, 4), "applied");
        assert_eq!(
            db.query_row(
                "SELECT json_extract(payload_json,'$.total') FROM stages",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            42
        );
        assert!(normal_order(&mut db, "project", &["S1".into()], NOW).unwrap());
        let reorder = pending(&db).pop().unwrap();
        assert_eq!(reorder.header.parent_event_ids, vec![order.header.event_id]);
        assert_eq!(echo(&mut db, &reorder, 5), "applied");
        assert!(normal_edit(&mut db, "project", "S2", stage.stage, NOW).unwrap());
        let create = pending(&db).pop().unwrap();
        assert_eq!(create.header.operation, "create");
        echo_enabled_metadata(&mut db, &create, 6);
        assert_eq!(echo(&mut db, &create, 7), "applied");
        advance(&mut db, ACCOUNT, "project").unwrap();
        let order = pending(&db).pop().unwrap();
        assert_eq!(order.stage_ids, json!(["S1", "S2"]));
        assert_eq!(echo(&mut db, &order, 8), "applied");
        assert_eq!(authority(&db, ACCOUNT, "project").unwrap().state, "active");
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn stage_sync_stale_creation_order_intent_requires_explicit_reconciliation() {
        let (mut db, path) = database();
        let (stage, _) = activate(&mut db);
        normal_edit(&mut db, "project", "S2", stage.stage.clone(), NOW).unwrap();
        let create = pending(&db).pop().unwrap();
        echo_enabled_metadata(&mut db, &create, 4);
        let edit = update(&stage, 181, "Peer rename");
        assert_eq!(apply_event(&mut db, &edit, 5), "applied");
        assert_eq!(echo(&mut db, &create, 6), "applied");
        assert!(advance(&mut db, ACCOUNT, "project").is_err());
        assert_eq!(
            authority(&db, ACCOUNT, "project").unwrap().state,
            "conflict"
        );
        let resolution = decision(
            &mut db,
            "stage_order",
            "stage_order",
            None,
            json!(["S1", "S2"]),
        );
        assert_eq!(echo(&mut db, &resolution, 7), "applied");
        drop(db);
        let mut db = sqlite::open_database(&path).unwrap();
        advance(&mut db, ACCOUNT, "project").unwrap();
        assert_eq!(authority(&db, ACCOUNT, "project").unwrap().state, "active");
        assert!(pending(&db).is_empty());
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM cloud_sync_structural_order_intents",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn stage_sync_full_tip_resolution_stale_stage_and_order() {
        let (mut db, path) = database();
        let (stage, order) = activate(&mut db);
        for (base, t, id) in [
            (stage, "stage", "S1"),
            (order, "stage_order", "stage_order"),
        ] {
            let mut a = update(&base, 120 + if t == "stage" { 0 } else { 10 }, "A");
            if t == "stage_order" {
                a.stage = Value::Null;
                a.stage_heads = heads(&db, ACCOUNT, "project", &["S1".into()]).unwrap();
            }
            let mut b = a.clone();
            b.header.event_id = id_fn(121 + if t == "stage" { 0 } else { 10 });
            if t == "stage" {
                b.stage["name"] = json!("B");
            }
            let seq = if t == "stage" { 4 } else { 9 };
            assert_eq!(apply_event(&mut db, &a, seq), "applied");
            assert_eq!(apply_event(&mut db, &b, seq + 1), "conflict_preserved");
            let r = decision(&mut db, t, id, Some(a.header.event_id.clone()), Value::Null);
            assert_eq!(r.version, 2);
            assert_eq!(r.header.parent_event_ids.len(), 2);
            let mut c = a.clone();
            c.header.event_id = id_fn(122 + if t == "stage" { 0 } else { 10 });
            assert_eq!(apply_event(&mut db, &c, seq + 2), "conflict_preserved");
            assert_eq!(echo(&mut db, &r, seq + 3), "conflict_preserved");
            let current = tips(&db, ACCOUNT, "project", t, id).unwrap();
            assert_eq!(current.len(), 2);
            assert!(current.contains(&c.header.event_id));
            assert!(current.contains(&r.header.event_id));
            drop(db);
            db = sqlite::open_database(&path).unwrap();
            let r2 = decision(&mut db, t, id, Some(r.header.event_id), Value::Null);
            assert_eq!(echo(&mut db, &r2, seq + 4), "applied");
            assert_eq!(
                tips(&db, ACCOUNT, "project", t, id).unwrap(),
                vec![r2.header.event_id]
            );
        }
        drop(db);
        std::fs::remove_file(path).unwrap();
        fn id_fn(n: u32) -> String {
            super::tests::id(n)
        }
    }
    #[test]
    fn stage_sync_selected_tombstone_is_causal_only_retry_cannot_resurrect() {
        let (mut db, path) = database_with_note(true);
        let (stage, _) = activate(&mut db);
        db.execute("INSERT INTO progress_entries(id,project_id,stage_id,payload_json) VALUES('child','project','S1','{}')",[]).unwrap();
        db.execute("INSERT INTO documents(id,scope_key,project_id,stage_id,title,content_json,content_format,extensions_json) VALUES('child-doc','stage:S1','project','S1','Draft','{}','tiptap','{}')",[]).unwrap();
        db.execute("UPDATE stages SET payload_json=json_set(payload_json,'$.mindmap',json('{\"private\":1}')) WHERE id='S1'",[]).unwrap();
        let mut delete = update(&stage, 150, "unused");
        delete.header.operation = "delete".into();
        delete.stage = Value::Null;
        delete.deleted_at = json!(NOW);
        assert_eq!(apply_event(&mut db, &delete, 4), "tombstone_blocked");
        let edit = update(&stage, 151, "Edit");
        assert_eq!(apply_event(&mut db, &edit, 5), "conflict_preserved");
        let r = decision(
            &mut db,
            "stage",
            "S1",
            Some(delete.header.event_id.clone()),
            Value::Null,
        );
        assert_eq!(echo(&mut db, &r, 6), "tombstone_blocked");
        assert_eq!(
            tips(&db, ACCOUNT, "project", "stage", "S1").unwrap(),
            vec![r.header.event_id.clone()]
        );
        retry(&mut db, ACCOUNT, 8).unwrap();
        assert_eq!(
            tips(&db, ACCOUNT, "project", "stage", "S1").unwrap(),
            vec![r.header.event_id]
        );
        let view = authority(&db, ACCOUNT, "project").unwrap();
        assert_eq!(view.state, "blocked");
        assert_eq!(view.entities[0]["causal_tombstone_selected"], true);
        assert_eq!(view.entities[0]["conflict"], false);
        assert!(guard_delete(&db, "project").is_err());
        assert_eq!(ack(&mut db), 3);
        assert_eq!(
            db.query_row("SELECT count(*) FROM progress_entries", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM notes WHERE id='child-note' AND stage_id='S1'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM documents WHERE id='child-doc' AND stage_id='S1'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        assert_eq!(
            db.query_row(
                "SELECT json_extract(payload_json,'$.mindmap.private') FROM stages WHERE id='S1'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        assert!(normal_order(&mut db, "project", &[], NOW).is_err());
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn stage_sync_existing_local_import_requires_explicit_reconciliation() {
        let (mut db, path) = database();
        metadata_active(&mut db);
        legacy_stage(&db, "S1");
        let e = event(101);
        assert_eq!(apply_event(&mut db, &e, 2), "conflict_preserved");
        let o = order(102, &[e.clone()]);
        assert_eq!(apply_event(&mut db, &o, 3), "conflict_preserved");
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM cloud_sync_structural_local_orders",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        assert_eq!(
            authority(&db, ACCOUNT, "project").unwrap().state,
            "conflict"
        );
        let r = decision(&mut db, "stage", "S1", Some(e.header.event_id), Value::Null);
        assert_eq!(echo(&mut db, &r, 4), "applied");
        let r = decision(
            &mut db,
            "stage_order",
            "stage_order",
            Some(o.header.event_id),
            Value::Null,
        );
        assert_eq!(echo(&mut db, &r, 5), "applied");
        assert_eq!(authority(&db, ACCOUNT, "project").unwrap().state, "active");
        drop(db);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn stage_sync_migration_blocker_repair_new_generation_and_atomic_begin() {
        let (mut db, path) = database();
        metadata_active(&mut db);
        legacy_stage(&db, "S1");
        db.execute(
            "UPDATE stages SET payload_json=json_set(payload_json,'$.unknown',1)",
            [],
        )
        .unwrap();
        let first = begin(&mut db, ACCOUNT, "project", NOW).unwrap();
        assert_eq!(first.state, "blocked");
        assert!(pending(&db).is_empty());
        drop(db);
        let mut db = sqlite::open_database(&path).unwrap();
        assert_eq!(authority(&db, ACCOUNT, "project").unwrap().state, "blocked");
        db.execute(
            "UPDATE stages SET payload_json=json_remove(payload_json,'$.unknown')",
            [],
        )
        .unwrap();
        db.execute_batch("CREATE TRIGGER fail_begin BEFORE INSERT ON cloud_sync_outbox BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
        assert!(begin(&mut db, ACCOUNT, "project", NOW).is_err());
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM cloud_sync_structural_migrations",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        db.execute_batch("DROP TRIGGER fail_begin").unwrap();
        let second = begin(&mut db, ACCOUNT, "project", NOW).unwrap();
        assert_ne!(first.migration_id, second.migration_id);
        assert_eq!(
            db.query_row(
                "SELECT max(generation) FROM cloud_sync_structural_migrations",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            2
        );
        assert_eq!(pending(&db).len(), 1);
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn stage_sync_decision_exact_retry_and_untracked_local_cas() {
        let (mut db, path) = database();
        let (stage, _) = activate(&mut db);
        let a = update(&stage, 160, "A");
        let b = update(&stage, 161, "B");
        apply_event(&mut db, &a, 4);
        apply_event(&mut db, &b, 5);
        let r = decision(
            &mut db,
            "stage",
            "S1",
            Some(a.header.event_id.clone()),
            Value::Null,
        );
        let same = decision(&mut db, "stage", "S1", Some(a.header.event_id), Value::Null);
        assert_eq!(r.header.event_id, same.header.event_id);
        assert_eq!(frame(&r).unwrap(), frame(&same).unwrap());
        seal(
            &mut db,
            ACCOUNT,
            &r.header.event_id,
            &frame(&r).unwrap(),
            &[0; 24],
            &[0; 16],
        )
        .unwrap();
        db.execute("UPDATE stages SET name='Untracked after decision',payload_json=json_set(payload_json,'$.name','Untracked after decision')",[]).unwrap();
        assert_eq!(apply_event(&mut db, &r, 6), "conflict_preserved");
        assert_eq!(
            db.query_row("SELECT name FROM stages", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "Untracked after decision"
        );
        assert_eq!(
            authority(&db, ACCOUNT, "project").unwrap().state,
            "conflict"
        );
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn stage_sync_remote_resolution_does_not_adopt_unrelated_local_candidate() {
        let (mut db, path) = database();
        metadata_active(&mut db);
        legacy_stage(&db, "S1");
        db.execute("UPDATE stages SET name='Private local',payload_json=json_set(payload_json,'$.name','Private local') WHERE id='S1'",[]).unwrap();
        let genesis = event(190);
        assert_eq!(apply_event(&mut db, &genesis, 2), "conflict_preserved");
        let mut remote = update(&genesis, 191, "Remote selected");
        remote.version = 2;
        remote.header.device_id = id(192);
        assert_eq!(apply_event(&mut db, &remote, 3), "conflict_preserved");
        assert_eq!(
            db.query_row("SELECT name FROM stages WHERE id='S1'", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "Private local"
        );
        drop(db);
        let mut db = sqlite::open_database(&path).unwrap();
        let local_decision = decision(
            &mut db,
            "stage",
            "S1",
            Some(remote.header.event_id),
            Value::Null,
        );
        assert_eq!(echo(&mut db, &local_decision, 4), "applied");
        assert_eq!(
            db.query_row("SELECT name FROM stages WHERE id='S1'", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "Remote selected"
        );
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn stage_sync_dormant_local_writer_does_not_require_metadata_activation() {
        let (mut db, path) = database();
        legacy_stage(&db, "S1");
        // Connected but metadata authority has not been activated; Stage writers
        // must remain ordinary local operations until explicit structural work.
        assert!(!normal_edit(&mut db, "project", "S1", json!({}), NOW).unwrap());
        assert!(!normal_order(&mut db, "project", &["S1".into()], NOW).unwrap());
        guard_delete(&db, "project").unwrap();
        assert!(pending(&db).is_empty());
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM cloud_sync_structural_migrations",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn stage_sync_frozen_order_reference_survives_rename_but_not_unknown_or_delete() {
        let (mut db, path) = database();
        let (stage, base) = activate(&mut db);
        let mut a = update(&base, 170, "unused");
        a.stage = Value::Null;
        prepare(&mut db, ACCOUNT, &a, &a.header.parent_event_ids).unwrap();
        seal(
            &mut db,
            ACCOUNT,
            &a.header.event_id,
            &frame(&a).unwrap(),
            &[0; 24],
            &[0; 16],
        )
        .unwrap();
        let edit = update(&stage, 171, "New name");
        assert_eq!(apply_event(&mut db, &edit, 4), "applied");
        assert_eq!(apply_event(&mut db, &a, 5), "applied");
        assert_eq!(ack(&mut db), 5);
        let mut bad = update(&a, 172, "unused");
        bad.stage = Value::Null;
        bad.stage_heads = json!({"S1":[id(199)]});
        assert_eq!(apply_event(&mut db, &bad, 6), "orphan");
        let mut delete = update(&edit, 173, "unused");
        delete.header.operation = "delete".into();
        delete.stage = Value::Null;
        delete.deleted_at = json!(NOW);
        assert_eq!(apply_event(&mut db, &delete, 7), "tombstone_blocked");
        let mut blocked = update(&a, 174, "unused");
        blocked.stage = Value::Null;
        assert_eq!(apply_event(&mut db, &blocked, 8), "orphan");
        assert_eq!(ack(&mut db), 5);
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
}
