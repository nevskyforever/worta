//! Device-local comparison evidence. Never part of any portable codec.
use super::*;
use rusqlite::{Connection, TransactionBehavior};

// Word may split an unchanged styled span into multiple XML runs. Normalize
// only equivalent run segmentation for comparison; never rewrite either source.
fn semantic(value: &Value) -> Value {
    let Some(object) = value.as_object() else {
        return value.clone();
    };
    let mut out = object.clone();
    if out
        .get("attrs")
        .is_some_and(|v| v.is_null() || v.as_object().is_some_and(|o| o.is_empty()))
    {
        out.remove("attrs");
    }
    if let Some(Value::Array(marks)) = out.get_mut("marks") {
        marks.sort_by_key(Value::to_string);
        if marks.is_empty() {
            out.remove("marks");
        }
    }
    if let Some(nodes) = object.get("content").and_then(Value::as_array) {
        let mut merged: Vec<Value> = Vec::new();
        for node in nodes {
            let n = semantic(node);
            if n["type"] == "text" {
                if let Some(last) = merged.last_mut().filter(|v| v["type"] == "text") {
                    let mut a = last.clone();
                    let mut b = n.clone();
                    a.as_object_mut().unwrap().remove("text");
                    b.as_object_mut().unwrap().remove("text");
                    if a == b {
                        last["text"] = Value::String(format!(
                            "{}{}",
                            last["text"].as_str().unwrap_or(""),
                            n["text"].as_str().unwrap_or("")
                        ));
                        continue;
                    }
                }
            }
            merged.push(n);
        }
        if merged.is_empty() {
            out.remove("content");
        } else {
            out.insert("content".into(), Value::Array(merged));
        }
    }
    Value::Object(out)
}
fn same_content(a: &Value, b: &Value) -> bool {
    semantic(a) == semantic(b)
}
fn err<T>(code: &str) -> Result<T, String> {
    Err(code.into())
}
fn evidence(db: &Connection, p: &str, id: &str) -> Result<Value, String> {
    let expected = crate::document_sync::expected(db, p, id).map_err(|e| e.to_string())?;
    let connected: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM cloud_document_project_consent WHERE project_id=?1)",
            [p],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if connected {
        let row: Option<(String,String)> = db.query_row("SELECT m.lifecycle,x.head_event_id FROM cloud_document_migrations m JOIN cloud_document_projection x ON x.account_id=m.account_id AND x.project_id=m.project_id AND x.entity_id=m.entity_id WHERE m.project_id=?1 AND m.entity_id=?2",params![p,id],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(|e|e.to_string())?;
        let Some((state, head)) = row else {
            return err("external_authority_pending");
        };
        if state != "active" || expected["tips"] != serde_json::json!([head]) {
            return err("external_authority_conflict");
        }
        let projected:String=db.query_row("SELECT snapshot_json FROM cloud_document_projection WHERE project_id=?1 AND entity_id=?2",params![p,id],|r|r.get(0)).map_err(|e|e.to_string())?;
        if serde_json::from_str::<Value>(&projected).map_err(|_| "external_invalid_evidence")?
            != expected["document"]
        {
            return err("external_authority_pending");
        }
    }
    if expected["document"].is_null() {
        return err("document_missing");
    }
    Ok(expected)
}
fn local_evidence(db: &Connection, id: &str) -> Result<Value, String> {
    let raw: String = db
        .query_row(
            "SELECT payload_json FROM document_bindings WHERE document_id=?1",
            [id],
            |r| r.get(0),
        )
        .map_err(|_| "external_reattach_required")?;
    let payload: Value = serde_json::from_str(&raw).map_err(|_| "external_invalid_evidence")?;
    payload
        .get("reattach_v1")
        .cloned()
        .ok_or_else(|| "external_revalidation_required".into())
}
fn store(db: &Connection, id: &str, e: &Value, state: &str, hash: &str) -> Result<(), String> {
    // Retain unknown pre-C18 binding metadata locally, never reinterpret it.
    let raw: String = db
        .query_row(
            "SELECT payload_json FROM document_bindings WHERE document_id=?1",
            [id],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    let mut payload: Value = serde_json::from_str(&raw).map_err(|_| "external_invalid_evidence")?;
    if !payload.is_object() {
        return err("external_invalid_evidence");
    }
    payload["reattach_v1"] = e.clone();
    db.execute("UPDATE document_bindings SET payload_json=?1,sync_state=?2,last_external_hash=?3,expected_external_hash=?3 WHERE document_id=?4",params![payload.to_string(),state,hash,id]).map_err(|e|e.to_string())?;
    if state == "synced" {
        db.execute("UPDATE document_bindings SET last_synced_hash=?1,last_synced_revision=(SELECT revision FROM documents WHERE id=?2),last_synced_at=?3 WHERE document_id=?2",params![hash,id,now()]).map_err(|e|e.to_string())?;
    }
    Ok(())
}
pub(super) fn compare(
    db: &mut Connection,
    p: &str,
    s: Option<&str>,
    path: &Path,
) -> Result<Value, String> {
    let source = read_stable_source(path).map_err(|_| "external_source_missing")?;
    let parsed = parse_docx(&source.bytes)?.0;
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    let id = document_id_for_scope(&tx, p, s)?;
    let head = evidence(&tx, p, &id)?;
    let state = if same_content(&parsed, &head["document"]["content_json"]) {
        "synced"
    } else {
        "external_proposal"
    };
    let e = serde_json::json!({"head":head,"external_hash":source.hash,"semantic_hash":sha256(parsed.to_string().as_bytes()),"action":"compare"});
    store(&tx, &id, &e, state, &source.hash)?;
    tx.commit().map_err(|e| e.to_string())?;
    let mut row = document_row(db, p, s)?.ok_or_else(|| "document_missing".to_string())?;
    if state == "external_proposal" {
        row["external_content"] = parsed;
    }
    Ok(row)
}
/// Confirmation always rereads the file; proposal contains no manuscript bytes.
pub(super) fn resolve(
    db: &mut Connection,
    p: &str,
    s: Option<&str>,
    choice: &str,
) -> Result<Value, String> {
    let id = document_id_for_scope(db, p, s)?;
    if choice == "unlink" {
        db.execute("DELETE FROM document_bindings WHERE document_id=?1", [&id])
            .map_err(|e| e.to_string())?;
        return document_row(db, p, s)?.ok_or_else(|| "document_missing".into());
    }
    let mut e = local_evidence(db, &id)?;
    let path:String=db.query_row("SELECT external_path FROM document_bindings WHERE document_id=?1 AND binding_type='word'",[&id],|r|r.get(0)).map_err(|_|"external_reattach_required")?;
    let selected = path.clone();
    let path = canonical_existing_file(&path, "Word").map_err(|_| "external_source_missing")?;
    if path.to_string_lossy() != selected {
        return err("external_binding_stale");
    }
    if choice == "compare" {
        return compare(db, p, s, &path);
    }
    let source = read_stable_source(&path).map_err(|_| "external_source_missing")?;
    if e["external_hash"].as_str() != Some(&source.hash) {
        return err("external_hash_stale");
    }
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    if evidence(&tx, p, &id)? != e["head"] {
        return err("external_head_stale");
    }
    match choice {
        "import" => {
            let mut doc = e["head"]["document"].clone();
            doc["content_json"] = parse_docx(&source.bytes)?.0;
            if same_content(&doc["content_json"], &e["head"]["document"]["content_json"]) {
                store(&tx, &id, &e, "synced", &source.hash)?;
                tx.commit().map_err(|e| e.to_string())?;
                return document_row(db, p, s)?.ok_or_else(|| "document_missing".into());
            }
            let handled =
                crate::document_sync::normal(&tx, p, &id, doc.clone(), Some(&e["head"]), &now())
                    .map_err(|e| e.to_string())?;
            if !handled {
                tx.execute("UPDATE documents SET content_json=?1,revision=revision+1,updated_at=?2 WHERE id=?3",params![doc["content_json"].to_string(),now(),id]).map_err(|e|e.to_string())?;
            }
            e["action"] = serde_json::json!("import");
            e["semantic_hash"] =
                serde_json::json!(sha256(doc["content_json"].to_string().as_bytes()));
            store(
                &tx,
                &id,
                &e,
                if handled {
                    "external_import_pending"
                } else {
                    "synced"
                },
                &source.hash,
            )?;
            tx.commit().map_err(|e| e.to_string())?;
        }
        "cloud" => {
            let bytes = build_docx(&e["head"]["document"]["content_json"])?;
            e["action"] = serde_json::json!("write");
            e["output_hash"] = serde_json::json!(sha256(&bytes));
            store(&tx, &id, &e, "external_write_pending", &source.hash)?;
            tx.commit().map_err(|e| e.to_string())?;
            let tx = db
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|e| e.to_string())?;
            if evidence(&tx, p, &id)? != e["head"] {
                return err("external_head_stale");
            }
            if read_stable_source(&path)?.hash != source.hash {
                return err("external_hash_stale");
            }
            atomic_write_guarded(&path, &bytes, Some(&source.hash))?;
            let actual = read_stable_source(&path)?.hash;
            if actual != sha256(&bytes) {
                return err("external_write_unverified");
            }
            e["external_hash"] = serde_json::json!(actual);
            store(&tx, &id, &e, "synced", &actual)?;
            tx.commit().map_err(|e| e.to_string())?;
        }
        _ => return err("external_invalid_choice"),
    }
    document_row(db, p, s)?.ok_or_else(|| "document_missing".into())
}
/// Polling may complete proven crash recovery, but never imports or writes.
pub(super) fn poll(db: &mut Connection, p: &str, s: Option<&str>) -> Result<Value, String> {
    let id = document_id_for_scope(db, p, s)?;
    let mut e = local_evidence(db, &id)?;
    let path: String = db
        .query_row(
            "SELECT external_path FROM document_bindings WHERE document_id=?1",
            [&id],
            |r| r.get(0),
        )
        .map_err(|_| "external_reattach_required")?;
    let source = match read_stable_source(Path::new(&path)) {
        Ok(v) => v,
        Err(_) => {
            db.execute(
                "UPDATE document_bindings SET sync_state='missing_external' WHERE document_id=?1",
                [&id],
            )
            .map_err(|e| e.to_string())?;
            return err("external_source_missing");
        }
    };
    let parsed = parse_docx(&source.bytes)?.0;
    let head = evidence(db, p, &id)?;
    if e["action"] == "write"
        && e["output_hash"].as_str() == Some(&source.hash)
        && head == e["head"]
        && same_content(&parsed, &head["document"]["content_json"])
    {
        e["external_hash"] = serde_json::json!(source.hash);
        store(db, &id, &e, "synced", &source.hash)?;
    } else if e["action"] == "import"
        && e["external_hash"].as_str() == Some(&source.hash)
        && e["semantic_hash"].as_str() == Some(sha256(parsed.to_string().as_bytes()).as_str())
        && same_content(&parsed, &head["document"]["content_json"])
    {
        e["head"] = head;
        e["action"] = serde_json::json!("compare");
        store(db, &id, &e, "synced", &source.hash)?;
    } else if e["external_hash"].as_str() != Some(&source.hash) || e["head"] != head {
        db.execute(
            "UPDATE document_bindings SET sync_state='external_proposal' WHERE document_id=?1",
            [&id],
        )
        .map_err(|e| e.to_string())?;
    }
    document_row(db, p, s)?.ok_or_else(|| "document_missing".into())
}

fn progress_evidence(db: &Connection, p: &str, s: Option<&str>) -> Result<Value, String> {
    let tips = crate::progress_sync::local_heads(db, p, s)?;
    if !tips.as_ref().is_some_and(|t| t.len() == 1) {
        return err("external_progress_authority_pending");
    }
    let owner = crate::progress_codec::scope_id(s);
    let (account,state):(String,String)=db.query_row("SELECT account_id,lifecycle FROM cloud_progress_migrations WHERE project_id=?1 AND entity_id=?2",params![p,owner],|r|Ok((r.get(0)?,r.get(1)?))).map_err(|_|"external_progress_authority_pending")?;
    if state != "active" {
        return err("external_progress_authority_pending");
    }
    let authenticated:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM cloud_progress_events e JOIN cloud_progress_apply_ledger l ON l.account_id=e.account_id AND l.event_id=e.event_id WHERE e.account_id=?1 AND e.project_id=?2 AND e.entity_id=?3 AND e.event_id=?4 AND e.state='applied' AND l.outcome='applied')",params![account,p,owner,tips.as_ref().unwrap()[0]],|r|r.get(0)).map_err(|_|"external_progress_authority_pending")?;
    if !authenticated {
        return err("external_progress_authority_pending");
    }
    let source = crate::progress_sync::source(db, p, s)?;
    let chain = crate::progress_sync::chain(db, &account, p, &owner, &tips.as_ref().unwrap()[0])?;
    if source["chain"] != serde_json::to_value(chain).map_err(|_| "external_invalid_evidence")? {
        return err("external_progress_authority_pending");
    }
    Ok(serde_json::json!({"tips":tips,"source":source}))
}
pub(super) fn connected(db: &Connection, p: &str) -> Result<bool, String> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM cloud_sync_project_bindings WHERE project_id=?1)",
        [p],
        |r| r.get(0),
    )
    .map_err(|e| e.to_string())
}
pub(super) fn progress_proposal(
    db: &mut Connection,
    p: &str,
    s: Option<&str>,
    confirm: bool,
) -> Result<SyncRunResult, String> {
    let id = project_binding_id(p, s);
    let b = binding_for_scope(db, p, s)?.ok_or("external_reattach_required")?;
    if b.0 == "scrivener" {
        let items = parse_scrivener_xml(
            &find_scrivener_xml(Path::new(&b.1)).map_err(|_| "sync_source_missing")?,
        )?;
        if !b.2.as_deref().is_some_and(|id| contains_item(&items, id)) {
            return err("sync_source_stale");
        }
    }
    let (symbols, hash) =
        sync_source(&b.0, Path::new(&b.1), b.2.as_deref()).map_err(|_| "sync_source_missing")?;
    let head = progress_evidence(db, p, s)?;
    let raw: String = db
        .query_row(
            "SELECT payload_json FROM project_bindings WHERE id=?1",
            [&id],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    let mut local: Value = serde_json::from_str(&raw).map_err(|_| "external_invalid_evidence")?;
    let (_, payload) = validate_sync_scope(db, p, s)?;
    let total = normalize_total(
        symbols as f64,
        payload["unit"].as_str().unwrap_or("symbols"),
    )?;
    if !confirm && b.3.as_deref() == Some(&hash) && local.get("progress_baseline_v1") == Some(&head)
    {
        return Ok(SyncRunResult {
            changed: false,
            symbols,
            sync: sync_summary(db, p, s)?,
            progress: None,
        });
    }
    if !confirm && (total - payload["total"].as_f64().unwrap_or(0.0)).abs() < 0.009 {
        local["progress_baseline_v1"] = head.clone();
        local
            .as_object_mut()
            .unwrap()
            .remove("progress_reattach_v1");
        local.as_object_mut().unwrap().remove("progress_pending_v1");
        db.execute("UPDATE project_bindings SET payload_json=?1,content_hash=?2,last_synced_at=?3 WHERE id=?4",params![local.to_string(),hash,now(),id]).map_err(|e|e.to_string())?;
        return Ok(SyncRunResult {
            changed: false,
            symbols,
            sync: sync_summary(db, p, s)?,
            progress: None,
        });
    }
    let proposal = serde_json::json!({"head":head,"hash":hash,"symbols":symbols,"total":total,"binding_type":b.0,"binding_path":b.1,"source_id":b.2});
    if !confirm {
        // Polls never replace a pending proposal with a newer head/hash.
        if local.get("progress_reattach_v1").is_none() {
            local["progress_reattach_v1"] = proposal;
            db.execute(
                "UPDATE project_bindings SET payload_json=?1 WHERE id=?2",
                params![local.to_string(), id],
            )
            .map_err(|e| e.to_string())?;
        }
        return Ok(SyncRunResult {
            changed: false,
            symbols,
            sync: sync_summary(db, p, s)?,
            progress: None,
        });
    }
    let expected = local
        .get("progress_reattach_v1")
        .ok_or("external_progress_proposal_required")?;
    if expected != &proposal {
        return err("external_progress_proposal_stale");
    }
    let (_, entry, _) = record_sync_progress(db, p, s, total, &hash, &now(), Some(&proposal))?;
    local["progress_pending_v1"] = proposal.clone();
    local
        .as_object_mut()
        .unwrap()
        .remove("progress_reattach_v1");
    db.execute(
        "UPDATE project_bindings SET payload_json=?1,content_hash=NULL WHERE id=?2",
        params![local.to_string(), id],
    )
    .map_err(|e| e.to_string())?;
    let changed = !entry.is_null();
    Ok(SyncRunResult {
        changed,
        symbols,
        sync: sync_summary(db, p, s)?,
        progress: if changed {
            Some(progress_result(crate::project_payload(db, p)?, entry))
        } else {
            None
        },
    })
}
pub(super) fn assert_progress_proposal(
    db: &Connection,
    p: &str,
    s: Option<&str>,
    e: &Value,
) -> Result<(), String> {
    if progress_evidence(db, p, s)? != e["head"] {
        return err("external_head_stale");
    }
    let b = binding_for_scope(db, p, s)?.ok_or("external_reattach_required")?;
    if e["binding_type"] != b.0
        || e["binding_path"] != b.1
        || e["source_id"] != serde_json::json!(b.2)
    {
        return err("external_binding_stale");
    }
    if sync_source(&b.0, Path::new(&b.1), b.2.as_deref())?.1
        != e["hash"].as_str().ok_or("external_invalid_evidence")?
    {
        return err("external_hash_stale");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(tag: &str) -> (rusqlite::Connection, PathBuf, PathBuf, Value) {
        let root = std::env::temp_dir().join(format!("c18602-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let path = root.join("state.db");
        let mut db = crate::sqlite::open_database(&path).unwrap();
        db.execute("INSERT INTO projects(id,name,infinite,unit,status,payload_json) VALUES('p','Book',1,'symbols','активен','{\"work_method\":\"app\"}')",[]).unwrap();
        db.execute("INSERT INTO project_order VALUES('p',0)", [])
            .unwrap();
        let original = serde_json::json!({"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"Manuscript"}]}]});
        let content = parse_docx(&build_docx(&original).unwrap()).unwrap().0;
        db.execute("INSERT INTO documents(id,scope_key,project_id,title,content_json,content_format,revision,extensions_json) VALUES('d','p:project','p','Book',?1,'tiptap-json/v1',1,'{}')",[content.to_string()]).unwrap();
        let word = root.join("C18_SECRET_PATH.docx");
        fs::write(&word, build_docx(&content).unwrap()).unwrap();
        let word = word.canonicalize().unwrap();
        db.execute("INSERT INTO document_bindings(id,document_id,binding_type,external_path,payload_json) VALUES('b','d','word',?1,'{\"unknown_local\":\"SOURCE-ID-C18-LOCAL-ONLY\"}')",[word.to_string_lossy().as_ref()]).unwrap();
        compare(&mut db, "p", None, &word).unwrap();
        (db, path, word, content)
    }
    #[test]
    fn external_reattach_equivalent_word_runs_preserve_styles_and_cloud_bytes() {
        let a = serde_json::json!({"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"Manuscript","marks":[{"type":"bold"}]}]}]});
        let b = serde_json::json!({"type":"doc","content":[{"type":"paragraph","attrs":{},"content":[{"type":"text","text":"Manu","marks":[{"type":"bold"}]},{"type":"text","text":"script","marks":[{"type":"bold"}]}]}]});
        let before = a.to_string();
        assert!(same_content(&a, &b));
        assert_eq!(a.to_string(), before);
        let mut plain = b.clone();
        plain["content"][0]["content"][0]["marks"] = serde_json::json!([]);
        assert!(!same_content(&a, &plain));
    }
    #[test]
    fn external_reattach_same_content_does_not_write_or_publish() {
        let (db, path, word, _) = fixture("same");
        let bytes = fs::read(&word).unwrap();
        assert_eq!(
            db.query_row("SELECT sync_state FROM document_bindings", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "synced"
        );
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM cloud_sync_outbox", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(fs::read(word).unwrap(), bytes);
        assert!(local_evidence(&db, "d").unwrap().get("head").is_some());
        drop(db);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }
    #[test]
    fn external_reattach_proposal_restart_hash_and_head_cas() {
        let (mut db, path, word, content) = fixture("stale");
        let different = serde_json::json!({"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"External"}]}]});
        fs::write(&word, build_docx(&different).unwrap()).unwrap();
        compare(&mut db, "p", None, &word).unwrap();
        drop(db);
        let mut db = crate::sqlite::open_database(&path).unwrap();
        let proposal = local_evidence(&db, "d").unwrap();
        fs::write(&word, build_docx(&content).unwrap()).unwrap();
        let bytes = fs::read(&word).unwrap();
        assert_eq!(
            resolve(&mut db, "p", None, "cloud").unwrap_err(),
            "external_hash_stale"
        );
        assert_eq!(
            resolve(&mut db, "p", None, "import").unwrap_err(),
            "external_hash_stale"
        );
        assert_eq!(fs::read(&word).unwrap(), bytes);
        assert_eq!(local_evidence(&db, "d").unwrap(), proposal);
        compare(&mut db, "p", None, &word).unwrap();
        db.execute(
            "UPDATE documents SET title='New cloud tip equivalent local CAS'",
            [],
        )
        .unwrap();
        assert_eq!(
            resolve(&mut db, "p", None, "cloud").unwrap_err(),
            "external_head_stale"
        );
        drop(db);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }
    #[test]
    fn external_reattach_explicit_import_and_unlink_preserve_file() {
        let (mut db, path, word, _) = fixture("import");
        let other = serde_json::json!({"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"External imported"}]}]});
        fs::write(&word, build_docx(&other).unwrap()).unwrap();
        let bytes = fs::read(&word).unwrap();
        compare(&mut db, "p", None, &word).unwrap();
        assert_eq!(
            resolve(&mut db, "p", None, "import").unwrap()["content"],
            parse_docx(&bytes).unwrap().0
        );
        assert_eq!(fs::read(&word).unwrap(), bytes);
        resolve(&mut db, "p", None, "unlink").unwrap();
        assert_eq!(fs::read(&word).unwrap(), bytes);
        drop(db);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }
    #[test]
    fn external_reattach_completed_write_crash_recovery_and_missing() {
        let (mut db, path, word, content) = fixture("crash");
        let bytes = build_docx(&content).unwrap();
        let mut e = local_evidence(&db, "d").unwrap();
        e["action"] = serde_json::json!("write");
        e["output_hash"] = serde_json::json!(sha256(&bytes));
        store(
            &db,
            "d",
            &e,
            "external_write_pending",
            e["external_hash"].as_str().unwrap(),
        )
        .unwrap();
        atomic_write(&word, &bytes).unwrap();
        drop(db);
        let mut db = crate::sqlite::open_database(&path).unwrap();
        assert_eq!(poll(&mut db, "p", None).unwrap()["sync_state"], "synced");
        fs::rename(&word, word.with_extension("moved.docx")).unwrap();
        assert_eq!(
            poll(&mut db, "p", None).unwrap_err(),
            "external_source_missing"
        );
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM documents", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        drop(db);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }
}

pub(super) fn import_pending(
    db: &Connection,
    id: &str,
    head: &Value,
    content: &Value,
    hash: &str,
) -> Result<(), String> {
    let e = serde_json::json!({"head":head,"external_hash":hash,"semantic_hash":sha256(content.to_string().as_bytes()),"action":"import"});
    store(db, id, &e, "external_import_pending", hash)
}

pub(super) fn project_binding_id(p: &str, s: Option<&str>) -> String {
    format!("external-progress:{}", scope_key(p, s))
}
pub(super) fn configure_project_source(
    db: &mut Connection,
    c: SyncConfigureCommand,
) -> Result<SyncSummary, String> {
    let (path, source) = match c.sync_type.as_str() {
        "word" => {
            let path = canonical_existing_file(&c.path, "Word")?;
            if path
                .extension()
                .and_then(|e| e.to_str())
                .is_none_or(|e| !e.eq_ignore_ascii_case("docx"))
            {
                return err("sync_source_invalid");
            };
            parse_docx(&read_stable_source(&path)?.bytes)?;
            (path, None)
        }
        "scrivener" => {
            let path = canonical_existing_directory(&c.path)?;
            let items = parse_scrivener_xml(&find_scrivener_xml(&path)?)?;
            let id = c.item_id.as_deref().ok_or("sync_source_stale")?;
            if !contains_item(&items, id) {
                return err("sync_source_stale");
            };
            sync_source("scrivener", &path, Some(id))?;
            (path, Some(id.to_string()))
        }
        _ => return err("sync_source_invalid"),
    };
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    let id = project_binding_id(&c.project_id, c.stage_id.as_deref());
    // Keep unknown local payload keys; a new explicit path invalidates only our evidence.
    let raw: Option<String> = tx
        .query_row(
            "SELECT payload_json FROM project_bindings WHERE id=?1",
            [&id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let mut local: Value = raw
        .map(|x| serde_json::from_str(&x))
        .transpose()
        .map_err(|_| "external_invalid_evidence")?
        .unwrap_or(serde_json::json!({}));
    if !local.is_object() {
        return err("external_invalid_evidence");
    };
    local
        .as_object_mut()
        .unwrap()
        .remove("progress_reattach_v1");
    tx.execute("INSERT INTO project_bindings(id,project_id,stage_id,binding_type,external_path,source_id,payload_json) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(id) DO UPDATE SET binding_type=excluded.binding_type,external_path=excluded.external_path,source_id=excluded.source_id,content_hash=NULL,payload_json=excluded.payload_json",params![id,c.project_id,c.stage_id,c.sync_type,path.to_string_lossy(),source,local.to_string()]).map_err(|e|e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    sync_summary(db, &c.project_id, c.stage_id.as_deref())
}
