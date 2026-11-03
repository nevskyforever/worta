//! Local cover transfer support. Metadata remains the only causal portable authority.
use crate::documents::{base64_encode, decode_base64};
use crate::note_sync::encode_canonical_base64url;
use crate::project_metadata_sync::{self as metadata, MetadataError, MetadataScope};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub(crate) fn validate_reference(r: &Value) -> Result<(), MetadataError> {
    if r.is_null() {
        return Ok(());
    }
    let o = r.as_object().ok_or(MetadataError::Invalid)?;
    let keys = [
        "aad_version",
        "blob_id",
        "crypto_version",
        "envelope_sha256",
        "key_fingerprint",
        "mime_type",
        "plaintext_size",
        "version",
    ];
    if o.len() != keys.len()
        || keys.iter().any(|k| !o.contains_key(*k))
        || r["version"] != 1
        || r["crypto_version"] != 1
        || r["aad_version"] != 1
        || r["mime_type"] != "image/jpeg"
        || !r["blob_id"].as_str().is_some_and(metadata::uuid)
        || !r["plaintext_size"]
            .as_u64()
            .is_some_and(|n| (4..=2097152).contains(&n))
        || ["envelope_sha256", "key_fingerprint"].iter().any(|k| {
            !r[*k].as_str().is_some_and(|s| {
                s.len() == 64
                    && s.bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
        })
    {
        return Err(MetadataError::Invalid);
    }
    Ok(())
}
fn bytes(data: &Value, key: &str) -> Result<Vec<u8>, MetadataError> {
    data[key]
        .as_array()
        .ok_or(MetadataError::Invalid)?
        .iter()
        .map(|v| {
            v.as_u64()
                .filter(|n| *n <= 255)
                .map(|n| n as u8)
                .ok_or(MetadataError::Invalid)
        })
        .collect()
}
fn material(
    tx: &Transaction<'_>,
    account: &str,
    project: &str,
    data: &Value,
    verified: bool,
) -> Result<(), MetadataError> {
    let r = &data["reference"];
    validate_reference(r)?;
    if r.is_null() {
        return Err(MetadataError::Invalid);
    }
    let nonce = bytes(data, "nonce")?;
    let ciphertext = bytes(data, "ciphertext")?;
    let jpeg = bytes(data, "jpeg")?;
    if nonce.len() != 24
        || ciphertext.len() != jpeg.len() + 16
        || jpeg.len() != r["plaintext_size"].as_u64().unwrap() as usize
        || jpeg.get(..2) != Some(&[255, 216])
        || jpeg.get(jpeg.len().saturating_sub(2)..) != Some(&[255, 217])
    {
        return Err(MetadataError::Invalid);
    }
    let envelope=json!({"aad_version":1,"ciphertext":encode_canonical_base64url(&ciphertext),"crypto_version":1,"nonce":encode_canonical_base64url(&nonce)}).to_string();
    let hash = format!("{:x}", Sha256::digest(envelope.as_bytes()));
    if r["envelope_sha256"] != hash {
        return Err(MetadataError::Invalid);
    }
    let id = r["blob_id"].as_str().unwrap();
    let reference = r.to_string();
    if let Some((prior,n,c,j))=tx.query_row("SELECT reference_json,nonce,ciphertext,jpeg FROM cloud_cover_material WHERE account_id=?1 AND project_id=?2 AND blob_id=?3",params![account,project,id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,Vec<u8>>(1)?,r.get::<_,Vec<u8>>(2)?,r.get::<_,Vec<u8>>(3)?))).optional()?{
        if (prior,n,c,j)!=(reference,nonce,ciphertext,jpeg){return Err(MetadataError::Invalid)}
        if verified {tx.execute("UPDATE cloud_cover_material SET remote_verified=1 WHERE account_id=?1 AND project_id=?2 AND blob_id=?3",params![account,project,id])?;}
    }else{tx.execute("INSERT INTO cloud_cover_material VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",params![account,project,id,reference,nonce,ciphertext,jpeg,verified as i64])?;}
    Ok(())
}
pub(crate) fn require_material(
    connection: &Connection,
    account: &str,
    project: &str,
    r: &Value,
) -> Result<Option<String>, MetadataError> {
    validate_reference(r)?;
    if r.is_null() {
        return Ok(None);
    }
    let stored:Option<(String,Vec<u8>)>=connection.query_row("SELECT reference_json,jpeg FROM cloud_cover_material WHERE account_id=?1 AND project_id=?2 AND blob_id=?3 AND remote_verified=1",params![account,project,r["blob_id"].as_str()],|row|Ok((row.get(0)?,row.get(1)?))).optional()?;
    let (reference, jpeg) = stored.ok_or(MetadataError::Invalid)?;
    if reference != r.to_string() || jpeg.len() != r["plaintext_size"].as_u64().unwrap() as usize {
        return Err(MetadataError::Invalid);
    }
    Ok(Some(format!(
        "data:image/jpeg;base64,{}",
        base64_encode(&jpeg)
    )))
}
pub(crate) fn capture_in_tx(
    tx: &Transaction<'_>,
    account: &str,
    project: &str,
    device: &str,
    source: &Value,
    desired: &Value,
    cover: Option<&str>,
    parents: &[String],
    now: &str,
) -> Result<String, MetadataError> {
    if parents.len() != 1 {
        return Err(MetadataError::Conflict);
    }
    let pending:Option<(String,Option<String>,String)>=tx.query_row("SELECT intent_id,source_cover,metadata_json FROM cloud_cover_intents WHERE account_id=?1 AND project_id=?2 AND state!='active'",params![account,project],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
    if let Some((id, old, meta)) = pending {
        if old.as_deref() == cover && meta == desired.to_string() {
            return Ok(id);
        }
        return Err(MetadataError::Conflict);
    }
    let id = metadata::new_event_id()?;
    tx.execute("INSERT INTO cloud_cover_intents(account_id,intent_id,project_id,device_id,source_json,metadata_json,parents_json,source_cover,state,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,'captured',?9)",params![account,id,project,device,source.to_string(),desired.to_string(),serde_json::to_string(parents).map_err(|_|MetadataError::Invalid)?,cover,now])?;
    Ok(id)
}
fn prepare_reference(
    tx: &Transaction<'_>,
    scope: &MetadataScope,
    project: &str,
    id: &str,
    now: &str,
) -> Result<String, MetadataError> {
    let (source,meta,parents,reference,state,existing):(String,String,String,Option<String>,String,Option<String>)=tx.query_row("SELECT source_json,metadata_json,parents_json,reference_json,state,event_id FROM cloud_cover_intents WHERE account_id=?1 AND project_id=?2 AND device_id=?3 AND intent_id=?4",params![scope.account_id,project,scope.device_id,id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)))?;
    if let Some(event) = existing {
        return Ok(event);
    }
    if state != "uploaded" {
        return Err(MetadataError::Invalid);
    }
    let mut desired: Value = serde_json::from_str(&meta).map_err(|_| MetadataError::Invalid)?;
    let reference: Value = reference
        .map(|s| serde_json::from_str(&s))
        .transpose()
        .map_err(|_| MetadataError::Invalid)?
        .unwrap_or(Value::Null);
    require_material(tx, &scope.account_id, project, &reference)?;
    desired["cover_reference"] = reference;
    let ids: Vec<String> = serde_json::from_str(&parents).map_err(|_| MetadataError::Invalid)?;
    if ids.len() != 1 {
        return Err(MetadataError::Conflict);
    }
    let (bootstrap,revision,generation):(String,i64,i64)=tx.query_row("SELECT bootstrap_id,revision+1,generation+1 FROM cloud_sync_metadata_events WHERE account_id=?1 AND project_id=?2 AND event_id=?3 AND state='applied'",params![scope.account_id,project,ids[0]],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    if revision > 9_007_199_254_740_991 || generation > 9_007_199_254_740_991 {
        return Err(MetadataError::Invalid);
    }
    let event = metadata::new_event_id()?;
    let ordinal:i64=tx.query_row("SELECT COALESCE(MAX(local_ordinal),0)+1 FROM cloud_sync_outbox WHERE account_id=?1 AND device_id=?2",params![scope.account_id,scope.device_id],|r|r.get(0))?;
    tx.execute("INSERT INTO cloud_sync_outbox(event_id,account_id,device_id,project_id,entity_id,entity_type,operation,revision,updated_at,created_at,parent_event_id,local_ordinal,lifecycle) VALUES(?1,?2,?3,?4,?4,'project_metadata','upsert',?5,?6,?6,?7,?8,'unsealed')",params![event,scope.account_id,scope.device_id,project,revision,now,ids[0],ordinal])?;
    tx.execute("INSERT INTO cloud_sync_metadata_events(account_id,event_id,project_id,device_id,bootstrap_id,parent_event_ids_json,generation,revision,operation,payload_json,state,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,'update',?9,'unsealed',?10)",params![scope.account_id,event,project,scope.device_id,bootstrap,parents,generation,revision,desired.to_string(),now])?;
    tx.execute(
        "INSERT INTO cloud_sync_metadata_decisions VALUES(?1,?2,?3,'edit',?4,?5,?6,'pending',?7)",
        params![
            scope.account_id,
            project,
            event,
            source,
            desired.to_string(),
            parents,
            now
        ],
    )?;
    // Preserve a later local user edit or remote tip; frozen candidate is still a causal branch.
    let current = metadata::authority_view(tx, &scope.account_id, project)?;
    if current.local.as_ref().map(Value::to_string).as_deref() == Some(meta.as_str())
        || current.local.as_ref().map(Value::to_string).as_deref() == Some(source.as_str())
    {
        metadata::write_visible_metadata(tx, project, &desired, now)?;
    }
    tx.execute("UPDATE cloud_cover_intents SET event_id=?1,state='metadata_pending',blocker=NULL WHERE intent_id=?2",params![event,id])?;
    Ok(event)
}
pub(crate) fn command(
    db: &mut Connection,
    scope: &MetadataScope,
    project: &str,
    action: &str,
    data: &Value,
    now: &str,
) -> Result<Value, MetadataError> {
    metadata::assert_runtime_scope(
        db,
        &scope.account_id,
        &scope.canonical_user_id,
        &scope.device_id,
    )?;
    if !metadata::timestamp(now) || project.is_empty() || project.len() > 512 {
        return Err(MetadataError::Invalid);
    }
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let result = match action {
        "capture" => {
            let view = metadata::authority_view(&tx, &scope.account_id, project)?;
            if view.state != "active" {
                return Err(MetadataError::Conflict);
            }
            let local = view.local.ok_or(MetadataError::Invalid)?;
            let cover: Option<String> = tx.query_row(
                "SELECT json_extract(payload_json,'$.cover_image') FROM projects WHERE id=?1",
                [project],
                |r| r.get(0),
            )?;
            let parents = view
                .branches
                .into_iter()
                .map(|b| b.event_id)
                .collect::<Vec<_>>();
            json!(capture_in_tx(
                &tx,
                &scope.account_id,
                project,
                &scope.device_id,
                &local,
                &local,
                cover.as_deref(),
                &parents,
                now
            )?)
        }
        "projects" => {
            let mut stmt=tx.prepare("SELECT project_id FROM cloud_cover_intents WHERE account_id=?1 AND device_id=?2 AND state IN ('captured','sealed','uploaded','blocked') ORDER BY created_at LIMIT 2")?;
            let ids = stmt
                .query_map(params![scope.account_id, scope.device_id], |r| {
                    r.get::<_, String>(0)
                })?
                .collect::<Result<Vec<_>, _>>()?;
            json!(ids)
        }
        "pending" => {
            let mut stmt=tx.prepare("SELECT intent_id,source_cover,state,reference_json,blob_id,nonce,ciphertext,event_id,blocker FROM cloud_cover_intents WHERE account_id=?1 AND project_id=?2 AND device_id=?3 AND state!='active' ORDER BY created_at LIMIT 1")?;
            let rows=stmt.query_map(params![scope.account_id,project,scope.device_id],|r|Ok(json!({"intent_id":r.get::<_,String>(0)?,"source_cover":r.get::<_,Option<String>>(1)?,"state":r.get::<_,String>(2)?,"reference":r.get::<_,Option<String>>(3)?.map(|s|serde_json::from_str::<Value>(&s).unwrap()),"blob_id":r.get::<_,Option<String>>(4)?,"nonce":r.get::<_,Option<Vec<u8>>>(5)?,"ciphertext":r.get::<_,Option<Vec<u8>>>(6)?,"event_id":r.get::<_,Option<String>>(7)?,"blocker":r.get::<_,Option<String>>(8)?})))?.collect::<Result<Vec<_>,_>>()?;
            rows.into_iter().next().unwrap_or(Value::Null)
        }
        "seal" => {
            if !data["reference"].is_null() {
                material(&tx, &scope.account_id, project, data, false)?;
            }
            let id = data["intent_id"].as_str().ok_or(MetadataError::Invalid)?;
            let r = &data["reference"];
            let source:Option<String>=tx.query_row("SELECT source_cover FROM cloud_cover_intents WHERE account_id=?1 AND project_id=?2 AND intent_id=?3",params![scope.account_id,project,id],|r|r.get(0))?;
            if r.is_null() != source.is_none() {
                return Err(MetadataError::Invalid);
            }
            if let Some(source) = source {
                let raw = source
                    .strip_prefix("data:image/jpeg;base64,")
                    .ok_or(MetadataError::Invalid)?;
                let decoded = decode_base64(raw).map_err(|_| MetadataError::Invalid)?;
                if decoded != bytes(data, "jpeg")? || base64_encode(&decoded) != raw {
                    return Err(MetadataError::Invalid);
                }
            }
            let n = if r.is_null() {
                None
            } else {
                Some(bytes(data, "nonce")?)
            };
            let c = if r.is_null() {
                None
            } else {
                Some(bytes(data, "ciphertext")?)
            };
            let changed=tx.execute("UPDATE cloud_cover_intents SET state=?1,reference_json=?2,blob_id=?3,nonce=?4,ciphertext=?5,blocker=NULL WHERE account_id=?6 AND project_id=?7 AND device_id=?8 AND intent_id=?9 AND state IN ('captured','blocked') AND nonce IS NULL",params![if r.is_null(){"uploaded"}else{"sealed"},if r.is_null(){None}else{Some(r.to_string())},r["blob_id"].as_str(),n,c,scope.account_id,project,scope.device_id,id])?;
            if changed != 1 {
                return Err(MetadataError::Conflict);
            }
            json!(true)
        }
        "uploaded" => {
            let changed=tx.execute("UPDATE cloud_cover_intents SET state='uploaded',blocker=NULL WHERE account_id=?1 AND project_id=?2 AND device_id=?3 AND intent_id=?4 AND state='sealed'",params![scope.account_id,project,scope.device_id,data["intent_id"].as_str()])?;
            if changed != 1 {
                return Err(MetadataError::Conflict);
            }
            json!(true)
        }
        "prepare" => json!(prepare_reference(
            &tx,
            scope,
            project,
            data["intent_id"].as_str().ok_or(MetadataError::Invalid)?,
            now
        )?),
        "material" => {
            material(&tx, &scope.account_id, project, data, true)?;
            json!(true)
        }
        "block" => {
            let code = data["code"].as_str().ok_or(MetadataError::Invalid)?;
            if !["cover_blob_missing", "cover_blob_invalid"].contains(&code) {
                return Err(MetadataError::Invalid);
            }
            if code == "cover_blob_missing" {
                validate_reference(&data["reference"])?;
            }
            if data["reference"].to_string().len() > 131072 {
                return Err(MetadataError::Invalid);
            }
            tx.execute("INSERT INTO cloud_cover_blockers VALUES(?1,?2,?3,?4,?5) ON CONFLICT(account_id,event_id) DO UPDATE SET code=excluded.code",params![scope.account_id,data["event_id"].as_str(),project,data["reference"].to_string(),code])?;
            json!(true)
        }
        "block_intent" => {
            let code = data["code"].as_str().ok_or(MetadataError::Invalid)?;
            if ![
                "cover_source_invalid",
                "cover_readers_not_ready",
                "cover_upload_pending",
            ]
            .contains(&code)
            {
                return Err(MetadataError::Invalid);
            }
            tx.execute("UPDATE cloud_cover_intents SET blocker=?1 WHERE account_id=?2 AND project_id=?3 AND intent_id=?4",params![code,scope.account_id,project,data["intent_id"].as_str()])?;
            json!(true)
        }
        "preview" => json!(require_material(
            &tx,
            &scope.account_id,
            project,
            &data["reference"]
        )?),
        "status" => {
            let mut stmt=tx.prepare("SELECT code FROM cloud_cover_blockers WHERE account_id=?1 AND project_id=?2 ORDER BY event_id")?;
            let blockers = stmt
                .query_map(params![scope.account_id, project], |r| {
                    r.get::<_, String>(0)
                })?
                .collect::<Result<Vec<_>, _>>()?;
            let authority = metadata::authority_view(&tx, &scope.account_id, project)?;
            let has_cover:bool=tx.query_row("SELECT COALESCE(length(json_extract(payload_json,'$.cover_image')),0)>0 FROM projects WHERE id=?1",[project],|r|r.get(0))?;
            json!({"has_local_cover":has_cover,"metadata_state":authority.state,"active":authority.authenticated.as_ref().is_some_and(|m|m.get("cover_reference").is_some()),"blockers":blockers})
        }
        _ => return Err(MetadataError::Invalid),
    };
    tx.commit()?;
    Ok(result)
}
