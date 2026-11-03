//! Account transport preservation; semantic apply and ACK proof live in account_catalog.
use crate::note_sync::{
    decode_canonical_base64url, validate_pull_scope, CommitMixedSyncInboundPageCommand,
    EncryptedNoteSyncEnvelope, NoteSyncError,
};
use rusqlite::{Connection, Transaction};
use serde::{Deserialize, Serialize};

pub(crate) const TYPES: [&str; 5] = [
    "folder",
    "folder_order",
    "folder_membership",
    "project_order",
    "account_game",
];
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InboundAccountItem {
    pub event_id: String,
    pub server_sequence: i64,
    pub source_device_id: String,
    pub canonical_user_id: String,
    pub scope: String,
    pub entity_id: String,
    pub entity_type: String,
    pub operation: String,
    pub revision: i64,
    pub updated_at: String,
    pub deleted_at: Option<String>,
    pub envelope: EncryptedNoteSyncEnvelope,
}
fn valid_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(i, b)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
}
fn invalid() -> NoteSyncError {
    NoteSyncError::InvalidEnvelope("invalid account descriptor or replay")
}
pub(crate) fn preserve(
    tx: &Transaction<'_>,
    command: &CommitMixedSyncInboundPageCommand,
    item: &InboundAccountItem,
    replay_only: bool,
) -> Result<(bool, usize), NoteSyncError> {
    if item.scope != "account"
        || !valid_uuid(&item.canonical_user_id)
        || item.canonical_user_id != command.canonical_user_id
        || !TYPES.contains(&item.entity_type.as_str())
        || item.entity_id.is_empty()
        || item.entity_id.len() > 512
        || !matches!(item.operation.as_str(), "upsert" | "delete")
        || item.entity_type == "account_game"
            && (item.operation != "upsert"
                || item.deleted_at.is_some()
                || item.entity_id != format!("game:{}", item.event_id))
        || !(1..=9_007_199_254_740_991).contains(&item.revision)
        || (item.operation == "delete") != item.deleted_at.is_some()
        || !valid_uuid(&item.event_id)
        || !valid_uuid(&item.source_device_id)
        || item.envelope.crypto_version != 2
        || item.envelope.aad_version != 2
    {
        return Err(invalid());
    }
    let nonce = decode_canonical_base64url(&item.envelope.nonce, 24, 24)?;
    let ciphertext = decode_canonical_base64url(&item.envelope.ciphertext, 16, 8_388_624)?;
    let existing: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM cloud_sync_account_inbox WHERE account_id=?1 AND event_id=?2)",
        rusqlite::params![command.account_id, item.event_id],
        |row| row.get(0),
    )?;
    if existing {
        let same: bool = tx.query_row("SELECT server_sequence=?3 AND device_id=?4 AND canonical_user_id=?5 AND scope=?6 AND entity_id=?7 AND entity_type=?8 AND operation=?9 AND sync_revision=?10 AND updated_at=?11 AND deleted_at IS ?12 AND crypto_version=2 AND aad_version=2 AND nonce=?13 AND ciphertext=?14 FROM cloud_sync_account_inbox WHERE account_id=?1 AND event_id=?2",
            rusqlite::params![command.account_id,item.event_id,item.server_sequence,item.source_device_id,item.canonical_user_id,item.scope,item.entity_id,item.entity_type,item.operation,item.revision,item.updated_at,item.deleted_at,nonce,ciphertext], |row|row.get(0))?;
        if !same {
            return Err(invalid());
        }
    } else {
        if replay_only {
            return Err(invalid());
        }
        tx.execute("INSERT INTO cloud_sync_account_inbox(account_id,event_id,server_sequence,device_id,canonical_user_id,scope,entity_id,entity_type,operation,sync_revision,updated_at,deleted_at,crypto_version,aad_version,nonce,ciphertext,received_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,2,2,?13,?14,strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
            rusqlite::params![command.account_id,item.event_id,item.server_sequence,item.source_device_id,item.canonical_user_id,item.scope,item.entity_id,item.entity_type,item.operation,item.revision,item.updated_at,item.deleted_at,nonce,ciphertext])?;
    }
    Ok((existing, ciphertext.len()))
}
#[derive(Serialize)]
pub(crate) struct ReceivedAccountItem {
    event_id: String,
    server_sequence: i64,
    canonical_user_id: String,
    scope: String,
    entity_id: String,
    entity_type: String,
    crypto_version: i64,
    aad_version: i64,
    nonce: Vec<u8>,
    ciphertext: Vec<u8>,
    source_device_id: String,
    operation: String,
    revision: i64,
    updated_at: String,
    deleted_at: Option<String>,
}
pub(crate) fn received(
    connection: &Connection,
    scope: &crate::project_metadata_sync::MetadataScope,
    limit: u32,
    after: i64,
) -> Result<Vec<ReceivedAccountItem>, NoteSyncError> {
    if !(1..=200).contains(&limit) || !(0..=9_007_199_254_740_991).contains(&after) {
        return Err(invalid());
    }
    let tx = connection.unchecked_transaction()?;
    validate_pull_scope(
        &tx,
        &scope.account_id,
        &scope.device_id,
        &scope.canonical_user_id,
    )?;
    let mut query = tx.prepare("SELECT event_id,server_sequence,canonical_user_id,scope,entity_id,entity_type,crypto_version,aad_version,nonce,ciphertext,device_id,operation,sync_revision,updated_at,deleted_at FROM cloud_sync_account_inbox WHERE account_id=?1 AND entity_type IN ('folder','folder_order','folder_membership','project_order') AND server_sequence>?2 AND NOT EXISTS(SELECT 1 FROM cloud_catalog_apply_ledger l WHERE l.account_id=cloud_sync_account_inbox.account_id AND l.event_id=cloud_sync_account_inbox.event_id) ORDER BY server_sequence LIMIT ?3")?;
    let rows = query
        .query_map(rusqlite::params![scope.account_id, after, limit], |r| {
            Ok(ReceivedAccountItem {
                event_id: r.get(0)?,
                server_sequence: r.get(1)?,
                canonical_user_id: r.get(2)?,
                scope: r.get(3)?,
                entity_id: r.get(4)?,
                entity_type: r.get(5)?,
                crypto_version: r.get(6)?,
                aad_version: r.get(7)?,
                nonce: r.get(8)?,
                ciphertext: r.get(9)?,
                source_device_id: r.get(10)?,
                operation: r.get(11)?,
                revision: r.get(12)?,
                updated_at: r.get(13)?,
                deleted_at: r.get(14)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(query);
    tx.commit()?;
    Ok(rows)
}
pub(crate) fn block(
    connection: &Connection,
    scope: &crate::project_metadata_sync::MetadataScope,
    event_id: &str,
    nonce: &[u8],
    ciphertext: &[u8],
    code: &str,
) -> Result<(), NoteSyncError> {
    if !crate::frame_compression::CODES.contains(&code)
        && !matches!(
            code,
            "account_entity_codec_not_activated"
                | "decrypt_failed"
                | "account_scope_rejected"
                | "invalid_catalog_frame"
                | "catalog_dependency_missing"
                | "catalog_parent_unknown"
                | "catalog_membership_changed"
                | "catalog_project_unproven"
                | "catalog_folder_has_members"
                | "catalog_resource_limit"
                | "catalog_dependency_conflict"
                | "unsupported_catalog_source"
        )
    {
        return Err(invalid());
    }
    let tx = connection.unchecked_transaction()?;
    let code = if crate::frame_compression::CODES.contains(&code) {
        crate::note_sync::record_frame_compression_blocker(&tx, &scope.account_id, event_id, code)?;
        "invalid_catalog_frame"
    } else {
        code
    };
    validate_pull_scope(
        &tx,
        &scope.account_id,
        &scope.device_id,
        &scope.canonical_user_id,
    )?;
    let changed=tx.execute("UPDATE cloud_sync_account_inbox SET state='blocked',error_code=?1 WHERE account_id=?2 AND event_id=?3 AND canonical_user_id=?4 AND nonce=?5 AND ciphertext=?6",
        rusqlite::params![if matches!(code,"decrypt_failed"|"account_scope_rejected"|"account_entity_codec_not_activated"){code}else{"account_scope_rejected"},scope.account_id,event_id,scope.canonical_user_id,nonce,ciphertext])?;
    if changed != 1 {
        return Err(invalid());
    }
    tx.execute("INSERT INTO cloud_catalog_inbox_blockers VALUES(?1,?2,?3) ON CONFLICT(account_id,event_id) DO UPDATE SET code=excluded.code",rusqlite::params![scope.account_id,event_id,code])?;
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::note_sync::{
        self, CommitMixedSyncInboundPageCommand, InboundSyncItem, PrepareNoteSyncAckCommand,
    };
    use sha2::{Digest, Sha256};
    const USER: &str = "123e4567-e89b-42d3-a456-426614174000";
    const DEVICE: &str = "123e4567-e89b-42d3-a456-426614174001";
    fn scope() -> crate::project_metadata_sync::MetadataScope {
        crate::project_metadata_sync::MetadataScope {
            account_id: "test-account".into(),
            canonical_user_id: USER.into(),
            device_id: DEVICE.into(),
        }
    }
    fn fixture() -> serde_json::Value {
        serde_json::from_str(include_str!(
            "../../src/crypto/account-object-v2.vectors.json"
        ))
        .unwrap()
    }
    fn bytes(v: &str) -> Vec<u8> {
        (0..v.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&v[i..i + 2], 16).unwrap())
            .collect()
    }
    // Independent verifier only: production native boundary preserves opaque bytes;
    // production AEAD/HKDF execute in WebCrypto/libsodium TypeScript, not Rust.
    fn hmac(key: &[u8], msg: &[u8]) -> Vec<u8> {
        let mut block = [0u8; 64];
        block[..key.len()].copy_from_slice(key);
        let inner: Vec<_> = block.iter().map(|b| b ^ 0x36).collect();
        let outer: Vec<_> = block.iter().map(|b| b ^ 0x5c).collect();
        let mut hash = Sha256::new();
        hash.update(inner);
        hash.update(msg);
        let digest = hash.finalize();
        let mut hash = Sha256::new();
        hash.update(outer);
        hash.update(digest);
        hash.finalize().to_vec()
    }
    #[test]
    fn account_sync_independent_tuple_hkdf_aad_vector() {
        let v = fixture();
        let c = &v["context"];
        let mut tuple = Vec::new();
        for name in ["userId", "scope", "entityId", "entityType"] {
            let value = c[name].as_str().unwrap().as_bytes();
            tuple.extend_from_slice(&(value.len() as u32).to_be_bytes());
            tuple.extend_from_slice(value);
        }
        assert_eq!(tuple, bytes(v["tuple"].as_str().unwrap()));
        let mut aad = b"worta/account-object-aad/v1".to_vec();
        aad.extend([2, 2]);
        aad.extend(&tuple);
        assert_eq!(aad, bytes(v["aad"].as_str().unwrap()));
        let mut info = b"worta/account-object-key/v1".to_vec();
        info.extend([2, 2]);
        info.extend(tuple);
        assert_eq!(info, bytes(v["info"].as_str().unwrap()));
        let prk = hmac(
            b"worta/hkdf/account-object-key/salt/v1",
            &bytes(v["amk"].as_str().unwrap()),
        );
        info.push(1);
        assert_eq!(hmac(&prk, &info), bytes(v["key"].as_str().unwrap()));
    }
    fn command() -> CommitMixedSyncInboundPageCommand {
        serde_json::from_value(serde_json::json!({"account_id":"test-account","canonical_user_id":USER,"device_id":DEVICE,
          "expected_cursor":0,"next_cursor":1,"has_more":false,"items":[{"event_id":"123e4567-e89b-42d3-a456-426614174099","server_sequence":1,"source_device_id":DEVICE,"canonical_user_id":USER,"scope":"account","entity_id":"каталог/📁","entity_type":"folder","operation":"upsert","revision":1,"updated_at":"2026-10-02T00:00:00Z","deleted_at":null,"envelope":{"crypto_version":2,"aad_version":2,"nonce":crate::note_sync::encode_canonical_base64url(&bytes(fixture()["nonce"].as_str().unwrap())),"ciphertext":crate::note_sync::encode_canonical_base64url(&bytes(fixture()["ciphertext"].as_str().unwrap()))}}]})).unwrap()
    }
    fn setup(c: &Connection) {
        crate::sqlite::apply_migrations(c).unwrap();
        c.execute(
            "INSERT INTO cloud_sync_state VALUES('test-account',?1,0,0,'now','now')",
            [DEVICE],
        )
        .unwrap();
        c.execute(
            "INSERT INTO cloud_account_bindings VALUES('test-account',?1,'now','now')",
            [USER],
        )
        .unwrap();
    }
    #[test]
    fn account_sync_restart_replay_blocker_and_shared_ack() {
        let path = std::env::temp_dir().join(format!(
            "worta-account-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut c = Connection::open(&path).unwrap();
        setup(&c);
        let command = command();
        assert_eq!(
            note_sync::commit_mixed_sync_inbound_page(&mut c, &command)
                .unwrap()
                .new_events,
            1
        );
        assert_eq!(
            note_sync::commit_mixed_sync_inbound_page(&mut c, &command)
                .unwrap()
                .replayed_events,
            1
        );
        let row = received(&c, &scope(), 8, 0).unwrap().remove(0);
        block(
            &c,
            &scope(),
            &row.event_id,
            &row.nonce,
            &row.ciphertext,
            "catalog_resource_limit",
        )
        .unwrap();
        drop(c);
        let mut c = Connection::open(&path).unwrap();
        crate::sqlite::apply_migrations(&c).unwrap();
        let rows = received(&c, &scope(), 8, 0).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].nonce, bytes(fixture()["nonce"].as_str().unwrap()));
        assert_eq!(
            rows[0].ciphertext,
            bytes(fixture()["ciphertext"].as_str().unwrap())
        );
        assert_eq!(
            c.query_row("SELECT error_code FROM cloud_sync_account_inbox", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "account_scope_rejected"
        );
        assert_eq!(
            c.query_row("SELECT code FROM cloud_catalog_inbox_blockers", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap(),
            "catalog_resource_limit"
        );
        assert_eq!(
            note_sync::prepare_note_sync_ack(
                &mut c,
                &PrepareNoteSyncAckCommand {
                    account_id: "test-account".into(),
                    canonical_user_id: USER.into(),
                    device_id: DEVICE.into()
                }
            )
            .unwrap()
            .candidate_cursor,
            0
        );
        for change in ["entity_id", "ciphertext", "canonical_user_id"] {
            let mut changed = command.clone();
            let InboundSyncItem::Account(item) = &mut changed.items[0] else {
                panic!()
            };
            match change {
                "entity_id" => item.entity_id = "changed".into(),
                "ciphertext" => item.envelope.ciphertext = "AQAAAAAAAAAAAAAAAAAAAA".into(),
                _ => item.canonical_user_id = DEVICE.into(),
            }
            assert!(note_sync::commit_mixed_sync_inbound_page(&mut c, &changed).is_err());
        }
        assert_eq!(
            c.query_row("SELECT COUNT(*) FROM projects", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            c.query_row("SELECT COUNT(*) FROM cloud_sync_inbox", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert!(c
            .execute(
                "UPDATE cloud_sync_account_inbox SET ciphertext=zeroblob(16)",
                []
            )
            .is_err());
        assert!(c.execute("INSERT INTO cloud_sync_inbox(account_id,event_id,server_sequence,device_id,project_id,entity_id,entity_type,operation,sync_revision,updated_at,state,received_at) VALUES('test-account','123e4567-e89b-42d3-a456-426614174098',1,?1,'project','note','note','upsert',1,'now','applied','now')",[DEVICE]).is_err());
        drop(c);
        std::fs::remove_file(&path).unwrap();
    }
    #[test]
    fn account_sync_wrong_project_frame_and_atomic_page_rollback() {
        let mut c = Connection::open_in_memory().unwrap();
        setup(&c);
        let mut command = command();
        command.next_cursor = 2;
        let mut second = command.items[0].clone();
        let InboundSyncItem::Account(item) = &mut second else {
            panic!()
        };
        item.server_sequence = 2;
        item.event_id = "123e4567-e89b-42d3-a456-426614174098".into();
        item.envelope.crypto_version = 1;
        command.items.push(second);
        assert!(note_sync::commit_mixed_sync_inbound_page(&mut c, &command).is_err());
        assert_eq!(
            c.query_row("SELECT COUNT(*) FROM cloud_sync_account_inbox", [], |r| r
                .get::<_, i64>(
                0
            ))
            .unwrap(),
            0
        );
        let item = serde_json::json!({"event_id":"123e4567-e89b-42d3-a456-426614174099","server_sequence":1,"source_device_id":DEVICE,"project_id":"fake","canonical_user_id":USER,"scope":"account","entity_id":"folder","entity_type":"folder","operation":"upsert","revision":1,"updated_at":"2026-10-02T00:00:00Z","deleted_at":null,"envelope":{"crypto_version":2,"aad_version":2,"nonce":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA","ciphertext":"AAAAAAAAAAAAAAAAAAAAAA"}});
        assert!(serde_json::from_value::<InboundSyncItem>(item).is_err());
    }
    #[test]
    fn account_sync_blocks_ack_past_later_applied_project_events() {
        let mut c = Connection::open_in_memory().unwrap();
        setup(&c);
        let project = |sequence: i64| note_sync::InboundNoteSyncItem {
            event_id: format!("123e4567-e89b-42d3-a456-426614174{sequence:03}"),
            server_sequence: sequence,
            source_device_id: DEVICE.into(),
            project_id: "connected-project".into(),
            entity_id: "note".into(),
            entity_type: "note".into(),
            operation: "upsert".into(),
            revision: 1,
            updated_at: "2026-10-02T00:00:00Z".into(),
            deleted_at: None,
            envelope: Some(EncryptedNoteSyncEnvelope {
                crypto_version: 1,
                aad_version: 1,
                nonce: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into(),
                ciphertext: "AAAAAAAAAAAAAAAAAAAAAA".into(),
            }),
        };
        let mut page = command();
        let InboundSyncItem::Account(account) = &mut page.items[0] else {
            panic!()
        };
        account.server_sequence = 2;
        page.items.insert(0, InboundSyncItem::Project(project(1)));
        page.items.push(InboundSyncItem::Project(project(3)));
        page.next_cursor = 3;
        note_sync::commit_mixed_sync_inbound_page(&mut c, &page).unwrap();
        assert_eq!(
            note_sync::commit_mixed_sync_inbound_page(&mut c, &page)
                .unwrap()
                .replayed_events,
            3
        );
        c.execute("UPDATE cloud_sync_inbox SET state='applied'", [])
            .unwrap();
        let candidate = note_sync::prepare_note_sync_ack(
            &mut c,
            &PrepareNoteSyncAckCommand {
                account_id: "test-account".into(),
                canonical_user_id: USER.into(),
                device_id: DEVICE.into(),
            },
        )
        .unwrap();
        assert_eq!(candidate.candidate_cursor, 1);
        assert!(note_sync::commit_note_sync_ack(
            &mut c,
            &note_sync::CommitNoteSyncAckCommand {
                account_id: "test-account".into(),
                canonical_user_id: USER.into(),
                device_id: DEVICE.into(),
                expected_old_ack_cursor: 0,
                acknowledged_cursor: 3
            }
        )
        .is_err());
        assert_eq!(received(&c, &scope(), 1, 0).unwrap()[0].server_sequence, 2);
    }

    #[test]
    fn account_compression_blocker_preserves_exact_local_code_without_schema_change() {
        let path = std::env::temp_dir().join(format!(
            "account-compression-{}.db",
            crate::project_metadata_sync::new_event_id().unwrap()
        ));
        let mut c = Connection::open(&path).unwrap();
        setup(&c);
        note_sync::commit_mixed_sync_inbound_page(&mut c, &command()).unwrap();
        let row = received(&c, &scope(), 8, 0).unwrap().remove(0);
        block(
            &c,
            &scope(),
            &row.event_id,
            &row.nonce,
            &row.ciphertext,
            "compression_invalid_stream",
        )
        .unwrap();
        let key = format!(
            "frame_compression_blocker:{}",
            serde_json::json!([scope().account_id, row.event_id])
        );
        drop(c);
        let c = Connection::open(&path).unwrap();
        crate::sqlite::apply_migrations(&c).unwrap();
        let value: String = c
            .query_row(
                "SELECT value FROM application_metadata WHERE key=?1",
                [&key],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(value, "{\"code\":\"compression_invalid_stream\"}");
        assert!(block(
            &c,
            &scope(),
            &row.event_id,
            &[9; 24],
            &row.ciphertext,
            "compression_length_mismatch"
        )
        .is_err());
        assert_eq!(
            c.query_row(
                "SELECT value FROM application_metadata WHERE key=?1",
                [&key],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            value
        );
        let mut c = c;
        let ack = note_sync::prepare_note_sync_ack(
            &mut c,
            &PrepareNoteSyncAckCommand {
                account_id: scope().account_id,
                canonical_user_id: USER.into(),
                device_id: DEVICE.into(),
            },
        )
        .unwrap();
        assert_eq!(ack.candidate_cursor, 0);
        drop(c);
        std::fs::remove_file(path).unwrap();
    }
}
