//! Framed Note gate only. C15/C17 own causal history, conflicts and mutations.
use crate::note_sync_plaintext::{
    NoteSyncPlaintext, NoteSyncResolutionV2, NoteSyncResolutionV2Result, NoteSyncRoute,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;

pub(crate) struct ContentNote {
    pub frame: Vec<u8>,
    pub root: Value,
    pub ordinary: Option<NoteSyncPlaintext>,
    pub resolution: Option<NoteSyncResolutionV2>,
}
impl ContentNote {
    pub fn route(&self) -> &NoteSyncRoute {
        if let Some(e) = &self.ordinary {
            match e {
                NoteSyncPlaintext::Create { note, .. } | NoteSyncPlaintext::Update { note, .. } => {
                    &note.route
                }
                NoteSyncPlaintext::Delete { note, .. } => &note.route,
            }
        } else {
            match &self.resolution.as_ref().unwrap().result {
                NoteSyncResolutionV2Result::Upsert(n) => &n.route,
                NoteSyncResolutionV2Result::Delete(n) => &n.route,
            }
        }
    }
}
fn exact(v: &Value, fields: &[&str]) -> bool {
    v.as_object()
        .is_some_and(|o| o.len() == fields.len() && fields.iter().all(|f| o.contains_key(*f)))
}
fn bounded(v: &Value, depth: usize, count: &mut usize) -> Result<(), &'static str> {
    *count += 1;
    if depth > 12 || *count > 131072 {
        return Err("content_note_resource_limit");
    }
    match v {
        Value::Array(a) => {
            for x in a {
                bounded(x, depth + 1, count)?
            }
        }
        Value::Object(o) => {
            for x in o.values() {
                bounded(x, depth + 1, count)?
            }
        }
        _ => {}
    };
    Ok(())
}
fn portable(n: &Value) -> Result<(), &'static str> {
    if n["source_type"] == "mindmap" {
        return Err("content_note_map_owned");
    }
    if n["source_type"] != "project"
        || !n["source_map_id"].is_null()
        || !n["source_node_id"].is_null()
    {
        return Err("content_note_unsupported_source");
    }
    if !matches!(n["content_format"].as_str(), Some("html" | "plain")) {
        return Err("unsupported_content_format");
    }
    for key in ["id", "project_id"] {
        if !n[key]
            .as_str()
            .is_some_and(|s| !s.is_empty() && s.len() <= 512)
        {
            return Err("invalid_note_payload");
        }
    }
    if !n["stage_id"].is_null()
        && !n["stage_id"]
            .as_str()
            .is_some_and(|s| !s.is_empty() && s.len() <= 512)
    {
        return Err("invalid_note_payload");
    }
    if n.get("content").is_some() {
        if !n["metadata"].as_object().is_some_and(|m| m.is_empty()) {
            return Err("content_note_unsupported_source");
        }
        if n["content"]
            .as_str()
            .map_or(true, |s| s.len() > 7 * 1024 * 1024)
            || n["title"].as_str().map_or(true, |s| s.len() > 512 * 1024)
            || n["color"].as_str().map_or(true, |s| s.len() > 512)
            || n["tags"].as_array().map_or(true, |a| {
                a.len() > 4096
                    || a.iter()
                        .any(|s| s.as_str().map_or(true, |s| s.len() > 16384))
            })
            || n["checklist"].as_array().map_or(true, |a| {
                a.len() > 16384
                    || a.iter().any(|x| {
                        x["id"].as_str().map_or(true, |s| s.len() > 512)
                            || x["text"].as_str().map_or(true, |s| s.len() > 65536)
                    })
            })
        {
            return Err("content_note_resource_limit");
        }
    }
    Ok(())
}
pub(crate) fn decode(frame: &[u8]) -> Result<ContentNote, &'static str> {
    let normalized;
    let frame = if frame.len() >= 20
        && frame[11] != 0
        && [8].contains(&frame[9])
        && [1].contains(&frame[10])
    {
        normalized = crate::frame_compression::normalize_authenticated_frame(
            frame,
            &[8],
            &[1],
            8388608 - 20,
        )
        .map_err(|e| e)?;
        normalized.as_slice()
    } else {
        frame
    };

    if frame.len() > 8388608 {
        return Err("content_note_resource_limit");
    }
    if frame.len() < 20 || &frame[..8] != b"WORTA-C1" || frame[8..12] != [1, 8, 1, 0] {
        return Err("content_note_codec_unsupported");
    }
    if u32::from_be_bytes(frame[12..16].try_into().unwrap()) as usize != frame.len() - 20
        || frame[12..16] != frame[16..20]
    {
        return Err("invalid_note_payload");
    }
    let root: Value = serde_json::from_slice(&frame[20..]).map_err(|_| "invalid_note_payload")?;
    if serde_json::to_vec(&root).map_err(|_| "invalid_note_payload")? != frame[20..] {
        return Err("invalid_note_payload");
    }
    bounded(&root, 0, &mut 0)?;
    if !exact(
        &root,
        &[
            "version",
            "account_id",
            "device_id",
            "dependencies",
            "event",
        ],
    ) || root["version"] != 1
        || !["account_id", "device_id"].iter().all(|k| {
            root[k]
                .as_str()
                .is_some_and(crate::project_metadata_sync::uuid)
        })
        || !exact(
            &root["dependencies"],
            &["bootstrap_id", "metadata_event_id", "stage_event_ids"],
        )
    {
        return Err("invalid_note_payload");
    }
    let d = &root["dependencies"];
    if !["bootstrap_id", "metadata_event_id"].iter().all(|k| {
        d[k].as_str()
            .is_some_and(crate::project_metadata_sync::uuid)
    }) {
        return Err("invalid_note_payload");
    }
    let refs: Vec<String> =
        serde_json::from_value(d["stage_event_ids"].clone()).map_err(|_| "invalid_note_payload")?;
    if refs.len() > 64
        || refs.iter().any(|r| !crate::project_metadata_sync::uuid(r))
        || refs.windows(2).any(|p| p[0] >= p[1])
    {
        return Err("invalid_note_payload");
    }
    let e = &root["event"];
    let n = if e["version"] == 1 {
        &e["note"]
    } else {
        &e["result"]["note"]
    };
    portable(n)?;
    if n["stage_id"].is_null() != refs.is_empty() {
        return Err("invalid_note_payload");
    }
    if e["resolution"]["strategy"] == "keep_both" {
        let retained = &e["resolution"]["retained_note"];
        portable(retained)?;
        if retained["stage_id"] != n["stage_id"]
            || retained["content_format"] != n["content_format"]
        {
            return Err("content_note_scope_mismatch");
        }
    }
    let bytes = serde_json::to_vec(e).map_err(|_| "invalid_note_payload")?;
    let (ordinary, resolution) = match e["version"].as_u64() {
        Some(1) => (
            Some(
                crate::note_sync_plaintext::decode_note_sync_plaintext(&bytes)
                    .map_err(|_| "invalid_note_payload")?,
            ),
            None,
        ),
        Some(2) => (
            None,
            Some(
                crate::note_sync_plaintext::decode_note_sync_resolution_v2(&bytes)
                    .map_err(|_| "invalid_note_payload")?,
            ),
        ),
        _ => return Err("content_note_codec_unsupported"),
    };
    Ok(ContentNote {
        frame: frame.into(),
        root,
        ordinary,
        resolution,
    })
}
// Exact immutable receipt is recorded in the SAME transaction as the existing
// protected Note engine. Waiting is never an ACK-eligible outcome.
pub(crate) fn receipt(
    db: &rusqlite::Transaction<'_>,
    c: &crate::note_sync::ApplyVerifiedReceivedNoteIpcCommand,
    frame: Option<&[u8]>,
) -> rusqlite::Result<()> {
    crate::note_sync::validate_pull_scope(
        db,
        &c.account_id,
        &c.pulling_device_id,
        &c.canonical_user_id,
    )
    .map_err(|_| rusqlite::Error::InvalidQuery)?;
    let valid:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_inbox i JOIN cloud_sync_event_objects o ON o.account_id=i.account_id AND o.event_id=i.event_id WHERE i.account_id=?1 AND i.event_id=?2 AND i.server_sequence=?3 AND i.device_id=?4 AND i.entity_type='note' AND i.operation='event' AND i.deleted_at IS NULL AND o.crypto_version=1 AND o.aad_version=1 AND o.nonce=?5 AND o.ciphertext=?6)",params![c.account_id,c.event_id,c.server_sequence,c.source_device_id,c.nonce,c.ciphertext],|r|r.get(0))?;
    if !valid || c.crypto_version != 1 || c.aad_version != 1 {
        return Err(rusqlite::Error::InvalidQuery);
    }
    let stored:Option<Option<Vec<u8>>>=db.query_row("SELECT canonical_frame FROM cloud_content_note_receipts WHERE account_id=?1 AND event_id=?2",params![c.account_id,c.event_id],|r|r.get(0)).optional()?;
    if let Some(Some(old)) = stored {
        if frame.is_some_and(|new| new != old) {
            return Err(rusqlite::Error::InvalidQuery);
        }
    }
    db.execute("INSERT OR IGNORE INTO cloud_content_note_receipts VALUES(?1,?2,?3,?4,?5,?6,'waiting',NULL)",params![c.account_id,c.event_id,c.server_sequence,frame,c.nonce,c.ciphertext])?;
    if frame.is_some() {
        db.execute("UPDATE cloud_content_note_receipts SET canonical_frame=COALESCE(canonical_frame,?3) WHERE account_id=?1 AND event_id=?2",params![c.account_id,c.event_id,frame])?;
        // Lost upload response: exact own immutable frame and encrypted pair
        // in the authenticated inbox prove acceptance without an HTTP receipt.
        if c.source_device_id == c.pulling_device_id {
            let own:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM cloud_content_note_writer_events w JOIN cloud_sync_outbox b ON b.event_id=w.event_id JOIN cloud_sync_event_objects o ON o.account_id=b.account_id AND o.event_id=b.event_id WHERE w.account_id=?1 AND w.event_id=?2 AND w.canonical_frame=?3 AND b.device_id=?4 AND b.lifecycle IN ('sealed','accepted') AND o.nonce=?5 AND o.ciphertext=?6)",params![c.account_id,c.event_id,frame,c.pulling_device_id,c.nonce,c.ciphertext],|r|r.get(0))?;
            if own {
                db.execute("INSERT OR IGNORE INTO cloud_sync_upload_receipts(account_id,event_id,device_id,server_sequence,duplicate,accepted_at) VALUES(?1,?2,?3,?4,0,strftime('%Y-%m-%dT%H:%M:%fZ','now'))",params![c.account_id,c.event_id,c.pulling_device_id,c.server_sequence])?;
                let proven:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_upload_receipts WHERE account_id=?1 AND event_id=?2 AND device_id=?3 AND server_sequence=?4)",params![c.account_id,c.event_id,c.pulling_device_id,c.server_sequence],|r|r.get(0))?;
                if !proven {
                    return Err(rusqlite::Error::InvalidQuery);
                }
                db.execute("UPDATE cloud_sync_outbox SET lifecycle='accepted' WHERE event_id=?1 AND account_id=?2 AND lifecycle='sealed'",params![c.event_id,c.account_id])?;
            }
        }
    }
    Ok(())
}
pub(crate) fn ready(
    db: &Connection,
    a: &str,
    user: &str,
    source: &str,
    e: &ContentNote,
) -> Result<(), String> {
    if e.root["account_id"] != user || e.root["device_id"] != source {
        return Err("content_note_scope_mismatch".into());
    }
    let route = e.route();
    let d = &e.root["dependencies"];
    crate::account_catalog::project_reference_ready(
        db,
        a,
        &route.project_id,
        d["bootstrap_id"].as_str().unwrap(),
        d["metadata_event_id"].as_str().unwrap(),
    )
    .map_err(|error| match error {
        crate::account_catalog::Error::Code("catalog_resource_limit") => {
            "content_note_resource_limit"
        }
        _ => "project_metadata_authority_unresolved",
    })?;
    if let Some(stage) = &route.stage_id {
        let refs: Vec<String> = serde_json::from_value(d["stage_event_ids"].clone()).unwrap();
        crate::stage_sync::content_reference_ready(db, a, &route.project_id, stage, &refs)?;
    }
    // Owner/source/format changes need a separately frozen mutation contract,
    // including after a tombstone has removed the visible row.
    let same_scope = |prior: &Value| {
        let incoming = if e.ordinary.is_some() {
            &e.root["event"]["note"]
        } else {
            &e.root["event"]["result"]["note"]
        };
        [
            "id",
            "project_id",
            "stage_id",
            "source_type",
            "source_map_id",
            "source_node_id",
            "content_format",
        ]
        .iter()
        .all(|k| prior[*k] == incoming[*k])
    };
    let existing: Option<String> = db
        .query_row(
            "SELECT payload_json FROM notes WHERE id=?1",
            [&route.id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|_| "content_note_scope_mismatch")?;
    if let Some(raw) = existing {
        let prior: Value = serde_json::from_str(&raw).map_err(|_| "content_note_scope_mismatch")?;
        portable(&prior).map_err(str::to_string)?;
        if !same_scope(&prior) {
            return Err("content_note_scope_mismatch".into());
        }
    }
    let h = &e.root["event"]["header"];
    let mut parents = Vec::new();
    if let Some(parent) = h["parent_event_id"].as_str() {
        parents.push(parent);
    }
    if let Some(additional) = h["additional_parent_event_ids"].as_array() {
        parents.extend(additional.iter().filter_map(Value::as_str));
    }
    for parent in parents {
        let raw:Option<String>=db.query_row("SELECT snapshot_json FROM cloud_sync_note_causal_history WHERE account_id=?1 AND project_id=?2 AND entity_id=?3 AND event_id=?4 UNION ALL SELECT snapshot_json FROM cloud_sync_note_conflict_versions WHERE account_id=?1 AND project_id=?2 AND entity_id=?3 AND event_id=?4 UNION ALL SELECT json_extract(CAST(canonical_payload AS TEXT),'$.result.note') FROM cloud_sync_note_applied_resolutions WHERE account_id=?1 AND project_id=?2 AND entity_id=?3 AND resolution_event_id=?4 AND lifecycle='applied' LIMIT 1",params![a,route.project_id,route.id,parent],|r|r.get(0)).optional().map_err(|_| "content_note_scope_mismatch")?;
        if let Some(raw) = raw {
            let prior: Value =
                serde_json::from_str(&raw).map_err(|_| "content_note_scope_mismatch")?;
            if !same_scope(&prior) {
                return Err("content_note_scope_mismatch".into());
            }
        }
    }
    Ok(())
}
pub(crate) fn complete(db: &Connection, a: &str, id: &str) -> rusqlite::Result<()> {
    let state: String = db.query_row(
        "SELECT state FROM cloud_sync_inbox WHERE account_id=?1 AND event_id=?2",
        params![a, id],
        |r| r.get(0),
    )?;
    if matches!(state.as_str(), "applied" | "conflict_preserved") {
        db.execute("UPDATE cloud_content_note_receipts SET outcome=?3,blocker=NULL WHERE account_id=?1 AND event_id=?2",params![a,id,state])?;
        if state == "applied" {
            db.execute("UPDATE cloud_content_note_migrations SET activated=1 WHERE account_id=?1 AND project_id=(SELECT project_id FROM cloud_sync_inbox WHERE account_id=?1 AND event_id=?2) AND EXISTS(SELECT 1 FROM cloud_content_note_writer_events WHERE account_id=?1 AND event_id=?2)",params![a,id])?;
        }
    } else {
        db.execute("UPDATE cloud_content_note_receipts SET blocker=(SELECT error_code FROM cloud_sync_inbox WHERE account_id=?1 AND event_id=?2) WHERE account_id=?1 AND event_id=?2",params![a,id])?;
    }
    Ok(())
}
pub(crate) fn ack_proven(db: &Connection, a: &str, seq: i64) -> rusqlite::Result<bool> {
    let row:Option<(Vec<u8>,String,String,String,i64,String,String,String,String)>=db.query_row("SELECT r.canonical_frame,i.event_id,i.project_id,i.entity_id,i.sync_revision,i.device_id,r.outcome,b.canonical_user_id,i.updated_at FROM cloud_content_note_receipts r JOIN cloud_sync_inbox i ON i.account_id=r.account_id AND i.event_id=r.event_id JOIN cloud_sync_event_objects o ON o.account_id=r.account_id AND o.event_id=r.event_id JOIN cloud_account_bindings b ON b.local_account_id=r.account_id WHERE r.account_id=?1 AND r.server_sequence=?2 AND i.server_sequence=r.server_sequence AND i.entity_type='note' AND i.operation='event' AND i.state=r.outcome AND r.outcome IN ('applied','conflict_preserved') AND r.canonical_frame IS NOT NULL AND o.crypto_version=1 AND o.aad_version=1 AND o.nonce=r.nonce AND o.ciphertext=r.ciphertext",params![a,seq],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?))).optional()?;
    let Some((frame, id, project, entity, revision, source, outcome, user, updated)) = row else {
        return Ok(false);
    };
    let Ok(event) = decode(&frame) else {
        return Ok(false);
    };
    let h = &event.root["event"]["header"];
    if event.root["account_id"] != user
        || event.root["device_id"] != source
        || h["event_id"] != id
        || h["project_id"] != project
        || h["entity_id"] != entity
        || h["revision"] != revision
        || !h["updated_at"]
            .as_str()
            .is_some_and(|t| crate::note_sync::note_sync_timestamps_equal(t, &updated))
    {
        return Ok(false);
    }
    if event.resolution.is_some() {
        let raw = serde_json::to_vec(&event.root["event"]).unwrap();
        return db.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_note_applied_resolutions WHERE account_id=?1 AND resolution_event_id=?2 AND server_sequence=?3 AND project_id=?4 AND entity_id=?5 AND lifecycle='applied' AND canonical_payload=?6)",params![a,id,seq,project,entity,raw],|r|r.get(0));
    }
    let raw: Option<String> = if outcome == "applied" {
        db.query_row("SELECT snapshot_json FROM cloud_sync_note_causal_history WHERE account_id=?1 AND event_id=?2 AND server_sequence=?3 AND project_id=?4 AND entity_id=?5 AND revision=?6",params![a,id,seq,project,entity,revision],|r|r.get(0)).optional()?
    } else {
        db.query_row("SELECT snapshot_json FROM cloud_sync_note_conflict_versions WHERE account_id=?1 AND event_id=?2 AND server_sequence=?3 AND project_id=?4 AND entity_id=?5 AND revision=?6 AND source='remote'",params![a,id,seq,project,entity,revision],|r|r.get(0)).optional()?
    };
    let Some(raw) = raw else { return Ok(false) };
    let Ok(mut snapshot) = serde_json::from_str::<Value>(&raw) else {
        return Ok(false);
    };
    if let Some(o) = snapshot.as_object_mut() {
        o.remove("revision");
    }
    Ok(snapshot == event.root["event"]["note"])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{note_sync as sync, sqlite::PrivilegedRemoteApplyConnection};
    use serde_json::json;
    const USER: &str = "123e4567-e89b-42d3-a456-426614174000";
    const DEVICE: &str = "123e4567-e89b-42d3-a456-426614174001";
    const SOURCE: &str = "123e4567-e89b-42d3-a456-000000000500";
    const NOW: &str = "2026-10-02T00:00:00.000000Z";
    fn id(n: u32) -> String {
        format!("123e4567-e89b-42d3-a456-{n:012}")
    }
    fn fixture(index: usize) -> Value {
        serde_json::from_str::<Value>(include_str!(
            "../../src/cloud/__fixtures__/contentNoteCodecV1.json"
        ))
        .unwrap()["examples"][index]["event"]
            .clone()
    }
    fn frame(root: &Value) -> Vec<u8> {
        let bytes = serde_json::to_vec(root).unwrap();
        let mut raw = b"WORTA-C1".to_vec();
        raw.extend([1, 8, 1, 0]);
        raw.extend((bytes.len() as u32).to_be_bytes());
        raw.extend((bytes.len() as u32).to_be_bytes());
        raw.extend(bytes);
        raw
    }
    fn database() -> (Connection, std::path::PathBuf) {
        let (mut db, path) = crate::account_catalog::tests::setup();
        crate::account_catalog::tests::seed(&mut db);
        crate::account_catalog::tests::migrate(&mut db);
        (db, path)
    }
    fn receive(
        db: &mut Connection,
        root: &Value,
        seq: i64,
    ) -> sync::ApplyVerifiedReceivedNoteIpcCommand {
        let raw = frame(root);
        let h = &root["event"]["header"];
        let nonce = vec![0; 24];
        let cipher = vec![17; raw.len() + 16];
        let cursor: i64 = db
            .query_row(
                "SELECT pull_cursor FROM cloud_sync_state WHERE account_id='a'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let cmd:sync::CommitMixedSyncInboundPageCommand=serde_json::from_value(json!({"account_id":"a","canonical_user_id":USER,"device_id":DEVICE,"expected_cursor":cursor,"next_cursor":seq,"has_more":false,"items":[{"event_id":h["event_id"],"server_sequence":seq,"source_device_id":SOURCE,"project_id":h["project_id"],"entity_id":h["entity_id"],"entity_type":"note","operation":"event","revision":h["revision"],"updated_at":h["updated_at"],"deleted_at":null,"envelope":{"crypto_version":1,"aad_version":1,"nonce":sync::encode_canonical_base64url(&nonce),"ciphertext":sync::encode_canonical_base64url(&cipher)}}]})).unwrap();
        sync::commit_mixed_sync_inbound_page(db, &cmd).unwrap();
        sync::ApplyVerifiedReceivedNoteIpcCommand {
            account_id: "a".into(),
            canonical_user_id: USER.into(),
            pulling_device_id: DEVICE.into(),
            event_id: h["event_id"].as_str().unwrap().into(),
            server_sequence: seq,
            source_device_id: SOURCE.into(),
            crypto_version: 1,
            aad_version: 1,
            nonce,
            ciphertext: cipher,
            plaintext: raw,
        }
    }
    fn stage(db: &mut Connection, seq: i64, branch: u32) {
        let mut root: Value = serde_json::from_str(include_str!(
            "../../src/cloud/__fixtures__/stageCodecV1.json"
        ))
        .unwrap();
        let e = &mut root["event"];
        let h = &mut e["header"];
        h["account_id"] = json!(USER);
        h["project_id"] = json!("C1");
        h["entity_id"] = json!("S1");
        h["device_id"] = json!(SOURCE);
        h["event_id"] = json!(id(800 + branch));
        h["bootstrap_id"] = json!(id(21));
        h["metadata_event_id"] = json!(id(1));
        h["updated_at"] = json!(NOW);
        if branch > 0 {
            h["operation"] = json!("update");
            h["revision"] = json!(2);
            h["generation"] = json!(2);
            h["parent_event_ids"] = json!([id(800)]);
            e["stage"]["name"] = json!("Renamed");
        }
        if branch == 3 {
            e["header"]["operation"] = json!("delete");
            e["header"]["revision"] = json!(3);
            e["header"]["generation"] = json!(3);
            e["header"]["parent_event_ids"] = json!([id(801)]);
            e["stage"] = Value::Null;
            e["deleted_at"] = json!(NOW);
        }
        let e: crate::stage_sync::Event = serde_json::from_value(e.clone()).unwrap();
        let raw = crate::stage_sync::frame(&e).unwrap();
        let cipher = vec![17; raw.len() + 16];
        let cursor: i64 = db
            .query_row(
                "SELECT pull_cursor FROM cloud_sync_state WHERE account_id='a'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let cmd:sync::CommitMixedSyncInboundPageCommand=serde_json::from_value(json!({"account_id":"a","canonical_user_id":USER,"device_id":DEVICE,"expected_cursor":cursor,"next_cursor":seq,"has_more":false,"items":[{"event_id":e.header.event_id,"server_sequence":seq,"source_device_id":SOURCE,"project_id":"C1","entity_id":"S1","entity_type":"stage","operation":if branch==3{"delete"}else{"upsert"},"revision":e.header.revision,"updated_at":NOW,"deleted_at":e.deleted_at,"envelope":{"crypto_version":1,"aad_version":1,"nonce":sync::encode_canonical_base64url(&[0;24]),"ciphertext":sync::encode_canonical_base64url(&cipher)}}]})).unwrap();
        sync::commit_mixed_sync_inbound_page(db, &cmd).unwrap();
        assert!(matches!(
            crate::stage_sync::apply_received(db, "a", &e.header.event_id, &raw, &[0; 24], &cipher)
                .unwrap(),
            "applied" | "conflict_preserved" | "tombstone_blocked"
        ));
    }
    fn ack(db: &mut Connection) -> i64 {
        sync::prepare_note_sync_ack(
            db,
            &sync::PrepareNoteSyncAckCommand {
                account_id: "a".into(),
                canonical_user_id: USER.into(),
                device_id: DEVICE.into(),
            },
        )
        .unwrap()
        .candidate_cursor
    }
    #[test]
    fn content_note_sync_cross_language_fixture_and_wrong_readers() {
        let f: Value = serde_json::from_str(include_str!(
            "../../src/cloud/__fixtures__/contentNoteCodecV1.json"
        ))
        .unwrap();
        for x in f["examples"].as_array().unwrap() {
            let raw = frame(&x["event"]);
            let hex = raw.iter().map(|b| format!("{b:02x}")).collect::<String>();
            assert_eq!(hex, x["frame_hex"]);
            assert!(decode(&raw).is_ok());
            assert!(crate::note_sync_plaintext::decode_note_sync_plaintext(&raw).is_err());
            assert!(crate::note_sync_plaintext::decode_note_sync_resolution_v2(&raw).is_err());
            assert!(crate::stage_sync::unframe(&raw).is_err());
            let mut bad = raw.clone();
            bad[9] = 1;
            assert!(decode(&bad).is_err());
        }
    }
    #[test]
    fn content_note_sync_variants_transactional_apply_replay_ack_and_no_writer() {
        for i in 0..4 {
            let (mut db, path) = database();
            if i % 2 == 1 {
                stage(&mut db, 9, 0)
            }
            let seq = if i % 2 == 1 { 10 } else { 9 };
            let root = fixture(i);
            let cmd = receive(&mut db, &root, seq);
            let mut privileged = PrivilegedRemoteApplyConnection::from_connection(db).unwrap();
            assert_eq!(
                sync::apply_verified_received_content_note_ipc(&mut privileged, cmd.clone())
                    .unwrap(),
                "applied"
            );
            assert_eq!(
                sync::apply_verified_received_content_note_ipc(&mut privileged, cmd).unwrap(),
                "already_applied"
            );
            let stage: Option<String> = privileged
                .connection()
                .query_row("SELECT stage_id FROM notes WHERE id='N1'", [], |r| r.get(0))
                .unwrap();
            assert_eq!(stage, if i % 2 == 1 { Some("S1".into()) } else { None });
            assert_eq!(ack(privileged.connection_mut_for_test()), seq);
            assert_eq!(
                privileged
                    .connection()
                    .query_row(
                        "SELECT count(*) FROM cloud_sync_outbox WHERE entity_type='note'",
                        [],
                        |r| r.get::<_, i64>(0)
                    )
                    .unwrap(),
                0
            );
            drop(privileged);
            let mut reopened = crate::sqlite::open_database(&path).unwrap();
            assert_eq!(ack(&mut reopened), seq);
            drop(reopened);
            std::fs::remove_file(path).unwrap();
        }
    }
    #[test]
    fn content_note_sync_unknown_stage_restart_rename_retry_fills_shared_hole() {
        let (mut db, path) = database();
        let root = fixture(1);
        let cmd = receive(&mut db, &root, 9);
        let mut p = PrivilegedRemoteApplyConnection::from_connection(db).unwrap();
        assert_eq!(
            sync::apply_verified_received_content_note_ipc(&mut p, cmd.clone()).unwrap(),
            "orphan"
        );
        assert_eq!(ack(p.connection_mut_for_test()), 8);
        assert_eq!(
            p.connection()
                .query_row("SELECT count(*) FROM notes WHERE id='N1'", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        drop(p);
        let mut db = crate::sqlite::open_database(&path).unwrap();
        stage(&mut db, 10, 0);
        stage(&mut db, 11, 1);
        // Later applied Stage events cannot bridge the still-blocked Note prefix.
        assert_eq!(ack(&mut db), 8);
        let mut p = PrivilegedRemoteApplyConnection::from_connection(db).unwrap();
        assert_eq!(
            sync::apply_verified_received_content_note_ipc(&mut p, cmd.clone()).unwrap(),
            "applied"
        );
        assert_eq!(ack(p.connection_mut_for_test()), 11);
        let retained: Vec<u8> = p
            .connection()
            .query_row(
                "SELECT canonical_frame FROM cloud_content_note_receipts WHERE event_id=?1",
                [&cmd.event_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(retained, cmd.plaintext);
        let mut tampered = cmd;
        tampered.ciphertext[0] ^= 1;
        assert!(sync::apply_verified_received_content_note_ipc(&mut p, tampered).is_err());
        drop(p);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn content_note_sync_map_unknown_format_source_and_resource_are_durable_no_ack() {
        for code in [
            "content_note_map_owned",
            "unsupported_content_format",
            "content_note_unsupported_source",
            "content_note_resource_limit",
        ] {
            let (mut db, path) = database();
            let mut root = fixture(0);
            match code {
                "content_note_map_owned" => root["event"]["note"]["source_type"] = json!("mindmap"),
                "unsupported_content_format" => {
                    root["event"]["note"]["content_format"] = json!("markdown")
                }
                "content_note_unsupported_source" => {
                    root["event"]["note"]["metadata"] = json!({"path":"private"})
                }
                _ => root["event"]["note"]["tags"] = json!(vec!["x"; 4097]),
            }
            let cmd = receive(&mut db, &root, 9);
            let mut p = PrivilegedRemoteApplyConnection::from_connection(db).unwrap();
            assert_eq!(
                sync::apply_verified_received_content_note_ipc(&mut p, cmd).unwrap(),
                code
            );
            assert_eq!(ack(p.connection_mut_for_test()), 8);
            drop(p);
            let mut db = crate::sqlite::open_database(&path).unwrap();
            assert_eq!(
                db.query_row("SELECT blocker FROM cloud_content_note_receipts", [], |r| r
                    .get::<_, String>(0))
                    .unwrap(),
                code
            );
            assert_eq!(ack(&mut db), 8);
            assert_eq!(
                db.query_row("SELECT count(*) FROM notes", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                0
            );
            drop(db);
            std::fs::remove_file(path).unwrap();
        }
    }
    #[test]
    fn content_note_sync_dependency_scope_negatives_never_project() {
        for case in [
            "wrong_account",
            "wrong_bootstrap",
            "unknown_metadata",
            "unresolved_metadata",
            "foreign_stage",
            "unrelated_stage",
            "wrong_stage_id",
        ] {
            let (mut db, path) = database();
            stage(&mut db, 9, 0);
            let mut root = fixture(1);
            match case {
                "wrong_account" => root["account_id"] = json!(id(999)),
                "wrong_bootstrap" => root["dependencies"]["bootstrap_id"] = json!(id(999)),
                "unknown_metadata" => root["dependencies"]["metadata_event_id"] = json!(id(999)),
                "unresolved_metadata" => {
                    db.execute("UPDATE projects SET name='Unreconciled' WHERE id='C1'", [])
                        .unwrap();
                }
                "foreign_stage" => {
                    root["event"]["header"]["project_id"] = json!("C2");
                    root["event"]["note"]["project_id"] = json!("C2");
                    root["dependencies"]["bootstrap_id"] = json!(id(22));
                    root["dependencies"]["metadata_event_id"] = json!(id(2));
                }
                "wrong_stage_id" => root["event"]["note"]["stage_id"] = json!("S2"),
                _ => root["dependencies"]["stage_event_ids"] = json!([id(999)]),
            }
            let cmd = receive(&mut db, &root, 10);
            let mut p = PrivilegedRemoteApplyConnection::from_connection(db).unwrap();
            assert_eq!(
                sync::apply_verified_received_content_note_ipc(&mut p, cmd).unwrap(),
                "orphan"
            );
            assert_eq!(ack(p.connection_mut_for_test()), 9);
            assert_eq!(
                p.connection()
                    .query_row("SELECT count(*) FROM notes", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                0
            );
            drop(p);
            std::fs::remove_file(path).unwrap();
        }
    }
    #[test]
    fn content_note_sync_transaction_failure_rolls_back_frame_history_and_projection() {
        let (mut db, path) = database();
        db.execute_batch("CREATE TRIGGER inject_content_failure AFTER INSERT ON notes BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
        let cmd = receive(&mut db, &fixture(0), 9);
        let mut p = PrivilegedRemoteApplyConnection::from_connection(db).unwrap();
        assert!(sync::apply_verified_received_content_note_ipc(&mut p, cmd).is_err());
        for table in [
            "cloud_content_note_receipts",
            "cloud_sync_note_causal_history",
            "notes",
        ] {
            assert_eq!(
                p.connection()
                    .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r
                        .get::<_, i64>(0))
                    .unwrap(),
                0
            );
        }
        assert_eq!(ack(p.connection_mut_for_test()), 8);
        drop(p);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn content_note_sync_plain_conflict_resolution_and_stale_decision_use_c17() {
        let (mut db, path) = database();
        let a = fixture(0);
        let cmd = receive(&mut db, &a, 9);
        let mut p = PrivilegedRemoteApplyConnection::from_connection(db).unwrap();
        assert_eq!(
            sync::apply_verified_received_content_note_ipc(&mut p, cmd).unwrap(),
            "applied"
        );
        let mut b = a.clone();
        b["event"]["header"]["event_id"] = json!(id(820));
        b["event"]["header"]["parent_event_id"] = a["event"]["header"]["event_id"].clone();
        b["event"]["header"]["revision"] = json!(2);
        b["event"]["mutation"] = json!("update");
        b["event"]["note"]["content"] = json!("B");
        let cmd = receive(p.connection_mut_for_test(), &b, 10);
        assert_eq!(
            sync::apply_verified_received_content_note_ipc(&mut p, cmd).unwrap(),
            "applied"
        );
        let mut c = b.clone();
        c["event"]["header"]["event_id"] = json!(id(821));
        c["event"]["note"]["content"] = json!("C");
        let cmd = receive(p.connection_mut_for_test(), &c, 11);
        assert_eq!(
            sync::apply_verified_received_content_note_ipc(&mut p, cmd).unwrap(),
            "conflict"
        );
        assert_eq!(ack(p.connection_mut_for_test()), 11);
        let group: String = p
            .connection()
            .query_row(
                "SELECT group_id FROM cloud_sync_note_conflict_groups WHERE lifecycle='open'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let mut r = a.clone();
        r["event"] = json!({"version":2,"header":{"event_id":id(822),"parent_event_id":id(820),"additional_parent_event_ids":[id(821)],"project_id":"C1","entity_id":"N1","entity_type":"note","operation":"resolution","revision":3,"updated_at":NOW},"mutation":"resolution","resolution":{"conflict_group_id":group,"conflict_generation":1,"resolved_event_ids":[id(820),id(821)],"strategy":"choose_version","selected_event_id":id(820)},"result":{"operation":"upsert","note":b["event"]["note"]}});
        let cmd = receive(p.connection_mut_for_test(), &r, 12);
        assert_eq!(
            sync::apply_verified_received_content_note_ipc(&mut p, cmd.clone()).unwrap(),
            "applied"
        );
        assert_eq!(
            sync::apply_verified_received_content_note_ipc(&mut p, cmd).unwrap(),
            "already_applied"
        );
        assert_eq!(ack(p.connection_mut_for_test()), 12);
        let mut d = b.clone();
        d["event"]["header"]["event_id"] = json!(id(823));
        d["event"]["header"]["parent_event_id"] = json!(id(822));
        d["event"]["header"]["revision"] = json!(4);
        d["event"]["note"]["content"] = json!("After resolution");
        let cmd = receive(p.connection_mut_for_test(), &d, 13);
        assert_eq!(
            sync::apply_verified_received_content_note_ipc(&mut p, cmd).unwrap(),
            "applied"
        );
        let mut stale = r.clone();
        stale["event"]["header"]["event_id"] = json!(id(824));
        let cmd = receive(p.connection_mut_for_test(), &stale, 14);
        assert_eq!(
            sync::apply_verified_received_content_note_ipc(&mut p, cmd).unwrap(),
            "orphan"
        );
        assert_eq!(ack(p.connection_mut_for_test()), 13);
        let payload: String = p
            .connection()
            .query_row("SELECT payload_json FROM notes WHERE id='N1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&payload).unwrap()["content"],
            "After resolution"
        );
        drop(p);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn content_note_sync_stage_tombstone_and_unresolved_tips_block_without_cleanup() {
        for tombstone in [false, true] {
            let (mut db, path) = database();
            stage(&mut db, 9, 0);
            stage(&mut db, 10, 1);
            stage(&mut db, 11, if tombstone { 3 } else { 2 });
            let cmd = receive(&mut db, &fixture(1), 12);
            let mut p = PrivilegedRemoteApplyConnection::from_connection(db).unwrap();
            assert_eq!(
                sync::apply_verified_received_content_note_ipc(&mut p, cmd).unwrap(),
                "orphan"
            );
            assert!(!ack_proven(p.connection(), "a", 12).unwrap());
            assert_eq!(
                p.connection()
                    .query_row("SELECT count(*) FROM stages WHERE id='S1'", [], |r| r
                        .get::<_, i64>(0))
                    .unwrap(),
                1
            );
            assert_eq!(
                p.connection()
                    .query_row("SELECT count(*) FROM notes", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                0
            );
            assert_eq!(
                p.connection()
                    .query_row("SELECT blocker FROM cloud_content_note_receipts", [], |r| r
                        .get::<_, String>(0))
                    .unwrap(),
                if tombstone {
                    "stage_tombstone_child_manifest_incomplete"
                } else {
                    "stage_dependency_missing"
                }
            );
            drop(p);
            std::fs::remove_file(path).unwrap();
        }
    }

    #[test]
    fn content_note_sync_project_frozen_metadata_survives_causal_rename() {
        let (mut db, path) = database();
        crate::account_catalog::tests::metadata_successor(&mut db, "C1", 9);
        let cmd = receive(&mut db, &fixture(0), 10);
        let mut p = PrivilegedRemoteApplyConnection::from_connection(db).unwrap();
        assert_eq!(
            sync::apply_verified_received_content_note_ipc(&mut p, cmd).unwrap(),
            "applied"
        );
        assert_eq!(ack(p.connection_mut_for_test()), 10);
        drop(p);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn content_note_sync_stage_delete_against_edit_preserves_both_branches() {
        let (mut db, path) = database();
        stage(&mut db, 9, 0);
        let a = fixture(1);
        let cmd = receive(&mut db, &a, 10);
        let mut p = PrivilegedRemoteApplyConnection::from_connection(db).unwrap();
        assert_eq!(
            sync::apply_verified_received_content_note_ipc(&mut p, cmd).unwrap(),
            "applied"
        );
        let mut b = a.clone();
        b["event"]["header"]["event_id"] = json!(id(850));
        b["event"]["header"]["parent_event_id"] = a["event"]["header"]["event_id"].clone();
        b["event"]["header"]["revision"] = json!(2);
        b["event"]["mutation"] = json!("update");
        b["event"]["note"]["content"] = json!("Edited");
        let cmd = receive(p.connection_mut_for_test(), &b, 11);
        assert_eq!(
            sync::apply_verified_received_content_note_ipc(&mut p, cmd).unwrap(),
            "applied"
        );
        let mut d = b.clone();
        d["event"]["header"]["event_id"] = json!(id(851));
        d["event"]["header"]["operation"] = json!("delete");
        d["event"]["header"]["deleted_at"] = json!(NOW);
        d["event"]["mutation"] = json!("delete");
        let n = d["event"]["note"].as_object_mut().unwrap();
        n.retain(|k, _| {
            [
                "id",
                "project_id",
                "stage_id",
                "source_type",
                "source_map_id",
                "source_node_id",
                "content_format",
            ]
            .contains(&k.as_str())
        });
        n.insert("deleted_at".into(), json!(NOW));
        let cmd = receive(p.connection_mut_for_test(), &d, 12);
        assert_eq!(
            sync::apply_verified_received_content_note_ipc(&mut p, cmd.clone()).unwrap(),
            "conflict"
        );
        assert_eq!(
            sync::apply_verified_received_content_note_ipc(&mut p, cmd).unwrap(),
            "conflict"
        );
        assert_eq!(ack(p.connection_mut_for_test()), 12);
        let operations:Vec<String>=p.connection().prepare("SELECT v.operation FROM cloud_sync_note_conflict_versions v JOIN cloud_sync_note_conflict_tips t ON t.version_id=v.version_id ORDER BY v.operation").unwrap().query_map([],|r|r.get(0)).unwrap().collect::<Result<_,_>>().unwrap();
        assert_eq!(operations, vec!["delete", "upsert"]);
        drop(p);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn content_note_sync_tombstone_retains_scope_and_rejects_format_move() {
        let (mut db, path) = database();
        let a = fixture(0);
        let cmd = receive(&mut db, &a, 9);
        let mut p = PrivilegedRemoteApplyConnection::from_connection(db).unwrap();
        assert_eq!(
            sync::apply_verified_received_content_note_ipc(&mut p, cmd).unwrap(),
            "applied"
        );
        let mut d = a.clone();
        d["event"]["header"]["event_id"] = json!(id(860));
        d["event"]["header"]["parent_event_id"] = a["event"]["header"]["event_id"].clone();
        d["event"]["header"]["revision"] = json!(2);
        d["event"]["header"]["operation"] = json!("delete");
        d["event"]["header"]["deleted_at"] = json!(NOW);
        d["event"]["mutation"] = json!("delete");
        let n = d["event"]["note"].as_object_mut().unwrap();
        n.retain(|k, _| {
            [
                "id",
                "project_id",
                "stage_id",
                "source_type",
                "source_map_id",
                "source_node_id",
                "content_format",
            ]
            .contains(&k.as_str())
        });
        n.insert("deleted_at".into(), json!(NOW));
        let cmd = receive(p.connection_mut_for_test(), &d, 10);
        assert_eq!(
            sync::apply_verified_received_content_note_ipc(&mut p, cmd).unwrap(),
            "applied"
        );
        assert_eq!(ack(p.connection_mut_for_test()), 10);
        let mut e = a.clone();
        e["event"]["header"]["event_id"] = json!(id(861));
        e["event"]["header"]["parent_event_id"] = json!(id(860));
        e["event"]["header"]["revision"] = json!(3);
        e["event"]["mutation"] = json!("update");
        e["event"]["note"]["content_format"] = json!("html");
        let cmd = receive(p.connection_mut_for_test(), &e, 11);
        assert_eq!(
            sync::apply_verified_received_content_note_ipc(&mut p, cmd).unwrap(),
            "orphan"
        );
        assert_eq!(ack(p.connection_mut_for_test()), 10);
        assert_eq!(
            p.connection()
                .query_row(
                    "SELECT blocker FROM cloud_content_note_receipts WHERE server_sequence=11",
                    [],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
            "content_note_scope_mismatch"
        );
        assert_eq!(
            p.connection()
                .query_row("SELECT count(*) FROM notes", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        drop(p);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn content_note_sync_unsupported_local_rows_are_retained_without_note_authority() {
        for map_owned in [false, true] {
            let (mut db, path) = database();
            let mut local = fixture(0)["event"]["note"].clone();
            local["project_id"] = json!("L1");
            let code = if map_owned {
                local["source_type"] = json!("mindmap");
                local["source_map_id"] = json!("M1");
                local["source_node_id"] = json!("node1");
                "content_note_map_owned"
            } else {
                local["metadata"] = json!({"unclassified":"retained"});
                "content_note_unsupported_source"
            };
            let raw = local.to_string();
            db.execute(
                "INSERT INTO notes VALUES('N1','L1',NULL,?1,?2)",
                params![NOW, raw],
            )
            .unwrap();
            let cmd = receive(&mut db, &fixture(0), 9);
            let mut p = PrivilegedRemoteApplyConnection::from_connection(db).unwrap();
            assert_eq!(
                sync::apply_verified_received_content_note_ipc(&mut p, cmd).unwrap(),
                "orphan"
            );
            assert_eq!(
                p.connection()
                    .query_row("SELECT payload_json FROM notes WHERE id='N1'", [], |r| {
                        r.get::<_, String>(0)
                    })
                    .unwrap(),
                raw
            );
            assert_eq!(
                p.connection()
                    .query_row("SELECT blocker FROM cloud_content_note_receipts", [], |r| r
                        .get::<_, String>(0))
                    .unwrap(),
                code
            );
            assert_eq!(
                p.connection()
                    .query_row(
                        "SELECT count(*) FROM cloud_sync_outbox WHERE entity_type='note'",
                        [],
                        |r| r.get::<_, i64>(0)
                    )
                    .unwrap(),
                0
            );
            assert_eq!(ack(p.connection_mut_for_test()), 8);
            drop(p);
            let mut reopened = crate::sqlite::open_database(&path).unwrap();
            assert_eq!(ack(&mut reopened), 8);
            assert_eq!(
                reopened
                    .query_row("SELECT payload_json FROM notes WHERE id='N1'", [], |r| {
                        r.get::<_, String>(0)
                    })
                    .unwrap(),
                raw
            );
            drop(reopened);
            std::fs::remove_file(path).unwrap();
        }
    }
}
