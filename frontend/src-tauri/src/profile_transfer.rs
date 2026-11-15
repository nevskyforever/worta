use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const REQUEST_FILE: &str = ".nfprogress-profile-transfer.json";
const RESULT_FILE: &str = ".nfprogress-profile-transfer-result.json";

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TransferDirection {
    RealToTest,
    TestToReal,
}

#[derive(Debug, Deserialize, Serialize)]
struct TransferRequest {
    direction: TransferDirection,
    requested_at: u64,
}

#[derive(Debug, Serialize)]
pub(crate) struct TransferRequestResponse {
    message: String,
    restart_required: bool,
}

fn timestamp() -> Result<u64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|error| error.to_string())
}

fn profile_roots(active_root: &Path) -> Result<(PathBuf, PathBuf), String> {
    if active_root.file_name().and_then(|value| value.to_str()) == Some("test_data") {
        let real = active_root
            .parent()
            .ok_or_else(|| "Не удалось определить real profile.".to_string())?
            .to_path_buf();
        return Ok((real, active_root.to_path_buf()));
    }
    Ok((active_root.to_path_buf(), active_root.join("test_data")))
}

fn atomic_json(path: &Path, value: &Value) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "Некорректный путь служебного файла.".to_string())?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    fs::write(
        &temporary,
        format!(
            "{}\n",
            serde_json::to_string_pretty(value).unwrap_or_default()
        ),
    )
    .map_err(|error| error.to_string())?;
    fs::rename(&temporary, path).map_err(|error| error.to_string())
}

pub(crate) fn request(
    active_root: &Path,
    direction: TransferDirection,
) -> Result<TransferRequestResponse, String> {
    if !crate::developer_mode_available() {
        return Err("Режим разработчика недоступен в этой сборке.".to_string());
    }
    let (real_root, _) = profile_roots(active_root)?;
    let request = TransferRequest {
        direction,
        requested_at: timestamp()?,
    };
    atomic_json(
        &real_root.join(REQUEST_FILE),
        &serde_json::to_value(request).map_err(|error| error.to_string())?,
    )?;
    Ok(TransferRequestResponse {
        message: "Запрос сохранён. Перезапустите приложение для безопасной замены данных."
            .to_string(),
        restart_required: true,
    })
}

fn open_read_only(path: &Path) -> Result<Connection, String> {
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| error.to_string())
}

fn qualified(connection: &Connection) -> Result<(), String> {
    crate::sqlite::validate_database(connection).map_err(|error| error.to_string())?;
    let sqlite_owners: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM storage_ownership WHERE owner='sqlite'",
            [],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    if sqlite_owners != 4 {
        return Err("Профиль не имеет полного SQLite ownership.".to_string());
    }
    let migration_ready = metadata_status(
        connection,
        "game_metadata",
        "migration_status",
        "ready_for_tauri",
    )?;
    let documents_ready = metadata_status(
        connection,
        "document_metadata",
        "documents_json_migration",
        "complete",
    )?;
    if !matches!(
        (migration_ready, documents_ready),
        (None, None) | (Some(true), Some(true))
    ) {
        return Err("Профиль не прошёл qualified migration markers.".to_string());
    }
    Ok(())
}

fn metadata_status(
    connection: &Connection,
    table: &str,
    key: &str,
    expected: &str,
) -> Result<Option<bool>, String> {
    let raw = connection
        .query_row(
            &format!("SELECT value_json FROM {table} WHERE key=?1"),
            [key],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| error.to_string())?;
    let Some(raw) = raw else {
        return Ok(None);
    };
    Ok(Some(
        serde_json::from_str::<Value>(&raw)
            .ok()
            .and_then(|value| {
                value
                    .get("status")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .as_deref()
            == Some(expected),
    ))
}

fn user_counts(connection: &Connection) -> Result<Vec<i64>, String> {
    [
        "projects",
        "stages",
        "progress_entries",
        "notes",
        "documents",
        "document_bindings",
    ]
    .iter()
    .map(|table| {
        connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .map_err(|error| error.to_string())
    })
    .collect()
}

fn snapshot_database(source: &Path, destination: &Path) -> Result<(), String> {
    if destination.exists() {
        fs::remove_file(destination).map_err(|error| error.to_string())?;
    }
    // Both processes are stopped when the startup marker is handled, so the
    // inactive source can safely traverse the shared qualified migrations
    // before its snapshot is verified.
    drop(crate::sqlite::open_database(source).map_err(|error| error.to_string())?);
    let connection = open_read_only(source)?;
    qualified(&connection)?;
    connection
        .execute("VACUUM INTO ?1", [destination.to_string_lossy().as_ref()])
        .map_err(|error| error.to_string())?;
    let snapshot = open_read_only(destination)?;
    qualified(&snapshot)?;
    if user_counts(&connection)? != user_counts(&snapshot)? {
        return Err("Semantic verification обнаружила неполный snapshot.".to_string());
    }
    Ok(())
}

fn strip_developer_metadata(database: &Path) -> Result<(), String> {
    let connection = Connection::open(database).map_err(|error| error.to_string())?;
    crate::sqlite::register_fail_closed_remote_apply_guard(&connection)
        .map_err(|error| error.to_string())?;
    let raw = connection
        .query_row(
            "SELECT payload_json FROM game_state WHERE id=1",
            [],
            |row| row.get::<_, String>(0),
        )
        .map_err(|error| error.to_string())?;
    let mut payload = serde_json::from_str::<Value>(&raw).map_err(|error| error.to_string())?;
    if let Some(extensions) = payload.get_mut("extensions").and_then(Value::as_object_mut) {
        extensions.remove("developer_clock");
    }
    if let Some(extensions) = payload
        .get_mut("game")
        .and_then(Value::as_object_mut)
        .and_then(|game| game.get_mut("extensions"))
        .and_then(Value::as_object_mut)
    {
        extensions.remove("developer_clock");
    }
    let transaction = connection
        .unchecked_transaction()
        .map_err(|error| error.to_string())?;
    transaction
        .execute(
            "UPDATE game_state SET payload_json=?1,updated_at=datetime('now') WHERE id=1",
            [payload.to_string()],
        )
        .map_err(|error| error.to_string())?;
    transaction
        .execute(
            "DELETE FROM settings WHERE key IN ('developer_mode','today_for_test_mode','today_for_test_datetime')",
            [],
        )
        .map_err(|error| error.to_string())?;
    transaction.commit().map_err(|error| error.to_string())?;
    let verified = open_read_only(database)?;
    qualified(&verified)
}

fn activate(staging: &Path, destination: &Path, backup_root: &Path) -> Result<PathBuf, String> {
    fs::create_dir_all(destination).map_err(|error| error.to_string())?;
    fs::create_dir_all(backup_root).map_err(|error| error.to_string())?;
    let target = destination.join("nfprogress.db");
    let backup = backup_root.join("nfprogress.db");
    if target.exists() {
        snapshot_database(&target, &backup)?;
    }
    let rollback = destination.join(format!(".nfprogress-profile-rollback-{}.db", timestamp()?));
    if target.exists() {
        fs::rename(&target, &rollback).map_err(|error| error.to_string())?;
    }
    if let Err(error) = fs::rename(staging, &target) {
        if rollback.exists() {
            let _ = fs::rename(&rollback, &target);
        }
        return Err(error.to_string());
    }
    for suffix in ["-wal", "-shm"] {
        let sidecar = destination.join(format!("nfprogress.db{suffix}"));
        if sidecar.exists() {
            let _ = fs::remove_file(sidecar);
        }
    }
    let activated = open_read_only(&target);
    if let Err(error) = activated.and_then(|connection| qualified(&connection)) {
        let _ = fs::remove_file(&target);
        if rollback.exists() {
            let _ = fs::rename(&rollback, &target);
        }
        return Err(format!("Activation verification failed: {error}"));
    }
    if rollback.exists() {
        fs::remove_file(&rollback).map_err(|error| error.to_string())?;
    }
    Ok(backup)
}

fn execute(active_root: &Path, request: &TransferRequest) -> Result<Value, String> {
    let (real_root, test_root) = profile_roots(active_root)?;
    let (source, destination, label, strip_developer) = match request.direction {
        TransferDirection::RealToTest => (&real_root, &test_root, "real_to_test", false),
        TransferDirection::TestToReal => (&test_root, &real_root, "test_to_real", true),
    };
    let source_db = source.join("nfprogress.db");
    if !source_db.is_file() {
        return Err(format!(
            "Source database отсутствует: {}",
            source_db.display()
        ));
    }
    let staging = destination.parent().unwrap_or(destination).join(format!(
        ".nfprogress-profile-transfer-{}-{}.db",
        std::process::id(),
        timestamp()?
    ));
    snapshot_database(&source_db, &staging)?;
    let source_counts = user_counts(&open_read_only(&source_db)?)?;
    if strip_developer {
        strip_developer_metadata(&staging)?;
    }
    let staged = open_read_only(&staging)?;
    qualified(&staged)?;
    if source_counts != user_counts(&staged)? {
        let _ = fs::remove_file(&staging);
        return Err("Semantic verification пользовательских данных не пройдена.".to_string());
    }
    drop(staged);
    let backup_root = real_root
        .join("backups")
        .join(format!("profile-transfer-{label}-{}", timestamp()?));
    match activate(&staging, destination, &backup_root) {
        Ok(backup) => Ok(json!({
            "status": "complete",
            "direction": label,
            "backup": backup,
            "integrity_check": "ok",
            "semantic_verification": "ok",
            "activation": "atomic",
        })),
        Err(error) => {
            let _ = fs::remove_file(&staging);
            Err(error)
        }
    }
}

pub(crate) fn process_pending(active_root: &Path) -> Result<(), String> {
    if !crate::developer_mode_available() {
        return Ok(());
    }
    let (real_root, _) = profile_roots(active_root)?;
    let marker = real_root.join(REQUEST_FILE);
    if !marker.is_file() {
        return Ok(());
    }
    let request: TransferRequest =
        serde_json::from_slice(&fs::read(&marker).map_err(|error| error.to_string())?)
            .map_err(|error| format!("Некорректный запрос замены профиля: {error}"))?;
    let result = match execute(active_root, &request) {
        Ok(result) => result,
        Err(error) => json!({"status":"error","error":error}),
    };
    atomic_json(&real_root.join(RESULT_FILE), &result)?;
    fs::remove_file(&marker).map_err(|error| error.to_string())?;
    // A failed transfer leaves the destination database unchanged (or restores
    // it from rollback). Keep normal startup available and surface the stored
    // result through Developer Mode instead of turning it into a storage gate.
    Ok(())
}

pub(crate) fn take_result(active_root: &Path) -> Result<Option<Value>, String> {
    if !crate::developer_mode_available() {
        return Err("Режим разработчика недоступен в этой сборке.".to_string());
    }
    let (real_root, _) = profile_roots(active_root)?;
    let result_path = real_root.join(RESULT_FILE);
    if !result_path.is_file() {
        return Ok(None);
    }
    let result = serde_json::from_slice::<Value>(
        &fs::read(&result_path).map_err(|error| error.to_string())?,
    )
    .map_err(|error| format!("Некорректный результат замены профиля: {error}"))?;
    fs::remove_file(result_path).map_err(|error| error.to_string())?;
    Ok(Some(result))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prepare_profile(root: &Path, marker: &str) {
        fs::create_dir_all(root).unwrap();
        let connection = crate::sqlite::open_database(&root.join("nfprogress.db")).unwrap();
        connection
            .execute("UPDATE storage_ownership SET owner='sqlite'", [])
            .unwrap();
        connection.execute("INSERT INTO game_metadata(key,value_json) VALUES('migration_status','{\"status\":\"ready_for_tauri\"}')", []).unwrap();
        connection.execute("INSERT INTO document_metadata(key,value_json) VALUES('documents_json_migration','{\"status\":\"complete\"}')", []).unwrap();
        connection
            .execute(
                "INSERT INTO settings(key,value_json) VALUES('profile_marker',?1)",
                [serde_json::to_string(marker).unwrap()],
            )
            .unwrap();
    }

    fn profile_marker(database: &Path) -> String {
        let connection = open_read_only(database).unwrap();
        let raw: String = connection
            .query_row(
                "SELECT value_json FROM settings WHERE key='profile_marker'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        serde_json::from_str(&raw).unwrap()
    }

    #[test]
    fn real_to_test_creates_verified_backup_and_activates_snapshot() {
        let real_root = std::env::temp_dir().join(format!(
            "nfprogress-profile-transfer-activation-{}",
            std::process::id()
        ));
        let test_root = real_root.join("test_data");
        let _ = fs::remove_dir_all(&real_root);
        prepare_profile(&real_root, "real");
        prepare_profile(&test_root, "old-test");

        let result = execute(
            &test_root,
            &TransferRequest {
                direction: TransferDirection::RealToTest,
                requested_at: 1,
            },
        )
        .unwrap();

        assert_eq!(profile_marker(&test_root.join("nfprogress.db")), "real");
        let backup = PathBuf::from(result["backup"].as_str().unwrap());
        assert_eq!(profile_marker(&backup), "old-test");
        qualified(&open_read_only(&backup).unwrap()).unwrap();
        let _ = fs::remove_dir_all(real_root);
    }

    #[test]
    fn stripping_developer_metadata_preserves_user_game_data() {
        let root = std::env::temp_dir().join(format!(
            "nfprogress-profile-transfer-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let database = root.join("nfprogress.db");
        let connection = crate::sqlite::open_database(&database).unwrap();
        connection
            .execute("UPDATE storage_ownership SET owner='sqlite'", [])
            .unwrap();
        connection.execute("INSERT INTO game_metadata(key,value_json) VALUES('migration_status','{\"status\":\"ready_for_tauri\"}')", []).unwrap();
        connection.execute("INSERT INTO document_metadata(key,value_json) VALUES('documents_json_migration','{\"status\":\"complete\"}')", []).unwrap();
        connection.execute("INSERT INTO game_state(id,schema_version,payload_json,updated_at) VALUES(1,2,?1,datetime('now')) ON CONFLICT(id) DO UPDATE SET payload_json=excluded.payload_json", [json!({"gamer":{"coins":42},"extensions":{"developer_clock":{"enabled":true,"datetime":"2040-01-01T12:00:00"},"kept":true}}).to_string()]).unwrap();
        connection
            .execute(
                "INSERT INTO settings(key,value_json) VALUES('developer_mode','true')",
                [],
            )
            .unwrap();
        drop(connection);

        strip_developer_metadata(&database).unwrap();

        let connection = open_read_only(&database).unwrap();
        let payload: String = connection
            .query_row(
                "SELECT payload_json FROM game_state WHERE id=1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let payload: Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(payload["gamer"]["coins"], json!(42));
        assert_eq!(payload["extensions"]["kept"], json!(true));
        assert!(payload["extensions"].get("developer_clock").is_none());
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM settings WHERE key='developer_mode'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        let _ = fs::remove_dir_all(root);
    }
}
