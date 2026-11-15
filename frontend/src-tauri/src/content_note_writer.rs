//! Explicit codec8 writers. The established C15/C17 queue and history stay authoritative.
use crate::{note_sync as notes, project_metadata_sync as metadata};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde_json::{json, Value};

fn error(value: impl std::fmt::Display) -> notes::NoteSyncError {
    let code = match value.to_string().as_str() {
        "stage_dependency_missing" => "stage_dependency_missing",
        "stage_tombstone_child_manifest_incomplete" => "stage_tombstone_child_manifest_incomplete",
        "project_metadata_authority_unresolved" => "project_metadata_authority_unresolved",
        "content_note_unsupported_source" => "content_note_unsupported_source",
        "unsupported_content_format" => "unsupported_content_format",
        "content_note_resource_limit" => "content_note_resource_limit",
        _ => "invalid_note_payload",
    };
    notes::NoteSyncError::InvalidSealState(code)
}
fn new_scope(v: &Value) -> bool {
    v["source_type"] == "project" && (!v["stage_id"].is_null() || v["content_format"] != "html")
}
fn timestamp(value: &str) -> Result<String, String> {
    if metadata::timestamp(value) {
        return Ok(value.into());
    }
    let body = value.strip_suffix('Z').ok_or("invalid_note_payload")?;
    let (seconds, fraction) = body.split_once('.').unwrap_or((body, ""));
    if seconds.len() != 19 || fraction.len() > 6 || !fraction.bytes().all(|b| b.is_ascii_digit()) {
        return Err("invalid_note_payload".into());
    }
    let canonical = format!("{}.{:0<6}Z", seconds, fraction);
    if !metadata::timestamp(&canonical) {
        return Err("invalid_note_payload".into());
    }
    Ok(canonical)
}
pub(crate) fn wire_note(mut note: Value) -> Result<Value, String> {
    note.as_object_mut()
        .ok_or("invalid_note_payload")?
        .remove("revision");
    for key in ["created_at", "updated_at", "deleted_at"] {
        if let Some(value) = note[key].as_str() {
            let canonical = timestamp(value)?;
            note[key] = json!(canonical);
        }
    }
    Ok(note)
}
fn frame(
    db: &Connection,
    a: &str,
    device: &str,
    snapshot: &str,
    id: &str,
    revision: i64,
    parent: Option<&str>,
    operation: &str,
    now: &str,
) -> Result<Vec<u8>, String> {
    let note = wire_note(serde_json::from_str(snapshot).map_err(|_| "invalid_note_payload")?)?;
    let now = timestamp(now)?;
    let project = note["project_id"].as_str().ok_or("invalid_note_payload")?;
    let view = metadata::authority_view(db, a, project)
        .map_err(|_| "project_metadata_authority_unresolved")?;
    let head = view
        .head_event_id
        .ok_or("project_metadata_authority_unresolved")?;
    let (user,boot):(String,String)=db.query_row("SELECT b.canonical_user_id,p.bootstrap_id FROM cloud_account_bindings b JOIN cloud_sync_project_bootstraps p ON p.account_id=b.local_account_id WHERE b.local_account_id=?1 AND p.project_id=?2",params![a,project],|r|Ok((r.get(0)?,r.get(1)?))).map_err(|_|"project_metadata_authority_unresolved")?;
    let refs: Vec<String> = if let Some(stage) = note["stage_id"].as_str() {
        let mut statement=db.prepare("SELECT event_id FROM cloud_sync_structural_tips WHERE account_id=?1 AND project_id=?2 AND entity_type='stage' AND entity_id=?3 ORDER BY event_id").map_err(|_|"stage_dependency_missing")?;
        let values = statement
            .query_map(params![a, project, stage], |r| r.get(0))
            .map_err(|_| "stage_dependency_missing")?
            .collect::<rusqlite::Result<Vec<String>>>()
            .map_err(|_| "stage_dependency_missing")?;
        if values.len() != 1 {
            return Err("stage_dependency_missing".into());
        }
        values
    } else {
        vec![]
    };
    let root = json!({"version":1,"account_id":user,"device_id":device,
        "dependencies":{"bootstrap_id":boot,"metadata_event_id":head,"stage_event_ids":refs},
        "event":{"version":1,"header":{"event_id":id,"parent_event_id":parent,"entity_type":"note",
            "project_id":project,"entity_id":note["id"],"operation":operation,"revision":revision,"updated_at":now,"deleted_at":if operation=="delete"{note["deleted_at"].clone()}else{Value::Null}},
            "mutation":if operation=="delete"{"delete"}else if parent.is_none(){"create"}else{"update"},"note":note}});
    let bytes = serde_json::to_vec(&root).map_err(|_| "invalid_note_payload")?;
    let mut result = b"WORTA-C1".to_vec();
    result.extend([1, 8, 1, 0]);
    result.extend((bytes.len() as u32).to_be_bytes());
    result.extend((bytes.len() as u32).to_be_bytes());
    result.extend(bytes);
    crate::content_note_sync::decode(&result).map_err(str::to_string)?;
    Ok(result)
}
fn store(
    tx: &Transaction<'_>,
    a: &str,
    device: &str,
    input: notes::PrepareNoteIntent<'_>,
    reserved: Option<&str>,
) -> Result<notes::PreparedNoteIntent, notes::NoteSyncError> {
    let snapshot = input.snapshot_json.to_string();
    let now = input.updated_at.to_string();
    let operation = input.operation.as_str().to_string();
    let intent = notes::prepare_legacy_note_intent_with_id(tx, input, reserved)?
        .ok_or_else(|| error("binding"))?;
    let canonical = frame(
        tx,
        a,
        device,
        &snapshot,
        &intent.event_id,
        intent.revision,
        intent.parent_event_id.as_deref(),
        &operation,
        &now,
    )
    .map_err(error)?;
    let opened = crate::content_note_sync::decode(&canonical).map_err(error)?;
    let blocker = crate::content_note_sync::ready(
        tx,
        a,
        opened.root["account_id"].as_str().unwrap(),
        device,
        &opened,
    )
    .err();
    tx.execute("INSERT INTO cloud_content_note_writer_events(event_id,account_id,canonical_frame,blocker,snapshot_json,mutation_generation) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(event_id) DO UPDATE SET canonical_frame=excluded.canonical_frame,blocker=excluded.blocker,snapshot_json=excluded.snapshot_json,mutation_generation=excluded.mutation_generation",params![intent.event_id,a,canonical,blocker,snapshot,intent.mutation_generation])?;
    Ok(intent)
}
pub(crate) fn normal(
    tx: &Transaction<'_>,
    input: notes::PrepareNoteIntent<'_>,
) -> Result<Option<notes::PreparedNoteIntent>, notes::NoteSyncError> {
    let Some(binding) = notes::resolve_project_cloud_binding(tx, input.project_id)? else {
        return Ok(None);
    };
    let active:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM cloud_content_note_migrations WHERE account_id=?1 AND project_id=?2 AND activated=1 UNION ALL SELECT 1 FROM cloud_content_note_receipts r JOIN cloud_sync_inbox i ON i.account_id=r.account_id AND i.event_id=r.event_id WHERE r.account_id=?1 AND i.project_id=?2 AND r.outcome='applied')",params![binding.account_id,input.project_id],|r|r.get(0))?;
    let candidate_pending:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM cloud_content_note_candidates c JOIN cloud_content_note_writer_events e ON e.event_id=c.event_id LEFT JOIN cloud_content_note_receipts r ON r.account_id=c.account_id AND r.event_id=c.event_id WHERE c.account_id=?1 AND c.project_id=?2 AND c.note_id=?3 AND r.outcome IS NULL)",params![binding.account_id,input.project_id,input.entity_id],|r|r.get(0))?;
    let local_collision:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM cloud_content_note_local_candidates c JOIN cloud_content_note_receipts r ON r.account_id=c.account_id AND r.event_id=c.event_id WHERE c.account_id=?1 AND c.project_id=?2 AND c.note_id=?3 AND r.outcome='waiting')",params![binding.account_id,input.project_id,input.entity_id],|r|r.get(0))?;
    if local_collision {
        return Err(notes::NoteSyncError::InvalidSealState(
            "content_note_local_candidate",
        ));
    }
    let conflict:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_note_conflict_groups WHERE account_id=?1 AND project_id=?2 AND entity_id=?3 AND lifecycle IN ('open','resolving'))",params![binding.account_id,input.project_id,input.entity_id],|r|r.get(0))?;
    if conflict {
        return Err(notes::NoteSyncError::InvalidSealState(
            "unresolved_note_conflict",
        ));
    }
    if candidate_pending {
        tx.execute("INSERT INTO cloud_content_note_pending_local_changes VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(account_id,project_id,note_id) DO UPDATE SET operation=excluded.operation,snapshot_json=excluded.snapshot_json,updated_at=excluded.updated_at",params![binding.account_id,input.project_id,input.entity_id,input.operation.as_str(),input.snapshot_json,input.updated_at])?;
        return Ok(None);
    }
    if !active {
        return Ok(None);
    }
    store(tx, &binding.account_id, &binding.device_id, input, None).map(Some)
}

pub(crate) fn begin(
    db: &mut Connection,
    scope: &metadata::MetadataScope,
    p: &str,
    now: &str,
) -> Result<Value, String> {
    metadata::assert_runtime_scope(
        db,
        &scope.account_id,
        &scope.canonical_user_id,
        &scope.device_id,
    )
    .map_err(|e| e.to_string())?;
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    let bound:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_project_bindings WHERE account_id=?1 AND project_id=?2)",params![scope.account_id,p],|r|r.get(0)).map_err(|e|e.to_string())?;
    if !bound {
        return Err("missing_local_binding".into());
    }
    if metadata::authority_view(&tx, &scope.account_id, p)
        .map_err(|e| e.to_string())?
        .state
        != "active"
    {
        return Err("project_metadata_authority_unresolved".into());
    }
    let existing:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM cloud_content_note_migrations WHERE account_id=?1 AND project_id=?2)",params![scope.account_id,p],|r|r.get(0)).map_err(|e|e.to_string())?;
    if !existing {
        tx.execute(
            "INSERT INTO cloud_content_note_migrations VALUES(?1,?2,?3,0,?4)",
            params![
                scope.account_id,
                p,
                metadata::new_event_id().map_err(|e| e.to_string())?,
                now
            ],
        )
        .map_err(|e| e.to_string())?;
    }
    {
        let rows = {
            let mut q = tx
                .prepare("SELECT id,payload_json FROM notes WHERE project_id=?1 ORDER BY id")
                .map_err(|e| e.to_string())?;
            let rows = q
                .query_map([p], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })
                .map_err(|e| e.to_string())?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(|e| e.to_string())?;
            rows
        };
        for (id, source) in rows {
            let value: Value = serde_json::from_str(&source).map_err(|_| "invalid_note_payload")?;
            if value["source_type"] == "project"
                && value["stage_id"].is_null()
                && value["content_format"] == "html"
            {
                continue;
            }
            let represented:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_note_causal_history WHERE account_id=?1 AND project_id=?2 AND entity_id=?3 UNION ALL SELECT 1 FROM cloud_content_note_writer_events e JOIN cloud_sync_outbox b ON b.account_id=e.account_id AND b.event_id=e.event_id WHERE b.account_id=?1 AND b.project_id=?2 AND b.entity_id=?3 AND b.lifecycle!='superseded')",params![scope.account_id,p,id],|r|r.get(0)).map_err(|e|e.to_string())?;
            if represented {
                continue;
            }
            let prior:Option<(String,Option<String>,Option<Vec<u8>>)>=tx.query_row("SELECT source_json,blocker,canonical_frame FROM cloud_content_note_candidates WHERE account_id=?1 AND project_id=?2 AND note_id=?3 ORDER BY rowid DESC LIMIT 1",params![scope.account_id,p,id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(|e|e.to_string())?;
            if prior
                .as_ref()
                .is_some_and(|(_, _, canonical)| canonical.is_some())
            {
                continue;
            }
            let cid = metadata::new_event_id().map_err(|e| e.to_string())?;
            let mut event_id = if value["source_type"] == "mindmap" {
                None
            } else {
                Some(cid.clone())
            };
            let mut canonical = None;
            let blocker = if value["source_type"] == "mindmap" {
                Some("content_note_map_owned".to_string())
            } else if !new_scope(&value) {
                Some("content_note_unsupported_source".to_string())
            } else {
                // A savepoint keeps a rejected candidate's source while discarding incomplete queue writes.
                tx.execute_batch("SAVEPOINT content_candidate")
                    .map_err(|e| e.to_string())?;
                let result = store(
                    &tx,
                    &scope.account_id,
                    &scope.device_id,
                    notes::PrepareNoteIntent {
                        project_id: p,
                        entity_id: &id,
                        operation: notes::NoteSyncOperation::Upsert,
                        updated_at: value["updated_at"].as_str().ok_or("invalid_note_payload")?,
                        deleted_at: None,
                        snapshot_json: &source,
                        state_updated_at: now,
                    },
                    Some(&cid),
                );
                match result {
                    Ok(intent) => {
                        event_id = Some(intent.event_id.clone());
                        canonical=Some(tx.query_row("SELECT canonical_frame FROM cloud_content_note_writer_events WHERE event_id=?1",[&intent.event_id],|r|r.get::<_,Vec<u8>>(0)).map_err(|e|e.to_string())?);
                        tx.execute_batch("RELEASE content_candidate")
                            .map_err(|e| e.to_string())?;
                        None
                    }
                    Err(e) => {
                        tx.execute_batch(
                            "ROLLBACK TO content_candidate; RELEASE content_candidate",
                        )
                        .map_err(|e| e.to_string())?;
                        Some(
                            match e {
                                notes::NoteSyncError::InvalidSealState(code) => code,
                                _ => "content_note_candidate_blocked",
                            }
                            .into(),
                        )
                    }
                }
            };
            if prior
                .as_ref()
                .is_some_and(|(old, old_blocker, _)| old == &source && old_blocker == &blocker)
            {
                continue;
            }
            let parent = notes::read_entity_sync_head(&tx, &scope.account_id, p, &id)
                .map_err(|e| e.to_string())?;
            let metadata =
                metadata::authority_view(&tx, &scope.account_id, p).map_err(|e| e.to_string())?;
            let stage_refs = {
                let mut q=tx.prepare("SELECT event_id FROM cloud_sync_structural_tips WHERE account_id=?1 AND project_id=?2 AND entity_type='stage' AND entity_id=?3 ORDER BY event_id").map_err(|e|e.to_string())?;
                let values = q
                    .query_map(
                        params![scope.account_id, p, value["stage_id"].as_str()],
                        |r| r.get::<_, String>(0),
                    )
                    .map_err(|e| e.to_string())?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(|e| e.to_string())?;
                values
            };
            let evidence = json!({"parent_event_id":parent.as_ref().map(|h|&h.event_id),"revision":parent.as_ref().map(|h|h.revision+1).unwrap_or(1),"local_revision":value["revision"],"metadata_event_id":metadata.head_event_id,"stage_event_ids":stage_refs,"future_event_id":event_id,"source":value});
            tx.execute("INSERT INTO cloud_content_note_candidates VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",params![cid,scope.account_id,p,id,scope.device_id,event_id,source,canonical,blocker,now,evidence.to_string()]).map_err(|e|e.to_string())?;
        }
    }
    tx.commit().map_err(|e| e.to_string())?;
    view(db, scope, p)
}

pub(crate) fn view(
    db: &Connection,
    scope: &metadata::MetadataScope,
    p: &str,
) -> Result<Value, String> {
    metadata::assert_runtime_scope(
        db,
        &scope.account_id,
        &scope.canonical_user_id,
        &scope.device_id,
    )
    .map_err(|e| e.to_string())?;
    let imported:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM cloud_content_note_receipts r JOIN cloud_sync_inbox i ON i.account_id=r.account_id AND i.event_id=r.event_id WHERE r.account_id=?1 AND i.project_id=?2 AND r.outcome='applied')",params![scope.account_id,p],|r|r.get(0)).map_err(|e|e.to_string())?;
    let conflict:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_note_conflict_groups WHERE account_id=?1 AND project_id=?2 AND lifecycle='open' UNION ALL SELECT 1 FROM cloud_content_note_local_candidates c JOIN cloud_content_note_receipts r ON r.account_id=c.account_id AND r.event_id=c.event_id WHERE c.account_id=?1 AND c.project_id=?2 AND r.outcome='waiting')",params![scope.account_id,p],|r|r.get(0)).map_err(|e|e.to_string())?;
    let active:Option<bool>=db.query_row("SELECT activated FROM cloud_content_note_migrations WHERE account_id=?1 AND project_id=?2",params![scope.account_id,p],|r|r.get(0)).optional().map_err(|e|e.to_string())?;
    let mut q=db.prepare("SELECT c.note_id,c.blocker,e.event_id,o.lifecycle,r.outcome,e.blocker,json_extract(c.source_json,'$.title') FROM cloud_content_note_candidates c LEFT JOIN cloud_content_note_writer_events e ON e.event_id=c.event_id LEFT JOIN cloud_sync_outbox o ON o.event_id=e.event_id LEFT JOIN cloud_content_note_receipts r ON r.account_id=c.account_id AND r.event_id=c.event_id WHERE c.account_id=?1 AND c.project_id=?2 AND NOT EXISTS(SELECT 1 FROM cloud_content_note_candidates later WHERE later.account_id=c.account_id AND later.project_id=c.project_id AND later.note_id=c.note_id AND later.rowid>c.rowid) ORDER BY c.note_id").map_err(|e|e.to_string())?;
    let candidates=q.query_map(params![scope.account_id,p],|r|Ok(json!({"title":r.get::<_,Option<String>>(6)?,"note_id":r.get::<_,String>(0)?,"blocker":r.get::<_,Option<String>>(1)?.or(r.get(5)?),"event_id":r.get::<_,Option<String>>(2)?,"publication":r.get::<_,Option<String>>(3)?,"outcome":r.get::<_,Option<String>>(4)?}))).map_err(|e|e.to_string())?.collect::<rusqlite::Result<Vec<_>>>().map_err(|e|e.to_string())?;
    let state = if conflict {
        "conflict"
    } else if active.is_none() && !imported {
        "content_local"
    } else if candidates
        .iter()
        .any(|c| !c["blocker"].is_null() && c["blocker"] != "content_note_map_owned")
    {
        "blocked"
    } else if candidates
        .iter()
        .any(|c| c["publication"] == "accepted" && c["outcome"].is_null())
    {
        "published_self_echo_pending"
    } else if candidates
        .iter()
        .any(|c| !c["event_id"].is_null() && c["outcome"].is_null())
    {
        "publication_pending"
    } else if active == Some(true) || imported {
        "active"
    } else {
        "candidate_captured"
    };
    Ok(json!({"state":state,"candidates":candidates,"activated":active.unwrap_or(false)||imported}))
}

fn ordinary_parent_ready(
    db: &Connection,
    a: &str,
    event: &crate::content_note_sync::ContentNote,
) -> Result<(), String> {
    if event.ordinary.is_none() {
        return Ok(());
    }
    let h = &event.root["event"]["header"];
    let Some(parent) = h["parent_event_id"].as_str() else {
        return Ok(());
    };
    let p = h["project_id"].as_str().ok_or("invalid_note_payload")?;
    let n = h["entity_id"].as_str().ok_or("invalid_note_payload")?;
    let revision = h["revision"].as_i64().ok_or("invalid_note_payload")?;
    let proved:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_note_causal_history WHERE account_id=?1 AND project_id=?2 AND entity_id=?3 AND event_id=?4 AND revision=?5)",params![a,p,n,parent,revision-1],|r|r.get(0)).map_err(|e|e.to_string())?;
    if proved {
        return Ok(());
    }
    let tx = db.unchecked_transaction().map_err(|e| e.to_string())?;
    let applied = notes::applied_resolution_parent_revision(&tx, a, p, n, parent)
        .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    if applied == Some(revision - 1) {
        Ok(())
    } else {
        Err("content_note_parent_pending".into())
    }
}

pub(crate) fn pending(
    db: &Connection,
    scope: &metadata::MetadataScope,
    sealed: bool,
) -> Result<Vec<Value>, String> {
    metadata::assert_runtime_scope(
        db,
        &scope.account_id,
        &scope.canonical_user_id,
        &scope.device_id,
    )
    .map_err(|e| e.to_string())?;
    let tx = db.unchecked_transaction().map_err(|e| e.to_string())?;
    let drafts = {
        let mut q=tx.prepare("SELECT d.project_id,d.note_id,d.operation,d.snapshot_json,d.updated_at FROM cloud_content_note_pending_local_changes d WHERE d.account_id=?1 AND EXISTS(SELECT 1 FROM cloud_content_note_candidates c JOIN cloud_content_note_receipts r ON r.account_id=c.account_id AND r.event_id=c.event_id WHERE c.account_id=d.account_id AND c.project_id=d.project_id AND c.note_id=d.note_id AND r.outcome='applied') ORDER BY d.project_id,d.note_id LIMIT 8").map_err(|e|e.to_string())?;
        let rows = q
            .query_map([&scope.account_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                ))
            })
            .map_err(|e| e.to_string())?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| e.to_string())?;
        rows
    };
    for (p, id, operation, snapshot, now) in drafts {
        let input = notes::PrepareNoteIntent {
            project_id: &p,
            entity_id: &id,
            operation: if operation == "delete" {
                notes::NoteSyncOperation::Delete
            } else {
                notes::NoteSyncOperation::Upsert
            },
            updated_at: &now,
            deleted_at: if operation == "delete" {
                Some(now.as_str())
            } else {
                None
            },
            snapshot_json: &snapshot,
            state_updated_at: &now,
        };
        store(&tx, &scope.account_id, &scope.device_id, input, None).map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM cloud_content_note_pending_local_changes WHERE account_id=?1 AND project_id=?2 AND note_id=?3",params![scope.account_id,p,id]).map_err(|e|e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())?;
    let cursor:String=db.query_row("SELECT last_event_id FROM cloud_content_note_writer_scan WHERE account_id=?1 AND device_id=?2 AND sealed=?3",params![scope.account_id,scope.device_id,sealed],|r|r.get(0)).optional().map_err(|e|e.to_string())?.unwrap_or_default();
    let mut q=db.prepare("SELECT * FROM (SELECT e.event_id,e.canonical_frame,o.nonce,o.ciphertext FROM cloud_content_note_writer_events e JOIN cloud_sync_outbox b ON b.account_id=e.account_id AND b.event_id=e.event_id LEFT JOIN cloud_sync_event_objects o ON o.account_id=e.account_id AND o.event_id=e.event_id WHERE e.account_id=?1 AND b.device_id=?2 AND b.lifecycle=?3 UNION ALL SELECT e.event_id,e.canonical_frame,o.nonce,o.ciphertext FROM cloud_content_note_resolution_events e JOIN cloud_sync_note_resolution_outbox b ON b.account_id=e.account_id AND b.resolution_event_id=e.event_id LEFT JOIN cloud_sync_event_objects o ON o.account_id=e.account_id AND o.event_id=e.event_id WHERE e.account_id=?1 AND b.device_id=?2 AND b.lifecycle=?4  ) ORDER BY CASE WHEN event_id>?5 THEN 0 ELSE 1 END,event_id LIMIT 8").map_err(|e|e.to_string())?;
    let rows = q
        .query_map(
            params![
                scope.account_id,
                scope.device_id,
                if sealed { "sealed" } else { "unsealed" },
                if sealed {
                    "sealed_local"
                } else {
                    "local_pending"
                },
                cursor
            ],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Vec<u8>>(1)?,
                    r.get::<_, Option<Vec<u8>>>(2)?,
                    r.get::<_, Option<Vec<u8>>>(3)?,
                ))
            },
        )
        .map_err(|e| e.to_string())?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| e.to_string())?;
    if let Some((id, _, _, _)) = rows.last() {
        db.execute("INSERT INTO cloud_content_note_writer_scan VALUES(?1,?2,?3,?4) ON CONFLICT(account_id,device_id,sealed) DO UPDATE SET last_event_id=excluded.last_event_id",params![scope.account_id,scope.device_id,sealed,id]).map_err(|e|e.to_string())?;
    }
    let mut result = vec![];
    for (id, bytes, nonce, ciphertext) in rows {
        let event = crate::content_note_sync::decode(&bytes).map_err(str::to_string)?;
        if event.resolution.is_some() {
            let tx = db.unchecked_transaction().map_err(|e| e.to_string())?;
            let ready =
                notes::resolution_dependencies_ready_for(&tx, &scope.account_id, &id, !sealed)
                    .map_err(|e| e.to_string())?;
            tx.commit().map_err(|e| e.to_string())?;
            if !ready {
                continue;
            }
        }
        let blocker = crate::content_note_sync::ready(
            db,
            &scope.account_id,
            &scope.canonical_user_id,
            &scope.device_id,
            &event,
        )
        .and_then(|_| ordinary_parent_ready(db, &scope.account_id, &event))
        .err();
        db.execute(
            if event.resolution.is_some() {
                "UPDATE cloud_content_note_resolution_events SET blocker=?2 WHERE event_id=?1"
            } else {
                "UPDATE cloud_content_note_writer_events SET blocker=?2 WHERE event_id=?1"
            },
            params![id, blocker],
        )
        .map_err(|e| e.to_string())?;
        if blocker.is_none() {
            result.push(json!({"event_id":id,"frame":bytes,"nonce":nonce,"ciphertext":ciphertext}))
        }
    }
    Ok(result)
}

pub(crate) fn seal(
    db: &mut Connection,
    scope: &metadata::MetadataScope,
    id: &str,
    expected: &[u8],
    envelope: notes::EncryptedNoteSyncEnvelope,
) -> Result<(), String> {
    let compression_view =
        crate::frame_compression::canonical_view(expected).map_err(str::to_owned)?;
    let expected = compression_view.as_ref();
    metadata::assert_runtime_scope(
        db,
        &scope.account_id,
        &scope.canonical_user_id,
        &scope.device_id,
    )
    .map_err(|e| e.to_string())?;
    let resolution:Option<Vec<u8>>=db.query_row("SELECT c.canonical_frame FROM cloud_content_note_resolution_events c JOIN cloud_sync_note_resolution_outbox b ON b.resolution_event_id=c.event_id WHERE c.account_id=?1 AND c.event_id=?2 AND b.device_id=?3",params![scope.account_id,id,scope.device_id],|r|r.get(0)).optional().map_err(|e|e.to_string())?;
    if let Some(stored) = resolution {
        if stored != expected {
            return Err("stale_content_note_frame".into());
        }
        let event = crate::content_note_sync::decode(expected).map_err(str::to_string)?;
        crate::content_note_sync::ready(
            db,
            &scope.account_id,
            &scope.canonical_user_id,
            &scope.device_id,
            &event,
        )?;
        let h = &event.root["event"]["header"];
        let payload = serde_json::to_vec(&event.root["event"]).unwrap();
        notes::commit_sealed_note_resolution_event(
            db,
            &notes::CommitSealedNoteResolutionCommand {
                event_id: id.into(),
                account_id: scope.account_id.clone(),
                canonical_user_id: scope.canonical_user_id.clone(),
                device_id: scope.device_id.clone(),
                project_id: h["project_id"].as_str().unwrap().into(),
                entity_id: h["entity_id"].as_str().unwrap().into(),
                expected_canonical_payload: notes::encode_canonical_base64url(&payload),
                envelope,
            },
        )
        .map_err(|e| e.to_string())?;
        return Ok(());
    }
    let (stored,generation):(Vec<u8>,Option<i64>)=db.query_row("SELECT c.canonical_frame,i.mutation_generation FROM cloud_content_note_writer_events c JOIN cloud_sync_outbox b ON b.event_id=c.event_id LEFT JOIN cloud_sync_note_intents i ON i.event_id=c.event_id WHERE c.account_id=?1 AND c.event_id=?2 AND b.device_id=?3",params![scope.account_id,id,scope.device_id],|r|Ok((r.get(0)?,r.get(1)?))).map_err(|e|e.to_string())?;
    if stored != expected {
        return Err("stale_content_note_frame".into());
    }
    let opened = crate::content_note_sync::decode(expected).map_err(str::to_string)?;
    crate::content_note_sync::ready(
        db,
        &scope.account_id,
        &scope.canonical_user_id,
        &scope.device_id,
        &opened,
    )?;
    ordinary_parent_ready(db, &scope.account_id, &opened)?;
    notes::commit_sealed_note_sync_event(
        db,
        &notes::CommitSealedNoteSyncEventCommand {
            event_id: id.into(),
            expected_mutation_generation: generation.unwrap_or(1),
            envelope,
        },
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}
pub(crate) fn receipt(
    db: &mut Connection,
    scope: &metadata::MetadataScope,
    id: &str,
    sequence: i64,
    duplicate: bool,
) -> Result<(), String> {
    metadata::assert_runtime_scope(
        db,
        &scope.account_id,
        &scope.canonical_user_id,
        &scope.device_id,
    )
    .map_err(|e| e.to_string())?;
    let owned:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM cloud_content_note_writer_events c JOIN cloud_sync_outbox b ON b.event_id=c.event_id WHERE c.account_id=?1 AND c.event_id=?2 AND b.device_id=?3)",params![scope.account_id,id,scope.device_id],|r|r.get(0)).map_err(|e|e.to_string())?;
    if !owned {
        let resolution:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM cloud_content_note_resolution_events c JOIN cloud_sync_note_resolution_outbox b ON b.resolution_event_id=c.event_id WHERE c.account_id=?1 AND c.event_id=?2 AND b.device_id=?3)",params![scope.account_id,id,scope.device_id],|r|r.get(0)).map_err(|e|e.to_string())?;
        if !resolution {
            return Err("content_note_scope_mismatch".into());
        }
        notes::commit_note_resolution_upload_acceptance(
            db,
            &notes::CommitNoteResolutionUploadAcceptanceCommand {
                account_id: scope.account_id.clone(),
                canonical_user_id: scope.canonical_user_id.clone(),
                device_id: scope.device_id.clone(),
                receipts: vec![notes::NoteSyncUploadReceipt {
                    event_id: id.into(),
                    server_sequence: sequence,
                    duplicate,
                }],
            },
        )
        .map_err(|e| e.to_string())?;
        return Ok(());
    }
    notes::commit_note_sync_upload_acceptance(
        db,
        &notes::CommitNoteSyncUploadAcceptanceCommand {
            account_id: scope.account_id.clone(),
            device_id: scope.device_id.clone(),
            receipts: vec![notes::NoteSyncUploadReceipt {
                event_id: id.into(),
                server_sequence: sequence,
                duplicate,
            }],
        },
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Local conflict snapshots never leave the native IPC boundary unencrypted.
pub(crate) fn conflicts(
    db: &Connection,
    scope: &metadata::MetadataScope,
    p: &str,
) -> Result<Vec<Value>, String> {
    metadata::assert_runtime_scope(
        db,
        &scope.account_id,
        &scope.canonical_user_id,
        &scope.device_id,
    )
    .map_err(|e| e.to_string())?;
    let mut q=db.prepare("SELECT group_id,entity_id,generation FROM cloud_sync_note_conflict_groups WHERE account_id=?1 AND project_id=?2 AND lifecycle='open' ORDER BY entity_id LIMIT 32").map_err(|e|e.to_string())?;
    let groups = q
        .query_map(params![scope.account_id, p], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })
        .map_err(|e| e.to_string())?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| e.to_string())?;
    let mut result = import_conflicts(db, scope, p)?;
    for (group, id, generation) in groups {
        let mut q=db.prepare("SELECT t.event_id,v.operation,v.revision,v.snapshot_json FROM cloud_sync_note_conflict_tips t JOIN cloud_sync_note_conflict_versions v ON v.version_id=t.version_id AND v.group_id=t.group_id WHERE t.group_id=?1 ORDER BY t.event_id").map_err(|e|e.to_string())?;
        let tips = q
            .query_map([&group], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })
            .map_err(|e| e.to_string())?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| e.to_string())?;
        let mut versions = vec![];
        for (event, operation, revision, snapshot) in tips {
            let n =
                wire_note(serde_json::from_str(&snapshot).map_err(|_| "invalid_note_payload")?)?;
            if !new_scope(&n) {
                continue;
            }
            versions
                .push(json!({"event_id":event,"operation":operation,"revision":revision,"note":n}));
        }
        if versions.is_empty() {
            continue;
        }
        let current: Option<String> = db
            .query_row(
                "SELECT payload_json FROM notes WHERE project_id=?1 AND id=?2",
                params![p, id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        let local = current
            .as_deref()
            .map(serde_json::from_str::<Value>)
            .transpose()
            .map_err(|_| "invalid_note_payload")?
            .unwrap_or(Value::Null);
        result.push(json!({"group_id":group,"note_id":id,"generation":generation,"local":local,"versions":versions}));
    }
    Ok(result)
}
pub(crate) fn prepare_choice(
    db: &mut Connection,
    scope: &metadata::MetadataScope,
    p: &str,
    decision: &Value,
    selected: &str,
    now: &str,
) -> Result<notes::PrepareNoteConflictResolutionCommand, String> {
    let groups = conflicts(db, scope, p)?;
    let current = groups
        .iter()
        .find(|g| g["group_id"] == decision["group_id"])
        .ok_or("stale_note_resolution")?;
    if current != decision {
        return Err("stale_note_resolution".into());
    }
    let tips = decision["versions"]
        .as_array()
        .ok_or("invalid_note_payload")?;
    let chosen = tips
        .iter()
        .find(|v| v["event_id"] == selected)
        .ok_or("stale_note_resolution")?;
    let ids: Vec<&str> = tips
        .iter()
        .map(|v| v["event_id"].as_str().unwrap())
        .collect();
    let revision = tips
        .iter()
        .filter_map(|v| v["revision"].as_i64())
        .max()
        .ok_or("invalid_note_payload")?
        + 1;
    let inner = json!({"version":2,"header":{"event_id":metadata::new_event_id().map_err(|e|e.to_string())?,"parent_event_id":ids[0],"additional_parent_event_ids":ids[1..],"project_id":p,"entity_id":decision["note_id"],"entity_type":"note","operation":"resolution","revision":revision,"updated_at":now},"mutation":"resolution","resolution":{"conflict_group_id":decision["group_id"],"conflict_generation":decision["generation"],"resolved_event_ids":ids,"strategy":"choose_version","selected_event_id":selected},"result":{"operation":chosen["operation"],"note":chosen["note"]}});
    let command = notes::PrepareNoteConflictResolutionCommand {
        account_id: scope.account_id.clone(),
        canonical_user_id: scope.canonical_user_id.clone(),
        device_id: scope.device_id.clone(),
        canonical_payload: serde_json::to_vec(&inner).map_err(|_| "invalid_note_payload")?,
    };
    notes::prepare_note_conflict_resolution_with_local(
        db,
        &command,
        false,
        Some(&decision["local"]),
    )
    .map_err(|e| match e {
        notes::PrepareNoteConflictResolutionError::StaleConflict => "stale_note_resolution",
        notes::PrepareNoteConflictResolutionError::MissingCausalProof => {
            "note_resolution_missing_proof"
        }
        _ => "note_resolution_invalid",
    })?;
    Ok(command)
}

pub(crate) fn import_conflicts(
    db: &Connection,
    scope: &metadata::MetadataScope,
    p: &str,
) -> Result<Vec<Value>, String> {
    metadata::assert_runtime_scope(
        db,
        &scope.account_id,
        &scope.canonical_user_id,
        &scope.device_id,
    )
    .map_err(|e| e.to_string())?;
    let mut q=db.prepare("SELECT c.event_id,c.note_id,c.local_snapshot,r.canonical_frame FROM cloud_content_note_local_candidates c JOIN cloud_content_note_receipts r ON r.account_id=c.account_id AND r.event_id=c.event_id WHERE c.account_id=?1 AND c.project_id=?2 AND r.outcome='waiting' ORDER BY c.note_id LIMIT 32").map_err(|e|e.to_string())?;
    let rows = q
        .query_map(params![scope.account_id, p], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Vec<u8>>(3)?,
            ))
        })
        .map_err(|e| e.to_string())?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| e.to_string())?;
    rows.into_iter().map(|(event,id,_source,frame)|{
        let opened=crate::content_note_sync::decode(&frame).map_err(str::to_string)?;
        let current:Option<String>=db.query_row("SELECT payload_json FROM notes WHERE project_id=?1 AND id=?2",params![p,id],|r|r.get(0)).optional().map_err(|e|e.to_string())?;
        let local=current.as_deref().map(serde_json::from_str::<Value>).transpose().map_err(|_|"invalid_note_payload")?.unwrap_or(Value::Null);
        Ok(json!({"kind":"import","group_id":event,"note_id":id,"generation":0,"local":local,"versions":[{"event_id":"local","operation":"upsert","revision":0,"note":local},{"event_id":event,"operation":"upsert","revision":1,"note":opened.root["event"]["note"]}]}))
    }).collect()
}
pub(crate) fn prepare_import_choice(
    db: &mut Connection,
    scope: &metadata::MetadataScope,
    p: &str,
    decision: &Value,
    selected: &str,
    now: &str,
) -> Result<notes::ApplyVerifiedReceivedNoteIpcCommand, String> {
    let groups = import_conflicts(db, scope, p)?;
    if !groups.iter().any(|g| g == decision)
        || (selected != "local" && Some(selected) != decision["group_id"].as_str())
    {
        return Err("stale_note_resolution".into());
    }
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    let id = decision["group_id"]
        .as_str()
        .ok_or("invalid_note_payload")?;
    let raw: Option<String> = tx
        .query_row(
            "SELECT payload_json FROM notes WHERE id=?1 AND project_id=?2",
            params![decision["note_id"].as_str().unwrap(), p],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    if raw
        .as_deref()
        .map(serde_json::from_str::<Value>)
        .transpose()
        .map_err(|_| "invalid_note_payload")?
        .unwrap_or(Value::Null)
        != decision["local"]
    {
        return Err("stale_note_resolution".into());
    }
    let sealed:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_outbox WHERE account_id=?1 AND project_id=?2 AND entity_id=?3 AND lifecycle IN ('sealed','accepted'))",params![scope.account_id,p,decision["note_id"].as_str().unwrap()],|r|r.get(0)).map_err(|e|e.to_string())?;
    if sealed {
        return Err("content_note_local_candidate".into());
    }
    tx.execute("UPDATE cloud_sync_outbox SET lifecycle='superseded' WHERE account_id=?1 AND project_id=?2 AND entity_id=?3 AND lifecycle='unsealed'",params![scope.account_id,p,decision["note_id"].as_str().unwrap()]).map_err(|e|e.to_string())?;
    tx.execute(
        "INSERT INTO cloud_content_note_import_decisions VALUES(?1,?2,?3,?4,?5) ON CONFLICT(account_id,event_id,expected_local,keep_local,updated_at) DO NOTHING",
        params![
            scope.account_id,
            id,
            raw.ok_or("stale_note_resolution")?,
            selected == "local",
            now
        ],
    )
    .map_err(|e| e.to_string())?;
    let (sequence,source,frame,nonce,ciphertext):(i64,String,Vec<u8>,Vec<u8>,Vec<u8>)=tx.query_row("SELECT i.server_sequence,i.device_id,r.canonical_frame,r.nonce,r.ciphertext FROM cloud_content_note_receipts r JOIN cloud_sync_inbox i ON i.account_id=r.account_id AND i.event_id=r.event_id WHERE r.account_id=?1 AND r.event_id=?2 AND r.outcome='waiting'",params![scope.account_id,id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).map_err(|e|e.to_string())?;
    tx.execute("UPDATE cloud_sync_inbox SET state='received',error_code=NULL WHERE account_id=?1 AND event_id=?2 AND state='conflict'",params![scope.account_id,id]).map_err(|e|e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(notes::ApplyVerifiedReceivedNoteIpcCommand {
        account_id: scope.account_id.clone(),
        canonical_user_id: scope.canonical_user_id.clone(),
        pulling_device_id: scope.device_id.clone(),
        event_id: id.into(),
        server_sequence: sequence,
        source_device_id: source,
        crypto_version: 1,
        aad_version: 1,
        nonce,
        ciphertext,
        plaintext: frame,
    })
}
pub(crate) fn finish_local_import(
    tx: &Transaction<'_>,
    a: &str,
    event: &str,
) -> Result<(), String> {
    let row:Option<(String,bool,String)>=tx.query_row("SELECT expected_local,keep_local,updated_at FROM cloud_content_note_import_decisions WHERE account_id=?1 AND event_id=?2 ORDER BY rowid DESC LIMIT 1",params![a,event],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(|e|e.to_string())?;
    let Some((local, true, now)) = row else {
        return Ok(());
    };
    let mut note: Value = serde_json::from_str(&local).map_err(|_| "invalid_note_payload")?;
    note["updated_at"] = json!(now);
    note["revision"] = json!(note["revision"].as_i64().unwrap_or(0) + 1);
    let p = note["project_id"].as_str().ok_or("invalid_note_payload")?;
    let id = note["id"].as_str().ok_or("invalid_note_payload")?;
    let snapshot = note.to_string();
    notes::prepare_unsealed_note_intent(
        tx,
        notes::PrepareNoteIntent {
            project_id: p,
            entity_id: id,
            operation: notes::NoteSyncOperation::Upsert,
            updated_at: &now,
            deleted_at: None,
            snapshot_json: &snapshot,
            state_updated_at: &now,
        },
    )
    .map_err(|e| e.to_string())?;
    tx.execute(
        "UPDATE notes SET updated_at=?1,payload_json=?2 WHERE project_id=?3 AND id=?4",
        params![now, snapshot, p, id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    const NOW: &str = "2026-10-02T00:00:00.000000Z";
    fn setup() -> (Connection, std::path::PathBuf, metadata::MetadataScope) {
        let (mut db, path) = crate::account_catalog::tests::setup();
        crate::account_catalog::tests::seed(&mut db);
        let (user,device):(String,String)=db.query_row("SELECT b.canonical_user_id,s.device_id FROM cloud_account_bindings b JOIN cloud_sync_state s ON s.account_id=b.local_account_id WHERE s.account_id='a'",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
        (
            db,
            path,
            metadata::MetadataScope {
                account_id: "a".into(),
                canonical_user_id: user,
                device_id: device,
            },
        )
    }
    fn source(id: &str) -> Value {
        let mut n: Value = serde_json::from_str::<Value>(include_str!(
            "../../src/cloud/__fixtures__/contentNoteCodecV1.json"
        ))
        .unwrap()["examples"][0]["event"]["event"]["note"]
            .clone();
        n["id"] = json!(id);
        n["project_id"] = json!("C1");
        n["stage_id"] = Value::Null;
        n["content_format"] = json!("plain");
        n["updated_at"] = json!(NOW);
        n["revision"] = json!(0);
        n
    }
    #[test]
    fn explicit_capture_excludes_map_and_replays_after_reopen() {
        let (mut db, path, scope) = setup();
        let n = source("P1");
        db.execute("INSERT INTO notes(id,project_id,stage_id,updated_at,payload_json) VALUES('P1','C1',NULL,?1,?2)",params![NOW,n.to_string()]).unwrap();
        let tx = db.transaction().unwrap();
        assert!(notes::prepare_unsealed_note_intent(
            &tx,
            notes::PrepareNoteIntent {
                project_id: "C1",
                entity_id: "P1",
                operation: notes::NoteSyncOperation::Upsert,
                updated_at: NOW,
                deleted_at: None,
                snapshot_json: &n.to_string(),
                state_updated_at: NOW
            }
        )
        .unwrap()
        .is_none());
        tx.commit().unwrap();
        let mut map = source("M1");
        map["source_type"] = json!("mindmap");
        map["source_map_id"] = json!("map");
        map["source_node_id"] = json!("node");
        db.execute("INSERT INTO notes(id,project_id,stage_id,updated_at,payload_json) VALUES('M1','C1',NULL,?1,?2)",params![NOW,map.to_string()]).unwrap();
        assert_eq!(view(&db, &scope, "C1").unwrap()["state"], "content_local");
        let captured = begin(&mut db, &scope, "C1", NOW).unwrap();
        assert_eq!(captured["state"], "publication_pending");
        let queued = pending(&db, &scope, false).unwrap();
        assert_eq!(queued.len(), 1);
        assert_eq!(
            notes::list_unsealed_note_sync_intents(&mut db, 8, false)
                .unwrap()
                .len(),
            0
        );
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM cloud_sync_outbox WHERE entity_id='M1'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        let frame = queued[0]["frame"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_u64().unwrap() as u8)
            .collect::<Vec<_>>();
        let opened = crate::content_note_sync::decode(&frame).unwrap();
        assert_eq!(opened.root["event"]["note"]["id"], "P1");
        let id = queued[0]["event_id"].as_str().unwrap();
        let envelope = notes::EncryptedNoteSyncEnvelope {
            crypto_version: 1,
            aad_version: 1,
            nonce: notes::encode_canonical_base64url(&[5; 24]),
            ciphertext: notes::encode_canonical_base64url(&vec![7; frame.len() + 16]),
        };
        seal(&mut db, &scope, id, &frame, envelope).unwrap();
        let sealed = pending(&db, &scope, true).unwrap();
        assert_eq!(sealed.len(), 1);
        let mut edited = n.clone();
        edited["content"] = json!("Local edit after sealing");
        edited["revision"] = json!(1);
        let tx = db.transaction().unwrap();
        assert!(normal(
            &tx,
            notes::PrepareNoteIntent {
                project_id: "C1",
                entity_id: "P1",
                operation: notes::NoteSyncOperation::Upsert,
                updated_at: NOW,
                deleted_at: None,
                snapshot_json: &edited.to_string(),
                state_updated_at: NOW
            }
        )
        .unwrap()
        .is_none());
        tx.execute(
            "UPDATE notes SET payload_json=?1 WHERE id='P1'",
            [edited.to_string()],
        )
        .unwrap();
        tx.commit().unwrap();
        assert_eq!(pending(&db, &scope, true).unwrap(), sealed);
        drop(db);
        let mut db = crate::sqlite::open_database(&path).unwrap();
        assert_eq!(pending(&db, &scope, true).unwrap(), sealed);
        assert_eq!(db.query_row("SELECT snapshot_json FROM cloud_content_note_pending_local_changes WHERE note_id='P1'",[],|r|r.get::<_,String>(0)).unwrap(),edited.to_string());
        assert_eq!(
            begin(&mut db, &scope, "C1", NOW).unwrap()["state"],
            captured["state"]
        );
        receipt(&mut db, &scope, id, 3, false).unwrap();
        assert_eq!(
            view(&db, &scope, "C1").unwrap()["state"],
            "published_self_echo_pending"
        );
        assert_eq!(view(&db, &scope, "C1").unwrap()["activated"], false);
        assert_eq!(
            db.query_row("SELECT payload_json FROM notes WHERE id='M1'", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap(),
            map.to_string()
        );
    }
    #[test]
    fn repeat_capture_skips_ordinary_writer_and_child_waits_for_parent_proof() {
        let (mut db, _, scope) = setup();
        begin(&mut db, &scope, "C1", NOW).unwrap();
        db.execute("UPDATE cloud_content_note_migrations SET activated=1", [])
            .unwrap();
        let n = source("P2");
        let tx = db.transaction().unwrap();
        let intent = normal(
            &tx,
            notes::PrepareNoteIntent {
                project_id: "C1",
                entity_id: "P2",
                operation: notes::NoteSyncOperation::Upsert,
                updated_at: NOW,
                deleted_at: None,
                snapshot_json: &n.to_string(),
                state_updated_at: NOW,
            },
        )
        .unwrap()
        .unwrap();
        tx.execute("INSERT INTO notes(id,project_id,stage_id,updated_at,payload_json) VALUES('P2','C1',NULL,?1,?2)",params![NOW,n.to_string()]).unwrap();
        let child = frame(
            &tx,
            &scope.account_id,
            &scope.device_id,
            &n.to_string(),
            &metadata::new_event_id().unwrap(),
            2,
            Some(&intent.event_id),
            "upsert",
            NOW,
        )
        .unwrap();
        tx.commit().unwrap();
        let opened = crate::content_note_sync::decode(&child).unwrap();
        assert_eq!(
            ordinary_parent_ready(&db, &scope.account_id, &opened).unwrap_err(),
            "content_note_parent_pending"
        );
        begin(&mut db, &scope, "C1", NOW).unwrap();
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM cloud_content_note_candidates",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM cloud_content_note_writer_events",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
    }
    #[test]
    fn active_content_migration_keeps_project_html_edits_on_legacy_writer() {
        for _ in 0..2 {
            let (mut db, path, scope) = setup();
            begin(&mut db, &scope, "C1", NOW).unwrap();
            db.execute("UPDATE cloud_content_note_migrations SET activated=1", [])
                .unwrap();
            let mut n = source("legacy-html");
            n["content_format"] = json!("html");
            let snapshot = n.to_string();
            let tx = db.transaction().unwrap();
            let intent = notes::prepare_unsealed_note_intent(
                &tx,
                notes::PrepareNoteIntent {
                    project_id: "C1",
                    entity_id: "legacy-html",
                    operation: notes::NoteSyncOperation::Upsert,
                    updated_at: NOW,
                    deleted_at: None,
                    snapshot_json: &snapshot,
                    state_updated_at: NOW,
                },
            )
            .unwrap();
            assert!(intent.is_some());
            assert_eq!(
                tx.query_row(
                    "SELECT COUNT(*) FROM cloud_content_note_writer_events",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
                0
            );
            tx.commit().unwrap();
            assert_eq!(
                notes::list_unsealed_note_sync_intents(&mut db, 8, false)
                    .unwrap()
                    .len(),
                1
            );
            drop(db);
            std::fs::remove_file(path).unwrap();
        }
    }
    #[test]
    fn local_only_project_never_acquires_authority() {
        let (mut db, _, scope) = setup();
        assert_eq!(
            begin(&mut db, &scope, "L1", NOW).unwrap_err(),
            "missing_local_binding"
        );
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM cloud_content_note_migrations",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }
}

pub(crate) fn wrap_resolution(
    tx: &Transaction<'_>,
    a: &str,
    device: &str,
    payload: &[u8],
) -> Result<(), notes::NoteSyncError> {
    let inner: Value = serde_json::from_slice(payload).map_err(error)?;
    let n = &inner["result"]["note"];
    if !new_scope(n) {
        return Ok(());
    }
    let h = &inner["header"];
    let id = h["event_id"].as_str().ok_or_else(|| error("identity"))?;
    let op = inner["result"]["operation"]
        .as_str()
        .ok_or_else(|| error("operation"))?;
    let updated = if op == "delete" {
        n["deleted_at"].as_str()
    } else {
        n["updated_at"].as_str()
    }
    .ok_or_else(|| error("timestamp"))?;
    let bytes = frame(
        tx,
        a,
        device,
        &n.to_string(),
        id,
        h["revision"].as_i64().unwrap(),
        h["parent_event_id"].as_str(),
        op,
        updated,
    )
    .map_err(error)?;
    let mut root: Value = serde_json::from_slice(&bytes[20..]).map_err(error)?;
    root["event"] = inner.clone();
    let raw = serde_json::to_vec(&root).map_err(error)?;
    let mut bytes = b"WORTA-C1".to_vec();
    bytes.extend([1, 8, 1, 0]);
    bytes.extend((raw.len() as u32).to_be_bytes());
    bytes.extend((raw.len() as u32).to_be_bytes());
    bytes.extend(raw);
    let event = crate::content_note_sync::decode(&bytes).map_err(error)?;
    let blocker = crate::content_note_sync::ready(
        tx,
        a,
        root["account_id"].as_str().unwrap(),
        device,
        &event,
    )
    .err();
    tx.execute(
        "INSERT INTO cloud_content_note_resolution_events VALUES(?1,?2,?3,?4)",
        params![id, a, bytes, blocker],
    )?;
    Ok(())
}
