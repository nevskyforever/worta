//! Positive local domain-event capture. Remote Progress never calls this writer.
use crate::{
    document_codec::canonical, game_codec, game_migration, game_projection, game_sync,
    progress_codec, project_metadata_sync as metadata,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
type Result<T> = std::result::Result<T, String>;
fn sql(_: rusqlite::Error) -> String {
    "game_storage_error".into()
}
fn scope(db: &Connection, p: &str) -> Result<Option<metadata::MetadataScope>> {
    db.query_row("SELECT b.account_id,a.canonical_user_id,s.device_id FROM cloud_sync_project_bindings b JOIN cloud_account_bindings a ON a.local_account_id=b.account_id JOIN cloud_sync_state s ON s.account_id=b.account_id WHERE b.project_id=?1",[p],|r|Ok(metadata::MetadataScope{account_id:r.get(0)?,canonical_user_id:r.get(1)?,device_id:r.get(2)?})).optional().map_err(sql)
}
/// A local completion may precede its metadata self echo. Freeze the exact
/// already-queued structural action; readers still require its authenticated
/// receipt before applying Game. No mutable status alone authorizes a reward.
fn completion_header(
    db: &Connection,
    scope: &metadata::MetadataScope,
    p: &str,
    s: Option<&str>,
    id: &str,
    parents: &[String],
    now: &str,
) -> Result<Value> {
    let a = &scope.account_id;
    let view =
        metadata::authority_view(db, a, p).map_err(|_| "game_project_authority_unresolved")?;
    let metadata_id = if view.state == "active" {
        view.head_event_id
            .ok_or("game_project_authority_unresolved")?
    } else if view.state == "resolution_pending" && view.branches.len() == 1 && s.is_none() {
        let pending = view
            .pending_event_id
            .ok_or("game_project_authority_unresolved")?;
        let valid:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_metadata_events e JOIN cloud_sync_outbox o USING(account_id,event_id) JOIN cloud_sync_metadata_decisions d USING(account_id,event_id) WHERE e.account_id=?1 AND e.event_id=?2 AND e.project_id=?3 AND e.device_id=?4 AND e.operation='update' AND e.state IN ('unsealed','sealed','accepted') AND d.state='pending' AND d.kind='edit' AND e.payload_json=d.proposed_json AND json_extract(e.payload_json,'$.status')='завершен' AND e.parent_event_ids_json=?5 AND o.entity_type='project_metadata' AND o.operation='upsert')",params![a,pending,p,scope.device_id,canonical(&json!([view.branches[0].event_id]))],|r|r.get(0)).map_err(sql)?;
        if !valid
            || view
                .local
                .as_ref()
                .is_none_or(|v| v["status"] != "завершен")
        {
            return Err("game_project_authority_unresolved".into());
        }
        pending
    } else {
        return Err("game_project_authority_unresolved".into());
    };
    let mut refs = Vec::new();
    if let Some(s) = s {
        let mut q=db.prepare("SELECT event_id FROM cloud_sync_structural_tips WHERE account_id=?1 AND project_id=?2 AND entity_type='stage' AND entity_id=?3 ORDER BY event_id").map_err(sql)?;
        refs = q
            .query_map(params![a, p, s], |r| r.get::<_, String>(0))
            .map_err(sql)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sql)?;
        crate::stage_sync::content_reference_ready(db, a, p, s, &refs)
            .map_err(|_| "game_stage_authority_unresolved")?;
        let mut q=db.prepare("SELECT e.canonical_frame FROM cloud_sync_structural_events e JOIN cloud_sync_outbox o USING(account_id,event_id) WHERE e.account_id=?1 AND e.project_id=?2 AND e.entity_type='stage' AND e.entity_id=?3 AND e.state IN ('unsealed','sealed') AND o.device_id=?4 LIMIT 2").map_err(sql)?;
        let pending = q
            .query_map(params![a, p, s, scope.device_id], |r| {
                r.get::<_, Vec<u8>>(0)
            })
            .map_err(sql)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sql)?;
        if pending.len() > 1 {
            return Err("game_stage_authority_unresolved".into());
        }
        if let Some(frame) = pending.first() {
            let e =
                crate::stage_sync::unframe(frame).map_err(|_| "game_stage_authority_unresolved")?;
            if e.header.operation != "update"
                || e.header.parent_event_ids != refs
                || e.stage["status"] != "завершен"
                || e.header.account_id != scope.canonical_user_id
            {
                return Err("game_stage_authority_unresolved".into());
            }
            refs = vec![e.header.event_id];
        }
    }
    let boot:String=db.query_row("SELECT bootstrap_id FROM cloud_sync_project_bootstraps WHERE account_id=?1 AND project_id=?2",params![a,p],|r|r.get(0)).map_err(|_|"game_project_authority_unresolved")?;
    let bound:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_project_bindings WHERE account_id=?1 AND project_id=?2)",params![a,p],|r|r.get(0)).map_err(sql)?;
    if !bound {
        return Err("game_scope_mismatch".into());
    }
    let mut h = game_migration::header(db, scope, None, None, id, parents, "native-game-v1", now)?;
    let o = h.as_object_mut().ok_or("invalid_game_payload")?;
    o.remove("entity_type");
    o.insert("scope".into(), json!("project"));
    o.insert("project_id".into(), json!(p));
    o.insert("stage_id".into(), json!(s));
    o.insert("metadata_event_id".into(), json!(metadata_id));
    o.insert("bootstrap_id".into(), json!(boot));
    o.insert("stage_event_ids".into(), json!(refs));
    o.insert(
        "entity_id".into(),
        json!(s
            .map(|s| format!("game:stage:{s}:{id}"))
            .unwrap_or_else(|| format!("game:project:{id}"))),
    );
    Ok(h)
}
/// Standalone account action; the parent Game chain is the source of truth.
/// Caller commits the returned compatibility state in the same transaction.
pub fn capture_loss(
    db: &Connection,
    before: &Value,
    day: &str,
    now: &str,
) -> Result<Option<Value>> {
    let mut q = db.prepare("SELECT m.account_id,b.canonical_user_id,s.device_id,m.lifecycle FROM cloud_game_migrations m JOIN cloud_account_bindings b ON b.local_account_id=m.account_id JOIN cloud_sync_state s ON s.account_id=m.account_id WHERE m.owner_key='account' LIMIT 2").map_err(sql)?;
    let scopes = q
        .query_map([], |r| {
            Ok((
                metadata::MetadataScope {
                    account_id: r.get(0)?,
                    canonical_user_id: r.get(1)?,
                    device_id: r.get(2)?,
                },
                r.get::<_, String>(3)?,
            ))
        })
        .map_err(sql)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(sql)?;
    if scopes.is_empty() {
        return Ok(None);
    }
    if scopes.len() != 1 || scopes[0].1 != "active" {
        return Err("game_noncommutative_conflict".into());
    }
    let scope = &scopes[0].0;
    metadata::assert_runtime_scope(
        db,
        &scope.account_id,
        &scope.canonical_user_id,
        &scope.device_id,
    )
    .map_err(|_| "game_scope_mismatch")?;
    let base = game_projection::account_base(before)?;
    let parents = game_sync::tips(db, &scope.account_id, "account")?;
    if parents.len() != 1 {
        return Err("game_noncommutative_conflict".into());
    }
    let proven = game_sync::rebuild_chain(db, &scope.account_id, "account", &parents[0])?;
    if proven != base {
        return Err("game_legacy_local_conflict".into());
    }
    let after = game_codec::loss_after(&base["global_streak"], day)?;
    let id = game_sync::loss_id(&scope.canonical_user_id, &base["global_streak"]);
    let event = json!({"version":2,"header":game_migration::header(db,scope,None,None,&id,&parents,"native-game-v1",now)?,"action":{"kind":"global_streak","writing_day":day,"project_action_ids":[],"before":base["global_streak"],"after":after,"reward":{"coins":"0.000000","experience":"0.000000"}}});
    // Validate before retaining anything. No optimistic state can outrun replay.
    let next = game_sync::transition(
        &event,
        &std::collections::BTreeMap::from([(parents[0].clone(), base)]),
    )?;
    let mut out = game_projection::materialize_account(before, &next)?;
    out["gamer"]["last_health_recovery_at"] = json!({"__type__":"datetime","value":now});
    crate::game::append_loss_notifications(before, &mut out, &id, now)?;
    game_sync::preserve(db, &scope.account_id, &event, "unsealed")?;
    if game_sync::rebuild_chain(db, &scope.account_id, "account", &id)? != next {
        return Err("game_invalid_rule".into());
    }
    db.execute(
        "DELETE FROM cloud_game_tips WHERE account_id=?1 AND owner_key='account'",
        [&scope.account_id],
    )
    .map_err(sql)?;
    db.execute(
        "INSERT INTO cloud_game_tips VALUES(?1,'account',?2)",
        params![scope.account_id, id],
    )
    .map_err(sql)?;
    db.execute("UPDATE cloud_game_projection SET head_event_id=?1,snapshot_json=?2,generation=?3 WHERE account_id=?4 AND owner_key='account'",params![id,canonical(&next),event["header"]["revision"].as_i64(),scope.account_id]).map_err(sql)?;
    db.execute("INSERT INTO cloud_game_write_intents(account_id,event_id,payload_json,prior_payload_json) VALUES(?1,?2,?3,?4)",params![scope.account_id,id,out.to_string(),before.to_string()]).map_err(sql)?;
    Ok(Some(out))
}
#[derive(Clone)]
pub struct InventoryIntent {
    pub operation: String,
    pub category: String,
    pub item_id: String,
    pub count: i64,
}
pub fn capture_inventory(
    db: &Connection,
    intent: &InventoryIntent,
    before_raw: &str,
    after: &Value,
    result: Option<&Value>,
    now: &str,
) -> Result<bool> {
    if !matches!(intent.operation.as_str(), "buy" | "sell") {
        return Ok(false);
    }
    let mut q=db.prepare("SELECT m.account_id,b.canonical_user_id,s.device_id FROM cloud_game_migrations m JOIN cloud_account_bindings b ON b.local_account_id=m.account_id JOIN cloud_sync_state s ON s.account_id=m.account_id WHERE m.owner_key='account' AND m.lifecycle='active' LIMIT 2").map_err(sql)?;
    let scopes = q
        .query_map([], |r| {
            Ok(metadata::MetadataScope {
                account_id: r.get(0)?,
                canonical_user_id: r.get(1)?,
                device_id: r.get(2)?,
            })
        })
        .map_err(sql)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(sql)?;
    if scopes.len() != 1 {
        return Ok(false);
    }
    let scope = &scopes[0];
    let a = &scope.account_id;
    db.execute_batch("SAVEPOINT game_inventory_writer")
        .map_err(sql)?;
    let outcome = (|| -> Result<()> {
        let before: Value =
            serde_json::from_str(before_raw).map_err(|_| "game_unsupported_local_mutation")?;
        let base = game_projection::account_base(&before)
            .map_err(|_| "game_unsupported_local_mutation")?;
        let raw:String=db.query_row("SELECT snapshot_json FROM cloud_game_projection WHERE account_id=?1 AND owner_key='account'",[a],|r|r.get(0)).map_err(sql)?;
        if serde_json::from_str::<Value>(&raw).map_err(|_| "invalid_game_payload")? != base {
            return Err("game_legacy_local_conflict".into());
        }
        let next =
            game_projection::account_base(after).map_err(|_| "game_unsupported_local_mutation")?;
        let count = |v: &Value| {
            v["inventory"]
                .as_array()
                .and_then(|rows| {
                    rows.iter().find(|row| {
                        row["category"] == intent.category && row["item_id"] == intent.item_id
                    })
                })
                .and_then(|v| v["count"].as_i64())
                .unwrap_or(0)
        };
        let details = result.ok_or("game_invalid_rule")?;
        if details["category"] != intent.category
            || details["item_key"] != intent.item_id
            || details["count"].as_i64() != Some(intent.count)
        {
            return Err("game_invalid_rule".into());
        }
        let price = details["unit_price"].as_f64().ok_or("game_invalid_rule")?;
        let price =
            progress_codec::micros(&format!("{price:.6}")).map_err(|_| "game_invalid_rule")?;
        let delta = progress_codec::micros(next["coins"].as_str().ok_or("game_invalid_rule")?)
            .map_err(|_| "game_invalid_rule")?
            - progress_codec::micros(base["coins"].as_str().ok_or("game_invalid_rule")?)
                .map_err(|_| "game_invalid_rule")?;
        let parents = game_sync::tips(db, a, "account")?;
        if parents.len() != 1 {
            return Err("game_noncommutative_conflict".into());
        }
        let id = metadata::new_event_id().map_err(|_| "game_storage_error")?;
        let e = json!({"version":1,"header":game_migration::header(db,scope,None,None,&id,&parents,"native-game-v1",now)?,"action":{"kind":"inventory","operation":intent.operation,"category":intent.category,"item_id":intent.item_id,"count":intent.count,"unit_price":progress_codec::decimal(price),"before_count":count(&base),"after_count":count(&next),"coins_delta":progress_codec::decimal(delta)}});
        game_sync::preserve(db, a, &e, "unsealed")?;
        let snapshot = game_sync::rebuild_chain(db, a, "account", &id)?;
        if snapshot != next {
            return Err("game_unsupported_local_mutation".into());
        }
        db.execute(
            "DELETE FROM cloud_game_tips WHERE account_id=?1 AND owner_key='account'",
            [a],
        )
        .map_err(sql)?;
        db.execute(
            "INSERT INTO cloud_game_tips VALUES(?1,'account',?2)",
            params![a, id],
        )
        .map_err(sql)?;
        db.execute("UPDATE cloud_game_projection SET head_event_id=?1,snapshot_json=?2,generation=?3 WHERE account_id=?4 AND owner_key='account'",params![id,canonical(&snapshot),e["header"]["revision"].as_i64(),a]).map_err(sql)?;
        db.execute("INSERT INTO cloud_game_write_intents(account_id,event_id,payload_json,prior_payload_json) VALUES(?1,?2,?3,?4)",params![a,id,after.to_string(),before_raw]).map_err(sql)?;
        Ok(())
    })();
    if outcome.is_err() {
        db.execute_batch("ROLLBACK TO game_inventory_writer")
            .map_err(sql)?;
    }
    db.execute_batch("RELEASE game_inventory_writer")
        .map_err(sql)?;
    match outcome {
        Ok(()) => Ok(true),
        Err(code) if code == "game_storage_error" => Err(code),
        Err(code) => {
            db.execute("INSERT INTO cloud_game_blockers VALUES(?1,'account','',?2) ON CONFLICT(account_id,owner_key,event_id) DO UPDATE SET code=excluded.code",params![a,code]).map_err(sql)?;
            db.execute("UPDATE cloud_game_migrations SET lifecycle='blocked',blocker=?1 WHERE account_id=?2",params![code,a]).map_err(sql)?;
            Ok(false)
        }
    }
}
pub fn already_recorded(
    db: &Connection,
    source: &str,
    p: &str,
    stage: Option<&str>,
    entry: Option<&str>,
    delta: f64,
) -> Result<bool> {
    let Some(scope) = scope(db, p)? else {
        return Ok(false);
    };
    db.query_row("SELECT EXISTS(SELECT 1 FROM cloud_game_sources s JOIN cloud_game_events g ON g.account_id=s.account_id AND g.event_id=s.project_action_id JOIN cloud_game_events r ON r.account_id=s.account_id AND r.event_id=s.reward_action_id WHERE s.account_id=?1 AND g.project_id=?3 AND r.scope='account' AND (s.source_key=?2 OR (?4 IS NOT NULL AND g.stage_id IS ?5 AND json_extract(CAST(substr(g.canonical_frame,21) AS TEXT),'$.action.kind')='writing' AND json_extract(CAST(substr(g.canonical_frame,21) AS TEXT),'$.action.fact.entry_id')=?4 AND json_extract(CAST(substr(g.canonical_frame,21) AS TEXT),'$.action.fact.delta')=?6)))",params![scope.account_id,source,p,entry,stage,format!("{delta:.6}")],|r|r.get(0)).map_err(sql)
}
/// Returns false for a local-only/unmigrated owner. An active owner with an
/// unsupported effect retains its exact local change and blocks completeness.
pub fn capture_domain(
    db: &Connection,
    source_key: &str,
    kind: &str,
    p: &str,
    s: Option<&str>,
    entry: Option<&str>,
    delta: f64,
    before_raw: &str,
    after: &Value,
    context: &serde_json::Map<String, Value>,
    now: &str,
) -> Result<bool> {
    let Some(scope) = scope(db, p)? else {
        return Ok(false);
    };
    let a = &scope.account_id;
    let key = canonical(&json!([p, s]));
    let active:bool=db.query_row("SELECT count(*)=2 FROM cloud_game_migrations WHERE account_id=?1 AND owner_key IN ('account',?2) AND lifecycle='active'",params![a,key],|r|r.get(0)).map_err(sql)?;
    if !active {
        return Ok(false);
    };
    if !(kind == "ProgressAdded" && delta > 0.0
        || matches!(kind, "ProjectCompleted" | "StageCompleted"))
    {
        return Ok(false);
    };
    if matches!(kind, "ProjectCompleted" | "StageCompleted")
        && serde_json::from_str::<Value>(before_raw).ok().as_ref() == Some(after)
    {
        return Ok(false); // A genesis claim is historical evidence, never another reward.
    }
    db.execute_batch("SAVEPOINT game_ordinary_writer")
        .map_err(sql)?;
    let result = (|| -> Result<()> {
        let before: Value =
            serde_json::from_str(before_raw).map_err(|_| "game_unsupported_local_mutation")?;
        let account_base = game_projection::account_base(&before)
            .map_err(|_| "game_unsupported_local_mutation")?;
        let old_account: String=db.query_row("SELECT snapshot_json FROM cloud_game_projection WHERE account_id=?1 AND owner_key='account'",[a],|r|r.get(0)).map_err(sql)?;
        if serde_json::from_str::<Value>(&old_account).map_err(|_| "invalid_game_payload")?
            != account_base
        {
            return Err("game_legacy_local_conflict".into());
        }
        let parents = game_sync::tips(db, a, &key)?;
        let account_parents = game_sync::tips(db, a, "account")?;
        if parents.len() != 1 || account_parents.len() != 1 {
            return Err("game_noncommutative_conflict".into());
        }
        let g = if kind == "ProgressAdded" {
            let ep = progress_codec::scope_id(s);
            let entry = entry.ok_or("game_dependency_progress_missing")?;
            let mut query=db.prepare("SELECT e.canonical_frame FROM cloud_progress_events e JOIN cloud_sync_outbox o USING(account_id,event_id) WHERE e.account_id=?1 AND e.project_id=?2 AND e.entity_id=?3 AND o.entity_type='progress' AND o.operation='event' AND o.lifecycle IN ('unsealed','sealed','accepted') AND json_extract(CAST(substr(e.canonical_frame,21) AS TEXT),'$.header.operation')='append' AND json_extract(CAST(substr(e.canonical_frame,21) AS TEXT),'$.entries[0].entry_id')=?4 LIMIT 2").map_err(sql)?;
            let frames = query
                .query_map(params![a, p, ep, entry], |r| r.get::<_, Vec<u8>>(0))
                .map_err(sql)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(sql)?;
            if frames.len() != 1 {
                return Err("game_dependency_progress_missing".into());
            }
            let progress = progress_codec::decode(&frames[0])
                .map_err(|_| "game_dependency_progress_missing")?;
            if progress.header.account_id != scope.canonical_user_id
                || progress.header.stage_id.as_deref() != s
                || progress.entries.len() != 1
                || progress.entries[0].entry_id != entry
                || progress_codec::micros(&progress.entries[0].delta)
                    .map_err(|_| "invalid_game_payload")?
                    != progress_codec::micros(&format!("{delta:.6}"))
                        .map_err(|_| "invalid_game_payload")?
            {
                return Err("game_scope_mismatch".into());
            }
            let id = game_sync::stable_action_id(
                &scope.canonical_user_id,
                "writing",
                &json!([p, s, progress.header.event_id, entry]),
            );
            let h = game_migration::header(
                db,
                &scope,
                Some(p),
                s,
                &id,
                &parents,
                "native-game-v1",
                now,
            )?;
            let mut action = json!({"kind":"writing","progress_event_id":progress.header.event_id,"progress_entity_id":ep,"fact":progress.entries[0],"inspiration":account_base["inspiration"],"writing_bonus":account_base["writing_bonus"],"coin_coefficient":account_base["coin_coefficient"],"experience_coefficient":account_base["experience_coefficient"],"reward":{"coins":"0.000000","experience":"0.000000"}});
            action["reward"] = game_sync::writing_reward(&action, "native-game-v1")?;
            json!({"version":1,"header":h,"action":action})
        } else {
            if (kind == "StageCompleted") != s.is_some() {
                return Err("game_scope_mismatch".into());
            }
            let ep = progress_codec::scope_id(s);
            let mut q=db.prepare("SELECT event_id FROM cloud_progress_tips WHERE account_id=?1 AND project_id=?2 AND entity_id=?3 ORDER BY event_id").map_err(sql)?;
            let ids = q
                .query_map(params![a, p, ep], |r| r.get::<_, String>(0))
                .map_err(sql)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(sql)?;
            if ids.len() != 1 {
                return Err("game_dependency_progress_missing".into());
            }
            let chain = crate::progress_sync::chain(db, a, p, &ep, &ids[0])
                .map_err(|_| "game_dependency_progress_missing")?;
            if !chain.migration_complete() {
                return Err("game_dependency_progress_missing".into());
            }
            let total = chain
                .total()
                .map_err(|_| "game_dependency_progress_missing")?;
            let actual = context
                .get("total_symbols")
                .and_then(Value::as_f64)
                .ok_or("game_invalid_rule")?;
            if progress_codec::micros(&format!("{actual:.6}")).map_err(|_| "game_invalid_rule")?
                != total
            {
                return Err("game_dependency_progress_missing".into());
            }
            let cid = format!("completion:{key}");
            let id = game_sync::stable_action_id(
                &scope.canonical_user_id,
                "completion",
                &json!([p, s, cid]),
            );
            let h = completion_header(db, &scope, p, s, &id, &parents, now)?;
            let coins =
                (actual / 1000.0 + 0.5).round() * 100.0 * if s.is_some() { 0.25 } else { 1.0 };
            json!({"version":1,"header":h,"action":{"kind":"completion","completion_id":cid,"progress_event_id":ids[0],"progress_entity_id":ep,"total_symbols":progress_codec::decimal(total),"reward":{"coins":format!("{coins:.6}"),"experience":format!("{:.6}",coins*100.0)}}})
        };
        game_codec::validate(&g)?;
        let id = g["header"]["event_id"]
            .as_str()
            .ok_or("invalid_game_payload")?
            .to_string();
        let rid = game_sync::stable_action_id(&scope.canonical_user_id, "reward", &json!(id));
        let r = json!({"version":1,"header":game_migration::header(db,&scope,None,None,&rid,&account_parents,"native-game-v1",now)?,"action":{"kind":"reward","project_action_id":id,"project_id":p,"reward_id":format!("reward:{id}"),"reward":g["action"]["reward"]}});
        game_sync::queue_reward_pair(db, a, source_key, &g, &r)?;
        let project_snapshot = game_sync::rebuild_chain(db, a, &key, &id)?;
        let account_snapshot = game_sync::rebuild_chain(db, a, "account", &rid)?;
        let after_base =
            game_projection::account_base(after).map_err(|_| "game_unsupported_local_mutation")?;
        if after_base != account_snapshot {
            return Err("game_unsupported_local_mutation".into());
        }
        game_migration::queue_project(db, a, &g)?;
        for (owner, id, value) in [
            (&key, &id, &project_snapshot),
            (&"account".into(), &rid, &account_snapshot),
        ] {
            db.execute(
                "DELETE FROM cloud_game_tips WHERE account_id=?1 AND owner_key=?2",
                params![a, owner],
            )
            .map_err(sql)?;
            db.execute(
                "INSERT INTO cloud_game_tips VALUES(?1,?2,?3)",
                params![a, owner, id],
            )
            .map_err(sql)?;
            let revision = if owner == &key {
                g["header"]["revision"].as_i64()
            } else {
                r["header"]["revision"].as_i64()
            };
            db.execute("INSERT INTO cloud_game_projection VALUES(?1,?2,?3,?4,?5) ON CONFLICT(account_id,owner_key) DO UPDATE SET head_event_id=excluded.head_event_id,snapshot_json=excluded.snapshot_json,generation=excluded.generation",params![a,owner,id,canonical(value),revision]).map_err(sql)?;
        }
        db.execute("INSERT INTO cloud_game_write_intents(account_id,event_id,payload_json,prior_payload_json) VALUES(?1,?2,?3,?4)",params![a,rid,after.to_string(),before_raw]).map_err(sql)?;
        Ok(())
    })();
    if result.is_err() {
        db.execute_batch("ROLLBACK TO game_ordinary_writer")
            .map_err(sql)?;
    }
    db.execute_batch("RELEASE game_ordinary_writer")
        .map_err(sql)?;
    match result {
        Ok(()) => Ok(true),
        Err(code) if code == "game_storage_error" => Err(code),
        Err(code) => {
            // This local mutation still follows the existing product rules. It
            // cannot be passed off as portable Game authority.
            db.execute("INSERT INTO cloud_game_blockers VALUES(?1,'account','',?2) ON CONFLICT(account_id,owner_key,event_id) DO UPDATE SET code=excluded.code",params![a,code]).map_err(sql)?;
            db.execute("UPDATE cloud_game_migrations SET lifecycle='blocked',blocker=?1 WHERE account_id=?2",params![code,a]).map_err(sql)?;
            Ok(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const NOW: &str = "2026-10-04T00:00:00.000000Z";
    fn seed() -> (
        Connection,
        std::path::PathBuf,
        metadata::MetadataScope,
        Value,
    ) {
        seed_kind(true)
    }
    fn seed_kind(
        infinite: bool,
    ) -> (
        Connection,
        std::path::PathBuf,
        metadata::MetadataScope,
        Value,
    ) {
        seed_with_source(infinite, None)
    }
    fn seed_with_source(
        infinite: bool,
        initial: Option<Value>,
    ) -> (
        Connection,
        std::path::PathBuf,
        metadata::MetadataScope,
        Value,
    ) {
        let path = std::env::temp_dir().join(format!(
            "game-writer-{}.db",
            metadata::new_event_id().unwrap()
        ));
        let mut db = crate::sqlite::open_database(&path).unwrap();
        let f: Value = serde_json::from_str(include_str!(
            "../../src/cloud/__fixtures__/progressCodecV1.json"
        ))
        .unwrap();
        let h = &f["examples"][0]["event"]["header"];
        let scope = metadata::MetadataScope {
            account_id: "local".into(),
            canonical_user_id: h["account_id"].as_str().unwrap().into(),
            device_id: h["device_id"].as_str().unwrap().into(),
        };
        db.execute("INSERT INTO projects(id,name,infinite,unit,status,payload_json) VALUES('P1','Project',?1,'symbols','active','{\"total\":0,\"progress_entries\":[]}')",[infinite]).unwrap();
        db.execute("INSERT INTO project_order VALUES('P1',0)", [])
            .unwrap();
        db.execute("INSERT INTO cloud_sync_state(account_id,device_id,created_at,updated_at) VALUES('local',?1,?2,?2)",params![scope.device_id,NOW]).unwrap();
        db.execute(
            "INSERT INTO cloud_account_bindings VALUES('local',?1,?2,?2)",
            params![scope.canonical_user_id, NOW],
        )
        .unwrap();
        db.execute(
            "INSERT INTO cloud_sync_project_bindings VALUES('P1','local',?1,?1)",
            [NOW],
        )
        .unwrap();
        db.execute("INSERT INTO cloud_sync_project_bootstraps(project_id,account_id,device_id,bootstrap_id,mode,phase,created_at,updated_at) VALUES('P1','local',?1,?2,'upload_existing','prepared',?3,?3)",params![scope.device_id,h["bootstrap_id"].as_str(),NOW]).unwrap();
        let e = json!({"version":1,"header":{"account_id":scope.canonical_user_id,"project_id":"P1","entity_id":"P1","device_id":scope.device_id,"bootstrap_id":h["bootstrap_id"],"event_id":h["metadata_event_id"],"revision":1,"generation":1,"operation":"create","parent_event_ids":[],"updated_at":NOW},"metadata":{"name":"Project","goal":null,"infinite":infinite,"unit":"symbols","status":"active","deadline":null,"personal_goal":0,"auto_freeze":true,"streak_enabled":true,"work_method":"manual","stages_enabled":false,"combine_stage_mindmaps":false},"deleted_at":null});
        let mut payload = e["metadata"].clone();
        payload["total"] = json!(0);
        payload["progress_entries"] = json!([]);
        db.execute(
            "UPDATE projects SET payload_json=?1 WHERE id='P1'",
            [payload.to_string()],
        )
        .unwrap();
        db.execute("INSERT INTO cloud_sync_inbox(account_id,event_id,server_sequence,device_id,project_id,entity_id,entity_type,operation,sync_revision,updated_at,state,received_at) VALUES('local',?1,1,?2,'P1','P1','project_metadata','upsert',1,?3,'received',?3)",params![h["metadata_event_id"].as_str(),scope.device_id,NOW]).unwrap();
        db.execute("INSERT INTO cloud_sync_event_objects VALUES('local',?1,1,1,zeroblob(24),zeroblob(16),?2)",params![h["metadata_event_id"].as_str(),NOW]).unwrap();
        metadata::preserve_authenticated_event(
            &mut db,
            "local",
            "P1",
            &serde_json::to_vec(&e).unwrap(),
            NOW,
        )
        .unwrap();
        let source = initial.unwrap_or_else(||json!({"gamer":serde_json::from_str::<Value>(include_str!("../../src/cloud/__fixtures__/gameLegacyDefaultsV1.json")).unwrap()}));
        db.execute(
            "INSERT INTO game_state VALUES(1,2,?1,?2)",
            params![canonical(&source), NOW],
        )
        .unwrap();
        game_migration::capture(&mut db, &scope, NOW).unwrap();
        let events: Vec<Vec<u8>> = db
            .prepare("SELECT canonical_frame FROM cloud_game_events ORDER BY scope")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        for (i, frame) in events.iter().enumerate() {
            let project = i == 1;
            let e = game_codec::unframe(frame, project).unwrap();
            let h = &e["header"];
            let seq = i as i64 + 2;
            if project {
                db.execute("INSERT INTO cloud_sync_inbox(account_id,event_id,server_sequence,device_id,project_id,entity_id,entity_type,operation,sync_revision,updated_at,state,received_at) VALUES('local',?1,?2,?3,'P1',?4,'project_game','event',1,?5,'received',?5)",params![h["event_id"].as_str(),seq,scope.device_id,h["entity_id"].as_str(),NOW]).unwrap();
                db.execute("INSERT INTO cloud_sync_event_objects VALUES('local',?1,1,1,zeroblob(24),zeroblob(32),?2)",params![h["event_id"].as_str(),NOW]).unwrap();
            } else {
                db.execute("INSERT INTO cloud_sync_account_inbox(account_id,event_id,canonical_user_id,scope,server_sequence,device_id,entity_id,entity_type,operation,sync_revision,updated_at,crypto_version,aad_version,nonce,ciphertext,received_at) VALUES('local',?1,?2,'account',?3,?4,?5,'account_game','upsert',1,?6,2,2,zeroblob(24),zeroblob(32),?6)",params![h["event_id"].as_str(),scope.canonical_user_id,seq,scope.device_id,h["entity_id"].as_str(),NOW]).unwrap();
            }
            let mut privileged =
                crate::sqlite::open_privileged_remote_apply_database(&path).unwrap();
            assert_eq!(
                game_sync::apply(&mut privileged, &scope, frame, &[0; 24], &[0; 32], project)
                    .unwrap(),
                "applied"
            );
        }
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM cloud_game_migrations WHERE lifecycle='active'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            2
        );
        (db, path, scope, f)
    }
    #[test]
    fn active_account_loss_is_atomic_and_self_echo_matches() {
        let mut source = json!({"gamer":serde_json::from_str::<Value>(include_str!("../../src/cloud/__fixtures__/gameLegacyDefaultsV1.json")).unwrap(),"global_streak":{"global_streaks":["2026-10-01","2026-10-02","2026-10-03"],"max_global_streak":3,"global_streak_status":"Go"}});
        source["gamer"]["health"] = json!(100.0);
        let (mut db, path, scope, _) = seed_with_source(true, Some(source));
        let before: String = db
            .query_row("SELECT payload_json FROM game_state", [], |r| r.get(0))
            .unwrap();
        let tip_before = game_sync::tips(&db, "local", "account").unwrap();
        {
            let tx = db.transaction().unwrap();
            assert!(crate::game::refresh_global_loss(
                &tx,
                "2026-10-05",
                "2026-10-05T00:00:00.000000Z"
            )
            .unwrap());
        }
        assert_eq!(
            db.query_row("SELECT payload_json FROM game_state", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            before
        );
        assert_eq!(
            game_sync::tips(&db, "local", "account").unwrap(),
            tip_before
        );
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM cloud_game_events WHERE revision=2 AND scope='account'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM cloud_game_write_intents", [], |r| r
                .get::<_, i64>(
                0
            ))
            .unwrap(),
            0
        );
        {
            let tx = db.transaction().unwrap();
            assert!(crate::game::refresh_global_loss(
                &tx,
                "2026-10-05",
                "2026-10-05T00:00:00.000000Z"
            )
            .unwrap());
            tx.commit().unwrap();
        }
        let after: String = db
            .query_row("SELECT payload_json FROM game_state", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&after).unwrap()["gamer"]["health"],
            85.0
        );
        assert_eq!(
            serde_json::from_str::<Value>(&after).unwrap()["notifications"]["new"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let (id,frame):(String,Vec<u8>)=db.query_row("SELECT event_id,canonical_frame FROM cloud_game_events WHERE revision=2 AND scope='account'",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
        let event = game_codec::unframe(&frame, false).unwrap();
        assert_eq!(event["version"], 2);
        assert_eq!(event["action"]["project_action_ids"], json!([]));
        assert_eq!(
            db.query_row(
                "SELECT payload_json FROM cloud_game_write_intents WHERE event_id=?1",
                [&id],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            after
        );
        assert!(!crate::game::refresh_global_loss(
            &db,
            "2026-10-06",
            "2026-10-06T00:00:00.000000Z"
        )
        .unwrap());
        db.execute("INSERT INTO cloud_sync_account_inbox(account_id,event_id,canonical_user_id,scope,server_sequence,device_id,entity_id,entity_type,operation,sync_revision,updated_at,crypto_version,aad_version,nonce,ciphertext,received_at) VALUES('local',?1,?2,'account',20,?3,?4,'account_game','upsert',2,?5,2,2,zeroblob(24),zeroblob(32),?5)",params![id,scope.canonical_user_id,scope.device_id,event["header"]["entity_id"].as_str(),event["header"]["updated_at"].as_str()]).unwrap();
        game_sync::seal(&db, "local", &id, &frame, &[0; 24], &[0; 32]).unwrap();
        let mut privileged = crate::sqlite::open_privileged_remote_apply_database(&path).unwrap();
        assert_eq!(
            game_sync::apply(&mut privileged, &scope, &frame, &[0; 24], &[0; 32], false).unwrap(),
            "applied"
        );
        assert_eq!(
            game_sync::apply(&mut privileged, &scope, &frame, &[0; 24], &[0; 32], false).unwrap(),
            "applied"
        );
        let current: String = db
            .query_row("SELECT payload_json FROM game_state", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&current).unwrap()["gamer"]["health"],
            85.0
        );
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM cloud_game_events WHERE revision=2 AND scope='account'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        assert_ne!(before, after);
        drop(privileged);
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn game_active_owner_migration_does_not_republish_accepted_bases() {
        let (mut db, path, scope, _) = seed();
        let before: Vec<Vec<u8>> = db
            .prepare("SELECT canonical_frame FROM cloud_game_events ORDER BY event_id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        game_migration::capture(&mut db, &scope, NOW).unwrap();
        let after: Vec<Vec<u8>> = db
            .prepare("SELECT canonical_frame FROM cloud_game_events ORDER BY event_id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(before, after);
        assert_eq!(
            db.query_row("SELECT count(*) FROM cloud_game_candidates", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            2
        );
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn game_ordinary_project_completion_freezes_pending_metadata_and_pays_once() {
        let (mut db, path, scope, fixture) = seed_kind(false);
        let mut root: progress_codec::Event =
            serde_json::from_value(fixture["examples"][0]["event"].clone()).unwrap();
        root.entries.clear();
        root.migration = Some(progress_codec::MigrationProof {
            entry_count: 0,
            final_total: "0.000000".into(),
        });
        root.base_total = Some("0.000000".into());
        let frame = progress_codec::encode(&root).unwrap();
        db.execute("INSERT INTO cloud_progress_events(account_id,event_id,project_id,entity_id,canonical_frame,parents_json,revision,state) VALUES('local',?1,'P1','project',?2,'[]',1,'unsealed')",params![root.header.event_id,frame]).unwrap();
        db.execute(
            "INSERT INTO cloud_progress_tips VALUES('local','P1','project',?1)",
            [&root.header.event_id],
        )
        .unwrap();
        let before_view = metadata::authority_view(&db, "local", "P1").unwrap();
        assert_eq!(
            before_view.state,
            "active",
            "{}",
            serde_json::to_string(&before_view).unwrap()
        );
        crate::complete_project_sqlite(
            &mut db,
            crate::ProjectIdCommand {
                project_id: "P1".into(),
            },
        )
        .unwrap();
        assert_eq!(
            metadata::authority_view(&db, "local", "P1").unwrap().state,
            "resolution_pending"
        );
        let pending = metadata::authority_view(&db, "local", "P1")
            .unwrap()
            .pending_event_id
            .unwrap();
        assert_eq!(
            crate::game::process_pending_events(&mut db, 10)
                .unwrap()
                .processed,
            1
        );
        let g:Vec<u8>=db.query_row("SELECT canonical_frame FROM cloud_game_events WHERE scope='project' AND revision=2",[],|r|r.get(0)).unwrap();
        let g = game_codec::unframe(&g, true).unwrap();
        assert_eq!(g["action"]["kind"], "completion");
        assert_eq!(g["header"]["metadata_event_id"], pending);
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM cloud_game_rewards", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM cloud_game_local_mutations", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        let source: String = db
            .query_row("SELECT payload_json FROM game_state", [], |r| r.get(0))
            .unwrap();
        let value: Value = serde_json::from_str(&source).unwrap();
        assert!(value["gamer"]["complete_bonus_projects"]
            .as_array()
            .unwrap()
            .contains(&json!("project:P1")));
        drop(db);
        let mut db = crate::sqlite::open_database(&path).unwrap();
        db.execute(
            "UPDATE domain_events SET status='pending',processed_at=NULL",
            [],
        )
        .unwrap();
        assert_eq!(
            crate::game::process_pending_events(&mut db, 10)
                .unwrap()
                .processed,
            1
        );
        assert_eq!(
            db.query_row("SELECT payload_json FROM game_state", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            source
        );
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM cloud_game_rewards", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        db.execute(
            "UPDATE storage_ownership SET owner='sqlite' WHERE subsystem='game'",
            [],
        )
        .unwrap();
        crate::game::GameApplicationService::inventory_sqlite(
            &mut db,
            "Зелья".into(),
            "Микро зелье здоровья".into(),
            2,
            "buy",
        )
        .unwrap();
        crate::game::GameApplicationService::inventory_sqlite(
            &mut db,
            "Зелья".into(),
            "Микро зелье здоровья".into(),
            1,
            "sell",
        )
        .unwrap();
        let after: String = db
            .query_row("SELECT payload_json FROM game_state", [], |r| r.get(0))
            .unwrap();
        let projection: String = db
            .query_row(
                "SELECT snapshot_json FROM cloud_game_projection WHERE owner_key='account'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            game_projection::account_base(&serde_json::from_str(&after).unwrap()).unwrap(),
            serde_json::from_str::<Value>(&projection).unwrap()
        );
        assert_eq!(db.query_row("SELECT COUNT(*) FROM cloud_game_events WHERE json_extract(CAST(substr(canonical_frame,21) AS TEXT),'$.action.kind')='inventory'",[],|r|r.get::<_,i64>(0)).unwrap(),2);
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM cloud_game_local_mutations", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM cloud_game_rewards", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        drop(db);
        let db = crate::sqlite::open_database(&path).unwrap();
        assert_eq!(
            db.query_row("SELECT payload_json FROM game_state", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            after
        );
        drop(db);
        std::fs::remove_file(path).unwrap();
        assert_eq!(scope.account_id, "local");
    }
    #[test]
    fn game_production_domain_writer_records_one_pair_and_restart_retry_does_not_reward_again() {
        let (mut db, path, scope, fixture) = seed();
        let mut root: progress_codec::Event =
            serde_json::from_value(fixture["examples"][0]["event"].clone()).unwrap();
        root.entries.clear();
        root.migration = Some(progress_codec::MigrationProof {
            entry_count: 0,
            final_total: "0.000000".into(),
        });
        root.base_total = Some("0.000000".into());
        let mut append: progress_codec::Event =
            serde_json::from_value(fixture["examples"][1]["event"].clone()).unwrap();
        append.header.stage_id = None;
        append.header.entity_id = "project".into();
        append.header.stage_event_ids.clear();
        append.header.parents = vec![root.header.event_id.clone()];
        append.selected_event_id = Some(root.header.event_id.clone());
        append.entries[0].new_total = "20.000000".into();
        for e in [&root, &append] {
            let frame = progress_codec::encode(e).unwrap();
            let h = &e.header;
            db.execute("INSERT INTO cloud_progress_events(account_id,event_id,project_id,entity_id,canonical_frame,parents_json,revision,state) VALUES('local',?1,'P1','project',?2,?3,?4,'unsealed')",params![h.event_id,frame,json!(h.parents).to_string(),h.revision]).unwrap();
        }
        db.execute("INSERT INTO cloud_sync_outbox(event_id,account_id,device_id,project_id,entity_id,entity_type,operation,revision,updated_at,created_at,local_ordinal,lifecycle) VALUES(?1,'local',?2,'P1','project','progress','event',2,?3,?3,2,'unsealed')",params![append.header.event_id,scope.device_id,NOW]).unwrap();
        db.execute("INSERT INTO domain_events(event_id,event_type,project_id,progress_id,delta_symbols,context_json,created_at) VALUES('ordinary-source','ProgressAdded','P1',?1,20,'{}',?2)",params![append.entries[0].entry_id,NOW]).unwrap();
        assert_eq!(
            crate::game::process_pending_events(&mut db, 10)
                .unwrap()
                .processed,
            1
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM cloud_game_rewards", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        let initial: String = db
            .query_row("SELECT payload_json FROM game_state", [], |r| r.get(0))
            .unwrap();
        let initial_value: Value = serde_json::from_str(&initial).unwrap();
        assert_eq!(initial_value["gamer"]["coins"], 2.0);
        assert_eq!(
            db.query_row("SELECT count(*) FROM cloud_game_local_mutations", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        let pair:Vec<(String,Vec<u8>)>=db.prepare("SELECT event_id,canonical_frame FROM cloud_game_events WHERE revision=2 ORDER BY event_id").unwrap().query_map([],|r|Ok((r.get(0)?,r.get(1)?))).unwrap().collect::<rusqlite::Result<_>>().unwrap();
        assert_eq!(pair.len(), 2);
        drop(db);
        let mut db = crate::sqlite::open_database(&path).unwrap();
        db.execute("UPDATE domain_events SET processed_at=NULL,status='pending' WHERE event_id='ordinary-source'",[]).unwrap();
        assert_eq!(
            crate::game::process_pending_events(&mut db, 10)
                .unwrap()
                .processed,
            1
        );
        assert_eq!(
            db.query_row("SELECT payload_json FROM game_state", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            initial
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM cloud_game_rewards", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert!(already_recorded(
            &db,
            "ordinary-source",
            "P1",
            None,
            Some(&append.entries[0].entry_id),
            20.0
        )
        .unwrap());
        db.execute("INSERT INTO domain_events(event_id,event_type,project_id,progress_id,delta_symbols,context_json,created_at) VALUES('duplicate-local-source','ProgressAdded','P1',?1,20,'{}',?2)",params![append.entries[0].entry_id,NOW]).unwrap();
        assert_eq!(
            crate::game::process_pending_events(&mut db, 10)
                .unwrap()
                .processed,
            1
        );
        assert_eq!(
            db.query_row("SELECT payload_json FROM game_state", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            initial
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM cloud_game_rewards", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        // Synthetic transport envelopes exercise real native apply and shared
        // ACK; production-crypto/PostgreSQL acceptance remains a separate gate.
        let pair: Vec<Value> = pair
            .iter()
            .map(|(_, f)| game_codec::unframe(f, f[9] == 12).unwrap())
            .collect();
        let n = [5u8; 24];
        let c = [6u8; 32];
        for e in &pair {
            let h = &e["header"];
            let project = h["scope"] == "project";
            let seq = if project { 6 } else { 7 };
            game_sync::seal(
                &db,
                "local",
                h["event_id"].as_str().unwrap(),
                &game_codec::frame(e).unwrap(),
                &n,
                &c,
            )
            .unwrap();
            if project {
                db.execute("INSERT INTO cloud_sync_inbox(account_id,event_id,server_sequence,device_id,project_id,entity_id,entity_type,operation,sync_revision,updated_at,state,received_at) VALUES('local',?1,?2,?3,'P1',?4,'project_game','event',2,?5,'received',?5)",params![h["event_id"].as_str(),seq,scope.device_id,h["entity_id"].as_str(),h["updated_at"].as_str()]).unwrap();
                db.execute(
                    "INSERT INTO cloud_sync_event_objects VALUES('local',?1,1,1,?2,?3,?4)",
                    params![h["event_id"].as_str(), n.as_slice(), c.as_slice(), NOW],
                )
                .unwrap();
            } else {
                db.execute("INSERT INTO cloud_sync_account_inbox(account_id,event_id,canonical_user_id,scope,server_sequence,device_id,entity_id,entity_type,operation,sync_revision,updated_at,crypto_version,aad_version,nonce,ciphertext,received_at) VALUES('local',?1,?2,'account',?3,?4,?5,'account_game','upsert',2,?6,2,2,?7,?8,?6)",params![h["event_id"].as_str(),scope.canonical_user_id,seq,scope.device_id,h["entity_id"].as_str(),h["updated_at"].as_str(),n.as_slice(),c.as_slice()]).unwrap();
            }
        }
        for (i, e) in [&root, &append].iter().enumerate() {
            let h = &e.header;
            db.execute("INSERT INTO cloud_sync_inbox(account_id,event_id,server_sequence,device_id,project_id,entity_id,entity_type,operation,sync_revision,updated_at,state,received_at) VALUES('local',?1,?2,?3,'P1','project','progress','event',?4,?5,'received',?5)",params![h.event_id,4+i as i64,scope.device_id,h.revision,h.updated_at]).unwrap();
            db.execute(
                "INSERT INTO cloud_sync_event_objects VALUES('local',?1,1,1,?2,?3,?4)",
                params![h.event_id, n.as_slice(), c.as_slice(), NOW],
            )
            .unwrap();
        }
        // An already processed unrelated legacy Note beyond the hole is safe
        // locally, but cannot carry ACK past the dependency ladder.
        db.execute("INSERT INTO cloud_sync_inbox(account_id,event_id,server_sequence,device_id,project_id,entity_id,entity_type,operation,sync_revision,updated_at,state,received_at) VALUES('local','123e4567-e89b-42d3-a456-426614174999',8,?1,'P1','retained-note','note','upsert',1,?2,'applied',?2)",params![scope.device_id,NOW]).unwrap();
        db.execute(
            "UPDATE cloud_sync_state SET pull_cursor=8 WHERE account_id='local'",
            [],
        )
        .unwrap();
        let command = crate::note_sync::PrepareNoteSyncAckCommand {
            account_id: "local".into(),
            device_id: scope.device_id.clone(),
            canonical_user_id: scope.canonical_user_id.clone(),
        };
        let g = pair
            .iter()
            .find(|e| e["header"]["scope"] == "project")
            .unwrap();
        let r = pair
            .iter()
            .find(|e| e["header"]["scope"] == "account")
            .unwrap();
        let mut privileged = crate::sqlite::open_privileged_remote_apply_database(&path).unwrap();
        assert_eq!(
            game_sync::apply(
                &mut privileged,
                &scope,
                &game_codec::frame(g).unwrap(),
                &n,
                &c,
                true
            )
            .unwrap(),
            "waiting"
        );
        assert_eq!(
            game_sync::apply(
                &mut privileged,
                &scope,
                &game_codec::frame(r).unwrap(),
                &n,
                &c,
                false
            )
            .unwrap(),
            "waiting"
        );
        assert_eq!(
            crate::note_sync::prepare_note_sync_ack(&mut db, &command)
                .unwrap()
                .candidate_cursor,
            3
        );
        for (i, e) in [&root, &append].iter().enumerate() {
            assert_eq!(
                crate::progress_sync::apply(
                    &mut privileged,
                    &scope,
                    &progress_codec::encode(e).unwrap(),
                    &n,
                    &c
                )
                .unwrap(),
                "applied"
            );
            assert_eq!(
                crate::note_sync::prepare_note_sync_ack(&mut db, &command)
                    .unwrap()
                    .candidate_cursor,
                4 + i as i64
            );
        }
        assert_eq!(
            game_sync::apply(
                &mut privileged,
                &scope,
                &game_codec::frame(g).unwrap(),
                &n,
                &c,
                true
            )
            .unwrap(),
            "applied"
        );
        assert_eq!(
            crate::note_sync::prepare_note_sync_ack(&mut db, &command)
                .unwrap()
                .candidate_cursor,
            6
        );
        assert_eq!(
            game_sync::apply(
                &mut privileged,
                &scope,
                &game_codec::frame(r).unwrap(),
                &n,
                &c,
                false
            )
            .unwrap(),
            "applied"
        );
        assert_eq!(
            crate::note_sync::prepare_note_sync_ack(&mut db, &command)
                .unwrap()
                .candidate_cursor,
            8
        );
        for _ in 0..2 {
            assert_eq!(
                game_sync::apply(
                    &mut privileged,
                    &scope,
                    &game_codec::frame(r).unwrap(),
                    &n,
                    &c,
                    false
                )
                .unwrap(),
                "applied"
            );
        }
        assert_eq!(
            db.query_row("SELECT payload_json FROM game_state", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            initial
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM cloud_game_rewards", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM domain_events", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            2
        );
        drop(privileged);
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
}
