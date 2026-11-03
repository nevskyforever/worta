//! Closed native boundary shared by the app and headless production acceptance.
use crate::{game_migration, game_sync, game_transport, project_metadata_sync as metadata};
use rusqlite::{params, Connection, TransactionBehavior};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    View,
    Begin {
        now: String,
    },
    Pending {
        sealed: bool,
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
        limit: i64,
    },
    Apply {
        frame: Vec<u8>,
        nonce: Vec<u8>,
        ciphertext: Vec<u8>,
        project: bool,
    },
    Block {
        event_id: String,
        nonce: Vec<u8>,
        ciphertext: Vec<u8>,
        code: String,
    },
    Rebuild {
        owner_key: String,
    },
    Decide {
        owner_key: String,
        expected_tips: Vec<String>,
        expected_local: Value,
        selected_event_id: String,
        now: String,
    },
    Compensate {
        target_action_id: String,
        expected_tips: Vec<String>,
        expected_local: Value,
        now: String,
    },
}
fn sql(_: rusqlite::Error) -> String {
    "game_storage_error".into()
}
pub fn assert_scope(db: &Connection, scope: &metadata::MetadataScope) -> Result<(), String> {
    metadata::assert_runtime_scope(
        db,
        &scope.account_id,
        &scope.canonical_user_id,
        &scope.device_id,
    )
    .map_err(|_| "game_scope_mismatch".to_string())?;
    let owner: String = db
        .query_row(
            "SELECT owner FROM storage_ownership WHERE subsystem='game'",
            [],
            |r| r.get(0),
        )
        .map_err(sql)?;
    if owner != "sqlite" {
        return Err("game_codec_not_activated".into());
    }
    Ok(())
}
pub fn view(db: &Connection, scope: &metadata::MetadataScope) -> Result<Value, String> {
    assert_scope(db, scope)?;
    let mut q=db.prepare("SELECT m.owner_key,m.lifecycle,m.blocker,p.snapshot_json FROM cloud_game_migrations m LEFT JOIN cloud_game_projection p USING(account_id,owner_key) WHERE m.account_id=?1 ORDER BY m.owner_key LIMIT 4097").map_err(sql)?;
    let records = q
        .query_map([&scope.account_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })
        .map_err(sql)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(sql)?;
    if records.len() > 4096 {
        return Err("game_resource_limit".into());
    }
    let mut owners = Vec::new();
    for (key, state, blocker, local) in records {
        let tips = game_sync::tips(db, &scope.account_id, &key)?;
        let mut b=db.prepare("SELECT DISTINCT code FROM cloud_game_blockers WHERE account_id=?1 AND owner_key=?2 ORDER BY code").map_err(sql)?;
        let blockers = b
            .query_map(params![scope.account_id, key], |r| r.get::<_, String>(0))
            .map_err(sql)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sql)?;
        if tips.len() > 64 {
            return Err("game_resource_limit".into());
        }
        let mut versions = Vec::new();
        for id in &tips {
            versions.push(json!({"event_id":id,"snapshot":game_sync::rebuild_chain(db,&scope.account_id,&key,id)?}));
        }
        owners.push(json!({"owner_key":key,"state":state,"blocker":blocker,"blockers":blockers,"tips":tips,"versions":versions,"local":local.map(|s|serde_json::from_str::<Value>(&s)).transpose().map_err(|_|"invalid_game_payload")?}));
    }
    let mut q=db.prepare("SELECT e.event_id,json_extract(CAST(substr(e.canonical_frame,21) AS TEXT),'$.header.updated_at'),json_extract(CAST(substr(e.canonical_frame,21) AS TEXT),'$.action.reward') FROM cloud_game_rewards r JOIN cloud_game_events e ON e.account_id=r.account_id AND e.event_id=r.event_id JOIN cloud_game_apply_ledger l ON l.account_id=e.account_id AND l.event_id=e.event_id AND l.outcome='applied' WHERE r.account_id=?1 AND NOT EXISTS(SELECT 1 FROM cloud_game_compensations c WHERE c.account_id=r.account_id AND c.target_action_id=r.event_id) ORDER BY e.server_sequence DESC LIMIT 32").map_err(sql)?;
    let rewards=q.query_map([&scope.account_id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?))).map_err(sql)?.collect::<rusqlite::Result<Vec<_>>>().map_err(sql)?.into_iter().map(|(id,date,reward)|Ok(json!({"event_id":id,"date":date,"reward":serde_json::from_str::<Value>(&reward).map_err(|_|"invalid_game_payload")?}))).collect::<Result<Vec<_>,String>>()?;
    let mut q = db
        .prepare("SELECT DISTINCT code FROM cloud_game_blockers WHERE account_id=?1 ORDER BY code")
        .map_err(sql)?;
    let blockers = q
        .query_map([&scope.account_id], |r| r.get::<_, String>(0))
        .map_err(sql)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(sql)?;
    let value = json!({"owners":owners,"rewards":rewards,"blockers":blockers});
    if value.to_string().len() > 67108864 {
        return Err("game_resource_limit".into());
    }
    Ok(value)
}
/// Rotate *every listed row*, including dependency/decryption failures. Durable
/// scheduling survives restart without introducing another transport cursor.
pub fn received(
    db: &mut Connection,
    scope: &metadata::MetadataScope,
    limit: i64,
) -> Result<Vec<Value>, String> {
    assert_scope(db, scope)?;
    if !(1..=32).contains(&limit) {
        return Err("game_resource_limit".into());
    }
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sql)?;
    let mut q=tx.prepare("WITH incoming AS (
      SELECT i.event_id,i.server_sequence,i.device_id,i.project_id,i.entity_id,i.sync_revision,i.updated_at,o.nonce,o.ciphertext,'project' AS scope
      FROM cloud_sync_inbox i JOIN cloud_sync_event_objects o USING(account_id,event_id)
      WHERE i.account_id=?1 AND i.entity_type='project_game' AND i.operation='event' AND i.state IN ('received','orphan')
      UNION ALL
      SELECT event_id,server_sequence,device_id,NULL,entity_id,sync_revision,updated_at,nonce,ciphertext,'account'
      FROM cloud_sync_account_inbox WHERE account_id=?1 AND entity_type='account_game' AND operation='upsert'
    ) SELECT i.* FROM incoming i LEFT JOIN cloud_game_reader_visits v ON v.account_id=?1 AND v.event_id=i.event_id
      WHERE NOT EXISTS(SELECT 1 FROM cloud_game_apply_ledger l WHERE l.account_id=?1 AND l.event_id=i.event_id)
      ORDER BY COALESCE(v.ordinal,0),i.server_sequence LIMIT ?2").map_err(sql)?;
    let rows=q.query_map(params![scope.account_id,limit],|r|Ok(json!({"event_id":r.get::<_,String>(0)?,"server_sequence":r.get::<_,i64>(1)?,"source_device_id":r.get::<_,String>(2)?,"project_id":r.get::<_,Option<String>>(3)?,"entity_id":r.get::<_,String>(4)?,"revision":r.get::<_,i64>(5)?,"updated_at":r.get::<_,String>(6)?,"nonce":r.get::<_,Vec<u8>>(7)?,"ciphertext":r.get::<_,Vec<u8>>(8)?,"scope":r.get::<_,String>(9)?}))).map_err(sql)?.collect::<rusqlite::Result<Vec<_>>>().map_err(sql)?;
    drop(q);
    let mut ordinal: i64 = tx
        .query_row(
            "SELECT COALESCE(MAX(ordinal),0) FROM cloud_game_reader_visits WHERE account_id=?1",
            [&scope.account_id],
            |r| r.get(0),
        )
        .map_err(sql)?;
    for row in &rows {
        ordinal = ordinal
            .checked_add(1)
            .filter(|n| *n <= 9007199254740991)
            .ok_or("game_resource_limit")?;
        tx.execute("INSERT INTO cloud_game_reader_visits VALUES(?1,?2,?3) ON CONFLICT(account_id,event_id) DO UPDATE SET ordinal=excluded.ordinal",params![scope.account_id,row["event_id"].as_str(),ordinal]).map_err(sql)?;
    }
    tx.commit().map_err(sql)?;
    Ok(rows)
}
/// Retain the encrypted source; failures never manufacture apply/ACK evidence.
pub fn block(
    db: &mut Connection,
    scope: &metadata::MetadataScope,
    id: &str,
    n: &[u8],
    c: &[u8],
    code: &str,
) -> Result<(), String> {
    assert_scope(db, scope)?;
    if !crate::frame_compression::CODES.contains(&code)
        && !matches!(
            code,
            "game_codec_not_activated"
                | "game_resource_limit"
                | "game_scope_mismatch"
                | "invalid_game_payload"
                | "decrypt_failed"
        )
    {
        return Err("invalid_game_payload".into());
    }
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sql)?;
    let key:Option<String>=tx.query_row("SELECT COALESCE((SELECT owner_key FROM cloud_game_events WHERE account_id=?1 AND event_id=?2),json_array(i.project_id,NULL)) FROM cloud_sync_inbox i JOIN cloud_sync_event_objects o USING(account_id,event_id) WHERE i.account_id=?1 AND i.event_id=?2 AND i.entity_type='project_game' AND o.nonce=?3 AND o.ciphertext=?4 UNION ALL SELECT 'account' FROM cloud_sync_account_inbox WHERE account_id=?1 AND event_id=?2 AND entity_type='account_game' AND canonical_user_id=?5 AND nonce=?3 AND ciphertext=?4",params![scope.account_id,id,n,c,scope.canonical_user_id],|r|r.get(0)).optional().map_err(sql)?;
    let key = key.ok_or("game_scope_mismatch")?;
    let proven:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM cloud_game_apply_ledger WHERE account_id=?1 AND event_id=?2)",params![scope.account_id,id],|r|r.get(0)).map_err(sql)?;
    if proven {
        return Err("game_exact_replay_mismatch".into());
    }
    // Schema37 game blocker CHECK is frozen. Preserve the precise transport
    // failure in generic local inbox support state, with the existing generic
    // game blocker in the same transaction. No schema/authority change.
    let game_code = if crate::frame_compression::CODES.contains(&code) {
        tx.execute("UPDATE cloud_sync_inbox SET state='orphan',error_code=?1 WHERE account_id=?2 AND event_id=?3 AND entity_type='project_game'",params![code,scope.account_id,id]).map_err(sql)?;
        tx.execute("UPDATE cloud_sync_account_inbox SET state='blocked',error_code='account_scope_rejected' WHERE account_id=?2 AND event_id=?3 AND entity_type='account_game' AND ?1 IS NOT NULL",params![code,scope.account_id,id]).map_err(sql)?;
        crate::note_sync::record_frame_compression_blocker(&tx, &scope.account_id, id, code)
            .map_err(sql)?;
        "invalid_game_payload"
    } else {
        code
    };
    tx.execute("INSERT INTO cloud_game_blockers VALUES(?1,?2,?3,?4) ON CONFLICT(account_id,owner_key,event_id) DO UPDATE SET code=excluded.code",params![scope.account_id,key,id,game_code]).map_err(sql)?;
    tx.commit().map_err(sql)
}
use rusqlite::OptionalExtension;
fn stored_event(db: &Connection, account: &str, id: &str) -> Result<Value, String> {
    let frame: Vec<u8> = db
        .query_row(
            "SELECT canonical_frame FROM cloud_game_events WHERE account_id=?1 AND event_id=?2",
            params![account, id],
            |r| r.get(0),
        )
        .map_err(sql)?;
    crate::game_codec::unframe(&frame, frame.get(9) == Some(&12))
}
fn current_local(db: &Connection, account: &str, key: &str) -> Result<Value, String> {
    let raw: Option<String> = db
        .query_row(
            "SELECT snapshot_json FROM cloud_game_projection WHERE account_id=?1 AND owner_key=?2",
            params![account, key],
            |r| r.get(0),
        )
        .optional()
        .map_err(sql)?;
    raw.map(|s| serde_json::from_str(&s).map_err(|_| "invalid_game_payload".into()))
        .transpose()
        .map(|s| s.unwrap_or(Value::Null))
}
pub fn decide(
    db: &mut Connection,
    scope: &metadata::MetadataScope,
    key: &str,
    tips: &[String],
    local: &Value,
    selected: &str,
    now: &str,
) -> Result<String, String> {
    assert_scope(db, scope)?;
    if tips.len() > 64 || !tips.iter().any(|id| id == selected) {
        return Err("game_noncommutative_conflict".into());
    }
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sql)?;
    let prior = stored_event(&tx, &scope.account_id, selected)?;
    if game_sync::owner(&prior) != key || prior["header"]["account_id"] != scope.canonical_user_id {
        return Err("game_scope_mismatch".into());
    }
    let id = metadata::new_event_id().map_err(|_| "game_storage_error")?;
    let h = game_migration::header(
        &tx,
        scope,
        prior["header"]["project_id"].as_str(),
        prior["header"]["stage_id"].as_str(),
        &id,
        tips,
        "native-game-v1",
        now,
    )?;
    let e =
        json!({"version":1,"header":h,"action":{"kind":"resolution","selected_event_id":selected}});
    game_sync::queue_resolution(&tx, &scope.account_id, &e, tips, local)?;
    if h["scope"] == "project" {
        game_migration::queue_project(&tx, &scope.account_id, &e)?;
    }
    tx.commit().map_err(sql)?;
    Ok(id)
}
pub fn compensate(
    db: &mut Connection,
    scope: &metadata::MetadataScope,
    target: &str,
    tips: &[String],
    local: &Value,
    now: &str,
) -> Result<String, String> {
    assert_scope(db, scope)?;
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sql)?;
    let id = game_sync::stable_action_id(&scope.canonical_user_id, "compensation", &json!(target));
    // Explicit repeat resumes the immutable original intent, even after echo.
    let old:Option<String>=tx.query_row("SELECT event_id FROM cloud_game_compensations WHERE account_id=?1 AND target_action_id=?2",params![scope.account_id,target],|r|r.get(0)).optional().map_err(sql)?;
    if let Some(old) = old {
        if old != id {
            return Err("game_reward_duplicate_mismatch".into());
        }
        tx.commit().map_err(sql)?;
        return Ok(id);
    }
    if tips.len() != 1
        || game_sync::tips(&tx, &scope.account_id, "account")? != tips
        || current_local(&tx, &scope.account_id, "account")? != *local
    {
        return Err("game_noncommutative_conflict".into());
    }
    let source = stored_event(&tx, &scope.account_id, target)?;
    if source["header"]["scope"] != "account"
        || source["header"]["account_id"] != scope.canonical_user_id
        || source["action"]["kind"] != "reward"
    {
        return Err("game_scope_mismatch".into());
    }
    let h = game_migration::header(&tx, scope, None, None, &id, tips, "native-game-v1", now)?;
    let mut reversal = json!({});
    for field in ["coins", "experience"] {
        let amount = crate::progress_codec::micros(
            source["action"]["reward"][field]
                .as_str()
                .ok_or("invalid_game_payload")?,
        )
        .map_err(|_| "game_invalid_rule")?;
        reversal[field] = json!(crate::progress_codec::decimal(-amount));
    }
    let e = json!({"version":1,"header":h,"action":{"kind":"compensation","target_action_id":target,"reward":reversal}});
    game_sync::queue_compensation(&tx, &scope.account_id, &e)?;
    tx.commit().map_err(sql)?;
    Ok(id)
}
pub fn ordinary(
    db: &mut Connection,
    scope: &metadata::MetadataScope,
    request: Request,
) -> Result<Value, String> {
    assert_scope(db, scope)?;
    match request {
        Request::View => view(db, scope),
        Request::Begin { now } => {
            game_migration::capture(db, scope, &now)?;
            view(db, scope)
        }
        Request::Pending { sealed } => Ok(json!(game_transport::pending(db, scope, sealed)?)),
        Request::Seal {
            event_id,
            frame,
            nonce,
            ciphertext,
        } => {
            game_transport::seal(db, scope, &event_id, &frame, &nonce, &ciphertext)?;
            Ok(Value::Null)
        }
        Request::Receipt {
            event_id,
            server_sequence,
            duplicate,
            now,
        } => {
            game_transport::receipt(db, scope, &event_id, server_sequence, duplicate, &now)?;
            Ok(Value::Null)
        }
        Request::Received { limit } => Ok(json!(received(db, scope, limit)?)),
        Request::Block {
            event_id,
            nonce,
            ciphertext,
            code,
        } => {
            block(db, scope, &event_id, &nonce, &ciphertext, &code)?;
            Ok(Value::Null)
        }
        Request::Decide {
            owner_key,
            expected_tips,
            expected_local,
            selected_event_id,
            now,
        } => Ok(json!(decide(
            db,
            scope,
            &owner_key,
            &expected_tips,
            &expected_local,
            &selected_event_id,
            &now
        )?)),
        Request::Compensate {
            target_action_id,
            expected_tips,
            expected_local,
            now,
        } => Ok(json!(compensate(
            db,
            scope,
            &target_action_id,
            &expected_tips,
            &expected_local,
            &now
        )?)),
        Request::Apply { .. } | Request::Rebuild { .. } => Err("game_scope_mismatch".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const USER: &str = "123e4567-e89b-42d3-a456-426614174000";
    const DEVICE: &str = "123e4567-e89b-42d3-a456-426614174001";
    fn scope() -> metadata::MetadataScope {
        metadata::MetadataScope {
            account_id: "test".into(),
            canonical_user_id: USER.into(),
            device_id: DEVICE.into(),
        }
    }
    #[test]
    fn game_reader_rotation_survives_restart_and_does_not_manufacture_ack() {
        let path = std::env::temp_dir().join(format!(
            "game-reader-{}.db",
            metadata::new_event_id().unwrap()
        ));
        let mut db = crate::sqlite::open_database(&path).unwrap();
        db.execute(
            "UPDATE storage_ownership SET owner='sqlite' WHERE subsystem='game'",
            [],
        )
        .unwrap();
        db.execute("INSERT INTO cloud_sync_state(account_id,device_id,created_at,updated_at) VALUES('test',?1,'now','now')",[DEVICE]).unwrap();
        db.execute(
            "INSERT INTO cloud_account_bindings VALUES('test',?1,'now','now')",
            [USER],
        )
        .unwrap();
        let mut ids = Vec::new();
        for sequence in 1..=35 {
            let id = metadata::new_event_id().unwrap();
            ids.push(id.clone());
            if sequence % 2 == 0 {
                db.execute("INSERT INTO cloud_sync_inbox(account_id,event_id,server_sequence,device_id,project_id,entity_id,entity_type,operation,sync_revision,updated_at,state,received_at) VALUES('test',?1,?2,?3,'p',?4,'project_game','event',1,'now','received','now')",params![id,sequence,DEVICE,format!("game:project:{id}")]).unwrap();
                db.execute(
                    "INSERT INTO cloud_sync_event_objects VALUES('test',?1,1,1,?2,?3,'now')",
                    params![id, vec![4u8; 24], vec![5u8; 32]],
                )
                .unwrap();
            } else {
                db.execute("INSERT INTO cloud_sync_account_inbox(account_id,event_id,canonical_user_id,scope,server_sequence,device_id,entity_id,entity_type,operation,sync_revision,updated_at,crypto_version,aad_version,nonce,ciphertext,received_at) VALUES('test',?1,?2,'account',?3,?4,?5,'account_game','upsert',1,'now',2,2,?6,?7,'now')",params![id,USER,sequence,DEVICE,format!("game:{id}"),vec![4u8;24],vec![5u8;32]]).unwrap();
            }
        }
        let first = received(&mut db, &scope(), 32).unwrap();
        assert_eq!(first.len(), 32);
        for row in &first {
            block(
                &mut db,
                &scope(),
                row["event_id"].as_str().unwrap(),
                &[4u8; 24],
                &[5u8; 32],
                "decrypt_failed",
            )
            .unwrap();
        }
        drop(db);
        let mut db = crate::sqlite::open_database(&path).unwrap();
        let next = received(&mut db, &scope(), 8).unwrap();
        for (row, id) in next[..3].iter().zip(&ids[32..]) {
            assert_eq!(row["event_id"], *id);
        }
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM cloud_game_apply_ledger", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM cloud_game_blockers", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            32
        );
        assert!(block(
            &mut db,
            &scope(),
            &ids[0],
            &[6u8; 24],
            &[5u8; 32],
            "decrypt_failed"
        )
        .is_err());
        assert!(received(&mut db, &scope(), 33).is_err());
        assert!(serde_json::from_value::<Request>(
            json!({"action":"begin","now":"now","payload":{"coins":999}})
        )
        .is_err());
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM cloud_game_candidates", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn compressed_frame_blocker_is_durable_and_rotates_without_ack() {
        let path = std::env::temp_dir().join(format!(
            "game-reader-{}.db",
            metadata::new_event_id().unwrap()
        ));
        let mut db = crate::sqlite::open_database(&path).unwrap();
        db.execute(
            "UPDATE storage_ownership SET owner='sqlite' WHERE subsystem='game'",
            [],
        )
        .unwrap();
        db.execute("INSERT INTO cloud_sync_state(account_id,device_id,created_at,updated_at) VALUES('test',?1,'now','now')",[DEVICE]).unwrap();
        db.execute(
            "INSERT INTO cloud_account_bindings VALUES('test',?1,'now','now')",
            [USER],
        )
        .unwrap();
        let mut ids = Vec::new();
        for sequence in 1..=35 {
            let id = metadata::new_event_id().unwrap();
            ids.push(id.clone());
            if sequence % 2 == 0 {
                db.execute("INSERT INTO cloud_sync_inbox(account_id,event_id,server_sequence,device_id,project_id,entity_id,entity_type,operation,sync_revision,updated_at,state,received_at) VALUES('test',?1,?2,?3,'p',?4,'project_game','event',1,'now','received','now')",params![id,sequence,DEVICE,format!("game:project:{id}")]).unwrap();
                db.execute(
                    "INSERT INTO cloud_sync_event_objects VALUES('test',?1,1,1,?2,?3,'now')",
                    params![id, vec![4u8; 24], vec![5u8; 32]],
                )
                .unwrap();
            } else {
                db.execute("INSERT INTO cloud_sync_account_inbox(account_id,event_id,canonical_user_id,scope,server_sequence,device_id,entity_id,entity_type,operation,sync_revision,updated_at,crypto_version,aad_version,nonce,ciphertext,received_at) VALUES('test',?1,?2,'account',?3,?4,?5,'account_game','upsert',1,'now',2,2,?6,?7,'now')",params![id,USER,sequence,DEVICE,format!("game:{id}"),vec![4u8;24],vec![5u8;32]]).unwrap();
            }
        }
        let first = received(&mut db, &scope(), 32).unwrap();
        assert_eq!(first.len(), 32);
        for row in &first {
            block(
                &mut db,
                &scope(),
                row["event_id"].as_str().unwrap(),
                &[4u8; 24],
                &[5u8; 32],
                "compression_invalid_stream",
            )
            .unwrap();
        }
        drop(db);
        let mut db = crate::sqlite::open_database(&path).unwrap();
        assert_eq!(db.query_row("SELECT COUNT(*) FROM application_metadata WHERE key LIKE 'frame_compression_blocker:%' AND value=json_object('code','compression_invalid_stream')",[],|r|r.get::<_,i64>(0)).unwrap(),32);
        let next = received(&mut db, &scope(), 8).unwrap();
        for (row, id) in next[..3].iter().zip(&ids[32..]) {
            assert_eq!(row["event_id"], *id);
        }
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM cloud_game_apply_ledger", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM cloud_game_blockers", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            32
        );
        assert!(block(
            &mut db,
            &scope(),
            &ids[0],
            &[6u8; 24],
            &[5u8; 32],
            "compression_invalid_stream"
        )
        .is_err());
        assert!(received(&mut db, &scope(), 33).is_err());
        assert!(serde_json::from_value::<Request>(
            json!({"action":"begin","now":"now","payload":{"coins":999}})
        )
        .is_err());
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM cloud_game_candidates", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        db.execute("UPDATE cloud_sync_state SET pull_cursor=35", [])
            .unwrap();
        let ack = crate::note_sync::prepare_note_sync_ack(
            &mut db,
            &crate::note_sync::PrepareNoteSyncAckCommand {
                account_id: "test".into(),
                canonical_user_id: USER.into(),
                device_id: DEVICE.into(),
            },
        )
        .unwrap();
        assert_eq!(ack.current_ack_cursor, 0);
        assert_eq!(ack.candidate_cursor, 0);
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
}
