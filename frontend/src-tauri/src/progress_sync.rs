//! Causal progress chains. Remote projection never produces ProgressAdded game events.
use crate::{document_codec::Header, progress_codec as codec, project_metadata_sync as metadata};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
type Result<T> = std::result::Result<T, String>;
#[derive(Debug)]
struct ApplyError(String);
impl From<String> for ApplyError {
    fn from(s: String) -> Self {
        Self(s)
    }
}
impl From<&str> for ApplyError {
    fn from(s: &str) -> Self {
        Self(s.into())
    }
}
impl From<rusqlite::Error> for ApplyError {
    fn from(_: rusqlite::Error) -> Self {
        Self("progress_storage_unavailable".into())
    }
}
impl From<crate::sqlite::StorageError> for ApplyError {
    fn from(_: crate::sqlite::StorageError) -> Self {
        Self("progress_storage_unavailable".into())
    }
}
type ApplyResult<T> = std::result::Result<T, ApplyError>;
fn sql(_: rusqlite::Error) -> String {
    "progress_storage_unavailable".into()
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Chain {
    pub base_total: String,
    pub entries: Vec<codec::Fact>,
    #[serde(skip)]
    ids: HashSet<String>,
    #[serde(skip)]
    migration: Option<(usize, i128)>,
}
impl Chain {
    pub(crate) fn migration_complete(&self) -> bool {
        self.migration.is_none()
    }
    pub fn total(&self) -> Result<i128> {
        self.entries
            .last()
            .map(codec::symbols)
            .unwrap_or_else(|| codec::micros(&self.base_total))
    }
    fn append(&mut self, f: &codec::Fact, compatibility: bool) -> Result<()> {
        self.append_at_base(f, compatibility, self.total()?)
    }
    fn append_at_base(&mut self, f: &codec::Fact, compatibility: bool, base: i128) -> Result<()> {
        if self.entries.len() >= codec::MAX_CHAIN {
            return Err("progress_resource_limit".into());
        }
        if self.ids.is_empty() {
            self.ids
                .extend(self.entries.iter().map(|f| f.entry_id.clone()));
        }
        if self.ids.contains(&f.entry_id) {
            return Err("progress_duplicate_entry".into());
        }
        if (base
            .checked_add(codec::delta_symbols(f)?)
            .ok_or("progress_resource_limit")?
            - codec::symbols(f)?)
        .abs()
            > if compatibility { 1 } else { 0 }
        {
            return Err("progress_base_inconsistent".into());
        }
        self.ids.insert(f.entry_id.clone());
        self.entries.push(f.clone());
        Ok(())
    }
}
/// Existing unit changes round the visible preceding total. This base is derived
/// from the immutable preceding fact and the authenticated new action's unit.
fn append_base(c: &Chain, f: &codec::Fact) -> Result<i128> {
    let total = c.total()?;
    if let Some(previous) = c.entries.last() {
        if previous.unit != f.unit {
            return codec::micros(&amount(
                display_fact(total, &f.unit, &previous.unit)? * codec::factor(&f.unit)? as f64,
            )?);
        }
    }
    Ok(total)
}
/// Pure, deterministic interpretation. Original facts are never modified.
pub fn transition(e: &codec::Event, mut parents: HashMap<String, Chain>) -> Result<Chain> {
    codec::validate(e)?;
    if parents.values().map(|c| c.entries.len()).sum::<usize>() > codec::MAX_CHAIN * 2 {
        return Err("progress_resource_limit".into());
    }
    let mut chain = if matches!(e.header.operation.as_str(), "genesis" | "adopt_local") {
        Chain {
            base_total: e.base_total.clone().unwrap(),
            entries: vec![],
            ids: HashSet::new(),
            migration: e
                .migration
                .as_ref()
                .map(|m| -> Result<(usize, i128)> {
                    Ok((m.entry_count, codec::micros(&m.final_total)?))
                })
                .transpose()?,
        }
    } else {
        parents
            .remove(e.selected_event_id.as_ref().unwrap())
            .ok_or("progress_parent_unknown")?
    };
    if chain.migration.is_some()
        && !matches!(
            e.header.operation.as_str(),
            "genesis" | "migrate" | "adopt_local"
        )
    {
        return Err("progress_migration_incomplete".into());
    }
    if e.header.operation == "migrate" && chain.migration.is_none() {
        return Err("progress_migration_already_complete".into());
    }
    if matches!(e.header.operation.as_str(), "correct" | "tombstone") {
        let target = e.target_entry_id.as_ref().unwrap();
        let pos = chain
            .entries
            .iter()
            .position(|f| &f.entry_id == target)
            .ok_or("progress_target_unknown")?;
        let old = chain.entries[pos..].to_vec();
        let required = if e.header.operation == "tombstone" {
            &old[1..]
        } else {
            &old[..]
        };
        if required.len() > codec::MAX_OPERATION {
            return Err("progress_resource_limit".into());
        }
        if e.rebased_from
            != required
                .iter()
                .map(|f| f.entry_id.clone())
                .collect::<Vec<_>>()
        {
            return Err("progress_descendant_rebase_required".into());
        }
        for (index, (new, original)) in e.entries.iter().zip(required).enumerate() {
            if new.entry_id == original.entry_id
                || new.occurred_at != original.occurred_at
                || new.writing_time != original.writing_time
                || new.writing_day != original.writing_day
                || new.unit != original.unit
                || (e.header.operation == "tombstone" || index > 0) && new.delta != original.delta
            {
                return Err("progress_rebase_invalid".into());
            }
        }
        chain.entries.truncate(pos);
        chain.ids = chain.entries.iter().map(|f| f.entry_id.clone()).collect();
    } else if e.header.operation == "rebase" {
        let originals: HashMap<_, _> = parents
            .values()
            .flat_map(|c| c.entries.iter())
            .map(|f| (f.entry_id.clone(), f))
            .collect();
        for (new, from) in e.entries.iter().zip(&e.rebased_from) {
            let original = originals
                .get(from)
                .ok_or("progress_rebase_source_unknown")?;
            if chain.entries.iter().any(|f| &f.entry_id == from)
                || new.entry_id == *from
                || new.delta != original.delta
                || new.unit != original.unit
                || new.occurred_at != original.occurred_at
                || new.writing_time != original.writing_time
                || new.writing_day != original.writing_day
            {
                return Err("progress_rebase_invalid".into());
            }
        }
        // An explicit selection of actions must preserve their order within each source branch.
        for parent in parents.values() {
            let positions: Vec<_> = e
                .rebased_from
                .iter()
                .filter_map(|id| parent.entries.iter().position(|f| &f.entry_id == id))
                .collect();
            if positions.windows(2).any(|p| p[0] >= p[1]) {
                return Err("progress_rebase_order_invalid".into());
            }
        }
    }
    for f in &e.entries {
        if e.header.operation == "append" {
            let base = append_base(&chain, f)?;
            chain.append_at_base(f, false, base)?;
            continue;
        }
        chain.append(
            f,
            matches!(
                e.header.operation.as_str(),
                "genesis" | "migrate" | "adopt_local"
            ),
        )?
    }
    if let Some((count, total)) = chain.migration {
        if chain.entries.len() > count {
            return Err("progress_migration_count_invalid".into());
        }
        if chain.entries.len() == count {
            if chain.total()? != total {
                return Err("progress_migration_total_invalid".into());
            }
            chain.migration = None
        }
    }
    Ok(chain)
}
fn rows(db: &Connection, q: &str, args: impl rusqlite::Params) -> Result<Vec<String>> {
    let mut q = db.prepare(q).map_err(sql)?;
    let r = q
        .query_map(args, |r| r.get(0))
        .map_err(sql)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(sql)?;
    Ok(r)
}
pub fn tips(db: &Connection, a: &str, p: &str, s: Option<&str>) -> Result<Vec<String>> {
    rows(db,"SELECT event_id FROM cloud_progress_tips WHERE account_id=?1 AND project_id=?2 AND entity_id=?3 ORDER BY event_id",params![a,p,codec::scope_id(s)])
}
pub fn local_heads(db: &Connection, p: &str, s: Option<&str>) -> Result<Option<Vec<String>>> {
    let a: Option<String> = db
        .query_row(
            "SELECT account_id FROM cloud_progress_migrations WHERE project_id=?1 AND entity_id=?2",
            params![p, codec::scope_id(s)],
            |r| r.get(0),
        )
        .optional()
        .map_err(sql)?;
    a.map(|a| tips(db, &a, p, s)).transpose()
}
fn event(db: &Connection, a: &str, p: &str, owner: &str, id: &str) -> Result<codec::Event> {
    let frame:Vec<u8>=db.query_row("SELECT canonical_frame FROM cloud_progress_events WHERE account_id=?1 AND project_id=?2 AND entity_id=?3 AND event_id=?4",params![a,p,owner,id],|r|r.get(0)).map_err(|_|"progress_parent_unknown")?;
    codec::decode(&frame)
}
pub fn chain(db: &Connection, a: &str, p: &str, owner: &str, id: &str) -> Result<Chain> {
    // Iterative DAG evaluation avoids recursive stack growth on years of history.
    let mut events = HashMap::new();
    let mut pending = vec![id.to_string()];
    while let Some(eid) = pending.pop() {
        if events.contains_key(&eid) {
            continue;
        }
        if events.len() >= codec::MAX_CHAIN {
            return Err("progress_resource_limit".into());
        }
        let e = event(db, a, p, owner, &eid)?;
        pending.extend(e.header.parents.clone());
        events.insert(eid, e);
    }
    let mut uses: HashMap<String, usize> = HashMap::new();
    for e in events.values() {
        for p in &e.header.parents {
            *uses.entry(p.clone()).or_default() += 1
        }
    }
    let mut order: Vec<_> = events.values().collect();
    order.sort_by_key(|e| e.header.revision);
    let mut chains: HashMap<String, Chain> = HashMap::new();
    for e in order {
        let mut parents = HashMap::new();
        let mut rev = 0;
        for parent in &e.header.parents {
            let count = uses.get_mut(parent).ok_or("progress_parent_unknown")?;
            *count -= 1;
            let c = if *count == 0 {
                chains.remove(parent)
            } else {
                chains.get(parent).cloned()
            }
            .ok_or("progress_parent_unknown")?;
            rev = rev.max(events[parent].header.revision);
            parents.insert(parent.clone(), c);
        }
        if e.header.revision != rev + 1 {
            return Err("progress_parent_revision_invalid".into());
        }
        let c = transition(e, parents)?;
        chains.insert(e.header.event_id.clone(), c);
    }
    chains.remove(id).ok_or("progress_parent_unknown".into())
}
pub fn dependencies(db: &Connection, a: &str, user: &str, e: &codec::Event) -> Result<()> {
    let h = &e.header;
    if h.account_id != user {
        return Err("progress_scope_mismatch".into());
    }
    crate::account_catalog::project_reference_ready(
        db,
        a,
        &h.project_id,
        &h.bootstrap_id,
        &h.metadata_event_id,
    )
    .map_err(|_| "project_metadata_authority_unresolved")?;
    if let Some(stage) = &h.stage_id {
        crate::stage_sync::content_reference_ready(db, a, &h.project_id, stage, &h.stage_event_ids)?
    }
    if h.operation == "append" {
        if let Some(f) = e.entries.first() {
            let unit: String = if let Some(stage) = &h.stage_id {
                let mut units = vec![];
                for id in &h.stage_event_ids {
                    let frame:Vec<u8>=db.query_row("SELECT canonical_frame FROM cloud_sync_structural_events WHERE account_id=?1 AND project_id=?2 AND entity_id=?3 AND event_id=?4",params![a,h.project_id,stage,id],|r|r.get(0)).map_err(sql)?;
                    let e = crate::stage_sync::unframe(&frame)
                        .map_err(|_| "progress_scope_mismatch")?;
                    units.push(
                        e.stage["unit"]
                            .as_str()
                            .ok_or("progress_unit_changed")?
                            .to_string(),
                    );
                }
                if units.iter().any(|unit| unit != &f.unit) {
                    return Err("progress_unit_changed".into());
                }
                f.unit.clone()
            } else {
                db.query_row("SELECT json_extract(payload_json,'$.unit') FROM cloud_sync_metadata_events WHERE account_id=?1 AND project_id=?2 AND event_id=?3",params![a,h.project_id,h.metadata_event_id],|r|r.get(0)).map_err(sql)?
            };
            if unit != f.unit {
                return Err("progress_unit_changed".into());
            }
        }
    }
    Ok(())
}
fn header(
    db: &Connection,
    a: &str,
    p: &str,
    s: Option<&str>,
    parents: Vec<String>,
    operation: &str,
    now: &str,
) -> Result<Header> {
    let view =
        metadata::authority_view(db, a, p).map_err(|_| "project_metadata_authority_unresolved")?;
    if view.state != "active" {
        return Err("project_metadata_authority_unresolved".into());
    }
    let (user,device,boot):(String,String,String)=db.query_row("SELECT b.canonical_user_id,s.device_id,p.bootstrap_id FROM cloud_account_bindings b JOIN cloud_sync_state s ON s.account_id=b.local_account_id JOIN cloud_sync_project_bootstraps p ON p.account_id=s.account_id WHERE s.account_id=?1 AND p.project_id=?2",params![a,p],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).map_err(sql)?;
    let refs = if let Some(s) = s {
        rows(db,"SELECT event_id FROM cloud_sync_structural_tips WHERE account_id=?1 AND project_id=?2 AND entity_type='stage' AND entity_id=?3 ORDER BY event_id",params![a,p,s])?
    } else {
        vec![]
    };
    let owner = codec::scope_id(s);
    let mut rev = 0;
    for parent in &parents {
        rev = rev.max(event(db, a, p, &owner, parent)?.header.revision)
    }
    let h = Header {
        account_id: user,
        device_id: device,
        project_id: p.into(),
        stage_id: s.map(str::to_string),
        entity_id: owner,
        event_id: metadata::new_event_id().map_err(|_| "progress_identity_unavailable")?,
        bootstrap_id: boot,
        metadata_event_id: view
            .head_event_id
            .ok_or("project_metadata_authority_unresolved")?,
        stage_event_ids: refs,
        parents,
        revision: rev + 1,
        generation: rev + 1,
        operation: operation.into(),
        updated_at: now.into(),
    };
    let dummy = codec::Event {
        version: 1,
        migration: None,
        header: h.clone(),
        base_total: None,
        entries: vec![],
        selected_event_id: None,
        target_entry_id: None,
        rebased_from: vec![],
    };
    dependencies(db, a, &h.account_id, &dummy)?;
    Ok(h)
}
fn parse(raw: &str) -> Result<Value> {
    if raw.len() > 64 * 1024 * 1024 {
        return Err("progress_resource_limit".into());
    }
    serde_json::from_str(raw).map_err(|_| "invalid_progress_source".into())
}
fn amount(n: f64) -> Result<String> {
    if !n.is_finite() || n.abs() > 1_000_000_000_000.0 {
        return Err("progress_resource_limit".into());
    }
    let value = format!("{n:.6}");
    let value = if value == "-0.000000" {
        "0.000000".to_string()
    } else {
        value
    };
    // At most one micro-symbol compatibility tolerance; never repair integer/page drift.
    if (value
        .parse::<f64>()
        .map_err(|_| "invalid_progress_source")?
        - n)
        .abs()
        > 0.000001
    {
        return Err("progress_precision_unsupported".into());
    }
    codec::micros(&value)?;
    Ok(value)
}
fn fact(raw: &Value, unit: &str) -> Result<codec::Fact> {
    if let Some(v) = raw.get("progress_fact") {
        let f: codec::Fact =
            serde_json::from_value(v.clone()).map_err(|_| "invalid_progress_source")?;
        if raw["id"].as_str() != Some(&f.entry_id)
            || raw["new_total_symbols"]
                .as_f64()
                .map(amount)
                .transpose()?
                .as_deref()
                != Some(&f.new_total)
            || raw["added_symbols"]
                .as_f64()
                .map(amount)
                .transpose()?
                .as_deref()
                != Some(&f.delta)
        {
            return Err("progress_base_inconsistent".into());
        }
        return Ok(f);
    }
    let t = raw["created_at"]
        .as_str()
        .ok_or("progress_timestamp_unknown")?;
    let (occurred, writing) = if t.ends_with('Z') || t.len() > 19 && t[19..].contains('+') {
        (
            Some(crate::document_sync::timestamp(t).map_err(|_| "progress_timestamp_invalid")?),
            None,
        )
    } else {
        (None, Some(t.to_string()))
    };
    let f = codec::Fact {
        entry_id: raw["id"].as_str().ok_or("invalid_progress_source")?.into(),
        new_total: amount(
            raw["new_total_symbols"]
                .as_f64()
                .or_else(|| {
                    raw["new_total"]
                        .as_f64()
                        .map(|n| n * codec::factor(unit).unwrap_or(0) as f64)
                })
                .ok_or("invalid_progress_source")?,
        )?,
        delta: amount(
            raw["added_symbols"]
                .as_f64()
                .ok_or("invalid_progress_source")?,
        )?,
        unit: raw
            .get("recorded_unit")
            .and_then(Value::as_str)
            .unwrap_or("symbols")
            .into(),
        occurred_at: occurred,
        writing_time: writing,
        writing_day: raw
            .get("writing_day")
            .and_then(Value::as_str)
            .unwrap_or(t.get(..10).ok_or("progress_timestamp_invalid")?)
            .into(),
    };
    Ok(f)
}
fn raw_evidence(db: &Connection, p: &str, s: Option<&str>) -> Result<Value> {
    let table = if s.is_some() { "stages" } else { "projects" };
    let raw: String = db
        .query_row(
            &format!("SELECT payload_json FROM {table} WHERE id=?1"),
            [s.unwrap_or(p)],
            |r| r.get(0),
        )
        .map_err(sql)?;
    let mut stmt=db.prepare("SELECT e.id,e.created_at,e.added_symbols,e.added_progress,e.payload_json,o.position,x.payload_json FROM progress_entries e LEFT JOIN progress_order o ON o.entry_id=e.id LEFT JOIN project_extensions x ON x.entity_type='progress' AND x.entity_id=e.id WHERE e.project_id=?1 AND e.stage_id IS ?2 ORDER BY o.position,e.id").map_err(sql)?;
    let values=stmt.query_map(params![p,s],|r|Ok(json!({"id":r.get::<_,String>(0)?,"created_at":r.get::<_,String>(1)?,"added_symbols":r.get::<_,f64>(2)?,"added_progress":r.get::<_,f64>(3)?,"payload":r.get::<_,String>(4)?,"position":r.get::<_,Option<i64>>(5)?,"extensions":r.get::<_,Option<String>>(6)?}))).map_err(sql)?.collect::<rusqlite::Result<Vec<_>>>().map_err(sql)?;
    let evidence = json!({"raw_entity":raw,"ordered_entry_rows":values});
    if evidence.to_string().len() > 64 * 1024 * 1024 {
        return Err("progress_resource_limit".into());
    }
    Ok(evidence)
}
pub fn source(db: &Connection, p: &str, s: Option<&str>) -> Result<Value> {
    let table = if s.is_some() { "stages" } else { "projects" };
    let id = s.unwrap_or(p);
    let (unit, raw): (String, String) = db
        .query_row(
            &format!("SELECT unit,payload_json FROM {table} WHERE id=?1"),
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(sql)?;
    if let Some(s) = s {
        let valid: bool = db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM stages WHERE id=?1 AND project_id=?2)",
                params![s, p],
                |r| r.get(0),
            )
            .map_err(sql)?;
        if !valid {
            return Err("progress_scope_mismatch".into());
        }
    }
    let payload = parse(&raw)?;
    let mut q=db.prepare("SELECT e.payload_json,x.payload_json,e.id,e.created_at,e.added_symbols FROM progress_entries e JOIN progress_order o ON o.entry_id=e.id LEFT JOIN project_extensions x ON x.entity_type='progress' AND x.entity_id=e.id WHERE e.project_id=?1 AND e.stage_id IS ?2 ORDER BY o.position").map_err(sql)?;
    let raw_entries = q
        .query_map(params![p, s], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, f64>(4)?,
            ))
        })
        .map_err(sql)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(sql)?;
    if raw_entries.len() > codec::MAX_CHAIN {
        return Err("progress_resource_limit".into());
    }
    let row_count: usize = db
        .query_row(
            "SELECT count(*) FROM progress_entries WHERE project_id=?1 AND stage_id IS ?2",
            params![p, s],
            |r| r.get(0),
        )
        .map_err(sql)?;
    if row_count != raw_entries.len() {
        return Err("progress_order_incomplete".into());
    }
    let mut entries = vec![];
    for (raw, extension, id, created, added) in raw_entries {
        let v = parse(&raw)?;
        if v["id"].as_str() != Some(&id)
            || v["created_at"].as_str() != Some(&created)
            || v["added_symbols"]
                .as_f64()
                .map(amount)
                .transpose()?
                .as_deref()
                != Some(&amount(added)?)
        {
            return Err("progress_source_columns_inconsistent".into());
        }
        if extension.as_deref().is_some_and(|s| s != "{}")
            || v.as_object()
                .ok_or("invalid_progress_source")?
                .keys()
                .any(|k| {
                    !matches!(
                        k.as_str(),
                        "id" | "new_total"
                            | "new_total_symbols"
                            | "added"
                            | "added_symbols"
                            | "added_progress"
                            | "created_at"
                            | "writing_day"
                            | "recorded_unit"
                            | "progress_fact"
                    )
                })
        {
            return Err("progress_unsupported_extension".into());
        }
        if let Some(f) = v.get("progress_fact") {
            let proven:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM cloud_progress_events e JOIN json_each(CAST(substr(e.canonical_frame,21) AS TEXT),'$.entries') j WHERE e.project_id=?1 AND e.stage_id IS ?2 AND e.state IN ('unsealed','sealed','applied','conflict_preserved') AND json(j.value)=json(?3))",params![p,s,f.to_string()],|r|r.get(0)).map_err(sql)?;
            if !proven {
                return Err("progress_unsupported_extension".into());
            }
        } else if v.get("recorded_unit").is_some() {
            return Err("progress_unsupported_extension".into());
        }
        let f = fact(&v, &unit)?;
        codec::validate_fact(&f)?;
        entries.push(f)
    }
    let total = amount(
        payload["total"].as_f64().ok_or("invalid_progress_source")? * codec::factor(&unit)? as f64,
    )?;
    let derived_root = s.is_none()
        && db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM stages WHERE project_id=?1)",
                [p],
                |r| r.get::<_, bool>(0),
            )
            .map_err(sql)?;
    let root_head: Option<(String, String)> = if derived_root {
        db.query_row("SELECT account_id,head_event_id FROM cloud_progress_projection WHERE project_id=?1 AND entity_id='project'",[p],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(sql)?
    } else {
        None
    };
    if derived_root && root_head.is_none() {
        return Err("progress_project_stage_history_ambiguous".into());
    }
    let base = if let Some((account, head)) = &root_head {
        codec::micros(&chain(db, account, p, "project", head)?.base_total)?
    } else if let Some(first) = entries.first() {
        codec::symbols(first)? - codec::delta_symbols(first)?
    } else {
        codec::micros(&total)?
    };
    if base < 0 {
        return Err("progress_base_inconsistent".into());
    }
    let mut c = Chain {
        base_total: codec::decimal(base),
        entries: vec![],
        ids: HashSet::new(),
        migration: None,
    };
    let admitted:Option<(String,String)>=db.query_row("SELECT account_id,head_event_id FROM cloud_progress_projection WHERE project_id=?1 AND entity_id=?2",params![p,codec::scope_id(s)],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(sql)?;
    if let Some((account, head)) = admitted {
        let proven = chain(db, &account, p, &codec::scope_id(s), &head)?;
        if proven.entries != entries {
            return Err("progress_projection_rebuild_required".into());
        }
        c = proven;
    } else {
        for f in entries {
            c.append(&f, true)?
        }
    }

    // Current unit projection uses the actual engine's rounding, not equality of converted rows.
    let expected = display_fact(
        c.total()?,
        &unit,
        c.entries.last().map(|f| f.unit.as_str()).unwrap_or(&unit),
    )?;
    if root_head.is_none() && (expected - payload["total"].as_f64().unwrap()).abs() > 0.000001 {
        return Err("progress_base_inconsistent".into());
    }
    Ok(json!({"chain":c,"unit":unit,"entity_payload":payload}))
}
fn display(symbols: i128, unit: &str) -> Result<f64> {
    let n = symbols as f64 / 1_000_000.0 / codec::factor(unit)? as f64;
    Ok(match unit {
        "symbols" => n,
        "author_list" => (n * 10.0).round_ties_even() / 10.0,
        _ => n.ceil(),
    })
}
fn display_fact(symbols: i128, current: &str, recorded: &str) -> Result<f64> {
    if current == recorded {
        Ok(symbols as f64 / 1_000_000.0 / codec::factor(current)? as f64)
    } else {
        display(symbols, current)
    }
}
fn queue(db: &Connection, a: &str, e: &codec::Event) -> Result<()> {
    let h = &e.header;
    let frame = codec::encode(e)?;
    db.execute("INSERT INTO cloud_progress_events(account_id,event_id,project_id,entity_id,stage_id,canonical_frame,parents_json,revision,state) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,'unsealed')",params![a,h.event_id,h.project_id,h.entity_id,h.stage_id,frame,json!(h.parents).to_string(),h.revision]).map_err(sql)?;
    db.execute("INSERT INTO cloud_sync_outbox(event_id,account_id,device_id,project_id,entity_id,entity_type,operation,revision,updated_at,deleted_at,created_at,parent_event_id,local_ordinal,lifecycle) VALUES(?1,?2,?3,?4,?5,'progress','event',?6,?7,NULL,?7,?8,(SELECT COALESCE(MAX(local_ordinal),0)+1 FROM cloud_sync_outbox WHERE account_id=?2),'unsealed')",params![h.event_id,a,h.device_id,h.project_id,h.entity_id,h.revision,h.updated_at,h.parents.first()]).map_err(sql)?;
    Ok(())
}
pub fn capture(
    db: &mut Connection,
    scope: &metadata::MetadataScope,
    p: &str,
    now: &str,
) -> Result<()> {
    metadata::assert_runtime_scope(
        db,
        &scope.account_id,
        &scope.canonical_user_id,
        &scope.device_id,
    )
    .map_err(|_| "progress_scope_mismatch")?;
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(sql)?;
    let a = &scope.account_id;
    let bound:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_project_bindings WHERE account_id=?1 AND project_id=?2)",params![a,p],|r|r.get(0)).map_err(sql)?;
    if !bound {
        return Err("progress_local_only_project".into());
    }
    let stages = rows(
        &tx,
        "SELECT id FROM stages WHERE project_id=?1 ORDER BY id",
        [p],
    )?;
    let root_rows:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM progress_entries WHERE project_id=?1 AND stage_id IS NULL)",[p],|r|r.get(0)).map_err(sql)?;
    let mixed = !stages.is_empty() && root_rows;
    let scopes: Vec<Option<String>> = if stages.is_empty() {
        vec![None]
    } else {
        let mut scopes: Vec<_> = stages.into_iter().map(Some).collect();
        if mixed {
            scopes.push(None)
        }
        scopes
    };
    for s in scopes {
        let owner = codec::scope_id(s.as_deref());
        let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM cloud_progress_migrations WHERE account_id=?1 AND project_id=?2 AND entity_id=?3)",params![a,p,owner],|r|r.get(0)).map_err(sql)?;
        if exists {
            continue;
        }
        let candidate = metadata::new_event_id().map_err(|_| "progress_identity_unavailable")?;
        let src = if mixed && s.is_none() {
            Err("progress_project_stage_history_ambiguous".into())
        } else {
            source(&tx, p, s.as_deref())
        };
        let (snapshot, blocker) = match src {
            Ok(v) => (Some(v), None),
            Err(code) => (None, Some(code)),
        };
        // The raw aggregate and ordered relational rows remain immutable recovery evidence.
        let evidence = raw_evidence(&tx, p, s.as_deref())?;
        tx.execute(
            "INSERT INTO cloud_progress_candidates VALUES(?1,?2,?3,?4,?5,NULL,?6,?7,?8)",
            params![
                candidate,
                a,
                p,
                owner,
                s,
                evidence.to_string(),
                snapshot.as_ref().map(Value::to_string),
                blocker
            ],
        )
        .map_err(sql)?;
        tx.execute(
            "INSERT INTO cloud_progress_migrations VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![
                a,
                p,
                owner,
                s,
                if blocker.is_some() {
                    "blocked"
                } else {
                    "captured"
                },
                candidate,
                blocker
            ],
        )
        .map_err(sql)?;
        if let Some(v) = snapshot {
            let c: Chain = serde_json::from_value(v["chain"].clone())
                .map_err(|_| "invalid_progress_source")?;
            let h = match header(&tx, a, p, s.as_deref(), vec![], "genesis", now) {
                Ok(h) => h,
                Err(code) => {
                    exec(&tx,"UPDATE cloud_progress_migrations SET lifecycle='blocked',blocker=?1 WHERE account_id=?2 AND project_id=?3 AND entity_id=?4",params![code,a,p,owner])?;
                    continue;
                }
            };
            let count = c.entries.len();
            let final_total = codec::decimal(c.total()?);
            let e = codec::Event {
                version: 1,
                migration: Some(codec::MigrationProof {
                    entry_count: count,
                    final_total,
                }),
                header: h,
                base_total: Some(c.base_total),
                entries: c.entries.into_iter().take(256).collect(),
                selected_event_id: None,
                target_entry_id: None,
                rebased_from: vec![],
            };
            queue(&tx, a, &e)?;
            tx.execute(
                "INSERT INTO cloud_progress_capture_cursor VALUES(?1,?2,?3,?4,?5)",
                params![a, p, owner, e.entries.len(), e.header.event_id],
            )
            .map_err(sql)?;
            tx.execute("UPDATE cloud_progress_migrations SET lifecycle='publication_pending' WHERE account_id=?1 AND project_id=?2 AND entity_id=?3",params![a,p,owner]).map_err(sql)?;
        }
    }
    tx.commit().map_err(sql)?;
    Ok(())
}
fn exec(db: &Connection, q: &str, args: impl rusqlite::Params) -> Result<()> {
    db.execute(q, args).map_err(sql)?;
    Ok(())
}
pub fn continue_capture(db: &mut Connection, a: &str, now: &str) -> Result<usize> {
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(sql)?;
    let mut q=tx.prepare("SELECT m.project_id,m.entity_id,m.stage_id,c.snapshot_json,k.next_position,k.tail_event_id FROM cloud_progress_migrations m JOIN cloud_progress_candidates c ON c.candidate_id=m.candidate_id JOIN cloud_progress_capture_cursor k USING(account_id,project_id,entity_id) WHERE m.account_id=?1 AND m.lifecycle IN ('publication_pending','self_echo_pending') ORDER BY m.project_id,m.entity_id LIMIT 8").map_err(sql)?;
    let owners = q
        .query_map([a], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, usize>(4)?,
                r.get::<_, String>(5)?,
            ))
        })
        .map_err(sql)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(sql)?;
    drop(q);
    let mut count = 0;
    for (p, owner, s, snapshot, mut position, mut tail) in owners {
        let c: Chain = serde_json::from_value(parse(&snapshot)?["chain"].clone())
            .map_err(|_| "invalid_progress_source")?;
        let entries: Vec<_> = c
            .entries
            .iter()
            .skip(position)
            .take(256 - count)
            .cloned()
            .collect();
        if !entries.is_empty() {
            let h = header(&tx, a, &p, s.as_deref(), vec![tail.clone()], "migrate", now)?;
            let e = codec::Event {
                version: 1,
                migration: None,
                header: h,
                base_total: None,
                entries,
                selected_event_id: Some(tail),
                target_entry_id: None,
                rebased_from: vec![],
            };
            queue(&tx, a, &e)?;
            tail = e.header.event_id;
            position += e.entries.len();
            count += e.entries.len();
        }
        exec(&tx,"UPDATE cloud_progress_capture_cursor SET next_position=?1,tail_event_id=?2 WHERE account_id=?3 AND project_id=?4 AND entity_id=?5",params![position,tail,a,p,owner])?;
        if count >= 256 {
            break;
        }
    }
    tx.commit().map_err(sql)?;
    Ok(count)
}
pub fn pending(db: &Connection, a: &str, device: &str, sealed: bool) -> Result<Vec<Value>> {
    let mut q=db.prepare("SELECT e.canonical_frame,o.nonce,o.ciphertext FROM cloud_progress_events e JOIN cloud_sync_outbox b ON b.account_id=e.account_id AND b.event_id=e.event_id LEFT JOIN cloud_sync_event_objects o ON o.account_id=e.account_id AND o.event_id=e.event_id WHERE e.account_id=?1 AND b.device_id=?2 AND e.state IN ('unsealed','sealed') AND b.lifecycle IN ('unsealed','sealed','accepted') AND (?3=0 AND o.event_id IS NULL OR ?3=1 AND o.event_id IS NOT NULL) ORDER BY b.local_ordinal LIMIT 8").map_err(sql)?;
    let items = q
        .query_map(params![a, device, sealed], |r| {
            Ok((
                r.get::<_, Vec<u8>>(0)?,
                r.get::<_, Option<Vec<u8>>>(1)?,
                r.get::<_, Option<Vec<u8>>>(2)?,
            ))
        })
        .map_err(sql)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(sql)?;
    items.into_iter().map(|(frame,nonce,ciphertext)|Ok(json!({"event":codec::decode(&frame)?,"frame":frame,"nonce":nonce,"ciphertext":ciphertext}))).collect()
}
pub fn seal(
    db: &mut Connection,
    a: &str,
    device: &str,
    id: &str,
    frame: &[u8],
    nonce: &[u8],
    ciphertext: &[u8],
) -> Result<()> {
    let e = codec::decode(frame)?;
    if e.header.device_id != device
        || e.header.event_id != id
        || nonce.len() != 24
        || ciphertext.len() != frame.len() + 16
    {
        return Err("progress_scope_mismatch".into());
    }
    let compression_view =
        crate::frame_compression::canonical_view(frame).map_err(str::to_owned)?;
    let frame = compression_view.as_ref();
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(sql)?;
    let valid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM cloud_progress_events e JOIN cloud_sync_outbox o USING(account_id,event_id) JOIN cloud_account_bindings b ON b.local_account_id=e.account_id WHERE e.account_id=?1 AND e.event_id=?2 AND e.canonical_frame=?3 AND o.device_id=?4 AND b.canonical_user_id=?5)",params![a,id,frame,device,e.header.account_id],|r|r.get(0)).map_err(sql)?;
    if !valid {
        return Err("progress_scope_mismatch".into());
    }
    let old:Option<(Vec<u8>,Vec<u8>)>=tx.query_row("SELECT nonce,ciphertext FROM cloud_sync_event_objects WHERE account_id=?1 AND event_id=?2",params![a,id],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(sql)?;
    if let Some((n, c)) = old {
        if n != nonce || c != ciphertext {
            return Err("progress_exact_replay_mismatch".into());
        }
    } else {
        exec(&tx,"INSERT INTO cloud_sync_event_objects(account_id,event_id,crypto_version,aad_version,nonce,ciphertext,stored_at) VALUES(?1,?2,1,1,?3,?4,?5)",params![a,id,nonce,ciphertext,e.header.updated_at])?;
        exec(&tx,"UPDATE cloud_progress_events SET state='sealed' WHERE account_id=?1 AND event_id=?2 AND state='unsealed'",params![a,id])?;
        exec(&tx,"UPDATE cloud_sync_outbox SET lifecycle='sealed' WHERE account_id=?1 AND event_id=?2 AND lifecycle='unsealed'",params![a,id])?;
        exec(&tx,"UPDATE cloud_progress_migrations SET lifecycle='self_echo_pending' WHERE account_id=?1 AND project_id=?2 AND entity_id=?3 AND lifecycle='publication_pending'",params![a,e.header.project_id,e.header.entity_id])?;
    }
    tx.commit().map_err(sql)?;
    Ok(())
}
pub fn receipt(
    db: &mut Connection,
    a: &str,
    device: &str,
    id: &str,
    seq: i64,
    duplicate: bool,
    now: &str,
) -> Result<()> {
    if !(1..=9_007_199_254_740_991).contains(&seq) || !metadata::timestamp(now) {
        return Err("invalid_progress_receipt".into());
    }
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(sql)?;
    let valid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_outbox WHERE account_id=?1 AND device_id=?2 AND event_id=?3 AND entity_type='progress' AND lifecycle IN ('sealed','accepted'))",params![a,device,id],|r|r.get(0)).map_err(sql)?;
    if !valid {
        return Err("progress_scope_mismatch".into());
    }
    let old:Option<i64>=tx.query_row("SELECT server_sequence FROM cloud_sync_upload_receipts WHERE account_id=?1 AND event_id=?2",params![a,id],|r|r.get(0)).optional().map_err(sql)?;
    if old.is_some_and(|n| n != seq) {
        return Err("progress_exact_replay_mismatch".into());
    }
    if old.is_none() {
        exec(
            &tx,
            "INSERT INTO cloud_sync_upload_receipts VALUES(?1,?2,?3,?4,?5,?6)",
            params![a, id, device, seq, duplicate, now],
        )?
    }
    exec(
        &tx,
        "UPDATE cloud_sync_outbox SET lifecycle='accepted' WHERE account_id=?1 AND event_id=?2",
        params![a, id],
    )?;
    tx.commit().map_err(sql)?;
    Ok(())
}
fn projection(db: &Connection, p: &str, s: Option<&str>, c: &Chain) -> Result<(Value, Vec<Value>)> {
    let table = if s.is_some() { "stages" } else { "projects" };
    let (raw, unit, goal, infinite): (String, String, Option<f64>, bool) = db
        .query_row(
            &format!("SELECT payload_json,unit,goal,infinite FROM {table} WHERE id=?1"),
            [s.unwrap_or(p)],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .map_err(sql)?;
    let mut payload = parse(&raw)?;
    let mut total = display_fact(
        c.total()?,
        &unit,
        c.entries.last().map(|f| f.unit.as_str()).unwrap_or(&unit),
    )?;
    let goal_symbols = goal.unwrap_or(0.0) * codec::factor(&unit)? as f64;
    let mut entries = vec![];
    for f in &c.entries {
        let absolute = codec::symbols(f)?;
        let delta = codec::delta_symbols(f)? as f64 / 1_000_000.0;
        let created = if let Some(t) = &f.writing_time {
            t.clone()
        } else {
            f.occurred_at.clone().ok_or("invalid_progress_payload")?
        };
        entries.push(json!({"id":f.entry_id,"recorded_unit":f.unit,"progress_fact":f,"new_total":display_fact(absolute,&unit,&f.unit)?,"new_total_symbols":absolute as f64/1_000_000.0,"added":display_fact((delta*1_000_000.0)as i128,&unit,&f.unit)?,"added_symbols":delta,"added_progress":if goal_symbols>0.0{delta/goal_symbols*100.0}else{0.0},"created_at":created,"writing_day":f.writing_day}));
    }
    let day =
        crate::streaks::logical_writing_day(db).map_err(|_| "progress_writing_day_invalid")?;
    let today_symbols = c
        .entries
        .iter()
        .filter(|f| f.writing_day == day)
        .try_fold(0i128, |sum, f| {
            Ok::<_, String>(sum + codec::delta_symbols(f)?)
        })?;
    let stage_sum: Option<f64> = if s.is_none() {
        db.query_row("SELECT SUM(COALESCE(json_extract(payload_json,'$.total'),0)) FROM stages WHERE project_id=?1",[p],|r|r.get(0)).map_err(sql)?
    } else {
        None
    };
    if let Some(sum) = stage_sum {
        total = sum
    }
    payload["remaining"] = if infinite {
        Value::Null
    } else {
        json!((goal.unwrap_or(total) - total).max(0.0))
    };
    payload["added_today"] = if stage_sum.is_some() {
        json!(db.query_row("SELECT SUM(COALESCE(json_extract(payload_json,'$.added_today'),0)) FROM stages WHERE project_id=?1",[p],|r|r.get::<_,f64>(0)).map_err(sql)?)
    } else {
        json!(display(today_symbols, &unit)?)
    };
    payload["progress_entries"] = json!(entries);
    payload["total"] = json!(total);
    payload["progress"] = json!(if !infinite && goal.unwrap_or(0.0) > 0.0 {
        total / goal.unwrap() * 100.0
    } else {
        0.0
    });
    Ok((payload, entries))
}
fn write_projection(
    tx: &rusqlite::Transaction<'_>,
    a: &str,
    e: &codec::Event,
    c: &Chain,
) -> Result<()> {
    let h = &e.header;
    let (payload, entries) = projection(tx, &h.project_id, h.stage_id.as_deref(), c)?;
    exec(
        tx,
        "INSERT OR IGNORE INTO cloud_progress_write_intents VALUES(?1,?2,?3,?4,?5,?6,?7)",
        params![
            a,
            h.event_id,
            h.project_id,
            h.entity_id,
            h.stage_id,
            payload.to_string(),
            json!(entries).to_string()
        ],
    )?;
    exec(tx,"INSERT INTO cloud_progress_projection VALUES(?1,?2,?3,?4,?5) ON CONFLICT(account_id,project_id,entity_id) DO UPDATE SET head_event_id=excluded.head_event_id,snapshot_json=excluded.snapshot_json",params![a,h.project_id,h.entity_id,h.event_id,json!(c).to_string()])?;
    exec(
        tx,
        "DELETE FROM progress_entries WHERE project_id=?1 AND stage_id IS ?2",
        params![h.project_id, h.stage_id],
    )?;
    // Compact only gaps, preserving every unaffected scope's relative sequence.
    crate::sqlite::compact_progress_order(tx).map_err(sql)?;
    let mut pos: i64 = tx
        .query_row(
            "SELECT COALESCE(MAX(position),-1)+1 FROM progress_order",
            [],
            |r| r.get(0),
        )
        .map_err(sql)?;
    for entry in &entries {
        exec(
            tx,
            "INSERT INTO progress_entries VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![
                entry["id"].as_str(),
                h.project_id,
                h.stage_id,
                entry["created_at"].as_str(),
                entry["added_symbols"].as_f64(),
                entry["added_progress"].as_f64(),
                entry.to_string()
            ],
        )?;
        exec(
            tx,
            "INSERT INTO progress_order VALUES(?1,?2)",
            params![entry["id"].as_str(), pos],
        )?;
        pos += 1
    }
    let table = if h.stage_id.is_some() {
        "stages"
    } else {
        "projects"
    };
    exec(
        tx,
        &format!("UPDATE {table} SET payload_json=?1 WHERE id=?2"),
        params![
            payload.to_string(),
            h.stage_id.as_deref().unwrap_or(&h.project_id)
        ],
    )?;
    if h.stage_id.is_some() {
        crate::refresh_project_totals_in_transaction(tx, &h.project_id)?
    }
    exec(tx,"INSERT INTO cloud_progress_projection VALUES(?1,?2,?3,?4,?5) ON CONFLICT(account_id,project_id,entity_id) DO UPDATE SET head_event_id=excluded.head_event_id,snapshot_json=excluded.snapshot_json",params![a,h.project_id,h.entity_id,h.event_id,json!(c).to_string()])?;
    Ok(())
}
fn covered(
    db: &Connection,
    a: &str,
    p: &str,
    owner: &str,
    descendant: &str,
    ancestor: &str,
) -> Result<bool> {
    let mut pending = vec![descendant.to_string()];
    let mut seen = HashSet::new();
    while let Some(id) = pending.pop() {
        if id == ancestor {
            return Ok(true);
        }
        if !seen.insert(id.clone()) {
            continue;
        }
        if seen.len() > codec::MAX_CHAIN {
            return Err("progress_resource_limit".into());
        }
        pending.extend(event(db, a, p, owner, &id)?.header.parents)
    }
    Ok(false)
}
pub fn apply(
    db: &mut crate::sqlite::PrivilegedRemoteApplyConnection,
    scope: &metadata::MetadataScope,
    frame: &[u8],
    nonce: &[u8],
    ciphertext: &[u8],
) -> Result<String> {
    let compression_view =
        crate::frame_compression::canonical_view(frame).map_err(str::to_owned)?;
    let frame = compression_view.as_ref();
    let e = codec::decode(frame)?;
    let h = &e.header;
    let a = &scope.account_id;
    db.execute_planned_many_once(|tx|->ApplyResult<(Vec<crate::sqlite::OwnedRemoteApplyAuthorization>,Option<(i64,String,Chain,Vec<String>,bool,String)>)>{
        metadata::assert_runtime_scope(tx,a,&scope.canonical_user_id,&scope.device_id).map_err(|_|"progress_scope_mismatch")?;
        if h.account_id!=scope.canonical_user_id{return Err("progress_scope_mismatch".into())}
        let seq:Option<i64>=tx.query_row("SELECT i.server_sequence FROM cloud_sync_inbox i JOIN cloud_sync_event_objects o USING(account_id,event_id) WHERE i.account_id=?1 AND i.event_id=?2 AND i.project_id=?3 AND i.entity_id=?4 AND i.entity_type='progress' AND i.operation='event' AND i.device_id=?5 AND i.sync_revision=?6 AND i.updated_at=?7 AND i.deleted_at IS NULL AND o.crypto_version=1 AND o.aad_version=1 AND o.nonce=?8 AND o.ciphertext=?9",params![a,h.event_id,h.project_id,h.entity_id,h.device_id,h.revision,h.updated_at,nonce,ciphertext],|r|r.get(0)).optional().map_err(sql)?;let seq=seq.ok_or("progress_scope_mismatch")?;
        let prior:Option<Vec<u8>>=tx.query_row("SELECT canonical_frame FROM cloud_progress_events WHERE account_id=?1 AND event_id=?2",params![a,h.event_id],|r|r.get(0)).optional().map_err(sql)?;
        if prior.as_ref().is_some_and(|f|f!=frame){return Err("progress_exact_replay_mismatch".into())}
        let applied:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM cloud_progress_apply_ledger WHERE account_id=?1 AND event_id=?2 AND server_sequence=?3 AND nonce=?4 AND ciphertext=?5)",params![a,h.event_id,seq,nonce,ciphertext],|r|r.get(0)).map_err(sql)?;if applied{return Ok((vec![],None))}
        if prior.is_none(){exec(tx,"INSERT INTO cloud_progress_events(account_id,event_id,project_id,entity_id,stage_id,canonical_frame,parents_json,revision,state,server_sequence) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,'waiting',?9)",params![a,h.event_id,h.project_id,h.entity_id,h.stage_id,frame,json!(h.parents).to_string(),h.revision,seq])?}else{exec(tx,"UPDATE cloud_progress_events SET server_sequence=?1 WHERE account_id=?2 AND event_id=?3",params![seq,a,h.event_id])?}
        let ready=(||->Result<Chain>{dependencies(tx,a,&scope.canonical_user_id,&e)?;let mut parents=HashMap::new();let mut revision=0;
            for id in &h.parents{let valid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM cloud_progress_events WHERE account_id=?1 AND event_id=?2 AND project_id=?3 AND entity_id=?4 AND state IN ('applied','conflict_preserved'))",params![a,id,h.project_id,h.entity_id],|r|r.get(0)).map_err(sql)?;if !valid{return Err("progress_parent_unknown".into())}let pe=event(tx,a,&h.project_id,&h.entity_id,id)?;dependencies(tx,a,&scope.canonical_user_id,&pe)?;revision=revision.max(pe.header.revision);parents.insert(id.clone(),chain(tx,a,&h.project_id,&h.entity_id,id)?);}
            if h.revision!=revision+1{return Err("progress_parent_revision_invalid".into())}transition(&e,parents)
        })();
        let c=match ready{Ok(c)=>c,Err(code)=>{exec(tx,"UPDATE cloud_progress_events SET state='waiting',blocker=?1 WHERE account_id=?2 AND event_id=?3",params![code,a,h.event_id])?;exec(tx,"UPDATE cloud_sync_inbox SET state='orphan',error_code=?1 WHERE account_id=?2 AND event_id=?3",params![code,a,h.event_id])?;return Ok((vec![],None))}};
        let ids=json!(c.entries.iter().map(|f|&f.entry_id).collect::<Vec<_>>()).to_string();
        let collision:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM progress_entries e JOIN json_each(?1) j ON j.value=e.id WHERE e.project_id<>?2 OR e.stage_id IS NOT ?3)",params![ids,h.project_id,h.stage_id],|r|r.get(0)).map_err(sql)?;
        if collision{let code="progress_entry_scope_collision";exec(tx,"UPDATE cloud_progress_events SET state='waiting',blocker=?1 WHERE account_id=?2 AND event_id=?3",params![code,a,h.event_id])?;exec(tx,"UPDATE cloud_sync_inbox SET state='orphan',error_code=?1 WHERE account_id=?2 AND event_id=?3",params![code,a,h.event_id])?;return Ok((vec![],None))}
        let own:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_outbox WHERE account_id=?1 AND event_id=?2 AND device_id=?3 AND lifecycle IN ('sealed','accepted'))",params![a,h.event_id,scope.device_id],|r|r.get(0)).map_err(sql)?;
        let old=tips(tx,a,&h.project_id,h.stage_id.as_deref())?;
        let mut new_tips=vec![];for id in &old{if id==&h.event_id||!covered(tx,a,&h.project_id,&h.entity_id,&h.event_id,id)?{new_tips.push(id.clone())}}
        // A local descendant already proves this self echo; never rewind its selected chain.
        let mut descended=false;for id in &old{descended|=covered(tx,a,&h.project_id,&h.entity_id,id,&h.event_id)?;}if !descended{new_tips.push(h.event_id.clone())}new_tips.sort();new_tips.dedup();
        let current=source(tx,&h.project_id,h.stage_id.as_deref());
        let selected:Option<String>=tx.query_row("SELECT snapshot_json FROM cloud_progress_projection WHERE account_id=?1 AND project_id=?2 AND entity_id=?3",params![a,h.project_id,h.entity_id],|r|r.get(0)).optional().map_err(sql)?;
        let unchanged=match (&selected,&current){(Some(raw),Ok(v))=>parse(raw)?==v["chain"],(None,Ok(v))=>v["chain"]["entries"].as_array().is_some_and(Vec::is_empty)&&v["chain"]["base_total"]=="0.000000",_=>false};
        let capture:Option<(usize,String,String)>=tx.query_row("SELECT k.next_position,k.tail_event_id,c.snapshot_json FROM cloud_progress_capture_cursor k JOIN cloud_progress_migrations m USING(account_id,project_id,entity_id) JOIN cloud_progress_candidates c ON c.candidate_id=m.candidate_id WHERE k.account_id=?1 AND k.project_id=?2 AND k.entity_id=?3",params![a,h.project_id,h.entity_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(sql)?;
        let migration_event=match &capture{Some((_,tail,_))=>covered(tx,a,&h.project_id,&h.entity_id,tail,&h.event_id)?,None=>false};
        let capture_last=match &capture{Some((position,tail,snapshot))=>*tail==h.event_id&&parse(snapshot)?["chain"]["entries"].as_array().is_some_and(|v|v.len()==*position),None=>false};
        let capture_matches=match (&capture,&current){(Some((_,_,snapshot)),Ok(local))=>parse(snapshot)?["chain"]==local["chain"],_=>true};
        let conflict=new_tips.len()>1||!own&&!unchanged||own&&migration_event&&!capture_matches;
        if !unchanged&&!own{let evidence=match current{Ok(v)=>v,Err(code)=>json!({"blocker":code,"source":raw_evidence(tx,&h.project_id,h.stage_id.as_deref())?})};exec(tx,"INSERT OR IGNORE INTO cloud_progress_local_candidates VALUES(?1,?2,?3,?4,?5)",params![a,h.event_id,h.project_id,h.entity_id,evidence.to_string()])?}
        let outcome=if conflict{"conflict_preserved"}else{"applied"}.to_string();
        let write=!conflict&&!descended&&c.migration.is_none()&&(!own||!migration_event||capture_last);let auth=if write{let(payload,entries)=projection(tx,&h.project_id,h.stage_id.as_deref(),&c)?;vec![crate::sqlite::OwnedRemoteApplyAuthorization{event_id:h.event_id.clone(),account_id:a.clone(),project_id:h.project_id.clone(),entity_id:h.entity_id.clone(),operation:"upsert".into(),payload_json:Some(json!({"entity_payload":payload,"entries":entries}).to_string()),prior_payload_json:None}]}else{vec![]};
        let lifecycle=if conflict{"conflict"}else if c.migration.is_some()||migration_event&&!capture_last{"self_echo_pending"}else{"active"}.to_string();
        Ok((auth,Some((seq,outcome,c,new_tips,write,lifecycle))))
    },|tx,plan|->ApplyResult<String>{let Some((seq,outcome,c,new_tips,write,lifecycle))=plan else{return Ok(tx.query_row("SELECT outcome FROM cloud_progress_apply_ledger WHERE account_id=?1 AND event_id=?2",params![a,h.event_id],|r|r.get(0)).optional().map_err(sql)?.unwrap_or("waiting".into()))};
        if write{write_projection(tx,a,&e,&c)?;exec(tx,"DELETE FROM cloud_sync_remote_apply_authorizations WHERE event_id=?1",[&h.event_id])?}
        exec(tx,"DELETE FROM cloud_progress_tips WHERE account_id=?1 AND project_id=?2 AND entity_id=?3",params![a,h.project_id,h.entity_id])?;for id in new_tips{exec(tx,"INSERT INTO cloud_progress_tips VALUES(?1,?2,?3,?4)",params![a,h.project_id,h.entity_id,id])?}
        exec(tx,"UPDATE cloud_progress_events SET state=?1,blocker=NULL WHERE account_id=?2 AND event_id=?3",params![outcome,a,h.event_id])?;
        exec(tx,"INSERT INTO cloud_progress_apply_ledger VALUES(?1,?2,?3,?4,?5,?6)",params![a,h.event_id,seq,outcome,nonce,ciphertext])?;
        exec(tx,"UPDATE cloud_sync_inbox SET state=?1,applied_at=?2,error_code=NULL WHERE account_id=?3 AND event_id=?4",params![if outcome=="applied"{"applied"}else{"conflict"},h.updated_at,a,h.event_id])?;
        let _cursor:Option<(usize,String)>=tx.query_row("SELECT next_position,tail_event_id FROM cloud_progress_capture_cursor WHERE account_id=?1 AND project_id=?2 AND entity_id=?3",params![a,h.project_id,h.entity_id],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(sql)?;
        let state=lifecycle;
        exec(tx,"INSERT INTO cloud_progress_migrations VALUES(?1,?2,?3,?4,?5,?6,NULL) ON CONFLICT(account_id,project_id,entity_id) DO UPDATE SET lifecycle=excluded.lifecycle,blocker=NULL",params![a,h.project_id,h.entity_id,h.stage_id,state,h.event_id])?;
        let own:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_outbox WHERE account_id=?1 AND event_id=?2 AND device_id=?3 AND lifecycle IN ('sealed','accepted'))",params![a,h.event_id,scope.device_id],|r|r.get(0)).map_err(sql)?;
        if own{let old:Option<i64>=tx.query_row("SELECT server_sequence FROM cloud_sync_upload_receipts WHERE account_id=?1 AND event_id=?2",params![a,h.event_id],|r|r.get(0)).optional().map_err(sql)?;if old.is_some_and(|n|n!=seq){return Err("progress_exact_replay_mismatch".into())}if old.is_none(){exec(tx,"INSERT INTO cloud_sync_upload_receipts VALUES(?1,?2,?3,?4,1,?5)",params![a,h.event_id,h.device_id,seq,h.updated_at])?}exec(tx,"UPDATE cloud_sync_outbox SET lifecycle='accepted' WHERE account_id=?1 AND event_id=?2",params![a,h.event_id])?;}
        Ok(outcome)
    }).map_err(|error:ApplyError|error.0)
}
pub fn normal(
    tx: &rusqlite::Transaction<'_>,
    p: &str,
    s: Option<&str>,
    entry: &Value,
    expected: Option<&[String]>,
    now: &str,
) -> Result<bool> {
    let owner = codec::scope_id(s);
    let migration:Option<(String,String)>=tx.query_row("SELECT account_id,lifecycle FROM cloud_progress_migrations WHERE project_id=?1 AND entity_id=?2",params![p,owner],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(sql)?;
    let Some((a, state)) = migration else {
        return Ok(false);
    };
    if state != "active" {
        return Err(if state == "conflict" {
            "progress_histories_diverged"
        } else {
            "progress_publication_pending"
        }
        .into());
    }
    let parents = tips(tx, &a, p, s)?;
    if parents.len() != 1 {
        return Err("progress_histories_diverged".into());
    }
    if expected != Some(parents.as_slice()) {
        return Err("progress_expected_head_stale".into());
    }
    let mut c = chain(tx, &a, p, &owner, &parents[0])?;
    let current = source(tx, p, s)?;
    if current["chain"] != json!(c) {
        return Err("progress_projection_rebuild_required".into());
    }
    let unit = current["unit"].as_str().ok_or("invalid_progress_source")?;
    if amount(
        entry["new_total"]
            .as_f64()
            .ok_or("invalid_progress_source")?
            * codec::factor(unit)? as f64,
    )? != amount(
        entry["new_total_symbols"]
            .as_f64()
            .ok_or("invalid_progress_source")?,
    )? {
        return Err("progress_unit_changed".into());
    }
    let mut f = fact(entry, unit)?;
    if let Some(time) = &f.occurred_at {
        let local: String = tx
            .query_row(
                "SELECT strftime('%Y-%m-%dT%H:%M:%S',?1,'localtime')",
                [time],
                |r| r.get(0),
            )
            .map_err(sql)?;
        let setting: Option<String> = tx
            .query_row(
                "SELECT value_json FROM settings WHERE key='start_day_time'",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(sql)?;
        let start = setting
            .map(|v| serde_json::from_str::<String>(&v).map_err(|_| "progress_writing_day_invalid"))
            .transpose()?
            .unwrap_or_else(|| "00:00:00".into());
        f.writing_day = crate::streaks::logical_writing_day_from(&local, &start)
            .ok_or("progress_writing_day_invalid")?;
    }
    // Absolute new total is the user's action. Delta derives from the proven canonical base.
    f.unit = current["unit"]
        .as_str()
        .ok_or("invalid_progress_source")?
        .into();
    let base = append_base(&c, &f)?;
    f.delta = codec::decimal(codec::symbols(&f)? - base);
    c.append_at_base(&f, false, base)?;
    let h = header(
        tx,
        &a,
        p,
        s,
        parents.clone(),
        "append",
        &crate::document_sync::timestamp(now).map_err(|_| "progress_timestamp_invalid")?,
    )?;
    let e = codec::Event {
        version: 1,
        migration: None,
        header: h,
        base_total: None,
        entries: vec![f],
        selected_event_id: Some(parents[0].clone()),
        target_entry_id: None,
        rebased_from: vec![],
    };
    queue(tx, &a, &e)?;
    exec(
        tx,
        "DELETE FROM cloud_progress_tips WHERE account_id=?1 AND project_id=?2 AND entity_id=?3",
        params![a, p, owner],
    )?;
    exec(
        tx,
        "INSERT INTO cloud_progress_tips VALUES(?1,?2,?3,?4)",
        params![a, p, owner, e.header.event_id],
    )?;
    write_projection(tx, &a, &e, &c)?;
    Ok(true)
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Decision {
    pub project_id: String,
    pub stage_id: Option<String>,
    pub expected_tips: Vec<String>,
    pub expected_local: Value,
    pub selected_event_id: String,
    pub operation: String,
    pub target_entry_id: Option<String>,
    pub rebased_from: Vec<String>,
    pub corrected_delta: Option<String>,
}
pub fn decide(
    db: &mut Connection,
    scope: &metadata::MetadataScope,
    d: &Decision,
    now: &str,
) -> Result<String> {
    metadata::assert_runtime_scope(
        db,
        &scope.account_id,
        &scope.canonical_user_id,
        &scope.device_id,
    )
    .map_err(|_| "progress_scope_mismatch")?;
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(sql)?;
    let a = &scope.account_id;
    let owner = codec::scope_id(d.stage_id.as_deref());
    let parents = tips(&tx, a, &d.project_id, d.stage_id.as_deref())?;
    let eid = metadata::new_event_id().map_err(|_| "progress_identity_unavailable")?;
    exec(
        &tx,
        "INSERT INTO cloud_progress_decisions VALUES(?1,?2,?3,?4)",
        params![
            a,
            eid,
            json!(d.expected_tips).to_string(),
            d.expected_local.to_string()
        ],
    )?;
    if parents != d.expected_tips
        || source(&tx, &d.project_id, d.stage_id.as_deref())? != d.expected_local
        || d.operation != "adopt_local" && !parents.contains(&d.selected_event_id)
    {
        tx.commit().map_err(sql)?;
        return Err("progress_resolution_stale".into());
    }
    let pending:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM cloud_progress_events e JOIN cloud_sync_outbox o USING(account_id,event_id) WHERE e.account_id=?1 AND e.project_id=?2 AND e.entity_id=?3 AND e.state IN ('unsealed','sealed') AND json_extract(CAST(substr(e.canonical_frame,21) AS TEXT),'$.header.operation') IN ('select','rebase','correct','tombstone','adopt_local'))",params![a,d.project_id,owner],|r|r.get(0)).map_err(sql)?;
    if pending {
        tx.commit().map_err(sql)?;
        return Err("progress_publication_pending".into());
    }
    let mut chains = HashMap::new();
    for id in &parents {
        chains.insert(id.clone(), chain(&tx, a, &d.project_id, &owner, id)?);
    }
    if d.operation == "adopt_local" {
        let snapshot = source(&tx, &d.project_id, d.stage_id.as_deref())?;
        let c: Chain = serde_json::from_value(snapshot["chain"].clone())
            .map_err(|_| "invalid_progress_source")?;
        let mut h = header(
            &tx,
            a,
            &d.project_id,
            d.stage_id.as_deref(),
            parents,
            "adopt_local",
            now,
        )?;
        h.event_id = eid.clone();
        let e = codec::Event {
            version: 1,
            header: h,
            migration: Some(codec::MigrationProof {
                entry_count: c.entries.len(),
                final_total: codec::decimal(c.total()?),
            }),
            base_total: Some(c.base_total),
            entries: c.entries.into_iter().take(256).collect(),
            selected_event_id: None,
            target_entry_id: None,
            rebased_from: vec![],
        };
        transition(&e, chains)?;
        queue(&tx, a, &e)?;
        exec(
            &tx,
            "INSERT INTO cloud_progress_candidates VALUES(?1,?2,?3,?4,?5,?1,?6,?7,NULL)",
            params![
                eid,
                a,
                d.project_id,
                owner,
                d.stage_id,
                snapshot.to_string(),
                snapshot.to_string()
            ],
        )?;
        exec(&tx,"INSERT INTO cloud_progress_capture_cursor VALUES(?1,?2,?3,?4,?5) ON CONFLICT(account_id,project_id,entity_id) DO UPDATE SET next_position=excluded.next_position,tail_event_id=excluded.tail_event_id",params![a,d.project_id,owner,e.entries.len(),eid])?;
        exec(&tx,"UPDATE cloud_progress_migrations SET candidate_id=?1,lifecycle='self_echo_pending' WHERE account_id=?2 AND project_id=?3 AND entity_id=?4",params![eid,a,d.project_id,owner])?;
        tx.commit().map_err(sql)?;
        return Ok(eid);
    }
    let selected = chains
        .get(&d.selected_event_id)
        .ok_or("progress_resolution_invalid")?;
    let all: HashMap<_, _> = chains
        .values()
        .flat_map(|c| c.entries.iter())
        .map(|f| (f.entry_id.clone(), f.clone()))
        .collect();
    let mut total = if matches!(d.operation.as_str(), "correct" | "tombstone") {
        let pos = selected
            .entries
            .iter()
            .position(|f| Some(&f.entry_id) == d.target_entry_id.as_ref())
            .ok_or("progress_target_unknown")?;
        if pos == 0 {
            codec::micros(&selected.base_total)?
        } else {
            codec::symbols(&selected.entries[pos - 1])?
        }
    } else {
        selected.total()?
    };
    let mut entries = vec![];
    for (i, id) in d.rebased_from.iter().enumerate() {
        let original = all.get(id).ok_or("progress_rebase_source_unknown")?;
        let mut f = original.clone();
        f.entry_id = metadata::new_event_id().map_err(|_| "progress_identity_unavailable")?;
        if i == 0 && d.operation == "correct" {
            f.delta = d
                .corrected_delta
                .clone()
                .ok_or("progress_correction_invalid")?
        } else if d.corrected_delta.is_some() && d.operation != "correct" {
            return Err("progress_correction_invalid".into());
        }
        total = total
            .checked_add(codec::delta_symbols(&f)?)
            .ok_or("progress_resource_limit")?;
        f.new_total = codec::decimal(total);
        entries.push(f);
    }
    let mut h = header(
        &tx,
        a,
        &d.project_id,
        d.stage_id.as_deref(),
        parents,
        &d.operation,
        now,
    )?;
    h.event_id = eid.clone();
    let e = codec::Event {
        version: 1,
        migration: None,
        header: h,
        base_total: None,
        entries,
        selected_event_id: Some(d.selected_event_id.clone()),
        target_entry_id: d.target_entry_id.clone(),
        rebased_from: d.rebased_from.clone(),
    };
    let c = transition(&e, chains)?;
    queue(&tx, a, &e)?;
    // Resolution takes effect only after its authenticated self echo.
    let _ = c;
    tx.commit().map_err(sql)?;
    Ok(eid)
}
pub fn view(db: &Connection, scope: &metadata::MetadataScope, p: &str) -> Result<Value> {
    metadata::assert_runtime_scope(
        db,
        &scope.account_id,
        &scope.canonical_user_id,
        &scope.device_id,
    )
    .map_err(|_| "progress_scope_mismatch")?;
    let mut stages = rows(
        db,
        "SELECT id FROM stages WHERE project_id=?1 ORDER BY id",
        [p],
    )?;
    let root:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM cloud_progress_migrations WHERE account_id=?1 AND project_id=?2 AND stage_id IS NULL)",params![scope.account_id,p],|r|r.get(0)).map_err(sql)?;
    let owners: Vec<_> = if stages.is_empty() {
        vec![None]
    } else {
        let mut v: Vec<_> = stages.drain(..).map(Some).collect();
        if root {
            v.push(None)
        }
        v
    };
    let mut views = vec![];
    for s in owners {
        let owner = codec::scope_id(s.as_deref());
        let tips = tips(db, &scope.account_id, p, s.as_deref())?;
        let mut state:Option<(String,Option<String>)>=db.query_row("SELECT lifecycle,blocker FROM cloud_progress_migrations WHERE account_id=?1 AND project_id=?2 AND entity_id=?3",params![scope.account_id,p,owner],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(sql)?;
        let waiting:Option<String>=db.query_row("SELECT blocker FROM cloud_progress_events WHERE account_id=?1 AND project_id=?2 AND entity_id=?3 AND state='waiting' AND blocker IS NOT NULL ORDER BY server_sequence LIMIT 1",params![scope.account_id,p,owner],|r|r.get(0)).optional().map_err(sql)?;
        if let Some(code) = waiting {
            if let Some((_, blocker)) = &mut state {
                *blocker = Some(code)
            } else {
                state = Some(("blocked".into(), Some(code)))
            }
        }
        let mut versions = vec![];
        for tip in &tips {
            versions.push(json!({"event_id":tip,"chain":chain(db,&scope.account_id,p,&owner,tip)?}))
        }
        views.push(json!({"entity_id":owner,"stage_id":s,"state":state.as_ref().map(|r|r.0.as_str()).unwrap_or("local"),"blocker":state.and_then(|r|r.1),"tips":tips,"versions":versions,"local":source(db,p,s.as_deref()).unwrap_or_else(|code|json!({"blocker":code}))}));
    }
    Ok(json!({"owners":views}))
}
pub fn ack_proven(db: &Connection, a: &str, seq: i64) -> rusqlite::Result<bool> {
    db.query_row("SELECT EXISTS(SELECT 1 FROM cloud_progress_apply_ledger l JOIN cloud_progress_events e USING(account_id,event_id) JOIN cloud_sync_inbox i USING(account_id,event_id) JOIN cloud_sync_event_objects o USING(account_id,event_id) WHERE l.account_id=?1 AND l.server_sequence=?2 AND e.server_sequence=l.server_sequence AND i.server_sequence=l.server_sequence AND e.state=l.outcome AND i.state=CASE WHEN l.outcome='applied' THEN 'applied' ELSE 'conflict' END AND i.entity_type='progress' AND i.operation='event' AND i.project_id=e.project_id AND i.entity_id=e.entity_id AND i.sync_revision=e.revision AND i.device_id=json_extract(CAST(substr(e.canonical_frame,21) AS TEXT),'$.header.device_id') AND i.updated_at=json_extract(CAST(substr(e.canonical_frame,21) AS TEXT),'$.header.updated_at') AND l.nonce=o.nonce AND l.ciphertext=o.ciphertext AND o.crypto_version=1 AND o.aad_version=1 AND i.deleted_at IS NULL)",params![a,seq],|r|r.get(0))
}
#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
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
    Rebuild {
        project_id: String,
        stage_id: Option<String>,
    },
}
pub fn received(db: &Connection, a: &str, after: i64, limit: i64) -> Result<Vec<Value>> {
    if after < 0 || !(1..=32).contains(&limit) {
        return Err("invalid_progress_page".into());
    }
    let tx = db.unchecked_transaction().map_err(sql)?;
    let mut q=tx.prepare("SELECT i.event_id,i.server_sequence,i.device_id,i.project_id,i.entity_id,i.sync_revision,i.updated_at,o.nonce,o.ciphertext FROM cloud_sync_inbox i JOIN cloud_sync_event_objects o USING(account_id,event_id) LEFT JOIN cloud_game_reader_visits v ON v.account_id=i.account_id AND v.event_id=i.event_id WHERE i.account_id=?1 AND i.entity_type='progress' AND i.operation='event' AND i.state IN ('received','orphan') AND i.server_sequence>?2 ORDER BY COALESCE(v.ordinal,0),i.server_sequence LIMIT ?3").map_err(sql)?;
    let mut rows=q.query_map(params![a,after,limit],|r|Ok(json!({"event_id":r.get::<_,String>(0)?,"server_sequence":r.get::<_,i64>(1)?,"source_device_id":r.get::<_,String>(2)?,"project_id":r.get::<_,String>(3)?,"entity_id":r.get::<_,String>(4)?,"revision":r.get::<_,i64>(5)?,"updated_at":r.get::<_,String>(6)?,"nonce":r.get::<_,Vec<u8>>(7)?,"ciphertext":r.get::<_,Vec<u8>>(8)?}))).map_err(sql)?.collect::<rusqlite::Result<Vec<_>>>().map_err(sql)?;
    drop(q);
    rows.sort_by_key(|r| r["server_sequence"].as_i64().unwrap_or(0));
    crate::sqlite::record_sync_reader_visits(
        &tx,
        a,
        rows.iter().filter_map(|r| r["event_id"].as_str()),
    )
    .map_err(sql)?;
    tx.commit().map_err(sql)?;
    Ok(rows)
}
pub fn rebuild(
    db: &mut crate::sqlite::PrivilegedRemoteApplyConnection,
    scope: &metadata::MetadataScope,
    p: &str,
    s: Option<&str>,
) -> Result<()> {
    db.execute_planned_many_once(|tx|->ApplyResult<_>{
        metadata::assert_runtime_scope(tx,&scope.account_id,&scope.canonical_user_id,&scope.device_id).map_err(|_|"progress_scope_mismatch")?;
        let a=&scope.account_id;let ids=tips(tx,a,p,s)?;
        if ids.len()!=1{return Err("progress_histories_diverged".into())}
        let e=event(tx,a,p,&codec::scope_id(s),&ids[0])?;dependencies(tx,a,&scope.canonical_user_id,&e)?;
        let accepted:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM cloud_progress_apply_ledger WHERE account_id=?1 AND event_id=?2 AND outcome='applied')",params![a,ids[0]],|r|r.get(0))?;
        if !accepted{return Err("progress_publication_pending".into())}
        let c=chain(tx,a,p,&codec::scope_id(s),&ids[0])?;let(payload,entries)=projection(tx,p,s,&c)?;
        if let Ok(current)=source(tx,p,s){
            let materialized=rows(tx,"SELECT e.payload_json FROM progress_entries e JOIN progress_order o ON o.entry_id=e.id WHERE e.project_id=?1 AND e.stage_id IS ?2 ORDER BY o.position",params![p,s])?.into_iter().map(|r|parse(&r)).collect::<Result<Vec<_>>>()?;
            if current["chain"]==json!(c)&&current["entity_payload"]==payload&&materialized==entries{return Ok((vec![],None))}
        }

        let auth=crate::sqlite::OwnedRemoteApplyAuthorization{event_id:e.header.event_id.clone(),account_id:a.clone(),project_id:p.into(),entity_id:codec::scope_id(s),operation:"upsert".into(),payload_json:Some(json!({"entity_payload":payload,"entries":entries}).to_string()),prior_payload_json:None};
        Ok((vec![auth],Some((e,c))))
    },|tx,plan|->ApplyResult<()>{let Some((e,c))=plan else{return Ok(())};write_projection(tx,&scope.account_id,&e,&c)?;exec(tx,"DELETE FROM cloud_sync_remote_apply_authorizations WHERE event_id=?1",[&e.header.event_id])?;Ok(())}).map_err(|e:ApplyError|e.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqlite;
    const A: &str = "map-test-account";
    const NOW: &str = "2026-10-03T00:00:00.000000Z";
    fn fixture() -> codec::Event {
        let f: Value = serde_json::from_str(include_str!(
            "../../src/cloud/__fixtures__/progressCodecV1.json"
        ))
        .unwrap();
        serde_json::from_value(f["examples"][0]["event"].clone()).unwrap()
    }
    fn scope() -> metadata::MetadataScope {
        let h = fixture().header;
        metadata::MetadataScope {
            account_id: A.into(),
            canonical_user_id: h.account_id,
            device_id: h.device_id,
        }
    }
    fn seed() -> (Connection, std::path::PathBuf) {
        let path =
            std::env::temp_dir().join(format!("c18505-{}.db", metadata::new_event_id().unwrap()));
        let mut db = sqlite::open_database(&path).unwrap();
        db.execute("INSERT INTO mirror_state(id,source_format,source_schema_version,sync_status) VALUES(1,'test','1','healthy')",[]).unwrap();
        db.execute(
            "UPDATE storage_ownership SET owner='sqlite' WHERE subsystem IN ('notes','projects')",
            [],
        )
        .unwrap();
        let h = fixture().header;
        db.execute("INSERT INTO projects(id,name,infinite,unit,status,payload_json) VALUES('P1','Project',1,'symbols','active','{\"total\":0,\"progress_entries\":[]}')",[]).unwrap();
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
        (db, path)
    }
    fn incoming(path: &std::path::Path, e: &codec::Event, seq: i64) -> String {
        let mut db = crate::sqlite::open_database(path).unwrap();
        let h = &e.header;
        let frame = codec::encode(e).unwrap();
        let ciphertext = vec![2u8; frame.len() + 16];
        let local: bool = db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM cloud_sync_outbox WHERE event_id=?1)",
                [&h.event_id],
                |r| r.get(0),
            )
            .unwrap();
        if local {
            seal(
                &mut db,
                A,
                &scope().device_id,
                &h.event_id,
                &frame,
                &[1; 24],
                &ciphertext,
            )
            .unwrap();
        }
        db.execute("INSERT OR IGNORE INTO cloud_sync_inbox(account_id,event_id,server_sequence,device_id,project_id,entity_id,entity_type,operation,sync_revision,updated_at,state,received_at) VALUES(?1,?2,?3,?4,?5,?6,'progress','event',?7,?8,'received',?8)",params![A,h.event_id,seq,h.device_id,h.project_id,h.entity_id,h.revision,h.updated_at]).unwrap();
        db.execute(
            "INSERT OR IGNORE INTO cloud_sync_event_objects VALUES(?1,?2,1,1,?3,?4,?5)",
            params![A, h.event_id, vec![1u8; 24], ciphertext, NOW],
        )
        .unwrap();
        drop(db);
        let mut db = crate::sqlite::open_privileged_remote_apply_database(path).unwrap();
        apply(&mut db, &scope(), &frame, &[1; 24], &ciphertext).unwrap()
    }
    fn append(parent: &codec::Event, total: &str, delta: &str) -> codec::Event {
        let mut e = parent.clone();
        e.header.event_id = metadata::new_event_id().unwrap();
        e.header.parents = vec![parent.header.event_id.clone()];
        e.header.operation = "append".into();
        e.header.revision += 1;
        e.header.generation = e.header.revision;
        e.base_total = None;
        e.migration = None;
        e.selected_event_id = Some(parent.header.event_id.clone());
        e.rebased_from.clear();
        e.target_entry_id = None;
        let mut f = fixture().entries[0].clone();
        f.entry_id = metadata::new_event_id().unwrap();
        f.new_total = total.into();
        f.delta = delta.into();
        e.entries = vec![f];
        e
    }
    #[test]
    fn progress_divergence_rebase_descendant_safety_and_projection_rebuild() {
        let (db, path) = seed();
        drop(db);
        let mut root = fixture();
        root.entries.clear();
        root.base_total = Some("100.000000".into());
        root.migration = Some(codec::MigrationProof {
            entry_count: 0,
            final_total: "100.000000".into(),
        });
        assert_eq!(incoming(&path, &root, 2), "applied");
        let a = append(&root, "120.000000", "20.000000");
        let b = append(&root, "130.000000", "30.000000");
        let b2 = append(&b, "140.000000", "10.000000");
        assert_eq!(incoming(&path, &a, 3), "applied");
        {
            let db = crate::sqlite::open_database(&path).unwrap();
            db.execute("INSERT INTO projects(id,name,infinite,unit,status,payload_json) VALUES('P2','Other',1,'symbols','active','{}')",[]).unwrap();
            db.execute("INSERT INTO project_order VALUES('P2',1)", [])
                .unwrap();
            assert!(db
                .execute(
                    "UPDATE progress_entries SET project_id='P2' WHERE id=?1",
                    [&a.entries[0].entry_id]
                )
                .is_err());
            assert!(db
                .execute(
                    "UPDATE progress_order SET position=99 WHERE entry_id=?1",
                    [&a.entries[0].entry_id]
                )
                .is_err());
            assert!(db.execute("UPDATE projects SET payload_json=json_set(payload_json,'$.total',999) WHERE id='P1'",[]).is_err());
        }

        assert_eq!(incoming(&path, &b, 4), "conflict_preserved");
        assert_eq!(incoming(&path, &b2, 5), "conflict_preserved");
        let mut db = crate::sqlite::open_database(&path).unwrap();
        assert_eq!(tips(&db, A, "P1", None).unwrap().len(), 2);
        let local = source(&db, "P1", None).unwrap();
        assert_eq!(local["entity_payload"]["total"], 120.0);
        let d = Decision {
            project_id: "P1".into(),
            stage_id: None,
            expected_tips: tips(&db, A, "P1", None).unwrap(),
            expected_local: local,
            selected_event_id: a.header.event_id.clone(),
            operation: "rebase".into(),
            target_entry_id: None,
            rebased_from: vec![
                b.entries[0].entry_id.clone(),
                b2.entries[0].entry_id.clone(),
            ],
            corrected_delta: None,
        };
        let id = decide(&mut db, &scope(), &d, NOW).unwrap();
        let chosen = event(&db, A, "P1", "project", &id).unwrap();
        assert_eq!(chosen.entries[0].new_total, "150.000000");
        assert_eq!(chosen.entries[1].new_total, "160.000000");
        assert_ne!(chosen.entries[0].entry_id, b.entries[0].entry_id);
        drop(db);
        assert_eq!(incoming(&path, &chosen, 6), "applied");
        assert_eq!(incoming(&path, &chosen, 6), "applied");
        let mut db = crate::sqlite::open_database(&path).unwrap();
        assert_eq!(
            source(&db, "P1", None).unwrap()["entity_payload"]["total"],
            160.0
        );
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM domain_events WHERE event_type='ProgressAdded'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        assert_eq!(
            chain(&db, A, "P1", "project", &b2.header.event_id)
                .unwrap()
                .total()
                .unwrap(),
            140_000_000
        );
        let mut delete = Decision {
            project_id: "P1".into(),
            stage_id: None,
            expected_tips: tips(&db, A, "P1", None).unwrap(),
            expected_local: source(&db, "P1", None).unwrap(),
            selected_event_id: chosen.header.event_id.clone(),
            operation: "tombstone".into(),
            target_entry_id: Some(a.entries[0].entry_id.clone()),
            rebased_from: vec![],
            corrected_delta: None,
        };
        assert_eq!(
            decide(&mut db, &scope(), &delete, NOW).unwrap_err(),
            "progress_descendant_rebase_required"
        );
        delete.rebased_from = chosen.entries.iter().map(|f| f.entry_id.clone()).collect();
        let id = decide(&mut db, &scope(), &delete, NOW).unwrap();
        let repair = event(&db, A, "P1", "project", &id).unwrap();
        drop(db);
        assert_eq!(incoming(&path, &repair, 7), "applied");
        let mut db = crate::sqlite::open_database(&path).unwrap();
        assert_eq!(
            source(&db, "P1", None).unwrap()["entity_payload"]["total"],
            140.0
        );
        db.execute_batch("DROP TRIGGER projects_progress_projection_guard")
            .unwrap();
        db.execute(
            "UPDATE projects SET payload_json=json_set(payload_json,'$.total',999) WHERE id='P1'",
            [],
        )
        .unwrap();
        drop(db);
        let mut privileged = sqlite::open_privileged_remote_apply_database(&path).unwrap();
        rebuild(&mut privileged, &scope(), "P1", None).unwrap();
        drop(privileged);
        let mut db = sqlite::open_database(&path).unwrap();
        assert_eq!(
            source(&db, "P1", None).unwrap()["entity_payload"]["total"],
            140.0
        );
        assert!(decide(&mut db, &scope(), &d, NOW).is_err());
        assert_eq!(
            db.query_row("SELECT count(*) FROM cloud_progress_events", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            6
        );
        assert!(ack_proven(&db, A, 7).unwrap());
        let mut correction = Decision {
            project_id: "P1".into(),
            stage_id: None,
            expected_tips: tips(&db, A, "P1", None).unwrap(),
            expected_local: source(&db, "P1", None).unwrap(),
            selected_event_id: repair.header.event_id.clone(),
            operation: "correct".into(),
            target_entry_id: Some(repair.entries[0].entry_id.clone()),
            rebased_from: vec![],
            corrected_delta: Some("35.000000".into()),
        };
        assert_eq!(
            decide(&mut db, &scope(), &correction, NOW).unwrap_err(),
            "progress_descendant_rebase_required"
        );
        correction.rebased_from = repair.entries.iter().map(|f| f.entry_id.clone()).collect();
        let corrected_id = decide(&mut db, &scope(), &correction, NOW).unwrap();
        let corrected = event(&db, A, "P1", "project", &corrected_id).unwrap();
        assert_ne!(corrected.entries[0].entry_id, repair.entries[0].entry_id);
        assert_eq!(corrected.entries[1].delta, repair.entries[1].delta);
        drop(db);
        assert_eq!(incoming(&path, &corrected, 8), "applied");
        let db = sqlite::open_database(&path).unwrap();
        assert_eq!(
            source(&db, "P1", None).unwrap()["entity_payload"]["total"],
            145.0
        );
        assert_eq!(
            chain(&db, A, "P1", "project", &repair.header.event_id)
                .unwrap()
                .total()
                .unwrap(),
            140_000_000
        );
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM domain_events WHERE event_type='ProgressAdded'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        assert!(ack_proven(&db, A, 8).unwrap());
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn progress_local_candidate_and_invalid_base_block_without_partial_apply() {
        let (mut db, path) = seed();
        db.execute("UPDATE projects SET payload_json='{\"total\":50,\"progress_entries\":[]}' WHERE id='P1'",[]).unwrap();
        drop(db);
        let root = fixture();
        assert_eq!(incoming(&path, &root, 2), "conflict_preserved");
        let db = crate::sqlite::open_database(&path).unwrap();
        assert_eq!(
            source(&db, "P1", None).unwrap()["entity_payload"]["total"],
            50.0
        );
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM cloud_progress_local_candidates",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        drop(db);
        let invalid = append(&root, "150.000000", "1.000000");
        assert_eq!(incoming(&path, &invalid, 3), "waiting");
        let db = crate::sqlite::open_database(&path).unwrap();
        assert!(!ack_proven(&db, A, 3).unwrap());
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM cloud_progress_apply_ledger",
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
    fn progress_explicit_capture_batches_reopen_and_local_adoption() {
        let (mut db, path) = seed();
        assert!(pending(&db, A, &scope().device_id, false)
            .unwrap()
            .is_empty());
        let mut source_rows = vec![];
        for i in 1..=600 {
            let entry = json!({"id":metadata::new_event_id().unwrap(),"new_total":i,"new_total_symbols":i,"added":1,"added_symbols":1,"added_progress":0,"created_at":"2026-09-01T03:00:00"});
            db.execute(
                "INSERT INTO progress_entries VALUES(?1,'P1',NULL,?2,1,0,?3)",
                params![
                    entry["id"].as_str(),
                    entry["created_at"].as_str(),
                    entry.to_string()
                ],
            )
            .unwrap();
            db.execute(
                "INSERT INTO progress_order VALUES(?1,?2)",
                params![entry["id"].as_str(), i - 1],
            )
            .unwrap();
            source_rows.push(entry);
        }
        db.execute(
            "UPDATE projects SET payload_json=?1",
            [json!({"total":600,"progress_entries":source_rows}).to_string()],
        )
        .unwrap();
        capture(&mut db, &scope(), "P1", NOW).unwrap();
        drop(db);
        let mut db = sqlite::open_database(&path).unwrap();
        assert_eq!(continue_capture(&mut db, A, NOW).unwrap(), 256);
        assert_eq!(continue_capture(&mut db, A, NOW).unwrap(), 88);
        assert_eq!(continue_capture(&mut db, A, NOW).unwrap(), 0);
        let items = pending(&db, A, &scope().device_id, false).unwrap();
        assert_eq!(items.len(), 3);
        drop(db);
        for (i, item) in items.iter().enumerate() {
            let e: codec::Event = serde_json::from_value(item["event"].clone()).unwrap();
            assert_eq!(incoming(&path, &e, i as i64 + 2), "applied");
        }
        let db = sqlite::open_database(&path).unwrap();
        let v = view(&db, &scope(), "P1").unwrap();
        assert_eq!(v["owners"][0]["state"], "active");
        assert_eq!(
            source(&db, "P1", None).unwrap()["chain"]["entries"]
                .as_array()
                .unwrap()
                .len(),
            600
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM cloud_sync_upload_receipts", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            3
        );
        drop(db);
        std::fs::remove_file(path).unwrap();
        let (mut db, path) = seed();
        db.execute(
            "UPDATE projects SET payload_json='{\"total\":50,\"progress_entries\":[]}'",
            [],
        )
        .unwrap();
        drop(db);
        assert_eq!(incoming(&path, &fixture(), 2), "conflict_preserved");
        let mut db = sqlite::open_database(&path).unwrap();
        let d = Decision {
            project_id: "P1".into(),
            stage_id: None,
            expected_tips: tips(&db, A, "P1", None).unwrap(),
            expected_local: source(&db, "P1", None).unwrap(),
            selected_event_id: "local".into(),
            operation: "adopt_local".into(),
            target_entry_id: None,
            rebased_from: vec![],
            corrected_delta: None,
        };
        let id = decide(&mut db, &scope(), &d, NOW).unwrap();
        let chosen = event(&db, A, "P1", "project", &id).unwrap();
        drop(db);
        assert_eq!(incoming(&path, &chosen, 3), "applied");
        let db = sqlite::open_database(&path).unwrap();
        assert_eq!(
            source(&db, "P1", None).unwrap()["entity_payload"]["total"],
            50.0
        );
        assert_eq!(
            view(&db, &scope(), "P1").unwrap()["owners"][0]["state"],
            "active"
        );
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn progress_unit_transition_preserves_visible_action_and_old_facts() {
        let mut root = fixture();
        root.base_total = Some("11000.000000".into());
        root.entries[0].new_total = "13000.000000".into();
        root.entries[0].delta = "2000.000000".into();
        root.migration = Some(codec::MigrationProof {
            entry_count: 1,
            final_total: "13000.000000".into(),
        });
        let c = transition(&root, HashMap::new()).unwrap();
        let original = c.entries.clone();
        let mut e = append(&root, "16200.000000", "1800.000000");
        e.entries[0].unit = "A4".into();
        let mut parents = HashMap::new();
        parents.insert(root.header.event_id.clone(), c.clone());
        let projected = transition(&e, parents).unwrap();
        assert_eq!(
            display_fact(c.total().unwrap(), "A4", "symbols").unwrap(),
            8.0
        );
        assert_eq!(projected.total().unwrap(), 16200_000000);
        assert_eq!(projected.entries[0], original[0]);
        assert_eq!(projected.entries[1].delta, "1800.000000");
        let mut e = append(&root, "16000.000000", "4000.000000");
        e.entries[0].unit = "author_list".into();
        let mut parents = HashMap::new();
        parents.insert(root.header.event_id.clone(), c);
        assert_eq!(
            transition(&e, parents).unwrap().entries[1].delta,
            "4000.000000"
        );
        let (mut db, path) = seed();
        db.execute("UPDATE projects SET unit='author_list',payload_json='{\"total\":0.3,\"progress_entries\":[]}'",[]).unwrap();
        let mut f = original[0].clone();
        f.occurred_at = None;
        f.writing_time = Some("2026-09-01T02:30:00".into());
        f.writing_day = "2026-09-01".into();
        let raw = json!({"id":f.entry_id,"new_total":0.3,"new_total_symbols":13000,"added":0.1,"added_symbols":2000,"added_progress":0,"created_at":f.writing_time});
        db.execute(
            "INSERT INTO progress_entries VALUES(?1,'P1',NULL,?2,2000,0,?3)",
            params![f.entry_id, f.writing_time, raw.to_string()],
        )
        .unwrap();
        db.execute("INSERT INTO progress_order VALUES(?1,0)", [&f.entry_id])
            .unwrap();
        let source = source(&db, "P1", None).unwrap();
        assert_eq!(source["chain"]["base_total"], "11000.000000");
        assert_eq!(
            source["chain"]["entries"][0]["writing_time"],
            "2026-09-01T02:30:00"
        );
        assert!(source["chain"]["entries"][0]["occurred_at"].is_null());
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
}
