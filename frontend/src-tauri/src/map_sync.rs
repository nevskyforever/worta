//! Explicit codec9 admission and full-map causal authority on the shared transport.
use crate::{map_codec as codec, project_metadata_sync as metadata, sqlite};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
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
            Self::Sql(_) | Self::Storage(_) => "map_storage_unavailable",
            Self::Metadata(_) => "map_dependency_unavailable",
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
    codec::bounded_stack(|| {
        let mut parser = serde_json::Deserializer::from_str(s);
        parser.disable_recursion_limit();
        let value =
            Value::deserialize(&mut parser).map_err(|_| "invalid_map_payload".to_string())?;
        parser
            .end()
            .map_err(|_| "invalid_map_payload".to_string())?;
        Ok(value)
    })
    .map_err(Error::from)
}
fn canonical_map(v: &Value) -> Result<Value> {
    let bytes = codec::bounded_stack(|| Ok(codec::canonical(v)))?;
    parse(&bytes)
}
fn rows(db: &Connection, sql: &str, args: impl rusqlite::Params) -> Result<Vec<String>> {
    let mut q = db.prepare(sql)?;
    let out = q
        .query_map(args, |r| r.get(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(out)
}
pub(crate) fn tips(db: &Connection, a: &str, p: &str, id: &str) -> Result<Vec<String>> {
    rows(db,"SELECT event_id FROM cloud_map_tips WHERE account_id=?1 AND project_id=?2 AND entity_id=?3 ORDER BY event_id",params![a,p,id])
}
fn owner(db: &Connection, p: &str, s: Option<&str>) -> Result<Value> {
    let raw: String = if let Some(s) = s {
        db.query_row(
            "SELECT payload_json FROM stages WHERE project_id=?1 AND id=?2",
            params![p, s],
            |r| r.get(0),
        )?
    } else {
        db.query_row("SELECT payload_json FROM projects WHERE id=?1", [p], |r| {
            r.get(0)
        })?
    };
    parse(&raw)
}
fn note_rows(db: &Connection, p: &str, s: Option<&str>) -> Result<Vec<Value>> {
    rows(
        db,
        "SELECT payload_json FROM notes WHERE project_id=?1 AND stage_id IS ?2 ORDER BY id",
        params![p, s],
    )?
    .iter()
    .map(|r| parse(r))
    .collect()
}
fn source(db: &Connection, p: &str, s: Option<&str>) -> Result<Value> {
    Ok(json!({"owner":owner(db,p,s)?,"notes":note_rows(db,p,s)?}))
}
fn timestamp(value: &str) -> Result<String> {
    if metadata::timestamp(value) {
        return Ok(value.into());
    }
    let body = value
        .strip_suffix('Z')
        .ok_or_else(|| Error::Code("invalid_map_payload".into()))?;
    let (seconds, fraction) = body.split_once('.').unwrap_or((body, ""));
    if seconds.len() != 19 || fraction.len() > 6 || !fraction.bytes().all(|b| b.is_ascii_digit()) {
        return fail("invalid_map_payload");
    }
    let canonical = format!("{}.{:0<6}Z", seconds, fraction);
    if !metadata::timestamp(&canonical) {
        return fail("invalid_map_payload");
    }
    Ok(canonical)
}
const ANNOTATION_FIELDS: &[&str] = &[
    "title",
    "checklist",
    "tags",
    "color",
    "pinned",
    "archived",
    "sort_order",
    "metadata",
    "created_at",
];
fn annotation(note: &Value) -> Value {
    let mut a = serde_json::Map::new();
    a.insert("note_id".into(), note["id"].clone());
    for k in ANNOTATION_FIELDS {
        a.insert((*k).into(), note[*k].clone());
    }
    Value::Object(a)
}
fn annotations(
    db: &Connection,
    p: &str,
    s: Option<&str>,
    data: &Value,
    now: &str,
    override_note: Option<&Value>,
) -> Result<Value> {
    let invalid:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM notes WHERE project_id=?1 AND stage_id IS ?2 AND json_extract(payload_json,'$.source_type')='mindmap' AND (json_extract(payload_json,'$.id') IS NOT id OR json_extract(payload_json,'$.project_id') IS NOT project_id OR json_extract(payload_json,'$.stage_id') IS NOT stage_id))",params![p,s],|r|r.get(0))?;
    if invalid {
        return fail("map_note_link_invalid");
    }
    let mut notes = BTreeMap::new();
    let root = data["nodeData"]["id"].as_str();
    for n in note_rows(db, p, s)? {
        if n["source_type"] != "mindmap" {
            continue;
        }
        let allowed = [
            "id",
            "project_id",
            "stage_id",
            "title",
            "content",
            "content_format",
            "checklist",
            "color",
            "pinned",
            "archived",
            "sort_order",
            "tags",
            "source_type",
            "source_map_id",
            "source_node_id",
            "created_at",
            "updated_at",
            "revision",
            "metadata",
        ];
        if n.as_object()
            .is_none_or(|o| o.keys().any(|k| !allowed.contains(&k.as_str())))
        {
            return fail("map_unsupported_extension");
        }
        if n["project_id"] != p || n["stage_id"] != json!(s) || n["content_format"] != "plain" {
            return fail("map_note_link_invalid");
        }
        let key = n["source_node_id"]
            .as_str()
            .ok_or_else(|| Error::Code("map_note_link_invalid".into()))?
            .to_string();
        if n["source_map_id"].as_str() != root || notes.insert(key, n).is_some() {
            return fail("map_note_link_invalid");
        }
    }
    let original = owner(db, p, s)?;
    let published_owner:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM cloud_map_migrations WHERE project_id=?1 AND entity_id=?2 AND lifecycle!='blocked')",params![p,codec::entity_id(s)],|r|r.get(0))?;
    if !published_owner && original.get("map_note_annotations").is_some() {
        return fail("map_unsupported_extension");
    }
    let admitted = if published_owner {
        original
            .get("map_note_annotations")
            .and_then(Value::as_object)
    } else {
        None
    };
    let mut out = serde_json::Map::new();
    let extracted = crate::mindmap::extract_notes(data);
    let present: HashSet<_> = extracted.iter().map(|(k, _)| k.as_str()).collect();
    // Stale old projection rows are removed by ordinary edits, but explicit first
    // admission may never guess a dangling source-node relationship.
    let migrating = admitted.is_none();
    if migrating && notes.keys().any(|k| !present.contains(k.as_str())) {
        return fail("map_note_link_invalid");
    }
    for (node, _) in extracted {
        let mut a = if let Some(n) =
            override_note.filter(|n| n["source_node_id"].as_str() == Some(&node))
        {
            annotation(n)
        } else if let Some(a) = admitted.and_then(|a| a.get(&node)) {
            a.clone()
        } else if let Some(n) = notes.get(&node) {
            annotation(n)
        } else {
            json!({"note_id":crate::mindmap::linked_note_id(&node),"title":"","checklist":[],"tags":[],"color":"default","pinned":false,"archived":false,"sort_order":0,"metadata":{},"created_at":now})
        };
        a["created_at"] = json!(timestamp(
            a["created_at"]
                .as_str()
                .ok_or_else(|| Error::Code("invalid_map_payload".into()))?
        )?);
        let collision: Option<String> = db
            .query_row(
                "SELECT payload_json FROM notes WHERE id=?1",
                [a["note_id"]
                    .as_str()
                    .ok_or_else(|| Error::Code("map_note_link_invalid".into()))?],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(raw) = collision {
            let n = parse(&raw)?;
            if n["source_type"] != "mindmap"
                || n["project_id"] != p
                || n["stage_id"] != json!(s)
                || n["source_node_id"] != node
            {
                return fail("map_note_identity_collision");
            }
        }
        out.insert(node, a);
    }
    let out = Value::Object(out);
    codec::validate_map(data, &out)?;
    Ok(out)
}
fn current(db: &Connection, p: &str, s: Option<&str>, now: &str) -> Result<Value> {
    let o = owner(db, p, s)?;
    let data = o.get("mindmap").cloned().unwrap_or(Value::Null);
    if data.is_null() {
        return Ok(Value::Null);
    }
    let a = annotations(db, p, s, &data, now, None)?;
    canonical_map(&json!({"data":data,"annotations":a}))
}
fn header(
    db: &Connection,
    a: &str,
    p: &str,
    s: Option<&str>,
    parents: Vec<String>,
    now: &str,
) -> Result<codec::Header> {
    let view = metadata::authority_view(db, a, p)?;
    if view.state != "active" {
        return fail("project_metadata_authority_unresolved");
    }
    let (user,device,boot):(String,String,String)=db.query_row("SELECT b.canonical_user_id,s.device_id,p.bootstrap_id FROM cloud_account_bindings b JOIN cloud_sync_state s ON s.account_id=b.local_account_id JOIN cloud_sync_project_bootstraps p ON p.account_id=s.account_id WHERE s.account_id=?1 AND p.project_id=?2",params![a,p],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    let stages = if let Some(s) = s {
        rows(db,"SELECT event_id FROM cloud_sync_structural_tips WHERE account_id=?1 AND project_id=?2 AND entity_type='stage' AND entity_id=?3 ORDER BY event_id",params![a,p,s])?
    } else {
        vec![]
    };
    let mut revision = 0;
    for parent in &parents {
        let r:i64=db.query_row("SELECT revision FROM cloud_map_events WHERE account_id=?1 AND project_id=?2 AND entity_id=?3 AND event_id=?4 AND state IN ('applied','conflict_preserved')",params![a,p,codec::entity_id(s),parent],|r|r.get(0))?;
        revision = revision.max(r)
    }
    let h = codec::Header {
        account_id: user,
        device_id: device,
        project_id: p.into(),
        stage_id: s.map(str::to_string),
        entity_id: codec::entity_id(s),
        event_id: metadata::new_event_id()?,
        bootstrap_id: boot,
        metadata_event_id: view
            .head_event_id
            .ok_or_else(|| Error::Code("project_metadata_authority_unresolved".into()))?,
        stage_event_ids: stages,
        operation: if parents.is_empty() {
            "create"
        } else if parents.len() > 1 {
            "resolution"
        } else {
            "update"
        }
        .into(),
        parents,
        revision: revision + 1,
        generation: revision + 1,
        updated_at: now.into(),
    };
    let e = codec::Event {
        version: 1,
        header: h.clone(),
        mutation: "delete".into(),
        map: Value::Null,
        deleted_at: json!(now),
    };
    codec::dependencies_ready(db, a, &h.account_id, &h.device_id, &e)?;
    Ok(h)
}
fn event(h: codec::Header, map: Value) -> codec::Event {
    let deleted = map.is_null();
    let mut h = h;
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
        map,
    }
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
    tx.execute("INSERT INTO cloud_map_events(account_id,event_id,project_id,entity_id,stage_id,canonical_frame,parents_json,revision,state,server_sequence) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",params![a,h.event_id,h.project_id,h.entity_id,h.stage_id,frame,json!(h.parents).to_string(),h.revision,state,seq])?;
    Ok(())
}
fn queue(tx: &Transaction<'_>, a: &str, e: &codec::Event) -> Result<()> {
    let h = &e.header;
    let frame = codec::encode(e)?;
    store(tx, a, e, &frame, "unsealed", None)?;
    tx.execute("INSERT INTO cloud_sync_outbox(event_id,account_id,device_id,project_id,entity_id,entity_type,operation,revision,updated_at,deleted_at,created_at,parent_event_id,local_ordinal,lifecycle) VALUES(?1,?2,?3,?4,?5,'map','event',?6,?7,NULL,?7,?8,(SELECT COALESCE(MAX(local_ordinal),0)+1 FROM cloud_sync_outbox WHERE account_id=?2),'unsealed')",params![h.event_id,a,h.device_id,h.project_id,h.entity_id,h.revision,h.updated_at,h.parents.first()])?;
    Ok(())
}
fn pending_owner(db: &Connection, a: &str, p: &str, id: &str) -> Result<bool> {
    Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM cloud_map_events e JOIN cloud_sync_outbox o ON o.account_id=e.account_id AND o.event_id=e.event_id WHERE e.account_id=?1 AND e.project_id=?2 AND e.entity_id=?3 AND o.lifecycle IN ('unsealed','sealed','accepted') AND e.state IN ('unsealed','sealed','waiting'))",params![a,p,id],|r|r.get(0))?)
}
/// A complete local mutation plan allows exact guards without a second Note authority.
fn projection(db: &Connection, p: &str, s: Option<&str>, map: &Value, now: &str) -> Result<Value> {
    let mut o = owner(db, p, s)?;
    o["mindmap"] = if map.is_null() {
        Value::Null
    } else {
        map["data"].clone()
    };
    o["map_note_annotations"] = if map.is_null() {
        json!({})
    } else {
        map["annotations"].clone()
    };
    o["mindmap_updated_at"] = json!(now);
    o["notes_updated_at"] = json!(now);
    let existing = note_rows(db, p, s)?;
    let mut out = Vec::new();
    let mut ids = HashSet::new();
    if !map.is_null() {
        let linked = codec::validate_map(&map["data"], &map["annotations"])?;
        for (node, text) in linked {
            let a = &map["annotations"][&node];
            let id = a["note_id"]
                .as_str()
                .ok_or_else(|| Error::Code("map_note_link_invalid".into()))?;
            let any: Option<String> = db
                .query_row("SELECT payload_json FROM notes WHERE id=?1", [id], |r| {
                    r.get(0)
                })
                .optional()?;
            let old = any.map(|r| parse(&r)).transpose()?;
            if old.as_ref().is_some_and(|n| {
                n["source_type"] != "mindmap"
                    || n["project_id"] != p
                    || n["stage_id"] != json!(s)
                    || n["source_node_id"] != node
            }) {
                return fail("map_note_identity_collision");
            }
            let rev = old
                .as_ref()
                .and_then(|n| n["revision"].as_i64())
                .unwrap_or(-1)
                + 1;
            let mut n = json!({"id":id,"project_id":p,"stage_id":s,"content":text,"content_format":"plain","source_type":"mindmap","source_node_id":node,"source_map_id":map["data"]["nodeData"]["id"],"revision":rev,"updated_at":now});
            for k in ANNOTATION_FIELDS {
                n[*k] = a[*k].clone()
            }
            out.push(n);
            ids.insert(id.to_string());
        }
    }
    let removed = existing
        .into_iter()
        .filter(|n| n["source_type"] == "mindmap" && !ids.contains(n["id"].as_str().unwrap_or("")))
        .collect::<Vec<_>>();
    Ok(json!({"owner_payload":o,"notes":out,"removed_notes":removed}))
}
fn write_projection(
    tx: &Transaction<'_>,
    p: &str,
    s: Option<&str>,
    plan: &Value,
    now: &str,
) -> Result<()> {
    let owner = plan["owner_payload"].to_string();
    if let Some(s) = s {
        tx.execute(
            "UPDATE stages SET payload_json=?1,updated_at=?2 WHERE project_id=?3 AND id=?4",
            params![owner, now, p, s],
        )?;
    } else {
        tx.execute(
            "UPDATE projects SET payload_json=?1,updated_at=?2 WHERE id=?3",
            params![owner, now, p],
        )?;
    }
    for n in plan["removed_notes"].as_array().unwrap() {
        tx.execute(
            "DELETE FROM notes WHERE id=?1 AND project_id=?2 AND stage_id IS ?3",
            params![n["id"].as_str(), p, s],
        )?;
    }
    for n in plan["notes"].as_array().unwrap() {
        tx.execute("INSERT INTO notes(id,project_id,stage_id,updated_at,payload_json) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(id) DO UPDATE SET updated_at=excluded.updated_at,payload_json=excluded.payload_json",params![n["id"].as_str(),p,s,now,n.to_string()])?;
    }
    Ok(())
}
fn draft(
    tx: &Transaction<'_>,
    a: &str,
    p: &str,
    s: Option<&str>,
    map: &Value,
    now: &str,
) -> Result<Value> {
    let plan = projection(tx, p, s, map, now)?;
    tx.execute("INSERT INTO cloud_map_local_drafts VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(account_id,project_id,entity_id) DO UPDATE SET snapshot_json=excluded.snapshot_json,projection_json=excluded.projection_json,updated_at=excluded.updated_at",params![a,p,codec::entity_id(s),map.to_string(),plan.to_string(),now])?;
    Ok(plan)
}
pub(crate) fn begin(
    db: &mut Connection,
    scope: &metadata::MetadataScope,
    p: &str,
    now: &str,
) -> Result<Value> {
    metadata::assert_runtime_scope(
        db,
        &scope.account_id,
        &scope.canonical_user_id,
        &scope.device_id,
    )?;
    if !metadata::timestamp(now) {
        return fail("invalid_map_payload");
    }
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    metadata::assert_runtime_scope(
        &tx,
        &scope.account_id,
        &scope.canonical_user_id,
        &scope.device_id,
    )?;
    let bound:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_project_bindings WHERE account_id=?1 AND project_id=?2)",params![scope.account_id,p],|r|r.get(0))?;
    if !bound {
        return fail("map_project_not_bound");
    }
    let mut owners = vec![None];
    for s in rows(
        &tx,
        "SELECT id FROM stages WHERE project_id=?1 ORDER BY id",
        [p],
    )? {
        owners.push(Some(s))
    }
    for s in owners {
        let id = codec::entity_id(s.as_deref());
        let existing:Option<String>=tx.query_row("SELECT lifecycle FROM cloud_map_migrations WHERE account_id=?1 AND project_id=?2 AND entity_id=?3",params![scope.account_id,p,id],|r|r.get(0)).optional()?;
        if existing.as_ref().is_some_and(|l| l != "blocked") {
            continue;
        }
        let src = source(&tx, p, s.as_deref())?;
        if src["owner"].get("mindmap").is_none_or(Value::is_null) {
            continue;
        }
        let cid = metadata::new_event_id()?;
        let captured = (|| -> Result<(Value, codec::Event)> {
            let map = current(&tx, p, s.as_deref(), now)?;
            let h = header(
                &tx,
                &scope.account_id,
                p,
                s.as_deref(),
                tips(&tx, &scope.account_id, p, &id)?,
                now,
            )?;
            let e = event(h, map.clone());
            codec::encode(&e)?;
            Ok((map, e))
        })();
        let (map, e, blocker) = match captured {
            Ok((m, e)) => (Some(m), Some(e), None),
            Err(e) => (None, None, Some(e.to_string())),
        };
        tx.execute(
            "INSERT INTO cloud_map_candidates VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![
                cid,
                scope.account_id,
                p,
                id,
                s,
                e.as_ref().map(|e| &e.header.event_id),
                src.to_string(),
                map.as_ref().map(Value::to_string),
                src["notes"].to_string(),
                blocker
            ],
        )?;
        tx.execute("INSERT INTO cloud_map_migrations VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(account_id,project_id,entity_id) DO UPDATE SET lifecycle=excluded.lifecycle,candidate_id=excluded.candidate_id,blocker=excluded.blocker",params![scope.account_id,p,id,s,if blocker.is_some(){"blocked"}else{"captured"},cid,blocker])?;
        if let (Some(map), Some(e)) = (map, e) {
            queue(&tx, &scope.account_id, &e)?;
            let plan = draft(&tx, &scope.account_id, p, s.as_deref(), &map, now)?;
            write_projection(&tx, p, s.as_deref(), &plan, now)?;
            tx.execute("UPDATE cloud_map_migrations SET lifecycle='publication_pending' WHERE account_id=?1 AND project_id=?2 AND entity_id=?3",params![scope.account_id,p,id])?;
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
    let mut q=db.prepare("SELECT entity_id,stage_id,lifecycle,blocker FROM cloud_map_migrations WHERE account_id=?1 AND project_id=?2 ORDER BY entity_id")?;
    let mut entries = q
        .query_map(params![scope.account_id, p], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(q);
    let bound:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_project_bindings WHERE account_id=?1 AND project_id=?2)",params![scope.account_id,p],|r|r.get(0))?;
    if !bound {
        return fail("map_project_not_bound");
    }
    let mut owners = vec![None];
    owners.extend(
        rows(
            db,
            "SELECT id FROM stages WHERE project_id=?1 ORDER BY id",
            [p],
        )?
        .into_iter()
        .map(Some),
    );
    for stage in owners {
        let id = codec::entity_id(stage.as_deref());
        if !entries.iter().any(|(existing, _, _, _)| existing == &id)
            && owner(db, p, stage.as_deref())?
                .get("mindmap")
                .is_some_and(|m| !m.is_null())
        {
            entries.push((id, stage, "local".into(), None));
        }
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let mut out = vec![];
    for (id, s, l, b) in entries {
        let ids = tips(db, &scope.account_id, p, &id)?;
        let mut versions = vec![];
        for eid in &ids {
            let raw: Vec<u8> = db.query_row(
                "SELECT canonical_frame FROM cloud_map_events WHERE account_id=?1 AND event_id=?2",
                params![scope.account_id, eid],
                |r| r.get(0),
            )?;
            let e = codec::decode(&raw)?;
            versions.push(json!({"event_id":eid,"revision":e.header.revision,"mutation":e.mutation,"map":e.map}));
        }
        out.push(json!({"entity_id":id,"stage_id":s,"state":l,"blocker":b,"tips":ids,"versions":versions,"local":source(db,p,s.as_deref())?}));
    }
    Ok(json!({"owners":out}))
}
/// Called inside the ordinary production mutation transaction, never captures.
pub(crate) fn local_edit(
    tx: &Transaction<'_>,
    p: &str,
    s: Option<&str>,
    data: &Value,
    override_note: Option<&Value>,
    now: &str,
) -> Result<bool> {
    let canonical_now = timestamp(now)?;
    let now = canonical_now.as_str();
    let id = codec::entity_id(s);
    let binding:Option<(String,String)>=tx.query_row("SELECT account_id,lifecycle FROM cloud_map_migrations WHERE project_id=?1 AND entity_id=?2",params![p,id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    let Some((a, l)) = binding else {
        return Ok(false);
    };
    if l == "blocked" {
        return Ok(false);
    }
    if l == "conflict" {
        return fail("map_conflict_requires_resolution");
    }
    let map = if data.is_null() {
        Value::Null
    } else {
        canonical_map(
            &json!({"data":data,"annotations":annotations(tx,p,s,data,now,override_note)?}),
        )?
    };
    if current(tx, p, s, now)? == map {
        return Ok(true);
    }
    let parents = tips(tx, &a, p, &id)?;
    if l == "active" && parents.len() != 1 {
        return fail("map_conflict_requires_resolution");
    }
    // Revalidate live metadata/Stage even when a preceding publication is pending.
    let h = header(tx, &a, p, s, parents, now)?;
    let plan = draft(tx, &a, p, s, &map, now)?;
    if l == "active" && !pending_owner(tx, &a, p, &id)? {
        queue(tx, &a, &event(h, map))?;
    }
    write_projection(tx, p, s, &plan, now)?;
    Ok(true)
}
pub(crate) fn advance(db: &mut Connection, a: &str, now: &str) -> Result<()> {
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut q=tx.prepare("SELECT d.project_id,m.stage_id,d.snapshot_json FROM cloud_map_local_drafts d JOIN cloud_map_migrations m ON m.account_id=d.account_id AND m.project_id=d.project_id AND m.entity_id=d.entity_id WHERE d.account_id=?1 AND m.lifecycle='active' ORDER BY d.updated_at LIMIT 8")?;
    let drafts = q
        .query_map([a], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(q);
    for (p, s, raw) in drafts {
        let id = codec::entity_id(s.as_deref());
        if pending_owner(&tx, a, &p, &id)? {
            continue;
        }
        let map = parse(&raw)?;
        let prior:Option<String>=tx.query_row("SELECT snapshot_json FROM cloud_map_projection WHERE account_id=?1 AND project_id=?2 AND entity_id=?3",params![a,p,id],|r|r.get(0)).optional()?;
        if prior
            .as_ref()
            .is_some_and(|v| parse(v).ok() == Some(map.clone()))
        {
            tx.execute("DELETE FROM cloud_map_local_drafts WHERE account_id=?1 AND project_id=?2 AND entity_id=?3",params![a,p,id])?;
            continue;
        }
        let parents = tips(&tx, a, &p, &id)?;
        if parents.len() != 1 {
            continue;
        }
        let h = match header(&tx, a, &p, s.as_deref(), parents, now) {
            Ok(h) => h,
            Err(_) => continue,
        };
        queue(&tx, a, &event(h, map))?;
    }
    tx.commit()?;
    Ok(())
}
pub(crate) fn pending(db: &Connection, a: &str, device: &str, sealed: bool) -> Result<Vec<Value>> {
    let mut q=db.prepare("SELECT e.canonical_frame,o.nonce,o.ciphertext FROM cloud_map_events e JOIN cloud_sync_outbox b ON b.account_id=e.account_id AND b.event_id=e.event_id LEFT JOIN cloud_sync_event_objects o ON o.account_id=e.account_id AND o.event_id=e.event_id WHERE e.account_id=?1 AND b.device_id=?2 AND e.state IN ('unsealed','sealed') AND b.lifecycle IN ('unsealed','sealed','accepted') AND (?3=0 AND o.event_id IS NULL OR ?3=1 AND o.event_id IS NOT NULL) ORDER BY e.retry_ordinal,b.local_ordinal LIMIT 8")?;
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
        return fail("invalid_map_payload");
    }
    let compression_view =
        crate::frame_compression::canonical_view(frame).map_err(|c| Error::Code(c.into()))?;
    let frame = compression_view.as_ref();
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let old:Vec<u8>=tx.query_row("SELECT canonical_frame FROM cloud_map_events WHERE account_id=?1 AND event_id=?2 AND state IN ('unsealed','sealed')",params![a,id],|r|r.get(0))?;
    if old != frame {
        return fail("map_sealing_stale");
    }
    let object:Option<(Vec<u8>,Vec<u8>)>=tx.query_row("SELECT nonce,ciphertext FROM cloud_sync_event_objects WHERE account_id=?1 AND event_id=?2",params![a,id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    if let Some((n, c)) = object {
        if n != nonce || c != ciphertext {
            return fail("map_exact_replay_mismatch");
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
            "UPDATE cloud_map_events SET state='sealed' WHERE account_id=?1 AND event_id=?2",
            params![a, id],
        )?;
        tx.execute("UPDATE cloud_map_migrations SET lifecycle='self_echo_pending' WHERE account_id=?1 AND project_id=(SELECT project_id FROM cloud_map_events WHERE account_id=?1 AND event_id=?2) AND entity_id=(SELECT entity_id FROM cloud_map_events WHERE account_id=?1 AND event_id=?2) AND lifecycle='publication_pending'",params![a,id])?;
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
        return fail("invalid_map_receipt");
    }
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let valid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_outbox WHERE account_id=?1 AND device_id=?2 AND event_id=?3 AND entity_type='map' AND lifecycle IN ('sealed','accepted'))",params![a,device,id],|r|r.get(0))?;
    if !valid {
        return fail("map_scope_mismatch");
    }
    let prior:Option<i64>=tx.query_row("SELECT server_sequence FROM cloud_sync_upload_receipts WHERE account_id=?1 AND event_id=?2",params![a,id],|r|r.get(0)).optional()?;
    if prior.is_some_and(|n| n != seq) {
        return fail("map_exact_replay_mismatch");
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
    tx.execute("UPDATE cloud_map_events SET retry_ordinal=(SELECT COALESCE(MAX(retry_ordinal),0)+1 FROM cloud_map_events WHERE account_id=?1) WHERE account_id=?1 AND event_id=?2",params![a,id])?;
    tx.commit()?;
    Ok(())
}
fn parent_revision(db: &Connection, a: &str, e: &codec::Event) -> Result<i64> {
    let h = &e.header;
    let mut max = 0;
    for id in &h.parents {
        let row:Option<i64>=db.query_row("SELECT revision FROM cloud_map_events WHERE account_id=?1 AND event_id=?2 AND project_id=?3 AND entity_id=?4 AND stage_id IS ?5 AND state IN ('applied','conflict_preserved')",params![a,id,h.project_id,h.entity_id,h.stage_id],|r|r.get(0)).optional()?;
        let Some(r) = row else {
            return fail("map_parent_unknown");
        };
        max = max.max(r)
    }
    if h.revision != max + 1 {
        return fail("map_parent_revision_invalid");
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
            return fail("map_causal_proof_limit");
        }
        let raw: Option<String> = db
            .query_row(
                "SELECT parents_json FROM cloud_map_events WHERE account_id=?1 AND event_id=?2",
                params![a, id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(raw) = raw {
            let parents: Vec<String> =
                serde_json::from_str(&raw).map_err(|_| Error::Code("map_parent_invalid".into()))?;
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
  if h.account_id!=scope.canonical_user_id{return fail("map_scope_mismatch")}
  let seq:Option<i64>=tx.query_row("SELECT i.server_sequence FROM cloud_sync_inbox i JOIN cloud_sync_event_objects o ON o.account_id=i.account_id AND o.event_id=i.event_id WHERE i.account_id=?1 AND i.event_id=?2 AND i.project_id=?3 AND i.entity_id=?4 AND i.entity_type='map' AND i.operation='event' AND i.device_id=?5 AND i.sync_revision=?6 AND i.updated_at=?7 AND i.deleted_at IS NULL AND o.crypto_version=1 AND o.aad_version=1 AND o.nonce=?8 AND o.ciphertext=?9",params![a,h.event_id,h.project_id,h.entity_id,h.device_id,h.revision,h.updated_at,nonce,ciphertext],|r|r.get(0)).optional()?;
  let Some(seq)=seq else{return fail("map_scope_mismatch")};
  let previous:Option<Vec<u8>>=tx.query_row("SELECT canonical_frame FROM cloud_map_events WHERE account_id=?1 AND event_id=?2",params![a,h.event_id],|r|r.get(0)).optional()?;
  if previous.as_ref().is_some_and(|p|p!=frame){return fail("map_exact_replay_mismatch")}
  let applied:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM cloud_map_apply_ledger WHERE account_id=?1 AND event_id=?2 AND server_sequence=?3 AND nonce=?4 AND ciphertext=?5)",params![a,h.event_id,seq,nonce,ciphertext],|r|r.get(0))?;
  if applied{return Ok((vec![],None))}
  if previous.is_none(){store(tx,a,&e,frame,"waiting",Some(seq))?;}else{tx.execute("UPDATE cloud_map_events SET server_sequence=?1 WHERE account_id=?2 AND event_id=?3",params![seq,a,h.event_id])?;}
  let ready=codec::dependencies_ready(tx,a,&scope.canonical_user_id,&h.device_id,&e).map_err(Error::from).and_then(|_|parent_revision(tx,a,&e).map(|_|()));
  if let Err(blocker)=ready {tx.execute("UPDATE cloud_map_events SET state='waiting',blocker=?1 WHERE account_id=?2 AND event_id=?3",params![blocker.to_string(),a,h.event_id])?;tx.execute("UPDATE cloud_sync_inbox SET state='orphan',error_code=?1 WHERE account_id=?2 AND event_id=?3",params![blocker.to_string(),a,h.event_id])?;return Ok((vec![],None))}
  let own:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_outbox WHERE account_id=?1 AND event_id=?2 AND device_id=?3 AND entity_type='map' AND project_id=?4 AND entity_id=?5 AND lifecycle IN ('sealed','accepted'))",params![a,h.event_id,scope.device_id,h.project_id,h.entity_id],|r|r.get(0))?;
  let old=tips(tx,a,&h.project_id,&h.entity_id)?;let src=source(tx,&h.project_id,h.stage_id.as_deref())?;
  let prior:Option<String>=tx.query_row("SELECT snapshot_json FROM cloud_map_projection WHERE account_id=?1 AND project_id=?2 AND entity_id=?3",params![a,h.project_id,h.entity_id],|r|r.get(0)).optional()?;
  let local=current(tx,&h.project_id,h.stage_id.as_deref(),&h.updated_at);
  let mut unchanged=match(&prior,&local){(Some(raw),Ok(local))=>parse(raw)?==*local,(None,Ok(local))=>local.is_null(),_=>false};
  let decision:Option<String>=tx.query_row("SELECT expected_local FROM cloud_map_decisions WHERE account_id=?1 AND event_id=?2",params![a,h.event_id],|r|r.get(0)).optional()?;
  let decision_matches=decision.as_ref().is_some_and(|raw|parse(raw).ok()==Some(src.clone()));
  // A remote full-tip resolution may replace an already authenticated local
  // branch. Preserve an unpublished local draft that differs from every parent.
  if h.operation=="resolution" && !unchanged && !pending_owner(tx,a,&h.project_id,&h.entity_id)? {
   if let Ok(local)=&local {for parent in &h.parents {let raw:Vec<u8>=tx.query_row("SELECT canonical_frame FROM cloud_map_events WHERE account_id=?1 AND event_id=?2",params![a,parent],|r|r.get(0))?;if codec::decode(&raw)?.map==*local {unchanged=true;break}}}
  }
  let conflict=old!=h.parents || (!own&&!unchanged&&!decision_matches) || (decision.is_some()&&!decision_matches);
  if !unchanged && (!own||conflict) {tx.execute("INSERT OR IGNORE INTO cloud_map_local_candidates VALUES(?1,?2,?3,?4,?5)",params![a,h.event_id,h.project_id,h.entity_id,src.to_string()])?;}
  let mut new_tips=Vec::new();for tip in old{if !covered(tx,a,&h.event_id,&tip)?{new_tips.push(tip)}}new_tips.push(h.event_id.clone());new_tips.sort();new_tips.dedup();
  let plan=if conflict || own&&decision.is_none()&&local.as_ref().is_ok_and(|local|local!=&e.map){None}else{match projection(tx,&h.project_id,h.stage_id.as_deref(),&e.map,&h.updated_at){Ok(p)=>Some(p),Err(blocker)=>{tx.execute("UPDATE cloud_map_events SET state='waiting',blocker=?1 WHERE account_id=?2 AND event_id=?3",params![blocker.to_string(),a,h.event_id])?;tx.execute("UPDATE cloud_sync_inbox SET state='orphan',error_code=?1 WHERE account_id=?2 AND event_id=?3",params![blocker.to_string(),a,h.event_id])?;return Ok((vec![],None))}}};
  let auth=plan.as_ref().map(|plan|sqlite::OwnedRemoteApplyAuthorization{event_id:h.event_id.clone(),account_id:a.clone(),project_id:h.project_id.clone(),entity_id:h.entity_id.clone(),operation:"upsert".into(),payload_json:Some(plan.to_string()),prior_payload_json:Some(src.to_string())}).into_iter().collect();
  Ok((auth,Some(ApplyPlan{event:e.clone(),seq,nonce:nonce.to_vec(),ciphertext:ciphertext.to_vec(),outcome:if conflict{"conflict_preserved"}else{"applied"}.into(),projection:plan,new_tips,own})))
 },|tx,plan|->Result<String>{let a=&scope.account_id;let Some(plan)=plan else{let outcome:Option<String>=tx.query_row("SELECT outcome FROM cloud_map_apply_ledger WHERE account_id=?1 AND event_id=?2",params![a,e.header.event_id],|r|r.get(0)).optional()?;return Ok(outcome.unwrap_or("waiting".into()))};let h=&plan.event.header;
  if let Some(projection)=&plan.projection{write_projection(tx,&h.project_id,h.stage_id.as_deref(),projection,&h.updated_at)?;tx.execute("DELETE FROM cloud_sync_remote_apply_authorizations WHERE event_id=?1",[&h.event_id])?;}
  tx.execute("DELETE FROM cloud_map_tips WHERE account_id=?1 AND project_id=?2 AND entity_id=?3",params![a,h.project_id,h.entity_id])?;for tip in &plan.new_tips{tx.execute("INSERT INTO cloud_map_tips VALUES(?1,?2,?3,?4)",params![a,h.project_id,h.entity_id,tip])?;}
  if plan.outcome=="applied" {
   let draft:Option<String>=tx.query_row("SELECT snapshot_json FROM cloud_map_local_drafts WHERE account_id=?1 AND project_id=?2 AND entity_id=?3",params![a,h.project_id,h.entity_id],|r|r.get(0)).optional()?;
   if draft.as_ref().is_some_and(|raw|parse(raw).ok()==Some(plan.event.map.clone())) || plan.projection.is_some()&&h.operation=="resolution" {tx.execute("DELETE FROM cloud_map_local_drafts WHERE account_id=?1 AND project_id=?2 AND entity_id=?3",params![a,h.project_id,h.entity_id])?;}
  }
  if plan.outcome=="applied"{tx.execute("INSERT INTO cloud_map_projection VALUES(?1,?2,?3,?4,?5) ON CONFLICT(account_id,project_id,entity_id) DO UPDATE SET head_event_id=excluded.head_event_id,snapshot_json=excluded.snapshot_json",params![a,h.project_id,h.entity_id,h.event_id,plan.event.map.to_string()])?;}
  tx.execute("UPDATE cloud_map_events SET state=?1,blocker=NULL WHERE account_id=?2 AND event_id=?3",params![plan.outcome,a,h.event_id])?;
  tx.execute("INSERT INTO cloud_map_apply_ledger VALUES(?1,?2,?3,?4,?5,?6)",params![a,h.event_id,plan.seq,plan.outcome,plan.nonce,plan.ciphertext])?;
  tx.execute("UPDATE cloud_sync_inbox SET state=?1,applied_at=?2,error_code=NULL WHERE account_id=?3 AND event_id=?4",params![if plan.outcome=="applied"{"applied"}else{"conflict"},h.updated_at,a,h.event_id])?;
  let lifecycle=if plan.outcome=="applied"{"active"}else{"conflict"};
  tx.execute("INSERT INTO cloud_map_migrations VALUES(?1,?2,?3,?4,?5,?6,NULL) ON CONFLICT(account_id,project_id,entity_id) DO UPDATE SET lifecycle=excluded.lifecycle,blocker=NULL",params![a,h.project_id,h.entity_id,h.stage_id,lifecycle,h.event_id])?;
  if plan.own {let receipt:Option<i64>=tx.query_row("SELECT server_sequence FROM cloud_sync_upload_receipts WHERE account_id=?1 AND event_id=?2",params![a,h.event_id],|r|r.get(0)).optional()?;if receipt.is_some_and(|n|n!=plan.seq){return fail("map_exact_replay_mismatch")}if receipt.is_none(){tx.execute("INSERT INTO cloud_sync_upload_receipts VALUES(?1,?2,?3,?4,1,?5)",params![a,h.event_id,h.device_id,plan.seq,h.updated_at])?;}tx.execute("UPDATE cloud_sync_outbox SET lifecycle='accepted' WHERE account_id=?1 AND event_id=?2",params![a,h.event_id])?;}
  Ok(plan.outcome)
 })
}
pub(crate) fn received(db: &Connection, a: &str, after: i64, limit: i64) -> Result<Vec<Value>> {
    if after < 0 || !(1..=32).contains(&limit) {
        return fail("invalid_map_page");
    }
    let tx = db.unchecked_transaction()?;
    let mut q=tx.prepare("SELECT i.event_id,i.server_sequence,i.device_id,i.project_id,i.entity_id,i.sync_revision,i.updated_at,o.nonce,o.ciphertext FROM cloud_sync_inbox i JOIN cloud_sync_event_objects o ON o.account_id=i.account_id AND o.event_id=i.event_id LEFT JOIN cloud_game_reader_visits v ON v.account_id=i.account_id AND v.event_id=i.event_id WHERE i.account_id=?1 AND i.entity_type='map' AND i.operation='event' AND i.state IN ('received','orphan') AND i.server_sequence>?2 ORDER BY COALESCE(v.ordinal,0),i.server_sequence LIMIT ?3")?;
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
    db.query_row("SELECT EXISTS(SELECT 1 FROM cloud_map_apply_ledger l JOIN cloud_map_events e ON e.account_id=l.account_id AND e.event_id=l.event_id JOIN cloud_sync_inbox i ON i.account_id=e.account_id AND i.event_id=e.event_id JOIN cloud_sync_event_objects o ON o.account_id=e.account_id AND o.event_id=e.event_id WHERE l.account_id=?1 AND l.server_sequence=?2 AND e.server_sequence=l.server_sequence AND i.server_sequence=l.server_sequence AND e.state=l.outcome AND i.state=CASE WHEN l.outcome='applied' THEN 'applied' ELSE 'conflict' END AND i.entity_type='map' AND i.operation='event' AND i.project_id=e.project_id AND i.entity_id=e.entity_id AND i.sync_revision=e.revision AND i.device_id=json_extract(CAST(substr(e.canonical_frame,21) AS TEXT),'$.header.device_id') AND i.updated_at=json_extract(CAST(substr(e.canonical_frame,21) AS TEXT),'$.header.updated_at') AND l.nonce=o.nonce AND l.ciphertext=o.ciphertext AND o.crypto_version=1 AND o.aad_version=1 AND i.deleted_at IS NULL)",params![a,seq],|r|r.get(0))
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Decision {
    pub project_id: String,
    pub stage_id: Option<String>,
    pub expected_tips: Vec<String>,
    pub expected_local: Value,
    pub selected_event_id: String,
}
pub(crate) fn decide(
    db: &mut Connection,
    scope: &metadata::MetadataScope,
    d: &Decision,
    now: &str,
) -> Result<String> {
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    metadata::assert_runtime_scope(
        &tx,
        &scope.account_id,
        &scope.canonical_user_id,
        &scope.device_id,
    )?;
    let a = &scope.account_id;
    let id = codec::entity_id(d.stage_id.as_deref());
    let parents = tips(&tx, a, &d.project_id, &id)?;
    let eid = metadata::new_event_id()?;
    tx.execute(
        "INSERT INTO cloud_map_decisions VALUES(?1,?2,?3,?4)",
        params![
            a,
            eid,
            json!(d.expected_tips).to_string(),
            d.expected_local.to_string()
        ],
    )?;
    if parents != d.expected_tips
        || parents.len() < 2
        || source(&tx, &d.project_id, d.stage_id.as_deref())? != d.expected_local
    {
        tx.commit()?;
        return fail("map_resolution_stale");
    }
    let selected = if d.selected_event_id == "local" {
        current(&tx, &d.project_id, d.stage_id.as_deref(), now)?
    } else {
        if !parents.contains(&d.selected_event_id) {
            return fail("map_resolution_invalid");
        }
        let frame:Vec<u8>=tx.query_row("SELECT canonical_frame FROM cloud_map_events WHERE account_id=?1 AND event_id=?2 AND project_id=?3 AND entity_id=?4",params![a,d.selected_event_id,d.project_id,id],|r|r.get(0))?;
        codec::decode(&frame)?.map
    };
    let mut h = header(&tx, a, &d.project_id, d.stage_id.as_deref(), parents, now)?;
    h.event_id = eid.clone();
    let e = event(h, selected);
    queue(&tx, a, &e)?;
    tx.commit()?;
    Ok(eid)
}
/// Explicit local/cloud reconciliation; never invents a synthetic cloud parent.
pub(crate) fn import_choice(
    db: &mut sqlite::PrivilegedRemoteApplyConnection,
    scope: &metadata::MetadataScope,
    d: &Decision,
    keep_local: bool,
    now: &str,
) -> Result<String> {
    db.execute_planned_many_once(|tx|->Result<(Vec<sqlite::OwnedRemoteApplyAuthorization>,(codec::Event,Value,Value,String))>{metadata::assert_runtime_scope(tx,&scope.account_id,&scope.canonical_user_id,&scope.device_id)?;let a=&scope.account_id;let id=codec::entity_id(d.stage_id.as_deref());let parents=tips(tx,a,&d.project_id,&id)?;if parents!=d.expected_tips||parents.len()!=1||parents.first()!=Some(&d.selected_event_id)||source(tx,&d.project_id,d.stage_id.as_deref())?!=d.expected_local{return fail("map_import_stale")}
 let raw:Vec<u8>=tx.query_row("SELECT canonical_frame FROM cloud_map_events WHERE account_id=?1 AND event_id=?2 AND state IN ('applied','conflict_preserved')",params![a,d.selected_event_id],|r|r.get(0))?;let e=codec::decode(&raw)?;codec::dependencies_ready(tx,a,&scope.canonical_user_id,&e.header.device_id,&e)?;
 let chosen=if keep_local{current(tx,&d.project_id,d.stage_id.as_deref(),now)?}else{e.map.clone()};let plan=projection(tx,&d.project_id,d.stage_id.as_deref(),&chosen,now)?;let choice=metadata::new_event_id()?;
 tx.execute("INSERT INTO cloud_map_import_decisions VALUES(?1,?2,?3,?4,?5)",params![a,d.selected_event_id,choice,d.expected_local.to_string(),if keep_local{"local"}else{"remote"}])?;
 let auth=sqlite::OwnedRemoteApplyAuthorization{event_id:e.header.event_id.clone(),account_id:a.clone(),project_id:d.project_id.clone(),entity_id:id,operation:"upsert".into(),payload_json:Some(plan.to_string()),prior_payload_json:Some(d.expected_local.to_string())};Ok((vec![auth],(e,chosen,plan,choice)))
 },|tx,(e,chosen,plan,choice)|->Result<String>{let h=&e.header;let a=&scope.account_id;write_projection(tx,&h.project_id,h.stage_id.as_deref(),&plan,now)?;tx.execute("DELETE FROM cloud_sync_remote_apply_authorizations WHERE event_id=?1",[&h.event_id])?;
 tx.execute("INSERT INTO cloud_map_projection VALUES(?1,?2,?3,?4,?5) ON CONFLICT(account_id,project_id,entity_id) DO UPDATE SET head_event_id=excluded.head_event_id,snapshot_json=excluded.snapshot_json",params![a,h.project_id,h.entity_id,h.event_id,e.map.to_string()])?;tx.execute("UPDATE cloud_map_migrations SET lifecycle='active' WHERE account_id=?1 AND project_id=?2 AND entity_id=?3",params![a,h.project_id,h.entity_id])?;
 if keep_local {let local=event(header(tx,a,&h.project_id,h.stage_id.as_deref(),vec![h.event_id.clone()],now)?,chosen.clone());draft(tx,a,&h.project_id,h.stage_id.as_deref(),&chosen,now)?;queue(tx,a,&local)?;}else{tx.execute("DELETE FROM cloud_map_local_drafts WHERE account_id=?1 AND project_id=?2 AND entity_id=?3",params![a,h.project_id,h.entity_id])?;}
 Ok(choice)
 })
}
/// Editor lifetime CAS covers every displayed owner, including local payloads.
pub(crate) fn expected(db: &Connection, p: &str, s: Option<&str>, combined: bool) -> Result<Value> {
    let mut owners = vec![s.map(str::to_string)];
    if combined {
        for s in rows(
            db,
            "SELECT id FROM stages WHERE project_id=?1 ORDER BY id",
            [p],
        )? {
            owners.push(Some(s))
        }
    }
    let mut out = serde_json::Map::new();
    for s in owners {
        let id = codec::entity_id(s.as_deref());
        let a: Option<String> = db
            .query_row(
                "SELECT account_id FROM cloud_sync_project_bindings WHERE project_id=?1",
                [p],
                |r| r.get(0),
            )
            .optional()?;
        let heads = if let Some(ref a) = a {
            tips(db, a, p, &id)?
        } else {
            vec![]
        };
        let o = owner(db, p, s.as_deref())?;
        out.insert(id,json!({"stage_id":s,"data":o.get("mindmap").unwrap_or(&Value::Null),"annotations":o.get("map_note_annotations").unwrap_or(&Value::Null),"heads":heads,"name":o.get("name"),"status":o.get("status"),"combine_stage_mindmaps":o.get("combine_stage_mindmaps"),"metadata_heads":if let Some(ref a)=a{metadata::authority_view(db,a,p)?.branches.into_iter().map(|b|b.event_id).collect::<Vec<_>>()}else{vec![]},"stage_heads":if let (Some(ref a),Some(ref s))=(&a,&s){rows(db,"SELECT event_id FROM cloud_sync_structural_tips WHERE account_id=?1 AND project_id=?2 AND entity_type='stage' AND entity_id=?3 ORDER BY event_id",params![a,p,s])?}else{vec![]}}));
    }
    canonical_map(&Value::Object(out))
}
pub(crate) fn check_expected(
    tx: &Transaction<'_>,
    p: &str,
    s: Option<&str>,
    combined: bool,
    rendered: Option<&Value>,
    now: &str,
) -> Result<()> {
    let current = expected(tx, p, s, combined)?;
    let started:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM cloud_map_migrations WHERE project_id=?1 AND lifecycle!='blocked')",[p],|r|r.get(0))?;
    if rendered.is_none() && !started {
        return Ok(());
    }
    if rendered != Some(&current) {
        return fail("map_combined_stale_heads");
    }
    if combined {
        tx.execute(
            "INSERT INTO cloud_map_combined_groups VALUES(?1,?2,?3,?4,?5)",
            params![
                metadata::new_event_id()?,
                p,
                rendered.unwrap().to_string(),
                current.to_string(),
                now
            ],
        )?;
    }
    Ok(())
}
#[derive(Deserialize)]
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
    Import {
        decision: Decision,
        keep_local: bool,
        now: String,
    },
    Delete {
        project_id: String,
        stage_id: Option<String>,
        expected: Value,
        now: String,
    },
}
#[cfg(test)]
mod tests {
    use super::*;
    const A: &str = "map-test-account";
    const NOW: &str = "2026-10-03T00:00:00.000000Z";
    fn fixture() -> codec::Event {
        let v: Value =
            serde_json::from_str(include_str!("../../src/cloud/__fixtures__/mapCodecV1.json"))
                .unwrap();
        serde_json::from_value(v["examples"][2]["event"].clone()).unwrap()
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
            std::env::temp_dir().join(format!("c18503-{}.db", metadata::new_event_id().unwrap()));
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
        if local {
            let e = fixture();
            let mut o = owner(&db, "P1", None).unwrap();
            o["mindmap"] = e.map["data"].clone();
            db.execute(
                "UPDATE projects SET payload_json=?1 WHERE id='P1'",
                [o.to_string()],
            )
            .unwrap();
            let plan = projection(&db, "P1", None, &e.map, NOW).unwrap();
            let tx = db.transaction().unwrap();
            write_projection(&tx, "P1", None, &plan, NOW).unwrap();
            tx.commit().unwrap();
            db.execute("UPDATE projects SET payload_json=json_remove(payload_json,'$.map_note_annotations') WHERE id='P1'",[]).unwrap();
        }
        (db, path)
    }
    fn inbox(db: &Connection, e: &codec::Event, seq: i64) {
        let h = &e.header;
        db.execute("INSERT INTO cloud_sync_inbox(account_id,event_id,server_sequence,device_id,project_id,entity_id,entity_type,operation,sync_revision,updated_at,state,received_at) VALUES(?1,?2,?3,?4,?5,?6,'map','event',?7,?8,'received',?8)",params![A,h.event_id,seq,h.device_id,h.project_id,h.entity_id,h.revision,NOW]).unwrap();
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
        inbox(&db, e, seq);
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
    fn activated() -> (Connection, std::path::PathBuf, codec::Event) {
        let (mut db, path) = seed(true);
        begin(&mut db, &scope(), "P1", NOW).unwrap();
        let v = pending(&db, A, &scope().device_id, false).unwrap();
        let e: codec::Event = serde_json::from_value(v[0]["event"].clone()).unwrap();
        seal(
            &mut db,
            A,
            &e.header.event_id,
            &codec::encode(&e).unwrap(),
            &[1; 24],
            &[2; 32],
        )
        .unwrap();
        drop(db);
        assert_eq!(receive(&path, &e, 2), "applied");
        (sqlite::open_database(&path).unwrap(), path, e)
    }
    #[test]
    fn map_explicit_capture_exactly_once_restart_lost_response_and_writer() {
        let (mut db, path) = seed(true);
        assert!(pending(&db, A, &scope().device_id, false)
            .unwrap()
            .is_empty());
        assert_eq!(
            view(&db, &scope(), "P1").unwrap()["owners"][0]["state"],
            "local"
        );
        let first = begin(&mut db, &scope(), "P1", NOW).unwrap();
        begin(&mut db, &scope(), "P1", NOW).unwrap();
        assert_eq!(
            db.query_row("SELECT count(*) FROM cloud_map_candidates", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(first["owners"][0]["state"], "publication_pending");
        let v = pending(&db, A, &scope().device_id, false).unwrap();
        let e: codec::Event = serde_json::from_value(v[0]["event"].clone()).unwrap();
        let frame = codec::encode(&e).unwrap();
        seal(&mut db, A, &e.header.event_id, &frame, &[1; 24], &[2; 32]).unwrap();
        assert!(seal(&mut db, A, &e.header.event_id, &frame, &[3; 24], &[2; 32]).is_err());
        drop(db);
        assert_eq!(receive(&path, &e, 2), "applied");
        let mut db = sqlite::open_database(&path).unwrap();
        assert!(ack_proven(&db, A, 2).unwrap());
        assert_eq!(
            view(&db, &scope(), "P1").unwrap()["owners"][0]["state"],
            "active"
        );
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM cloud_sync_upload_receipts WHERE event_id=?1",
                [&e.header.event_id],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        let id = e.map["annotations"]["note-1"]["note_id"].as_str().unwrap();
        crate::update_note_in_connection(
            &mut db,
            "P1",
            id,
            &json!({"content":"Edited via Note UI","tags":["new"],"pinned":false}),
            None,
        )
        .unwrap();
        let payload = owner(&db, "P1", None).unwrap();
        assert_eq!(
            payload["mindmap"]["freeNodes"][0]["topic"],
            "Edited via Note UI"
        );
        assert_eq!(
            payload["map_note_annotations"]["note-1"]["tags"],
            json!(["new"])
        );
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM cloud_sync_outbox WHERE entity_type='note'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        assert_eq!(pending(&db, A, &scope().device_id, false).unwrap().len(), 1);
    }
    #[test]
    fn map_remote_atomic_rollback_and_preexisting_local_reconciliation() {
        let (db, path) = seed(false);
        let e = fixture();
        inbox(&db, &e, 2);
        db.execute_batch("CREATE TRIGGER crash_map_projection BEFORE INSERT ON notes BEGIN SELECT RAISE(ABORT,'crash'); END;").unwrap();
        drop(db);
        let mut privileged = sqlite::open_privileged_remote_apply_database(&path).unwrap();
        assert!(apply(
            &mut privileged,
            &scope(),
            &codec::encode(&e).unwrap(),
            &[1; 24],
            &[2; 32]
        )
        .is_err());
        let db = privileged.connection();
        assert_eq!(
            db.query_row("SELECT count(*) FROM cloud_map_events", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert!(owner(db, "P1", None).unwrap().get("mindmap").is_none());
        assert!(!ack_proven(db, A, 2).unwrap());
        db.execute_batch("DROP TRIGGER crash_map_projection")
            .unwrap();
        assert_eq!(
            apply(
                &mut privileged,
                &scope(),
                &codec::encode(&e).unwrap(),
                &[1; 24],
                &[2; 32]
            )
            .unwrap(),
            "applied"
        );
        assert_eq!(privileged.connection().query_row("SELECT count(*) FROM notes WHERE json_extract(payload_json,'$.source_type')='mindmap'",[],|r|r.get::<_,i64>(0)).unwrap(),1);
        let (db, path) = seed(true);
        drop(db);
        assert_eq!(receive(&path, &e, 2), "conflict_preserved");
        let db = sqlite::open_database(&path).unwrap();
        let local = source(&db, "P1", None).unwrap();
        let d = Decision {
            project_id: "P1".into(),
            stage_id: None,
            expected_tips: vec![e.header.event_id.clone()],
            expected_local: local,
            selected_event_id: e.header.event_id.clone(),
        };
        assert_eq!(
            db.query_row("SELECT count(*) FROM cloud_map_local_candidates", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        drop(db);
        let mut db = sqlite::open_privileged_remote_apply_database(&path).unwrap();
        import_choice(&mut db, &scope(), &d, false, NOW).unwrap();
        assert_eq!(current(db.connection(), "P1", None, NOW).unwrap(), e.map);
    }
    #[test]
    fn map_writer_rejects_lossy_renderer_normalization() {
        let (mut db, _path, root) = activated();
        let before = source(&db, "P1", None).unwrap();
        let heads = expected(&db, "P1", None, false).unwrap();
        let mut data = root.map["data"].clone();
        data["freeNodes"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id":"bad","topic":null,"children":[]}));
        let normalized = crate::mindmap::normalize(data.clone()).unwrap();
        let command = crate::MapCommand {
            project_id: "P1".into(),
            stage_id: None,
            data,
            expected_heads: Some(heads),
        };
        assert!(crate::save_map_in_connection(&mut db, &command, &normalized).is_err());
        assert_eq!(source(&db, "P1", None).unwrap(), before);
        assert!(pending(&db, A, &scope().device_id, false)
            .unwrap()
            .is_empty());
    }
    #[test]
    fn map_post_sealing_draft_survives_echo_and_queues_causal_child() {
        let (mut db, path) = seed(true);
        begin(&mut db, &scope(), "P1", NOW).unwrap();
        let item = pending(&db, A, &scope().device_id, false)
            .unwrap()
            .remove(0);
        let e: codec::Event = serde_json::from_value(item["event"].clone()).unwrap();
        let frame = codec::encode(&e).unwrap();
        seal(&mut db, A, &e.header.event_id, &frame, &[1; 24], &[2; 32]).unwrap();
        let mut data = e.map["data"].clone();
        data["nodeData"]["topic"] = json!("Edited after seal");
        let tx = db.transaction().unwrap();
        local_edit(&tx, "P1", None, &data, None, NOW).unwrap();
        tx.commit().unwrap();
        let frozen: Vec<u8> = db
            .query_row(
                "SELECT canonical_frame FROM cloud_map_events WHERE event_id=?1",
                [&e.header.event_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(frame, frozen);
        drop(db);
        assert_eq!(receive(&path, &e, 2), "applied");
        let mut db = sqlite::open_database(&path).unwrap();
        assert_eq!(
            owner(&db, "P1", None).unwrap()["mindmap"]["nodeData"]["topic"],
            "Edited after seal"
        );
        advance(&mut db, A, NOW).unwrap();
        let child = pending(&db, A, &scope().device_id, false).unwrap();
        assert_eq!(child.len(), 1);
        assert_eq!(
            child[0]["event"]["header"]["parents"],
            json!([e.header.event_id])
        );
        assert_eq!(child[0]["event"]["header"]["revision"], 2);
        assert_eq!(
            child[0]["event"]["map"]["data"]["nodeData"]["topic"],
            "Edited after seal"
        );
    }
    #[test]
    fn map_combined_owner_cas_and_transaction_rollback() {
        let (mut db, _path, _root) = activated();
        let raw: Vec<u8> = db
            .query_row(
                "SELECT canonical_payload FROM cloud_sync_metadata_apply_ledger LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let mut m: Value = serde_json::from_slice(&raw).unwrap();
        let parent = m["header"]["event_id"].clone();
        m["header"]["event_id"] = json!(metadata::new_event_id().unwrap());
        m["header"]["operation"] = json!("update");
        m["header"]["parent_event_ids"] = json!([parent]);
        m["header"]["revision"] = json!(2);
        m["header"]["generation"] = json!(2);
        m["metadata"]["combine_stage_mindmaps"] = json!(true);
        db.execute("INSERT INTO cloud_sync_inbox(account_id,event_id,server_sequence,device_id,project_id,entity_id,entity_type,operation,sync_revision,updated_at,state,received_at) VALUES(?1,?2,3,?3,'P1','P1','project_metadata','upsert',2,?4,'received',?4)",params![A,m["header"]["event_id"].as_str(),scope().device_id,NOW]).unwrap();
        db.execute(
            "INSERT INTO cloud_sync_event_objects VALUES(?1,?2,1,1,?3,?4,?5)",
            params![
                A,
                m["header"]["event_id"].as_str(),
                vec![1u8; 24],
                vec![2u8; 32],
                NOW
            ],
        )
        .unwrap();
        metadata::preserve_authenticated_event(&mut db, A, "P1", m.to_string().as_bytes(), NOW)
            .unwrap();
        let sm = json!({"nodeData":{"id":"stage-root","topic":"Stage","children":[]},"freeNodes":[],"arrows":[],"summaries":[]});
        db.execute("INSERT INTO stages(id,project_id,name,infinite,unit,status,payload_json) VALUES('S','P1','Stage',1,'symbols','active',?1)",[json!({"mindmap":sm}).to_string()]).unwrap();
        db.execute("INSERT INTO stage_order VALUES('S','P1',0)", [])
            .unwrap();
        let (project, stages) = {
            let repo = crate::project_repository::ProjectsRepository::new(&mut db);
            (
                repo.get_project("P1").unwrap().unwrap(),
                repo.list_stages("P1").unwrap(),
            )
        };
        let mut data = crate::compose_combined_map(&project, &stages).unwrap();
        data["nodeData"]["topic"] = json!("Changed project");
        let before = source(&db, "P1", None).unwrap();
        let before_stage = owner(&db, "P1", Some("S")).unwrap();
        let expected = expected(&db, "P1", None, true).unwrap();
        let count: i64 = db
            .query_row("SELECT count(*) FROM cloud_sync_outbox", [], |r| r.get(0))
            .unwrap();
        db.execute_batch("CREATE TRIGGER crash_last_owner BEFORE UPDATE ON stages BEGIN SELECT RAISE(ABORT,'crash'); END;").unwrap();
        let command = crate::MapCommand {
            project_id: "P1".into(),
            stage_id: None,
            data: data.clone(),
            expected_heads: Some(expected.clone()),
        };
        assert!(crate::save_map_in_connection(&mut db, &command, &data).is_err());
        assert_eq!(source(&db, "P1", None).unwrap(), before);
        assert_eq!(owner(&db, "P1", Some("S")).unwrap(), before_stage);
        assert_eq!(
            db.query_row("SELECT count(*) FROM cloud_sync_outbox", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            count
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM cloud_map_combined_groups", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap(),
            0
        );
        db.execute_batch("DROP TRIGGER crash_last_owner").unwrap();
        db.execute("UPDATE stages SET payload_json=json_set(payload_json,'$.mindmap.nodeData.topic','New Stage') WHERE id='S'",[]).unwrap();
        assert_eq!(
            crate::save_map_in_connection(&mut db, &command, &data).unwrap_err(),
            "map_combined_stale_heads"
        );
        assert_eq!(source(&db, "P1", None).unwrap(), before);
        let command = crate::MapCommand {
            expected_heads: Some(super::expected(&db, "P1", None, true).unwrap()),
            ..command
        };
        crate::save_map_in_connection(&mut db, &command, &data).unwrap();
        assert_eq!(
            db.query_row("SELECT count(*) FROM cloud_map_combined_groups", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap(),
            1
        );
        assert_eq!(db.query_row("SELECT count(*) FROM cloud_sync_outbox WHERE entity_id='combined-map' OR entity_type='note'",[],|r|r.get::<_,i64>(0)).unwrap(),0);
    }
    #[test]
    fn map_unsupported_annotation_preserves_evidence_and_local_only_excluded() {
        let (mut db, _) = seed(true);
        db.execute(
            "UPDATE projects SET payload_json=json_remove(payload_json,'$.map_note_annotations')",
            [],
        )
        .unwrap();
        db.execute("UPDATE notes SET payload_json=json_set(payload_json,'$.metadata.extension','retained')",[]).unwrap();
        let before = source(&db, "P1", None).unwrap();
        let view = begin(&mut db, &scope(), "P1", NOW).unwrap();
        assert_eq!(view["owners"][0]["state"], "blocked");
        assert_eq!(source(&db, "P1", None).unwrap(), before);
        assert!(pending(&db, A, &scope().device_id, false)
            .unwrap()
            .is_empty());
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM cloud_map_candidates WHERE blocker IS NOT NULL",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        db.execute("INSERT INTO projects(id,name,infinite,unit,status,payload_json) VALUES('local','Local',1,'symbols','active','{}')",[]).unwrap();
        assert!(begin(&mut db, &scope(), "local", NOW).is_err());
    }
    #[test]
    fn map_full_tip_resolution_stale_decision_late_branch_and_delete_edit() {
        let (mut db, path, root) = activated();
        let mut a = root.clone();
        a.header.event_id = metadata::new_event_id().unwrap();
        a.header.parents = vec![root.header.event_id.clone()];
        a.header.operation = "update".into();
        a.header.revision = 2;
        a.header.generation = 2;
        a.map["data"]["nodeData"]["topic"] = json!("A");
        let mut b = a.clone();
        b.header.event_id = metadata::new_event_id().unwrap();
        b.map["data"]["nodeData"]["topic"] = json!("B");
        drop(db);
        assert_eq!(receive(&path, &a, 3), "applied");
        assert_eq!(receive(&path, &b, 4), "conflict_preserved");
        db = sqlite::open_database(&path).unwrap();
        let parents = tips(&db, A, "P1", "project-map").unwrap();
        assert_eq!(parents.len(), 2);
        let d = Decision {
            project_id: "P1".into(),
            stage_id: None,
            expected_tips: parents,
            expected_local: source(&db, "P1", None).unwrap(),
            selected_event_id: b.header.event_id.clone(),
        };
        let eid = decide(&mut db, &scope(), &d, NOW).unwrap();
        let raw: Vec<u8> = db
            .query_row(
                "SELECT canonical_frame FROM cloud_map_events WHERE event_id=?1",
                [&eid],
                |r| r.get(0),
            )
            .unwrap();
        let r = codec::decode(&raw).unwrap();
        seal(&mut db, A, &eid, &raw, &[1; 24], &[2; 32]).unwrap();
        drop(db);
        assert_eq!(receive(&path, &r, 5), "applied");
        db = sqlite::open_database(&path).unwrap();
        assert_eq!(current(&db, "P1", None, NOW).unwrap(), b.map);
        assert!(decide(&mut db, &scope(), &d, NOW).is_err());
        let mut c = a.clone();
        c.header.event_id = metadata::new_event_id().unwrap();
        c.header.operation = "delete".into();
        c.mutation = "delete".into();
        c.map = Value::Null;
        c.deleted_at = json!(NOW);
        drop(db);
        assert_eq!(receive(&path, &c, 6), "conflict_preserved");
        db = sqlite::open_database(&path).unwrap();
        let tips = tips(&db, A, "P1", "project-map").unwrap();
        assert!(tips.contains(&eid) && tips.contains(&c.header.event_id));
        assert_eq!(tips.len(), 2);
        assert_eq!(current(&db, "P1", None, NOW).unwrap(), b.map);
        assert!(ack_proven(&db, A, 6).unwrap());
    }
}

pub(crate) fn admitted(db: &Connection, p: &str, s: Option<&str>) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM cloud_map_migrations WHERE project_id=?1 AND entity_id=?2)",
        params![p, codec::entity_id(s)],
        |r| r.get(0),
    )?)
}
pub(crate) fn owner_data(db: &Connection, p: &str, s: Option<&str>) -> Result<Value> {
    Ok(owner(db, p, s)?
        .get("mindmap")
        .cloned()
        .unwrap_or(Value::Null))
}
