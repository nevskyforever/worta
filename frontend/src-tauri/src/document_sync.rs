//! Explicit codec10 admission and full-document causal authority on the shared transport.
use crate::{document_codec as codec, project_metadata_sync as metadata, sqlite};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashSet;
#[derive(Debug)]
pub(crate) enum Error {
    Code(String),
    Sql(rusqlite::Error),
    Storage(sqlite::StorageError),
    Metadata(metadata::MetadataError),
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Code(s) => s,
            Self::Sql(_) | Self::Storage(_) => "document_storage_unavailable",
            Self::Metadata(_) => "document_dependency_unavailable",
        })
    }
}
impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Sql(e) => Some(e),
            Self::Storage(e) => Some(e),
            Self::Metadata(metadata::MetadataError::Database(e)) => Some(e),
            Self::Metadata(_) => None,
            Self::Code(_) => None,
        }
    }
}
impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Self::Sql(e)
    }
}
impl From<sqlite::StorageError> for Error {
    fn from(e: sqlite::StorageError) -> Self {
        Self::Storage(e)
    }
}
impl From<metadata::MetadataError> for Error {
    fn from(e: metadata::MetadataError) -> Self {
        Self::Metadata(e)
    }
}
impl From<String> for Error {
    fn from(s: String) -> Self {
        Self::Code(s)
    }
}
type Result<T> = std::result::Result<T, Error>;
fn fail<T>(s: &str) -> Result<T> {
    Err(Error::Code(s.into()))
}
fn parse(s: &str) -> Result<Value> {
    if s.len() > codec::MAX_FRAME_BYTES {
        return fail("document_resource_limit");
    }
    let (mut depth, mut quoted, mut escape) = (0usize, false, false);
    for b in s.bytes() {
        if quoted {
            if escape {
                escape = false
            } else if b == b'\\' {
                escape = true
            } else if b == b'"' {
                quoted = false
            }
        } else {
            match b {
                b'"' => quoted = true,
                b'{' | b'[' => {
                    depth += 1;
                    if depth > 160 {
                        return fail("document_resource_limit");
                    }
                }
                b'}' | b']' => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
    }
    codec::bounded_stack(|| {
        let mut parser = serde_json::Deserializer::from_str(s);
        parser.disable_recursion_limit();
        let value =
            Value::deserialize(&mut parser).map_err(|_| "invalid_document_payload".to_string())?;
        parser
            .end()
            .map_err(|_| "invalid_document_payload".to_string())?;
        Ok(value)
    })
    .map_err(Error::from)
}

fn rows(db: &Connection, sql: &str, args: impl rusqlite::Params) -> Result<Vec<String>> {
    let mut q = db.prepare(sql)?;
    let out = q
        .query_map(args, |r| r.get(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(out)
}
pub(crate) fn tips(db: &Connection, a: &str, p: &str, id: &str) -> Result<Vec<String>> {
    rows(db,"SELECT event_id FROM cloud_document_tips WHERE account_id=?1 AND project_id=?2 AND entity_id=?3 ORDER BY event_id",params![a,p,id])
}

pub(crate) fn timestamp(value: &str) -> Result<String> {
    if metadata::timestamp(value) {
        return Ok(value.into());
    }
    let body = value
        .strip_suffix('Z')
        .ok_or_else(|| Error::Code("invalid_document_payload".into()))?;
    let (seconds, fraction) = body.split_once('.').unwrap_or((body, ""));
    if seconds.len() != 19 || fraction.len() > 6 || !fraction.bytes().all(|b| b.is_ascii_digit()) {
        return fail("invalid_document_payload");
    }
    let canonical = format!("{}.{:0<6}Z", seconds, fraction);
    if !metadata::timestamp(&canonical) {
        return fail("invalid_document_payload");
    }
    Ok(canonical)
}

fn store(
    tx: &Transaction<'_>,
    a: &str,
    e: &codec::Event,
    frame: &[u8],
    state: &str,
    seq: Option<i64>,
) -> Result<()> {
    let h = &e.header;
    tx.execute("INSERT INTO cloud_document_events(account_id,event_id,project_id,entity_id,stage_id,canonical_frame,parents_json,revision,state,server_sequence) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",params![a,h.event_id,h.project_id,h.entity_id,h.stage_id,frame,json!(h.parents).to_string(),h.revision,state,seq])?;
    Ok(())
}
fn queue(tx: &Transaction<'_>, a: &str, e: &codec::Event) -> Result<()> {
    let h = &e.header;
    let frame = codec::encode(e)?;
    store(tx, a, e, &frame, "unsealed", None)?;
    tx.execute("INSERT INTO cloud_sync_outbox(event_id,account_id,device_id,project_id,entity_id,entity_type,operation,revision,updated_at,deleted_at,created_at,parent_event_id,local_ordinal,lifecycle) VALUES(?1,?2,?3,?4,?5,'document','event',?6,?7,NULL,?7,?8,(SELECT COALESCE(MAX(local_ordinal),0)+1 FROM cloud_sync_outbox WHERE account_id=?2),'unsealed')",params![h.event_id,a,h.device_id,h.project_id,h.entity_id,h.revision,h.updated_at,h.parents.first()])?;
    Ok(())
}
fn pending_owner(db: &Connection, a: &str, p: &str, id: &str) -> Result<bool> {
    Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM cloud_document_events e JOIN cloud_sync_outbox o ON o.account_id=e.account_id AND o.event_id=e.event_id WHERE e.account_id=?1 AND e.project_id=?2 AND e.entity_id=?3 AND o.lifecycle IN ('unsealed','sealed','accepted') AND e.state IN ('unsealed','sealed','waiting'))",params![a,p,id],|r|r.get(0))?)
}

pub(crate) fn pending(db: &Connection, a: &str, device: &str, sealed: bool) -> Result<Vec<Value>> {
    let mut q=db.prepare("SELECT e.canonical_frame,o.nonce,o.ciphertext FROM cloud_document_events e JOIN cloud_sync_outbox b ON b.account_id=e.account_id AND b.event_id=e.event_id LEFT JOIN cloud_sync_event_objects o ON o.account_id=e.account_id AND o.event_id=e.event_id WHERE e.account_id=?1 AND b.device_id=?2 AND e.state IN ('unsealed','sealed') AND b.lifecycle IN ('unsealed','sealed','accepted') AND (?3=0 AND o.event_id IS NULL OR ?3=1 AND o.event_id IS NOT NULL) ORDER BY e.retry_ordinal,b.local_ordinal LIMIT 8")?;
    let items = q
        .query_map(params![a, device, sealed], |r| {
            Ok((
                r.get::<_, Vec<u8>>(0)?,
                r.get::<_, Option<Vec<u8>>>(1)?,
                r.get::<_, Option<Vec<u8>>>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    items
        .into_iter()
        .map(|(frame, n, c)| {
            Ok(json!({"event":codec::decode(&frame)?,"frame":frame,"nonce":n,"ciphertext":c}))
        })
        .collect()
}
pub(crate) fn seal(
    db: &mut Connection,
    a: &str,
    id: &str,
    frame: &[u8],
    nonce: &[u8],
    ciphertext: &[u8],
) -> Result<()> {
    if nonce.len() != 24 || !(16..=codec::MAX_FRAME_BYTES + 16).contains(&ciphertext.len()) {
        return fail("invalid_document_payload");
    }
    let compression_view =
        crate::frame_compression::canonical_view(frame).map_err(|c| Error::Code(c.into()))?;
    let frame = compression_view.as_ref();
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let old:Vec<u8>=tx.query_row("SELECT canonical_frame FROM cloud_document_events WHERE account_id=?1 AND event_id=?2 AND state IN ('unsealed','sealed')",params![a,id],|r|r.get(0))?;
    if old != frame {
        return fail("document_sealing_stale");
    }
    let object:Option<(Vec<u8>,Vec<u8>)>=tx.query_row("SELECT nonce,ciphertext FROM cloud_sync_event_objects WHERE account_id=?1 AND event_id=?2",params![a,id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    if let Some((n, c)) = object {
        if n != nonce || c != ciphertext {
            return fail("document_exact_replay_mismatch");
        }
    } else {
        tx.execute(
            "INSERT INTO cloud_sync_event_objects VALUES(?1,?2,1,1,?3,?4,?5)",
            params![
                a,
                id,
                nonce,
                ciphertext,
                codec::decode(frame)?.header.updated_at
            ],
        )?;
        tx.execute("UPDATE cloud_sync_outbox SET lifecycle='sealed' WHERE account_id=?1 AND event_id=?2 AND lifecycle='unsealed'",params![a,id])?;
        tx.execute(
            "UPDATE cloud_document_events SET state='sealed' WHERE account_id=?1 AND event_id=?2",
            params![a, id],
        )?;
        tx.execute("UPDATE cloud_document_migrations SET lifecycle='self_echo_pending' WHERE account_id=?1 AND project_id=(SELECT project_id FROM cloud_document_events WHERE account_id=?1 AND event_id=?2) AND entity_id=(SELECT entity_id FROM cloud_document_events WHERE account_id=?1 AND event_id=?2) AND lifecycle='publication_pending'",params![a,id])?;
    }
    tx.commit()?;
    Ok(())
}
pub(crate) fn receipt(
    db: &mut Connection,
    a: &str,
    device: &str,
    id: &str,
    seq: i64,
    duplicate: bool,
    now: &str,
) -> Result<()> {
    if !(1..=9_007_199_254_740_991).contains(&seq) || !metadata::timestamp(now) {
        return fail("invalid_document_receipt");
    }
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let valid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_outbox WHERE account_id=?1 AND device_id=?2 AND event_id=?3 AND entity_type='document' AND lifecycle IN ('sealed','accepted'))",params![a,device,id],|r|r.get(0))?;
    if !valid {
        return fail("document_scope_mismatch");
    }
    let prior:Option<i64>=tx.query_row("SELECT server_sequence FROM cloud_sync_upload_receipts WHERE account_id=?1 AND event_id=?2",params![a,id],|r|r.get(0)).optional()?;
    if prior.is_some_and(|n| n != seq) {
        return fail("document_exact_replay_mismatch");
    }
    if prior.is_none() {
        tx.execute(
            "INSERT INTO cloud_sync_upload_receipts VALUES(?1,?2,?3,?4,?5,?6)",
            params![a, id, device, seq, duplicate, now],
        )?;
    }
    tx.execute(
        "UPDATE cloud_sync_outbox SET lifecycle='accepted' WHERE account_id=?1 AND event_id=?2",
        params![a, id],
    )?;
    tx.execute("UPDATE cloud_document_events SET retry_ordinal=(SELECT COALESCE(MAX(retry_ordinal),0)+1 FROM cloud_document_events WHERE account_id=?1) WHERE account_id=?1 AND event_id=?2",params![a,id])?;
    tx.commit()?;
    Ok(())
}
fn parent_revision(db: &Connection, a: &str, e: &codec::Event) -> Result<i64> {
    let h = &e.header;
    let mut max = 0;
    for id in &h.parents {
        let row:Option<i64>=db.query_row("SELECT revision FROM cloud_document_events WHERE account_id=?1 AND event_id=?2 AND project_id=?3 AND entity_id=?4 AND state IN ('applied','conflict_preserved')",params![a,id,h.project_id,h.entity_id],|r|r.get(0)).optional()?;
        let Some(r) = row else {
            return fail("document_parent_unknown");
        };
        max = max.max(r)
    }
    if h.revision != max + 1 {
        return fail("document_parent_revision_invalid");
    }
    Ok(max)
}
fn covered(db: &Connection, a: &str, descendant: &str, ancestor: &str) -> Result<bool> {
    let mut pending = vec![descendant.to_string()];
    let mut seen = HashSet::new();
    while let Some(id) = pending.pop() {
        if id == ancestor {
            return Ok(true);
        }
        if !seen.insert(id.clone()) {
            continue;
        }
        if seen.len() > 4096 {
            return fail("document_causal_proof_limit");
        }
        let raw: Option<String> = db
            .query_row(
                "SELECT parents_json FROM cloud_document_events WHERE account_id=?1 AND event_id=?2",
                params![a, id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(raw) = raw {
            let parents: Vec<String> = serde_json::from_str(&raw)
                .map_err(|_| Error::Code("document_parent_invalid".into()))?;
            pending.extend(parents)
        }
    }
    Ok(false)
}
struct ApplyPlan {
    event: codec::Event,
    seq: i64,
    nonce: Vec<u8>,
    ciphertext: Vec<u8>,
    outcome: String,
    projection: Option<Value>,
    new_tips: Vec<String>,
    own: bool,
}

pub(crate) fn source(db: &Connection, p: &str, id: &str) -> Result<Value> {
    let row:Option<(String,Option<String>,String,String,String,Option<String>,String)>=db.query_row("SELECT project_id,stage_id,title,content_json,content_format,created_at,extensions_json FROM documents WHERE id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?))).optional()?;
    let Some((project, stage, title, content, format, created, extensions)) = row else {
        return Ok(Value::Null);
    };
    if project != p {
        return fail("document_cross_project_move_blocked");
    }
    Ok(
        json!({"id":id,"project_id":project,"stage_id":stage,"title":title,"content_json":parse(&content)?,"content_format":format,"created_at":created,"extensions":parse(&extensions)?}),
    )
}
pub(crate) fn current(db: &Connection, p: &str, id: &str) -> Result<Value> {
    let mut v = source(db, p, id)?;
    if v.is_null() {
        return Ok(v);
    }
    if let Some(t) = v["created_at"].as_str() {
        v["created_at"] = json!(timestamp(t)?)
    }
    codec::validate_document(&v)?;
    Ok(v)
}
fn header(
    db: &Connection,
    a: &str,
    p: &str,
    id: &str,
    s: Option<&str>,
    parents: Vec<String>,
    now: &str,
) -> Result<codec::Header> {
    let view = metadata::authority_view(db, a, p)?;
    if view.state != "active" {
        return fail("project_metadata_authority_unresolved");
    }
    let (user,device,boot):(String,String,String)=db.query_row("SELECT b.canonical_user_id,s.device_id,p.bootstrap_id FROM cloud_account_bindings b JOIN cloud_sync_state s ON s.account_id=b.local_account_id JOIN cloud_sync_project_bootstraps p ON p.account_id=s.account_id WHERE s.account_id=?1 AND p.project_id=?2",params![a,p],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    let refs = if let Some(s) = s {
        rows(db,"SELECT event_id FROM cloud_sync_structural_tips WHERE account_id=?1 AND project_id=?2 AND entity_type='stage' AND entity_id=?3 ORDER BY event_id",params![a,p,s])?
    } else {
        vec![]
    };
    let mut rev = 0;
    for parent in &parents {
        let r:i64=db.query_row("SELECT revision FROM cloud_document_events WHERE account_id=?1 AND project_id=?2 AND entity_id=?3 AND event_id=?4 AND state IN ('applied','conflict_preserved')",params![a,p,id,parent],|r|r.get(0))?;
        rev = rev.max(r)
    }
    let h = codec::Header {
        account_id: user,
        device_id: device,
        project_id: p.into(),
        stage_id: s.map(str::to_string),
        entity_id: id.into(),
        event_id: metadata::new_event_id()?,
        bootstrap_id: boot,
        metadata_event_id: view
            .head_event_id
            .ok_or_else(|| Error::Code("project_metadata_authority_unresolved".into()))?,
        stage_event_ids: refs,
        operation: if parents.is_empty() {
            "create"
        } else if parents.len() > 1 {
            "resolution"
        } else {
            "update"
        }
        .into(),
        parents,
        revision: rev + 1,
        generation: rev + 1,
        updated_at: timestamp(now)?,
    };
    codec::dependencies_ready(
        db,
        a,
        &h.account_id,
        &h.device_id,
        &event(h.clone(), Value::Null),
    )?;
    Ok(h)
}
fn event(mut h: codec::Header, document: Value) -> codec::Event {
    let deleted = document.is_null();
    if deleted && h.operation == "update" {
        h.operation = "delete".into()
    }
    codec::Event {
        version: 1,
        deleted_at: if deleted {
            json!(h.updated_at)
        } else {
            Value::Null
        },
        header: h,
        mutation: if deleted { "delete" } else { "upsert" }.into(),
        document,
    }
}
fn migration_blocker(db: &Connection, p: &str) -> Result<Option<String>> {
    let marker: Option<String> = db
        .query_row(
            "SELECT value_json FROM document_metadata WHERE key='documents_json_migration'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if marker
        .as_deref()
        .and_then(|raw| parse(raw).ok())
        .is_none_or(|m| m["status"] != "complete")
    {
        return Ok(Some("document_legacy_migration_incomplete".into()));
    }
    let orphans:i64=db.query_row("SELECT count(*) FROM document_migration_orphans WHERE CASE WHEN json_valid(payload_json) THEN json_extract(payload_json,'$.project_id') ELSE NULL END IS NULL OR CASE WHEN json_valid(payload_json) THEN json_extract(payload_json,'$.project_id') ELSE NULL END=?1",[p],|r|r.get(0))?;
    Ok(if orphans > 0 {
        Some("document_migration_orphan".into())
    } else {
        None
    })
}
fn collision(db: &Connection, p: &str, id: &str, s: Option<&str>) -> Result<()> {
    let collision: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM documents WHERE project_id=?1 AND stage_id IS ?2 AND id!=?3)",
        params![p, s, id],
        |r| r.get(0),
    )?;
    if collision {
        return fail("document_scope_occupied");
    }
    Ok(())
}
fn write_projection(tx: &Transaction<'_>, p: &str, id: &str, doc: &Value, now: &str) -> Result<()> {
    if doc.is_null() {
        tx.execute("DELETE FROM document_bindings WHERE document_id=?1", [id])?;
        tx.execute(
            "DELETE FROM documents WHERE id=?1 AND project_id=?2",
            params![id, p],
        )?;
        return Ok(());
    }
    codec::validate_document(doc)?;
    if doc["id"] != id || doc["project_id"] != p {
        return fail("document_scope_mismatch");
    }
    let s = doc["stage_id"].as_str();
    collision(tx, p, id, s)?;
    tx.execute("INSERT INTO documents(id,scope_key,project_id,stage_id,title,content_json,content_format,created_at,updated_at,revision,extensions_json) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,0,'{}') ON CONFLICT(id) DO UPDATE SET scope_key=excluded.scope_key,stage_id=excluded.stage_id,title=excluded.title,content_json=excluded.content_json,content_format=excluded.content_format,created_at=excluded.created_at,updated_at=excluded.updated_at,revision=documents.revision+1",params![id,format!("{}:{}",p,s.unwrap_or("project")),p,s,doc["title"].as_str(),codec::canonical(&doc["content_json"]),doc["content_format"].as_str(),doc["created_at"].as_str(),now])?;
    tx.execute("UPDATE document_bindings SET sync_state='local_changed',expected_external_hash=NULL WHERE document_id=?1",[id])?;
    Ok(())
}
fn draft(tx: &Transaction<'_>, a: &str, p: &str, id: &str, doc: &Value, now: &str) -> Result<()> {
    tx.execute("INSERT INTO cloud_document_local_drafts VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(account_id,project_id,entity_id) DO UPDATE SET snapshot_json=excluded.snapshot_json,projection_json=excluded.projection_json,updated_at=excluded.updated_at",params![a,p,id,doc.to_string(),json!({"document":doc}).to_string(),now])?;
    Ok(())
}
pub(crate) fn expected(db: &Connection, p: &str, id: &str) -> Result<Value> {
    let a: Option<String> = db
        .query_row(
            "SELECT account_id FROM cloud_document_migrations WHERE project_id=?1 AND entity_id=?2",
            params![p, id],
            |r| r.get(0),
        )
        .optional()?;
    let heads = if let Some(a) = a {
        tips(db, &a, p, id)?
    } else {
        vec![]
    };
    Ok(json!({"document_id":id,"document":source(db,p,id)?,"tips":heads}))
}
/// Ordinary desktop operations preserve extensions and require editor lifetime CAS.
pub(crate) fn normal(
    tx: &Transaction<'_>,
    p: &str,
    id: &str,
    mut doc: Value,
    expected_head: Option<&Value>,
    now: &str,
) -> Result<bool> {
    let admitted:Option<(String,String)>=tx.query_row("SELECT account_id,lifecycle FROM cloud_document_migrations WHERE project_id=?1 AND entity_id=?2",params![p,id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    let (a, state) = if let Some(value) = admitted {
        value
    } else {
        let a: Option<String> = tx
            .query_row(
                "SELECT account_id FROM cloud_document_project_consent WHERE project_id=?1",
                [p],
                |r| r.get(0),
            )
            .optional()?;
        let Some(a) = a else { return Ok(false) };
        if doc.is_null() {
            return fail("document_missing");
        }
        if expected_head != Some(&expected(tx, p, id)?) {
            return fail("document_stale_heads");
        }
        if let Some(b) = migration_blocker(tx, p)? {
            return fail(&b);
        }
        if let Some(created) = doc["created_at"].as_str() {
            doc["created_at"] = json!(timestamp(created)?);
        }
        codec::validate_document(&doc)?;
        if doc["id"] != id || doc["project_id"] != p {
            return fail("document_scope_mismatch");
        }
        collision(tx, p, id, doc["stage_id"].as_str())?;
        let h = header(tx, &a, p, id, doc["stage_id"].as_str(), vec![], now)?;
        let e = event(h, doc.clone());
        let cid = metadata::new_event_id()?;
        tx.execute(
            "INSERT INTO cloud_document_candidates VALUES(?1,?2,?3,?4,?5,?6,?7,?7,NULL)",
            params![
                cid,
                a,
                p,
                id,
                doc["stage_id"].as_str(),
                e.header.event_id,
                doc.to_string()
            ],
        )?;
        tx.execute("INSERT INTO cloud_document_migrations VALUES(?1,?2,?3,?4,'publication_pending',?5,NULL)",params![a,p,id,doc["stage_id"].as_str(),cid])?;
        draft(tx, &a, p, id, &doc, now)?;
        write_projection(tx, p, id, &doc, now)?;
        queue(tx, &a, &e)?;
        return Ok(true);
    };
    if let Some(created) = doc["created_at"].as_str() {
        doc["created_at"] = json!(timestamp(created)?);
    }
    if state == "blocked" {
        return fail("document_migration_blocked");
    }
    if state == "conflict" {
        return fail("document_conflict");
    }
    if expected_head != Some(&expected(tx, p, id)?) {
        return fail("document_stale_heads");
    }
    if let Some(blocker) = migration_blocker(tx, p)? {
        return fail(&blocker);
    }
    let old = current(tx, p, id)?;
    let s = if doc.is_null() {
        old["stage_id"].as_str()
    } else {
        doc["stage_id"].as_str()
    };
    if !old.is_null() {
        header(
            tx,
            &a,
            p,
            id,
            old["stage_id"].as_str(),
            tips(tx, &a, p, id)?,
            now,
        )?;
    }
    if !doc.is_null() {
        codec::validate_document(&doc)?;
        if doc["id"] != id || doc["project_id"] != p {
            return fail("document_cross_project_move_blocked");
        }
        collision(tx, p, id, s)?;
        header(tx, &a, p, id, s, tips(tx, &a, p, id)?, now)?;
    }
    if old == doc {
        return Ok(true);
    }
    draft(tx, &a, p, id, &doc, now)?;
    write_projection(tx, p, id, &doc, now)?;
    if !pending_owner(tx, &a, p, id)? {
        let parents = tips(tx, &a, p, id)?;
        if parents.len() != 1 {
            return fail("document_conflict");
        }
        queue(tx, &a, &event(header(tx, &a, p, id, s, parents, now)?, doc))?;
    }
    Ok(true)
}
pub(crate) fn advance(db: &mut Connection, a: &str, now: &str) -> Result<()> {
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let ids=rows(&tx,"SELECT entity_id FROM cloud_document_local_drafts WHERE account_id=?1 ORDER BY updated_at,entity_id LIMIT 8",[a])?;
    for id in ids {
        let(p,raw):(String,String)=tx.query_row("SELECT project_id,snapshot_json FROM cloud_document_local_drafts WHERE account_id=?1 AND entity_id=?2",params![a,id],|r|Ok((r.get(0)?,r.get(1)?)))?;
        if pending_owner(&tx, a, &p, &id)? {
            continue;
        }
        let state: String = tx.query_row(
            "SELECT lifecycle FROM cloud_document_migrations WHERE account_id=?1 AND entity_id=?2",
            params![a, id],
            |r| r.get(0),
        )?;
        if state != "active" {
            continue;
        }
        let doc = parse(&raw)?;
        let projection:String=tx.query_row("SELECT snapshot_json FROM cloud_document_projection WHERE account_id=?1 AND entity_id=?2",params![a,id],|r|r.get(0))?;
        if parse(&projection)? == doc {
            tx.execute(
                "DELETE FROM cloud_document_local_drafts WHERE account_id=?1 AND entity_id=?2",
                params![a, id],
            )?;
            continue;
        }
        let old = parse(&projection)?;
        let s = if doc.is_null() {
            old["stage_id"].as_str()
        } else {
            doc["stage_id"].as_str()
        };
        queue(
            &tx,
            a,
            &event(
                header(&tx, a, &p, &id, s, tips(&tx, a, &p, &id)?, now)?,
                doc,
            ),
        )?;
    }
    tx.commit()?;
    Ok(())
}
pub(crate) fn begin(
    db: &mut Connection,
    scope: &metadata::MetadataScope,
    p: &str,
    now: &str,
) -> Result<Value> {
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    metadata::assert_runtime_scope(
        &tx,
        &scope.account_id,
        &scope.canonical_user_id,
        &scope.device_id,
    )?;
    let bound:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_project_bindings WHERE account_id=?1 AND project_id=?2)",params![scope.account_id,p],|r|r.get(0))?;
    if !bound {
        return fail("document_project_not_connected");
    }
    tx.execute(
        "INSERT OR IGNORE INTO cloud_document_project_consent VALUES(?1,?2,?3)",
        params![scope.account_id, p, now],
    )?;
    if metadata::authority_view(&tx, &scope.account_id, p)?.state != "active" {
        return fail("project_metadata_authority_unresolved");
    }
    let global = migration_blocker(&tx, p)?;
    if let Some(code) = &global {
        let evidence = rows(
            &tx,
            "SELECT payload_json FROM document_migration_orphans ORDER BY source_key",
            [],
        )?;
        tx.execute("INSERT INTO cloud_document_project_blockers VALUES(?1,?2,?3,?4) ON CONFLICT(account_id,project_id) DO UPDATE SET blocker=excluded.blocker,evidence_json=excluded.evidence_json",params![scope.account_id,p,code,json!(evidence).to_string()])?;
    } else {
        tx.execute(
            "DELETE FROM cloud_document_project_blockers WHERE account_id=?1 AND project_id=?2",
            params![scope.account_id, p],
        )?;
    }
    for id in rows(
        &tx,
        "SELECT id FROM documents WHERE project_id=?1 ORDER BY id",
        [p],
    )? {
        let existing:Option<String>=tx.query_row("SELECT lifecycle FROM cloud_document_migrations WHERE account_id=?1 AND project_id=?2 AND entity_id=?3",params![scope.account_id,p,id],|r|r.get(0)).optional()?;
        if existing.as_ref().is_some_and(|s| s != "blocked") {
            continue;
        }
        let src = match source(&tx, p, &id) {
            Ok(value) => value,
            Err(_) => {
                let raw:(String,String,String,String,String,Option<String>,Option<String>)=tx.query_row("SELECT title,content_json,content_format,extensions_json,project_id,stage_id,created_at FROM documents WHERE id=?1",[&id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?)))?;
                json!({"id":id,"project_id":raw.4,"stage_id":raw.5,"retained_raw":{"title":raw.0,"content_json":raw.1,"content_format":raw.2,"extensions_json":raw.3,"created_at":raw.6}})
            }
        };
        let cid = metadata::new_event_id()?;
        let capture = (|| -> Result<codec::Event> {
            if let Some(code) = &global {
                return fail(code);
            }
            let doc = current(&tx, p, &id)?;
            let h = header(
                &tx,
                &scope.account_id,
                p,
                &id,
                doc["stage_id"].as_str(),
                tips(&tx, &scope.account_id, p, &id)?,
                now,
            )?;
            let e = event(h, doc);
            codec::encode(&e)?;
            Ok(e)
        })();
        let (e, blocker) = match capture {
            Ok(e) => (Some(e), None),
            Err(e) => (None, Some(e.to_string())),
        };
        tx.execute(
            "INSERT INTO cloud_document_candidates VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                cid,
                scope.account_id,
                p,
                id,
                src["stage_id"].as_str(),
                e.as_ref().map(|e| e.header.event_id.as_str()),
                src.to_string(),
                e.as_ref().map(|e| e.document.to_string()),
                blocker
            ],
        )?;
        tx.execute("INSERT INTO cloud_document_migrations VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(account_id,project_id,entity_id) DO UPDATE SET lifecycle=excluded.lifecycle,candidate_id=excluded.candidate_id,blocker=excluded.blocker",params![scope.account_id,p,id,src["stage_id"].as_str(),if e.is_some(){"captured"}else{"blocked"},cid,blocker])?;
        if let Some(e) = e {
            queue(&tx, &scope.account_id, &e)?;
            tx.execute("UPDATE cloud_document_migrations SET lifecycle='publication_pending' WHERE account_id=?1 AND entity_id=?2",params![scope.account_id,id])?;
        }
    }
    tx.commit()?;
    view(db, scope, p)
}
pub(crate) fn view(db: &Connection, scope: &metadata::MetadataScope, p: &str) -> Result<Value> {
    metadata::assert_runtime_scope(
        db,
        &scope.account_id,
        &scope.canonical_user_id,
        &scope.device_id,
    )?;
    let mut ids=rows(db,"SELECT id FROM documents WHERE project_id=?1 UNION SELECT entity_id FROM cloud_document_migrations WHERE project_id=?1 AND account_id=?2 UNION SELECT entity_id FROM cloud_sync_inbox WHERE project_id=?1 AND account_id=?2 AND entity_type='document' ORDER BY 1",params![p,scope.account_id])?;
    ids.dedup();
    let mut owners = vec![];
    for id in ids {
        let state:Option<(String,Option<String>)>=db.query_row("SELECT lifecycle,blocker FROM cloud_document_migrations WHERE project_id=?1 AND entity_id=?2 AND account_id=?3",params![p,id,scope.account_id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        let heads = tips(db, &scope.account_id, p, &id)?;
        let mut versions = vec![];
        let mut dependency_blocker = None;
        for tip in &heads {
            let frame:Vec<u8>=db.query_row("SELECT canonical_frame FROM cloud_document_events WHERE account_id=?1 AND event_id=?2",params![scope.account_id,tip],|r|r.get(0))?;
            let e = codec::decode(&frame)?;
            if let Err(code) = codec::dependencies_ready(
                db,
                &scope.account_id,
                &scope.canonical_user_id,
                &e.header.device_id,
                &e,
            ) {
                dependency_blocker = Some(code)
            }
            versions.push(json!({"event_id":tip,"revision":e.header.revision,"stage_id":e.header.stage_id,"document":e.document,"mutation":e.mutation}));
        }
        let (local, local_blocker) = match source(db, p, &id) {
            Ok(value) => (value, None),
            Err(error) => (Value::Null, Some(error.to_string())),
        };
        let blocked:Option<String>=db.query_row("SELECT error_code FROM cloud_sync_inbox WHERE account_id=?1 AND project_id=?2 AND entity_id=?3 AND entity_type='document' AND state='orphan' ORDER BY server_sequence LIMIT 1",params![scope.account_id,p,id],|r|r.get(0)).optional()?.flatten();
        let (mut state, mut blocker) = state.unwrap_or(("local".into(), None));
        if let Some(code) = blocked.or(local_blocker).or(dependency_blocker) {
            state = "blocked".into();
            blocker = Some(code)
        }
        owners.push(json!({"entity_id":id,"stage_id":local["stage_id"],"state":state,"blocker":blocker,"tips":heads,"versions":versions,"local":local}));
    }
    let blocker:Option<String>=db.query_row("SELECT blocker FROM cloud_document_project_blockers WHERE account_id=?1 AND project_id=?2",params![scope.account_id,p],|r|r.get(0)).optional()?;
    Ok(json!({"owners":owners,"blocker":blocker}))
}

pub(crate) fn apply(
    db: &mut sqlite::PrivilegedRemoteApplyConnection,
    scope: &metadata::MetadataScope,
    frame: &[u8],
    nonce: &[u8],
    ciphertext: &[u8],
) -> Result<String> {
    let compression_view =
        crate::frame_compression::canonical_view(frame).map_err(|c| Error::Code(c.into()))?;
    let frame = compression_view.as_ref();
    let e = codec::decode(frame)?;
    db.execute_planned_many_once(|tx|->Result<(Vec<sqlite::OwnedRemoteApplyAuthorization>,Option<ApplyPlan>)>{
  metadata::assert_runtime_scope(tx,&scope.account_id,&scope.canonical_user_id,&scope.device_id)?;let a=&scope.account_id;let h=&e.header;
  if h.account_id!=scope.canonical_user_id{return fail("document_scope_mismatch")}
  let seq:Option<i64>=tx.query_row("SELECT i.server_sequence FROM cloud_sync_inbox i JOIN cloud_sync_event_objects o ON o.account_id=i.account_id AND o.event_id=i.event_id WHERE i.account_id=?1 AND i.event_id=?2 AND i.project_id=?3 AND i.entity_id=?4 AND i.entity_type='document' AND i.operation='event' AND i.device_id=?5 AND i.sync_revision=?6 AND i.updated_at=?7 AND i.deleted_at IS NULL AND o.crypto_version=1 AND o.aad_version=1 AND o.nonce=?8 AND o.ciphertext=?9",params![a,h.event_id,h.project_id,h.entity_id,h.device_id,h.revision,h.updated_at,nonce,ciphertext],|r|r.get(0)).optional()?;
  let Some(seq)=seq else{return fail("document_scope_mismatch")};
  let previous:Option<Vec<u8>>=tx.query_row("SELECT canonical_frame FROM cloud_document_events WHERE account_id=?1 AND event_id=?2",params![a,h.event_id],|r|r.get(0)).optional()?;
  if previous.as_ref().is_some_and(|p|p!=frame){return fail("document_exact_replay_mismatch")}
  let applied:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM cloud_document_apply_ledger WHERE account_id=?1 AND event_id=?2 AND server_sequence=?3 AND nonce=?4 AND ciphertext=?5)",params![a,h.event_id,seq,nonce,ciphertext],|r|r.get(0))?;
  if applied{return Ok((vec![],None))}
  if previous.is_none(){store(tx,a,&e,frame,"waiting",Some(seq))?;}else{tx.execute("UPDATE cloud_document_events SET server_sequence=?1 WHERE account_id=?2 AND event_id=?3",params![seq,a,h.event_id])?;}
  let ready=codec::dependencies_ready(tx,a,&scope.canonical_user_id,&h.device_id,&e).map_err(Error::from).and_then(|_|parent_revision(tx,a,&e).and_then(|_|{for parent in &h.parents{let raw:Vec<u8>=tx.query_row("SELECT canonical_frame FROM cloud_document_events WHERE account_id=?1 AND event_id=?2",params![a,parent],|r|r.get(0))?;let parent=codec::decode(&raw)?;codec::dependencies_ready(tx,a,&scope.canonical_user_id,&parent.header.device_id,&parent)?;}Ok(())}));
  if let Err(blocker)=ready.and_then(|_|{if let Some(b)=migration_blocker(tx,&h.project_id)?{return fail(&b)}let src=source(tx,&h.project_id,&h.entity_id)?;if !src.is_null()&&src["extensions"]!=json!({}){return fail("document_unsupported_extension")}Ok(())}) {tx.execute("UPDATE cloud_document_events SET state='waiting',blocker=?1 WHERE account_id=?2 AND event_id=?3",params![blocker.to_string(),a,h.event_id])?;tx.execute("UPDATE cloud_sync_inbox SET state='orphan',error_code=?1 WHERE account_id=?2 AND event_id=?3",params![blocker.to_string(),a,h.event_id])?;return Ok((vec![],None))}
  let own:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_outbox WHERE account_id=?1 AND event_id=?2 AND device_id=?3 AND entity_type='document' AND project_id=?4 AND entity_id=?5 AND lifecycle IN ('sealed','accepted'))",params![a,h.event_id,scope.device_id,h.project_id,h.entity_id],|r|r.get(0))?;
  let old=tips(tx,a,&h.project_id,&h.entity_id)?;let src=source(tx,&h.project_id,&h.entity_id)?;
  let prior:Option<String>=tx.query_row("SELECT snapshot_json FROM cloud_document_projection WHERE account_id=?1 AND project_id=?2 AND entity_id=?3",params![a,h.project_id,h.entity_id],|r|r.get(0)).optional()?;
  let local=current(tx,&h.project_id,&h.entity_id);
  let mut unchanged=match(&prior,&local){(Some(raw),Ok(local))=>parse(raw)?==*local,(None,Ok(local))=>local.is_null(),_=>false};
  let decision:Option<String>=tx.query_row("SELECT expected_local FROM cloud_document_decisions WHERE account_id=?1 AND event_id=?2",params![a,h.event_id],|r|r.get(0)).optional()?;
  let decision_matches=decision.as_ref().is_some_and(|raw|parse(raw).ok()==Some(src.clone()));
  // A remote full-tip resolution may replace an already authenticated local
  // branch. Preserve an unpublished local draft that differs from every parent.
  if h.operation=="resolution" && !unchanged && !pending_owner(tx,a,&h.project_id,&h.entity_id)? {
   if let Ok(local)=&local {for parent in &h.parents {let raw:Vec<u8>=tx.query_row("SELECT canonical_frame FROM cloud_document_events WHERE account_id=?1 AND event_id=?2",params![a,parent],|r|r.get(0))?;if codec::decode(&raw)?.document==*local {unchanged=true;break}}}
  }
  let conflict=old!=h.parents || (!own&&!unchanged&&!decision_matches) || (decision.is_some()&&!decision_matches);
  if !unchanged && (!own||conflict) {tx.execute("INSERT OR IGNORE INTO cloud_document_local_candidates VALUES(?1,?2,?3,?4,?5)",params![a,h.event_id,h.project_id,h.entity_id,src.to_string()])?;}
  let mut new_tips=Vec::new();for tip in old{if !covered(tx,a,&h.event_id,&tip)?{new_tips.push(tip)}}new_tips.push(h.event_id.clone());new_tips.sort();new_tips.dedup();
  let plan=if conflict || own&&decision.is_none()&&local.as_ref().is_ok_and(|local|local!=&e.document){None}else{match projection(tx,&h.project_id,&h.entity_id,&e.document){Ok(p)=>Some(p),Err(blocker)=>{tx.execute("UPDATE cloud_document_events SET state='waiting',blocker=?1 WHERE account_id=?2 AND event_id=?3",params![blocker.to_string(),a,h.event_id])?;tx.execute("UPDATE cloud_sync_inbox SET state='orphan',error_code=?1 WHERE account_id=?2 AND event_id=?3",params![blocker.to_string(),a,h.event_id])?;return Ok((vec![],None))}}};
  let auth=plan.as_ref().map(|plan|sqlite::OwnedRemoteApplyAuthorization{event_id:h.event_id.clone(),account_id:a.clone(),project_id:h.project_id.clone(),entity_id:h.entity_id.clone(),operation:"upsert".into(),payload_json:Some(plan.to_string()),prior_payload_json:Some(src.to_string())}).into_iter().collect();
  Ok((auth,Some(ApplyPlan{event:e.clone(),seq,nonce:nonce.to_vec(),ciphertext:ciphertext.to_vec(),outcome:if conflict{"conflict_preserved"}else{"applied"}.into(),projection:plan,new_tips,own})))
 },|tx,plan|->Result<String>{let a=&scope.account_id;let Some(plan)=plan else{let outcome:Option<String>=tx.query_row("SELECT outcome FROM cloud_document_apply_ledger WHERE account_id=?1 AND event_id=?2",params![a,e.header.event_id],|r|r.get(0)).optional()?;return Ok(outcome.unwrap_or("waiting".into()))};let h=&plan.event.header;
  if let Some(projection)=&plan.projection{write_projection(tx,&h.project_id,&h.entity_id,&projection["document"],&h.updated_at)?;tx.execute("DELETE FROM cloud_sync_remote_apply_authorizations WHERE event_id=?1",[&h.event_id])?;}
  tx.execute("DELETE FROM cloud_document_tips WHERE account_id=?1 AND project_id=?2 AND entity_id=?3",params![a,h.project_id,h.entity_id])?;for tip in &plan.new_tips{tx.execute("INSERT INTO cloud_document_tips VALUES(?1,?2,?3,?4)",params![a,h.project_id,h.entity_id,tip])?;}
  if plan.outcome=="applied" {
   let draft:Option<String>=tx.query_row("SELECT snapshot_json FROM cloud_document_local_drafts WHERE account_id=?1 AND project_id=?2 AND entity_id=?3",params![a,h.project_id,h.entity_id],|r|r.get(0)).optional()?;
   if draft.as_ref().is_some_and(|raw|parse(raw).ok()==Some(plan.event.document.clone())) || plan.projection.is_some()&&tx.query_row("SELECT EXISTS(SELECT 1 FROM cloud_document_decisions WHERE account_id=?1 AND event_id=?2)",params![a,h.event_id],|r|r.get::<_,bool>(0))? {tx.execute("DELETE FROM cloud_document_local_drafts WHERE account_id=?1 AND project_id=?2 AND entity_id=?3",params![a,h.project_id,h.entity_id])?;}
  }
  if plan.outcome=="applied"{tx.execute("INSERT INTO cloud_document_projection VALUES(?1,?2,?3,?4,?5) ON CONFLICT(account_id,project_id,entity_id) DO UPDATE SET head_event_id=excluded.head_event_id,snapshot_json=excluded.snapshot_json",params![a,h.project_id,h.entity_id,h.event_id,plan.event.document.to_string()])?;}
  tx.execute("UPDATE cloud_document_events SET state=?1,blocker=NULL WHERE account_id=?2 AND event_id=?3",params![plan.outcome,a,h.event_id])?;
  tx.execute("INSERT INTO cloud_document_apply_ledger VALUES(?1,?2,?3,?4,?5,?6)",params![a,h.event_id,plan.seq,plan.outcome,plan.nonce,plan.ciphertext])?;
  tx.execute("UPDATE cloud_sync_inbox SET state=?1,applied_at=?2,error_code=NULL WHERE account_id=?3 AND event_id=?4",params![if plan.outcome=="applied"{"applied"}else{"conflict"},h.updated_at,a,h.event_id])?;
  let lifecycle=if plan.outcome=="applied"{"active"}else{"conflict"};
  tx.execute("INSERT INTO cloud_document_migrations VALUES(?1,?2,?3,?4,?5,?6,NULL) ON CONFLICT(account_id,project_id,entity_id) DO UPDATE SET lifecycle=excluded.lifecycle,stage_id=excluded.stage_id,blocker=NULL",params![a,h.project_id,h.entity_id,h.stage_id,lifecycle,h.event_id])?;
  if plan.own {let receipt:Option<i64>=tx.query_row("SELECT server_sequence FROM cloud_sync_upload_receipts WHERE account_id=?1 AND event_id=?2",params![a,h.event_id],|r|r.get(0)).optional()?;if receipt.is_some_and(|n|n!=plan.seq){return fail("document_exact_replay_mismatch")}if receipt.is_none(){tx.execute("INSERT INTO cloud_sync_upload_receipts VALUES(?1,?2,?3,?4,1,?5)",params![a,h.event_id,h.device_id,plan.seq,h.updated_at])?;}tx.execute("UPDATE cloud_sync_outbox SET lifecycle='accepted' WHERE account_id=?1 AND event_id=?2",params![a,h.event_id])?;}
  Ok(plan.outcome)
 })
}
pub(crate) fn received(db: &Connection, a: &str, after: i64, limit: i64) -> Result<Vec<Value>> {
    if after < 0 || !(1..=32).contains(&limit) {
        return fail("invalid_document_page");
    }
    let tx = db.unchecked_transaction()?;
    let mut q=tx.prepare("SELECT i.event_id,i.server_sequence,i.device_id,i.project_id,i.entity_id,i.sync_revision,i.updated_at,o.nonce,o.ciphertext FROM cloud_sync_inbox i JOIN cloud_sync_event_objects o ON o.account_id=i.account_id AND o.event_id=i.event_id LEFT JOIN cloud_game_reader_visits v ON v.account_id=i.account_id AND v.event_id=i.event_id WHERE i.account_id=?1 AND i.entity_type='document' AND i.operation='event' AND i.state IN ('received','orphan') AND i.server_sequence>?2 ORDER BY COALESCE(v.ordinal,0),i.server_sequence LIMIT ?3")?;
    let mut rows=q.query_map(params![a,after,limit],|r|Ok(json!({"event_id":r.get::<_,String>(0)?,"server_sequence":r.get::<_,i64>(1)?,"source_device_id":r.get::<_,String>(2)?,"project_id":r.get::<_,String>(3)?,"entity_id":r.get::<_,String>(4)?,"revision":r.get::<_,i64>(5)?,"updated_at":r.get::<_,String>(6)?,"nonce":r.get::<_,Vec<u8>>(7)?,"ciphertext":r.get::<_,Vec<u8>>(8)?})))?.collect::<std::result::Result<Vec<_>,_>>()?;
    drop(q);
    rows.sort_by_key(|r| r["server_sequence"].as_i64().unwrap_or(0));
    crate::sqlite::record_sync_reader_visits(
        &tx,
        a,
        rows.iter().filter_map(|r| r["event_id"].as_str()),
    )?;
    tx.commit()?;
    Ok(rows)
}
pub(crate) fn ack_proven(db: &Connection, a: &str, seq: i64) -> rusqlite::Result<bool> {
    db.query_row("SELECT EXISTS(SELECT 1 FROM cloud_document_apply_ledger l JOIN cloud_document_events e ON e.account_id=l.account_id AND e.event_id=l.event_id JOIN cloud_sync_inbox i ON i.account_id=e.account_id AND i.event_id=e.event_id JOIN cloud_sync_event_objects o ON o.account_id=e.account_id AND o.event_id=e.event_id WHERE l.account_id=?1 AND l.server_sequence=?2 AND e.server_sequence=l.server_sequence AND i.server_sequence=l.server_sequence AND e.state=l.outcome AND i.state=CASE WHEN l.outcome='applied' THEN 'applied' ELSE 'conflict' END AND i.entity_type='document' AND i.operation='event' AND i.project_id=e.project_id AND i.entity_id=e.entity_id AND i.sync_revision=e.revision AND i.device_id=json_extract(CAST(substr(e.canonical_frame,21) AS TEXT),'$.header.device_id') AND i.updated_at=json_extract(CAST(substr(e.canonical_frame,21) AS TEXT),'$.header.updated_at') AND l.nonce=o.nonce AND l.ciphertext=o.ciphertext AND o.crypto_version=1 AND o.aad_version=1 AND i.deleted_at IS NULL)",params![a,seq],|r|r.get(0))
}
fn projection(db: &Connection, p: &str, id: &str, doc: &Value) -> Result<Value> {
    if !doc.is_null() {
        codec::validate_document(doc)?;
        collision(db, p, id, doc["stage_id"].as_str())?;
    }
    let local = source(db, p, id)?;
    if !local.is_null() && local["extensions"] != json!({}) {
        return fail("document_unsupported_extension");
    }
    Ok(json!({"document":doc}))
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Decision {
    pub project_id: String,
    pub document_id: String,
    pub expected_tips: Vec<String>,
    pub expected_local: Value,
    pub selected_event_id: String,
}
pub(crate) fn assert_project_scope(
    db: &Connection,
    scope: &metadata::MetadataScope,
    p: &str,
) -> Result<()> {
    metadata::assert_runtime_scope(
        db,
        &scope.account_id,
        &scope.canonical_user_id,
        &scope.device_id,
    )?;
    let bound:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_project_bindings WHERE account_id=?1 AND project_id=?2)",params![scope.account_id,p],|r|r.get(0))?;
    let foreign:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM cloud_document_migrations WHERE project_id=?1 AND account_id!=?2)",params![p,scope.account_id],|r|r.get(0))?;
    if !bound || foreign {
        return fail("document_scope_mismatch");
    }
    Ok(())
}

pub(crate) fn decide(
    db: &mut Connection,
    scope: &metadata::MetadataScope,
    d: &Decision,
    now: &str,
) -> Result<String> {
    assert_project_scope(db, scope, &d.project_id)?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    metadata::assert_runtime_scope(
        &tx,
        &scope.account_id,
        &scope.canonical_user_id,
        &scope.device_id,
    )?;
    let a = &scope.account_id;
    let parents = tips(&tx, a, &d.project_id, &d.document_id)?;
    let eid = metadata::new_event_id()?;
    tx.execute(
        "INSERT INTO cloud_document_decisions VALUES(?1,?2,?3,?4)",
        params![
            a,
            eid,
            json!(d.expected_tips).to_string(),
            d.expected_local.to_string()
        ],
    )?;
    if parents != d.expected_tips
        || parents.is_empty()
        || source(&tx, &d.project_id, &d.document_id)? != d.expected_local
    {
        tx.commit()?;
        return fail("document_resolution_stale");
    }
    if let Some(b) = migration_blocker(&tx, &d.project_id)? {
        return fail(&b);
    }
    let mut selected_scope: Option<String> = None;
    let selected = if d.selected_event_id == "local" {
        current(&tx, &d.project_id, &d.document_id)?
    } else {
        if !parents.contains(&d.selected_event_id) {
            return fail("document_resolution_invalid");
        }
        let frame:Vec<u8>=tx.query_row("SELECT canonical_frame FROM cloud_document_events WHERE account_id=?1 AND event_id=?2 AND project_id=?3 AND entity_id=?4",params![a,d.selected_event_id,d.project_id,d.document_id],|r|r.get(0))?;
        let decoded = codec::decode(&frame)?;
        selected_scope = decoded.header.stage_id;
        decoded.document
    };
    projection(&tx, &d.project_id, &d.document_id, &selected)?;
    let old = source(&tx, &d.project_id, &d.document_id)?;
    if selected.is_null() && d.selected_event_id == "local" && old.is_null() {
        let frame:Option<Vec<u8>>=tx.query_row("SELECT e.canonical_frame FROM cloud_document_projection p JOIN cloud_document_events e ON e.account_id=p.account_id AND e.event_id=p.head_event_id WHERE p.account_id=?1 AND p.project_id=?2 AND p.entity_id=?3",params![a,d.project_id,d.document_id],|r|r.get(0)).optional()?;
        if let Some(frame) = frame {
            selected_scope = codec::decode(&frame)?.header.stage_id;
        }
    }
    let s = if selected.is_null() {
        selected_scope
            .as_deref()
            .or_else(|| old["stage_id"].as_str())
    } else {
        selected["stage_id"].as_str()
    };
    let mut h = header(&tx, a, &d.project_id, &d.document_id, s, parents, now)?;
    h.event_id = eid.clone();
    queue(&tx, a, &event(h, selected))?;
    tx.commit()?;
    Ok(eid)
}
#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Request {
    View {
        project_id: String,
    },
    Begin {
        project_id: String,
        now: String,
    },
    Pending {
        sealed: bool,
        now: String,
    },
    Seal {
        event_id: String,
        frame: Vec<u8>,
        nonce: Vec<u8>,
        ciphertext: Vec<u8>,
    },
    Receipt {
        event_id: String,
        server_sequence: i64,
        duplicate: bool,
        now: String,
    },
    Received {
        after: i64,
        limit: i64,
    },
    Apply {
        frame: Vec<u8>,
        nonce: Vec<u8>,
        ciphertext: Vec<u8>,
    },
    Block {
        event_id: String,
        nonce: Vec<u8>,
        ciphertext: Vec<u8>,
        code: String,
    },
    Decide {
        decision: Decision,
        now: String,
    },
    Move {
        project_id: String,
        document_id: String,
        stage_id: Option<String>,
        expected: Value,
        now: String,
    },
    Delete {
        project_id: String,
        document_id: String,
        expected: Value,
        now: String,
    },
}
pub(crate) fn move_scope(
    db: &mut Connection,
    p: &str,
    id: &str,
    s: Option<&str>,
    expected: &Value,
    now: &str,
) -> Result<()> {
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut doc = current(&tx, p, id)?;
    if doc.is_null() {
        return fail("document_missing");
    }
    doc["stage_id"] = json!(s);
    if !normal(&tx, p, id, doc, Some(expected), now)? {
        return fail("document_authority_required");
    }
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    const A: &str = "map-test-account";
    const NOW: &str = "2026-10-03T00:00:00.000000Z";
    fn fixture() -> codec::Event {
        let v: Value = serde_json::from_str(include_str!(
            "../../src/cloud/__fixtures__/documentCodecV1.json"
        ))
        .unwrap();
        serde_json::from_value(v["examples"][0]["event"].clone()).unwrap()
    }
    fn scope() -> metadata::MetadataScope {
        let h = fixture().header;
        metadata::MetadataScope {
            account_id: A.into(),
            canonical_user_id: h.account_id,
            device_id: h.device_id,
        }
    }
    fn seed(local: bool) -> (Connection, std::path::PathBuf) {
        let path =
            std::env::temp_dir().join(format!("c18504-{}.db", metadata::new_event_id().unwrap()));
        let mut db = sqlite::open_database(&path).unwrap();
        db.execute("INSERT INTO mirror_state(id,source_format,source_schema_version,sync_status) VALUES(1,'test','1','healthy')",[]).unwrap();
        db.execute(
            "UPDATE storage_ownership SET owner='sqlite' WHERE subsystem IN ('notes','projects')",
            [],
        )
        .unwrap();
        let h = fixture().header;
        db.execute("INSERT INTO projects(id,name,infinite,unit,status,payload_json) VALUES('P1','Project',1,'symbols','active','{}')",[]).unwrap();
        db.execute("INSERT INTO project_order VALUES('P1',0)", [])
            .unwrap();
        db.execute("INSERT INTO cloud_sync_state(account_id,device_id,created_at,updated_at) VALUES(?1,?2,?3,?3)",params![A,h.device_id,NOW]).unwrap();
        db.execute(
            "INSERT INTO cloud_account_bindings VALUES(?1,?2,?3,?3)",
            params![A, h.account_id, NOW],
        )
        .unwrap();
        db.execute(
            "INSERT INTO cloud_sync_project_bindings VALUES('P1',?1,?2,?2)",
            params![A, NOW],
        )
        .unwrap();
        db.execute("INSERT INTO cloud_sync_project_bootstraps(project_id,account_id,device_id,bootstrap_id,mode,phase,created_at,updated_at) VALUES('P1',?1,?2,?3,'upload_existing','prepared',?4,?4)",params![A,h.device_id,h.bootstrap_id,NOW]).unwrap();
        let e = json!({"version":1,"header":{"account_id":h.account_id,"project_id":"P1","entity_id":"P1","device_id":h.device_id,"bootstrap_id":h.bootstrap_id,"event_id":h.metadata_event_id,"revision":1,"generation":1,"operation":"create","parent_event_ids":[],"updated_at":NOW},"metadata":{"name":"Project","goal":null,"infinite":true,"unit":"symbols","status":"active","deadline":null,"personal_goal":0,"auto_freeze":true,"streak_enabled":true,"work_method":"manual","stages_enabled":false,"combine_stage_mindmaps":false},"deleted_at":null});
        db.execute("INSERT INTO cloud_sync_inbox(account_id,event_id,server_sequence,device_id,project_id,entity_id,entity_type,operation,sync_revision,updated_at,state,received_at) VALUES(?1,?2,1,?3,'P1','P1','project_metadata','upsert',1,?4,'received',?4)",params![A,h.metadata_event_id,h.device_id,NOW]).unwrap();
        db.execute(
            "INSERT INTO cloud_sync_event_objects VALUES(?1,?2,1,1,zeroblob(24),zeroblob(16),?3)",
            params![A, h.metadata_event_id, NOW],
        )
        .unwrap();
        metadata::preserve_authenticated_event(
            &mut db,
            A,
            "P1",
            &serde_json::to_vec(&e).unwrap(),
            NOW,
        )
        .unwrap();
        assert_eq!(
            metadata::authority_view(&db, A, "P1").unwrap().state,
            "active"
        );
        db.execute("INSERT INTO document_metadata VALUES('documents_json_migration','{\"status\":\"complete\"}')",[]).unwrap();
        if local {
            let doc = fixture().document;
            let tx = db.transaction().unwrap();
            write_projection(&tx, "P1", doc["id"].as_str().unwrap(), &doc, NOW).unwrap();
            tx.commit().unwrap();
        }
        (db, path)
    }
    fn inbox(db: &Connection, e: &codec::Event, seq: i64) {
        let h = &e.header;
        db.execute("INSERT INTO cloud_sync_inbox(account_id,event_id,server_sequence,device_id,project_id,entity_id,entity_type,operation,sync_revision,updated_at,state,received_at) VALUES(?1,?2,?3,?4,?5,?6,'document','event',?7,?8,'received',?8)",params![A,h.event_id,seq,h.device_id,h.project_id,h.entity_id,h.revision,NOW]).unwrap();
        let exists:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_event_objects WHERE account_id=?1 AND event_id=?2)",params![A,h.event_id],|r|r.get(0)).unwrap();
        if !exists {
            db.execute(
                "INSERT INTO cloud_sync_event_objects VALUES(?1,?2,1,1,?3,?4,?5)",
                params![A, h.event_id, vec![1u8; 24], vec![2u8; 32], NOW],
            )
            .unwrap();
        }
    }
    fn receive(path: &std::path::Path, e: &codec::Event, seq: i64) -> String {
        let db = sqlite::open_database(path).unwrap();
        let exists:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_inbox WHERE account_id=?1 AND server_sequence=?2)",params![A,seq],|r|r.get(0)).unwrap();
        if !exists {
            inbox(&db, e, seq);
        }
        drop(db);
        let mut db = sqlite::open_privileged_remote_apply_database(path).unwrap();
        apply(
            &mut db,
            &scope(),
            &codec::encode(e).unwrap(),
            &[1; 24],
            &[2; 32],
        )
        .unwrap()
    }
    #[test]
    fn compressed_apply_bad_stream_retry_restart_and_canonical_identity() {
        let (db, path) = seed(false);
        let mut e = fixture();
        e.document["content_json"] = json!({"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"Portable manuscript paragraph. ".repeat(100)}]}]});
        let original = codec::encode(&e).unwrap();
        let (id, payload) =
            crate::frame_compression::compress_for_frame(&original[20..], 10).unwrap();
        assert_eq!(id, 1);
        let mut frame = original[..20].to_vec();
        frame[11] = 1;
        frame[16..20].copy_from_slice(&(payload.len() as u32).to_be_bytes());
        frame.extend(payload);
        inbox(&db, &e, 2);
        drop(db);
        let mut bad = frame.clone();
        let last = bad.len() - 1;
        bad[last] ^= 1;
        {
            let mut db = sqlite::open_privileged_remote_apply_database(&path).unwrap();
            assert!(apply(&mut db, &scope(), &bad, &[1; 24], &[2; 32]).is_err());
        }
        {
            let db = sqlite::open_database(&path).unwrap();
            assert!(!ack_proven(&db, A, 2).unwrap());
            assert_eq!(
                db.query_row(
                    "SELECT COUNT(*) FROM cloud_document_apply_ledger",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
                0
            );
        }
        for _ in 0..2 {
            let mut db = sqlite::open_privileged_remote_apply_database(&path).unwrap();
            assert_eq!(
                apply(&mut db, &scope(), &frame, &[1; 24], &[2; 32]).unwrap(),
                "applied"
            );
        }
        let db = sqlite::open_database(&path).unwrap();
        assert!(ack_proven(&db, A, 2).unwrap());
        let stored: Vec<u8> = db
            .query_row(
                "SELECT canonical_frame FROM cloud_document_events WHERE event_id=?1",
                [&e.header.event_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stored, original);
        assert_eq!(stored[11], 0);
        assert_eq!(source(&db, "P1", &e.header.entity_id).unwrap(), e.document);
        drop(db);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn document_extensions_and_orphans_survive_blocked_edits_restart() {
        let (mut db, path) = seed(true);
        let id = fixture().header.entity_id;
        db.execute(
            "UPDATE documents SET extensions_json='{\"legacy_flag\":true}' WHERE id=?1",
            [&id],
        )
        .unwrap();
        let v = begin(&mut db, &scope(), "P1", NOW).unwrap();
        assert_eq!(v["owners"][0]["blocker"], "document_unsupported_extension");
        assert!(pending(&db, A, &scope().device_id, false)
            .unwrap()
            .is_empty());
        let before = source(&db, "P1", &id).unwrap();
        let mut lossy = before.clone();
        lossy["extensions"] = json!({});
        let expected = expected(&db, "P1", &id).unwrap();
        let tx = db.transaction().unwrap();
        assert!(normal(&tx, "P1", &id, lossy, Some(&expected), NOW).is_err());
        drop(tx);
        drop(db);
        let mut db = sqlite::open_database(&path).unwrap();
        assert_eq!(source(&db, "P1", &id).unwrap(), before);
        assert_eq!(
            view(&db, &scope(), "P1").unwrap()["owners"][0]["state"],
            "blocked"
        );
        db.execute("INSERT INTO document_migration_orphans VALUES('unknown','{\"legacy_flag\":true}','missing ownership')",[]).unwrap();
        assert_eq!(
            begin(&mut db, &scope(), "P1", NOW).unwrap()["blocker"],
            "document_migration_orphan"
        );
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn document_atomic_apply_replay_and_fail_closed_guard() {
        let (db, path) = seed(false);
        let e = fixture();
        let frame = codec::encode(&e).unwrap();
        inbox(&db, &e, 2);
        db.execute("CREATE TRIGGER document_crash BEFORE INSERT ON documents BEGIN SELECT RAISE(ABORT,'crash'); END",[]).unwrap();
        drop(db);
        let mut privileged = sqlite::open_privileged_remote_apply_database(&path).unwrap();
        assert!(apply(&mut privileged, &scope(), &frame, &[1; 24], &[2; 32]).is_err());
        drop(privileged);
        let db = sqlite::open_database(&path).unwrap();
        assert_eq!(
            db.query_row("SELECT count(*) FROM cloud_document_events", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM documents", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        db.execute("DROP TRIGGER document_crash", []).unwrap();
        drop(db);
        assert_eq!(receive(&path, &e, 2), "applied");
        let mut privileged = sqlite::open_privileged_remote_apply_database(&path).unwrap();
        assert_eq!(
            apply(&mut privileged, &scope(), &frame, &[1; 24], &[2; 32]).unwrap(),
            "applied"
        );
        drop(privileged);
        let db = sqlite::open_database(&path).unwrap();
        assert!(ack_proven(&db, A, 2).unwrap());
        assert!(db
            .execute("UPDATE documents SET title='unauthorized'", [])
            .is_err());
        assert!(db.execute("DELETE FROM documents", []).is_err());
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    fn child(root: &codec::Event, operation: &str, text: &str) -> codec::Event {
        let mut e = root.clone();
        e.header.event_id = metadata::new_event_id().unwrap();
        e.header.parents = vec![root.header.event_id.clone()];
        e.header.revision = root.header.revision + 1;
        e.header.generation = e.header.revision;
        e.header.operation = operation.into();
        if operation == "delete" {
            e.document = Value::Null;
            e.mutation = "delete".into();
            e.deleted_at = json!(e.header.updated_at)
        } else {
            e.document["title"] = json!(text)
        }
        e
    }
    #[test]
    fn document_all_tips_resolution_late_branch_and_stale_evidence() {
        let (db, path) = seed(false);
        drop(db);
        let root = fixture();
        assert_eq!(receive(&path, &root, 2), "applied");
        let left = child(&root, "update", "left");
        let right = child(&root, "delete", "");
        let late = child(&root, "update", "late");
        assert_eq!(receive(&path, &left, 3), "applied");
        assert_eq!(receive(&path, &right, 4), "conflict_preserved");
        let mut db = sqlite::open_database(&path).unwrap();
        let id = &root.header.entity_id;
        let heads = tips(&db, A, "P1", id).unwrap();
        assert_eq!(heads.len(), 2);
        let d = Decision {
            project_id: "P1".into(),
            document_id: id.clone(),
            expected_tips: heads,
            expected_local: source(&db, "P1", id).unwrap(),
            selected_event_id: left.header.event_id.clone(),
        };
        let resolution = decide(&mut db, &scope(), &d, NOW).unwrap();
        let item = pending(&db, A, &scope().device_id, false)
            .unwrap()
            .into_iter()
            .find(|v| v["event"]["header"]["event_id"] == resolution)
            .unwrap();
        let e: codec::Event = serde_json::from_value(item["event"].clone()).unwrap();
        assert_eq!(e.header.revision, 3);
        assert_eq!(e.header.parents.len(), 2);
        drop(db);
        assert_eq!(receive(&path, &e, 5), "applied");
        assert_eq!(receive(&path, &late, 6), "conflict_preserved");
        let mut db = sqlite::open_database(&path).unwrap();
        assert_eq!(tips(&db, A, "P1", id).unwrap().len(), 2);
        assert!(decide(&mut db, &scope(), &d, NOW).is_err());
        assert_eq!(
            db.query_row("SELECT count(*) FROM cloud_document_events", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            5
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM cloud_document_decisions", [], |r| r
                .get::<_, i64>(
                0
            ))
            .unwrap(),
            2
        );
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn document_missing_parent_collision_extensions_and_identity_block_apply_ack() {
        for kind in ["parent", "collision", "extension", "stage", "tombstone"] {
            let (mut db, path) = seed(false);
            let mut e = fixture();
            if kind == "parent" {
                e.header.event_id = metadata::new_event_id().unwrap();
                e.header.parents = vec![metadata::new_event_id().unwrap()];
                e.header.operation = "update".into();
                e.header.revision = 2;
                e.header.generation = 2;
            }
            if kind == "stage" || kind == "tombstone" {
                e.header.stage_id = Some("fabricated".into());
                e.header.stage_event_ids = vec![metadata::new_event_id().unwrap()];
                e.document["stage_id"] = json!("fabricated");
            }
            if kind == "tombstone" {
                let fixture: Value = serde_json::from_str(include_str!(
                    "../../src/cloud/__fixtures__/stageCodecV1.json"
                ))
                .unwrap();
                let mut stage: crate::stage_sync::Event =
                    serde_json::from_value(fixture["event"].clone()).unwrap();
                stage.header.entity_id = "fabricated".into();
                stage.header.operation = "delete".into();
                stage.header.revision = 2;
                stage.header.generation = 2;
                stage.stage = Value::Null;
                stage.deleted_at = json!(stage.header.updated_at);
                stage.header.parent_event_ids = vec![metadata::new_event_id().unwrap()];
                stage.header.event_id = e.header.stage_event_ids[0].clone();
                let frame = crate::stage_sync::frame(&stage).unwrap();
                db.execute("INSERT INTO cloud_sync_structural_events(account_id,event_id,project_id,entity_type,entity_id,canonical_frame,metadata_event_id,parent_event_id,revision,generation,operation,state) VALUES(?1,?2,'P1','stage','fabricated',?3,?4,?5,2,2,'delete','tombstone_blocked')",params![A,stage.header.event_id,frame,e.header.metadata_event_id,stage.header.parent_event_ids[0]]).unwrap();
                db.execute("INSERT INTO cloud_sync_structural_tips VALUES(?1,'P1','stage','fabricated',?2)",params![A,stage.header.event_id]).unwrap();
            }
            if kind == "collision" {
                let mut local = e.document.clone();
                local["id"] = json!("occupied");
                let tx = db.transaction().unwrap();
                write_projection(&tx, "P1", "occupied", &local, NOW).unwrap();
                tx.commit().unwrap();
            }
            if kind == "extension" {
                let tx = db.transaction().unwrap();
                write_projection(&tx, "P1", &e.header.entity_id, &e.document, NOW).unwrap();
                tx.commit().unwrap();
                db.execute(
                    "UPDATE documents SET extensions_json='{\"legacy_flag\":true}'",
                    [],
                )
                .unwrap();
            }
            drop(db);
            assert_eq!(receive(&path, &e, 2), "waiting");
            let db = sqlite::open_database(&path).unwrap();
            assert!(!ack_proven(&db, A, 2).unwrap());
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM cloud_document_apply_ledger",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
                0
            );
            drop(db);
            let mut db = sqlite::open_privileged_remote_apply_database(&path).unwrap();
            let mut foreign = scope();
            foreign.canonical_user_id = metadata::new_event_id().unwrap();
            assert!(apply(
                &mut db,
                &foreign,
                &codec::encode(&e).unwrap(),
                &[1; 24],
                &[2; 32]
            )
            .is_err());
            assert!(apply(
                &mut db,
                &scope(),
                &codec::encode(&e).unwrap(),
                &[0; 24],
                &[2; 32]
            )
            .is_err());
            drop(db);
            let db = sqlite::open_database(&path).unwrap();
            assert_eq!(
                db.query_row(
                    "SELECT state FROM cloud_sync_inbox WHERE server_sequence=2",
                    [],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
                "orphan"
            );
            drop(db);
            if kind == "parent" {
                let valid = fixture();
                assert_eq!(receive(&path, &valid, 3), "applied");
                let mut db = sqlite::open_database(&path).unwrap();
                db.execute(
                    "UPDATE cloud_sync_state SET pull_cursor=3,ack_cursor=1 WHERE account_id=?1",
                    [A],
                )
                .unwrap();
                let ack = crate::note_sync::prepare_note_sync_ack(
                    &mut db,
                    &crate::note_sync::PrepareNoteSyncAckCommand {
                        account_id: A.into(),
                        device_id: scope().device_id,
                        canonical_user_id: scope().canonical_user_id,
                    },
                )
                .unwrap();
                assert_eq!(ack.candidate_cursor, 1);
                assert!(ack_proven(&db, A, 3).unwrap());
                drop(db);
                let mut db = sqlite::open_privileged_remote_apply_database(&path).unwrap();
                let mut changed = valid.clone();
                changed.document["title"] = json!("changed replay");
                assert!(apply(
                    &mut db,
                    &scope(),
                    &codec::encode(&changed).unwrap(),
                    &[1; 24],
                    &[2; 32]
                )
                .is_err());
                drop(db);
            }
            std::fs::remove_file(path).unwrap();
        }
    }
    #[test]
    fn document_local_only_and_incomplete_legacy_never_publish() {
        let (mut db, path) = seed(true);
        db.execute("INSERT INTO projects(id,name,infinite,unit,status,payload_json) VALUES('local-only','Local',0,'symbols','active','{}')",[]).unwrap();
        assert!(begin(&mut db, &scope(), "local-only", NOW).is_err());
        assert!(pending(&db, A, &scope().device_id, false)
            .unwrap()
            .is_empty());
        db.execute("UPDATE document_metadata SET value_json='{\"status\":\"incomplete\"}' WHERE key='documents_json_migration'",[]).unwrap();
        let v = begin(&mut db, &scope(), "P1", NOW).unwrap();
        assert_eq!(v["blocker"], "document_legacy_migration_incomplete");
        assert!(pending(&db, A, &scope().device_id, false)
            .unwrap()
            .is_empty());
        assert!(!source(&db, "P1", &fixture().header.entity_id)
            .unwrap()
            .is_null());
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn document_local_resource_blocker_retains_raw_capture() {
        let (mut db, path) = seed(true);
        let raw = serde_json::to_string(&"a".repeat(codec::MAX_FRAME_BYTES + 1)).unwrap();
        db.execute("UPDATE documents SET content_json=?1", [&raw])
            .unwrap();
        let v = begin(&mut db, &scope(), "P1", NOW).unwrap();
        assert_eq!(v["owners"][0]["blocker"], "document_resource_limit");
        assert!(pending(&db, A, &scope().device_id, false)
            .unwrap()
            .is_empty());
        let evidence: String = db
            .query_row(
                "SELECT source_json FROM cloud_document_candidates",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&evidence).unwrap()["retained_raw"]["content_json"],
            raw
        );
        drop(db);
        let mut db = sqlite::open_database(&path).unwrap();
        assert_eq!(
            db.query_row("SELECT content_json FROM documents", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            raw
        );
        db.execute(
            "INSERT INTO document_migration_orphans VALUES('raw','not-json','unknown ownership')",
            [],
        )
        .unwrap();
        assert_eq!(
            begin(&mut db, &scope(), "P1", NOW).unwrap()["blocker"],
            "document_migration_orphan"
        );
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn document_max_tiptap_depth_survives_sqlite_reader_reopen() {
        let (mut db, path) = seed(true);
        let mut tree = json!({"type":"paragraph"});
        for _ in 0..59 {
            tree = json!({"type":"blockquote","content":[tree]})
        }
        let mut doc = fixture().document;
        doc["content_json"] = json!({"type":"doc","content":[tree]});
        codec::validate_document(&doc).unwrap();
        let tx = db.transaction().unwrap();
        write_projection(&tx, "P1", doc["id"].as_str().unwrap(), &doc, NOW).unwrap();
        tx.commit().unwrap();
        drop(db);
        let db = sqlite::open_database(&path).unwrap();
        assert_eq!(
            current(&db, "P1", doc["id"].as_str().unwrap()).unwrap(),
            doc
        );
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
}
