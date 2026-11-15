//! Explicit-only Game capture. Nothing in this module runs on startup/open/pull.
use crate::{
    document_codec::canonical, game_codec, game_projection, game_sync,
    project_metadata_sync as metadata,
};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};
type Result<T> = std::result::Result<T, String>;
fn sql(_: rusqlite::Error) -> String {
    "game_storage_error".into()
}
fn rows(db: &Connection, q: &str, args: impl rusqlite::Params) -> Result<Vec<String>> {
    let mut q = db.prepare(q).map_err(sql)?;
    let rows = q
        .query_map(args, |r| r.get(0))
        .map_err(sql)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(sql)?;
    Ok(rows)
}
fn revision(db: &Connection, account: &str, parents: &[String]) -> Result<u64> {
    let mut max = 0;
    for id in parents {
        let n: u64 = db
            .query_row(
                "SELECT revision FROM cloud_game_events WHERE account_id=?1 AND event_id=?2",
                params![account, id],
                |r| r.get(0),
            )
            .map_err(sql)?;
        max = max.max(n);
    }
    Ok(max + 1)
}
pub fn header(
    db: &Connection,
    scope: &metadata::MetadataScope,
    project: Option<&str>,
    stage: Option<&str>,
    id: &str,
    parents: &[String],
    rule: &str,
    now: &str,
) -> Result<Value> {
    metadata::assert_runtime_scope(
        db,
        &scope.account_id,
        &scope.canonical_user_id,
        &scope.device_id,
    )
    .map_err(|_| "game_scope_mismatch")?;
    let mut h = json!({"account_id":scope.canonical_user_id,"device_id":scope.device_id,"event_id":id,"entity_id":format!("game:{id}"),"parents":parents,"revision":revision(db,&scope.account_id,parents)?,"updated_at":now,"rule":rule,"scope":"account","entity_type":"account_game"});
    if let Some(p) = project {
        let view = metadata::authority_view(db, &scope.account_id, p)
            .map_err(|_| "game_project_authority_unresolved")?;
        if view.state != "active" {
            return Err("game_project_authority_unresolved".into());
        }
        let boot:String=db.query_row("SELECT bootstrap_id FROM cloud_sync_project_bootstraps WHERE account_id=?1 AND project_id=?2",params![scope.account_id,p],|r|r.get(0)).map_err(|_|"game_project_authority_unresolved")?;
        let refs = if let Some(s) = stage {
            rows(db,"SELECT event_id FROM cloud_sync_structural_tips WHERE account_id=?1 AND project_id=?2 AND entity_type='stage' AND entity_id=?3 ORDER BY event_id",params![scope.account_id,p,s])?
        } else {
            vec![]
        };
        if let Some(s) = stage {
            crate::stage_sync::content_reference_ready(db, &scope.account_id, p, s, &refs)
                .map_err(|_| "game_stage_authority_unresolved")?;
        }
        let o = h.as_object_mut().unwrap();
        o.remove("entity_type");
        o.insert("scope".into(), json!("project"));
        o.insert("project_id".into(), json!(p));
        o.insert("stage_id".into(), json!(stage));
        o.insert("bootstrap_id".into(), json!(boot));
        o.insert(
            "metadata_event_id".into(),
            json!(view
                .head_event_id
                .ok_or("game_project_authority_unresolved")?),
        );
        o.insert("stage_event_ids".into(), json!(refs));
        o.insert(
            "entity_id".into(),
            json!(stage
                .map(|s| format!("game:stage:{s}:{id}"))
                .unwrap_or_else(|| format!("game:project:{id}"))),
        );
    }
    Ok(h)
}
pub fn queue_project(db: &Connection, account: &str, e: &Value) -> Result<()> {
    let h = &e["header"];
    if h["scope"] != "project" {
        return Err("game_scope_mismatch".into());
    }
    game_sync::preserve(db, account, e, "unsealed")?;
    let existing: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_outbox WHERE account_id=?1 AND event_id=?2 AND project_id=?3 AND entity_id=?4 AND entity_type='project_game' AND operation='event' AND revision=?5 AND updated_at=?6)",params![account,h["event_id"].as_str(),h["project_id"].as_str(),h["entity_id"].as_str(),h["revision"].as_i64(),h["updated_at"].as_str()],|r|r.get(0)).map_err(sql)?;
    if existing {
        return Ok(());
    }
    db.execute("INSERT INTO cloud_sync_outbox(event_id,account_id,device_id,project_id,entity_id,entity_type,operation,revision,updated_at,deleted_at,created_at,parent_event_id,local_ordinal,lifecycle) VALUES(?1,?2,?3,?4,?5,'project_game','event',?6,?7,NULL,?7,?8,(SELECT COALESCE(MAX(local_ordinal),0)+1 FROM cloud_sync_outbox WHERE account_id=?2),'unsealed')",params![h["event_id"].as_str(),account,h["device_id"].as_str(),h["project_id"].as_str(),h["entity_id"].as_str(),h["revision"].as_i64(),h["updated_at"].as_str(),h["parents"][0].as_str()]).map_err(sql)?;
    Ok(())
}
/// Overlay absent in older profiles: preserve the actual entity's legacy series.
pub(crate) fn owner_source(
    db: &Connection,
    source: &Value,
    p: &str,
    s: Option<&str>,
) -> Result<Value> {
    let key = s
        .map(|s| format!("stage:{p}:{s}"))
        .unwrap_or_else(|| format!("project:{p}"));
    if let Some(v) = source["project_game_state"].get(&key) {
        return Ok(v.clone());
    }
    let raw: Option<String> = if let Some(s) = s {
        db.query_row(
            "SELECT payload_json FROM stages WHERE project_id=?1 AND id=?2",
            params![p, s],
            |r| r.get(0),
        )
        .optional()
        .map_err(sql)?
    } else {
        db.query_row("SELECT payload_json FROM projects WHERE id=?1", [p], |r| {
            r.get(0)
        })
        .optional()
        .map_err(sql)?
    };
    let raw: Value = serde_json::from_str(&raw.ok_or("game_stage_authority_unresolved")?)
        .map_err(|_| "game_legacy_extension_unsupported")?;
    let mut result = serde_json::Map::new();
    for k in [
        "streaks",
        "max_streak",
        "streak_status",
        "last_streak_bonus",
        "last_streak_lost_date",
        "last_streak_lose_len",
        "freezes",
    ] {
        if let Some(v) = raw.get(k) {
            result.insert(k.into(), v.clone());
        }
    }
    Ok(Value::Object(result))
}
fn blocked(db: &Connection, account: &str, key: &str, id: &str, code: &str) -> Result<()> {
    db.execute("INSERT INTO cloud_game_blockers VALUES(?1,?2,'',?3) ON CONFLICT(account_id,owner_key,event_id) DO UPDATE SET code=excluded.code",params![account,key,code]).map_err(sql)?;
    db.execute("INSERT INTO cloud_game_migrations VALUES(?1,?2,?3,'blocked',?4) ON CONFLICT(account_id,owner_key) DO UPDATE SET lifecycle='blocked',blocker=excluded.blocker,candidate_id=excluded.candidate_id",params![account,key,id,code]).map_err(sql)?;
    Ok(())
}
/// Whole capture is atomic; one unsupported bound owner prevents any partial
/// genesis publication. Every candidate retains its original local JSON.
pub fn capture(db: &mut Connection, scope: &metadata::MetadataScope, now: &str) -> Result<()> {
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sql)?;
    metadata::assert_runtime_scope(
        &tx,
        &scope.account_id,
        &scope.canonical_user_id,
        &scope.device_id,
    )
    .map_err(|_| "game_scope_mismatch")?;
    if !metadata::timestamp(now) {
        return Err("invalid_game_payload".into());
    }
    let a = &scope.account_id;
    let pending:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM cloud_game_migrations WHERE account_id=?1 AND lifecycle IN ('captured','publication_pending','self_echo_pending','conflict'))",[a],|r|r.get(0)).map_err(sql)?;
    if pending {
        tx.commit().map_err(sql)?;
        return Ok(());
    }
    let raw: Option<String> = tx
        .query_row("SELECT payload_json FROM game_state WHERE id=1", [], |r| {
            r.get(0)
        })
        .optional()
        .map_err(sql)?;
    let failure = match raw.as_deref() {
        None => Some("game_legacy_extension_unsupported"),
        Some(raw) if raw.len() > game_sync::MAX_HISTORY_BYTES => Some("game_resource_limit"),
        Some(raw) if serde_json::from_str::<Value>(raw).is_err() => {
            Some("game_legacy_extension_unsupported")
        }
        _ => None,
    };
    if let Some(code) = failure {
        // Keep the exact original source in game_state. A bounded reference is
        // sufficient recovery evidence when the source cannot fit a candidate.
        let id = metadata::new_event_id().map_err(|_| "game_storage_error")?;
        tx.execute(
            "INSERT INTO cloud_game_candidates VALUES(?1,?2,'account',?3,NULL,?4)",
            params![
                id,
                a,
                canonical(&json!({"retained_source":"game_state","reason":code})),
                now
            ],
        )
        .map_err(sql)?;
        blocked(&tx, a, "account", &id, code)?;
        tx.commit().map_err(sql)?;
        return Ok(());
    }
    let source: Value = serde_json::from_str(raw.as_deref().unwrap())
        .map_err(|_| "game_legacy_extension_unsupported")?;
    let account_id = metadata::new_event_id().map_err(|_| "game_storage_error")?;
    let restricted: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM cloud_game_local_restrictions)",
            [],
            |r| r.get(0),
        )
        .map_err(sql)?;
    let base = if restricted {
        Err("game_developer_state_restricted".to_string())
    } else {
        game_projection::account_base(&source)
    };
    let account_active: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM cloud_game_migrations WHERE account_id=?1 AND owner_key='account' AND lifecycle='active')", [a], |r|r.get(0)).map_err(sql)?;
    let mut candidates = Vec::new();
    let mut events = Vec::new();
    if !account_active {
        tx.execute(
            "INSERT INTO cloud_game_candidates VALUES(?1,?2,'account',?3,?4,?5)",
            params![
                account_id,
                a,
                canonical(&source),
                base.as_ref().ok().map(canonical),
                now
            ],
        )
        .map_err(sql)?;
        candidates.push((
            "account".to_string(),
            account_id.clone(),
            base.as_ref().err().cloned(),
        ));
        if let Ok(base) = &base {
            let parents = game_sync::tips(&tx, a, "account")?;
            events.push(json!({"version":1,"header":header(&tx,scope,None,None,&account_id,&parents,"legacy-game-v1",now)?,"action":{"kind":if parents.is_empty(){"genesis"}else{"adopt_local"},"base":base}}));
        }
    }
    let projects=rows(&tx,"SELECT project_id FROM cloud_sync_project_bindings WHERE account_id=?1 ORDER BY project_id",[a])?;
    for p in projects {
        let mut owners = vec![None];
        owners.extend(
            rows(
                &tx,
                "SELECT id FROM stages WHERE project_id=?1 ORDER BY id",
                [&p],
            )?
            .into_iter()
            .map(Some),
        );
        // A removed Stage overlay remains a recovery candidate. It may not be
        // silently omitted or converted into an implicit Stage binding.
        if let Some(overlays) = source["project_game_state"].as_object() {
            let prefix = format!("stage:{p}:");
            for k in overlays.keys() {
                if let Some(s) = k.strip_prefix(&prefix) {
                    if !owners.iter().any(|id| id.as_deref() == Some(s)) {
                        owners.push(Some(s.into()));
                    }
                }
            }
        }
        if candidates.len() + owners.len() > 4096 {
            if candidates.is_empty() {
                tx.execute(
                    "INSERT INTO cloud_game_candidates VALUES(?1,?2,'account',?3,NULL,?4)",
                    params![account_id, a, canonical(&source), now],
                )
                .map_err(sql)?;
                candidates.push((
                    "account".into(),
                    account_id.clone(),
                    Some("game_resource_limit".into()),
                ));
            } else {
                candidates[0].2 = Some("game_resource_limit".into());
            }
            break;
        }
        for stage in owners {
            let key = canonical(&json!([p, stage]));
            let owner_active:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM cloud_game_migrations WHERE account_id=?1 AND owner_key=?2 AND lifecycle='active')",params![a,key],|r|r.get(0)).map_err(sql)?;
            if owner_active {
                continue;
            }
            let legacy = stage
                .as_ref()
                .map(|s| format!("stage:{p}:{s}"))
                .unwrap_or_else(|| format!("project:{p}"));
            let id = metadata::new_event_id().map_err(|_| "game_storage_error")?;
            let source_result = owner_source(&tx, &source, &p, stage.as_deref());
            let owner_source = source_result.as_ref().cloned().unwrap_or_else(
                |_| json!({"retained_source":"game_state","project_id":p,"stage_id":stage}),
            );
            let result = (|| -> Result<Value> {
                if let Err(code) = &source_result {
                    return Err(code.clone());
                }
                if let Err(code) = &base {
                    return Err(code.clone());
                }
                let base = game_projection::streak_base(&owner_source, false)?;
                let parents = game_sync::tips(&tx, a, &key)?;
                let h = header(
                    &tx,
                    scope,
                    Some(&p),
                    stage.as_deref(),
                    &id,
                    &parents,
                    "legacy-game-v1",
                    now,
                )?;
                let claimed = source["gamer"]["complete_bonus_projects"]
                    .as_array()
                    .is_some_and(|v| {
                        v.contains(&json!(legacy))
                            || v.contains(&json!(format!("completion:{key}")))
                    });
                let e = json!({"version":1,"header":h,"action":{"kind":if parents.is_empty(){"genesis"}else{"adopt_local"},"base":base,"completion_claimed":claimed}});
                game_sync::dependencies(&tx, a, &scope.canonical_user_id, &e)?;
                game_codec::frame(&e)?;
                Ok(e)
            })();
            tx.execute(
                "INSERT INTO cloud_game_candidates VALUES(?1,?2,?3,?4,?5,?6)",
                params![
                    id,
                    a,
                    key,
                    canonical(&owner_source),
                    result.as_ref().ok().map(canonical),
                    now
                ],
            )
            .map_err(sql)?;
            candidates.push((key, id, result.as_ref().err().cloned()));
            if let Ok(e) = result {
                events.push(e)
            }
        }
    }
    // A resource failure must commit the retained candidates and blocker, not
    // roll the whole capture back and invite an apparently fresh migration.
    let mut total = 0usize;
    let frame_error = events.iter().find_map(|e| match game_codec::frame(e) {
        Err(code) => Some(code),
        Ok(frame) => {
            total = total.saturating_add(frame.len());
            (total > game_sync::MAX_HISTORY_BYTES).then(|| "game_resource_limit".into())
        }
    });
    let common = candidates
        .iter()
        .find_map(|(_, _, e)| e.clone())
        .or(frame_error);
    if let Some(common) = common {
        for (key, id, code) in candidates {
            blocked(&tx, a, &key, &id, code.as_deref().unwrap_or(&common))?;
        }
    } else {
        for e in events {
            let h = &e["header"];
            let key = game_sync::owner(&e);
            let id = h["event_id"].as_str().unwrap();
            if h["scope"] == "project" {
                queue_project(&tx, a, &e)?;
            } else {
                game_sync::preserve(&tx, a, &e, "unsealed")?;
            }
            tx.execute("INSERT INTO cloud_game_migrations VALUES(?1,?2,?3,'publication_pending',NULL) ON CONFLICT(account_id,owner_key) DO UPDATE SET candidate_id=excluded.candidate_id,lifecycle=excluded.lifecycle,blocker=NULL",params![a,key,id]).map_err(sql)?;
            tx.execute("DELETE FROM cloud_game_blockers WHERE account_id=?1 AND owner_key=?2 AND event_id=''",params![a,key]).map_err(sql)?;
        }
    }
    tx.commit().map_err(sql)
}

#[cfg(test)]
mod tests {
    use super::*;
    const USER: &str = "123e4567-e89b-42d3-a456-426614174000";
    const DEVICE: &str = "123e4567-e89b-42d3-a456-426614174001";
    const NOW: &str = "2026-10-04T00:00:00.000000Z";
    fn setup() -> (
        Connection,
        std::path::PathBuf,
        metadata::MetadataScope,
        Value,
    ) {
        let path = std::env::temp_dir().join(format!(
            "game-migration-{}.db",
            metadata::new_event_id().unwrap()
        ));
        let db = crate::sqlite::open_database(&path).unwrap();
        db.execute("INSERT INTO cloud_sync_state(account_id,device_id,created_at,updated_at) VALUES('local',?1,?2,?2)",params![DEVICE,NOW]).unwrap();
        db.execute(
            "INSERT INTO cloud_account_bindings VALUES('local',?1,?2,?2)",
            params![USER, NOW],
        )
        .unwrap();
        let mut source = json!({"gamer":serde_json::from_str::<Value>(include_str!("../../src/cloud/__fixtures__/gameLegacyDefaultsV1.json")).unwrap(),"notifications":{"new":["local"]},"global_streak":{"global_streaks":[{"__type__":"date","value":"2026-10-01"},"freeze"],"max_global_streak":10}});
        source["gamer"]["coins"] = json!(765.4);
        source["gamer"]["complete_bonus_projects"] = json!(["project:local-only"]);
        db.execute(
            "INSERT INTO game_state VALUES(1,2,?1,?2)",
            params![canonical(&source), NOW],
        )
        .unwrap();
        (
            db,
            path,
            metadata::MetadataScope {
                account_id: "local".into(),
                canonical_user_id: USER.into(),
                device_id: DEVICE.into(),
            },
            source,
        )
    }
    #[test]
    fn game_missing_source_retains_durable_migration_blocker() {
        let (mut db, path, scope, _) = setup();
        db.execute("DELETE FROM game_state", []).unwrap();
        capture(&mut db, &scope, NOW).unwrap();
        assert_eq!(
            db.query_row("SELECT lifecycle FROM cloud_game_migrations", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap(),
            "blocked"
        );
        assert_eq!(
            db.query_row("SELECT code FROM cloud_game_blockers", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "game_legacy_extension_unsupported"
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM cloud_game_events", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn game_explicit_migration_retains_baseline_pays_nothing_and_resumes_same_frame() {
        let (mut db, path, scope, source) = setup();
        assert_eq!(
            db.query_row("SELECT count(*) FROM cloud_game_events", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        capture(&mut db, &scope, NOW).unwrap();
        let frame: Vec<u8> = db
            .query_row("SELECT canonical_frame FROM cloud_game_events", [], |r| {
                r.get(0)
            })
            .unwrap();
        let event = game_codec::unframe(&frame, false).unwrap();
        assert_eq!(event["action"]["base"]["coins"], "765.400000");
        assert_eq!(
            event["action"]["base"]["completion_claims"],
            json!(["project:local-only"])
        );
        assert_eq!(event["action"]["kind"], "genesis");
        assert_eq!(
            db.query_row("SELECT payload_json FROM game_state", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            canonical(&source)
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM cloud_game_rewards", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM cloud_sync_project_bindings",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        drop(db);
        let mut db = crate::sqlite::open_database(&path).unwrap();
        capture(&mut db, &scope, "2026-10-05T00:00:00.000000Z").unwrap();
        assert_eq!(
            db.query_row("SELECT canonical_frame FROM cloud_game_events", [], |r| r
                .get::<_, Vec<
                u8,
            >>(
                0
            ))
            .unwrap(),
            frame
        );
        assert_eq!(
            db.query_row("SELECT lifecycle FROM cloud_game_migrations", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap(),
            "publication_pending"
        );
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn game_migration_unsupported_and_developer_sources_are_retained_without_publication() {
        for dev in [false, true] {
            let (mut db, path, scope, mut source) = setup();
            if dev {
                game_sync::record_developer_override(&db, &source, NOW).unwrap();
            } else {
                source["gamer"]["unknown_extension"] = json!({"value":99});
                db.execute(
                    "UPDATE game_state SET payload_json=?1",
                    [canonical(&source)],
                )
                .unwrap();
            }
            capture(&mut db, &scope, NOW).unwrap();
            let(code,retained):(String,String)=db.query_row("SELECT m.blocker,c.source_json FROM cloud_game_migrations m JOIN cloud_game_candidates c USING(candidate_id)",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
            assert_eq!(
                code,
                if dev {
                    "game_developer_state_restricted"
                } else {
                    "game_legacy_extension_unsupported"
                }
            );
            assert_eq!(retained, canonical(&source));
            assert_eq!(
                db.query_row("SELECT count(*) FROM cloud_game_events", [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                0
            );
            drop(db);
            let db = crate::sqlite::open_database(&path).unwrap();
            assert_eq!(
                db.query_row("SELECT lifecycle FROM cloud_game_migrations", [], |r| {
                    r.get::<_, String>(0)
                })
                .unwrap(),
                "blocked"
            );
            drop(db);
            std::fs::remove_file(path).unwrap();
        }
    }
    #[test]
    fn game_migration_reads_real_legacy_project_series_when_overlay_is_absent() {
        let (db, path, _, source) = setup();
        db.execute("INSERT INTO projects(id,name,infinite,unit,status,payload_json) VALUES('p','rename',1,'symbols','активен',?1)",[canonical(&json!({"streaks":[{"__type__":"date","value":"2026-10-01"}],"max_streak":17,"name":"rename","notes":"local"}))]).unwrap();
        let src = owner_source(&db, &source, "p", None).unwrap();
        let base = game_projection::streak_base(&src, false).unwrap();
        assert_eq!(base["maximum"], 17);
        assert_eq!(base["history"][0]["day"], "2026-10-01");
        assert!(src.get("notes").is_none());
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn game_migration_frame_overflow_commits_recovery_and_blocker() {
        let (mut db, path, scope, mut source) = setup();
        source["gamer"]["complete_bonus_projects"] = json!((0..4096)
            .map(|i| format!("{i:04}{}", "x".repeat(500)))
            .collect::<Vec<_>>());
        db.execute(
            "UPDATE game_state SET payload_json=?1",
            [canonical(&source)],
        )
        .unwrap();
        capture(&mut db, &scope, NOW).unwrap();
        assert_eq!(
            db.query_row("SELECT blocker FROM cloud_game_migrations", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "game_resource_limit"
        );
        assert_eq!(
            db.query_row("SELECT source_json FROM cloud_game_candidates", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap(),
            canonical(&source)
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM cloud_game_events", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
}
