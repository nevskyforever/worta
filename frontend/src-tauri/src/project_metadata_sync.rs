//! C18 project-metadata storage boundary, activated only by explicit migration.
//! The caller must authenticate and unframe the C18 object before passing its
//! canonical payload here; this module repeats scope and causal checks in SQLite.
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

const MAX_BYTES: usize = 1024 * 1024;
const MAX_REVISION: i64 = 9_007_199_254_740_991;
const FIELDS: [&str; 12] = [
    "auto_freeze",
    "combine_stage_mindmaps",
    "deadline",
    "goal",
    "infinite",
    "name",
    "personal_goal",
    "stages_enabled",
    "status",
    "streak_enabled",
    "unit",
    "work_method",
];
const LEGACY_PROJECT_FIELDS: &[&str] = &[
    "id",
    "name",
    "goal",
    "infinite",
    "total",
    "progress",
    "deadline",
    "status",
    "unit",
    "created_at",
    "updated_at",
    "notes_updated_at",
    "mindmap_updated_at",
    "completed_at",
    "personal_goal",
    "today_goal",
    "planning_date",
    "plan_daily_goal",
    "added_today",
    "remaining",
    "streak_enabled",
    "streak_status",
    "streak_length",
    "max_streak",
    "auto_freeze",
    "progress_entries",
    "project_notes",
    "mindmap",
    "stages",
    "stages_enabled",
    "combine_stage_mindmaps",
    "cover_image",
    "cover_reference",
    "folder_id",
    "synch",
    "last_synch",
    "sync_available",
    "work_method",
];

#[derive(Debug)]
pub(crate) enum MetadataError {
    Compression(&'static str),
    Invalid,
    Scope,
    Conflict,
    Database(rusqlite::Error),
}
impl From<rusqlite::Error> for MetadataError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Database(e)
    }
}
impl std::fmt::Display for MetadataError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Compression(code) => write!(f, "{code}"),
            Self::Invalid => write!(f, "invalid_project_metadata"),
            Self::Scope => write!(f, "metadata_scope_mismatch"),
            Self::Conflict => write!(f, "metadata_conflict"),
            Self::Database(e) => write!(f, "{e}"),
        }
    }
}
pub(crate) fn uuid(value: &str) -> bool {
    let b = value.as_bytes();
    b.len() == 36
        && (b'1'..=b'5').contains(&b[14])
        && matches!(b[19], b'8' | b'9' | b'a' | b'b')
        && b.iter().enumerate().all(|(i, c)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                *c == b'-'
            } else {
                c.is_ascii_hexdigit() && !c.is_ascii_uppercase()
            }
        })
}
fn exact(o: &Map<String, Value>, keys: &[&str]) -> bool {
    o.len() == keys.len() && keys.iter().all(|k| o.contains_key(*k))
}
fn str_field<'a>(o: &'a Map<String, Value>, key: &str) -> Result<&'a str, MetadataError> {
    o.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.len() <= 512)
        .ok_or(MetadataError::Invalid)
}
fn positive(o: &Map<String, Value>, key: &str) -> Result<i64, MetadataError> {
    o.get(key)
        .and_then(Value::as_i64)
        .filter(|v| (1..=MAX_REVISION).contains(v))
        .ok_or(MetadataError::Invalid)
}
pub(crate) fn timestamp(value: &str) -> bool {
    let b = value.as_bytes();
    if !(b.len() == 27
        && b[4] == b'-'
        && b[7] == b'-'
        && b[10] == b'T'
        && b[13] == b':'
        && b[16] == b':'
        && b[19] == b'.'
        && b[26] == b'Z'
        && b.iter()
            .enumerate()
            .all(|(i, c)| matches!(i, 4 | 7 | 10 | 13 | 16 | 19 | 26) || c.is_ascii_digit()))
    {
        return false;
    }
    let number = |start: usize, end: usize| value[start..end].parse::<u32>().unwrap();
    let (year, month, day, hour, minute, second) = (
        number(0, 4),
        number(5, 7),
        number(8, 10),
        number(11, 13),
        number(14, 16),
        number(17, 19),
    );
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => 0,
    };
    day > 0 && day <= days && hour < 24 && minute < 60 && second < 60
}
pub(crate) fn new_event_id() -> Result<String, MetadataError> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| MetadataError::Invalid)?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Ok(format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        u32::from_be_bytes(bytes[0..4].try_into().unwrap()),
        u16::from_be_bytes(bytes[4..6].try_into().unwrap()),
        u16::from_be_bytes(bytes[6..8].try_into().unwrap()),
        u16::from_be_bytes(bytes[8..10].try_into().unwrap()),
        u64::from_be_bytes([
            0, 0, bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
        ])
    ))
}
fn validate_metadata(value: &Value) -> Result<(), MetadataError> {
    let o = value.as_object().ok_or(MetadataError::Invalid)?;
    let mut keys = FIELDS.to_vec();
    if o.contains_key("cover_reference") {
        keys.push("cover_reference");
        crate::project_cover_sync::validate_reference(&o["cover_reference"])?;
    }
    if !exact(o, &keys) {
        return Err(MetadataError::Invalid);
    }
    for key in ["name", "unit", "status", "work_method"] {
        str_field(o, key)?;
    }
    if !o["goal"].is_null()
        && !o["goal"]
            .as_f64()
            .is_some_and(|v| v.is_finite() && v >= 0.0)
    {
        return Err(MetadataError::Invalid);
    }
    if !o["personal_goal"]
        .as_f64()
        .is_some_and(|v| v.is_finite() && v >= 0.0)
    {
        return Err(MetadataError::Invalid);
    }
    if !o["deadline"].is_null()
        && !o["deadline"]
            .as_str()
            .is_some_and(|s| !s.is_empty() && s.len() <= 512)
    {
        return Err(MetadataError::Invalid);
    }
    for key in [
        "infinite",
        "auto_freeze",
        "streak_enabled",
        "stages_enabled",
        "combine_stage_mindmaps",
    ] {
        if !o[key].is_boolean() {
            return Err(MetadataError::Invalid);
        }
    }
    Ok(())
}

pub(crate) fn normalize_metadata_numbers(mut metadata: Value) -> Value {
    for key in ["goal", "personal_goal"] {
        if let Some(value) = metadata[key].as_f64() {
            if value.fract() == 0.0 && value >= 0.0 && value <= MAX_REVISION as f64 {
                metadata[key] = json!(value as i64);
            }
        }
    }
    metadata
}

/// Capture the exact current legacy source and its allowlisted projection. The
/// existing row is reused on retry, including after process restart.
pub(crate) fn capture_legacy_candidate(
    connection: &mut Connection,
    account: &str,
    project: &str,
    now: &str,
) -> Result<String, MetadataError> {
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let row = tx.query_row(
        "SELECT b.account_id,s.device_id,boot.bootstrap_id,p.name,p.goal,p.infinite,p.unit,p.status,p.payload_json,p.updated_at \
         FROM cloud_sync_project_bindings b JOIN cloud_sync_state s ON s.account_id=b.account_id \
         LEFT JOIN cloud_sync_project_bootstraps boot ON boot.project_id=b.project_id AND boot.account_id=b.account_id \
         JOIN projects p ON p.id=b.project_id WHERE b.account_id=?1 AND b.project_id=?2",
        params![account, project], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,Option<String>>(2)?,r.get::<_,String>(3)?,r.get::<_,Option<f64>>(4)?,r.get::<_,i64>(5)?,r.get::<_,String>(6)?,r.get::<_,String>(7)?,r.get::<_,String>(8)?,r.get::<_,Option<String>>(9)?))
    ).optional()?.ok_or(MetadataError::Scope)?;
    let (scope, device, bootstrap, name, goal, infinite, unit, status, source, updated) = row;
    if scope != account || !uuid(&device) || bootstrap.as_deref().is_some_and(|id| !uuid(id)) {
        return Err(MetadataError::Scope);
    }
    let raw: Value = serde_json::from_str(&source).map_err(|_| MetadataError::Invalid)?;
    let original = raw.as_object().ok_or(MetadataError::Invalid)?;
    let snapshot = json!({
        "name":name,"goal":goal,"infinite":infinite!=0,"unit":unit,"status":status,
        "deadline":original.get("deadline").cloned().unwrap_or(Value::Null),
        "personal_goal":original.get("personal_goal").cloned().unwrap_or(json!(0)),
        "auto_freeze":original.get("auto_freeze").cloned().unwrap_or(json!(true)),
        "streak_enabled":original.get("streak_enabled").cloned().unwrap_or(json!(true)),
        "work_method":original.get("work_method").cloned().unwrap_or(json!("manual")),
        "stages_enabled":original.get("stages_enabled").cloned().unwrap_or(json!(false)),
        "combine_stage_mindmaps":original.get("combine_stage_mindmaps").cloned().unwrap_or(json!(false))
    });
    let snapshot = normalize_metadata_numbers(snapshot);
    validate_metadata(&snapshot)?;
    let extensions: Option<String> = tx.query_row(
        "SELECT payload_json FROM project_extensions WHERE entity_type='project' AND entity_id=?1",
        [project], |r| r.get(0)).optional()?;
    let mut unsupported: Vec<String> = original
        .keys()
        .filter(|key| !LEGACY_PROJECT_FIELDS.contains(&key.as_str()))
        .cloned()
        .collect();
    if bootstrap.is_none() {
        unsupported.push("bootstrap_lineage_missing".into());
    }
    if let Some(raw) = extensions {
        let extension: Value = serde_json::from_str(&raw).map_err(|_| MetadataError::Invalid)?;
        if extension
            .as_object()
            .is_none_or(|object| !object.is_empty())
        {
            unsupported.push("project_extensions".into());
        }
    }
    unsupported.sort();
    let snapshot_json = serde_json::to_string(&snapshot).map_err(|_| MetadataError::Invalid)?;
    let unsupported_json =
        serde_json::to_string(&unsupported).map_err(|_| MetadataError::Invalid)?;
    if let Some((id, previous_snapshot, previous_unsupported)) = tx.query_row(
        "SELECT candidate_id,snapshot_json,unsupported_json FROM cloud_sync_metadata_candidates WHERE account_id=?1 AND project_id=?2 AND device_id=?3 ORDER BY generation DESC LIMIT 1",
        params![account,project,device], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?))).optional()? {
        if previous_snapshot == snapshot_json && previous_unsupported == unsupported_json {
            tx.commit()?; return Ok(id);
        }
    }
    let generation: i64 = tx.query_row(
        "SELECT COALESCE(MAX(generation),0)+1 FROM cloud_sync_metadata_candidates WHERE account_id=?1 AND project_id=?2 AND device_id=?3",
        params![account,project,device], |r| r.get(0))?;
    if !(1..=MAX_REVISION).contains(&generation) {
        return Err(MetadataError::Invalid);
    }
    let id = new_event_id()?;
    tx.execute("INSERT INTO cloud_sync_metadata_candidates(candidate_id,account_id,project_id,device_id,bootstrap_id,generation,codec_version,snapshot_json,unsupported_json,source_payload_json,source_updated_at,state,created_at) VALUES(?1,?2,?3,?4,?5,?6,1,?7,?8,?9,?10,'candidate',?11)",
        params![id,account,project,device,bootstrap,generation,snapshot_json,unsupported_json,source,updated,now])?;
    tx.commit()?;
    Ok(id)
}

/// Explicit preparation. C18.3.01 sealing/upload reuses this exact event ID;
/// neither capture nor this method contacts the server.
pub(crate) fn prepare_metadata_genesis(
    connection: &mut Connection,
    account: &str,
    candidate_id: &str,
    now: &str,
) -> Result<String, MetadataError> {
    if !timestamp(now) {
        return Err(MetadataError::Invalid);
    }
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let row = tx.query_row(
        "SELECT c.project_id,c.device_id,c.bootstrap_id,c.snapshot_json,c.unsupported_json,c.state,b.account_id,s.device_id \
         FROM cloud_sync_metadata_candidates c JOIN cloud_sync_project_bindings b ON b.project_id=c.project_id \
         JOIN cloud_sync_state s ON s.account_id=c.account_id \
         WHERE c.account_id=?1 AND c.candidate_id=?2",
        params![account,candidate_id], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,Option<String>>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?,r.get::<_,String>(6)?,r.get::<_,String>(7)?))
    ).optional()?.ok_or(MetadataError::Scope)?;
    let (project, device, bootstrap, snapshot, unsupported, state, binding_account, current_device) =
        row;
    if binding_account != account
        || current_device != device
        || bootstrap.is_none()
        || unsupported != "[]"
    {
        return Err(MetadataError::Scope);
    }
    if let Some(existing) = tx.query_row(
        "SELECT event_id FROM cloud_sync_metadata_events WHERE account_id=?1 AND candidate_id=?2",
        params![account,candidate_id], |r|r.get::<_,String>(0)).optional()? {
        tx.commit()?; return Ok(existing);
    }
    if state != "candidate" {
        return Err(MetadataError::Conflict);
    }
    let known_history: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM cloud_sync_metadata_tips WHERE account_id=?1 AND project_id=?2)",
        params![account,project],|r|r.get(0))?;
    if known_history {
        return Err(MetadataError::Conflict);
    }
    let active: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM cloud_sync_metadata_candidates WHERE account_id=?1 AND project_id=?2 AND state='publishing')",
        params![account,project], |r|r.get(0))?;
    if active {
        return Err(MetadataError::Conflict);
    }
    let event_id = new_event_id()?;
    let ordinal: i64 = tx.query_row(
        "SELECT COALESCE(MAX(local_ordinal),0)+1 FROM cloud_sync_outbox WHERE account_id=?1 AND device_id=?2",
        params![account,device], |r|r.get(0))?;
    if !(1..=MAX_REVISION).contains(&ordinal) {
        return Err(MetadataError::Invalid);
    }
    tx.execute("INSERT INTO cloud_sync_outbox(event_id,account_id,device_id,project_id,entity_id,entity_type,operation,revision,updated_at,created_at,parent_event_id,local_ordinal,lifecycle) VALUES(?1,?2,?3,?4,?4,'project_metadata','upsert',1,?5,?5,NULL,?6,'unsealed')",
        params![event_id,account,device,project,now,ordinal])?;
    tx.execute("INSERT INTO cloud_sync_metadata_events(account_id,event_id,project_id,device_id,bootstrap_id,candidate_id,parent_event_ids_json,generation,revision,operation,payload_json,state,created_at) VALUES(?1,?2,?3,?4,?5,?6,'[]',1,1,'create',?7,'unsealed',?8)",
        params![account,event_id,project,device,bootstrap,candidate_id,snapshot,now])?;
    tx.execute(
        "UPDATE cloud_sync_metadata_candidates SET state='publishing' WHERE candidate_id=?1",
        [candidate_id],
    )?;
    tx.commit()?;
    Ok(event_id)
}

/// Stores an authenticated plaintext event in a transaction. The visible
/// project row is untouched; concurrent genesis tips remain explicit.
pub(crate) fn preserve_authenticated_event(
    connection: &mut Connection,
    account: &str,
    project: &str,
    bytes: &[u8],
    now: &str,
) -> Result<&'static str, MetadataError> {
    preserve_authenticated_event_checked(connection, account, project, bytes, now, None)
}

pub(crate) fn preserve_authenticated_event_checked(
    connection: &mut Connection,
    account: &str,
    project: &str,
    bytes: &[u8],
    now: &str,
    expected_object: Option<(&[u8], &[u8])>,
) -> Result<&'static str, MetadataError> {
    let value = decode_metadata_event(bytes, project)?;
    let root = value.as_object().ok_or(MetadataError::Invalid)?;
    let header = root["header"].as_object().ok_or(MetadataError::Invalid)?;
    let event_id = str_field(header, "event_id")?;
    let device = str_field(header, "device_id")?;
    let bootstrap = str_field(header, "bootstrap_id")?;
    let operation = str_field(header, "operation")?;
    let revision = positive(header, "revision")?;
    let generation = positive(header, "generation")?;
    let updated = str_field(header, "updated_at")?;
    let parents = header["parent_event_ids"]
        .as_array()
        .ok_or(MetadataError::Invalid)?;
    let deleted = root["deleted_at"].as_str();
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let bound: Option<String> = tx.query_row("SELECT a.canonical_user_id FROM cloud_sync_project_bindings b JOIN cloud_account_bindings a ON a.local_account_id=b.account_id JOIN cloud_sync_project_bootstraps boot ON boot.project_id=b.project_id AND boot.account_id=b.account_id WHERE b.account_id=?1 AND b.project_id=?2 AND boot.bootstrap_id=?3",
        params![account,project,bootstrap], |r| r.get(0)).optional()?;
    if bound.as_deref() != header["account_id"].as_str() {
        return Err(MetadataError::Scope);
    }
    if let Some(reference) = root["metadata"].get("cover_reference") {
        crate::project_cover_sync::require_material(&tx, account, project, reference)?;
    }
    if let Some((stored, outcome)) = tx.query_row(
        "SELECT canonical_payload,outcome FROM cloud_sync_metadata_apply_ledger WHERE account_id=?1 AND event_id=?2",
        params![account,event_id], |r| Ok((r.get::<_,Vec<u8>>(0)?, r.get::<_,String>(1)?))
    ).optional()? {
        if let Some((expected_nonce,expected_ciphertext))=expected_object {
            let object:Option<(Vec<u8>,Vec<u8>)>=tx.query_row("SELECT nonce,ciphertext FROM cloud_sync_event_objects WHERE account_id=?1 AND event_id=?2 AND crypto_version=1 AND aad_version=1",params![account,event_id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            if object.as_ref().is_none_or(|(nonce,ciphertext)|nonce!=expected_nonce||ciphertext!=expected_ciphertext){return Err(MetadataError::Scope);}
        }
        if stored == bytes { return Ok(if outcome == "applied" { "applied" } else { "conflict_preserved" }); }
        return Err(MetadataError::Conflict);
    }
    let inbox: Option<(String,String,String,String,i64,String,Option<String>,i64)> = tx.query_row(
        "SELECT device_id,project_id,entity_id,operation,sync_revision,updated_at,deleted_at,server_sequence FROM cloud_sync_inbox WHERE account_id=?1 AND event_id=?2 AND entity_type='project_metadata' AND state IN ('received','orphan')",
        params![account,event_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?))).optional()?;
    let Some((
        in_device,
        in_project,
        in_entity,
        in_operation,
        in_revision,
        in_updated,
        in_deleted,
        server_sequence,
    )) = inbox
    else {
        if let Some((stored, outcome)) = tx.query_row("SELECT canonical_payload,outcome FROM cloud_sync_metadata_apply_ledger WHERE account_id=?1 AND event_id=?2",params![account,event_id],|r| Ok((r.get::<_,Vec<u8>>(0)?,r.get::<_,String>(1)?))).optional()? {
            if stored == bytes { return Ok(if outcome == "applied" {"applied"} else {"conflict_preserved"}); }
        }
        return Err(MetadataError::Scope);
    };
    if in_device != device
        || in_project != project
        || in_entity != project
        || in_revision != revision
        || in_updated != updated
        || in_operation
            != if operation == "delete" {
                "delete"
            } else {
                "upsert"
            }
        || in_deleted.as_deref() != deleted
    {
        return Err(MetadataError::Scope);
    }
    let object: Option<(i64, i64, Vec<u8>, Vec<u8>)> = tx.query_row(
        "SELECT crypto_version,aad_version,nonce,ciphertext FROM cloud_sync_event_objects WHERE account_id=?1 AND event_id=?2",
        params![account, event_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    ).optional()?;
    let Some((crypto_version, aad_version, nonce, ciphertext)) = object else {
        return Err(MetadataError::Scope);
    };
    if crypto_version != 1
        || aad_version != 1
        || expected_object.is_some_and(|(n, c)| n != nonce || c != ciphertext)
    {
        return Err(MetadataError::Scope);
    }
    let local_event: Option<(Option<String>,String,String,String,String,i64,i64,String,String)> = tx.query_row(
        "SELECT candidate_id,payload_json,state,operation,parent_event_ids_json,revision,generation,bootstrap_id,device_id FROM cloud_sync_metadata_events WHERE account_id=?1 AND event_id=?2",
        params![account,event_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?))).optional()?;
    let existing_orphan = local_event
        .as_ref()
        .is_some_and(|(candidate, _, state, ..)| candidate.is_none() && state == "orphan");
    let self_echo = if let Some((
        candidate_id,
        stored_payload,
        state,
        stored_operation,
        stored_parents,
        stored_revision,
        stored_generation,
        stored_bootstrap,
        stored_device,
    )) = &local_event
    {
        if stored_operation != operation
            || stored_parents
                != &serde_json::to_string(parents).map_err(|_| MetadataError::Invalid)?
            || *stored_revision != revision
            || *stored_generation != generation
            || stored_bootstrap != bootstrap
            || stored_device != device
        {
            return Err(MetadataError::Conflict);
        }
        if existing_orphan {
            if stored_payload
                != &if operation == "delete" {
                    "{}".to_string()
                } else {
                    root["metadata"].to_string()
                }
            {
                return Err(MetadataError::Conflict);
            }
            false
        } else {
            let decision:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_metadata_decisions WHERE account_id=?1 AND event_id=?2 AND state IN ('pending','conflict'))",
                params![account,event_id],|r|r.get(0))?;
            if (candidate_id.is_none() && !decision)
                || !matches!(state.as_str(), "unsealed" | "sealed" | "accepted")
                || stored_payload != &root["metadata"].to_string()
                || (candidate_id.is_some() && operation != "create")
            {
                return Err(MetadataError::Conflict);
            }
            let receipt: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM cloud_sync_upload_receipts receipt JOIN cloud_sync_outbox outbox ON outbox.event_id=receipt.event_id WHERE receipt.account_id=?1 AND receipt.event_id=?2 AND receipt.device_id=?3 AND receipt.server_sequence=?4 AND outbox.account_id=?1 AND outbox.project_id=?5 AND outbox.entity_type='project_metadata' AND outbox.revision=?6 AND outbox.lifecycle IN ('sealed','accepted'))",
            params![account,event_id,device,server_sequence,project,revision],|r|r.get(0))?;
            if !receipt {
                let sealed: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM cloud_sync_outbox WHERE account_id=?1 AND event_id=?2 AND device_id=?3 AND project_id=?4 AND entity_type='project_metadata' AND lifecycle='sealed')",
                    params![account,event_id,device,project], |r|r.get(0))?;
                if !sealed {
                    return Err(MetadataError::Scope);
                }
                // The authenticated server echo is the lost upload response.
                tx.execute("INSERT INTO cloud_sync_upload_receipts(account_id,event_id,device_id,server_sequence,duplicate,accepted_at) VALUES(?1,?2,?3,?4,0,?5)",
                    params![account,event_id,device,server_sequence,now])?;
                tx.execute("UPDATE cloud_sync_outbox SET lifecycle='accepted' WHERE account_id=?1 AND event_id=?2 AND lifecycle='sealed'",
                    params![account,event_id])?;
            }
            true
        }
    } else {
        false
    };
    let parents_json = serde_json::to_string(parents).map_err(|_| MetadataError::Invalid)?;
    let payload_json = if operation == "delete" {
        "{}".to_string()
    } else {
        root["metadata"].to_string()
    };
    if existing_orphan {
        let exact_orphan: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM cloud_sync_metadata_events WHERE account_id=?1 AND event_id=?2 AND project_id=?3 AND device_id=?4 AND bootstrap_id=?5 AND parent_event_ids_json=?6 AND generation=?7 AND revision=?8 AND operation=?9 AND payload_json=?10 AND deleted_at IS ?11 AND server_sequence=?12 AND state='orphan')",
            params![account,event_id,project,device,bootstrap,parents_json,generation,revision,operation,payload_json,deleted,server_sequence], |r|r.get(0))?;
        if !exact_orphan {
            return Err(MetadataError::Conflict);
        }
    }
    let tip_count: i64 = tx.query_row(
        "SELECT COUNT(*) FROM cloud_sync_metadata_tips WHERE account_id=?1 AND project_id=?2",
        params![account, project],
        |r| r.get(0),
    )?;
    if operation != "create" {
        let mut max_parent_revision = 0_i64;
        let mut max_parent_generation = 0_i64;
        let mut genesis_parents = true;
        for parent in parents {
            let parent = parent.as_str().ok_or(MetadataError::Invalid)?;
            let prior: Option<(i64, String, String, i64)> = tx.query_row(
                "SELECT revision,operation,parent_event_ids_json,generation FROM cloud_sync_metadata_events WHERE account_id=?1 AND event_id=?2 AND project_id=?3 AND state IN ('applied','conflict_preserved')",
                params![account,parent,project], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
            if prior.is_none() {
                if !existing_orphan {
                    tx.execute("INSERT INTO cloud_sync_metadata_events(account_id,event_id,project_id,device_id,bootstrap_id,parent_event_ids_json,generation,revision,operation,payload_json,deleted_at,state,server_sequence,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'orphan',?12,?13)",
                        params![account,event_id,project,device,bootstrap,parents_json,generation,revision,operation,payload_json,deleted,server_sequence,now])?;
                    tx.execute("UPDATE cloud_sync_inbox SET state='orphan' WHERE account_id=?1 AND event_id=?2",params![account,event_id])?;
                }
                tx.commit()?;
                return Ok("orphan");
            }
            let (parent_revision, parent_operation, parent_parents, parent_generation) =
                prior.ok_or(MetadataError::Invalid)?;
            max_parent_revision = max_parent_revision.max(parent_revision);
            max_parent_generation = max_parent_generation.max(parent_generation);
            genesis_parents &=
                parent_operation == "create" && parent_revision == 1 && parent_parents == "[]";
        }
        if revision
            != max_parent_revision
                .checked_add(1)
                .ok_or(MetadataError::Invalid)?
            || generation <= max_parent_generation
            || (operation == "genesis_resolution" && !genesis_parents)
        {
            return Err(MetadataError::Invalid);
        }
    }
    let local_candidates: i64 = tx.query_row("SELECT COUNT(*) FROM cloud_sync_metadata_candidates WHERE account_id=?1 AND project_id=?2 AND state!='resolved'",params![account,project],|r|r.get(0))?;
    let mut tip_query = tx.prepare("SELECT event_id FROM cloud_sync_metadata_tips WHERE account_id=?1 AND project_id=?2 ORDER BY event_id")?;
    let tip_ids: Vec<String> = tip_query
        .query_map(params![account, project], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    drop(tip_query);
    let parent_ids: Vec<&str> = parents
        .iter()
        .map(|p| p.as_str().ok_or(MetadataError::Invalid))
        .collect::<Result<_, _>>()?;
    let exact_tips = tip_ids
        .iter()
        .map(String::as_str)
        .eq(parent_ids.iter().copied());
    let outcome = if self_echo && tip_count == 0 {
        "applied"
    } else if operation == "create" && (tip_count > 0 || local_candidates > 0) {
        "conflict_preserved"
    } else if operation == "create" || exact_tips {
        "applied"
    } else {
        "conflict_preserved"
    };
    if self_echo {
        tx.execute("UPDATE cloud_sync_metadata_events SET state=?1,server_sequence=?2 WHERE account_id=?3 AND event_id=?4",
            params![if outcome=="applied" {"applied"} else {"conflict_preserved"},server_sequence,account,event_id])?;
        tx.execute("UPDATE cloud_sync_metadata_candidates SET state=?1 WHERE candidate_id=(SELECT candidate_id FROM cloud_sync_metadata_events WHERE account_id=?2 AND event_id=?3)",params![if outcome=="applied" {"published"} else {"conflict"},account,event_id])?;
    } else if existing_orphan {
        tx.execute(
            "UPDATE cloud_sync_metadata_events SET state=?1 WHERE account_id=?2 AND event_id=?3",
            params![
                if outcome == "applied" {
                    "applied"
                } else {
                    "conflict_preserved"
                },
                account,
                event_id
            ],
        )?;
    } else {
        tx.execute("INSERT INTO cloud_sync_metadata_events(account_id,event_id,project_id,device_id,bootstrap_id,parent_event_ids_json,generation,revision,operation,payload_json,deleted_at,state,server_sequence,created_at) SELECT ?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,server_sequence,?13 FROM cloud_sync_inbox WHERE account_id=?1 AND event_id=?2",
            params![account,event_id,project,device,bootstrap,parents_json,generation,revision,operation,payload_json,deleted,if outcome=="applied" {"applied"} else {"conflict_preserved"},now])?;
    }
    tx.execute(
        "INSERT INTO cloud_sync_metadata_tips(account_id,project_id,event_id) VALUES(?1,?2,?3)",
        params![account, project, event_id],
    )?;
    // Tips are causal maxima, even for a stale resolution. Uncovered branches remain.
    for parent in parents {
        tx.execute("DELETE FROM cloud_sync_metadata_tips WHERE account_id=?1 AND project_id=?2 AND event_id=?3",params![account,project,parent.as_str()])?;
    }
    if outcome == "applied" {
        tx.execute("INSERT INTO cloud_sync_metadata_projection(account_id,project_id,head_event_id,revision,payload_json,deleted_at) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(account_id,project_id) DO UPDATE SET head_event_id=excluded.head_event_id,revision=excluded.revision,payload_json=excluded.payload_json,deleted_at=excluded.deleted_at",
            params![account,project,event_id,revision,payload_json,deleted])?;
    } else {
        tx.execute(
            "DELETE FROM cloud_sync_metadata_projection WHERE account_id=?1 AND project_id=?2",
            params![account, project],
        )?;
    }
    tx.execute("INSERT INTO cloud_sync_metadata_apply_ledger(account_id,event_id,project_id,outcome,canonical_payload,applied_at) VALUES(?1,?2,?3,?4,?5,?6)",params![account,event_id,project,outcome,bytes,now])?;
    tx.execute("UPDATE cloud_sync_inbox SET state=?1,applied_at=CASE WHEN ?1='applied' THEN ?2 ELSE applied_at END WHERE account_id=?3 AND event_id=?4 AND state IN ('received','orphan')",
        params![if outcome=="applied" {"applied"} else {"conflict"},now,account,event_id])?;
    if outcome == "applied" && matches!(operation, "resolution" | "genesis_resolution") {
        tx.execute("UPDATE cloud_sync_metadata_candidates SET state='resolved' WHERE account_id=?1 AND project_id=?2 AND state IN ('conflict','published')",params![account,project])?;
    }
    if self_echo {
        tx.execute("UPDATE cloud_sync_metadata_decisions SET state=?1 WHERE account_id=?2 AND event_id=?3 AND state='pending'",
            params![if outcome=="applied" {"applied"} else {"conflict"},account,event_id])?;
    }
    if outcome == "applied" && operation != "delete" && !self_echo {
        let proof:Option<(String,String)>=tx.query_row(
            "SELECT head_event_id,portable_json FROM cloud_sync_metadata_reconciliation WHERE account_id=?1 AND project_id=?2",
            params![account,project],|r|Ok((r.get(0)?,r.get(1)?)),
        ).optional()?;
        if let Some((head, raw)) = proof {
            let previous: Value = serde_json::from_str(&raw).map_err(|_| MetadataError::Invalid)?;
            // A verified full resolution covers prior reconciled ancestors and
            // authenticated local edits on its branches. Untracked local edits stay visible.
            let resolution = matches!(operation, "resolution" | "genesis_resolution");
            let covered: bool = if resolution {
                tx.query_row("WITH RECURSIVE ancestors(id) AS (SELECT value FROM json_each(?3) UNION SELECT parent.value FROM ancestors JOIN cloud_sync_metadata_events e ON e.account_id=?1 AND e.project_id=?2 AND e.event_id=ancestors.id JOIN json_each(e.parent_event_ids_json) parent) SELECT EXISTS(SELECT 1 FROM ancestors WHERE id=?4)",params![account,project,parents_json,head],|r|r.get(0))?
            } else {
                parents.iter().any(|parent| parent.as_str() == Some(&head))
            };
            let visible = visible_metadata(&tx, project)?;
            let covered_local_change: bool = if resolution {
                tx.query_row("WITH RECURSIVE ancestors(id) AS (SELECT value FROM json_each(?3) UNION SELECT parent.value FROM ancestors JOIN cloud_sync_metadata_events e ON e.account_id=?1 AND e.project_id=?2 AND e.event_id=ancestors.id JOIN json_each(e.parent_event_ids_json) parent) SELECT EXISTS(SELECT 1 FROM ancestors JOIN cloud_sync_metadata_events e ON e.account_id=?1 AND e.event_id=ancestors.id JOIN cloud_sync_metadata_decisions d ON d.account_id=e.account_id AND d.event_id=e.event_id WHERE e.project_id=?2 AND e.payload_json=?4 AND e.state IN ('applied','conflict_preserved'))",params![account,project,parents_json,visible.to_string()],|r|r.get(0))?
            } else {
                false
            };
            if covered && (visible == previous || covered_local_change) {
                write_visible_metadata(&tx, project, &root["metadata"], now)?;
            }
        }
    }
    if outcome == "applied"
        && visible_metadata(&tx, project).ok().as_ref() == Some(&root["metadata"])
    {
        tx.execute("INSERT INTO cloud_sync_metadata_reconciliation(account_id,project_id,head_event_id,portable_json,reconciled_at) VALUES(?1,?2,?3,?4,?5) \
            ON CONFLICT(account_id,project_id) DO UPDATE SET head_event_id=excluded.head_event_id,portable_json=excluded.portable_json,reconciled_at=excluded.reconciled_at",
            params![account,project,event_id,payload_json,now])?;
    }
    if outcome == "applied" && root["metadata"].get("cover_reference").is_some() {
        // Existing full-record conflict handling decides the winner; project the verified winner only.
        if visible_metadata(&tx, project).ok().as_ref() == Some(&root["metadata"]) {
            write_visible_metadata(&tx, project, &root["metadata"], now)?;
        }
        tx.execute(
            "DELETE FROM cloud_cover_blockers WHERE account_id=?1 AND event_id=?2",
            params![account, event_id],
        )?;
        tx.execute("UPDATE cloud_cover_intents SET state='active',blocker=NULL WHERE account_id=?1 AND event_id=?2",params![account,event_id])?;
        if matches!(operation, "resolution" | "genesis_resolution") {
            tx.execute("UPDATE cloud_cover_intents SET state='active',blocker=NULL WHERE account_id=?1 AND project_id=?2 AND state='metadata_pending'",params![account,project])?;
        }
    }
    if outcome == "conflict_preserved" {
        tx.execute("UPDATE cloud_cover_intents SET blocker='cover_conflict' WHERE account_id=?1 AND event_id=?2",params![account,event_id])?;
    }
    tx.execute(
        "DELETE FROM cloud_cover_blockers WHERE account_id=?1 AND event_id=?2",
        params![account, event_id],
    )?;
    invalidate_stale_decisions(&tx, account, project, now)?;
    tx.commit()?;
    Ok(outcome)
}

pub(crate) fn assert_runtime_scope(
    connection: &Connection,
    account: &str,
    user: &str,
    device: &str,
) -> Result<(), MetadataError> {
    let stored: Option<(String,String)> = connection.query_row(
        "SELECT binding.canonical_user_id,state.device_id FROM cloud_account_bindings binding JOIN cloud_sync_state state ON state.account_id=binding.local_account_id WHERE binding.local_account_id=?1",
        [account], |r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    if stored
        .as_ref()
        .is_none_or(|(u, d)| u != user || d != device)
        || !uuid(user)
        || !uuid(device)
    {
        return Err(MetadataError::Scope);
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MetadataScope {
    pub account_id: String,
    pub canonical_user_id: String,
    pub device_id: String,
}

#[derive(Serialize)]
pub(crate) struct MetadataMigrationStatus {
    pub state: String,
    pub candidate_id: Option<String>,
    pub event_id: Option<String>,
    pub blockers: Value,
    pub genesis_tips: i64,
}

pub(crate) fn migration_status(
    connection: &Connection,
    account: &str,
    project: &str,
) -> Result<MetadataMigrationStatus, MetadataError> {
    let candidate: Option<(String,String,String)> = connection.query_row(
        "SELECT candidate_id,state,unsupported_json FROM cloud_sync_metadata_candidates WHERE account_id=?1 AND project_id=?2 ORDER BY generation DESC LIMIT 1",
        params![account,project], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
    let tips: i64 = connection.query_row(
        "SELECT COUNT(*) FROM cloud_sync_metadata_tips WHERE account_id=?1 AND project_id=?2",
        params![account, project],
        |r| r.get(0),
    )?;
    let event: Option<(String, String)> = if let Some((candidate_id, _, _)) = &candidate {
        connection.query_row("SELECT event_id,state FROM cloud_sync_metadata_events WHERE account_id=?1 AND candidate_id=?2",params![account,candidate_id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?
    } else {
        None
    };
    let blockers: Value = candidate
        .as_ref()
        .map(|(_, _, raw)| serde_json::from_str(raw))
        .transpose()
        .map_err(|_| MetadataError::Invalid)?
        .unwrap_or_else(|| json!([]));
    let pending_inbox:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_inbox WHERE account_id=?1 AND project_id=?2 AND entity_type='project_metadata' AND state IN ('received','orphan'))",params![account,project],|r|r.get(0))?;
    let state = if tips > 1
        || tips > 0
            && candidate
                .as_ref()
                .is_some_and(|(_, state, _)| state == "candidate")
        || candidate
            .as_ref()
            .is_some_and(|(_, state, _)| state == "conflict")
    {
        "genesis_conflict"
    } else if tips == 1 && candidate.is_none()
        || event.as_ref().is_some_and(|(_, state)| state == "applied")
    {
        "authenticated_metadata_active"
    } else if blockers
        .as_array()
        .is_some_and(|a| a.iter().any(|v| v == "bootstrap_lineage_missing"))
    {
        "blocked_missing_bootstrap"
    } else if blockers.as_array().is_some_and(|a| !a.is_empty()) {
        "blocked_unsupported"
    } else if pending_inbox {
        "apply_blocked"
    } else if event.as_ref().is_some_and(|(_, state)| state == "accepted") {
        "awaiting_remote_confirmation"
    } else if event
        .as_ref()
        .is_some_and(|(_, state)| matches!(state.as_str(), "unsealed" | "sealed"))
    {
        "genesis_pending"
    } else if candidate.is_some() {
        "legacy_candidate_present"
    } else {
        "legacy_local"
    };
    Ok(MetadataMigrationStatus {
        state: state.into(),
        candidate_id: candidate.as_ref().map(|(id, _, _)| id.clone()),
        event_id: event.map(|(id, _)| id),
        blockers,
        genesis_tips: tips,
    })
}

pub(crate) fn unsealed_genesis(
    connection: &Connection,
    account: &str,
    device: &str,
) -> Result<Vec<Value>, MetadataError> {
    let mut statement=connection.prepare("SELECT e.event_id,e.project_id,e.bootstrap_id,e.payload_json,o.updated_at,e.operation,e.parent_event_ids_json,e.generation,e.revision FROM cloud_sync_metadata_events e JOIN cloud_sync_outbox o ON o.event_id=e.event_id WHERE e.account_id=?1 AND e.device_id=?2 AND e.state='unsealed' AND o.lifecycle='unsealed' AND NOT EXISTS(SELECT 1 FROM cloud_sync_metadata_invalidated_decisions stale WHERE stale.account_id=e.account_id AND stale.event_id=e.event_id) ORDER BY o.local_ordinal LIMIT 8")?;
    let events: Result<Vec<Value>, MetadataError> = statement.query_map(params![account,device],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?,r.get::<_,String>(6)?,r.get::<_,i64>(7)?,r.get::<_,i64>(8)?)))?
        .map(|row| { let (event,project,bootstrap,payload,updated,operation,parents,generation,revision)=row?;
            let user:String=connection.query_row("SELECT canonical_user_id FROM cloud_account_bindings WHERE local_account_id=?1",[account],|r|r.get(0))?;
            let metadata:Value=serde_json::from_str(&payload).map_err(|_|MetadataError::Invalid)?;
            let parent_ids:Value=serde_json::from_str(&parents).map_err(|_|MetadataError::Invalid)?;
            Ok(json!({"version":if metadata.get("cover_reference").is_some(){2}else{1},"header":{"account_id":user,"bootstrap_id":bootstrap,"device_id":device,"entity_id":project,"event_id":event,"generation":generation,"operation":operation,"parent_event_ids":parent_ids,"project_id":project,"revision":revision,"updated_at":updated},"metadata":metadata,"deleted_at":null}))
        }).collect();
    events
}

pub(crate) fn commit_sealed_genesis(
    connection: &mut Connection,
    account: &str,
    device: &str,
    event: &str,
    nonce: &[u8],
    ciphertext: &[u8],
) -> Result<(), MetadataError> {
    if nonce.len() != 24 || !(16..=MAX_BYTES + 64).contains(&ciphertext.len()) {
        return Err(MetadataError::Invalid);
    }
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let stale:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_metadata_invalidated_decisions WHERE account_id=?1 AND event_id=?2)",params![account,event],|r|r.get(0))?;
    if stale {
        return Err(MetadataError::Conflict);
    }
    let state:Option<(String,String)>=tx.query_row("SELECT e.state,o.lifecycle FROM cloud_sync_metadata_events e JOIN cloud_sync_outbox o ON o.event_id=e.event_id WHERE e.account_id=?1 AND e.device_id=?2 AND e.event_id=?3 AND o.entity_type='project_metadata'",params![account,device,event],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    if state
        .as_ref()
        .is_none_or(|(e, o)| e != "unsealed" || o != "unsealed")
    {
        return Err(MetadataError::Scope);
    }
    tx.execute("INSERT INTO cloud_sync_event_objects(account_id,event_id,crypto_version,aad_version,nonce,ciphertext,stored_at) VALUES(?1,?2,1,1,?3,?4,strftime('%Y-%m-%dT%H:%M:%fZ','now'))",params![account,event,nonce,ciphertext])?;
    tx.execute(
        "UPDATE cloud_sync_metadata_events SET state='sealed' WHERE account_id=?1 AND event_id=?2",
        params![account, event],
    )?;
    tx.execute(
        "UPDATE cloud_sync_outbox SET lifecycle='sealed' WHERE account_id=?1 AND event_id=?2",
        params![account, event],
    )?;
    tx.commit()?;
    Ok(())
}

#[derive(Serialize)]
pub(crate) struct MetadataUploadItem {
    pub event_id: String,
    pub project_id: String,
    pub revision: i64,
    pub updated_at: String,
    pub nonce: Vec<u8>,
    pub ciphertext: Vec<u8>,
    pub metadata_codec_version: i64,
}
pub(crate) fn sealed_genesis(
    connection: &Connection,
    account: &str,
    device: &str,
) -> Result<Vec<MetadataUploadItem>, MetadataError> {
    let mut statement=connection.prepare("SELECT e.event_id,e.project_id,e.revision,o.updated_at,obj.nonce,obj.ciphertext,CASE WHEN json_type(e.payload_json,'$.cover_reference') IS NULL THEN 1 ELSE 2 END FROM cloud_sync_metadata_events e JOIN cloud_sync_outbox o ON o.event_id=e.event_id JOIN cloud_sync_event_objects obj ON obj.account_id=e.account_id AND obj.event_id=e.event_id WHERE e.account_id=?1 AND e.device_id=?2 AND e.state='sealed' AND o.lifecycle='sealed' AND NOT EXISTS(SELECT 1 FROM cloud_sync_metadata_invalidated_decisions stale WHERE stale.account_id=e.account_id AND stale.event_id=e.event_id) ORDER BY o.local_ordinal LIMIT 8")?;
    let items = statement
        .query_map(params![account, device], |r| {
            Ok(MetadataUploadItem {
                event_id: r.get(0)?,
                project_id: r.get(1)?,
                revision: r.get(2)?,
                updated_at: r.get(3)?,
                nonce: r.get(4)?,
                ciphertext: r.get(5)?,
                metadata_codec_version: r.get(6)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(items)
}

pub(crate) fn commit_upload_receipt(
    connection: &mut Connection,
    account: &str,
    device: &str,
    event: &str,
    sequence: i64,
    duplicate: bool,
    now: &str,
) -> Result<(), MetadataError> {
    if !(1..=MAX_REVISION).contains(&sequence) || !timestamp(now) {
        return Err(MetadataError::Invalid);
    }
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let state:Option<String>=tx.query_row("SELECT lifecycle FROM cloud_sync_outbox WHERE account_id=?1 AND device_id=?2 AND event_id=?3 AND entity_type='project_metadata'",params![account,device,event],|r|r.get(0)).optional()?;
    if !matches!(state.as_deref(), Some("sealed" | "accepted")) {
        return Err(MetadataError::Scope);
    }
    let existing:Option<i64>=tx.query_row("SELECT server_sequence FROM cloud_sync_upload_receipts WHERE account_id=?1 AND event_id=?2",params![account,event],|r|r.get(0)).optional()?;
    if let Some(old) = existing {
        if old != sequence {
            return Err(MetadataError::Conflict);
        }
    } else {
        tx.execute("INSERT INTO cloud_sync_upload_receipts(account_id,event_id,device_id,server_sequence,duplicate,accepted_at) VALUES(?1,?2,?3,?4,?5,?6)",params![account,event,device,sequence,i64::from(duplicate),now])?;
    }
    tx.execute("UPDATE cloud_sync_outbox SET lifecycle='accepted' WHERE account_id=?1 AND event_id=?2 AND lifecycle='sealed'",params![account,event])?;
    tx.execute("UPDATE cloud_sync_metadata_events SET state='accepted',server_sequence=?1 WHERE account_id=?2 AND event_id=?3 AND state='sealed'",params![sequence,account,event])?;
    tx.commit()?;
    Ok(())
}

#[derive(Serialize)]
pub(crate) struct MetadataInboxItem {
    pub event_id: String,
    pub server_sequence: i64,
    pub source_device_id: String,
    pub project_id: String,
    pub entity_id: String,
    pub revision: i64,
    pub updated_at: String,
    pub deleted_at: Option<String>,
    pub operation: String,
    pub crypto_version: i64,
    pub aad_version: i64,
    pub nonce: Vec<u8>,
    pub ciphertext: Vec<u8>,
}
pub(crate) fn received_metadata(
    connection: &Connection,
    account: &str,
    limit: i64,
    after: i64,
) -> Result<Vec<MetadataInboxItem>, MetadataError> {
    if !(1..=32).contains(&limit) || !(0..=MAX_REVISION).contains(&after) {
        return Err(MetadataError::Invalid);
    }
    let tx = connection.unchecked_transaction()?;
    let mut statement=tx.prepare("SELECT inbox.event_id,inbox.server_sequence,inbox.device_id,inbox.project_id,inbox.entity_id,inbox.sync_revision,inbox.updated_at,inbox.deleted_at,inbox.operation,obj.crypto_version,obj.aad_version,obj.nonce,obj.ciphertext FROM cloud_sync_inbox inbox JOIN cloud_sync_event_objects obj ON obj.account_id=inbox.account_id AND obj.event_id=inbox.event_id WHERE inbox.account_id=?1 AND inbox.entity_type='project_metadata' AND inbox.state IN ('received','orphan') AND inbox.server_sequence>?3 ORDER BY COALESCE((SELECT ordinal FROM cloud_game_reader_visits v WHERE v.account_id=inbox.account_id AND v.event_id=inbox.event_id),0),inbox.server_sequence LIMIT ?2")?;
    let mut items = statement
        .query_map(params![account, limit, after], |r| {
            Ok(MetadataInboxItem {
                event_id: r.get(0)?,
                server_sequence: r.get(1)?,
                source_device_id: r.get(2)?,
                project_id: r.get(3)?,
                entity_id: r.get(4)?,
                revision: r.get(5)?,
                updated_at: r.get(6)?,
                deleted_at: r.get(7)?,
                operation: r.get(8)?,
                crypto_version: r.get(9)?,
                aad_version: r.get(10)?,
                nonce: r.get(11)?,
                ciphertext: r.get(12)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    crate::sqlite::record_sync_reader_visits(
        &tx,
        account,
        items.iter().map(|i| i.event_id.as_str()),
    )?;
    items.sort_by_key(|i| i.server_sequence);
    tx.commit()?;
    Ok(items)
}

fn visible_metadata(connection: &Connection, project: &str) -> Result<Value, MetadataError> {
    let row: (String, Option<f64>, i64, String, String, String) = connection.query_row(
        "SELECT name,goal,infinite,unit,status,payload_json FROM projects WHERE id=?1",
        [project],
        |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        },
    )?;
    let payload: Value = serde_json::from_str(&row.5).map_err(|_| MetadataError::Invalid)?;
    let source = payload.as_object().ok_or(MetadataError::Invalid)?;
    let metadata = json!({
        "name":row.0,"goal":row.1,"infinite":row.2!=0,"unit":row.3,"status":row.4,
        "deadline":source.get("deadline").cloned().unwrap_or(Value::Null),
        "personal_goal":source.get("personal_goal").cloned().unwrap_or(json!(0)),
        "auto_freeze":source.get("auto_freeze").cloned().unwrap_or(json!(true)),
        "streak_enabled":source.get("streak_enabled").cloned().unwrap_or(json!(true)),
        "work_method":source.get("work_method").cloned().unwrap_or(json!("manual")),
        "stages_enabled":source.get("stages_enabled").cloned().unwrap_or(json!(false)),
        "combine_stage_mindmaps":source.get("combine_stage_mindmaps").cloned().unwrap_or(json!(false)),
    });
    let mut metadata = normalize_metadata_numbers(metadata);
    if let Some(reference)=source.get("cover_reference") {metadata["cover_reference"]=reference.clone();}
    else if let Some(raw)=connection.query_row("SELECT payload_json FROM cloud_sync_metadata_projection WHERE project_id=?1 AND deleted_at IS NULL",[project],|r|r.get::<_,String>(0)).optional()? {let prior:Value=serde_json::from_str(&raw).map_err(|_|MetadataError::Invalid)?;if let Some(reference)=prior.get("cover_reference"){metadata["cover_reference"]=reference.clone();}}
    validate_metadata(&metadata)?;
    Ok(metadata)
}

pub(crate) fn write_visible_metadata(
    tx: &rusqlite::Transaction<'_>,
    project: &str,
    metadata: &Value,
    now: &str,
) -> Result<(), MetadataError> {
    validate_metadata(metadata)?;
    let raw: String = tx.query_row(
        "SELECT payload_json FROM projects WHERE id=?1",
        [project],
        |r| r.get(0),
    )?;
    let mut payload: Value = serde_json::from_str(&raw).map_err(|_| MetadataError::Invalid)?;
    if ["unit", "goal", "infinite"]
        .iter()
        .any(|key| payload[*key] != metadata[*key])
    {
        let pending:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM cloud_progress_events WHERE project_id=?1 AND state IN ('unsealed','sealed'))",[project],|r|r.get(0))?;
        if pending {
            return Err(MetadataError::Conflict);
        }
    }
    let object = payload.as_object_mut().ok_or(MetadataError::Invalid)?;
    for key in FIELDS {
        object.insert(key.into(), metadata[key].clone());
    }
    if let Some(reference) = metadata.get("cover_reference") {
        let account: String = tx.query_row(
            "SELECT account_id FROM cloud_sync_project_bindings WHERE project_id=?1",
            [project],
            |r| r.get(0),
        )?;
        let projection =
            crate::project_cover_sync::require_material(tx, &account, project, reference)?;
        object.insert("cover_reference".into(), reference.clone());
        object.insert(
            "cover_image".into(),
            projection.map(Value::String).unwrap_or(Value::Null),
        );
    }
    let name = metadata["name"].as_str().ok_or(MetadataError::Invalid)?;
    let unit = metadata["unit"].as_str().ok_or(MetadataError::Invalid)?;
    let status = metadata["status"].as_str().ok_or(MetadataError::Invalid)?;
    let goal = metadata["goal"].as_f64();
    let changed = tx.execute(
        "UPDATE projects SET name=?1,goal=?2,infinite=?3,unit=?4,status=?5,payload_json=?6,updated_at=?7 WHERE id=?8",
        params![name,goal,i64::from(metadata["infinite"]==true),unit,status,payload.to_string(),now,project],
    )?;
    if changed != 1 {
        return Err(MetadataError::Scope);
    }
    Ok(())
}

#[derive(Serialize)]
pub(crate) struct MetadataAuthorityBranch {
    pub event_id: String,
    pub revision: i64,
    pub operation: String,
    pub metadata: Value,
    pub device_id: String,
    pub local_candidate: bool,
}

#[derive(Serialize)]
pub(crate) struct MetadataAuthorityView {
    pub state: String,
    pub local: Option<Value>,
    pub authenticated: Option<Value>,
    pub head_event_id: Option<String>,
    pub branches: Vec<MetadataAuthorityBranch>,
    pub pending_event_id: Option<String>,
    pub blockers: Value,
}

pub(crate) fn authority_view(
    connection: &Connection,
    account: &str,
    project: &str,
) -> Result<MetadataAuthorityView, MetadataError> {
    let migration = migration_status(connection, account, project)?;
    let local = visible_metadata(connection, project).ok();
    let mut statement = connection.prepare(
        "SELECT tip.event_id,event.revision,event.operation,event.payload_json,event.device_id,         (event.candidate_id IS NOT NULL AND event.device_id=(SELECT device_id FROM cloud_sync_state WHERE account_id=tip.account_id)) FROM cloud_sync_metadata_tips tip \
         JOIN cloud_sync_metadata_events event ON event.account_id=tip.account_id AND event.event_id=tip.event_id \
         WHERE tip.account_id=?1 AND tip.project_id=?2 ORDER BY tip.event_id",
    )?;
    let branches = statement
        .query_map(params![account, project], |r| {
            let raw: String = r.get(3)?;
            let metadata = serde_json::from_str(&raw).map_err(|_| rusqlite::Error::InvalidQuery)?;
            Ok(MetadataAuthorityBranch {
                event_id: r.get(0)?,
                revision: r.get(1)?,
                operation: r.get(2)?,
                metadata,
                device_id: r.get(4)?,
                local_candidate: r.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let projection: Option<(String,Value)> = connection.query_row(
        "SELECT head_event_id,payload_json FROM cloud_sync_metadata_projection WHERE account_id=?1 AND project_id=?2",
        params![account,project], |r| {
            let raw:String=r.get(1)?;
            Ok((r.get(0)?,serde_json::from_str(&raw).map_err(|_|rusqlite::Error::InvalidQuery)?))
        },
    ).optional()?;
    let pending:Option<String> = connection.query_row(
        "SELECT decision.event_id FROM cloud_sync_metadata_decisions decision JOIN cloud_sync_metadata_events event \
         ON event.account_id=decision.account_id AND event.event_id=decision.event_id \
         WHERE decision.account_id=?1 AND decision.project_id=?2 AND decision.state='pending' \
         AND event.state IN ('unsealed','sealed','accepted') ORDER BY decision.created_at LIMIT 1",
        params![account,project], |r|r.get(0),
    ).optional()?;
    let reconciled:Option<String> = connection.query_row(
        "SELECT head_event_id FROM cloud_sync_metadata_reconciliation WHERE account_id=?1 AND project_id=?2",
        params![account,project], |r|r.get(0),
    ).optional()?;
    let (head, authenticated) = if branches.len() == 1
        && projection
            .as_ref()
            .is_some_and(|(id, _)| id == &branches[0].event_id)
    {
        let (id, payload) = projection.ok_or(MetadataError::Invalid)?;
        (Some(id), Some(payload))
    } else {
        (None, None)
    };
    let pending_event_id = pending.or_else(|| {
        if branches.is_empty() {
            migration.event_id.clone()
        } else {
            None
        }
    });
    let state = if local.is_none()
        || migration.state.starts_with("blocked_")
        || migration.state == "apply_blocked"
    {
        "blocked"
    } else if pending_event_id.is_some() {
        "resolution_pending"
    } else if branches.len() > 1 {
        if branches
            .iter()
            .all(|branch| branch.operation == "create" && branch.revision == 1)
        {
            "genesis_conflict"
        } else {
            "metadata_conflict"
        }
    } else if let (Some(local), Some(cloud), Some(head)) = (&local, &authenticated, &head) {
        if local == cloud && reconciled.as_ref() == Some(head) {
            "active"
        } else if local == cloud {
            "local_matches_authenticated"
        } else {
            "local_differs_from_authenticated"
        }
    } else if migration.candidate_id.is_some() {
        "local_candidate_ready"
    } else if branches.len() == 1 {
        "blocked"
    } else {
        "local_legacy_only"
    };
    Ok(MetadataAuthorityView {
        state: state.into(),
        local,
        authenticated,
        head_event_id: head,
        branches,
        pending_event_id,
        blockers: migration.blockers,
    })
}

pub(crate) fn adopt_authenticated_metadata(
    connection: &mut Connection,
    account: &str,
    project: &str,
    expected_head: &str,
    expected_local: &Value,
    now: &str,
) -> Result<MetadataAuthorityView, MetadataError> {
    if !timestamp(now) {
        return Err(MetadataError::Invalid);
    }
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let view = authority_view(&tx, account, project)?;
    if view.head_event_id.as_deref() != Some(expected_head)
        || view.branches.len() != 1
        || view.pending_event_id.is_some()
        || view.local.as_ref() != Some(expected_local)
    {
        return Err(MetadataError::Conflict);
    }
    let cloud = view.authenticated.ok_or(MetadataError::Conflict)?;
    if cloud != *expected_local {
        write_visible_metadata(&tx, project, &cloud, now)?;
    }
    tx.execute("INSERT INTO cloud_sync_metadata_reconciliation(account_id,project_id,head_event_id,portable_json,reconciled_at) VALUES(?1,?2,?3,?4,?5) \
        ON CONFLICT(account_id,project_id) DO UPDATE SET head_event_id=excluded.head_event_id,portable_json=excluded.portable_json,reconciled_at=excluded.reconciled_at",
        params![account,project,expected_head,cloud.to_string(),now])?;
    tx.commit()?;
    authority_view(connection, account, project)
}

pub(crate) fn prepare_authoritative_change(
    connection: &mut Connection,
    account: &str,
    project: &str,
    device: &str,
    kind: &str,
    selected_event_id: Option<&str>,
    proposed: Option<&Value>,
    expected_local: &Value,
    expected_tips: &[String],
    now: &str,
) -> Result<String, MetadataError> {
    if !timestamp(now)
        || !matches!(
            kind,
            "keep_local" | "manual" | "edit" | "choose_branch" | "resolve_manual"
        )
    {
        return Err(MetadataError::Invalid);
    }
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let event_id = prepare_change_in_transaction(
        &tx,
        account,
        project,
        device,
        kind,
        selected_event_id,
        proposed,
        expected_local,
        expected_tips,
        now,
    )?;
    tx.commit()?;
    Ok(event_id)
}

pub(crate) fn prepare_change_in_transaction(
    tx: &rusqlite::Transaction<'_>,
    account: &str,
    project: &str,
    device: &str,
    kind: &str,
    selected_event_id: Option<&str>,
    proposed: Option<&Value>,
    expected_local: &Value,
    expected_tips: &[String],
    now: &str,
) -> Result<String, MetadataError> {
    let view = authority_view(&tx, account, project)?;
    if view.branches.is_empty() || view.state == "blocked" {
        return Err(MetadataError::Conflict);
    }
    let tips: Vec<String> = view
        .branches
        .iter()
        .map(|branch| branch.event_id.clone())
        .collect();
    if tips != expected_tips
        || expected_tips.len() > 64
        || expected_tips.windows(2).any(|pair| pair[0] >= pair[1])
    {
        return Err(MetadataError::Conflict);
    }
    let mut desired = match kind {
        "keep_local" => expected_local.clone(),
        "choose_branch" => view
            .branches
            .iter()
            .find(|branch| Some(branch.event_id.as_str()) == selected_event_id)
            .map(|branch| branch.metadata.clone())
            .ok_or(MetadataError::Invalid)?,
        _ => proposed.cloned().ok_or(MetadataError::Invalid)?,
    };
    // Resolving an older complete v1 branch against a v2 cover branch means
    // choosing its no-cover value. Keep the transition explicit and versioned;
    // an already published cover authority must not silently revert to v1.
    if desired.get("cover_reference").is_none()
        && view
            .branches
            .iter()
            .any(|branch| branch.metadata.get("cover_reference").is_some())
    {
        desired["cover_reference"] = Value::Null;
    }
    validate_metadata(&desired)?;
    let tip_json = serde_json::to_string(&tips).map_err(|_| MetadataError::Invalid)?;
    if let Some((pending_id,pending_kind,pending_source,pending_json,pending_tips))=tx.query_row(
        "SELECT decision.event_id,decision.kind,decision.source_json,decision.proposed_json,decision.expected_tips_json FROM cloud_sync_metadata_decisions decision \
         JOIN cloud_sync_metadata_events event ON event.account_id=decision.account_id AND event.event_id=decision.event_id \
         WHERE decision.account_id=?1 AND decision.project_id=?2 AND decision.state='pending' AND event.state IN ('unsealed','sealed','accepted')",
        params![account,project], |r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?)),
    ).optional()? {
        if pending_kind==kind && pending_source==expected_local.to_string()
            && pending_json==desired.to_string() && pending_tips==tip_json { return Ok(pending_id); }
        return Err(MetadataError::Conflict);
    }
    if view.local.as_ref() != Some(expected_local) {
        return Err(MetadataError::Conflict);
    }
    if kind == "edit" && view.state != "active" {
        return Err(MetadataError::Conflict);
    }
    if matches!(kind, "keep_local" | "manual" | "edit") && tips.len() != 1 {
        return Err(MetadataError::Conflict);
    }
    if matches!(kind, "choose_branch" | "resolve_manual") && tips.len() < 2 {
        return Err(MetadataError::Conflict);
    }
    let bound:Option<(String,String)>=tx.query_row(
        "SELECT state.device_id,boot.bootstrap_id FROM cloud_sync_state state JOIN cloud_sync_project_bindings binding ON binding.account_id=state.account_id \
         JOIN cloud_sync_project_bootstraps boot ON boot.account_id=binding.account_id AND boot.project_id=binding.project_id \
         WHERE state.account_id=?1 AND binding.project_id=?2",
        params![account,project],|r|Ok((r.get(0)?,r.get(1)?)),
    ).optional()?;
    let (bound_device, bootstrap) = bound.ok_or(MetadataError::Scope)?;
    if bound_device != device || !uuid(&bootstrap) {
        return Err(MetadataError::Scope);
    }
    let revision = view
        .branches
        .iter()
        .map(|branch| branch.revision)
        .max()
        .ok_or(MetadataError::Invalid)?
        .checked_add(1)
        .ok_or(MetadataError::Invalid)?;
    if revision > MAX_REVISION {
        return Err(MetadataError::Invalid);
    }
    let generation:i64=tx.query_row(
        "SELECT COALESCE(MAX(generation),0)+1 FROM cloud_sync_metadata_events WHERE account_id=?1 AND project_id=?2",
        params![account,project],|r|r.get(0),
    )?;
    if !(1..=MAX_REVISION).contains(&generation) {
        return Err(MetadataError::Invalid);
    }
    let operation = if tips.len() == 1 {
        "update"
    } else if view
        .branches
        .iter()
        .all(|branch| branch.operation == "create" && branch.revision == 1)
    {
        "genesis_resolution"
    } else {
        "resolution"
    };
    let event_id = new_event_id()?;
    let ordinal:i64=tx.query_row("SELECT COALESCE(MAX(local_ordinal),0)+1 FROM cloud_sync_outbox WHERE account_id=?1 AND device_id=?2",
        params![account,device],|r|r.get(0))?;
    if !(1..=MAX_REVISION).contains(&ordinal) {
        return Err(MetadataError::Invalid);
    }
    tx.execute("INSERT INTO cloud_sync_outbox(event_id,account_id,device_id,project_id,entity_id,entity_type,operation,revision,updated_at,created_at,parent_event_id,local_ordinal,lifecycle) VALUES(?1,?2,?3,?4,?4,'project_metadata','upsert',?5,?6,?6,?7,?8,'unsealed')",
        params![event_id,account,device,project,revision,now,if tips.len()==1 {Some(tips[0].as_str())} else {None},ordinal])?;
    tx.execute("INSERT INTO cloud_sync_metadata_events(account_id,event_id,project_id,device_id,bootstrap_id,candidate_id,parent_event_ids_json,generation,revision,operation,payload_json,state,created_at) VALUES(?1,?2,?3,?4,?5,NULL,?6,?7,?8,?9,?10,'unsealed',?11)",
        params![account,event_id,project,device,bootstrap,tip_json,generation,revision,operation,desired.to_string(),now])?;
    tx.execute("INSERT INTO cloud_sync_metadata_decisions(account_id,project_id,event_id,kind,source_json,proposed_json,expected_tips_json,state,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,'pending',?8)",
        params![account,project,event_id,kind,expected_local.to_string(),desired.to_string(),tip_json,now])?;
    if desired != *expected_local {
        write_visible_metadata(&tx, project, &desired, now)?;
    }
    Ok(event_id)
}

/// Intercept ordinary editors only when this project has authenticated metadata history.
/// Legacy/local projects retain their existing editor; an unresolved history fails closed.
pub(crate) fn capture_normal_edit(
    connection: &mut Connection,
    project: &str,
    payload: &Value,
    now: &str,
) -> Result<bool, MetadataError> {
    let binding:Option<(String,String)>=connection.query_row(
        "SELECT binding.account_id,state.device_id FROM cloud_sync_project_bindings binding JOIN cloud_sync_state state ON state.account_id=binding.account_id WHERE binding.project_id=?1",
        [project],|r|Ok((r.get(0)?,r.get(1)?)),
    ).optional()?;
    let Some((account, device)) = binding else {
        return Ok(false);
    };
    let history: i64 = connection.query_row(
        "SELECT COUNT(*) FROM cloud_sync_metadata_events WHERE account_id=?1 AND project_id=?2",
        params![account, project],
        |r| r.get(0),
    )?;
    if history == 0 {
        return Ok(false);
    };
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let view = authority_view(&tx, &account, project)?;
    let local = view.local.ok_or(MetadataError::Invalid)?;
    let source = payload.as_object().ok_or(MetadataError::Invalid)?;
    let mut desired = local.clone();
    for field in FIELDS {
        if let Some(value) = source.get(field) {
            desired[field] = value.clone();
        }
    }
    let desired = normalize_metadata_numbers(desired);
    validate_metadata(&desired)?;
    let old_cover: Option<String> = tx.query_row(
        "SELECT json_extract(payload_json,'$.cover_image') FROM projects WHERE id=?1",
        [project],
        |r| r.get(0),
    )?;
    let new_cover = source.get("cover_image").and_then(Value::as_str);
    let cover_changed = source.contains_key("cover_image")
        && old_cover.as_deref() != new_cover
        && view
            .authenticated
            .as_ref()
            .is_some_and(|m| m.get("cover_reference").is_some());
    if cover_changed {
        if view.state != "active" {
            return Err(MetadataError::Conflict);
        }
        let tips = view
            .branches
            .iter()
            .map(|b| b.event_id.clone())
            .collect::<Vec<_>>();
        crate::project_cover_sync::capture_in_tx(
            &tx, &account, project, &device, &local, &desired, new_cover, &tips, now,
        )?;
        // Keep ordinary local editor fields coherent while the new blob is
        // pending. The authenticated old reference remains unchanged until
        // prepare seals the causally bound metadata candidate.
        write_visible_metadata(&tx, project, &desired, now)?;
    } else if desired != local {
        if view.state != "active" {
            return Err(MetadataError::Conflict);
        };
        let tips: Vec<String> = view
            .branches
            .iter()
            .map(|branch| branch.event_id.clone())
            .collect();
        prepare_change_in_transaction(
            &tx,
            &account,
            project,
            &device,
            "edit",
            None,
            Some(&desired),
            &local,
            &tips,
            now,
        )?;
    }
    // The full payload came from the existing editor and contains device-only fields as well.
    tx.execute(
        "UPDATE projects SET payload_json=?1,updated_at=?2 WHERE id=?3",
        params![payload.to_string(), now, project],
    )?;
    tx.commit()?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{note_sync, sqlite};
    const ACCOUNT: &str = "local-account";
    const USER: &str = "123e4567-e89b-42d3-a456-426614174099";
    const DEVICE: &str = "123e4567-e89b-42d3-a456-426614174003";
    const BOOT: &str = "123e4567-e89b-42d3-a456-426614174002";
    const NOW: &str = "2026-09-21T00:00:00.000000Z";
    #[test]
    fn project_metadata_external_bindings_are_losslessly_local_only() {
        let (mut db, path) = database();
        db.execute("UPDATE projects SET payload_json=json_set(payload_json,'$.synch',json(?1),'$.last_synch',?2) WHERE id='project'",params![json!({"path":"/Users/C18_SECRET_PATH/book-private.docx","source_id":"SOURCE-ID-C18-LOCAL-ONLY"}).to_string(),"local-clock-only"]).unwrap();
        let cid = capture_legacy_candidate(&mut db, ACCOUNT, "project", NOW).unwrap();
        let (snapshot,unsupported,source):(String,String,String)=db.query_row("SELECT snapshot_json,unsupported_json,source_payload_json FROM cloud_sync_metadata_candidates WHERE candidate_id=?1",[cid],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
        assert_eq!(unsupported, "[]");
        assert!(!snapshot.contains("C18_SECRET_PATH"));
        assert!(!snapshot.contains("SOURCE-ID-C18-LOCAL-ONLY"));
        assert!(!snapshot.contains("last_synch"));
        assert!(source.contains("C18_SECRET_PATH"));
        assert!(db
            .query_row(
                "SELECT payload_json FROM projects WHERE id='project'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap()
            .contains("C18_SECRET_PATH"));
        drop(db);
        let _ = std::fs::remove_file(path);
    }
    #[test]
    fn cover_missing_restart_and_atomic_projection_rollback() {
        use crate::project_cover_sync as cover;
        use sha2::{Digest, Sha256};
        let (mut db, path) = database();
        let id = "123e4567-e89b-42d3-a456-426614174051";
        let blob = "123e4567-e89b-42d3-a456-426614174052";
        let envelope = json!({"aad_version":1,"crypto_version":1,"nonce":crate::note_sync::encode_canonical_base64url(&[0;24]),"ciphertext":crate::note_sync::encode_canonical_base64url(&[1;20])});
        let reference = json!({"version":1,"blob_id":blob,"crypto_version":1,"aad_version":1,"mime_type":"image/jpeg","plaintext_size":4,"key_fingerprint":"a".repeat(64),"envelope_sha256":format!("{:x}",Sha256::digest(envelope.to_string().as_bytes()))});
        let mut root: Value = serde_json::from_slice(&event(id, DEVICE, "Local name")).unwrap();
        root["version"] = json!(2);
        root["metadata"]["cover_reference"] = reference.clone();
        let payload = serde_json::to_vec(&root).unwrap();
        inbound(&db, id, DEVICE, 1);
        assert!(preserve_authenticated_event(&mut db, ACCOUNT, "project", &payload, NOW).is_err());
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM cloud_sync_metadata_events", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        let scope = MetadataScope {
            account_id: ACCOUNT.into(),
            canonical_user_id: USER.into(),
            device_id: DEVICE.into(),
        };
        cover::command(
            &mut db,
            &scope,
            "project",
            "block",
            &json!({"event_id":id,"reference":reference,"code":"cover_blob_missing"}),
            NOW,
        )
        .unwrap();
        drop(db);
        let mut db = sqlite::open_database(&path).unwrap();
        assert_eq!(
            cover::command(&mut db, &scope, "project", "status", &json!({}), NOW).unwrap()
                ["blockers"],
            json!(["cover_blob_missing"])
        );
        cover::command(&mut db,&scope,"project","material",&json!({"reference":reference,"nonce":vec![0;24],"ciphertext":vec![1;20],"jpeg":[255,216,255,217]}),NOW).unwrap();
        db.execute_batch("CREATE TRIGGER interrupted_cover_apply BEFORE UPDATE ON projects BEGIN SELECT RAISE(ABORT,'interrupted apply'); END;").unwrap();
        assert!(preserve_authenticated_event(&mut db, ACCOUNT, "project", &payload, NOW).is_err());
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM cloud_sync_metadata_events", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM cloud_cover_blockers", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        db.execute_batch("DROP TRIGGER interrupted_cover_apply;")
            .unwrap();
        assert_eq!(
            preserve_authenticated_event(&mut db, ACCOUNT, "project", &payload, NOW).unwrap(),
            "applied"
        );
        assert_eq!(
            authority_view(&db, ACCOUNT, "project")
                .unwrap()
                .authenticated
                .unwrap()["cover_reference"],
            reference
        );
        let local: String = db
            .query_row(
                "SELECT json_extract(payload_json,'$.cover_image') FROM projects",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(local, "data:image/jpeg;base64,/9j/2Q==");
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM cloud_cover_blockers", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        drop(db);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn cover_reader_rotation_is_bounded_and_survives_restart() {
        let (db, path) = database();
        for sequence in 1..=41 {
            let id = format!("123e4567-e89b-42d3-a456-{:012}", sequence + 100);
            inbound(&db, &id, DEVICE, sequence);
        }
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..6 {
            let reopened = sqlite::open_database(&path).unwrap();
            let page = received_metadata(&reopened, ACCOUNT, 8, 0).unwrap();
            assert!(page.len() <= 8);
            seen.extend(page.iter().map(|item| item.server_sequence));
        }
        assert!(seen.contains(&41));
        assert_eq!(seen.len(), 41);
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM cloud_game_reader_visits", [], |r| r
                .get::<_, i64>(
                0
            ))
            .unwrap(),
            41
        );
        assert_eq!(
            db.query_row("SELECT ack_cursor FROM cloud_sync_state", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        drop(db);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn cover_and_v1_name_change_share_full_tip_conflict_and_resolution() {
        let (mut db, path) = database();
        let ids = [
            "123e4567-e89b-42d3-a456-426614174061",
            "123e4567-e89b-42d3-a456-426614174062",
            "123e4567-e89b-42d3-a456-426614174063",
        ];
        inbound(&db, ids[0], DEVICE, 1);
        preserve_authenticated_event(
            &mut db,
            ACCOUNT,
            "project",
            &event(ids[0], DEVICE, "Local name"),
            NOW,
        )
        .unwrap();
        let renamed = causal_event(
            ids[1],
            DEVICE,
            "Changed name",
            "update",
            &[ids[0].into()],
            2,
            2,
        );
        inbound_causal(&db, ids[1], DEVICE, 2, 2);
        preserve_authenticated_event(&mut db, ACCOUNT, "project", &renamed, NOW).unwrap();
        let mut cover: Value = serde_json::from_slice(&causal_event(
            ids[2],
            DEVICE,
            "Local name",
            "update",
            &[ids[0].into()],
            2,
            2,
        ))
        .unwrap();
        cover["version"] = json!(2);
        cover["metadata"]["cover_reference"] = Value::Null;
        inbound_causal(&db, ids[2], DEVICE, 3, 2);
        preserve_authenticated_event(
            &mut db,
            ACCOUNT,
            "project",
            &serde_json::to_vec(&cover).unwrap(),
            NOW,
        )
        .unwrap();
        let view = authority_view(&db, ACCOUNT, "project").unwrap();
        assert_eq!(view.state, "metadata_conflict");
        let tips = view
            .branches
            .iter()
            .map(|branch| branch.event_id.clone())
            .collect::<Vec<_>>();
        prepare_authoritative_change(
            &mut db,
            ACCOUNT,
            "project",
            DEVICE,
            "choose_branch",
            Some(ids[1]),
            None,
            view.local.as_ref().unwrap(),
            &tips,
            NOW,
        )
        .unwrap();
        let pending = unsealed_genesis(&db, ACCOUNT, DEVICE).unwrap();
        let resolution = pending.last().unwrap();
        assert_eq!(resolution["version"], 2);
        assert_eq!(resolution["metadata"]["name"], "Changed name");
        assert!(resolution["metadata"]["cover_reference"].is_null());
        assert_eq!(resolution["header"]["parent_event_ids"], json!(tips));
        drop(db);
        let _ = std::fs::remove_file(path);
    }

    fn database() -> (Connection, std::path::PathBuf) {
        database_with_contents(false)
    }
    fn database_with_contents(with_note: bool) -> (Connection, std::path::PathBuf) {
        let path = std::env::temp_dir().join(format!(
            "c18-metadata-{}-{}.db",
            std::process::id(),
            new_event_id().unwrap()
        ));
        let db = sqlite::open_database(&path).unwrap();
        db.execute("INSERT INTO projects(id,name,infinite,unit,status,payload_json) VALUES('project','Local name',1,'symbols','active',?1)",
            [r#"{"deadline":null,"personal_goal":100,"auto_freeze":true,"streak_enabled":true,"work_method":"manual","stages_enabled":false,"combine_stage_mindmaps":false}"#]).unwrap();
        db.execute(
            "INSERT INTO project_order(project_id,position) VALUES('project',0)",
            [],
        )
        .unwrap();
        if with_note {
            db.execute(
                "INSERT INTO notes(id,project_id,payload_json) VALUES('note','project','{}')",
                [],
            )
            .unwrap();
        }
        db.execute("INSERT INTO cloud_sync_state(account_id,device_id,pull_cursor,ack_cursor,created_at,updated_at) VALUES(?1,?2,0,0,?3,?3)",params![ACCOUNT,DEVICE,NOW]).unwrap();
        db.execute("INSERT INTO cloud_account_bindings(local_account_id,canonical_user_id,created_at,validated_at) VALUES(?1,?2,?3,?3)",params![ACCOUNT,USER,NOW]).unwrap();
        db.execute("INSERT INTO cloud_sync_project_bindings(project_id,account_id,created_at,updated_at) VALUES('project',?1,?2,?2)",params![ACCOUNT,NOW]).unwrap();
        db.execute("INSERT INTO cloud_sync_project_bootstraps(project_id,account_id,device_id,bootstrap_id,mode,phase,created_at,updated_at) VALUES('project',?1,?2,?3,'upload_existing','prepared',?4,?4)",params![ACCOUNT,DEVICE,BOOT,NOW]).unwrap();
        (db, path)
    }
    fn event(id: &str, device: &str, name: &str) -> Vec<u8> {
        serde_json::to_vec(&json!({"version":1,"header":{"account_id":USER,"bootstrap_id":BOOT,"device_id":device,"entity_id":"project","event_id":id,"generation":1,"operation":"create","parent_event_ids":[],"project_id":"project","revision":1,"updated_at":NOW},"metadata":{"name":name,"goal":null,"infinite":true,"unit":"symbols","deadline":null,"status":"active","personal_goal":100,"auto_freeze":true,"streak_enabled":true,"work_method":"manual","stages_enabled":false,"combine_stage_mindmaps":false},"deleted_at":null})).unwrap()
    }
    fn causal_event(
        id: &str,
        device: &str,
        name: &str,
        operation: &str,
        parents: &[String],
        revision: i64,
        generation: i64,
    ) -> Vec<u8> {
        let mut value: Value = serde_json::from_slice(&event(id, device, name)).unwrap();
        value["header"]["operation"] = json!(operation);
        value["header"]["parent_event_ids"] = json!(parents);
        value["header"]["revision"] = json!(revision);
        value["header"]["generation"] = json!(generation);
        serde_json::to_vec(&value).unwrap()
    }
    fn inbound_causal(db: &Connection, id: &str, device: &str, sequence: i64, revision: i64) {
        db.execute("INSERT INTO cloud_sync_inbox(account_id,event_id,server_sequence,device_id,project_id,entity_id,entity_type,operation,sync_revision,updated_at,state,received_at) VALUES(?1,?2,?3,?4,'project','project','project_metadata','upsert',?5,?6,'received',?6)",params![ACCOUNT,id,sequence,device,revision,NOW]).unwrap();
        let exists:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_event_objects WHERE account_id=?1 AND event_id=?2)",params![ACCOUNT,id],|r|r.get(0)).unwrap();
        if !exists {
            db.execute("INSERT INTO cloud_sync_event_objects(account_id,event_id,crypto_version,aad_version,nonce,ciphertext,stored_at) VALUES(?1,?2,1,1,zeroblob(24),zeroblob(16),?3)",params![ACCOUNT,id,NOW]).unwrap();
        }
    }
    fn inbound(db: &Connection, id: &str, device: &str, sequence: i64) {
        db.execute("INSERT INTO cloud_sync_inbox(account_id,event_id,server_sequence,device_id,project_id,entity_id,entity_type,operation,sync_revision,updated_at,state,received_at) VALUES(?1,?2,?3,?4,'project','project','project_metadata','upsert',1,?5,'received',?5)",params![ACCOUNT,id,sequence,device,NOW]).unwrap();
        db.execute("INSERT INTO cloud_sync_event_objects(account_id,event_id,crypto_version,aad_version,nonce,ciphertext,stored_at) VALUES(?1,?2,1,1,zeroblob(24),zeroblob(16),?3)",params![ACCOUNT,id,NOW]).unwrap();
    }
    fn inbox_for_sealed(db: &Connection, id: &str, device: &str, sequence: i64) {
        db.execute("INSERT INTO cloud_sync_inbox(account_id,event_id,server_sequence,device_id,project_id,entity_id,entity_type,operation,sync_revision,updated_at,state,received_at) VALUES(?1,?2,?3,?4,'project','project','project_metadata','upsert',1,?5,'received',?5)",params![ACCOUNT,id,sequence,device,NOW]).unwrap();
        let exists:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_event_objects WHERE account_id=?1 AND event_id=?2)",params![ACCOUNT,id],|r|r.get(0)).unwrap();
        if !exists {
            let nonce = if device == DEVICE {
                [1_u8; 24]
            } else {
                [2_u8; 24]
            };
            let ciphertext = if device == DEVICE {
                [1_u8; 32]
            } else {
                [2_u8; 32]
            };
            db.execute("INSERT INTO cloud_sync_event_objects(account_id,event_id,crypto_version,aad_version,nonce,ciphertext,stored_at) VALUES(?1,?2,1,1,?3,?4,?5)",params![ACCOUNT,id,nonce.as_slice(),ciphertext.as_slice(),NOW]).unwrap();
        }
    }
    fn ack_candidate(db: &mut Connection) -> i64 {
        let device = db
            .query_row(
                "SELECT device_id FROM cloud_sync_state WHERE account_id=?1",
                [ACCOUNT],
                |r| r.get(0),
            )
            .unwrap();
        note_sync::prepare_note_sync_ack(
            db,
            &note_sync::PrepareNoteSyncAckCommand {
                account_id: ACCOUNT.into(),
                device_id: device,
                canonical_user_id: USER.into(),
            },
        )
        .unwrap()
        .candidate_cursor
    }
    #[test]
    fn project_metadata_lost_upload_response_self_echo_is_durable_and_ack_safe() {
        let (mut db, path) = database();
        let candidate = capture_legacy_candidate(&mut db, ACCOUNT, "project", NOW).unwrap();
        let id = prepare_metadata_genesis(&mut db, ACCOUNT, &candidate, NOW).unwrap();
        assert_eq!(unsealed_genesis(&db, ACCOUNT, DEVICE).unwrap().len(), 1);
        commit_sealed_genesis(&mut db, ACCOUNT, DEVICE, &id, &[0; 24], &[1; 32]).unwrap();
        assert_eq!(sealed_genesis(&db, ACCOUNT, DEVICE).unwrap().len(), 1);
        inbox_for_sealed(&db, &id, DEVICE, 1);
        db.execute(
            "UPDATE cloud_sync_state SET pull_cursor=1 WHERE account_id=?1",
            [ACCOUNT],
        )
        .unwrap();
        let bytes = event(&id, DEVICE, "Local name");
        assert_eq!(
            preserve_authenticated_event_checked(
                &mut db,
                ACCOUNT,
                "project",
                &bytes,
                NOW,
                Some((&[0; 24], &[1; 32]))
            )
            .unwrap(),
            "applied"
        );
        assert_eq!(ack_candidate(&mut db), 1);
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM cloud_sync_upload_receipts WHERE event_id=?1",
                [&id],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        drop(db);
        let mut reopened = sqlite::open_database(&path).unwrap();
        assert_eq!(
            preserve_authenticated_event_checked(
                &mut reopened,
                ACCOUNT,
                "project",
                &bytes,
                NOW,
                Some((&[0; 24], &[1; 32]))
            )
            .unwrap(),
            "applied"
        );
        assert!(matches!(
            preserve_authenticated_event_checked(
                &mut reopened,
                ACCOUNT,
                "project",
                &bytes,
                NOW,
                Some((&[9; 24], &[1; 32]))
            ),
            Err(MetadataError::Scope)
        ));
        assert_eq!(ack_candidate(&mut reopened), 1);
        assert!(received_metadata(&reopened, ACCOUNT, 8, 0)
            .unwrap()
            .is_empty());
        drop(reopened);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn project_metadata_two_device_genesis_survives_restart_without_winner() {
        let (mut a, path_a) = database();
        let (mut b, path_b) = database();
        let b_device = "123e4567-e89b-42d3-a456-426614174004";
        b.execute(
            "UPDATE cloud_sync_state SET device_id=?1 WHERE account_id=?2",
            params![b_device, ACCOUNT],
        )
        .unwrap();
        b.execute(
            "UPDATE projects SET name='Other name' WHERE id='project'",
            [],
        )
        .unwrap();
        let a_candidate = capture_legacy_candidate(&mut a, ACCOUNT, "project", NOW).unwrap();
        let b_candidate = capture_legacy_candidate(&mut b, ACCOUNT, "project", NOW).unwrap();
        let a_id = prepare_metadata_genesis(&mut a, ACCOUNT, &a_candidate, NOW).unwrap();
        let b_id = prepare_metadata_genesis(&mut b, ACCOUNT, &b_candidate, NOW).unwrap();
        assert_ne!(a_id, b_id);
        commit_sealed_genesis(&mut a, ACCOUNT, DEVICE, &a_id, &[1; 24], &[1; 32]).unwrap();
        commit_sealed_genesis(&mut b, ACCOUNT, b_device, &b_id, &[2; 24], &[2; 32]).unwrap();
        for db in [&a, &b] {
            inbox_for_sealed(db, &a_id, DEVICE, 1);
            inbox_for_sealed(db, &b_id, b_device, 2);
            db.execute(
                "UPDATE cloud_sync_state SET pull_cursor=2 WHERE account_id=?1",
                [ACCOUNT],
            )
            .unwrap();
        }
        // Each device applies its own self echo and preserves the independent peer genesis.
        for (db, own_id, own_device, own_name, peer_id, peer_device, peer_name) in [
            (
                &mut a,
                &a_id,
                DEVICE,
                "Local name",
                &b_id,
                b_device,
                "Other name",
            ),
            (
                &mut b,
                &b_id,
                b_device,
                "Other name",
                &a_id,
                DEVICE,
                "Local name",
            ),
        ] {
            let own = event(own_id, own_device, own_name);
            let peer = event(peer_id, peer_device, peer_name);
            assert_eq!(
                preserve_authenticated_event(db, ACCOUNT, "project", &own, NOW).unwrap(),
                "applied"
            );
            assert_eq!(
                preserve_authenticated_event(db, ACCOUNT, "project", &peer, NOW).unwrap(),
                "conflict_preserved"
            );
            assert_eq!(ack_candidate(db), 2);
            assert_eq!(
                db.query_row("SELECT COUNT(*) FROM cloud_sync_metadata_tips", [], |r| r
                    .get::<_, i64>(
                    0
                ))
                .unwrap(),
                2
            );
            assert_eq!(
                db.query_row("SELECT name FROM projects WHERE id='project'", [], |r| {
                    r.get::<_, String>(0)
                })
                .unwrap(),
                own_name
            );
        }
        drop(a);
        drop(b);
        for (path, device) in [(&path_a, DEVICE), (&path_b, b_device)] {
            let mut db = sqlite::open_database(path).unwrap();
            assert_eq!(
                migration_status(&db, ACCOUNT, "project").unwrap().state,
                "genesis_conflict"
            );
            assert_eq!(ack_candidate(&mut db), 2);
            assert_eq!(
                db.query_row("SELECT COUNT(*) FROM cloud_sync_metadata_tips", [], |r| r
                    .get::<_, i64>(
                    0
                ))
                .unwrap(),
                2
            );
            assert_runtime_scope(&db, ACCOUNT, USER, device).unwrap();
            drop(db);
            std::fs::remove_file(path).unwrap();
        }
    }
    #[test]
    fn project_metadata_explicit_adoption_preserves_device_only_fields_and_restarts() {
        let (mut db, path) = database();
        db.execute("UPDATE projects SET payload_json=json_set(payload_json,'$.cover_image','device-only') WHERE id='project'",[]).unwrap();
        let remote = "123e4567-e89b-42d3-a456-426614174071";
        let peer = "123e4567-e89b-42d3-a456-426614174004";
        inbound_causal(&db, remote, peer, 1, 1);
        assert_eq!(
            preserve_authenticated_event(
                &mut db,
                ACCOUNT,
                "project",
                &event(remote, peer, "Remote name"),
                NOW
            )
            .unwrap(),
            "applied"
        );
        let before = authority_view(&db, ACCOUNT, "project").unwrap();
        assert_eq!(before.state, "local_differs_from_authenticated");
        assert_eq!(
            db.query_row("SELECT name FROM projects WHERE id='project'", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap(),
            "Local name"
        );
        let after = adopt_authenticated_metadata(
            &mut db,
            ACCOUNT,
            "project",
            remote,
            before.local.as_ref().unwrap(),
            NOW,
        )
        .unwrap();
        assert_eq!(after.state, "active");
        assert_eq!(db.query_row("SELECT json_extract(payload_json,'$.cover_image') FROM projects WHERE id='project'",[],|r|r.get::<_,String>(0)).unwrap(),"device-only");
        drop(db);
        let reopened = sqlite::open_database(&path).unwrap();
        assert_eq!(
            authority_view(&reopened, ACCOUNT, "project").unwrap().state,
            "active"
        );
        drop(reopened);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn project_metadata_concurrent_rename_requires_full_tip_resolution() {
        let (mut db, path) = database();
        let parent = "123e4567-e89b-42d3-a456-426614174071";
        let peer = "123e4567-e89b-42d3-a456-426614174004";
        let gamma = "123e4567-e89b-42d3-a456-426614174073";
        inbound_causal(&db, parent, peer, 1, 1);
        preserve_authenticated_event(
            &mut db,
            ACCOUNT,
            "project",
            &event(parent, peer, "Project Alpha"),
            NOW,
        )
        .unwrap();
        let before = authority_view(&db, ACCOUNT, "project").unwrap();
        adopt_authenticated_metadata(
            &mut db,
            ACCOUNT,
            "project",
            parent,
            before.local.as_ref().unwrap(),
            NOW,
        )
        .unwrap();
        let active = authority_view(&db, ACCOUNT, "project").unwrap();
        let mut beta = active.local.clone().unwrap();
        beta["name"] = json!("Project Beta");
        let beta_id = prepare_authoritative_change(
            &mut db,
            ACCOUNT,
            "project",
            DEVICE,
            "edit",
            None,
            Some(&beta),
            active.local.as_ref().unwrap(),
            &[parent.into()],
            NOW,
        )
        .unwrap();
        assert_eq!(
            prepare_authoritative_change(
                &mut db,
                ACCOUNT,
                "project",
                DEVICE,
                "edit",
                None,
                Some(&beta),
                active.local.as_ref().unwrap(),
                &[parent.into()],
                NOW
            )
            .unwrap(),
            beta_id
        );
        let outbound = unsealed_genesis(&db, ACCOUNT, DEVICE).unwrap();
        assert_eq!(outbound[0]["header"]["operation"], "update");
        commit_sealed_genesis(&mut db, ACCOUNT, DEVICE, &beta_id, &[1; 24], &[1; 32]).unwrap();
        commit_upload_receipt(&mut db, ACCOUNT, DEVICE, &beta_id, 2, false, NOW).unwrap();
        inbound_causal(&db, &beta_id, DEVICE, 2, 2);
        assert_eq!(
            preserve_authenticated_event(
                &mut db,
                ACCOUNT,
                "project",
                outbound[0].to_string().as_bytes(),
                NOW
            )
            .unwrap(),
            "applied"
        );
        inbound_causal(&db, gamma, peer, 3, 2);
        assert_eq!(
            preserve_authenticated_event(
                &mut db,
                ACCOUNT,
                "project",
                &causal_event(
                    gamma,
                    peer,
                    "Project Gamma",
                    "update",
                    &[parent.into()],
                    2,
                    2
                ),
                NOW
            )
            .unwrap(),
            "conflict_preserved"
        );
        let conflict = authority_view(&db, ACCOUNT, "project").unwrap();
        assert_eq!(conflict.state, "metadata_conflict");
        assert_eq!(
            db.query_row("SELECT name FROM projects WHERE id='project'", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap(),
            "Project Beta"
        );
        let tips: Vec<String> = conflict
            .branches
            .iter()
            .map(|branch| branch.event_id.clone())
            .collect();
        assert!(prepare_authoritative_change(
            &mut db,
            ACCOUNT,
            "project",
            DEVICE,
            "choose_branch",
            Some(gamma),
            None,
            conflict.local.as_ref().unwrap(),
            &tips[..1],
            NOW
        )
        .is_err());
        let resolution = prepare_authoritative_change(
            &mut db,
            ACCOUNT,
            "project",
            DEVICE,
            "choose_branch",
            Some(gamma),
            None,
            conflict.local.as_ref().unwrap(),
            &tips,
            NOW,
        )
        .unwrap();
        let resolved = unsealed_genesis(&db, ACCOUNT, DEVICE).unwrap();
        assert_eq!(resolved[0]["header"]["operation"], "resolution");
        assert_eq!(resolved[0]["header"]["parent_event_ids"], json!(tips));
        commit_sealed_genesis(&mut db, ACCOUNT, DEVICE, &resolution, &[2; 24], &[2; 32]).unwrap();
        inbound_causal(&db, &resolution, DEVICE, 4, 3);
        assert_eq!(
            preserve_authenticated_event(
                &mut db,
                ACCOUNT,
                "project",
                resolved[0].to_string().as_bytes(),
                NOW
            )
            .unwrap(),
            "applied"
        );
        assert_eq!(
            authority_view(&db, ACCOUNT, "project").unwrap().state,
            "active"
        );
        assert_eq!(
            db.query_row("SELECT name FROM projects WHERE id='project'", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap(),
            "Project Gamma"
        );
        drop(db);
        let reopened = sqlite::open_database(&path).unwrap();
        assert_eq!(
            authority_view(&reopened, ACCOUNT, "project").unwrap().state,
            "active"
        );
        drop(reopened);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn project_metadata_adoption_and_edit_rollback_are_atomic() {
        let (mut db, path) = database();
        let parent = "123e4567-e89b-42d3-a456-426614174071";
        let peer = "123e4567-e89b-42d3-a456-426614174004";
        inbound_causal(&db, parent, peer, 1, 1);
        preserve_authenticated_event(
            &mut db,
            ACCOUNT,
            "project",
            &event(parent, peer, "Project Alpha"),
            NOW,
        )
        .unwrap();
        let before = authority_view(&db, ACCOUNT, "project").unwrap();
        db.execute_batch("CREATE TRIGGER reject_reconciliation BEFORE INSERT ON cloud_sync_metadata_reconciliation BEGIN SELECT RAISE(ABORT,'test failure'); END;").unwrap();
        assert!(adopt_authenticated_metadata(
            &mut db,
            ACCOUNT,
            "project",
            parent,
            before.local.as_ref().unwrap(),
            NOW
        )
        .is_err());
        assert_eq!(
            visible_metadata(&db, "project").unwrap(),
            before.local.unwrap()
        );
        db.execute_batch("DROP TRIGGER reject_reconciliation")
            .unwrap();
        let local = visible_metadata(&db, "project").unwrap();
        adopt_authenticated_metadata(&mut db, ACCOUNT, "project", parent, &local, NOW).unwrap();
        let active = visible_metadata(&db, "project").unwrap();
        let mut desired = active.clone();
        desired["name"] = json!("Project Beta");
        db.execute_batch("CREATE TRIGGER reject_decision BEFORE INSERT ON cloud_sync_metadata_decisions BEGIN SELECT RAISE(ABORT,'test failure'); END;").unwrap();
        assert!(capture_normal_edit(&mut db, "project", &desired, NOW).is_err());
        assert_eq!(visible_metadata(&db, "project").unwrap(), active);
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM cloud_sync_outbox", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        db.execute_batch("DROP TRIGGER reject_decision").unwrap();
        assert!(capture_normal_edit(&mut db, "project", &desired, NOW).unwrap());
        assert_eq!(
            authority_view(&db, ACCOUNT, "project").unwrap().state,
            "resolution_pending"
        );
        assert_eq!(
            unsealed_genesis(&db, ACCOUNT, DEVICE).unwrap()[0]["header"]["parent_event_ids"],
            json!([parent])
        );
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn project_metadata_peer_descendant_updates_only_previously_reconciled_shell() {
        let (mut db, path) = database();
        let parent = "123e4567-e89b-42d3-a456-426614174071";
        let child = "123e4567-e89b-42d3-a456-426614174072";
        let peer = "123e4567-e89b-42d3-a456-426614174004";
        inbound_causal(&db, parent, peer, 1, 1);
        preserve_authenticated_event(
            &mut db,
            ACCOUNT,
            "project",
            &event(parent, peer, "Project Alpha"),
            NOW,
        )
        .unwrap();
        let local = visible_metadata(&db, "project").unwrap();
        adopt_authenticated_metadata(&mut db, ACCOUNT, "project", parent, &local, NOW).unwrap();
        db.execute("UPDATE projects SET payload_json=json_set(payload_json,'$.cover_image','local','$.sync_path','private') WHERE id='project'",[]).unwrap();
        inbound_causal(&db, child, peer, 2, 2);
        let update = causal_event(
            child,
            peer,
            "Project Beta",
            "update",
            &[parent.into()],
            2,
            2,
        );
        preserve_authenticated_event(&mut db, ACCOUNT, "project", &update, NOW).unwrap();
        assert_eq!(
            authority_view(&db, ACCOUNT, "project").unwrap().state,
            "active"
        );
        assert_eq!(
            visible_metadata(&db, "project").unwrap()["name"],
            "Project Beta"
        );
        assert_eq!(
            db.query_row(
                "SELECT json_extract(payload_json,'$.sync_path') FROM projects",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "private"
        );
        drop(db);
        let mut db = sqlite::open_database(&path).unwrap();
        assert_eq!(
            preserve_authenticated_event(&mut db, ACCOUNT, "project", &update, NOW).unwrap(),
            "applied"
        );
        assert_eq!(
            visible_metadata(&db, "project").unwrap()["name"],
            "Project Beta"
        );
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn project_metadata_genesis_resolution_preserves_both_original_events() {
        let (mut db, path) = database();
        let first = "123e4567-e89b-42d3-a456-426614174071";
        let second = "123e4567-e89b-42d3-a456-426614174072";
        let peer = "123e4567-e89b-42d3-a456-426614174004";
        for (id, sequence, name) in [
            (first, 1, "First candidate"),
            (second, 2, "Second candidate"),
        ] {
            inbound_causal(&db, id, peer, sequence, 1);
            preserve_authenticated_event(&mut db, ACCOUNT, "project", &event(id, peer, name), NOW)
                .unwrap();
        }
        let conflict = authority_view(&db, ACCOUNT, "project").unwrap();
        assert_eq!(conflict.state, "genesis_conflict");
        assert_eq!(conflict.local.as_ref().unwrap()["name"], "Local name");
        let tips = conflict
            .branches
            .iter()
            .map(|branch| branch.event_id.clone())
            .collect::<Vec<_>>();
        let resolution = prepare_authoritative_change(
            &mut db,
            ACCOUNT,
            "project",
            DEVICE,
            "choose_branch",
            Some(second),
            None,
            conflict.local.as_ref().unwrap(),
            &tips,
            NOW,
        )
        .unwrap();
        let outbound = unsealed_genesis(&db, ACCOUNT, DEVICE).unwrap();
        assert_eq!(outbound[0]["header"]["operation"], "genesis_resolution");
        assert_eq!(outbound[0]["header"]["parent_event_ids"], json!(tips));
        commit_sealed_genesis(&mut db, ACCOUNT, DEVICE, &resolution, &[2; 24], &[2; 32]).unwrap();
        inbound_causal(&db, &resolution, DEVICE, 3, 2);
        preserve_authenticated_event(
            &mut db,
            ACCOUNT,
            "project",
            outbound[0].to_string().as_bytes(),
            NOW,
        )
        .unwrap();
        assert_eq!(
            authority_view(&db, ACCOUNT, "project").unwrap().state,
            "active"
        );
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM cloud_sync_metadata_events", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            3
        );
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM cloud_sync_metadata_apply_ledger",
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
    fn project_metadata_candidate_is_durable_and_reused() {
        let (mut db, path) = database();
        let first = capture_legacy_candidate(&mut db, ACCOUNT, "project", NOW).unwrap();
        assert_eq!(
            first,
            capture_legacy_candidate(&mut db, ACCOUNT, "project", NOW).unwrap()
        );
        drop(db);
        let mut db = sqlite::open_database(&path).unwrap();
        assert_eq!(
            first,
            capture_legacy_candidate(&mut db, ACCOUNT, "project", NOW).unwrap()
        );
        db.execute("UPDATE projects SET name='Changed' WHERE id='project'", [])
            .unwrap();
        let second = capture_legacy_candidate(&mut db, ACCOUNT, "project", NOW).unwrap();
        assert_ne!(first, second);
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM cloud_sync_metadata_candidates",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            2
        );
        let event_id = prepare_metadata_genesis(&mut db, ACCOUNT, &first, NOW).unwrap();
        assert_eq!(
            event_id,
            prepare_metadata_genesis(&mut db, ACCOUNT, &first, NOW).unwrap()
        );
        assert!(matches!(
            prepare_metadata_genesis(&mut db, ACCOUNT, &second, NOW),
            Err(MetadataError::Conflict)
        ));
        drop(db);
        let mut db = sqlite::open_database(&path).unwrap();
        assert_eq!(
            event_id,
            prepare_metadata_genesis(&mut db, ACCOUNT, &first, NOW).unwrap()
        );
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM cloud_sync_outbox WHERE entity_type='project_metadata'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        db.execute("UPDATE projects SET payload_json=json_set(payload_json,'$.future_user_field','private') WHERE id='project'",[]).unwrap();
        let unsupported = capture_legacy_candidate(&mut db, ACCOUNT, "project", NOW).unwrap();
        assert!(matches!(
            prepare_metadata_genesis(&mut db, ACCOUNT, &unsupported, NOW),
            Err(MetadataError::Scope)
        ));
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn project_metadata_legacy_binding_without_bootstrap_is_preserved_but_not_publishable() {
        let (mut db, path) = database();
        db.execute(
            "DELETE FROM cloud_sync_project_bootstraps WHERE project_id='project'",
            [],
        )
        .unwrap();
        let candidate = capture_legacy_candidate(&mut db, ACCOUNT, "project", NOW).unwrap();
        let (bootstrap,unsupported): (Option<String>,String) = db.query_row(
            "SELECT bootstrap_id,unsupported_json FROM cloud_sync_metadata_candidates WHERE candidate_id=?1",
            [&candidate],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
        assert!(bootstrap.is_none());
        assert_eq!(unsupported, r#"["bootstrap_lineage_missing"]"#);
        assert!(matches!(
            prepare_metadata_genesis(&mut db, ACCOUNT, &candidate, NOW),
            Err(MetadataError::Scope)
        ));
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn project_metadata_two_parentless_genesis_preserves_both_without_winner() {
        let (mut db, path) = database();
        let id1 = "123e4567-e89b-42d3-a456-426614174010";
        let id2 = "123e4567-e89b-42d3-a456-426614174011";
        let device2 = "123e4567-e89b-42d3-a456-426614174004";
        inbound(&db, id1, DEVICE, 1);
        let first = event(id1, DEVICE, "First");
        assert_eq!(
            preserve_authenticated_event(&mut db, ACCOUNT, "project", &first, NOW).unwrap(),
            "applied"
        );
        assert_eq!(
            preserve_authenticated_event(&mut db, ACCOUNT, "project", &first, NOW).unwrap(),
            "applied"
        );
        inbound(&db, id2, device2, 2);
        let second = event(id2, device2, "Second");
        assert_eq!(
            preserve_authenticated_event(&mut db, ACCOUNT, "project", &second, NOW).unwrap(),
            "conflict_preserved"
        );
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM cloud_sync_metadata_tips", [], |r| r
                .get::<_, i64>(
                0
            ))
            .unwrap(),
            2
        );
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM cloud_sync_metadata_projection",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        assert!(matches!(
            preserve_authenticated_event(
                &mut db,
                ACCOUNT,
                "project",
                &event(id1, DEVICE, "Tampered"),
                NOW
            ),
            Err(MetadataError::Conflict)
        ));
        assert_eq!(
            db.query_row("SELECT name FROM projects WHERE id='project'", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap(),
            "Local name"
        );
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn project_metadata_self_echo_requires_receipt_and_never_rewrites_visible_project() {
        let (mut db, path) = database();
        let candidate = capture_legacy_candidate(&mut db, ACCOUNT, "project", NOW).unwrap();
        let id = prepare_metadata_genesis(&mut db, ACCOUNT, &candidate, NOW).unwrap();
        inbound(&db, &id, DEVICE, 1);
        let payload = event(&id, DEVICE, "Local name");
        assert!(matches!(
            preserve_authenticated_event(&mut db, ACCOUNT, "project", &payload, NOW),
            Err(MetadataError::Scope)
        ));
        db.execute(
            "UPDATE cloud_sync_outbox SET lifecycle='sealed' WHERE event_id=?1",
            [&id],
        )
        .unwrap();
        db.execute("INSERT INTO cloud_sync_upload_receipts(account_id,event_id,device_id,server_sequence,duplicate,accepted_at) VALUES(?1,?2,?3,1,0,?4)",params![ACCOUNT,id,DEVICE,NOW]).unwrap();
        assert_eq!(
            preserve_authenticated_event(&mut db, ACCOUNT, "project", &payload, NOW).unwrap(),
            "applied"
        );
        assert_eq!(
            preserve_authenticated_event(&mut db, ACCOUNT, "project", &payload, NOW).unwrap(),
            "applied"
        );
        assert_eq!(
            db.query_row("SELECT name FROM projects WHERE id='project'", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap(),
            "Local name"
        );
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn project_metadata_malformed_scope_and_orphan_roll_back() {
        let (mut db, path) = database();
        let id = "123e4567-e89b-42d3-a456-426614174012";
        inbound(&db, id, DEVICE, 1);
        let mut value: Value = serde_json::from_slice(&event(id, DEVICE, "Good")).unwrap();
        value["metadata"]["secret"] = json!("leak");
        assert!(matches!(
            preserve_authenticated_event(
                &mut db,
                ACCOUNT,
                "project",
                &serde_json::to_vec(&value).unwrap(),
                NOW
            ),
            Err(MetadataError::Invalid)
        ));
        let mut value: Value = serde_json::from_slice(&event(id, DEVICE, "Good")).unwrap();
        value["header"]["account_id"] = json!("other");
        assert!(matches!(
            preserve_authenticated_event(
                &mut db,
                ACCOUNT,
                "project",
                &serde_json::to_vec(&value).unwrap(),
                NOW
            ),
            Err(MetadataError::Scope)
        ));
        value["header"]["account_id"] = json!(USER);
        value["header"]["operation"] = json!("update");
        value["header"]["revision"] = json!(2);
        value["header"]["generation"] = json!(2);
        value["header"]["parent_event_ids"] = json!(["123e4567-e89b-42d3-a456-426614174099"]);
        db.execute(
            "UPDATE cloud_sync_inbox SET sync_revision=2 WHERE event_id=?1",
            [id],
        )
        .unwrap();
        let orphan_bytes = serde_json::to_vec(&value).unwrap();
        assert_eq!(
            preserve_authenticated_event(&mut db, ACCOUNT, "project", &orphan_bytes, NOW).unwrap(),
            "orphan"
        );
        assert_eq!(
            preserve_authenticated_event(&mut db, ACCOUNT, "project", &orphan_bytes, NOW).unwrap(),
            "orphan"
        );
        assert_eq!(
            db.query_row(
                "SELECT state FROM cloud_sync_inbox WHERE event_id=?1",
                [id],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "orphan"
        );
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM cloud_sync_metadata_apply_ledger",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        drop(db);
        let mut db = sqlite::open_database(&path).unwrap();
        inbound(&db, USER, DEVICE, 2);
        assert_eq!(
            preserve_authenticated_event(
                &mut db,
                ACCOUNT,
                "project",
                &event(USER, DEVICE, "Parent"),
                NOW
            )
            .unwrap(),
            "applied"
        );
        assert_eq!(
            preserve_authenticated_event(&mut db, ACCOUNT, "project", &orphan_bytes, NOW).unwrap(),
            "applied"
        );
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM cloud_sync_metadata_tips", [], |r| r
                .get::<_, i64>(
                0
            ))
            .unwrap(),
            1
        );
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn project_metadata_edge_resolution_invalidation_all_durable_timings() {
        for timing in 0..5 {
            let (mut db, path) = database();
            let peer = "123e4567-e89b-42d3-a456-426614174004";
            let a = "123e4567-e89b-42d3-a456-426614174081";
            let b = "123e4567-e89b-42d3-a456-426614174082";
            let c = "123e4567-e89b-42d3-a456-426614174083";
            for (id, seq) in [(a, 1), (b, 2)] {
                inbound(&db, id, peer, seq);
                preserve_authenticated_event(
                    &mut db,
                    ACCOUNT,
                    "project",
                    &event(id, peer, id),
                    NOW,
                )
                .unwrap();
            }
            let view = authority_view(&db, ACCOUNT, "project").unwrap();
            let tips = view
                .branches
                .iter()
                .map(|b| b.event_id.clone())
                .collect::<Vec<_>>();
            let r = prepare_authoritative_change(
                &mut db,
                ACCOUNT,
                "project",
                DEVICE,
                "choose_branch",
                Some(b),
                None,
                view.local.as_ref().unwrap(),
                &tips,
                NOW,
            )
            .unwrap();
            let frame = unsealed_genesis(&db, ACCOUNT, DEVICE).unwrap()[0]
                .to_string()
                .into_bytes();
            drop(db);
            db = sqlite::open_database(&path).unwrap();
            if timing > 0 {
                commit_sealed_genesis(&mut db, ACCOUNT, DEVICE, &r, &[1; 24], &[1; 32]).unwrap();
            }
            let r_sequence = if timing == 4 { 3 } else { 4 };
            let c_sequence = if timing == 4 { 4 } else { 3 };
            // R may precede C in server order while C is observed before R's echo.
            if timing >= 3 {
                commit_upload_receipt(&mut db, ACCOUNT, DEVICE, &r, r_sequence, false, NOW)
                    .unwrap();
            }
            drop(db);
            db = sqlite::open_database(&path).unwrap();
            inbound(&db, c, peer, c_sequence);
            preserve_authenticated_event(&mut db, ACCOUNT, "project", &event(c, peer, "C"), NOW)
                .unwrap();
            assert_eq!(
                authority_view(&db, ACCOUNT, "project").unwrap().state,
                "genesis_conflict"
            );
            assert_eq!(
                db.query_row(
                    "SELECT state FROM cloud_sync_metadata_decisions WHERE event_id=?1",
                    [&r],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
                "conflict"
            );
            assert!(unsealed_genesis(&db, ACCOUNT, DEVICE).unwrap().is_empty());
            assert!(sealed_genesis(&db, ACCOUNT, DEVICE).unwrap().is_empty());
            drop(db);
            db = sqlite::open_database(&path).unwrap();
            if timing == 0 {
                assert!(
                    commit_sealed_genesis(&mut db, ACCOUNT, DEVICE, &r, &[1; 24], &[1; 32])
                        .is_err()
                );
            }
            if timing >= 2 {
                inbound_causal(&db, &r, DEVICE, r_sequence, 2);
                assert_eq!(
                    preserve_authenticated_event(&mut db, ACCOUNT, "project", &frame, NOW).unwrap(),
                    "conflict_preserved"
                );
                assert_eq!(
                    preserve_authenticated_event(&mut db, ACCOUNT, "project", &frame, NOW).unwrap(),
                    "conflict_preserved"
                );
                let ids = authority_view(&db, ACCOUNT, "project")
                    .unwrap()
                    .branches
                    .into_iter()
                    .map(|b| b.event_id)
                    .collect::<Vec<_>>();
                assert_eq!(ids.len(), 2);
                assert!(ids.contains(&r) && ids.contains(&c.to_string()));
            }
            let current = authority_view(&db, ACCOUNT, "project").unwrap();
            let tips = current
                .branches
                .iter()
                .map(|b| b.event_id.clone())
                .collect::<Vec<_>>();
            assert!(prepare_authoritative_change(
                &mut db,
                ACCOUNT,
                "project",
                DEVICE,
                "choose_branch",
                Some(c),
                None,
                current.local.as_ref().unwrap(),
                &tips[..1],
                NOW
            )
            .is_err());
            let r2 = prepare_authoritative_change(
                &mut db,
                ACCOUNT,
                "project",
                DEVICE,
                "choose_branch",
                Some(c),
                None,
                current.local.as_ref().unwrap(),
                &tips,
                NOW,
            )
            .unwrap();
            assert_ne!(r, r2);
            let final_frame = unsealed_genesis(&db, ACCOUNT, DEVICE).unwrap()[0]
                .to_string()
                .into_bytes();
            let revision = serde_json::from_slice::<Value>(&final_frame).unwrap()["header"]
                ["revision"]
                .as_i64()
                .unwrap();
            commit_sealed_genesis(&mut db, ACCOUNT, DEVICE, &r2, &[2; 24], &[2; 32]).unwrap();
            let seq = if timing >= 2 { 5 } else { 4 };
            inbound_causal(&db, &r2, DEVICE, seq, revision);
            assert_eq!(
                preserve_authenticated_event(&mut db, ACCOUNT, "project", &final_frame, NOW)
                    .unwrap(),
                "applied"
            );
            db.execute("UPDATE cloud_sync_state SET pull_cursor=?1", [seq])
                .unwrap();
            assert_eq!(ack_candidate(&mut db), seq);
            assert_eq!(
                authority_view(&db, ACCOUNT, "project").unwrap().state,
                "active"
            );
            assert_eq!(
                db.query_row(
                    "SELECT COUNT(*) FROM cloud_sync_metadata_invalidated_decisions",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
                1
            );
            drop(db);
            let reopened = sqlite::open_database(&path).unwrap();
            assert_eq!(
                authority_view(&reopened, ACCOUNT, "project").unwrap().state,
                "active"
            );
            drop(reopened);
            std::fs::remove_file(path).unwrap();
        }
    }
    #[test]
    fn project_metadata_edge_import_pages_resume_replay_and_bounds() {
        let (mut db, path) = database();
        let mut parent = String::new();
        for seq in 1..=35 {
            let id = format!("123e4567-e89b-42d3-a456-{:012}", seq + 100);
            let bytes = if seq == 1 {
                event(&id, DEVICE, "Import")
            } else {
                causal_event(&id, DEVICE, "Import", "update", &[parent], seq, seq)
            };
            let page = MetadataImportPage {
                expected_cursor: seq - 1,
                next_cursor: seq,
                has_more: true,
                page_events: 1,
                page_identity: format!("{:064x}", seq),
                events: vec![MetadataImportEvent {
                    server_sequence: seq,
                    plaintext: bytes,
                }],
            };
            let state = commit_metadata_import_page(&mut db, ACCOUNT, USER, "project", BOOT, &page)
                .unwrap();
            assert_eq!(state.cursor, seq);
            assert!(state.head.is_none());
            drop(db);
            db = sqlite::open_database(&path).unwrap();
            let replay =
                commit_metadata_import_page(&mut db, ACCOUNT, USER, "project", BOOT, &page)
                    .unwrap();
            assert_eq!(replay.event_count, seq);
            assert_eq!(replay.cursor, seq);
            parent = id;
        }
        let end = MetadataImportPage {
            expected_cursor: 35,
            next_cursor: 35,
            has_more: false,
            page_events: 0,
            page_identity: "0".repeat(64),
            events: vec![],
        };
        assert_eq!(
            commit_metadata_import_page(&mut db, ACCOUNT, USER, "project", BOOT, &end)
                .unwrap()
                .head,
            Some(parent)
        );
        let incomplete = MetadataImportPage {
            expected_cursor: 35,
            next_cursor: 36,
            has_more: false,
            page_events: 1,
            page_identity: "a".repeat(64),
            events: vec![MetadataImportEvent {
                server_sequence: 36,
                plaintext: causal_event(
                    "123e4567-e89b-42d3-a456-426614174666",
                    DEVICE,
                    "Bad",
                    "update",
                    &["123e4567-e89b-42d3-a456-426614174999".into()],
                    2,
                    2,
                ),
            }],
        };
        assert!(
            commit_metadata_import_page(&mut db, ACCOUNT, USER, "project", BOOT, &incomplete)
                .is_err()
        );
        assert_eq!(
            read_metadata_import(&mut db, ACCOUNT, "project", BOOT)
                .unwrap()
                .cursor,
            35
        );
        db.execute(
            "UPDATE cloud_sync_metadata_imports SET event_count=3200 WHERE account_id=?1",
            [ACCOUNT],
        )
        .unwrap();
        let blocked =
            commit_metadata_import_page(&mut db, ACCOUNT, USER, "project", BOOT, &incomplete)
                .unwrap();
        assert_eq!(blocked.state, "blocked");
        assert!(blocked.head.is_none());
        assert_eq!(blocked.cursor, 35);
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM cloud_sync_metadata_import_events",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            35
        );
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn project_metadata_acceptance_all_fields_peer_apply_rollback_and_restart() {
        let (mut db, path) = database_with_contents(true);
        let peer = "123e4567-e89b-42d3-a456-426614174004";
        let parent = "123e4567-e89b-42d3-a456-426614174071";
        let child = "123e4567-e89b-42d3-a456-426614174072";
        db.execute("UPDATE projects SET payload_json=json_set(payload_json,'$.sync_path','/private/book.docx','$.cover_image','local-cover','$.extensions',json('{\"local\":true}'))", []).unwrap();
        inbound_causal(&db, parent, peer, 1, 1);
        preserve_authenticated_event(
            &mut db,
            ACCOUNT,
            "project",
            &event(parent, peer, "Base"),
            NOW,
        )
        .unwrap();
        let local = visible_metadata(&db, "project").unwrap();
        adopt_authenticated_metadata(&mut db, ACCOUNT, "project", parent, &local, NOW).unwrap();
        let before = visible_metadata(&db, "project").unwrap();
        let desired = json!({"name":"All fields","goal":2500.5,"infinite":false,"unit":"A4","deadline":"2027-01-02","status":"заморожен","personal_goal":125.5,"auto_freeze":false,"streak_enabled":false,"work_method":"app","stages_enabled":true,"combine_stage_mindmaps":true});
        let mut update: Value = serde_json::from_slice(&causal_event(
            child,
            peer,
            "All fields",
            "update",
            &[parent.into()],
            2,
            2,
        ))
        .unwrap();
        update["metadata"] = desired.clone();
        let bytes = serde_json::to_vec(&update).unwrap();
        inbound_causal(&db, child, peer, 2, 2);
        db.execute("UPDATE cloud_sync_state SET pull_cursor=2", [])
            .unwrap();
        // The inbox is durable before apply. A failure at proof commit must undo
        // the visible write, event/tip changes and ACK evidence together.
        db.execute_batch("CREATE TRIGGER reject_peer_proof BEFORE UPDATE ON cloud_sync_metadata_reconciliation BEGIN SELECT RAISE(ABORT,'test failure'); END;").unwrap();
        assert!(preserve_authenticated_event(&mut db, ACCOUNT, "project", &bytes, NOW).is_err());
        assert_eq!(visible_metadata(&db, "project").unwrap(), before);
        assert_eq!(ack_candidate(&mut db), 1);
        assert_eq!(
            db.query_row(
                "SELECT state FROM cloud_sync_inbox WHERE event_id=?1",
                [child],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "received"
        );
        db.execute_batch("DROP TRIGGER reject_peer_proof").unwrap();
        drop(db);
        let mut db = sqlite::open_database(&path).unwrap();
        assert_eq!(
            preserve_authenticated_event(&mut db, ACCOUNT, "project", &bytes, NOW).unwrap(),
            "applied"
        );
        assert_eq!(visible_metadata(&db, "project").unwrap(), desired);
        assert_eq!(ack_candidate(&mut db), 2);
        let raw: String = db
            .query_row("SELECT payload_json FROM projects", [], |r| r.get(0))
            .unwrap();
        let payload: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(payload["sync_path"], "/private/book.docx");
        assert_eq!(payload["cover_image"], "local-cover");
        assert_eq!(payload["extensions"], json!({"local":true}));
        assert!(!String::from_utf8(bytes.clone())
            .unwrap()
            .contains("/private/"));
        drop(db);
        let mut db = sqlite::open_database(&path).unwrap();
        assert_eq!(
            preserve_authenticated_event(&mut db, ACCOUNT, "project", &bytes, NOW).unwrap(),
            "applied"
        );
        assert_eq!(visible_metadata(&db, "project").unwrap(), desired);
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM notes", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        let next = json!({"name":"Local edit","goal":3000,"infinite":true,"unit":"symbols","deadline":null,"status":"active","personal_goal":200,"auto_freeze":true,"streak_enabled":true,"work_method":"manual","stages_enabled":false,"combine_stage_mindmaps":false});
        assert!(capture_normal_edit(&mut db, "project", &next, NOW).unwrap());
        let outbox = unsealed_genesis(&db, ACCOUNT, DEVICE).unwrap();
        assert_eq!(outbox.len(), 1);
        assert_eq!(outbox[0]["metadata"], next);
        assert_eq!(outbox[0]["header"]["parent_event_ids"], json!([child]));
        let pending_id = outbox[0]["header"]["event_id"].clone();
        drop(db);
        let db = sqlite::open_database(&path).unwrap();
        assert_eq!(visible_metadata(&db, "project").unwrap(), next);
        assert_eq!(
            unsealed_genesis(&db, ACCOUNT, DEVICE).unwrap()[0]["header"]["event_id"],
            pending_id
        );
        drop(db);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn project_metadata_acceptance_scope_isolation_and_foreign_parent() {
        let (mut db, path) = database();
        let peer = "123e4567-e89b-42d3-a456-426614174004";
        let id = "123e4567-e89b-42d3-a456-426614174071";
        let bytes = event(id, peer, "Remote");
        let before = visible_metadata(&db, "project").unwrap();
        inbound_causal(&db, id, peer, 1, 1);
        assert!(
            preserve_authenticated_event(&mut db, "other-account", "project", &bytes, NOW).is_err()
        );
        assert!(
            preserve_authenticated_event(&mut db, ACCOUNT, "other-project", &bytes, NOW).is_err()
        );
        for key in [
            "account_id",
            "project_id",
            "entity_id",
            "bootstrap_id",
            "event_id",
        ] {
            let mut invalid: Value = serde_json::from_slice(&bytes).unwrap();
            invalid["header"][key] = json!("123e4567-e89b-42d3-a456-426614174088");
            assert!(
                preserve_authenticated_event(
                    &mut db,
                    ACCOUNT,
                    "project",
                    &serde_json::to_vec(&invalid).unwrap(),
                    NOW
                )
                .is_err(),
                "{key}"
            );
        }
        assert_eq!(visible_metadata(&db, "project").unwrap(), before);
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM cloud_sync_metadata_apply_ledger",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        preserve_authenticated_event(&mut db, ACCOUNT, "project", &bytes, NOW).unwrap();
        let candidate = capture_legacy_candidate(&mut db, ACCOUNT, "project", NOW).unwrap();
        assert!(prepare_metadata_genesis(&mut db, "other-account", &candidate, NOW).is_err());
        assert!(adopt_authenticated_metadata(
            &mut db,
            "other-account",
            "project",
            id,
            &before,
            NOW
        )
        .is_err());
        assert!(assert_runtime_scope(&db, ACCOUNT, peer, DEVICE).is_err());
        assert!(assert_runtime_scope(&db, ACCOUNT, USER, peer).is_err());
        assert!(prepare_authoritative_change(
            &mut db,
            "other-account",
            "project",
            DEVICE,
            "keep_local",
            None,
            None,
            &before,
            &[id.into()],
            NOW
        )
        .is_err());
        assert!(prepare_authoritative_change(
            &mut db,
            ACCOUNT,
            "other-project",
            DEVICE,
            "keep_local",
            None,
            None,
            &before,
            &[id.into()],
            NOW
        )
        .is_err());
        let mut foreign: Value = serde_json::from_slice(&causal_event(
            "123e4567-e89b-42d3-a456-426614174072",
            peer,
            "Foreign",
            "update",
            &[id.into()],
            2,
            2,
        ))
        .unwrap();
        foreign["header"]["project_id"] = json!("other-project");
        foreign["header"]["entity_id"] = json!("other-project");
        let page = MetadataImportPage {
            expected_cursor: 0,
            next_cursor: 2,
            has_more: false,
            page_events: 1,
            page_identity: "a".repeat(64),
            events: vec![MetadataImportEvent {
                server_sequence: 2,
                plaintext: serde_json::to_vec(&foreign).unwrap(),
            }],
        };
        assert!(
            commit_metadata_import_page(&mut db, ACCOUNT, USER, "other-project", BOOT, &page)
                .is_err()
        );
        let progress = read_metadata_import(&mut db, ACCOUNT, "other-project", BOOT).unwrap();
        assert_eq!(progress.cursor, 0);
        assert!(progress.head.is_none());
        let wrong_user_page = MetadataImportPage {
            events: vec![MetadataImportEvent {
                server_sequence: 1,
                plaintext: bytes,
            }],
            ..page
        };
        assert!(commit_metadata_import_page(
            &mut db,
            ACCOUNT,
            peer,
            "project",
            BOOT,
            &wrong_user_page
        )
        .is_err());
        assert!(read_metadata_import(&mut db, ACCOUNT, "project", peer).is_err());
        assert_eq!(visible_metadata(&db, "project").unwrap(), before);
        drop(db);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn project_metadata_acceptance_tombstone_preserves_contents_and_mixed_ack() {
        let (mut db, path) = database_with_contents(true);
        let peer = "123e4567-e89b-42d3-a456-426614174004";
        let parent = "123e4567-e89b-42d3-a456-426614174071";
        let deletion = "123e4567-e89b-42d3-a456-426614174072";
        inbound_causal(&db, parent, peer, 1, 1);
        preserve_authenticated_event(
            &mut db,
            ACCOUNT,
            "project",
            &event(parent, peer, "Base"),
            NOW,
        )
        .unwrap();
        let local = visible_metadata(&db, "project").unwrap();
        adopt_authenticated_metadata(&mut db, ACCOUNT, "project", parent, &local, NOW).unwrap();
        let before = visible_metadata(&db, "project").unwrap();
        // A malformed intervening Note blocks the shared contiguous prefix.
        db.execute("INSERT INTO cloud_sync_inbox(account_id,event_id,server_sequence,device_id,project_id,entity_id,entity_type,operation,sync_revision,updated_at,state,received_at) VALUES(?1,'123e4567-e89b-42d3-a456-426614174073',2,?2,'project','note','note','upsert',1,?3,'received',?3)", params![ACCOUNT,peer,NOW]).unwrap();
        inbound_causal(&db, deletion, peer, 3, 2);
        db.execute(
            "UPDATE cloud_sync_inbox SET operation='delete',deleted_at=?1 WHERE event_id=?2",
            params![NOW, deletion],
        )
        .unwrap();
        let mut value: Value = serde_json::from_slice(&causal_event(
            deletion,
            peer,
            "Delete",
            "delete",
            &[parent.into()],
            2,
            2,
        ))
        .unwrap();
        value["metadata"] = Value::Null;
        value["deleted_at"] = json!(NOW);
        let bytes = serde_json::to_vec(&value).unwrap();
        db.execute("UPDATE cloud_sync_state SET pull_cursor=3", [])
            .unwrap();
        assert_eq!(
            preserve_authenticated_event(&mut db, ACCOUNT, "project", &bytes, NOW).unwrap(),
            "applied"
        );
        assert_eq!(ack_candidate(&mut db), 1);
        drop(db);
        let mut db = sqlite::open_database(&path).unwrap();
        assert_eq!(
            preserve_authenticated_event(&mut db, ACCOUNT, "project", &bytes, NOW).unwrap(),
            "applied"
        );
        assert_eq!(visible_metadata(&db, "project").unwrap(), before);
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM notes", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM cloud_sync_metadata_events", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            2
        );
        assert_eq!(ack_candidate(&mut db), 1);
        drop(db);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn project_metadata_acceptance_import_byte_cap_preserves_prefix_on_restart() {
        let (mut db, path) = database();
        let id = "123e4567-e89b-42d3-a456-426614174071";
        let first = MetadataImportPage {
            expected_cursor: 0,
            next_cursor: 1,
            has_more: true,
            page_events: 1,
            page_identity: "a".repeat(64),
            events: vec![MetadataImportEvent {
                server_sequence: 1,
                plaintext: event(id, DEVICE, "Prefix"),
            }],
        };
        commit_metadata_import_page(&mut db, ACCOUNT, USER, "project", BOOT, &first).unwrap();
        // Model an already verified aggregate at the frozen byte bound without
        // generating a large fixture. The production boundary is checked next.
        db.execute(
            "UPDATE cloud_sync_metadata_imports SET payload_bytes=16777216 WHERE account_id=?1",
            [ACCOUNT],
        )
        .unwrap();
        let next = MetadataImportPage {
            expected_cursor: 1,
            next_cursor: 2,
            has_more: false,
            page_events: 1,
            page_identity: "b".repeat(64),
            events: vec![MetadataImportEvent {
                server_sequence: 2,
                plaintext: causal_event(
                    "123e4567-e89b-42d3-a456-426614174072",
                    DEVICE,
                    "Tail",
                    "update",
                    &[id.into()],
                    2,
                    2,
                ),
            }],
        };
        let blocked =
            commit_metadata_import_page(&mut db, ACCOUNT, USER, "project", BOOT, &next).unwrap();
        assert_eq!(
            blocked.blocker.as_deref(),
            Some("metadata_import_resource_limit")
        );
        assert_eq!(blocked.cursor, 1);
        assert_eq!(blocked.event_count, 1);
        assert!(blocked.head.is_none() && blocked.metadata.is_none());
        drop(db);
        let mut db = sqlite::open_database(&path).unwrap();
        let blocked = read_metadata_import(&mut db, ACCOUNT, "project", BOOT).unwrap();
        assert_eq!(blocked.state, "blocked");
        assert_eq!(blocked.tips, vec![id.to_string()]);
        assert!(blocked.head.is_none());
        assert!(
            commit_metadata_import_page(&mut db, ACCOUNT, USER, "project", BOOT, &next).is_err()
        );
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM cloud_sync_metadata_import_events",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        assert_eq!(
            db.query_row(
                "SELECT pull_cursor+ack_cursor FROM cloud_sync_state",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        assert_eq!(
            visible_metadata(&db, "project").unwrap()["name"],
            "Local name"
        );
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
}

fn decode_metadata_event(bytes: &[u8], project: &str) -> Result<Value, MetadataError> {
    if bytes.is_empty() || bytes.len() > MAX_BYTES {
        return Err(MetadataError::Invalid);
    }
    let value: Value = serde_json::from_slice(bytes).map_err(|_| MetadataError::Invalid)?;
    if value.to_string().as_bytes() != bytes {
        return Err(MetadataError::Invalid);
    }
    let root = value.as_object().ok_or(MetadataError::Invalid)?;
    if !exact(root, &["version", "header", "metadata", "deleted_at"])
        || !matches!(root["version"].as_u64(), Some(1 | 2))
    {
        return Err(MetadataError::Invalid);
    }
    let header = root["header"].as_object().ok_or(MetadataError::Invalid)?;
    if !exact(
        header,
        &[
            "account_id",
            "bootstrap_id",
            "device_id",
            "entity_id",
            "event_id",
            "generation",
            "operation",
            "parent_event_ids",
            "project_id",
            "revision",
            "updated_at",
        ],
    ) {
        return Err(MetadataError::Invalid);
    }
    let event_id = str_field(header, "event_id")?;
    let device = str_field(header, "device_id")?;
    let bootstrap = str_field(header, "bootstrap_id")?;
    let operation = str_field(header, "operation")?;
    let revision = positive(header, "revision")?;
    let generation = positive(header, "generation")?;
    let updated = str_field(header, "updated_at")?;
    if !uuid(event_id)
        || !uuid(device)
        || !uuid(bootstrap)
        || !timestamp(updated)
        || str_field(header, "project_id")? != project
        || str_field(header, "entity_id")? != project
    {
        return Err(MetadataError::Invalid);
    }
    let parents = header["parent_event_ids"]
        .as_array()
        .ok_or(MetadataError::Invalid)?;
    if parents.len() > 64
        || parents.iter().any(|v| !v.as_str().is_some_and(uuid))
        || parents.windows(2).any(|w| w[0].as_str() >= w[1].as_str())
        || parents.iter().any(|v| v.as_str() == Some(event_id))
    {
        return Err(MetadataError::Invalid);
    }
    if (operation == "create" && (revision != 1 || generation != 1 || !parents.is_empty()))
        || (operation == "genesis_resolution" && (revision < 2 || parents.len() < 2))
        || (matches!(operation, "update" | "delete") && parents.len() != 1)
        || (operation == "resolution" && parents.len() < 2)
        || !matches!(
            operation,
            "create" | "update" | "delete" | "genesis_resolution" | "resolution"
        )
    {
        return Err(MetadataError::Invalid);
    }
    let deleted = root["deleted_at"].as_str();
    if operation == "delete" {
        if !root["metadata"].is_null() || deleted != Some(updated) {
            return Err(MetadataError::Invalid);
        }
    } else if !root["deleted_at"].is_null() {
        return Err(MetadataError::Invalid);
    } else {
        let has_cover = root["metadata"].get("cover_reference").is_some();
        if has_cover != (root["version"] == 2) {
            return Err(MetadataError::Invalid);
        }
        validate_metadata(&root["metadata"])?;
    }
    Ok(value)
}

fn invalidate_stale_decisions(
    tx: &rusqlite::Transaction<'_>,
    account: &str,
    project: &str,
    now: &str,
) -> Result<(), MetadataError> {
    let mut query=tx.prepare("SELECT event_id FROM cloud_sync_metadata_tips WHERE account_id=?1 AND project_id=?2 ORDER BY event_id")?;
    let tips = query
        .query_map(params![account, project], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(query);
    let tip_json = serde_json::to_string(&tips).map_err(|_| MetadataError::Invalid)?;
    let mut query=tx.prepare("SELECT d.event_id FROM cloud_sync_metadata_decisions d JOIN cloud_sync_metadata_events e ON e.account_id=d.account_id AND e.event_id=d.event_id WHERE d.account_id=?1 AND d.project_id=?2 AND d.state='pending' AND e.operation IN ('resolution','genesis_resolution') AND d.expected_tips_json!=?3")?;
    let stale = query
        .query_map(params![account, project, tip_json], |r| {
            r.get::<_, String>(0)
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(query);
    for id in stale {
        tx.execute("INSERT INTO cloud_sync_metadata_invalidated_decisions VALUES(?1,?2,?3,'tip_set_changed',?4)",params![account,id,tip_json,now])?;
        tx.execute("UPDATE cloud_sync_metadata_decisions SET state='conflict' WHERE account_id=?1 AND event_id=?2",params![account,id])?;
    }
    Ok(())
}

// Keep the old 16*200 event budget, now independent of account-history page count.
// The aggregate canonical payload budget equals the existing 16 MiB v3 batch budget.
const IMPORT_PAGE_EVENTS: usize = 200;
const IMPORT_HISTORY_EVENTS: i64 = 3200;
const IMPORT_HISTORY_BYTES: i64 = 16 * 1024 * 1024;
#[derive(Deserialize, Serialize)]
pub(crate) struct MetadataImportEvent {
    pub server_sequence: i64,
    pub plaintext: Vec<u8>,
}
#[derive(Deserialize, Serialize)]
pub(crate) struct MetadataImportPage {
    pub expected_cursor: i64,
    pub next_cursor: i64,
    pub has_more: bool,
    pub page_events: usize,
    pub page_identity: String,
    pub events: Vec<MetadataImportEvent>,
}
#[derive(Serialize)]
pub(crate) struct MetadataImportProgress {
    pub cursor: i64,
    pub state: String,
    pub blocker: Option<String>,
    pub event_count: i64,
    pub metadata: Option<Value>,
    pub head: Option<String>,
    pub tips: Vec<String>,
}
pub(crate) fn read_metadata_import(
    connection: &mut Connection,
    account: &str,
    project: &str,
    bootstrap: &str,
) -> Result<MetadataImportProgress, MetadataError> {
    if !uuid(bootstrap) || project.is_empty() || project.len() > 512 {
        return Err(MetadataError::Invalid);
    }
    connection.execute("INSERT INTO cloud_sync_metadata_imports(account_id,project_id,bootstrap_id) VALUES(?1,?2,?3) ON CONFLICT DO NOTHING",params![account,project,bootstrap])?;
    let row:(String,i64,String,Option<String>,i64)=connection.query_row("SELECT bootstrap_id,cursor,state,blocker,event_count FROM cloud_sync_metadata_imports WHERE account_id=?1 AND project_id=?2",params![account,project],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?;
    if row.0 != bootstrap {
        return Err(MetadataError::Scope);
    }
    let mut query=connection.prepare("SELECT event_id FROM cloud_sync_metadata_import_tips WHERE account_id=?1 AND project_id=?2 ORDER BY event_id")?;
    let tips = query
        .query_map(params![account, project], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let head = if row.2 == "complete" && tips.len() == 1 {
        Some(tips[0].clone())
    } else {
        None
    };
    let metadata = if let Some(id) = &head {
        let bytes:Vec<u8>=connection.query_row("SELECT canonical_payload FROM cloud_sync_metadata_import_events WHERE account_id=?1 AND project_id=?2 AND event_id=?3",params![account,project,id],|r|r.get(0))?;
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| MetadataError::Invalid)?;
        if value["metadata"].is_null() {
            None
        } else {
            Some(value["metadata"].clone())
        }
    } else {
        None
    };
    Ok(MetadataImportProgress {
        cursor: row.1,
        state: row.2,
        blocker: row.3,
        event_count: row.4,
        metadata,
        head,
        tips,
    })
}

pub(crate) fn commit_metadata_import_page(
    connection: &mut Connection,
    account: &str,
    user: &str,
    project: &str,
    bootstrap: &str,
    page: &MetadataImportPage,
) -> Result<MetadataImportProgress, MetadataError> {
    if page.expected_cursor < 0
        || page.next_cursor < page.expected_cursor
        || page.next_cursor > MAX_REVISION
        || page.page_events > IMPORT_PAGE_EVENTS
        || page.events.len() > page.page_events
        || (page.has_more && (page.page_events == 0 || page.next_cursor == page.expected_cursor))
        || page.page_identity.len() != 64
        || !page.page_identity.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(MetadataError::Invalid);
    }
    read_metadata_import(connection, account, project, bootstrap)?;
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let (cursor,state,count,size):(i64,String,i64,i64)=tx.query_row("SELECT cursor,state,event_count,payload_bytes FROM cloud_sync_metadata_imports WHERE account_id=?1 AND project_id=?2",params![account,project],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
    let previous:Option<(i64,String,bool)>=tx.query_row("SELECT next_cursor,page_identity,has_more FROM cloud_sync_metadata_import_pages WHERE account_id=?1 AND project_id=?2 AND expected_cursor=?3",params![account,project,page.expected_cursor],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
    if let Some((next, identity, more)) = previous {
        if next != page.next_cursor || identity != page.page_identity || more != page.has_more {
            return Err(MetadataError::Conflict);
        }
        tx.commit()?;
        return read_metadata_import(connection, account, project, bootstrap);
    }
    if cursor != page.expected_cursor || state == "blocked" {
        return Err(MetadataError::Conflict);
    }
    let bytes: i64 = page.events.iter().map(|e| e.plaintext.len() as i64).sum();
    if count + page.events.len() as i64 > IMPORT_HISTORY_EVENTS
        || size + bytes > IMPORT_HISTORY_BYTES
    {
        tx.execute("UPDATE cloud_sync_metadata_imports SET state='blocked',blocker='metadata_import_resource_limit' WHERE account_id=?1 AND project_id=?2",params![account,project])?;
        tx.commit()?;
        return read_metadata_import(connection, account, project, bootstrap);
    }
    let mut previous_sequence = cursor;
    for item in &page.events {
        if item.server_sequence <= previous_sequence || item.server_sequence > page.next_cursor {
            return Err(MetadataError::Invalid);
        }
        previous_sequence = item.server_sequence;
        let value = decode_metadata_event(&item.plaintext, project)?;
        if let Some(reference) = value["metadata"].get("cover_reference") {
            crate::project_cover_sync::require_material(&tx, account, project, reference)?;
        }
        let h = value["header"].as_object().ok_or(MetadataError::Invalid)?;
        if str_field(h, "account_id")? != user || str_field(h, "bootstrap_id")? != bootstrap {
            return Err(MetadataError::Scope);
        }
        let id = str_field(h, "event_id")?;
        let parents = h["parent_event_ids"]
            .as_array()
            .ok_or(MetadataError::Invalid)?;
        let mut revision = 0;
        let mut generation = 0;
        for parent in parents {
            let raw:Option<Vec<u8>>=tx.query_row("SELECT canonical_payload FROM cloud_sync_metadata_import_events WHERE account_id=?1 AND project_id=?2 AND event_id=?3",params![account,project,parent.as_str()],|r|r.get(0)).optional()?;
            let raw = raw.ok_or(MetadataError::Scope)?;
            let prior: Value = serde_json::from_slice(&raw).map_err(|_| MetadataError::Invalid)?;
            revision = revision.max(
                prior["header"]["revision"]
                    .as_i64()
                    .ok_or(MetadataError::Invalid)?,
            );
            generation = generation.max(
                prior["header"]["generation"]
                    .as_i64()
                    .ok_or(MetadataError::Invalid)?,
            );
            if h["operation"] == "genesis_resolution" && prior["header"]["operation"] != "create" {
                return Err(MetadataError::Invalid);
            }
        }
        if !parents.is_empty()
            && (positive(h, "revision")? != revision + 1
                || positive(h, "generation")? <= generation)
        {
            return Err(MetadataError::Invalid);
        }
        tx.execute(
            "INSERT INTO cloud_sync_metadata_import_events VALUES(?1,?2,?3,?4,?5)",
            params![account, project, id, item.server_sequence, item.plaintext],
        )?;
        for parent in parents {
            tx.execute("DELETE FROM cloud_sync_metadata_import_tips WHERE account_id=?1 AND project_id=?2 AND event_id=?3",params![account,project,parent.as_str()])?;
        }
        tx.execute(
            "INSERT INTO cloud_sync_metadata_import_tips VALUES(?1,?2,?3)",
            params![account, project, id],
        )?;
    }
    // Empty terminal reads do not occupy the continuation cursor: later history can extend it.
    if page.page_events > 0 {
        tx.execute(
            "INSERT INTO cloud_sync_metadata_import_pages VALUES(?1,?2,?3,?4,?5,?6)",
            params![
                account,
                project,
                page.expected_cursor,
                page.next_cursor,
                page.page_identity,
                page.has_more
            ],
        )?;
    }
    tx.execute("UPDATE cloud_sync_metadata_imports SET cursor=?3,state=?4,event_count=event_count+?5,payload_bytes=payload_bytes+?6 WHERE account_id=?1 AND project_id=?2",params![account,project,page.next_cursor,if page.has_more {"running"} else {"complete"},page.events.len() as i64,bytes])?;
    tx.commit()?;
    read_metadata_import(connection, account, project, bootstrap)
}

/// Independent native framing verifier; historical v1 is never rewritten.
pub(crate) fn unframe_metadata_event(frame: &[u8]) -> Result<Value, MetadataError> {
    let normalized;
    let frame = if frame.len() >= 20
        && frame[11] != 0
        && [1].contains(&frame[9])
        && [1, 2].contains(&frame[10])
    {
        normalized = crate::frame_compression::normalize_authenticated_frame(
            frame,
            &[1],
            &[1, 2],
            MAX_BYTES,
        )
        .map_err(MetadataError::Compression)?;
        normalized.as_slice()
    } else {
        frame
    };

    if frame.len() < 20
        || frame.len() > MAX_BYTES + 20
        || &frame[..8] != b"WORTA-C1"
        || frame[8] != 1
        || frame[9] != 1
        || !matches!(frame[10], 1 | 2)
        || frame[11] != 0
    {
        return Err(MetadataError::Invalid);
    }
    let len = frame.len() - 20;
    if u32::from_be_bytes(frame[12..16].try_into().unwrap()) as usize != len
        || u32::from_be_bytes(frame[16..20].try_into().unwrap()) as usize != len
    {
        return Err(MetadataError::Invalid);
    }
    let value: Value = serde_json::from_slice(&frame[20..]).map_err(|_| MetadataError::Invalid)?;
    let project = value["header"]["project_id"]
        .as_str()
        .ok_or(MetadataError::Invalid)?;
    let value = decode_metadata_event(&frame[20..], project)?;
    if value["version"] != frame[10] {
        return Err(MetadataError::Invalid);
    }
    Ok(value)
}
#[cfg(test)]
mod cover_vectors {
    use super::*;
    #[test]
    fn project_metadata_cover_v2_golden_vectors_preserve_v1() {
        let vectors: Vec<Value> = serde_json::from_str(include_str!(
            "../../src/cloud/__fixtures__/projectMetadataV2.json"
        ))
        .unwrap();
        for vector in vectors {
            let raw = vector["frame_hex"].as_str().unwrap();
            let bytes = (0..raw.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&raw[i..i + 2], 16).unwrap())
                .collect::<Vec<_>>();
            assert_eq!(
                &bytes[20..],
                vector["canonical"].as_str().unwrap().as_bytes()
            );
            assert_eq!(unframe_metadata_event(&bytes).unwrap(), vector["event"]);
            for offset in [8, 9, 10, 11] {
                let mut bad = bytes.clone();
                bad[offset] = 255;
                assert!(unframe_metadata_event(&bad).is_err());
            }
            let mut bad = vector["event"].clone();
            bad["metadata"]["unknown"] = json!(true);
            assert!(decode_metadata_event(bad.to_string().as_bytes(), "project").is_err());
            if bad["version"] == 2 {
                bad["metadata"].as_object_mut().unwrap().remove("unknown");
                bad["version"] = json!(1);
                assert!(decode_metadata_event(bad.to_string().as_bytes(), "project").is_err());
            }
        }
    }
}

/// Reader failure preserves the immutable inbox/object and leaves shared ACK hole.
pub(crate) fn compression_reader_blocker(
    db: &Connection,
    a: &str,
    event: &str,
    nonce: &[u8],
    ciphertext: &[u8],
    code: &str,
) -> Result<(), MetadataError> {
    if !crate::frame_compression::CODES.contains(&code) {
        return Err(MetadataError::Invalid);
    }
    db.execute("UPDATE cloud_sync_inbox SET state='orphan',error_code=?1 WHERE account_id=?2 AND event_id=?3 AND entity_type='project_metadata' AND state IN ('received','orphan') AND EXISTS(SELECT 1 FROM cloud_sync_event_objects o WHERE o.account_id=?2 AND o.event_id=?3 AND o.nonce=?4 AND o.ciphertext=?5)",params![code,a,event,nonce,ciphertext])?;
    Ok(())
}
