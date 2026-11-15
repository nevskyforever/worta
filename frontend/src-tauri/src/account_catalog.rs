//! Generic account catalog. Renderer authenticates v2; native repeats codec,
//! descriptor, scope, dependency and causal checks in one IMMEDIATE transaction.
use crate::project_metadata_sync::{self as metadata, MetadataScope};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
const LIMIT: usize = 16384;
const MAX_BYTES: usize = 4 * 1024 * 1024;
const TYPES: [&str; 4] = [
    "folder",
    "folder_order",
    "folder_membership",
    "project_order",
];
#[derive(Debug)]
pub(crate) enum Error {
    Database(rusqlite::Error),
    Code(&'static str),
}
impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Self::Database(e)
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Database(_) => write!(f, "catalog_storage_error"),
            Self::Code(c) => write!(f, "{c}"),
        }
    }
}
type Result<T> = std::result::Result<T, Error>;
fn invalid() -> Error {
    Error::Code("invalid_catalog_frame")
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Header {
    pub account_id: String,
    pub scope: String,
    pub device_id: String,
    pub entity_type: String,
    pub entity_id: String,
    pub event_id: String,
    pub operation: String,
    pub parent_event_ids: Vec<String>,
    pub revision: i64,
    pub generation: i64,
    pub updated_at: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProjectProof {
    pub bootstrap_id: String,
    pub metadata_event_id: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct Dependencies {
    pub folders: BTreeMap<String, Vec<String>>,
    pub projects: BTreeMap<String, ProjectProof>,
    pub memberships: BTreeMap<String, Vec<String>>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Event {
    pub version: i64,
    pub header: Header,
    pub payload: Value,
    pub dependencies: Dependencies,
    pub deleted_at: Option<String>,
}
fn text(s: &str) -> bool {
    !s.is_empty() && s.len() <= 512
}
fn heads(ids: &[String], empty: bool) -> bool {
    (empty || !ids.is_empty())
        && ids.len() <= 64
        && ids.iter().all(|s| metadata::uuid(s))
        && ids.windows(2).all(|p| p[0] < p[1])
}
fn strings(v: &Value) -> Result<Vec<String>> {
    v.as_array()
        .ok_or_else(invalid)?
        .iter()
        .map(|v| {
            v.as_str()
                .filter(|s| text(s))
                .map(String::from)
                .ok_or_else(invalid)
        })
        .collect()
}
fn exact(v: &Value, keys: &[&str]) -> bool {
    v.as_object()
        .is_some_and(|o| o.len() == keys.len() && keys.iter().all(|k| o.contains_key(*k)))
}
fn keys_match<T>(map: &BTreeMap<String, T>, ids: &[String]) -> bool {
    map.len() == ids.len() && ids.iter().all(|id| map.contains_key(id))
}
pub(crate) fn validate(e: &Event) -> Result<()> {
    let h = &e.header;
    let d = &e.dependencies;
    if e.version != 1
        || h.scope != "account"
        || ![&h.account_id, &h.device_id, &h.event_id]
            .iter()
            .all(|s| metadata::uuid(s))
        || !text(&h.entity_id)
        || !TYPES.contains(&h.entity_type.as_str())
        || !matches!(
            h.operation.as_str(),
            "create" | "update" | "delete" | "resolution"
        )
        || !(1..=9007199254740991).contains(&h.revision)
        || !(1..=9007199254740991).contains(&h.generation)
        || !metadata::timestamp(&h.updated_at)
        || !heads(&h.parent_event_ids, true)
        || h.parent_event_ids.contains(&h.event_id)
        || (if h.operation == "create" {
            !h.parent_event_ids.is_empty() || h.revision != 1 || h.generation != 1
        } else {
            h.parent_event_ids.is_empty() || h.revision < 2 || h.generation < 2
        })
    {
        return Err(invalid());
    }
    if matches!(h.operation.as_str(), "update" | "delete") && h.parent_event_ids.len() != 1 {
        return Err(invalid());
    }
    if d.folders.len() > LIMIT
        || d.projects.len() > LIMIT
        || d.memberships.len() > LIMIT
        || !d
            .folders
            .iter()
            .chain(d.memberships.iter())
            .all(|(k, v)| text(k) && heads(v, false))
        || !d.projects.iter().all(|(k, p)| {
            text(k) && metadata::uuid(&p.bootstrap_id) && metadata::uuid(&p.metadata_event_id)
        })
    {
        return Err(invalid());
    }
    match h.entity_type.as_str() {
        "folder" => {
            if !d.folders.is_empty() || !d.projects.is_empty() || !d.memberships.is_empty() {
                return Err(invalid());
            }
            if e.payload.is_null() {
                if e.deleted_at.as_ref() != Some(&h.updated_at)
                    || !matches!(h.operation.as_str(), "delete" | "resolution")
                {
                    return Err(invalid());
                }
            } else if !exact(&e.payload, &["name"])
                || !e.payload["name"]
                    .as_str()
                    .is_some_and(|s| text(s) && s.chars().count() <= 120 && !s.trim().is_empty())
                || e.deleted_at.is_some()
                || h.operation == "delete"
            {
                return Err(invalid());
            }
        }
        "folder_membership" => {
            if !exact(&e.payload, &["folder_id"])
                || !keys_match(&d.projects, &[h.entity_id.clone()])
                || !d.memberships.is_empty()
            {
                return Err(invalid());
            }
            let ids = if e.payload["folder_id"].is_null() {
                if e.deleted_at.as_ref() != Some(&h.updated_at) {
                    return Err(invalid());
                }
                vec![]
            } else {
                if e.deleted_at.is_some() || h.operation == "delete" {
                    return Err(invalid());
                }
                vec![e.payload["folder_id"]
                    .as_str()
                    .filter(|s| text(s))
                    .ok_or_else(invalid)?
                    .into()]
            };
            if !keys_match(&d.folders, &ids) {
                return Err(invalid());
            }
        }
        _ => {
            if h.entity_id != h.entity_type
                || h.operation == "delete"
                || e.deleted_at.is_some()
                || !exact(&e.payload, &["ids"])
            {
                return Err(invalid());
            }
            let ids = strings(&e.payload["ids"])?;
            let set: std::collections::BTreeSet<_> = ids.iter().collect();
            if ids.len() > LIMIT || set.len() != ids.len() {
                return Err(invalid());
            }
            if h.entity_type == "folder_order" {
                if !keys_match(&d.folders, &ids)
                    || !d.projects.is_empty()
                    || !d.memberships.is_empty()
                {
                    return Err(invalid());
                }
            } else if !keys_match(&d.projects, &ids)
                || !d.folders.is_empty()
                || !keys_match(&d.memberships, &ids)
            {
                return Err(invalid());
            }
        }
    };
    Ok(())
}
// Reuse the accepted ECMAScript canonical serializer (UTF-16 key ordering).
pub(crate) fn frame(e: &Event) -> Result<Vec<u8>> {
    validate(e)?;
    let value = serde_json::to_value(e).map_err(|_| invalid())?;
    let raw = crate::stage_sync::canonical(&value)
        .map_err(|_| invalid())?
        .into_bytes();
    if raw.len() > MAX_BYTES {
        return Err(Error::Code("catalog_resource_limit"));
    }
    let mut result = b"WORTA-C1".to_vec();
    result.extend_from_slice(&[
        1,
        4 + TYPES
            .iter()
            .position(|t| *t == e.header.entity_type)
            .unwrap() as u8,
        1,
        0,
    ]);
    result.extend_from_slice(&(raw.len() as u32).to_be_bytes());
    result.extend_from_slice(&(raw.len() as u32).to_be_bytes());
    result.extend(raw);
    Ok(result)
}
pub(crate) fn unframe(raw: &[u8]) -> Result<Event> {
    let normalized;
    let raw = if raw.len() >= 20
        && raw[11] != 0
        && [4, 5, 6, 7].contains(&raw[9])
        && [1].contains(&raw[10])
    {
        normalized = crate::frame_compression::normalize_authenticated_frame(
            raw,
            &[4, 5, 6, 7],
            &[1],
            MAX_BYTES,
        )
        .map_err(Error::Code)?;
        normalized.as_slice()
    } else {
        raw
    };

    if raw.len() < 20
        || raw.len() > MAX_BYTES + 20
        || &raw[..8] != b"WORTA-C1"
        || raw[8] != 1
        || raw[10] != 1
        || raw[11] != 0
        || u32::from_be_bytes(raw[12..16].try_into().unwrap()) as usize != raw.len() - 20
        || raw[12..16] != raw[16..20]
    {
        return Err(invalid());
    }
    let e: Event = serde_json::from_slice(&raw[20..]).map_err(|_| invalid())?;
    if frame(&e)? != raw {
        return Err(invalid());
    }
    Ok(e)
}
fn scope(db: &Connection, s: &MetadataScope) -> Result<()> {
    metadata::assert_runtime_scope(db, &s.account_id, &s.canonical_user_id, &s.device_id)
        .map_err(|_| Error::Code("account_scope_rejected"))
}
fn rows(db: &Connection, sql: &str, a: &str) -> Result<Vec<(String, String)>> {
    let mut q = db.prepare(sql)?;
    let out = q
        .query_map(
            rusqlite::params_from_iter(if sql.contains("?1") { vec![a] } else { vec![] }),
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(out)
}
pub(crate) fn tips(db: &Connection, a: &str, t: &str, id: &str) -> Result<Vec<String>> {
    let mut q=db.prepare("SELECT event_id FROM cloud_catalog_tips WHERE account_id=?1 AND entity_type=?2 AND entity_id=?3 ORDER BY event_id")?;
    let out = q
        .query_map(params![a, t, id], |r| r.get(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(out)
}
fn event(db: &Connection, a: &str, id: &str) -> Result<Option<Event>> {
    let raw: Option<Vec<u8>> = db
        .query_row(
            "SELECT canonical_frame FROM cloud_catalog_events WHERE account_id=?1 AND event_id=?2",
            params![a, id],
            |r| r.get(0),
        )
        .optional()?;
    raw.map(|b| unframe(&b)).transpose()
}
fn eligible(db: &Connection, a: &str) -> Result<BTreeMap<String, ProjectProof>> {
    let pairs=rows(db,"SELECT b.project_id,boot.bootstrap_id FROM cloud_sync_project_bindings b JOIN cloud_sync_project_bootstraps boot ON boot.project_id=b.project_id AND boot.account_id=b.account_id WHERE b.account_id=?1 ORDER BY b.project_id",a)?;
    let mut result = BTreeMap::new();
    for (p, boot) in pairs {
        let view = metadata::authority_view(db, a, &p)
            .map_err(|_| Error::Code("catalog_project_unproven"))?;
        if view.state != "active" {
            return Err(Error::Code("catalog_project_unproven"));
        }
        result.insert(
            p,
            ProjectProof {
                bootstrap_id: boot,
                metadata_event_id: view
                    .head_event_id
                    .ok_or(Error::Code("catalog_project_unproven"))?,
            },
        );
    }
    let n: i64 = db.query_row(
        "SELECT count(*) FROM cloud_sync_project_bindings WHERE account_id=?1",
        [a],
        |r| r.get(0),
    )?;
    if n as usize != result.len() {
        return Err(Error::Code("catalog_project_unproven"));
    }
    if result.len() > LIMIT {
        return Err(Error::Code("catalog_resource_limit"));
    }
    Ok(result)
}
fn source(db: &Connection) -> Result<Value> {
    let folders = rows(
        db,
        "SELECT id,payload_json FROM project_folders ORDER BY position,id",
        "",
    )?;
    let members = rows(
        db,
        "SELECT project_id,folder_id FROM project_folder_members ORDER BY project_id",
        "",
    )?;
    let mut q = db.prepare("SELECT project_id FROM project_order ORDER BY position")?;
    let order = q
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(json!({"folders":folders,"members":members,"order":order}))
}
fn portable(db: &Connection, a: &str) -> Result<Value> {
    let projects = eligible(db, a)?;
    let folders = rows(
        db,
        "SELECT id,name FROM project_folders ORDER BY position,id",
        "",
    )?;
    if folders
        .iter()
        .any(|(id, n)| !text(id) || !text(n) || n.chars().count() > 120 || n.trim().is_empty())
    {
        return Err(Error::Code("unsupported_catalog_source"));
    }
    if folders.len() > LIMIT {
        return Err(Error::Code("catalog_resource_limit"));
    }
    let mut q = db.prepare("SELECT project_id FROM project_order ORDER BY position")?;
    let order = q
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|id| projects.contains_key(id))
        .collect::<Vec<_>>();
    if order.len() != projects.len() {
        return Err(Error::Code("unsupported_catalog_source"));
    }
    let members = rows(
        db,
        "SELECT project_id,folder_id FROM project_folder_members ORDER BY project_id",
        "",
    )?
    .into_iter()
    .filter(|(id, _)| projects.contains_key(id))
    .collect::<BTreeMap<_, _>>();
    let raw = source(db)?;
    for pair in raw["folders"].as_array().unwrap() {
        let payload: Value = serde_json::from_str(pair[1].as_str().unwrap())
            .map_err(|_| Error::Code("unsupported_catalog_source"))?;
        if !exact(&payload, &["id", "name"])
            || payload["id"] != pair[0]
            || !folders
                .iter()
                .any(|(id, n)| json!(id) == pair[0] && json!(n) == payload["name"])
        {
            return Err(Error::Code("unsupported_catalog_source"));
        }
    }
    Ok(json!({"folders":folders,"members":members,"order":order,"projects":projects}))
}
fn local(db: &Connection, a: &str, t: &str, id: &str) -> Result<Value> {
    match t {
        "folder" => {
            let deleted:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM cloud_catalog_projection WHERE account_id=?1 AND entity_type='folder' AND entity_id=?2 AND deleted=1)",params![a,id],|r|r.get(0))?;
            if deleted {
                return Ok(Value::Null);
            }
            let name: Option<String> = db
                .query_row("SELECT name FROM project_folders WHERE id=?1", [id], |r| {
                    r.get(0)
                })
                .optional()?;
            Ok(name.map(|n| json!({"name":n})).unwrap_or(Value::Null))
        }
        "folder_membership" => {
            let folder: Option<String> = db
                .query_row(
                    "SELECT folder_id FROM project_folder_members WHERE project_id=?1",
                    [id],
                    |r| r.get(0),
                )
                .optional()?;
            Ok(json!({"folder_id":folder}))
        }
        "folder_order" => {
            let has:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM cloud_catalog_projection WHERE account_id=?1 AND entity_type='folder_order')",[a],|r|r.get(0))?;
            let all = rows(
                db,
                "SELECT id,name FROM project_folders ORDER BY position,id",
                "",
            )?
            .into_iter()
            .map(|(id, _)| id)
            .collect::<Vec<_>>();
            let live=rows(db,"SELECT entity_id,head_event_id FROM cloud_catalog_projection WHERE account_id=?1 AND entity_type='folder' AND deleted=0",a)?.into_iter().collect::<BTreeMap<_,_>>();
            Ok(
                json!({"ids":all.into_iter().filter(|id|!has||live.contains_key(id)).collect::<Vec<_>>()}),
            )
        }
        "project_order" => {
            let p = portable(db, a)?;
            Ok(json!({"ids":p["order"]}))
        }
        _ => Err(invalid()),
    }
}
fn insert(db: &Connection, a: &str, e: &Event, state: &str) -> Result<()> {
    let raw = frame(e)?;
    let(count,size):(i64,i64)=db.query_row("SELECT count(*),COALESCE(sum(length(canonical_frame)+COALESCE(length(ciphertext),0)),0) FROM cloud_catalog_events WHERE account_id=?1",[a],|r|Ok((r.get(0)?,r.get(1)?)))?;
    if count >= 131072 || size + 2 * raw.len() as i64 + 16 > 268435456 {
        return Err(Error::Code("catalog_resource_limit"));
    }
    db.execute("INSERT INTO cloud_catalog_events(account_id,event_id,entity_type,entity_id,canonical_frame,state) VALUES(?1,?2,?3,?4,?5,?6)",params![a,e.header.event_id,e.header.entity_type,e.header.entity_id,raw,state])?;
    Ok(())
}
fn make(
    s: &MetadataScope,
    t: &str,
    id: &str,
    payload: Value,
    parents: Vec<String>,
    rev: i64,
    gen: i64,
    d: Dependencies,
    now: &str,
    resolution: bool,
) -> Result<Event> {
    let deleted = (t == "folder" && payload.is_null())
        || (t == "folder_membership" && payload["folder_id"].is_null());
    let e = Event {
        version: 1,
        header: Header {
            account_id: s.canonical_user_id.clone(),
            scope: "account".into(),
            device_id: s.device_id.clone(),
            entity_type: t.into(),
            entity_id: id.into(),
            event_id: metadata::new_event_id().map_err(|_| invalid())?,
            operation: if parents.is_empty() {
                "create"
            } else if resolution {
                "resolution"
            } else if deleted {
                "delete"
            } else {
                "update"
            }
            .into(),
            parent_event_ids: parents,
            revision: rev,
            generation: gen,
            updated_at: now.into(),
        },
        payload,
        dependencies: d,
        deleted_at: deleted.then(|| now.into()),
    };
    validate(&e)?;
    Ok(e)
}
fn folder_heads(db: &Connection, a: &str) -> Result<BTreeMap<String, Vec<String>>> {
    let mut map = BTreeMap::new();
    for (id,_) in rows(db,"SELECT entity_id,head_event_id FROM cloud_catalog_projection WHERE account_id=?1 AND entity_type='folder' AND deleted=0 ORDER BY entity_id",a)?{let hs=tips(db,a,"folder",&id)?;if hs.len()!=1{return Err(Error::Code("catalog_dependency_conflict"))}map.insert(id,hs);}
    Ok(map)
}
fn dependencies(db: &Connection, a: &str, t: &str, id: &str, p: &Value) -> Result<Dependencies> {
    let mut d = Dependencies::default();
    match t {
        "folder" => {}
        "folder_order" => {
            d.folders = folder_heads(db, a)?;
            if !keys_match(&d.folders, &strings(&p["ids"])?) {
                return Err(Error::Code("catalog_membership_changed"));
            }
        }
        "folder_membership" => {
            let projects = eligible(db, a)?;
            d.projects.insert(
                id.into(),
                projects
                    .get(id)
                    .ok_or(Error::Code("catalog_local_only_project"))?
                    .clone(),
            );
            if let Some(f) = p["folder_id"].as_str() {
                let fs = folder_heads(db, a)?;
                d.folders.insert(
                    f.into(),
                    fs.get(f)
                        .ok_or(Error::Code("catalog_dependency_missing"))?
                        .clone(),
                );
            }
        }
        "project_order" => {
            d.projects = eligible(db, a)?;
            if !keys_match(&d.projects, &strings(&p["ids"])?) {
                return Err(Error::Code("catalog_membership_changed"));
            }
            for p in d.projects.keys() {
                let hs = tips(db, a, "folder_membership", p)?;
                if hs.len() != 1 {
                    return Err(Error::Code("catalog_dependency_missing"));
                }
                d.memberships.insert(p.clone(), hs);
            }
        }
        _ => return Err(invalid()),
    };
    Ok(d)
}
pub(crate) fn begin(db: &mut Connection, s: &MetadataScope, now: &str) -> Result<Value> {
    scope(db, s)?;
    if !metadata::timestamp(now) {
        return Err(invalid());
    }
    match begin_inner(db, s, now) {
        Err(Error::Code(code))
            if matches!(
                code,
                "catalog_resource_limit"
                    | "unsupported_catalog_source"
                    | "invalid_catalog_frame"
                    | "catalog_project_unproven"
            ) =>
        {
            scope(db, s)?;
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let src = source(&tx)?;
            tx.execute(
                "INSERT INTO cloud_catalog_candidates VALUES(?1,?2,?3,?4,?5)",
                params![
                    metadata::new_event_id().map_err(|_| invalid())?,
                    s.account_id,
                    src.to_string(),
                    code,
                    now
                ],
            )?;
            tx.execute("INSERT INTO cloud_catalog_state VALUES(?1,'blocked',?2,?2,?3,?4) ON CONFLICT(account_id) DO UPDATE SET state='blocked',blocker=excluded.blocker",params![s.account_id,src.to_string(),code,now])?;
            tx.commit()?;
            authority(db, s)
        }
        result => result,
    }
}
fn begin_inner(db: &mut Connection, s: &MetadataScope, now: &str) -> Result<Value> {
    scope(db, s)?;
    if !metadata::timestamp(now) {
        return Err(invalid());
    }
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM cloud_catalog_state WHERE account_id=?1)",
        [&s.account_id],
        |r| r.get(0),
    )?;
    if exists {
        let retry:bool=tx.query_row("SELECT blocker IS NOT NULL AND NOT EXISTS(SELECT 1 FROM cloud_catalog_events WHERE account_id=?1) FROM cloud_catalog_state WHERE account_id=?1",[&s.account_id],|r|r.get(0))?;
        if !retry {
            tx.commit()?;
            return authority(db, s);
        }
        tx.execute(
            "DELETE FROM cloud_catalog_state WHERE account_id=?1",
            [&s.account_id],
        )?;
    }
    let src = source(&tx)?;
    tx.execute(
        "INSERT INTO cloud_catalog_candidates VALUES(?1,?2,?3,NULL,?4)",
        params![
            metadata::new_event_id().map_err(|_| invalid())?,
            s.account_id,
            src.to_string(),
            now
        ],
    )?;
    let snapshot = match portable(&tx, &s.account_id) {
        Ok(v) => v,
        Err(Error::Code(code)) => {
            tx.execute(
                "INSERT INTO cloud_catalog_state VALUES(?1,'blocked',?2,?2,?3,?4)",
                params![s.account_id, src.to_string(), code, now],
            )?;
            tx.commit()?;
            return authority(db, s);
        }
        Err(e) => return Err(e),
    };
    let projects: BTreeMap<String, ProjectProof> =
        serde_json::from_value(snapshot["projects"].clone()).map_err(|_| invalid())?;
    let mut fs = BTreeMap::new();
    for f in snapshot["folders"].as_array().ok_or_else(invalid)? {
        let id = f[0].as_str().ok_or_else(invalid)?;
        let e = make(
            s,
            "folder",
            id,
            json!({"name":f[1]}),
            vec![],
            1,
            1,
            Dependencies::default(),
            now,
            false,
        )?;
        fs.insert(id.to_string(), vec![e.header.event_id.clone()]);
        insert(&tx, &s.account_id, &e, "unsealed")?;
    }
    let order = snapshot["folders"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f[0].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    let e = make(
        s,
        "folder_order",
        "folder_order",
        json!({"ids":order}),
        vec![],
        1,
        1,
        Dependencies {
            folders: fs.clone(),
            ..Default::default()
        },
        now,
        false,
    )?;
    insert(&tx, &s.account_id, &e, "unsealed")?;
    let mut ms = BTreeMap::new();
    for (id, proof) in &projects {
        let f = snapshot["members"][id].as_str();
        let mut d = Dependencies::default();
        d.projects.insert(id.clone(), proof.clone());
        if let Some(f) = f {
            d.folders.insert(
                f.into(),
                fs.get(f)
                    .ok_or(Error::Code("unsupported_catalog_source"))?
                    .clone(),
            );
        }
        let e = make(
            s,
            "folder_membership",
            id,
            json!({"folder_id":f}),
            vec![],
            1,
            1,
            d,
            now,
            false,
        )?;
        ms.insert(id.clone(), vec![e.header.event_id.clone()]);
        insert(&tx, &s.account_id, &e, "unsealed")?;
    }
    let e = make(
        s,
        "project_order",
        "project_order",
        json!({"ids":snapshot["order"]}),
        vec![],
        1,
        1,
        Dependencies {
            projects,
            memberships: ms,
            ..Default::default()
        },
        now,
        false,
    )?;
    insert(&tx, &s.account_id, &e, "unsealed")?;
    tx.execute(
        "INSERT INTO cloud_catalog_state VALUES(?1,'captured',?2,?3,NULL,?4)",
        params![s.account_id, snapshot.to_string(), src.to_string(), now],
    )?;
    tx.commit()?;
    authority(db, s)
}
// Catalog history is retained: proof is derived around the immutable event,
// never by replacing its frame/dependencies. Stage's accepted walker stays separate.
const DEPENDENCY_UNIT_NODES: usize = 256;
const DEPENDENCY_TOTAL_NODES: usize = 65536;
struct CoverageNode {
    parents: Vec<String>,
    live: bool,
}
struct ProofBudget(usize);
impl ProofBudget {
    fn take(&mut self) -> Result<()> {
        if self.0 == 0 {
            return Err(Error::Code("catalog_resource_limit"));
        }
        self.0 -= 1;
        Ok(())
    }
}
fn ancestry_coverage(
    current: &[String],
    refs: &[String],
    budget: &mut ProofBudget,
    mut load: impl FnMut(&str) -> Result<CoverageNode>,
) -> Result<bool> {
    if current.is_empty() {
        return Err(Error::Code("catalog_dependency_missing"));
    }
    // No arbitrary choice among unresolved dependency branches.
    if current.len() != 1 {
        return Err(Error::Code("catalog_dependency_conflict"));
    }
    let mut cache = BTreeMap::new();
    for reference in refs {
        budget.take()?;
        let node = load(reference)?;
        if !node.live {
            return Err(Error::Code("catalog_dependency_missing"));
        }
        cache.insert(reference.clone(), node);
    }
    let mut stack = current.to_vec();
    let mut visited = std::collections::BTreeSet::new();
    let mut covered = std::collections::BTreeSet::new();
    let mut incompatible = false;
    while let Some(id) = stack.pop() {
        if !visited.insert(id.clone()) {
            continue;
        }
        if !cache.contains_key(&id) {
            if cache.len() >= DEPENDENCY_UNIT_NODES {
                return Err(Error::Code("catalog_resource_limit"));
            }
            budget.take()?;
            cache.insert(id.clone(), load(&id)?);
        }
        let node = &cache[&id];
        if current.contains(&id) {
            incompatible = !node.live;
        }
        if refs.contains(&id) {
            covered.insert(id);
            continue;
        }
        stack.extend(node.parents.iter().cloned());
    }
    if covered.len() != refs.len() {
        return Err(Error::Code("catalog_membership_changed"));
    }
    Ok(incompatible)
}
fn catalog_node(db: &Connection, a: &str, t: &str, entity: &str, id: &str) -> Result<CoverageNode> {
    let e = event(db, a, id)?.ok_or(Error::Code("catalog_dependency_missing"))?;
    let (seq,user): (Option<i64>,String) = db.query_row(
        "SELECT server_sequence,(SELECT canonical_user_id FROM cloud_account_bindings WHERE local_account_id=?1) FROM cloud_catalog_events WHERE account_id=?1 AND event_id=?2",
        params![a, id],
        |r| Ok((r.get(0)?,r.get(1)?)),
    )?;
    let proven = match seq {
        Some(seq) => ack_proven(db, a, seq)?,
        None => false,
    };
    if e.header.account_id != user
        || e.header.entity_type != t
        || e.header.entity_id != entity
        || !proven
    {
        return Err(Error::Code("catalog_dependency_missing"));
    }
    Ok(CoverageNode {
        parents: e.header.parent_event_ids,
        // A relation tombstone removes a folder assignment, not the bound project.
        live: t == "folder_membership" || e.deleted_at.is_none(),
    })
}
fn proof_ready(
    db: &Connection,
    a: &str,
    t: &str,
    id: &str,
    refs: &[String],
    budget: &mut ProofBudget,
) -> Result<bool> {
    ancestry_coverage(&tips(db, a, t, id)?, refs, budget, |h| {
        catalog_node(db, a, t, id, h)
    })
}
fn project_coverage(
    db: &Connection,
    a: &str,
    id: &str,
    p: &ProjectProof,
    budget: &mut ProofBudget,
) -> Result<()> {
    let valid:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_project_bindings b JOIN cloud_sync_project_bootstraps boot ON boot.project_id=b.project_id AND boot.account_id=b.account_id JOIN cloud_sync_metadata_events e ON e.account_id=b.account_id AND e.project_id=b.project_id AND e.bootstrap_id=boot.bootstrap_id JOIN cloud_sync_metadata_apply_ledger l ON l.account_id=e.account_id AND l.event_id=e.event_id WHERE b.account_id=?1 AND b.project_id=?2 AND boot.bootstrap_id=?3 AND e.event_id=?4 AND e.deleted_at IS NULL AND l.outcome='applied')",params![a,id,p.bootstrap_id,p.metadata_event_id],|r|r.get(0))?;
    if !valid {
        return Err(Error::Code("catalog_project_unproven"));
    }
    // Preserve C18.4.06: historic proof alone never establishes present authority.
    let view =
        metadata::authority_view(db, a, id).map_err(|_| Error::Code("catalog_project_unproven"))?;
    if view.state != "active" {
        return Err(Error::Code("catalog_project_unproven"));
    }
    let current = vec![view
        .head_event_id
        .ok_or(Error::Code("catalog_project_unproven"))?];
    let user: String = db.query_row(
        "SELECT canonical_user_id FROM cloud_account_bindings WHERE local_account_id=?1",
        [a],
        |r| r.get(0),
    )?;
    let incompatible = ancestry_coverage(&current, &[p.metadata_event_id.clone()], budget, |h| {
        let row: Option<(String,Vec<u8>,bool)> = db.query_row("SELECT e.parent_event_ids_json,l.canonical_payload,e.deleted_at IS NULL FROM cloud_sync_metadata_events e JOIN cloud_sync_metadata_apply_ledger l ON l.account_id=e.account_id AND l.event_id=e.event_id AND l.project_id=e.project_id AND l.outcome=e.state WHERE e.account_id=?1 AND e.project_id=?2 AND e.bootstrap_id=?3 AND e.event_id=?4",params![a,id,p.bootstrap_id,h],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        let (parents, raw, live) = row.ok_or(Error::Code("catalog_project_unproven"))?;
        let root: Value = serde_json::from_slice(&raw).map_err(|_| invalid())?;
        let parents: Vec<String> = serde_json::from_str(&parents).map_err(|_| invalid())?;
        let header = &root["header"];
        if header["account_id"] != user
            || header["project_id"] != id
            || header["entity_id"] != id
            || header["event_id"] != h
            || header["bootstrap_id"] != p.bootstrap_id
            || header["parent_event_ids"] != json!(parents)
        {
            return Err(Error::Code("catalog_project_unproven"));
        }
        Ok(CoverageNode { parents, live })
    })?;
    if incompatible {
        return Err(Error::Code("catalog_project_unproven"));
    }
    Ok(())
}
// Shared consumer boundary; catalog recovery semantics stay unchanged.
pub(crate) fn project_reference_ready(
    db: &Connection,
    account: &str,
    project: &str,
    bootstrap: &str,
    reference: &str,
) -> Result<()> {
    project_coverage(
        db,
        account,
        project,
        &ProjectProof {
            bootstrap_id: bootstrap.into(),
            metadata_event_id: reference.into(),
        },
        &mut ProofBudget(DEPENDENCY_TOTAL_NODES),
    )
}
// true means a known incompatible live-set/destructive evolution: retain a
// complete conflict, never use that payload as an exact current projection.
fn dependencies_ready(db: &Connection, a: &str, e: &Event) -> Result<bool> {
    let d = &e.dependencies;
    let mut budget = ProofBudget(DEPENDENCY_TOTAL_NODES);
    let mut incompatible = false;
    for (id, refs) in &d.folders {
        incompatible |= proof_ready(db, a, "folder", id, refs, &mut budget)?;
    }
    for (id, p) in &d.projects {
        project_coverage(db, a, id, p, &mut budget)?;
    }
    for (id, refs) in &d.memberships {
        incompatible |= proof_ready(db, a, "folder_membership", id, refs, &mut budget)?;
    }
    if e.header.entity_type == "folder_order" {
        let live = folder_heads(db, a)?;
        for (id, refs) in &live {
            proof_ready(db, a, "folder", id, refs, &mut budget)?;
        }
        incompatible |= live.keys().ne(d.folders.keys());
    }
    if e.header.entity_type == "project_order" {
        let projects = eligible(db, a)?;
        // New connected projects need complete authenticated metadata/relation
        // history too; local-only IDs can never become conflict proof through this.
        for (id, p) in &projects {
            project_coverage(db, a, id, p, &mut budget)?;
            let refs = tips(db, a, "folder_membership", id)?;
            if refs.is_empty() {
                return Err(Error::Code("catalog_dependency_missing"));
            }
            proof_ready(db, a, "folder_membership", id, &refs, &mut budget)?;
        }
        incompatible |= projects.keys().ne(d.projects.keys());
    }
    Ok(incompatible)
}
pub(crate) fn pending(db: &Connection, s: &MetadataScope, sealed: bool) -> Result<Vec<Value>> {
    let tx = rusqlite::Transaction::new_unchecked(db, TransactionBehavior::Immediate)?;
    scope(&tx, s)?;
    let mut q=tx.prepare("SELECT canonical_frame,nonce,ciphertext FROM cloud_catalog_events WHERE account_id=?1 AND state=?2 ORDER BY rowid")?;
    let rows = q
        .query_map(
            params![s.account_id, if sealed { "sealed" } else { "unsealed" }],
            |r| {
                Ok((
                    r.get::<_, Vec<u8>>(0)?,
                    r.get::<_, Option<Vec<u8>>>(1)?,
                    r.get::<_, Option<Vec<u8>>>(2)?,
                ))
            },
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(q);
    let mut out = vec![];
    for (raw, n, c) in rows {
        let e = unframe(&raw)?;
        if e.header.device_id != s.device_id {
            return Err(Error::Code("account_scope_rejected"));
        }
        let mut blocker = match dependencies_ready(&tx, &s.account_id, &e) {
            Ok(_) => None,
            Err(Error::Code(code)) => Some(code),
            Err(error) => return Err(error),
        };
        for id in &e.header.parent_event_ids {
            let proven:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM cloud_catalog_apply_ledger WHERE account_id=?1 AND event_id=?2)",params![s.account_id,id],|r|r.get(0))?;
            if !proven {
                blocker = Some("catalog_parent_unknown");
            }
        }
        tx.execute(
            "UPDATE cloud_catalog_events SET blocker=?3 WHERE account_id=?1 AND event_id=?2",
            params![s.account_id, e.header.event_id, blocker],
        )?;
        if blocker.is_none() {
            out.push(json!({"event":e,"nonce":n,"ciphertext":c}));
            if out.len() == 8 {
                break;
            }
        }
    }
    refresh(&tx, &s.account_id)?;
    tx.commit()?;
    Ok(out)
}
pub(crate) fn seal(
    db: &mut Connection,
    s: &MetadataScope,
    id: &str,
    raw: &[u8],
    nonce: &[u8],
    cipher: &[u8],
) -> Result<()> {
    scope(db, s)?;
    let e = unframe(raw)?;
    if e.header.event_id != id
        || e.header.account_id != s.canonical_user_id
        || e.header.device_id != s.device_id
        || nonce.len() != 24
        || cipher.len() != raw.len() + 16
    {
        return Err(invalid());
    }
    let compression_view = crate::frame_compression::canonical_view(raw).map_err(Error::Code)?;
    let raw = compression_view.as_ref();
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    dependencies_ready(&tx, &s.account_id, &e)?;
    let old:Option<(Vec<u8>,String,Option<Vec<u8>>,Option<Vec<u8>>)>=tx.query_row("SELECT canonical_frame,state,nonce,ciphertext FROM cloud_catalog_events WHERE account_id=?1 AND event_id=?2",params![s.account_id,id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
    let Some((stored, state, n, c)) = old else {
        return Err(invalid());
    };
    if stored != raw {
        return Err(invalid());
    }
    if state != "unsealed" {
        if n.as_deref() != Some(nonce) || c.as_deref() != Some(cipher) {
            return Err(invalid());
        }
    } else {
        tx.execute("UPDATE cloud_catalog_events SET state='sealed',nonce=?3,ciphertext=?4 WHERE account_id=?1 AND event_id=?2",params![s.account_id,id,nonce,cipher])?;
    }
    refresh(&tx, &s.account_id)?;
    tx.commit()?;
    Ok(())
}
pub(crate) fn receipt(db: &mut Connection, s: &MetadataScope, id: &str, seq: i64) -> Result<()> {
    scope(db, s)?;
    if !(1..=9007199254740991).contains(&seq) {
        return Err(invalid());
    }
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let old:Option<(String,Option<i64>)>=tx.query_row("SELECT state,receipt_sequence FROM cloud_catalog_events WHERE account_id=?1 AND event_id=?2 AND nonce IS NOT NULL",params![s.account_id,id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    let Some((state, prior)) = old else {
        return Err(invalid());
    };
    if prior.is_some_and(|n| n != seq) {
        return Err(invalid());
    }
    if !matches!(
        state.as_str(),
        "sealed" | "accepted" | "applied" | "conflict_preserved" | "blocked"
    ) {
        return Err(invalid());
    }
    tx.execute("UPDATE cloud_catalog_events SET receipt_sequence=?3,state=CASE WHEN state='sealed' THEN 'accepted' ELSE state END WHERE account_id=?1 AND event_id=?2",params![s.account_id,id,seq])?;
    refresh(&tx, &s.account_id)?;
    tx.commit()?;
    Ok(())
}
fn materialize(db: &Connection, a: &str, e: &Event) -> Result<()> {
    let h = &e.header;
    match h.entity_type.as_str() {
        "folder" => {
            if e.payload.is_null() { // Tombstone retains the row for local-only membership and history.
            } else {
                db.execute("INSERT INTO project_folders(id,name,position,payload_json) VALUES(?1,?2,(SELECT COALESCE(MAX(position),-1)+1 FROM project_folders),?3) ON CONFLICT(id) DO UPDATE SET name=excluded.name,payload_json=json_set(project_folders.payload_json,'$.name',excluded.name)",params![h.entity_id,e.payload["name"].as_str(),json!({"id":h.entity_id,"name":e.payload["name"]}).to_string()])?;
            }
        }
        "folder_membership" => {
            db.execute(
                "DELETE FROM project_folder_members WHERE project_id=?1",
                [&h.entity_id],
            )?;
            if let Some(f) = e.payload["folder_id"].as_str() {
                db.execute(
                    "INSERT INTO project_folder_members VALUES(?1,?2)",
                    params![h.entity_id, f],
                )?;
            }
            db.execute("UPDATE projects SET payload_json=json_set(payload_json,'$.folder_id',?2) WHERE id=?1",params![h.entity_id,e.payload["folder_id"].as_str()])?;
        }
        "project_order" | "folder_order" => {
            let ids = strings(&e.payload["ids"])?;
            let sql = if h.entity_type == "project_order" {
                "SELECT project_id,position FROM project_order ORDER BY position"
            } else {
                "SELECT id,position FROM project_folders ORDER BY position"
            };
            let mut q = db.prepare(sql)?;
            let all = q
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            drop(q);
            let slots = all
                .iter()
                .filter(|(id, _)| ids.contains(id))
                .map(|(_, pos)| *pos)
                .collect::<Vec<_>>();
            if slots.len() != ids.len() {
                return Err(Error::Code("catalog_dependency_missing"));
            }
            let table = if h.entity_type == "project_order" {
                "project_order"
            } else {
                "project_folders"
            };
            let column = if h.entity_type == "project_order" {
                "project_id"
            } else {
                "id"
            };
            let offset = all.len() as i64 + LIMIT as i64;
            for id in &ids {
                db.execute(
                    &format!("UPDATE {table} SET position=position+?2 WHERE {column}=?1"),
                    params![id, offset],
                )?;
            }
            for (id, pos) in ids.iter().zip(slots) {
                db.execute(
                    &format!("UPDATE {table} SET position=?2 WHERE {column}=?1"),
                    params![id, pos],
                )?;
            }
        }
        _ => return Err(invalid()),
    };
    let _ = a;
    Ok(())
}
fn refresh(db: &Connection, a: &str) -> Result<()> {
    let blocked:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM cloud_catalog_events WHERE account_id=?1 AND (state IN ('blocked','orphan') OR blocker IS NOT NULL))",[a],|r|r.get(0))?;
    let pending:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM cloud_catalog_events WHERE account_id=?1 AND state IN ('unsealed','sealed'))",[a],|r|r.get(0))?;
    let accepted:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM cloud_catalog_events WHERE account_id=?1 AND state='accepted')",[a],|r|r.get(0))?;
    let conflict:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM cloud_catalog_tips WHERE account_id=?1 GROUP BY entity_type,entity_id HAVING count(*)>1) OR EXISTS(SELECT 1 FROM cloud_catalog_local_conflicts WHERE account_id=?1)",[a],|r|r.get(0))?;
    db.execute("UPDATE cloud_catalog_state SET state=?2 WHERE account_id=?1 AND blocker IS NULL",params![a,if blocked{"blocked"}else if conflict{"conflict"}else if pending{"publication_pending"}else if accepted{"self_echo_pending"}else{let complete:bool=db.query_row("SELECT count(*)=2 FROM cloud_catalog_projection WHERE account_id=?1 AND entity_type IN ('folder_order','project_order')",[a],|r|r.get(0))?;if complete{"active"}else{"captured"}}])?;
    Ok(())
}
fn preserve_block(db: &Connection, a: &str, id: &str, code: &str) -> Result<String> {
    db.execute("UPDATE cloud_catalog_events SET state='orphan',blocker=?3 WHERE account_id=?1 AND event_id=?2",params![a,id,code])?;
    refresh(db, a)?;
    Ok(code.into())
}
pub(crate) fn apply(
    db: &mut Connection,
    s: &MetadataScope,
    id: &str,
    raw: &[u8],
    nonce: &[u8],
    cipher: &[u8],
) -> Result<String> {
    let compression_view = crate::frame_compression::canonical_view(raw).map_err(Error::Code)?;
    let raw = compression_view.as_ref();
    scope(db, s)?;
    let e = unframe(raw)?;
    let h = &e.header;
    let a = &s.account_id;
    if h.event_id != id || h.account_id != s.canonical_user_id {
        return Err(Error::Code("account_scope_rejected"));
    }
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let seq:Option<i64>=tx.query_row("SELECT server_sequence FROM cloud_sync_account_inbox WHERE account_id=?1 AND event_id=?2 AND canonical_user_id=?3 AND scope='account' AND device_id=?4 AND entity_type=?5 AND entity_id=?6 AND sync_revision=?7 AND operation=?8 AND updated_at=?9 AND deleted_at IS ?10 AND nonce=?11 AND ciphertext=?12 AND crypto_version=2 AND aad_version=2",params![a,id,h.account_id,h.device_id,h.entity_type,h.entity_id,h.revision,if e.deleted_at.is_some(){"delete"}else{"upsert"},h.updated_at,e.deleted_at,nonce,cipher],|r|r.get(0)).optional()?;
    let seq = seq.ok_or(Error::Code("account_scope_rejected"))?;
    let ledger:Option<(Vec<u8>,Vec<u8>,Vec<u8>,String)>=tx.query_row("SELECT canonical_frame,nonce,ciphertext,outcome FROM cloud_catalog_apply_ledger WHERE account_id=?1 AND event_id=?2",params![a,id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
    if let Some((r, n, c, outcome)) = ledger {
        if r != raw || n != nonce || c != cipher {
            return Err(invalid());
        }
        return Ok(outcome);
    }
    let old:Option<(Vec<u8>,Option<Vec<u8>>,Option<Vec<u8>>,Option<i64>)>=tx.query_row("SELECT canonical_frame,nonce,ciphertext,receipt_sequence FROM cloud_catalog_events WHERE account_id=?1 AND event_id=?2",params![a,id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
    if let Some((r, n, c, receipt)) = old {
        if r != raw
            || n.as_ref().is_some_and(|n| n != nonce)
            || c.as_ref().is_some_and(|c| c != cipher)
            || receipt.is_some_and(|n| n != seq)
        {
            return Err(invalid());
        }
    } else {
        let (count,size):(i64,i64)=tx.query_row("SELECT count(*),COALESCE(sum(length(canonical_frame)+length(ciphertext)),0) FROM cloud_catalog_events WHERE account_id=?1",[a],|r|Ok((r.get(0)?,r.get(1)?)))?;
        if count >= 131072 || size + raw.len() as i64 + cipher.len() as i64 > 268435456 {
            return Err(Error::Code("catalog_resource_limit"));
        }
        insert(&tx, a, &e, "orphan")?;
    }
    tx.execute("UPDATE cloud_catalog_events SET nonce=?3,ciphertext=?4,server_sequence=?5 WHERE account_id=?1 AND event_id=?2",params![a,id,nonce,cipher,seq])?;
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM cloud_catalog_state WHERE account_id=?1)",
        [a],
        |r| r.get(0),
    )?;
    if !exists {
        let src = source(&tx)?;
        tx.execute(
            "INSERT INTO cloud_catalog_state VALUES(?1,'captured','{}',?2,NULL,?3)",
            params![a, src.to_string(), h.updated_at],
        )?;
    }
    // Parent lineage and revision/generation are independently recomputed.
    let mut revision = 0;
    let mut generation = 0;
    for p in &h.parent_event_ids {
        let parent = event(&tx, a, p)?;
        let proven:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM cloud_catalog_apply_ledger WHERE account_id=?1 AND event_id=?2)",params![a,p],|r|r.get(0))?;
        if parent.is_none() || !proven {
            let out = preserve_block(&tx, a, id, "catalog_parent_unknown")?;
            tx.commit()?;
            return Ok(out);
        }
        let p = parent.unwrap();
        if p.header.entity_type != h.entity_type || p.header.entity_id != h.entity_id {
            return Err(invalid());
        }
        revision = revision.max(p.header.revision);
        generation = generation.max(p.header.generation);
    }
    if h.revision != revision + 1 || h.generation != generation + 1 {
        return Err(invalid());
    }
    let dependency_conflict = match dependencies_ready(&tx, a, &e) {
        Ok(incompatible) => incompatible,
        Err(Error::Code(code)) => {
            let out = preserve_block(&tx, a, id, code)?;
            tx.commit()?;
            return Ok(out);
        }
        Err(e) => return Err(e),
    };
    let before = tips(&tx, a, &h.entity_type, &h.entity_id)?;
    for parent in &h.parent_event_ids {
        tx.execute(
            "DELETE FROM cloud_catalog_tips WHERE account_id=?1 AND event_id=?2",
            params![a, parent],
        )?;
    }
    tx.execute(
        "INSERT OR IGNORE INTO cloud_catalog_tips VALUES(?1,?2,?3,?4)",
        params![a, h.entity_type, h.entity_id, id],
    )?;
    let current = tips(&tx, a, &h.entity_type, &h.entity_id)?;
    if current.len() > 64 {
        let out = preserve_block(&tx, a, id, "catalog_resource_limit")?;
        tx.commit()?;
        return Ok(out);
    }
    if h.entity_type == "folder" && e.payload.is_null() {
        let referenced:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM project_folder_members WHERE folder_id=?1) OR EXISTS(SELECT 1 FROM cloud_catalog_tips t JOIN cloud_catalog_events ev ON ev.account_id=t.account_id AND ev.event_id=t.event_id WHERE t.account_id=?2 AND t.entity_type='folder_membership' AND json_extract(CAST(substr(ev.canonical_frame,21) AS TEXT),'$.payload.folder_id')=?1)",params![h.entity_id,a],|r|r.get(0))?;
        if referenced {
            let out = preserve_block(&tx, a, id, "catalog_folder_has_members")?;
            tx.commit()?;
            return Ok(out);
        }
    }
    let visible = local(&tx, a, &h.entity_type, &h.entity_id)?;
    let previous:Option<String>=tx.query_row("SELECT payload_json FROM cloud_catalog_projection WHERE account_id=?1 AND entity_type=?2 AND entity_id=?3",params![a,h.entity_type,h.entity_id],|r|r.get(0)).optional()?;
    let decision:Option<String>=tx.query_row("SELECT expected_local_json FROM cloud_catalog_decisions WHERE account_id=?1 AND event_id=?2",params![a,id],|r|r.get(0)).optional()?;
    let decided = decision
        .as_ref()
        .is_some_and(|raw| serde_json::from_str::<Value>(raw).ok().as_ref() == Some(&visible))
        && before == h.parent_event_ids;
    let empty_local = match h.entity_type.as_str() {
        "folder" => visible.is_null(),
        "folder_membership" => visible["folder_id"].is_null(),
        _ => visible["ids"].as_array().is_some_and(Vec::is_empty),
    };
    let compatible = decided
        || visible == e.payload
        || previous
            .as_ref()
            .is_some_and(|p| serde_json::from_str::<Value>(p).ok().as_ref() == Some(&visible))
        || previous.is_none() && empty_local;
    let local_conflict:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM cloud_catalog_local_conflicts WHERE account_id=?1 AND entity_type=?2 AND entity_id=?3)",params![a,h.entity_type,h.entity_id],|r|r.get(0))?;
    let conflict =
        dependency_conflict || current.len() != 1 || !compatible || local_conflict && !decided;
    if !compatible || dependency_conflict {
        tx.execute(
            "INSERT OR IGNORE INTO cloud_catalog_local_conflicts VALUES(?1,?2,?3,?4)",
            params![a, h.entity_type, h.entity_id, visible.to_string()],
        )?;
    }
    if decided && current.len() == 1 && !dependency_conflict {
        tx.execute("DELETE FROM cloud_catalog_local_conflicts WHERE account_id=?1 AND entity_type=?2 AND entity_id=?3",params![a,h.entity_type,h.entity_id])?;
    }
    let outcome = if conflict {
        "conflict_preserved"
    } else {
        "applied"
    };
    if current.len() == 1 && !dependency_conflict {
        tx.execute("INSERT INTO cloud_catalog_projection VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(account_id,entity_type,entity_id) DO UPDATE SET head_event_id=excluded.head_event_id,payload_json=excluded.payload_json,deleted=excluded.deleted",params![a,h.entity_type,h.entity_id,id,e.payload.to_string(),e.deleted_at.is_some()])?;
        if !conflict {
            materialize(&tx, a, &e)?;
        }
    }
    tx.execute(
        "UPDATE cloud_catalog_events SET state=?3,blocker=NULL WHERE account_id=?1 AND event_id=?2",
        params![a, id, outcome],
    )?;
    tx.execute(
        "INSERT INTO cloud_catalog_apply_ledger VALUES(?1,?2,?3,?4,?5,?6,?7)",
        params![a, id, seq, raw, nonce, cipher, outcome],
    )?;
    tx.execute("UPDATE cloud_sync_account_inbox SET state='received',error_code=NULL WHERE account_id=?1 AND event_id=?2",params![a,id])?;
    tx.execute(
        "DELETE FROM cloud_catalog_inbox_blockers WHERE account_id=?1 AND event_id=?2",
        params![a, id],
    )?;
    refresh(&tx, a)?;
    if conflict {
        tx.execute(
            "UPDATE cloud_catalog_state SET state='conflict' WHERE account_id=?1",
            [a],
        )?;
    }
    tx.commit()?;
    Ok(outcome.into())
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
    s: &MetadataScope,
    d: &Decision,
    now: &str,
) -> Result<String> {
    scope(db, s)?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if !TYPES.contains(&d.entity_type.as_str())
        || !heads(&d.expected_tips, true)
        || tips(&tx, &s.account_id, &d.entity_type, &d.entity_id)? != d.expected_tips
        || local(&tx, &s.account_id, &d.entity_type, &d.entity_id)? != d.expected_local
    {
        return Err(Error::Code("stale_catalog_resolution"));
    }
    let payload = if let Some(id) = &d.selected_event_id {
        if !d.expected_tips.contains(id) {
            return Err(Error::Code("stale_catalog_resolution"));
        }
        event(&tx, &s.account_id, id)?.ok_or_else(invalid)?.payload
    } else {
        d.proposed.clone()
    };
    let mut revision = 0;
    let mut generation = 0;
    for id in &d.expected_tips {
        let e = event(&tx, &s.account_id, id)?.ok_or_else(invalid)?;
        revision = revision.max(e.header.revision);
        generation = generation.max(e.header.generation);
    }
    let dep = dependencies(&tx, &s.account_id, &d.entity_type, &d.entity_id, &payload)?;
    let e = make(
        s,
        &d.entity_type,
        &d.entity_id,
        payload,
        d.expected_tips.clone(),
        revision + 1,
        generation + 1,
        dep,
        now,
        true,
    )?;
    // Repeating the same explicit decision uses the durable identity, including after seal.
    let mut q=tx.prepare("SELECT canonical_frame FROM cloud_catalog_events WHERE account_id=?1 AND entity_type=?2 AND entity_id=?3 AND state IN ('unsealed','sealed','accepted') ORDER BY rowid DESC")?;
    let rows = q
        .query_map(params![s.account_id, d.entity_type, d.entity_id], |r| {
            r.get::<_, Vec<u8>>(0)
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(q);
    for raw in rows {
        let prior = unframe(&raw)?;
        if prior.payload == e.payload && prior.header.parent_event_ids == e.header.parent_event_ids
        {
            return Ok(prior.header.event_id);
        }
    }
    insert(&tx, &s.account_id, &e, "unsealed")?;
    tx.execute(
        "INSERT INTO cloud_catalog_decisions VALUES(?1,?2,?3,?4)",
        params![
            s.account_id,
            e.header.event_id,
            json!(d.expected_tips).to_string(),
            d.expected_local.to_string()
        ],
    )?;
    refresh(&tx, &s.account_id)?;
    tx.commit()?;
    Ok(e.header.event_id)
}
pub(crate) fn authority(db: &Connection, s: &MetadataScope) -> Result<Value> {
    scope(db, s)?;
    let state: Option<(String, Option<String>)> = db
        .query_row(
            "SELECT state,blocker FROM cloud_catalog_state WHERE account_id=?1",
            [&s.account_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let mut units=rows(db,"SELECT entity_type,entity_id FROM cloud_catalog_events WHERE account_id=?1 GROUP BY entity_type,entity_id ORDER BY entity_type,entity_id",&s.account_id)?;
    for (id, _) in rows(
        db,
        "SELECT id,name FROM project_folders ORDER BY position,id",
        "",
    )? {
        if !units.contains(&("folder".into(), id.clone())) {
            units.push(("folder".into(), id));
        }
    }
    let mut entities = vec![];
    let mut conflict = false;
    for (t, id) in units {
        let hs = tips(db, &s.account_id, &t, &id)?;
        let branches = hs
            .iter()
            .map(|id| event(db, &s.account_id, id)?.ok_or_else(invalid))
            .collect::<Result<Vec<_>>>()?;
        let visible = local(db, &s.account_id, &t, &id)?;
        let differs = branches.len() == 1 && branches[0].payload != visible;
        let preserved_local:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM cloud_catalog_local_conflicts WHERE account_id=?1 AND entity_type=?2 AND entity_id=?3)",params![s.account_id,t,id],|r|r.get(0))?;
        let c = hs.len() > 1 || differs || preserved_local;
        conflict |= c;
        entities.push(json!({"entity_type":t,"entity_id":id,"tips":hs,"branches":branches,"local":visible,"conflict":c}));
    }
    let mut blockers=rows(db,"SELECT DISTINCT blocker,blocker FROM cloud_catalog_events WHERE account_id=?1 AND blocker IS NOT NULL",&s.account_id)?.into_iter().map(|(b,_)|b).collect::<Vec<_>>();
    if let Some((_, Some(b))) = &state {
        blockers.push(b.clone());
    }
    blockers.extend(
        rows(
            db,
            "SELECT DISTINCT code,code FROM cloud_catalog_inbox_blockers WHERE account_id=?1",
            &s.account_id,
        )?
        .into_iter()
        .map(|(code, _)| code),
    );
    blockers.sort();
    blockers.dedup();
    let names = rows(db, "SELECT id,name FROM projects ORDER BY id", "")?
        .into_iter()
        .collect::<BTreeMap<_, _>>();
    Ok(
        json!({"state":if !blockers.is_empty(){"blocked"}else if conflict{"conflict"}else{state.as_ref().map(|(s,_)|s.as_str()).unwrap_or("catalog_local")},"entities":entities,"blockers":blockers,"project_names":names}),
    )
}
fn owning_scope(db: &Connection) -> Result<Option<MetadataScope>> {
    let mut q=db.prepare("SELECT c.account_id,b.canonical_user_id,s.device_id FROM cloud_catalog_state c JOIN cloud_account_bindings b ON b.local_account_id=c.account_id JOIN cloud_sync_state s ON s.account_id=c.account_id")?;
    let rows = q
        .query_map([], |r| {
            Ok(MetadataScope {
                account_id: r.get(0)?,
                canonical_user_id: r.get(1)?,
                device_id: r.get(2)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if rows.len() > 1 {
        return Err(Error::Code("account_scope_rejected"));
    }
    Ok(rows.into_iter().next())
}
/// Called by ordinary native writers before visible mutation. No state => legacy
/// local semantics. Captured snapshots stay frozen; pre-authority edits stay local.
pub(crate) fn normal(
    db: &mut Connection,
    t: &str,
    id: &str,
    payload: Value,
    now: &str,
) -> Result<bool> {
    let Some(s) = owning_scope(db)? else {
        return Ok(false);
    };
    scope(db, &s)?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if t == "folder_membership" {
        let bound:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_project_bindings WHERE account_id=?1 AND project_id=?2)",params![s.account_id,id],|r|r.get(0))?;
        if !bound {
            return Ok(false);
        }
    }
    let existing = tips(&tx, &s.account_id, t, id)?;
    let established:bool=tx.query_row("SELECT count(*)=2 FROM cloud_catalog_projection WHERE account_id=?1 AND entity_type IN ('folder_order','project_order')",[&s.account_id],|r|r.get(0))?;
    if !established && existing.is_empty() {
        let authorized:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM cloud_catalog_events WHERE account_id=?1 AND entity_type=?2 AND entity_id=?3 AND state IN ('sealed','accepted'))",params![s.account_id,t,id],|r|r.get(0))?;
        if !authorized {
            return Ok(false);
        }
    }
    let local_conflict:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM cloud_catalog_local_conflicts WHERE account_id=?1 AND entity_type=?2 AND entity_id=?3)",params![s.account_id,t,id],|r|r.get(0))?;
    if existing.len() > 1 || local_conflict {
        return Err(Error::Code("catalog_conflict"));
    }
    let mut parents = existing.clone();
    let mut revision = 0;
    let mut generation = 0;
    if let Some(h) = existing.first() {
        let e = event(&tx, &s.account_id, h)?.ok_or_else(invalid)?;
        revision = e.header.revision;
        generation = e.header.generation;
    }
    let pending:Option<Vec<u8>>=tx.query_row("SELECT canonical_frame FROM cloud_catalog_events WHERE account_id=?1 AND entity_type=?2 AND entity_id=?3 AND state IN ('unsealed','sealed','accepted') ORDER BY rowid DESC LIMIT 1",params![s.account_id,t,id],|r|r.get(0)).optional()?;
    if let Some(raw) = pending {
        let e = unframe(&raw)?;
        if e.payload == payload {
            return Ok(true);
        }
        if e.header.operation == "resolution" {
            return Err(Error::Code("catalog_conflict"));
        }
        parents = vec![e.header.event_id];
        revision = e.header.revision;
        generation = e.header.generation;
    }
    if !existing.is_empty() {
        let e = event(&tx, &s.account_id, &existing[0])?.ok_or_else(invalid)?;
        if e.payload == payload && parents == existing {
            return Ok(true);
        }
    }
    let dep = dependencies(&tx, &s.account_id, t, id, &payload)?;
    let e = make(
        &s,
        t,
        id,
        payload,
        parents,
        revision + 1,
        generation + 1,
        dep,
        now,
        false,
    )?;
    insert(&tx, &s.account_id, &e, "unsealed")?;
    refresh(&tx, &s.account_id)?;
    tx.commit()?;
    Ok(true)
}
/// Recoverably derives connection intents from durable bindings. No startup
/// migration: only an already established account authority reaches this path.
pub(crate) fn reconcile_connections(
    db: &mut Connection,
    s: &MetadataScope,
    now: &str,
) -> Result<()> {
    scope(db, s)?;
    let active:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM cloud_catalog_state WHERE account_id=?1 AND state IN ('active','publication_pending','self_echo_pending'))",[&s.account_id],|r|r.get(0))?;
    if !active {
        return Ok(());
    }
    let projects = eligible(db, &s.account_id)?;
    let live = folder_heads(db, &s.account_id)?;
    let folder_order = local(db, &s.account_id, "folder_order", "folder_order")?;
    if strings(&folder_order["ids"])?
        .iter()
        .all(|id| live.contains_key(id))
    {
        normal(db, "folder_order", "folder_order", folder_order, now)?;
    }
    for id in projects.keys() {
        let hs = tips(db, &s.account_id, "folder_membership", id)?;
        let pending:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM cloud_catalog_events WHERE account_id=?1 AND entity_type='folder_membership' AND entity_id=?2)",params![s.account_id,id],|r|r.get(0))?;
        if hs.is_empty() && !pending {
            let p = local(db, &s.account_id, "folder_membership", id)?;
            normal(db, "folder_membership", id, p, now)?;
        }
    }
    // Membership self echoes gate creation of the complete project permutation.
    if projects
        .keys()
        .any(|id| tips(db, &s.account_id, "folder_membership", id).map_or(true, |h| h.len() != 1))
    {
        return Ok(());
    }
    let p = local(db, &s.account_id, "project_order", "project_order")?;
    normal(db, "project_order", "project_order", p, now)?;
    Ok(())
}
pub(crate) fn ack_proven(db: &Connection, a: &str, seq: i64) -> Result<bool> {
    Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_account_inbox i JOIN cloud_catalog_events e ON e.account_id=i.account_id AND e.event_id=i.event_id JOIN cloud_catalog_apply_ledger l ON l.account_id=e.account_id AND l.event_id=e.event_id WHERE i.account_id=?1 AND i.server_sequence=?2 AND e.server_sequence=i.server_sequence AND l.server_sequence=i.server_sequence AND e.entity_type=i.entity_type AND e.entity_id=i.entity_id AND e.state=l.outcome AND e.canonical_frame=l.canonical_frame AND i.nonce=l.nonce AND i.ciphertext=l.ciphertext AND e.nonce=l.nonce AND e.ciphertext=l.ciphertext AND i.crypto_version=2 AND i.aad_version=2 AND i.error_code IS NULL)",params![a,seq],|r|r.get(0))?)
}
#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    const USER: &str = "123e4567-e89b-42d3-a456-426614174000";
    const DEVICE: &str = "123e4567-e89b-42d3-a456-426614174001";
    const NOW: &str = "2026-10-02T00:00:00.000000Z";
    fn scope() -> MetadataScope {
        MetadataScope {
            account_id: "a".into(),
            canonical_user_id: USER.into(),
            device_id: DEVICE.into(),
        }
    }
    fn id(n: u32) -> String {
        format!("123e4567-e89b-42d3-a456-{n:012}")
    }
    pub(crate) fn setup() -> (Connection, std::path::PathBuf) {
        let path =
            std::env::temp_dir().join(format!("catalog-{}.db", metadata::new_event_id().unwrap()));
        let db = crate::sqlite::open_database(&path).unwrap();
        db.execute("INSERT INTO cloud_sync_state(account_id,device_id,pull_cursor,ack_cursor,created_at,updated_at) VALUES('a',?1,0,0,?2,?2)",params![DEVICE,NOW]).unwrap();
        db.execute(
            "INSERT INTO cloud_account_bindings VALUES('a',?1,?2,?2)",
            params![USER, NOW],
        )
        .unwrap();
        (db, path)
    }
    pub(crate) fn seed(db: &mut Connection) {
        for (i, p) in ["L1", "C1", "C2"].iter().enumerate() {
            let meta = json!({"name":p,"goal":null,"infinite":true,"unit":"symbols","status":"active","deadline":null,"personal_goal":0,"auto_freeze":true,"streak_enabled":true,"work_method":"manual","stages_enabled":false,"combine_stage_mindmaps":false});
            db.execute("INSERT INTO projects(id,name,infinite,unit,status,payload_json) VALUES(?1,?1,1,'symbols','active',?2)",params![p,meta.to_string()]).unwrap();
            db.execute(
                "INSERT INTO project_order VALUES(?1,?2)",
                params![p, i as i64],
            )
            .unwrap();
            if i == 0 {
                continue;
            }
            db.execute(
                "INSERT INTO cloud_sync_project_bindings VALUES(?1,'a',?2,?2)",
                params![p, NOW],
            )
            .unwrap();
            let boot = id(20 + i as u32);
            db.execute("INSERT INTO cloud_sync_project_bootstraps(project_id,account_id,device_id,bootstrap_id,mode,phase,created_at,updated_at) VALUES(?1,'a',?2,?3,'upload_existing','prepared',?4,?4)",params![p,DEVICE,boot,NOW]).unwrap();
            let ev = id(i as u32);
            let e = json!({"version":1,"header":{"account_id":USER,"project_id":p,"entity_id":p,"device_id":DEVICE,"bootstrap_id":boot,"event_id":ev,"revision":1,"generation":1,"operation":"create","parent_event_ids":[],"updated_at":NOW},"metadata":meta,"deleted_at":null});
            db.execute("INSERT INTO cloud_sync_inbox(account_id,event_id,server_sequence,device_id,project_id,entity_id,entity_type,operation,sync_revision,updated_at,state,received_at) VALUES('a',?1,?2,?3,?4,?4,'project_metadata','upsert',1,?5,'received',?5)",params![ev,i as i64,DEVICE,p,NOW]).unwrap();
            db.execute("INSERT INTO cloud_sync_event_objects VALUES('a',?1,1,1,zeroblob(24),zeroblob(16),?2)",params![ev,NOW]).unwrap();
            metadata::preserve_authenticated_event(
                db,
                "a",
                p,
                &serde_json::to_vec(&e).unwrap(),
                NOW,
            )
            .unwrap();
            assert_eq!(
                metadata::authority_view(db, "a", p).unwrap().state,
                "active"
            );
        }
        db.execute("UPDATE cloud_sync_state SET pull_cursor=2", [])
            .unwrap();
        for (i, (f, n)) in [("F1", "Work"), ("F2", "Archive")].iter().enumerate() {
            db.execute(
                "INSERT INTO project_folders VALUES(?1,?2,?3,?4)",
                params![f, n, i as i64, json!({"id":f,"name":n}).to_string()],
            )
            .unwrap();
        }
        for (p, f) in [("C1", "F1"), ("L1", "F1"), ("C2", "F2")] {
            db.execute(
                "INSERT INTO project_folder_members VALUES(?1,?2)",
                params![p, f],
            )
            .unwrap();
        }
    }
    fn persist_arrival(db: &mut Connection, e: &Event, seq: i64) {
        let raw = frame(e).unwrap();
        let cipher = vec![17u8; raw.len() + 16];
        db.execute("INSERT OR IGNORE INTO cloud_sync_account_inbox(account_id,event_id,canonical_user_id,scope,server_sequence,device_id,entity_id,entity_type,operation,sync_revision,updated_at,deleted_at,crypto_version,aad_version,nonce,ciphertext,received_at) VALUES('a',?1,?2,'account',?3,?4,?5,?6,?7,?8,?9,?10,2,2,?11,?12,?9)",params![e.header.event_id,USER,seq,e.header.device_id,e.header.entity_id,e.header.entity_type,if e.deleted_at.is_some(){"delete"}else{"upsert"},e.header.revision,NOW,e.deleted_at,vec![0u8;24],cipher]).unwrap();
        db.execute(
            "UPDATE cloud_sync_state SET pull_cursor=MAX(pull_cursor,?1)",
            [seq],
        )
        .unwrap();
    }
    fn arrival(db: &mut Connection, e: &Event, seq: i64) -> String {
        persist_arrival(db, e, seq);
        let raw = frame(e).unwrap();
        apply(
            db,
            &scope(),
            &e.header.event_id,
            &raw,
            &[0; 24],
            &vec![17; raw.len() + 16],
        )
        .unwrap()
    }
    pub(crate) fn migrate(db: &mut Connection) -> Vec<Event> {
        begin(db, &scope(), NOW).unwrap();
        let mut out = vec![];
        let mut seq = 3;
        loop {
            let ready = pending(db, &scope(), false).unwrap();
            if ready.is_empty() {
                break;
            }
            for row in ready {
                let e: Event = serde_json::from_value(row["event"].clone()).unwrap();
                let raw = frame(&e).unwrap();
                seal(
                    db,
                    &scope(),
                    &e.header.event_id,
                    &raw,
                    &[0; 24],
                    &vec![17; raw.len() + 16],
                )
                .unwrap();
                assert_eq!(arrival(db, &e, seq), "applied");
                seq += 1;
                out.push(e);
            }
        }
        out
    }
    fn update(base: &Event, n: u32, payload: Value) -> Event {
        let mut e = base.clone();
        e.header.event_id = id(n);
        e.header.operation = "update".into();
        e.header.parent_event_ids = vec![base.header.event_id.clone()];
        e.header.revision += 1;
        e.header.generation += 1;
        e.payload = payload;
        e.deleted_at = (e.header.entity_type == "folder_membership"
            && e.payload["folder_id"].is_null()
            || e.header.entity_type == "folder" && e.payload.is_null())
        .then(|| NOW.into());
        e
    }
    #[test]
    fn account_catalog_migration_local_only_restart_seal_self_echo_ack() {
        let (mut db, path) = setup();
        seed(&mut db);
        let view = begin(&mut db, &scope(), NOW).unwrap();
        assert_eq!(view["state"], "captured");
        let snapshot: String = db
            .query_row("SELECT snapshot_json FROM cloud_catalog_state", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(!snapshot.contains("L1"));
        let events = migrate(&mut db);
        assert_eq!(events.len(), 6);
        assert_eq!(authority(&db, &scope()).unwrap()["state"], "active");
        let raw = source(&db).unwrap();
        assert_eq!(raw["order"], json!(["L1", "C1", "C2"]));
        assert_eq!(raw["members"][2], json!(["L1", "F1"]));
        drop(db);
        let db = crate::sqlite::open_database(&path).unwrap();
        assert_eq!(authority(&db, &scope()).unwrap()["state"], "active");
        assert!(ack_proven(&db, "a", 8).unwrap());
        assert!(db
            .execute(
                "DELETE FROM cloud_sync_project_bindings WHERE project_id='C1'",
                []
            )
            .is_err());
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn account_catalog_conflicts_stale_resolution_all_types_and_tombstone() {
        let (mut db, path) = setup();
        seed(&mut db);
        let bases = migrate(&mut db);
        let mut seq = 9;
        for t in TYPES {
            let base = bases.iter().find(|e| e.header.entity_type == t).unwrap();
            let mut a = update(base, 100 + seq as u32, base.payload.clone());
            let mut b = update(base, 200 + seq as u32, base.payload.clone());
            if t == "folder" {
                a.payload = json!({"name":"Alpha"});
                b.payload = json!({"name":"Beta"});
            }
            if t == "folder_order" {
                a.payload = json!({"ids":["F2","F1"]});
            }
            // Current dependency heads may have advanced in previous units.
            a.dependencies = dependencies(&db, "a", t, &base.header.entity_id, &a.payload).unwrap();
            b.dependencies = dependencies(&db, "a", t, &base.header.entity_id, &b.payload).unwrap();
            assert_eq!(arrival(&mut db, &a, seq), "applied");
            seq += 1;
            assert_eq!(arrival(&mut db, &b, seq), "conflict_preserved");
            seq += 1;
            let hs = tips(&db, "a", t, &base.header.entity_id).unwrap();
            let d = Decision {
                entity_type: t.into(),
                entity_id: base.header.entity_id.clone(),
                expected_tips: hs.clone(),
                expected_local: local(&db, "a", t, &base.header.entity_id).unwrap(),
                proposed: a.payload.clone(),
                selected_event_id: None,
            };
            let rid = decide(&mut db, &scope(), &d, NOW).unwrap();
            assert_eq!(decide(&mut db, &scope(), &d, NOW).unwrap(), rid);
            let r = event(&db, "a", &rid).unwrap().unwrap();
            let mut c = update(base, 300 + seq as u32, base.payload.clone());
            c.dependencies = a.dependencies.clone();
            assert_eq!(arrival(&mut db, &c, seq), "conflict_preserved");
            seq += 1;
            assert_eq!(arrival(&mut db, &r, seq), "conflict_preserved");
            seq += 1;
            assert!(tips(&db, "a", t, &base.header.entity_id)
                .unwrap()
                .contains(&c.header.event_id));
            assert_eq!(tips(&db, "a", t, &base.header.entity_id).unwrap().len(), 2);
            let d2 = Decision {
                expected_tips: tips(&db, "a", t, &base.header.entity_id).unwrap(),
                expected_local: local(&db, "a", t, &base.header.entity_id).unwrap(),
                ..d
            };
            let id = decide(&mut db, &scope(), &d2, NOW).unwrap();
            let r2 = event(&db, "a", &id).unwrap().unwrap();
            assert_eq!(arrival(&mut db, &r2, seq), "applied");
            seq += 1;
        }
        let base = event(&db, "a", &tips(&db, "a", "folder", "F1").unwrap()[0])
            .unwrap()
            .unwrap();
        let mut del = update(&base, 999, Value::Null);
        del.header.operation = "delete".into();
        assert_eq!(arrival(&mut db, &del, seq), "catalog_folder_has_members");
        assert!(!ack_proven(&db, "a", seq).unwrap());
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM project_folder_members WHERE folder_id='F1'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            2
        );
        drop(db);
        let db = crate::sqlite::open_database(&path).unwrap();
        assert_eq!(
            tips(&db, "a", "folder", "F1").unwrap(),
            vec![del.header.event_id]
        );
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn account_catalog_pending_edits_frozen_retries_and_source_blocker_recovery() {
        let (mut db, path) = setup();
        seed(&mut db);
        db.execute("UPDATE project_folders SET payload_json=json_set(payload_json,'$.unsupported',42) WHERE id='F1'",[]).unwrap();
        assert_eq!(begin(&mut db, &scope(), NOW).unwrap()["state"], "blocked");
        assert!(!normal(
            &mut db,
            "folder",
            "F1",
            json!({"name":"not published"}),
            NOW
        )
        .unwrap());
        assert_eq!(
            db.query_row("SELECT count(*) FROM cloud_catalog_events", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        db.execute("UPDATE project_folders SET payload_json=json_remove(payload_json,'$.unsupported') WHERE id='F1'",[]).unwrap();
        migrate(&mut db);
        normal(&mut db, "folder", "F1", json!({"name":"First"}), NOW).unwrap();
        let first: Event =
            serde_json::from_value(pending(&db, &scope(), false).unwrap()[0]["event"].clone())
                .unwrap();
        let raw = frame(&first).unwrap();
        let cipher = vec![17u8; raw.len() + 16];
        seal(
            &mut db,
            &scope(),
            &first.header.event_id,
            &raw,
            &[0; 24],
            &cipher,
        )
        .unwrap();
        normal(&mut db, "folder", "F1", json!({"name":"Second"}), NOW).unwrap();
        assert_eq!(
            pending(&db, &scope(), true).unwrap()[0]["event"]["payload"]["name"],
            "First"
        );
        assert!(pending(&db, &scope(), false).unwrap().is_empty());
        assert_eq!(arrival(&mut db, &first, 9), "applied");
        let second: Event =
            serde_json::from_value(pending(&db, &scope(), false).unwrap()[0]["event"].clone())
                .unwrap();
        assert_eq!(
            second.header.parent_event_ids,
            vec![first.header.event_id.clone()]
        );
        assert_eq!(second.header.revision, first.header.revision + 1);
        assert_eq!(arrival(&mut db, &second, 10), "applied");
        assert_eq!(arrival(&mut db, &second, 10), "applied");
        assert_eq!(
            db.query_row("SELECT name FROM project_folders WHERE id='F1'", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap(),
            "Second"
        );
        drop(db);
        let db = crate::sqlite::open_database(&path).unwrap();
        assert_eq!(
            event(&db, "a", &first.header.event_id)
                .unwrap()
                .unwrap()
                .payload,
            json!({"name":"First"})
        );
        assert!(db
            .query_row(
                "SELECT count(*)>=2 FROM cloud_catalog_candidates",
                [],
                |r| r.get::<_, bool>(0)
            )
            .unwrap());
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn account_catalog_interleaved_local_projects_and_mixed_membership_are_device_owned() {
        let (mut db, path) = setup();
        seed(&mut db);
        for p in ["L2", "L3"] {
            db.execute("INSERT INTO projects(id,name,infinite,unit,status,payload_json) VALUES(?1,?1,1,'symbols','active','{}')",[p]).unwrap();
        }
        db.execute("DELETE FROM project_order", []).unwrap();
        for (i, p) in ["L1", "C1", "L2", "C2", "L3"].iter().enumerate() {
            db.execute(
                "INSERT INTO project_order VALUES(?1,?2)",
                params![p, i as i64],
            )
            .unwrap();
        }
        let events = migrate(&mut db);
        assert!(!serde_json::to_string(&events).unwrap().contains("L1"));
        normal(
            &mut db,
            "project_order",
            "project_order",
            json!({"ids":["C2","C1"]}),
            NOW,
        )
        .unwrap();
        let e: Event =
            serde_json::from_value(pending(&db, &scope(), false).unwrap()[0]["event"].clone())
                .unwrap();
        assert_eq!(arrival(&mut db, &e, 9), "applied");
        assert_eq!(
            source(&db).unwrap()["order"],
            json!(["L1", "C2", "L2", "C1", "L3"])
        );
        assert!(!normal(
            &mut db,
            "folder_membership",
            "L1",
            json!({"folder_id":"F2"}),
            NOW
        )
        .unwrap());
        assert!(normal(
            &mut db,
            "project_order",
            "project_order",
            json!({"ids":["C1","L1","C2"]}),
            NOW
        )
        .is_err());
        assert_eq!(
            db.query_row(
                "SELECT folder_id FROM project_folder_members WHERE project_id='L1'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "F1"
        );
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn account_catalog_remote_local_candidate_requires_explicit_adoption() {
        let (mut db, path) = setup();
        let remote = make(
            &scope(),
            "folder",
            "F1",
            json!({"name":"Remote"}),
            vec![],
            1,
            1,
            Dependencies::default(),
            NOW,
            false,
        )
        .unwrap();
        db.execute("INSERT INTO project_folders VALUES('F1','Local',0,'{\"id\":\"F1\",\"name\":\"Local\"}')",[]).unwrap();
        assert_eq!(arrival(&mut db, &remote, 1), "conflict_preserved");
        assert_eq!(
            db.query_row("SELECT name FROM project_folders", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "Local"
        );
        let v = authority(&db, &scope()).unwrap();
        assert_eq!(v["state"], "conflict");
        assert!(ack_proven(&db, "a", 1).unwrap());
        let d = Decision {
            entity_type: "folder".into(),
            entity_id: "F1".into(),
            expected_tips: vec![remote.header.event_id.clone()],
            expected_local: json!({"name":"Local"}),
            proposed: Value::Null,
            selected_event_id: Some(remote.header.event_id.clone()),
        };
        let id = decide(&mut db, &scope(), &d, NOW).unwrap();
        let e = event(&db, "a", &id).unwrap().unwrap();
        assert_eq!(arrival(&mut db, &e, 2), "applied");
        assert_eq!(
            db.query_row("SELECT name FROM project_folders", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "Remote"
        );
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM cloud_catalog_local_conflicts",
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
    fn account_catalog_child_before_parent_and_invalid_scope_are_not_acked() {
        let (mut db, path) = setup();
        seed(&mut db);
        begin(&mut db, &scope(), NOW).unwrap();
        let order: Vec<u8> = db
            .query_row(
                "SELECT canonical_frame FROM cloud_catalog_events WHERE entity_type='folder_order'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let e = unframe(&order).unwrap();
        assert_eq!(arrival(&mut db, &e, 3), "catalog_dependency_missing");
        assert!(!ack_proven(&db, "a", 3).unwrap());
        let mut bad = e.clone();
        bad.header.account_id = id(500);
        assert!(apply(
            &mut db,
            &scope(),
            &bad.header.event_id,
            &frame(&bad).unwrap(),
            &[0; 24],
            &[0; 16]
        )
        .is_err());
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn account_catalog_membership_waits_for_reconciled_metadata_after_restart() {
        let (mut db, path) = setup();
        seed(&mut db);
        migrate(&mut db);
        let payload = json!({"folder_id":null});
        let parent = event(
            &db,
            "a",
            &tips(&db, "a", "folder_membership", "C1").unwrap()[0],
        )
        .unwrap()
        .unwrap();
        let e = make(
            &scope(),
            "folder_membership",
            "C1",
            payload.clone(),
            vec![parent.header.event_id],
            2,
            2,
            dependencies(&db, "a", "folder_membership", "C1", &payload).unwrap(),
            NOW,
            false,
        )
        .unwrap();
        // An authenticated old metadata ledger is not current reconciled authority.
        db.execute(
            "UPDATE projects SET name='Unreconciled local rename' WHERE id='C1'",
            [],
        )
        .unwrap();
        assert_ne!(
            metadata::authority_view(&db, "a", "C1").unwrap().state,
            "active"
        );
        assert_eq!(arrival(&mut db, &e, 9), "catalog_project_unproven");
        assert!(!ack_proven(&db, "a", 9).unwrap());
        assert_eq!(
            db.query_row(
                "SELECT folder_id FROM project_folder_members WHERE project_id='C1'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "F1"
        );
        drop(db);
        let mut db = crate::sqlite::open_database(&path).unwrap();
        assert_eq!(
            frame(&event(&db, "a", &e.header.event_id).unwrap().unwrap()).unwrap(),
            frame(&e).unwrap()
        );
        db.execute("UPDATE projects SET name='C1' WHERE id='C1'", [])
            .unwrap();
        assert_eq!(
            metadata::authority_view(&db, "a", "C1").unwrap().state,
            "active"
        );
        assert_eq!(arrival(&mut db, &e, 9), "applied");
        assert!(ack_proven(&db, "a", 9).unwrap());
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn account_catalog_stale_dependency_recovery_preserves_newer_resolution_and_shared_ack() {
        let (mut db, path) = setup();
        seed(&mut db);
        migrate(&mut db);
        normal(
            &mut db,
            "folder_order",
            "folder_order",
            json!({"ids":["F2","F1"]}),
            NOW,
        )
        .unwrap();
        let pending_order: Event =
            serde_json::from_value(pending(&db, &scope(), false).unwrap()[0]["event"].clone())
                .unwrap();
        let raw = frame(&pending_order).unwrap();
        let cipher = vec![17u8; raw.len() + 16];
        seal(
            &mut db,
            &scope(),
            &pending_order.header.event_id,
            &raw,
            &[0; 24],
            &cipher,
        )
        .unwrap();
        // A different device renames F1 before this frozen order is uploaded.
        let parent = event(&db, "a", &tips(&db, "a", "folder", "F1").unwrap()[0])
            .unwrap()
            .unwrap();
        let mut peer = scope();
        peer.device_id = id(500);
        let rename = make(
            &peer,
            "folder",
            "F1",
            json!({"name":"Renamed"}),
            vec![parent.header.event_id],
            2,
            2,
            Dependencies::default(),
            NOW,
            false,
        )
        .unwrap();
        assert_eq!(arrival(&mut db, &rename, 9), "applied");
        // Upgrade/retry of an exact blocker retained by the accepted old reader.
        persist_arrival(&mut db, &pending_order, 10);
        crate::account_sync::block(
            &db,
            &scope(),
            &pending_order.header.event_id,
            &[0; 24],
            &cipher,
            "catalog_membership_changed",
        )
        .unwrap();
        let before = crate::note_sync::prepare_note_sync_ack(
            &mut db,
            &crate::note_sync::PrepareNoteSyncAckCommand {
                account_id: "a".into(),
                canonical_user_id: USER.into(),
                device_id: DEVICE.into(),
            },
        )
        .unwrap();
        assert_eq!(before.candidate_cursor, 9);
        // A newer decision must survive the delayed admission of the old order.
        let expected_tips = tips(&db, "a", "folder_order", "folder_order").unwrap();
        let d = Decision {
            entity_type: "folder_order".into(),
            entity_id: "folder_order".into(),
            expected_tips: expected_tips.clone(),
            expected_local: local(&db, "a", "folder_order", "folder_order").unwrap(),
            selected_event_id: Some(expected_tips[0].clone()),
            proposed: Value::Null,
        };
        let rid = decide(&mut db, &scope(), &d, NOW).unwrap();
        let r = event(&db, "a", &rid).unwrap().unwrap();
        assert_eq!(arrival(&mut db, &r, 11), "applied");
        assert!(ack_proven(&db, "a", 11).unwrap());
        // A real project-scoped Stage after the hole uses the same account ACK.
        let mut value: Value = serde_json::from_str(include_str!(
            "../../src/cloud/__fixtures__/stageCodecV1.json"
        ))
        .unwrap();
        let h = &mut value["event"]["header"];
        h["account_id"] = json!(USER);
        h["project_id"] = json!("C1");
        h["entity_id"] = json!("S1");
        h["device_id"] = json!(id(500));
        h["event_id"] = json!(id(800));
        h["updated_at"] = json!(NOW);
        h["bootstrap_id"] = json!(id(21));
        h["metadata_event_id"] = json!(id(1));
        let stage: crate::stage_sync::Event =
            serde_json::from_value(value["event"].clone()).unwrap();
        let stage_frame = crate::stage_sync::frame(&stage).unwrap();
        let encrypted = vec![17u8; stage_frame.len() + 16];
        let cmd:crate::note_sync::CommitMixedSyncInboundPageCommand=serde_json::from_value(json!({"account_id":"a","canonical_user_id":USER,"device_id":DEVICE,"expected_cursor":11,"next_cursor":12,"has_more":false,"items":[{"event_id":id(800),"server_sequence":12,"source_device_id":id(500),"project_id":"C1","entity_id":"S1","entity_type":"stage","operation":"upsert","revision":1,"updated_at":NOW,"deleted_at":null,"envelope":{"crypto_version":1,"aad_version":1,"nonce":crate::note_sync::encode_canonical_base64url(&[0u8;24]),"ciphertext":crate::note_sync::encode_canonical_base64url(&encrypted)}}]})).unwrap();
        crate::note_sync::commit_mixed_sync_inbound_page(&mut db, &cmd).unwrap();
        assert_eq!(
            crate::stage_sync::apply_received(
                &mut db,
                "a",
                &id(800),
                &stage_frame,
                &[0; 24],
                &encrypted
            )
            .unwrap(),
            "applied"
        );
        drop(db);
        let mut db = crate::sqlite::open_database(&path).unwrap();
        assert_eq!(arrival(&mut db, &pending_order, 10), "conflict_preserved");
        let mut expected = vec![pending_order.header.event_id.clone(), rid];
        expected.sort();
        assert_eq!(
            tips(&db, "a", "folder_order", "folder_order").unwrap(),
            expected
        );
        assert_eq!(
            local(&db, "a", "folder_order", "folder_order").unwrap(),
            r.payload
        );
        assert!(ack_proven(&db, "a", 10).unwrap());
        let ack = crate::note_sync::prepare_note_sync_ack(
            &mut db,
            &crate::note_sync::PrepareNoteSyncAckCommand {
                account_id: "a".into(),
                canonical_user_id: USER.into(),
                device_id: DEVICE.into(),
            },
        )
        .unwrap();
        assert_eq!(ack.candidate_cursor, 12);
        let stored: (Vec<u8>, Vec<u8>) = db
            .query_row(
                "SELECT nonce,ciphertext FROM cloud_catalog_events WHERE event_id=?1",
                [&pending_order.header.event_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(stored, (vec![0; 24], cipher));
        assert_eq!(
            frame(
                &event(&db, "a", &pending_order.header.event_id)
                    .unwrap()
                    .unwrap()
            )
            .unwrap(),
            frame(&pending_order).unwrap()
        );
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    fn next_event(db: &Connection, t: &str, id: &str, payload: Value) -> Event {
        let hs = tips(db, "a", t, id).unwrap();
        let parent = event(db, "a", &hs[0]).unwrap().unwrap();
        make(
            &scope(),
            t,
            id,
            payload.clone(),
            hs,
            parent.header.revision + 1,
            parent.header.generation + 1,
            dependencies(db, "a", t, id, &payload).unwrap(),
            NOW,
            false,
        )
        .unwrap()
    }
    pub(crate) fn metadata_successor(db: &mut Connection, project: &str, seq: i64) {
        let head = metadata::authority_view(db, "a", project)
            .unwrap()
            .head_event_id
            .unwrap();
        let raw: Vec<u8> = db
            .query_row(
                "SELECT canonical_payload FROM cloud_sync_metadata_apply_ledger WHERE event_id=?1",
                [&head],
                |r| r.get(0),
            )
            .unwrap();
        let mut e: Value = serde_json::from_slice(&raw).unwrap();
        let h = &mut e["header"];
        h["event_id"] = json!(id(600));
        h["operation"] = json!("update");
        h["device_id"] = json!(id(500));
        h["parent_event_ids"] = json!([head]);
        h["revision"] = json!(h["revision"].as_i64().unwrap() + 1);
        h["generation"] = json!(h["generation"].as_i64().unwrap() + 1);
        e["metadata"]["name"] = json!("Causally renamed");
        db.execute("INSERT INTO cloud_sync_inbox(account_id,event_id,server_sequence,device_id,project_id,entity_id,entity_type,operation,sync_revision,updated_at,state,received_at) VALUES('a',?1,?2,?3,?4,?4,'project_metadata','upsert',2,?5,'received',?5)",params![id(600),seq,id(500),project,NOW]).unwrap();
        db.execute(
            "INSERT INTO cloud_sync_event_objects VALUES('a',?1,1,1,zeroblob(24),zeroblob(16),?2)",
            params![id(600), NOW],
        )
        .unwrap();
        metadata::preserve_authenticated_event(
            db,
            "a",
            project,
            &serde_json::to_vec(&e).unwrap(),
            NOW,
        )
        .unwrap();
        assert_eq!(
            metadata::authority_view(db, "a", project).unwrap().state,
            "active"
        );
        db.execute(
            "UPDATE cloud_sync_state SET pull_cursor=MAX(pull_cursor,?1)",
            [seq],
        )
        .unwrap();
    }
    #[test]
    fn account_catalog_project_order_and_membership_cover_causal_metadata_move_and_null() {
        for removal in [false, true] {
            let (mut db, path) = setup();
            seed(&mut db);
            migrate(&mut db);
            let order = next_event(
                &db,
                "project_order",
                "project_order",
                json!({"ids":["C2","C1"]}),
            );
            let relation = next_event(&db, "folder_membership", "C1", json!({"folder_id":"F2"}));
            metadata_successor(&mut db, "C1", 9);
            let change = next_event(
                &db,
                "folder_membership",
                "C2",
                json!({"folder_id":if removal {Value::Null}else{json!("F1")}}),
            );
            assert_eq!(arrival(&mut db, &change, 10), "applied");
            let folder_rename = next_event(&db, "folder", "F2", json!({"name":"Renamed target"}));
            assert_eq!(arrival(&mut db, &folder_rename, 11), "applied");
            assert_eq!(arrival(&mut db, &relation, 12), "applied");
            assert_eq!(arrival(&mut db, &order, 13), "applied");
            assert!(ack_proven(&db, "a", 13).unwrap());
            assert_eq!(
                local(&db, "a", "project_order", "project_order").unwrap(),
                order.payload
            );
            assert!(!serde_json::to_string(&order).unwrap().contains("L1"));
            for (offset, (field, value)) in
                [("bootstrap_id", id(999)), ("metadata_event_id", id(998))]
                    .into_iter()
                    .enumerate()
            {
                let mut bad = order.clone();
                bad.header.event_id = metadata::new_event_id().unwrap();
                if field == "bootstrap_id" {
                    bad.dependencies
                        .projects
                        .get_mut("C1")
                        .unwrap()
                        .bootstrap_id = value;
                } else {
                    bad.dependencies
                        .projects
                        .get_mut("C1")
                        .unwrap()
                        .metadata_event_id = value;
                }
                assert_eq!(
                    arrival(&mut db, &bad, 14 + offset as i64),
                    "catalog_project_unproven"
                );
                assert!(!ack_proven(&db, "a", 14 + offset as i64).unwrap());
            }
            drop(db);
            std::fs::remove_file(path).unwrap();
        }
    }
    #[test]
    fn account_catalog_dependency_conflict_restart_resolution_and_unrelated_history() {
        let (mut db, path) = setup();
        seed(&mut db);
        migrate(&mut db);
        let order = next_event(
            &db,
            "folder_order",
            "folder_order",
            json!({"ids":["F2","F1"]}),
        );
        let b = next_event(&db, "folder", "F1", json!({"name":"B"}));
        let mut c = b.clone();
        c.header.event_id = metadata::new_event_id().unwrap();
        c.payload = json!({"name":"C"});
        assert_eq!(arrival(&mut db, &b, 9), "applied");
        assert_eq!(arrival(&mut db, &c, 10), "conflict_preserved");
        assert_eq!(arrival(&mut db, &order, 11), "catalog_dependency_conflict");
        drop(db);
        let mut db = crate::sqlite::open_database(&path).unwrap();
        let d = Decision {
            entity_type: "folder".into(),
            entity_id: "F1".into(),
            expected_tips: tips(&db, "a", "folder", "F1").unwrap(),
            expected_local: local(&db, "a", "folder", "F1").unwrap(),
            selected_event_id: Some(b.header.event_id.clone()),
            proposed: Value::Null,
        };
        let rid = decide(&mut db, &scope(), &d, NOW).unwrap();
        let r = event(&db, "a", &rid).unwrap().unwrap();
        assert_eq!(arrival(&mut db, &r, 12), "applied");
        drop(db);
        let mut db = crate::sqlite::open_database(&path).unwrap();
        assert_eq!(arrival(&mut db, &order, 11), "applied");
        assert!(ack_proven(&db, "a", 11).unwrap());
        // Same ID and same name on an unrelated genesis do not establish ancestry.
        let mut unrelated = b.clone();
        unrelated.header.event_id = metadata::new_event_id().unwrap();
        unrelated.header.operation = "create".into();
        unrelated.header.parent_event_ids.clear();
        unrelated.header.revision = 1;
        unrelated.header.generation = 1;
        assert_eq!(arrival(&mut db, &unrelated, 13), "conflict_preserved");
        let mut newer = order.clone();
        newer.header.event_id = metadata::new_event_id().unwrap();
        assert_eq!(arrival(&mut db, &newer, 14), "catalog_dependency_conflict");
        let mut budget = ProofBudget(DEPENDENCY_TOTAL_NODES);
        assert!(ancestry_coverage(
            &[unrelated.header.event_id.clone()],
            &order.dependencies.folders["F1"],
            &mut budget,
            |h| catalog_node(&db, "a", "folder", "F1", h)
        )
        .is_err());
        assert!(!ack_proven(&db, "a", 14).unwrap());
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn account_catalog_live_set_change_and_tombstone_are_preserved_not_materialized() {
        for deletion in [false, true] {
            let (mut db, path) = setup();
            seed(&mut db);
            migrate(&mut db);
            let old = next_event(
                &db,
                "folder_order",
                "folder_order",
                json!({"ids":["F2","F1"]}),
            );
            let old_relation =
                next_event(&db, "folder_membership", "C1", json!({"folder_id":"F2"}));
            if deletion {
                let removal = next_event(&db, "folder_membership", "C2", json!({"folder_id":null}));
                assert_eq!(arrival(&mut db, &removal, 9), "applied");
                let delete = next_event(&db, "folder", "F2", Value::Null);
                assert_eq!(arrival(&mut db, &delete, 10), "applied");
            } else {
                let f = make(
                    &scope(),
                    "folder",
                    "F3",
                    json!({"name":"New"}),
                    vec![],
                    1,
                    1,
                    Dependencies::default(),
                    NOW,
                    false,
                )
                .unwrap();
                assert_eq!(arrival(&mut db, &f, 9), "applied");
            }
            let visible = local(&db, "a", "folder_order", "folder_order").unwrap();
            assert_eq!(arrival(&mut db, &old, 11), "conflict_preserved");
            assert_eq!(
                local(&db, "a", "folder_order", "folder_order").unwrap(),
                visible
            );
            assert!(ack_proven(&db, "a", 11).unwrap());
            let d = Decision {
                entity_type: "folder_order".into(),
                entity_id: "folder_order".into(),
                expected_tips: tips(&db, "a", "folder_order", "folder_order").unwrap(),
                expected_local: visible.clone(),
                proposed: visible,
                selected_event_id: None,
            };
            let rid = decide(&mut db, &scope(), &d, NOW).unwrap();
            let r = event(&db, "a", &rid).unwrap().unwrap();
            assert_eq!(arrival(&mut db, &r, 12), "applied");
            if deletion {
                assert_eq!(arrival(&mut db, &old_relation, 13), "conflict_preserved");
                assert_eq!(
                    local(&db, "a", "folder_membership", "C1").unwrap(),
                    json!({"folder_id":"F1"})
                );
                assert!(ack_proven(&db, "a", 13).unwrap());
            }
            let raw = frame(&old).unwrap();
            assert!(apply(
                &mut db,
                &scope(),
                &old.header.event_id,
                &raw,
                &[0; 24],
                &vec![18; raw.len() + 16]
            )
            .is_err());
            assert!(db
                .execute(
                    "UPDATE cloud_sync_account_inbox SET ciphertext=zeroblob(16) WHERE event_id=?1",
                    [&old.header.event_id]
                )
                .is_err());
            assert!(!ack_proven(&db, "foreign", 11).unwrap());
            drop(db);
            std::fs::remove_file(path).unwrap();
        }
    }
    #[test]
    fn account_catalog_ancestry_budget_cycle_and_missing_are_not_ack_proofs() {
        let mut budget = ProofBudget(DEPENDENCY_TOTAL_NODES);
        assert!(
            ancestry_coverage(&["300".into()], &["0".into()], &mut budget, |h| Ok(
                CoverageNode {
                    parents: if h == "0" {
                        vec![]
                    } else {
                        vec![(h.parse::<usize>().unwrap() - 1).to_string()]
                    },
                    live: true
                }
            ))
            .is_err()
        );
        let mut budget = ProofBudget(0);
        assert!(matches!(
            ancestry_coverage(
                &["0".into()],
                &["0".into()],
                &mut budget,
                |_| unreachable!()
            ),
            Err(Error::Code("catalog_resource_limit"))
        ));
        let mut budget = ProofBudget(DEPENDENCY_TOTAL_NODES);
        assert!(
            ancestry_coverage(&["cycle".into()], &["ref".into()], &mut budget, |h| Ok(
                CoverageNode {
                    parents: vec![h.into()],
                    live: true
                }
            ))
            .is_err()
        );
        let mut budget = ProofBudget(DEPENDENCY_TOTAL_NODES);
        assert!(
            ancestry_coverage(&["tip".into()], &["unknown".into()], &mut budget, |_| Err(
                Error::Code("catalog_dependency_missing")
            ))
            .is_err()
        );
    }
    #[test]
    fn account_catalog_project_set_change_preserves_order_and_unproven_local_ids_block() {
        let (mut db, path) = setup();
        seed(&mut db);
        migrate(&mut db);
        let old = next_event(
            &db,
            "project_order",
            "project_order",
            json!({"ids":["C2","C1"]}),
        );
        let mut meta = metadata::authority_view(&db, "a", "C1")
            .unwrap()
            .local
            .unwrap();
        meta["name"] = json!("C3");
        db.execute("INSERT INTO projects(id,name,infinite,unit,status,payload_json) VALUES('C3','C3',1,'symbols','active',?1)",[meta.to_string()]).unwrap();
        db.execute("INSERT INTO project_order VALUES('C3',3)", [])
            .unwrap();
        db.execute(
            "INSERT INTO cloud_sync_project_bindings VALUES('C3','a',?1,?1)",
            [NOW],
        )
        .unwrap();
        db.execute("INSERT INTO cloud_sync_project_bootstraps(project_id,account_id,device_id,bootstrap_id,mode,phase,created_at,updated_at) VALUES('C3','a',?1,?2,'upload_existing','prepared',?3,?3)",params![DEVICE,id(701),NOW]).unwrap();
        let root = json!({"version":1,"header":{"account_id":USER,"project_id":"C3","entity_id":"C3","device_id":DEVICE,"bootstrap_id":id(701),"event_id":id(700),"revision":1,"generation":1,"operation":"create","parent_event_ids":[],"updated_at":NOW},"metadata":meta,"deleted_at":null});
        db.execute("INSERT INTO cloud_sync_inbox(account_id,event_id,server_sequence,device_id,project_id,entity_id,entity_type,operation,sync_revision,updated_at,state,received_at) VALUES('a',?1,9,?2,'C3','C3','project_metadata','upsert',1,?3,'received',?3)",params![id(700),DEVICE,NOW]).unwrap();
        db.execute(
            "INSERT INTO cloud_sync_event_objects VALUES('a',?1,1,1,zeroblob(24),zeroblob(16),?2)",
            params![id(700), NOW],
        )
        .unwrap();
        metadata::preserve_authenticated_event(
            &mut db,
            "a",
            "C3",
            &serde_json::to_vec(&root).unwrap(),
            NOW,
        )
        .unwrap();
        let proof = eligible(&db, "a").unwrap()["C3"].clone();
        let member = make(
            &scope(),
            "folder_membership",
            "C3",
            json!({"folder_id":null}),
            vec![],
            1,
            1,
            Dependencies {
                projects: BTreeMap::from([("C3".into(), proof)]),
                ..Default::default()
            },
            NOW,
            false,
        )
        .unwrap();
        assert_eq!(arrival(&mut db, &member, 10), "applied");
        let visible = local(&db, "a", "project_order", "project_order").unwrap();
        assert_eq!(arrival(&mut db, &old, 11), "conflict_preserved");
        assert_eq!(
            local(&db, "a", "project_order", "project_order").unwrap(),
            visible
        );
        assert!(ack_proven(&db, "a", 11).unwrap());
        let mut bad = old.clone();
        bad.header.event_id = metadata::new_event_id().unwrap();
        bad.payload = json!({"ids":["C2","C1","L1"]});
        bad.dependencies
            .projects
            .insert("L1".into(), bad.dependencies.projects["C1"].clone());
        bad.dependencies
            .memberships
            .insert("L1".into(), bad.dependencies.memberships["C1"].clone());
        assert_eq!(arrival(&mut db, &bad, 12), "catalog_project_unproven");
        assert!(!ack_proven(&db, "a", 12).unwrap());
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM cloud_sync_project_bindings WHERE project_id='L1'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        db.execute("UPDATE projects SET name='Unreconciled' WHERE id='C2'", [])
            .unwrap();
        let mut unproven = old.clone();
        unproven.header.event_id = metadata::new_event_id().unwrap();
        assert_eq!(arrival(&mut db, &unproven, 13), "catalog_project_unproven");
        assert!(!ack_proven(&db, "a", 13).unwrap());
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
}
