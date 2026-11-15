//! Local support journal, isolated from application entities and cloud transport.
//! Both ingress and export rebuild safe structured events; arbitrary objects,
//! messages, paths, tokens and content are never serialized into the journal.
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::path::Path;
pub(crate) const MAX_EVENTS: usize = 512;
pub(crate) const MAX_BYTES: usize = 512 * 1024;
const COPY_EVENTS: usize = 100;
const SUBSYSTEMS: &[&str] = &[
    "application",
    "sync",
    "encryption",
    "projects",
    "stages",
    "documents",
    "migrations",
    "game",
    "developer",
];
const OPERATIONS: &[&str] = &[
    "runtime_start",
    "sync_cycle",
    "retry",
    "metadata_migration",
    "stage_migration",
    "note_migration",
    "progress_migration",
    "progress_writer",
    "progress_apply",
    "progress_resolution",
    "progress_rebase",
    "progress_projection_rebuild",
    "map_migration",
    "document_migration",
    "document_writer",
    "document_apply",
    "document_resolution",
    "map_writer",
    "map_apply",
    "map_resolution",
    "catalog_migration",
    "conflict_resolution",
    "project_connect",
    "project_import",
    "unlock",
    "create",
    "update",
    "reorder",
    "load",
    "save",
    "documents_sync",
    "restore_streak",
    "create_streak",
    "copy",
    "export",
    "clear",
];
const EVENT_CODES: &[&str] = &[
    "requested",
    "started",
    "succeeded",
    "failed",
    "cancelled",
    "sync_result",
    "blocker",
    "native_validation",
    "native_attempt",
    "native_succeeded",
    "native_failed",
    "pull_result",
    "upload_result",
    "apply_result",
    "ack_result",
];
const SAFE_CODES: &[&str] = &[
    "compression_unsupported",
    "compression_invalid_stream",
    "compression_input_limit",
    "compression_output_limit",
    "compression_resource_limit",
    "compression_length_mismatch",
    "progress_readers_not_ready",
    "progress_reader_required",
    "progress_resource_limit",
    "progress_codec_unsupported",
    "invalid_progress_payload",
    "progress_scope_mismatch",
    "progress_base_inconsistent",
    "progress_precision_unsupported",
    "progress_unsupported_extension",
    "progress_histories_diverged",
    "progress_descendant_rebase_required",
    "progress_resolution_stale",
    "progress_expected_head_stale",
    "progress_projection_rebuild_required",
    "progress_publication_pending",
    "progress_local_only_project",
    "progress_parent_unknown",
    "progress_rebase_invalid",
    "progress_rebase_order_invalid",
    "progress_correction_invalid",
    "document_readers_not_ready",
    "document_reader_required",
    "document_unsupported_extension",
    "document_unsupported_structure",
    "document_resource_limit",
    "document_codec_unsupported",
    "invalid_document_payload",
    "document_migration_orphan",
    "document_legacy_migration_incomplete",
    "document_scope_occupied",
    "document_cross_project_move_blocked",
    "document_conflict",
    "document_resolution_stale",
    "document_stale_heads",
    "document_migration_blocked",
    "document_scope_mismatch",
    "document_project_not_connected",
    "document_parent_unknown",
    "document_parent_invalid",
    "document_causal_proof_limit",
    "map_readers_not_ready",
    "map_reader_required",
    "map_scope_mismatch",
    "map_parent_unknown",
    "map_conflict_requires_resolution",
    "map_combined_stale_heads",
    "map_resolution_stale",
    "map_import_stale",
    "map_note_identity_collision",
    "map_note_link_invalid",
    "map_unsupported_extension",
    "map_resource_limit",
    "invalid_map_payload",
    "map_storage_unavailable",
    "self_echo_pending",
    "captured",
    "waiting",
    "content_note_parent_pending",
    "unresolved_note_conflict",
    "content_note_local_candidate",
    "note_resolution_missing_proof",
    "note_resolution_invalid",
    "stale_note_resolution",
    "content_note_readers_not_ready",
    "content_note_reader_required",
    "content_note_candidate_blocked",
    "stale_content_note_frame",
    "content_note_codec_unsupported",
    "content_note_map_owned",
    "content_note_unsupported_source",
    "content_note_scope_mismatch",
    "content_note_resource_limit",
    "applied",
    "folder",
    "folder_order",
    "folder_membership",
    "project_order",
    "invalid_catalog_frame",
    "catalog_dependency_missing",
    "catalog_parent_unknown",
    "catalog_membership_changed",
    "catalog_project_unproven",
    "catalog_folder_has_members",
    "catalog_resource_limit",
    "catalog_dependency_conflict",
    "catalog_conflict",
    "stale_catalog_resolution",
    "unsupported_catalog_source",
    "catalog_disconnect_requires_reconciliation",
    "account_entity_codec_not_activated",
    "account_scope_rejected",
    "decrypt_failed",
    "unknown_error",
    "Validation",
    "NotFound",
    "Database",
    "PrerequisiteMissing",
    "InvalidState",
    "ApiError",
    "TypeError",
    "Error",
    "KeyNotProvisionedError",
    "StaleAuthContextError",
    "ApiResponseTooLargeError",
    "streak_restore_no_history",
    "diagnostic_storage_unavailable",
    "metadata_import_resource_limit",
    "metadata_import_remaining_work",
    "invalid_stage_frame",
    "structural_authentication_failed",
    "structural_scope_mismatch",
    "stage_dependency_missing",
    "stage_membership_head_changed",
    "stage_order_membership_mismatch",
    "stage_order_foreign_stage",
    "stage_tombstone_child_manifest_incomplete",
    "stale_structural_resolution",
    "project_metadata_authority_unresolved",
    "unresolved_structural_conflict",
    "stage_dependency_proof_limit",
    "structural_local_lineage_limit",
    "metadata_scope_mismatch",
    "unsupported_stage_source",
    "blocked",
    "orphan",
    "conflict_preserved",
    "active",
    "conflict",
    "publication_pending",
    "published_self_echo_pending",
    "candidate_captured",
    "structural_local",
    "resolution_pending",
    "ready",
    "completed",
    "remaining_work",
    "retryable_error",
    "logged_out",
    "key_locked",
    "global",
    "project",
    "stage",
    "no_progress",
    "advanced",
    "already_acknowledged",
    "already_advanced",
    "stale",
    "CloudProjectBootstrapBlockedError",
    "AccountCryptoAlreadyProvisionedError",
    "AccountCryptoProvisioningConflictError",
    "RecoveryKeyConfirmationRequiredError",
    "AccountCryptoProvisioningDisposedError",
    "metadata_import_continuation_required",
    "dependency_not_synced",
    "unsupported_content_format",
    "invalid_note_payload",
    "missing_created_at",
    "invalid_created_at",
    "missing_updated_at",
    "invalid_updated_at",
    "remote_project_not_active",
    "bootstrap_operation_in_progress",
    "local_project_lineage_not_found",
    "missing_local_binding",
    "missing_remote_registration",
    "lineage_conflict",
    "local_device_conflict",
    "binding_not_ready",
    "initializing",
    "legacy",
    "paused",
];
fn allowed(value: &Value, values: &[&str]) -> bool {
    value.as_str().is_some_and(|s| values.contains(&s))
}
fn open(path: &Path) -> Result<Connection, String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|_| "diagnostic_storage_unavailable")?;
    }
    let db = Connection::open(path).map_err(|_| "diagnostic_storage_unavailable")?;
    db.busy_timeout(std::time::Duration::from_secs(2))
        .map_err(|_| "diagnostic_storage_unavailable")?;
    db.execute_batch("PRAGMA page_size=4096; PRAGMA max_page_count=512; CREATE TABLE IF NOT EXISTS diagnostic_events(seq INTEGER PRIMARY KEY, event_json TEXT NOT NULL, bytes INTEGER NOT NULL);").map_err(|_|"diagnostic_storage_unavailable")?;
    Ok(db)
}
fn sanitize(db: &Connection, raw: &Value) -> Option<Value> {
    if raw["schema_version"] != 1
        || !allowed(&raw["subsystem"], SUBSYSTEMS)
        || !allowed(&raw["operation"], OPERATIONS)
        || !allowed(&raw["code"], EVENT_CODES)
        || !allowed(&raw["severity"], &["info", "warning", "error"])
    {
        return None;
    }
    let correlation = raw["correlation_id"].as_str()?;
    if !crate::project_metadata_sync::uuid(correlation) {
        return None;
    }
    let stamp = raw["timestamp"].as_str()?;
    if !(24..=27).contains(&stamp.len())
        || !stamp.ends_with('Z')
        || !stamp
            .chars()
            .all(|c| c.is_ascii_digit() || "-:.TZ".contains(c))
        || !db
            .query_row("SELECT julianday(?1) IS NOT NULL", [stamp], |r| {
                r.get::<_, bool>(0)
            })
            .ok()?
    {
        return None;
    }
    let mut context = serde_json::Map::new();
    if let Some(input) = raw["context"].as_object() {
        for (k, v) in input {
            let keep = match k.as_str() {
                "count" | "applied" | "conflicts" | "orphans" | "pending" | "duration_ms"
                | "http_status" => v.as_u64().is_some_and(|n| n <= 1_000_000_000),
                "retry" | "supported" => v.is_boolean(),
                "status" | "error_code" | "error_class" | "target_type" => allowed(v, SAFE_CODES),
                _ => false,
            };
            if keep {
                context.insert(k.clone(), v.clone());
            }
        }
    }
    Some(
        json!({"schema_version":1,"timestamp":stamp,"severity":raw["severity"],"subsystem":raw["subsystem"],"operation":raw["operation"],"code":raw["code"],"correlation_id":correlation,"context":context}),
    )
}
pub(crate) fn append(path: &Path, events: &[Value]) -> Result<(), String> {
    if events.len() > 64 {
        return Err("diagnostic_batch_limit".into());
    }
    let mut db = open(path)?;
    let safe: Vec<String> = events
        .iter()
        .filter_map(|e| sanitize(&db, e).map(|v| v.to_string()))
        .collect();
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|_| "diagnostic_storage_unavailable")?;
    for text in safe {
        tx.execute(
            "INSERT INTO diagnostic_events(event_json,bytes) VALUES(?1,?2)",
            params![text, text.len()],
        )
        .map_err(|_| "diagnostic_storage_unavailable")?;
    }
    tx.execute("DELETE FROM diagnostic_events WHERE seq NOT IN (SELECT seq FROM diagnostic_events ORDER BY seq DESC LIMIT ?1)",[MAX_EVENTS as i64]).map_err(|_|"diagnostic_storage_unavailable")?;
    loop {
        let bytes: i64 = tx
            .query_row(
                "SELECT COALESCE(SUM(bytes),0) FROM diagnostic_events",
                [],
                |r| r.get(0),
            )
            .map_err(|_| "diagnostic_storage_unavailable")?;
        if bytes <= MAX_BYTES as i64 {
            break;
        }
        tx.execute(
            "DELETE FROM diagnostic_events WHERE seq=(SELECT MIN(seq) FROM diagnostic_events)",
            [],
        )
        .map_err(|_| "diagnostic_storage_unavailable")?;
    }
    tx.commit()
        .map_err(|_| "diagnostic_storage_unavailable".into())
}
pub(crate) fn stats(path: &Path) -> Result<Value, String> {
    let db = open(path)?;
    let (count,bytes,last):(i64,i64,Option<String>)=db.query_row("SELECT count(*),COALESCE(sum(bytes),0),(SELECT json_extract(event_json,'$.timestamp') FROM diagnostic_events ORDER BY seq DESC LIMIT 1) FROM diagnostic_events",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).map_err(|_|"diagnostic_storage_unavailable")?;
    Ok(json!({"count":count,"bytes":bytes,"last_event_at":last}))
}
pub(crate) fn text(path: &Path, copy: bool) -> Result<String, String> {
    let db = open(path)?;
    let total: i64 = db
        .query_row("SELECT count(*) FROM diagnostic_events", [], |r| r.get(0))
        .map_err(|_| "diagnostic_storage_unavailable")?;
    let limit = if copy { COPY_EVENTS } else { MAX_EVENTS };
    let mut q=db.prepare("SELECT event_json FROM (SELECT seq,event_json FROM diagnostic_events ORDER BY seq DESC LIMIT ?1) ORDER BY seq").map_err(|_|"diagnostic_storage_unavailable")?;
    let rows = q
        .query_map([limit as i64], |r| r.get::<_, String>(0))
        .map_err(|_| "diagnostic_storage_unavailable")?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "diagnostic_storage_unavailable")?;
    let mut events: Vec<Value> = rows
        .iter()
        .filter_map(|s| serde_json::from_str(s).ok())
        .filter_map(|v| sanitize(&db, &v))
        .collect();
    if copy {
        while events
            .iter()
            .map(|e| e.to_string().len() + 1)
            .sum::<usize>()
            > 60 * 1024
        {
            events.remove(0);
        }
    }
    let now: String = db
        .query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')", [], |r| {
            r.get(0)
        })
        .map_err(|_| "diagnostic_storage_unavailable")?;
    let header = json!({"type":"worta_diagnostics","schema_version":1,"app_version":env!("CARGO_PKG_VERSION"),"runtime":"tauri","os":std::env::consts::OS,"architecture":std::env::consts::ARCH,"build":if cfg!(debug_assertions){"debug"}else{"release"},"exported_at":now,"retained_events":total,"included_events":events.len(),"truncated":total as usize>events.len(),"privacy":"allowlisted technical fields only; local journal; no automatic upload"});
    let mut out = header.to_string();
    out.push('\n');
    for event in events {
        out.push_str(&event.to_string());
        out.push('\n');
    }
    Ok(out)
}
pub(crate) fn clear(path: &Path) -> Result<(), String> {
    let db = open(path)?;
    db.execute_batch("DELETE FROM diagnostic_events; VACUUM;")
        .map_err(|_| "diagnostic_storage_unavailable".into())
}
pub(crate) fn export(path: &Path, destination: &Path) -> Result<(), String> {
    let content = text(path, false)?;
    // Never overwrite the journal/database itself; no application payload input.
    if destination == path || destination.extension().and_then(|s| s.to_str()) != Some("jsonl") {
        return Err("diagnostic_export_path_invalid".into());
    }
    std::fs::write(destination, content).map_err(|_| "diagnostic_export_failed".into())
}

pub(crate) fn native_streak_event(
    code: &str,
    correlation: &str,
    error_code: Option<&str>,
) -> Value {
    // UTC timestamp is populated with the same SQLite clock used by the journal.
    let stamp = Connection::open_in_memory()
        .and_then(|db| {
            db.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')", [], |r| {
                r.get::<_, String>(0)
            })
        })
        .unwrap_or_default();
    json!({"schema_version":1,"timestamp":stamp,"severity":if error_code.is_some(){"error"}else{"info"},"subsystem":"game","operation":"restore_streak","code":code,"correlation_id":correlation,"context":error_code.map(|code|json!({"error_code":code})).unwrap_or(json!({}))})
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(code: &str, n: usize) -> Value {
        json!({"schema_version":1,"timestamp":"2026-10-02T10:00:00.000Z","severity":"info","subsystem":"game","operation":"restore_streak","code":code,"correlation_id":"123e4567-e89b-42d3-a456-426614174000","context":{"count":n}})
    }
    fn directory() -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "worta-diagnostics-{}",
            crate::project_metadata_sync::new_event_id().unwrap()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }
    #[test]
    fn diagnostics_survive_reopen_prune_oldest_copy_latest_and_clear() {
        let dir = directory();
        let path = dir.join("support.db");
        for base in (0..576).step_by(64) {
            append(
                &path,
                &(base..base + 64)
                    .map(|n| fixture("succeeded", n))
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        }
        let info = stats(&path).unwrap();
        assert_eq!(info["count"], 512);
        assert!(info["bytes"].as_u64().unwrap() <= MAX_BYTES as u64);
        assert!(std::fs::metadata(&path).unwrap().len() <= 512 * 4096);
        let copy = text(&path, true).unwrap();
        let rows: Vec<Value> = copy
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect();
        assert_eq!(rows[0]["truncated"], true);
        assert_eq!(rows.len(), 101);
        assert_eq!(rows[1]["context"]["count"], 476);
        assert_eq!(rows.last().unwrap()["context"]["count"], 575);
        let output = dir.join("export.jsonl");
        export(&path, &output).unwrap();
        let full = std::fs::read_to_string(output).unwrap();
        let rows: Vec<Value> = full
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect();
        assert_eq!(rows[0]["truncated"], false);
        assert_eq!(rows[1]["context"]["count"], 64);
        assert_eq!(rows[0]["runtime"], "tauri");
        clear(&path).unwrap();
        assert_eq!(stats(&path).unwrap()["count"], 0);
        assert_eq!(text(&path, false).unwrap().lines().count(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn diagnostics_reject_secrets_on_ingress_and_export_even_for_tampered_rows() {
        let dir = directory();
        let path = dir.join("support.db");
        let secret = "SECRET_PASSWORD_AMK_KEY_TOKEN_AUTHORIZATION_NOTE_DOCUMENT_PAYLOAD";
        let mut event = fixture("failed", 1);
        for key in [
            "password",
            "AMK",
            "key",
            "token",
            "Authorization",
            "nonce",
            "name",
            "email",
            "note",
            "document",
            "payload",
            "error_code",
        ] {
            event["context"][key] = json!(secret);
        }
        event["message"] = json!(secret);
        event["stack"] = json!(secret);
        append(&path, &[event.clone()]).unwrap();
        {
            let db = open(&path).unwrap();
            db.execute(
                "INSERT INTO diagnostic_events(event_json,bytes) VALUES(?1,?2)",
                params![event.to_string(), event.to_string().len()],
            )
            .unwrap();
        }
        for copy in [true, false] {
            let out = text(&path, copy).unwrap();
            assert!(!out.contains(secret));
            assert!(!out.contains("password"));
        }
        let out = dir.join("safe.jsonl");
        export(&path, &out).unwrap();
        assert!(!std::fs::read_to_string(out).unwrap().contains(secret));
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn diagnostics_invalid_events_debug_and_large_batches_are_not_persisted() {
        let dir = directory();
        let path = dir.join("support.db");
        for field in [
            "timestamp",
            "severity",
            "code",
            "subsystem",
            "operation",
            "correlation_id",
        ] {
            let mut event = fixture("started", 1);
            event[field] = json!("SECRET");
            append(&path, &[event]).unwrap();
        }
        assert_eq!(stats(&path).unwrap()["count"], 0);
        assert!(append(&path, &vec![fixture("started", 1); 65]).is_err());
        let mut debug = fixture("started", 1);
        debug["severity"] = json!("debug");
        append(&path, &[debug]).unwrap();
        assert_eq!(stats(&path).unwrap()["count"], 0);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn diagnostics_native_streak_sequence_preserves_correlation_without_message() {
        let dir = directory();
        let path = dir.join("support.db");
        let id = "123e4567-e89b-42d3-a456-426614174000";
        append(
            &path,
            &[
                native_streak_event("native_validation", id, None),
                native_streak_event("native_attempt", id, None),
                native_streak_event("native_failed", id, Some("Validation")),
            ],
        )
        .unwrap();
        let out = text(&path, false).unwrap();
        let rows: Vec<Value> = out
            .lines()
            .skip(1)
            .map(|s| serde_json::from_str(s).unwrap())
            .collect();
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|e| e["correlation_id"] == id));
        assert_eq!(rows[2]["context"]["error_code"], "Validation");
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn diagnostics_frontend_and_native_allowlists_match() {
        let source = include_str!("../../src/diagnostics/events.ts");
        for (name, values) in [
            ("subsystems", SUBSYSTEMS),
            ("operations", OPERATIONS),
            ("eventCodes", EVENT_CODES),
            ("safeCodes", SAFE_CODES),
        ] {
            let list = source
                .split(&format!("export const {name} = ["))
                .nth(1)
                .unwrap()
                .split(']')
                .next()
                .unwrap();
            let parsed: Vec<&str> = list
                .split(',')
                .map(|s| s.trim().trim_matches('\''))
                .collect();
            assert_eq!(parsed, values);
        }
    }
}
