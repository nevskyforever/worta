//! SQLite-owned document records and bounded local Word/Scrivener boundaries.
//!
//! The frontend only passes typed document data or bytes selected by the user.
//! This module never exposes an arbitrary read/write filesystem bridge.

use std::fs::{self, OpenOptions};
use std::io::{Cursor, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use quick_xml::events::Event;
use quick_xml::Reader;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use zip::write::SimpleFileOptions;
use zip::ZipArchive;

#[path = "external_reattach.rs"]
mod external_reattach;

const EMPTY_DOCUMENT: &str = r#"{"type":"doc","content":[{"type":"paragraph"}]}"#;
const MAX_DOCX_BYTES: usize = 100 * 1024 * 1024;
const MAX_DOCX_ENTRIES: usize = 10_000;
const MAX_DOCX_EXPANDED_BYTES: u64 = 250 * 1024 * 1024;
const MIN_LINE_HEIGHT_MULTIPLIER: f64 = 0.5;
const MAX_LINE_HEIGHT_MULTIPLIER: f64 = 5.0;
const MAX_XML_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WordImportResult {
    pub content: Value,
    pub symbols: usize,
    pub hash: String,
    pub changed: bool,
    pub project: Option<Value>,
    pub progress: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DocumentScope {
    pub project_id: String,
    #[serde(default)]
    pub stage_id: Option<String>,
    #[serde(default)]
    pub expected_heads: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DocumentSaveCommand {
    pub project_id: String,
    #[serde(default)]
    pub stage_id: Option<String>,
    pub content: Value,
    #[serde(default)]
    pub expected_heads: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DocumentRenameCommand {
    pub project_id: String,
    #[serde(default)]
    pub stage_id: Option<String>,
    pub title: String,
    #[serde(default)]
    pub expected_heads: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DocumentFileCommand {
    pub project_id: String,
    #[serde(default)]
    pub stage_id: Option<String>,
    pub path: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DocumentWordWriteCommand {
    pub project_id: String,
    #[serde(default)]
    pub stage_id: Option<String>,
    pub content_base64: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DocumentExternalAcceptCommand {
    pub project_id: String,
    #[serde(default)]
    pub stage_id: Option<String>,
    pub content: Value,
    pub source_hash: String,
    #[serde(default)]
    pub expected_heads: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WordImportCommand {
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub stage_id: Option<String>,
    pub bytes: Vec<u8>,
    #[serde(default)]
    pub filename: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WordExportCommand {
    pub content: Value,
    pub target_path: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DocumentProgressCommand {
    pub project_id: String,
    #[serde(default)]
    pub stage_id: Option<String>,
    #[serde(default)]
    pub content: Option<Value>,
    #[serde(default)]
    pub expected_heads: Option<Value>,
}

#[derive(Debug, Serialize)]
pub struct ExternalDocumentResult {
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_base64: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ScrivenerItem {
    pub id: String,
    pub title: String,
    pub children: Vec<ScrivenerItem>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScrivenerInspectCommand {
    pub path: String,
}

#[derive(Debug)]
struct SourceSnapshot {
    bytes: Vec<u8>,
    hash: String,
    modified_at: String,
}

fn now() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = seconds / 86_400;
    let remainder = seconds % 86_400;
    let (year, month, day) = civil_from_days(days as i64);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        remainder / 3600,
        (remainder % 3600) / 60,
        remainder % 60
    )
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    (y + if m <= 2 { 1 } else { 0 }, m, d)
}

fn sha256(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn scope_key(project_id: &str, stage_id: Option<&str>) -> String {
    format!("{project_id}:{}", stage_id.unwrap_or("project"))
}

fn stable_id(scope: &str) -> String {
    format!("document-{}", &sha256(scope.as_bytes())[..40])
}

fn binding_id(document_id: &str) -> String {
    format!("document-binding-{}", &sha256(document_id.as_bytes())[..40])
}

fn document_id_for_scope(
    connection: &rusqlite::Connection,
    project_id: &str,
    stage_id: Option<&str>,
) -> Result<String, String> {
    let scope = scope_key(project_id, stage_id);
    let existing: Option<String> = connection
        .query_row(
            "SELECT id FROM documents WHERE scope_key=?1",
            [scope.as_str()],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    if let Some(id) = existing {
        return Ok(id);
    }
    let base = stable_id(&scope);
    for generation in 0..4096 {
        let candidate = if generation == 0 {
            base.clone()
        } else {
            format!("{base}-{generation}")
        };
        let occupied: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM documents WHERE id=?1 AND scope_key!=?2)",
                params![candidate, scope],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        if !occupied {
            return Ok(candidate);
        }
    }
    Err("document_identity_collision".into())
}

fn validate_content(content: &Value) -> Result<(), String> {
    if content.get("type").and_then(Value::as_str) != Some("doc") {
        return Err("Документ должен быть в формате Tiptap JSON.".to_string());
    }
    if serde_json::to_vec(content)
        .map_err(|_| "Некорректный документ.".to_string())?
        .len()
        > 32 * 1024 * 1024
    {
        return Err("Документ превышает допустимый размер 32 МБ.".to_string());
    }
    Ok(())
}

fn symbols(value: &Value) -> usize {
    match value {
        Value::Object(object) => {
            object
                .get("text")
                .and_then(Value::as_str)
                .map_or(0, |text| text.chars().count())
                + object
                    .get("content")
                    .and_then(Value::as_array)
                    .map_or(0, |items| items.iter().map(symbols).sum())
        }
        Value::Array(items) => items.iter().map(symbols).sum(),
        _ => 0,
    }
}

fn open() -> Result<(rusqlite::Connection, PathBuf), String> {
    let root = crate::sqlite_data_root()?;
    fs::create_dir_all(&root)
        .map_err(|error| format!("Не удалось открыть data directory: {error}"))?;
    let mut connection = crate::sqlite::open_database(&root.join("nfprogress.db"))
        .map_err(|error| error.to_string())?;
    migrate_legacy_documents(&mut connection, &root)?;
    Ok((connection, root))
}

fn project_entity(
    connection: &rusqlite::Connection,
    project_id: &str,
    stage_id: Option<&str>,
) -> Result<(String, Value, bool), String> {
    let (id, raw): (String, String) = if let Some(stage_id) = stage_id {
        connection
            .query_row(
                "SELECT id,payload_json FROM stages WHERE id=?1 AND project_id=?2",
                params![stage_id, project_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|_| "Этап не найден.".to_string())?
    } else {
        connection
            .query_row(
                "SELECT id,payload_json FROM projects WHERE id=?1",
                [project_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|_| "Проект не найден.".to_string())?
    };
    let payload: Value =
        serde_json::from_str(&raw).map_err(|_| "Некорректный payload проекта.".to_string())?;
    let app = payload
        .get("work_method")
        .and_then(Value::as_str)
        .unwrap_or("manual")
        == "app";
    Ok((id, payload, app))
}

fn validate_scope(
    connection: &rusqlite::Connection,
    project_id: &str,
    stage_id: Option<&str>,
) -> Result<(String, Value), String> {
    let (id, payload, app) = project_entity(connection, project_id, stage_id)?;
    if stage_id.is_none()
        && payload
            .get("stages")
            .and_then(Value::as_array)
            .is_some_and(|stages| !stages.is_empty())
    {
        return Err("У проекта с этапами нет отдельного текста.".to_string());
    }
    if !app {
        return Err("Для работы с текстом выберите метод «В приложении».".to_string());
    }
    Ok((id, payload))
}

fn parse_content(raw: String) -> Result<Value, String> {
    serde_json::from_str(&raw).map_err(|_| "Некорректный документ в SQLite.".to_string())
}

fn document_row(
    connection: &rusqlite::Connection,
    project_id: &str,
    stage_id: Option<&str>,
) -> Result<Option<Value>, String> {
    if connection.is_autocommit() {
        connection
            .execute_batch("BEGIN DEFERRED")
            .map_err(|e| e.to_string())?;
        let result = document_row(connection, project_id, stage_id);
        connection
            .execute_batch(if result.is_ok() { "COMMIT" } else { "ROLLBACK" })
            .map_err(|e| e.to_string())?;
        return result;
    }
    let scope = scope_key(project_id, stage_id);
    let row = connection
        .query_row(
            "SELECT id,title,content_json,content_format,created_at,updated_at,revision,extensions_json FROM documents WHERE scope_key=?1",
            [scope.as_str()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, String>(7)?,
                ))
            },
        )
        .optional()
        .map_err(|error| error.to_string())?;
    let Some((
        id,
        title,
        content_raw,
        content_format,
        created_at,
        updated_at,
        revision,
        extensions_raw,
    )) = row
    else {
        return Ok(None);
    };
    let content = parse_content(content_raw)?;
    let extensions = parse_content(extensions_raw)?;
    let binding = connection
        .query_row(
            "SELECT binding_type,external_path,source_id,last_external_hash,last_synced_revision,last_synced_hash,last_synced_at,sync_state,payload_json FROM document_bindings WHERE document_id=?1",
            [id.as_str()],
            |row| {
                Ok(serde_json::json!({
                    "type": row.get::<_, String>(0)?,
                    "path": row.get::<_, String>(1)?,
                    "source_id": row.get::<_, Option<String>>(2)?,
                    "last_external_hash": row.get::<_, Option<String>>(3)?,
                    "last_synced_revision": row.get::<_, i64>(4)?,
                    "last_synced_hash": row.get::<_, Option<String>>(5)?,
                    "last_synced_at": row.get::<_, Option<String>>(6)?,
                    "sync_state": row.get::<_, String>(7)?,
                    "metadata": parse_content(row.get::<_, String>(8)?).unwrap_or(Value::Object(Map::new())),
                }))
            },
        )
        .optional()
        .map_err(|error| error.to_string())?;
    let state = binding
        .as_ref()
        .and_then(|value| value.get("sync_state"))
        .and_then(Value::as_str)
        .unwrap_or("unlinked");
    let last_external_hash = binding
        .as_ref()
        .and_then(|value| value.get("last_external_hash"))
        .cloned()
        .unwrap_or(Value::Null);
    let last_synced_hash = binding
        .as_ref()
        .and_then(|value| value.get("last_synced_hash"))
        .cloned()
        .unwrap_or(Value::Null);
    Ok(Some(serde_json::json!({
        "document_id": id,
        "expected_heads": crate::document_sync::expected(connection,project_id,&id).map_err(|e|e.to_string())?,
        "project_id": project_id,
        "stage_id": stage_id,
        "title": title,
        "content": content,
        "content_format": content_format,
        "created_at": created_at,
        "updated_at": updated_at,
        "revision": revision,
        "extensions": extensions,
        "exists": true,
        "docx_path": binding.as_ref().and_then(|value| value.get("path")).cloned().unwrap_or(Value::Null),
        "sync_state": state,
        "last_external_hash": last_external_hash,
        "last_synced_hash": last_synced_hash,
        "last_synced_at": binding.as_ref().and_then(|value| value.get("last_synced_at")).cloned().unwrap_or(Value::Null),
        "local_dirty": state == "local_changed" || state == "conflict",
        "word_dirty": state == "external_changed" || state == "conflict",
        "symbols": symbols(&content),
        "has_content": symbols(&content) > 0,
    })))
}

fn new_document_value(project_id: &str, stage_id: Option<&str>, title: &str) -> Value {
    serde_json::json!({
        "document_id": stable_id(&scope_key(project_id, stage_id)),
        "project_id": project_id,
        "stage_id": stage_id,
        "title": title,
        "content": serde_json::from_str::<Value>(EMPTY_DOCUMENT).unwrap(),
        "content_format": "tiptap-json/v1",
        "created_at": Value::Null,
        "updated_at": Value::Null,
        "revision": 0,
        "extensions": {},
        "exists": false,
        "docx_path": Value::Null,
        "sync_state": "unlinked",
        "last_external_hash": Value::Null,
        "last_synced_hash": Value::Null,
        "last_synced_at": Value::Null,
        "local_dirty": false,
        "word_dirty": false,
        "symbols": 0,
        "has_content": false,
    })
}

pub fn list_documents() -> Result<Vec<Value>, String> {
    let (connection, _) = open()?;
    let mut statement = connection
        .prepare("SELECT project_id,stage_id FROM documents ORDER BY COALESCE(updated_at,''),id")
        .map_err(|error| error.to_string())?;
    let scopes = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    let mut result = Vec::new();
    for (project_id, stage_id) in scopes {
        if let Some(value) = document_row(&connection, &project_id, stage_id.as_deref())? {
            result.push(value);
        }
    }
    result.reverse();
    Ok(result)
}

pub fn get_document(scope: DocumentScope) -> Result<Value, String> {
    let (connection, _) = open()?;
    connection
        .execute_batch("BEGIN DEFERRED")
        .map_err(|e| e.to_string())?;
    let (_, payload) = validate_scope(&connection, &scope.project_id, scope.stage_id.as_deref())?;
    let mut document = document_row(&connection, &scope.project_id, scope.stage_id.as_deref())?
        .unwrap_or_else(|| {
            new_document_value(
                &scope.project_id,
                scope.stage_id.as_deref(),
                payload
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("Текст"),
            )
        });
    if !document["exists"].as_bool().unwrap_or(false) {
        document["document_id"] = serde_json::json!(document_id_for_scope(
            &connection,
            &scope.project_id,
            scope.stage_id.as_deref()
        )?);
    }
    document["expected_heads"] = crate::document_sync::expected(
        &connection,
        &scope.project_id,
        document["document_id"].as_str().unwrap(),
    )
    .map_err(|e| e.to_string())?;
    connection
        .execute_batch("COMMIT")
        .map_err(|e| e.to_string())?;
    Ok(document)
}

pub fn save_document(command: DocumentSaveCommand) -> Result<Value, String> {
    validate_content(&command.content)?;
    let (mut connection, _) = open()?;
    let (_, payload) = validate_scope(
        &connection,
        &command.project_id,
        command.stage_id.as_deref(),
    )?;
    let scope = scope_key(&command.project_id, command.stage_id.as_deref());
    let document_id = document_id_for_scope(
        &connection,
        &command.project_id,
        command.stage_id.as_deref(),
    )?;
    let timestamp = now();
    let title = payload
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("Текст");
    let tx = connection
        .transaction()
        .map_err(|error| error.to_string())?;
    let mut canonical = crate::document_sync::source(&tx, &command.project_id, &document_id)
        .map_err(|e| e.to_string())?;
    if canonical.is_null() {
        canonical = serde_json::json!({"id":document_id,"project_id":command.project_id,"stage_id":command.stage_id,"title":title,"content_json":command.content,"content_format":"tiptap-json/v1","created_at":crate::document_sync::timestamp(&timestamp).map_err(|e|e.to_string())?,"extensions":{}})
    } else {
        canonical["content_json"] = command.content.clone();
    }
    if let Some(created) = canonical["created_at"].as_str() {
        canonical["created_at"] =
            serde_json::json!(crate::document_sync::timestamp(created).map_err(|e| e.to_string())?);
    }
    if crate::document_sync::normal(
        &tx,
        &command.project_id,
        &document_id,
        canonical,
        command.expected_heads.as_ref(),
        &timestamp,
    )
    .map_err(|e| e.to_string())?
    {
        let document = document_row(&tx, &command.project_id, command.stage_id.as_deref())?
            .ok_or_else(|| "document_missing".to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        return Ok(document);
    }
    tx.execute(
        "INSERT INTO documents(id,scope_key,project_id,stage_id,title,content_json,content_format,created_at,updated_at,revision,extensions_json) VALUES(?1,?2,?3,?4,?5,?6,'tiptap-json/v1',?7,?7,0,'{}') ON CONFLICT(scope_key) DO UPDATE SET content_json=excluded.content_json,updated_at=excluded.updated_at,revision=documents.revision+1,title=excluded.title",
        params![document_id, scope, command.project_id, command.stage_id, title, command.content.to_string(), timestamp],
    ).map_err(|error| error.to_string())?;
    tx.execute(
        "UPDATE document_bindings SET sync_state='local_changed',expected_external_hash=NULL WHERE document_id=?1",
        [&document_id],
    ).map_err(|error| error.to_string())?;
    let document = document_row(&tx, &command.project_id, command.stage_id.as_deref())?
        .ok_or_else(|| "Документ не сохранён.".to_string())?;
    tx.commit().map_err(|error| error.to_string())?;
    Ok(document)
}

pub fn record_document_progress(command: DocumentProgressCommand) -> Result<Value, String> {
    let (mut connection, _) = open()?;
    let (_, payload) = validate_scope(
        &connection,
        &command.project_id,
        command.stage_id.as_deref(),
    )?;
    if matches!(payload["status"].as_str(), Some("в архиве" | "завершен")) {
        return Err("Архивные и завершённые сущности доступны только для просмотра.".to_string());
    }
    let document = if let Some(content) = command.content {
        save_document(DocumentSaveCommand {
            project_id: command.project_id.clone(),
            stage_id: command.stage_id.clone(),
            content,
            expected_heads: command.expected_heads,
        })?
    } else {
        document_row(
            &connection,
            &command.project_id,
            command.stage_id.as_deref(),
        )?
        .ok_or_else(|| "Сначала добавьте текст в документ.".to_string())?
    };
    let symbol_count = document.get("symbols").and_then(Value::as_u64).unwrap_or(0);
    let total = normalize_total(
        symbol_count as f64,
        payload
            .get("unit")
            .and_then(Value::as_str)
            .unwrap_or("symbols"),
    )?;
    let previous = payload.get("total").and_then(Value::as_f64).unwrap_or(0.0);
    if (total - previous).abs() < 0.009 {
        return Ok(
            serde_json::json!({"changed":false,"symbols":symbol_count,"progress":null,"document":document}),
        );
    }
    let document_id = document
        .get("document_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "У документа отсутствует идентификатор.".to_string())?;
    let revision = document
        .get("revision")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let source_key = format!("local-document:{document_id}:{revision}");
    let (project, entry, feedback) = record_document_or_sync_progress(
        &mut connection,
        &command.project_id,
        command.stage_id.as_deref(),
        total,
        &source_key,
        &now(),
        None,
        Some(revision),
        None,
    )?;
    Ok(serde_json::json!({
        "changed": !entry.is_null(),
        "symbols": symbol_count,
        "progress": (!entry.is_null()).then(|| {
            let mut progress = progress_result(project, entry);
            match feedback {
                Ok(Some(game)) => progress["game"] = serde_json::json!(game),
                Err(warning) => progress["warning"] = serde_json::json!(warning),
                Ok(None) => {}
            }
            progress
        }),
        "document": document,
    }))
}

pub fn rename_document(command: DocumentRenameCommand) -> Result<Value, String> {
    if command.title.trim().is_empty() {
        return Err("Название документа не может быть пустым.".to_string());
    }
    let (mut connection, _) = open()?;
    validate_scope(
        &connection,
        &command.project_id,
        command.stage_id.as_deref(),
    )?;
    let document_id = document_id_for_scope(
        &connection,
        &command.project_id,
        command.stage_id.as_deref(),
    )?;
    let tx = connection.transaction().map_err(|e| e.to_string())?;
    let mut canonical = crate::document_sync::source(&tx, &command.project_id, &document_id)
        .map_err(|e| e.to_string())?;
    if !canonical.is_null() {
        canonical["title"] = serde_json::json!(command.title.trim());
    }
    if crate::document_sync::normal(
        &tx,
        &command.project_id,
        &document_id,
        canonical,
        command.expected_heads.as_ref(),
        &now(),
    )
    .map_err(|e| e.to_string())?
    {
        tx.commit().map_err(|e| e.to_string())?;
        return document_row(
            &connection,
            &command.project_id,
            command.stage_id.as_deref(),
        )?
        .ok_or_else(|| "document_missing".into());
    }
    tx.commit().map_err(|e| e.to_string())?;
    let changed = connection
        .execute(
            "UPDATE documents SET title=?,updated_at=?,revision=revision+1 WHERE scope_key=?",
            params![
                command.title.trim(),
                now(),
                scope_key(&command.project_id, command.stage_id.as_deref())
            ],
        )
        .map_err(|error| error.to_string())?;
    if changed == 0 {
        return Err("Документ не найден.".to_string());
    }
    document_row(
        &connection,
        &command.project_id,
        command.stage_id.as_deref(),
    )?
    .ok_or_else(|| "Документ не найден.".to_string())
}

pub fn delete_document(scope: DocumentScope) -> Result<(), String> {
    let (mut connection, _) = open()?;
    validate_scope(&connection, &scope.project_id, scope.stage_id.as_deref())?;
    let id = document_id_for_scope(&connection, &scope.project_id, scope.stage_id.as_deref())?;
    let tx = connection.transaction().map_err(|e| e.to_string())?;
    if crate::document_sync::normal(
        &tx,
        &scope.project_id,
        &id,
        Value::Null,
        scope.expected_heads.as_ref(),
        &now(),
    )
    .map_err(|e| e.to_string())?
    {
        tx.commit().map_err(|e| e.to_string())?;
        return Ok(());
    }
    tx.commit().map_err(|e| e.to_string())?;
    let key = scope_key(&scope.project_id, scope.stage_id.as_deref());
    connection.execute("DELETE FROM document_bindings WHERE document_id IN (SELECT id FROM documents WHERE scope_key=?)", [&key]).map_err(|error| error.to_string())?;
    connection
        .execute("DELETE FROM documents WHERE scope_key=?", [&key])
        .map_err(|error| error.to_string())?;
    Ok(())
}

pub fn move_project_document_to_stage(project_id: &str, stage_id: &str) -> Result<(), String> {
    let mut connection = open()?.0;
    let source_id = document_id_for_scope(&connection, project_id, None)?;
    let source: Option<(String, String, Option<String>)> = connection
        .query_row(
            "SELECT documents.id,documents.content_json,document_bindings.external_path FROM documents LEFT JOIN document_bindings ON document_bindings.document_id=documents.id WHERE documents.scope_key=?1",
            [scope_key(project_id, None)],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(|error| error.to_string())?;
    let Some((source_document_id, content, source_path)) = source else {
        return Ok(());
    };
    if source_document_id != source_id
        || symbols(&parse_content(content)?) == 0 && source_path.is_none()
    {
        return Ok(());
    }
    if connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM cloud_document_migrations WHERE entity_id=?)",
            [&source_document_id],
            |r| r.get::<_, bool>(0),
        )
        .map_err(|e| e.to_string())?
    {
        return Ok(());
    }
    let target_scope = scope_key(project_id, Some(stage_id));
    let target: Option<(String, String, Option<String>)> = connection
        .query_row(
            "SELECT documents.id,documents.content_json,document_bindings.external_path FROM documents LEFT JOIN document_bindings ON document_bindings.document_id=documents.id WHERE documents.scope_key=?1",
            [target_scope.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(|error| error.to_string())?;
    let tx = connection
        .transaction()
        .map_err(|error| error.to_string())?;
    if target.is_some() {
        return Err("document_scope_occupied".into());
    }
    tx.execute(
        "UPDATE documents SET scope_key=?,stage_id=? WHERE id=?",
        params![target_scope, stage_id, source_document_id],
    )
    .map_err(|error| error.to_string())?;
    tx.commit().map_err(|error| error.to_string())
}

fn require_existing_connected_binding_document(
    db: &rusqlite::Connection,
    p: &str,
    id: &str,
) -> Result<(), String> {
    let connected: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM cloud_document_project_consent WHERE project_id=?1)",
            [p],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    let exists: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM documents WHERE id=?1)",
            [id],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if connected && !exists {
        return Err("document_binding_requires_saved_document".into());
    }
    Ok(())
}

pub fn bind_document_file(command: DocumentFileCommand) -> Result<Value, String> {
    let path = canonical_existing_file(&command.path, "Word")?;
    if path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.eq_ignore_ascii_case("docx"))
        != Some(true)
    {
        return Err("Поддерживаются только документы Word .docx.".to_string());
    }
    let (mut connection, _) = open()?;
    let (_, payload) = validate_scope(
        &connection,
        &command.project_id,
        command.stage_id.as_deref(),
    )?;
    let scope = scope_key(&command.project_id, command.stage_id.as_deref());
    let document_id = document_id_for_scope(
        &connection,
        &command.project_id,
        command.stage_id.as_deref(),
    )?;
    require_existing_connected_binding_document(&connection, &command.project_id, &document_id)?;
    let timestamp = now();
    let tx = connection
        .transaction()
        .map_err(|error| error.to_string())?;
    tx.execute(
        "INSERT INTO documents(id,scope_key,project_id,stage_id,title,content_json,content_format,created_at,updated_at,revision,extensions_json) SELECT ?1,?2,?3,?4,?5,?6,'tiptap-json/v1',?7,?7,0,'{}' WHERE NOT EXISTS(SELECT 1 FROM documents WHERE id=?1)",
        params![document_id, scope, command.project_id, command.stage_id, payload.get("name").and_then(Value::as_str).unwrap_or("Текст"), EMPTY_DOCUMENT, timestamp],
    ).map_err(|error| error.to_string())?;
    tx.execute(
        "INSERT INTO document_bindings(id,document_id,binding_type,external_path,source_id,last_synced_revision,sync_state,payload_json) VALUES(?1,?2,'word',?3,NULL,0,'local_changed','{}') ON CONFLICT(document_id) DO UPDATE SET external_path=excluded.external_path,binding_type='word',sync_state='local_changed'",
        params![binding_id(&document_id), document_id, path.to_string_lossy()],
    ).map_err(|error| error.to_string())?;
    tx.commit().map_err(|error| error.to_string())?;
    external_reattach::compare(
        &mut connection,
        &command.project_id,
        command.stage_id.as_deref(),
        &path,
    )
}

pub fn write_document_word(command: DocumentWordWriteCommand) -> Result<Value, String> {
    let bytes = decode_base64(&command.content_base64)?;
    let (mut connection, _) = open()?;
    validate_scope(
        &connection,
        &command.project_id,
        command.stage_id.as_deref(),
    )?;
    let document_id = document_id_for_scope(
        &connection,
        &command.project_id,
        command.stage_id.as_deref(),
    )?;
    let path: String = connection
        .query_row(
            "SELECT external_path FROM document_bindings WHERE document_id=?",
            [document_id.as_str()],
            |row| row.get(0),
        )
        .map_err(|_| "Сначала свяжите документ с файлом Word.".to_string())?;
    let target = canonical_existing_or_parent(&path)?;
    ensure_external_write_safe(&mut connection, &document_id, &target)?;
    atomic_write(&target, &bytes)?;
    let digest = sha256(&bytes);
    connection.execute(
        "UPDATE document_bindings SET last_external_hash=?,last_synced_hash=?,last_synced_revision=(SELECT revision FROM documents WHERE id=?),last_synced_at=?,sync_state='synced',expected_external_hash=? WHERE document_id=?",
        params![digest, digest, document_id, now(), digest, document_id],
    ).map_err(|error| error.to_string())?;
    document_row(
        &connection,
        &command.project_id,
        command.stage_id.as_deref(),
    )?
    .ok_or_else(|| "Документ не найден.".to_string())
}

pub fn read_external(scope: DocumentScope) -> Result<ExternalDocumentResult, String> {
    let (mut connection, _) = open()?;
    validate_scope(&connection, &scope.project_id, scope.stage_id.as_deref())?;
    let document_id =
        document_id_for_scope(&connection, &scope.project_id, scope.stage_id.as_deref())?;
    let row = connection.query_row(
        "SELECT binding_type,external_path,last_external_hash,last_synced_hash,last_synced_revision FROM document_bindings WHERE document_id=?",
        [document_id.as_str()],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, Option<String>>(2)?, row.get::<_, Option<String>>(3)?, row.get::<_, i64>(4)?)),
    ).optional().map_err(|error| error.to_string())?;
    let Some((binding_type, path, _last_external_hash, last_synced_hash, last_revision)) = row
    else {
        return Ok(ExternalDocumentResult {
            state: "unlinked".to_string(),
            content_base64: None,
            hash: None,
        });
    };
    if binding_type != "word" {
        return Ok(ExternalDocumentResult {
            state: "unsupported_external".to_string(),
            content_base64: None,
            hash: None,
        });
    }
    let source = match read_stable_source(Path::new(&path)) {
        Ok(source) => source,
        Err(_error) if !Path::new(&path).is_file() => {
            connection
                .execute(
                    "UPDATE document_bindings SET sync_state='missing_external' WHERE document_id=?",
                    [document_id.as_str()],
                )
                .map_err(|update_error| update_error.to_string())?;
            return Ok(ExternalDocumentResult {
                state: "missing_external".to_string(),
                content_base64: None,
                hash: None,
            });
        }
        Err(error) => return Err(error),
    };
    let current_revision: i64 = connection
        .query_row(
            "SELECT revision FROM documents WHERE id=?",
            [document_id.as_str()],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    let external_changed = last_synced_hash.as_deref() != Some(source.hash.as_str());
    let internal_changed = current_revision != last_revision;
    let state = if !external_changed {
        if internal_changed {
            "local_changed"
        } else {
            "synced"
        }
    } else if internal_changed {
        "conflict"
    } else {
        "external_changed"
    };
    connection
        .execute(
            "UPDATE document_bindings SET sync_state=?,last_external_hash=? WHERE document_id=?",
            params![state, source.hash, document_id],
        )
        .map_err(|error| error.to_string())?;
    if !external_changed {
        return Ok(ExternalDocumentResult {
            state: state.to_string(),
            content_base64: None,
            hash: None,
        });
    }
    Ok(ExternalDocumentResult {
        state: state.to_string(),
        content_base64: Some(base64_encode(&source.bytes)),
        hash: Some(source.hash),
    })
}

pub fn accept_external(command: DocumentExternalAcceptCommand) -> Result<Value, String> {
    validate_content(&command.content)?;
    let (mut connection, _) = open()?;
    validate_scope(
        &connection,
        &command.project_id,
        command.stage_id.as_deref(),
    )?;
    let document_id = document_id_for_scope(
        &connection,
        &command.project_id,
        command.stage_id.as_deref(),
    )?;
    let connected: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM cloud_document_migrations WHERE project_id=?1 AND entity_id=?2 UNION SELECT 1 FROM cloud_document_project_consent WHERE project_id=?1)", params![command.project_id, document_id], |r|r.get(0)).map_err(|e|e.to_string())?;
    if connected {
        let binding: Option<(String, String)> = connection
            .query_row(
                "SELECT binding_type,external_path FROM document_bindings WHERE document_id=?1",
                [&document_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        let Some((kind, path)) = binding else {
            return Err("document_external_proposal_stale".into());
        };
        if kind != "word" {
            return Err("document_external_proposal_stale".into());
        }
        let fresh = read_stable_source(Path::new(&path))?;
        if fresh.hash != command.source_hash || parse_docx(&fresh.bytes)?.0 != command.content {
            return Err("document_external_proposal_stale".into());
        }
    }
    let timestamp = now();
    let tx = connection.transaction().map_err(|e| e.to_string())?;
    let mut canonical = crate::document_sync::source(&tx, &command.project_id, &document_id)
        .map_err(|e| e.to_string())?;
    if !canonical.is_null() {
        canonical["content_json"] = command.content.clone();
    }
    if crate::document_sync::normal(
        &tx,
        &command.project_id,
        &document_id,
        canonical,
        command.expected_heads.as_ref(),
        &timestamp,
    )
    .map_err(|e| e.to_string())?
    {
        external_reattach::import_pending(
            &tx,
            &document_id,
            command
                .expected_heads
                .as_ref()
                .ok_or("document_stale_heads")?,
            &command.content,
            &command.source_hash,
        )?;
        tx.commit().map_err(|e| e.to_string())?;
        return document_row(
            &connection,
            &command.project_id,
            command.stage_id.as_deref(),
        )?
        .ok_or_else(|| "document_missing".into());
    }
    tx.commit().map_err(|e| e.to_string())?;
    connection
        .execute(
            "UPDATE documents SET content_json=?,updated_at=?,revision=revision+1 WHERE id=?",
            params![command.content.to_string(), timestamp, document_id],
        )
        .map_err(|error| error.to_string())?;
    connection.execute("UPDATE document_bindings SET last_external_hash=?,last_synced_hash=?,last_synced_revision=(SELECT revision FROM documents WHERE id=?),last_synced_at=?,sync_state='synced',expected_external_hash=? WHERE document_id=?", params![command.source_hash, command.source_hash, document_id, timestamp, command.source_hash, document_id]).map_err(|error| error.to_string())?;
    document_row(
        &connection,
        &command.project_id,
        command.stage_id.as_deref(),
    )?
    .ok_or_else(|| "Документ не найден.".to_string())
}

pub fn count_word(command: WordImportCommand) -> Result<usize, String> {
    validate_docx_filename(command.filename.as_deref())?;
    Ok(parse_docx(&command.bytes)?.1)
}

pub fn parse_word(command: WordImportCommand) -> Result<WordImportResult, String> {
    validate_docx_filename(command.filename.as_deref())?;
    let (content, symbols) = parse_docx(&command.bytes)?;
    Ok(WordImportResult {
        content,
        symbols,
        hash: sha256(&command.bytes),
        changed: false,
        project: None,
        progress: None,
    })
}

pub fn import_word(command: WordImportCommand) -> Result<WordImportResult, String> {
    validate_docx_filename(command.filename.as_deref())?;
    let (content, symbols) = parse_docx(&command.bytes)?;
    let hash = sha256(&command.bytes);
    let (project, progress, changed) = if let Some(project_id) = command.project_id.as_deref() {
        let mut connection = open()?.0;
        let (_, payload, _) = project_entity(&connection, project_id, command.stage_id.as_deref())?;
        let unit = payload
            .get("unit")
            .and_then(Value::as_str)
            .unwrap_or("symbols");
        let total = normalize_total(symbols as f64, unit)?;
        let previous = payload.get("total").and_then(Value::as_f64).unwrap_or(0.0);
        if (total - previous).abs() >= 0.009 {
            let (project, entry) = record_sync_progress(
                &mut connection,
                project_id,
                command.stage_id.as_deref(),
                total,
                &hash,
                &now(),
                None,
            )?;
            (
                Some(project.clone()),
                Some(progress_result(project, entry)),
                true,
            )
        } else {
            (
                Some(crate::project_payload(&mut connection, project_id)?),
                None,
                false,
            )
        }
    } else {
        (None, None, false)
    };
    Ok(WordImportResult {
        content,
        symbols,
        hash,
        changed,
        project,
        progress,
    })
}

fn validate_docx_filename(filename: Option<&str>) -> Result<(), String> {
    if filename.is_some_and(|value| {
        Path::new(value)
            .extension()
            .and_then(|extension| extension.to_str())
            .is_none_or(|extension| !extension.eq_ignore_ascii_case("docx"))
    }) {
        return Err("Поддерживаются только документы Word .docx.".to_string());
    }
    Ok(())
}

pub fn export_word(command: WordExportCommand) -> Result<(), String> {
    validate_content(&command.content)?;
    let path = canonical_existing_or_parent(&command.target_path)?;
    atomic_write(&path, &build_docx(&command.content)?)
}

pub fn write_document_word_content(
    project_id: String,
    stage_id: Option<String>,
    content: Value,
) -> Result<Value, String> {
    validate_content(&content)?;
    let bytes = build_docx(&content)?;
    let (mut connection, _) = open()?;
    validate_scope(&connection, &project_id, stage_id.as_deref())?;
    let document_id = document_id_for_scope(&connection, &project_id, stage_id.as_deref())?;
    let path: String = connection
        .query_row(
            "SELECT external_path FROM document_bindings WHERE document_id=?",
            [document_id.as_str()],
            |row| row.get(0),
        )
        .map_err(|_| "Сначала свяжите документ с файлом Word.".to_string())?;
    let target = canonical_existing_or_parent(&path)?;
    ensure_external_write_safe(&mut connection, &document_id, &target)?;
    atomic_write(&target, &bytes)?;
    let digest = sha256(&bytes);
    connection
        .execute(
            "UPDATE document_bindings SET last_external_hash=?,last_synced_hash=?,last_synced_revision=(SELECT revision FROM documents WHERE id=?),last_synced_at=?,sync_state='synced',expected_external_hash=? WHERE document_id=?",
            params![digest, digest, document_id, now(), digest, document_id],
        )
        .map_err(|error| error.to_string())?;
    document_row(&connection, &project_id, stage_id.as_deref())?
        .ok_or_else(|| "Документ не найден.".to_string())
}

pub fn inspect_scrivener(command: ScrivenerInspectCommand) -> Result<Vec<ScrivenerItem>, String> {
    let root = canonical_existing_directory(&command.path)?;
    let xml = find_scrivener_xml(&root)?;
    parse_scrivener_xml(&xml)
}

pub fn configure_scrivener_binding(
    project_id: &str,
    stage_id: Option<&str>,
    path: &str,
    item_id: &str,
) -> Result<Value, String> {
    let root = canonical_existing_directory(path)?;
    let xml = find_scrivener_xml(&root)?;
    let items = parse_scrivener_xml(&xml)?;
    if !contains_item(&items, item_id) {
        return Err("Документ Scrivener не найден в проекте.".to_string());
    }
    let (mut connection, _) = open()?;
    let (_, payload) = validate_scope(&connection, project_id, stage_id)?;
    let scope = scope_key(project_id, stage_id);
    let document_id = document_id_for_scope(&connection, project_id, stage_id)?;
    require_existing_connected_binding_document(&connection, project_id, &document_id)?;
    let timestamp = now();
    let tx = connection
        .transaction()
        .map_err(|error| error.to_string())?;
    tx.execute("INSERT INTO documents(id,scope_key,project_id,stage_id,title,content_json,content_format,created_at,updated_at,revision,extensions_json) VALUES(?1,?2,?3,?4,?5,?6,'tiptap-json/v1',?7,?7,0,'{}') ON CONFLICT(scope_key) DO NOTHING", params![document_id,scope,project_id,stage_id,payload.get("name").and_then(Value::as_str).unwrap_or("Текст"),EMPTY_DOCUMENT,timestamp]).map_err(|error| error.to_string())?;
    tx.execute("INSERT INTO document_bindings(id,document_id,binding_type,external_path,source_id,last_synced_revision,sync_state,payload_json) VALUES(?1,?2,'scrivener',?3,?4,0,'external_changed',?5) ON CONFLICT(document_id) DO UPDATE SET external_path=excluded.external_path,source_id=excluded.source_id,binding_type='scrivener',sync_state='external_changed',payload_json=excluded.payload_json", params![binding_id(&document_id),document_id,root.to_string_lossy(),item_id,serde_json::json!({"type":"scrivener","path":root,"item_id":item_id}).to_string()]).map_err(|error| error.to_string())?;
    tx.commit().map_err(|error| error.to_string())?;
    document_row(&connection, project_id, stage_id)?
        .ok_or_else(|| "Документ не найден.".to_string())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SyncConfigureCommand {
    pub project_id: String,
    #[serde(default)]
    pub stage_id: Option<String>,
    #[serde(rename = "type")]
    pub sync_type: String,
    pub path: String,
    #[serde(default)]
    pub item_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SyncScopeCommand {
    pub project_id: String,
    #[serde(default)]
    pub stage_id: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct SyncSummary {
    pub project_id: String,
    pub stage_id: Option<String>,
    pub configured: bool,
    #[serde(rename = "type")]
    pub sync_type: Option<String>,
    pub path: Option<String>,
    pub item_id: Option<String>,
    pub last_synced_at: Option<String>,
    pub desktop_only: bool,
    pub proposal_pending: bool,
}

#[derive(Debug, Serialize)]
pub struct SyncRunResult {
    pub changed: bool,
    pub symbols: usize,
    pub sync: SyncSummary,
    pub progress: Option<Value>,
}

#[derive(Debug, Serialize)]
pub struct SyncBatchItem {
    pub project_id: String,
    pub stage_id: Option<String>,
    pub ok: bool,
    pub changed: bool,
    pub symbols: Option<usize>,
    pub progress: Option<Value>,
    pub error: Option<Value>,
}

#[derive(Debug, Serialize)]
pub struct SyncBatchResult {
    pub checked: usize,
    pub changed: usize,
    pub failed: usize,
    pub items: Vec<SyncBatchItem>,
}

fn validate_sync_scope(
    connection: &rusqlite::Connection,
    project_id: &str,
    stage_id: Option<&str>,
) -> Result<(String, Value), String> {
    let (id, payload, _) = project_entity(connection, project_id, stage_id)?;
    if stage_id.is_none()
        && payload
            .get("stages")
            .and_then(Value::as_array)
            .is_some_and(|stages| !stages.is_empty())
    {
        return Err("У проекта с этапами нет отдельного текста.".to_string());
    }
    if payload.get("status").and_then(Value::as_str) == Some("завершен") {
        return Err("Завершённая сущность доступна только для просмотра.".to_string());
    }
    Ok((id, payload))
}

type SyncBinding = (
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
);

fn binding_with_legacy_id(
    connection: &rusqlite::Connection,
    project_id: &str,
    stage_id: Option<&str>,
) -> Result<Option<(SyncBinding, Option<String>)>, String> {
    let read = |row: &rusqlite::Row<'_>| -> rusqlite::Result<SyncBinding> {
        Ok((
            row.get(0)?,
            row.get(1)?,
            row.get(2)?,
            row.get(3)?,
            row.get(4)?,
        ))
    };
    if external_reattach::connected(connection, project_id)? {
        return connection.query_row(
            "SELECT binding_type,external_path,source_id,content_hash,last_synced_at FROM project_bindings WHERE id=?1",
            [external_reattach::project_binding_id(project_id, stage_id)],
            read,
        ).optional().map(|binding| binding.map(|binding| (binding, None))).map_err(|e| e.to_string());
    }
    let document_id = document_id_for_scope(connection, project_id, stage_id)?;
    let document = connection.query_row(
        "SELECT binding_type,external_path,source_id,last_external_hash,last_synced_at FROM document_bindings WHERE document_id=?1",
        [document_id],
        read,
    ).optional().map_err(|e| e.to_string())?;
    if let Some(binding) = document {
        return Ok(Some((binding, None)));
    }
    // Migrated desktop profiles store progress sources independently of text
    // documents. Keep those sources usable without requiring reattachment.
    connection
        .query_row(
            "SELECT binding_type,external_path,source_id,content_hash,last_synced_at,id
         FROM project_bindings WHERE project_id=?1 AND stage_id IS ?2
         AND id NOT LIKE 'external-progress:%' AND external_path IS NOT NULL
         AND binding_type IN ('word','scrivener') ORDER BY id LIMIT 1",
            params![project_id, stage_id],
            |row| Ok((read(row)?, Some(row.get(5)?))),
        )
        .optional()
        .map_err(|e| e.to_string())
}

fn binding_for_scope(
    connection: &rusqlite::Connection,
    project_id: &str,
    stage_id: Option<&str>,
) -> Result<Option<SyncBinding>, String> {
    Ok(binding_with_legacy_id(connection, project_id, stage_id)?.map(|(binding, _)| binding))
}

fn sync_summary(
    connection: &rusqlite::Connection,
    project_id: &str,
    stage_id: Option<&str>,
) -> Result<SyncSummary, String> {
    let binding = binding_for_scope(connection, project_id, stage_id)?;
    Ok(SyncSummary {
        project_id: project_id.to_string(),
        stage_id: stage_id.map(str::to_string),
        configured: binding.is_some(),
        sync_type: binding.as_ref().map(|value| value.0.clone()),
        path: binding.as_ref().map(|value| value.1.clone()),
        item_id: binding.as_ref().and_then(|value| value.2.clone()),
        last_synced_at: binding.and_then(|value| value.4),
        desktop_only: true,
        proposal_pending: connection.query_row("SELECT EXISTS(SELECT 1 FROM project_bindings WHERE project_id=?1 AND stage_id IS ?2 AND json_type(payload_json,'$.progress_reattach_v1') IS NOT NULL)",params![project_id,stage_id],|r|r.get(0)).map_err(|e|e.to_string())?,
    })
}

pub fn configure_sync(command: SyncConfigureCommand) -> Result<SyncSummary, String> {
    let mut connection = open()?.0;
    validate_sync_scope(
        &connection,
        &command.project_id,
        command.stage_id.as_deref(),
    )?;
    if external_reattach::connected(&connection, &command.project_id)? {
        return external_reattach::configure_project_source(&mut connection, command);
    }
    let document_id = document_id_for_scope(
        &connection,
        &command.project_id,
        command.stage_id.as_deref(),
    )?;
    let (path, source_id) = match command.sync_type.as_str() {
        "word" => {
            let path = canonical_existing_file(&command.path, "Word")?;
            if !path
                .extension()
                .and_then(|value| value.to_str())
                .is_some_and(|value| value.eq_ignore_ascii_case("docx"))
            {
                return Err("Поддерживаются только документы Word .docx.".to_string());
            }
            (path, None)
        }
        "scrivener" => {
            let root = canonical_existing_directory(&command.path)?;
            let items = parse_scrivener_xml(&find_scrivener_xml(&root)?)?;
            let item_id = command
                .item_id
                .as_deref()
                .ok_or_else(|| "Выберите документ Scrivener.".to_string())?;
            if !contains_item(&items, item_id) {
                return Err("Документ Scrivener не найден в проекте.".to_string());
            }
            (root, Some(item_id.to_string()))
        }
        _ => return Err("Неизвестный тип синхронизации.".to_string()),
    };
    let timestamp = now();
    let tx = connection
        .transaction()
        .map_err(|error| error.to_string())?;
    let document_id = if tx
        .query_row(
            "SELECT 1 FROM documents WHERE id=?1",
            [document_id.as_str()],
            |_| Ok(()),
        )
        .optional()
        .map_err(|error| error.to_string())?
        .is_some()
    {
        document_id
    } else {
        let scope = scope_key(&command.project_id, command.stage_id.as_deref());
        let (_, payload) =
            validate_sync_scope(&tx, &command.project_id, command.stage_id.as_deref())?;
        let title = payload
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("Текст");
        tx.execute(
            "INSERT INTO documents(id,scope_key,project_id,stage_id,title,content_json,content_format,created_at,updated_at,revision,extensions_json) VALUES(?1,?2,?3,?4,?5,?6,'tiptap-json/v1',NULL,NULL,0,'{}')",
            params![document_id, scope, command.project_id, command.stage_id, title, EMPTY_DOCUMENT],
        ).map_err(|error| error.to_string())?;
        document_id
    };
    tx.execute(
        "INSERT INTO document_bindings(id,document_id,binding_type,external_path,source_id,last_synced_revision,sync_state,payload_json) VALUES(?1,?2,?3,?4,?5,0,'external_changed',?6) ON CONFLICT(document_id) DO UPDATE SET binding_type=excluded.binding_type,external_path=excluded.external_path,source_id=excluded.source_id,sync_state='external_changed',payload_json=excluded.payload_json",
        params![binding_id(&document_id), document_id, command.sync_type, path.to_string_lossy(), source_id, serde_json::json!({"configured_at": timestamp}).to_string()],
    ).map_err(|error| error.to_string())?;
    let (entity_id, mut payload) =
        validate_sync_scope(&tx, &command.project_id, command.stage_id.as_deref())?;
    payload["work_method"] = Value::String("sync".to_string());
    payload["sync_available"] = Value::Bool(true);
    payload["synch"] =
        serde_json::json!({"type": command.sync_type, "path": path, "item_id": source_id});
    payload["last_synch"] = Value::Null;
    let table = if command.stage_id.is_some() {
        "stages"
    } else {
        "projects"
    };
    tx.execute(
        &format!("UPDATE {table} SET payload_json=?,updated_at=? WHERE id=?"),
        params![payload.to_string(), timestamp, entity_id],
    )
    .map_err(|error| error.to_string())?;
    remove_legacy_sync_binding(&tx, &command.project_id, command.stage_id.as_deref())?;
    tx.commit().map_err(|error| error.to_string())?;
    sync_summary(
        &connection,
        &command.project_id,
        command.stage_id.as_deref(),
    )
}

pub fn get_sync(command: SyncScopeCommand) -> Result<SyncSummary, String> {
    let connection = open()?.0;
    validate_sync_scope(
        &connection,
        &command.project_id,
        command.stage_id.as_deref(),
    )?;
    sync_summary(
        &connection,
        &command.project_id,
        command.stage_id.as_deref(),
    )
}

pub fn get_project_syncs(project_id: String) -> Result<Value, String> {
    let connection = open()?.0;
    let has_stages: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM stages WHERE project_id=?1)",
            [project_id.as_str()],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    let mut summaries = Vec::new();
    if has_stages {
        let mut statement = connection
            .prepare("SELECT id FROM stages WHERE project_id=?1 ORDER BY id")
            .map_err(|error| error.to_string())?;
        let stages = statement
            .query_map([project_id.as_str()], |row| row.get::<_, String>(0))
            .map_err(|error| error.to_string())?;
        let stage_ids = stages
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        drop(statement);
        for stage_id in stage_ids {
            summaries.push(
                serde_json::to_value(sync_summary(&connection, &project_id, Some(&stage_id))?)
                    .map_err(|error| error.to_string())?,
            );
        }
    } else {
        summaries.push(
            serde_json::to_value(sync_summary(&connection, &project_id, None)?)
                .map_err(|error| error.to_string())?,
        );
    }
    Ok(serde_json::json!({"project_id": project_id, "syncs": summaries}))
}

pub fn remove_sync(command: SyncScopeCommand) -> Result<SyncSummary, String> {
    let mut connection = open()?.0;
    validate_sync_scope(
        &connection,
        &command.project_id,
        command.stage_id.as_deref(),
    )?;
    if external_reattach::connected(&connection, &command.project_id)? {
        connection
            .execute(
                "DELETE FROM project_bindings WHERE id=?1",
                [external_reattach::project_binding_id(
                    &command.project_id,
                    command.stage_id.as_deref(),
                )],
            )
            .map_err(|e| e.to_string())?;
        return sync_summary(
            &connection,
            &command.project_id,
            command.stage_id.as_deref(),
        );
    }
    let document_id = document_id_for_scope(
        &connection,
        &command.project_id,
        command.stage_id.as_deref(),
    )?;
    let tx = connection.transaction().map_err(|e| e.to_string())?;
    tx.execute(
        "DELETE FROM document_bindings WHERE document_id=?",
        [document_id],
    )
    .map_err(|e| e.to_string())?;
    remove_legacy_sync_binding(&tx, &command.project_id, command.stage_id.as_deref())?;
    tx.commit().map_err(|e| e.to_string())?;
    sync_summary(
        &connection,
        &command.project_id,
        command.stage_id.as_deref(),
    )
}

fn remove_legacy_sync_binding(
    db: &rusqlite::Connection,
    project_id: &str,
    stage_id: Option<&str>,
) -> Result<(), String> {
    db.execute(
        "DELETE FROM project_bindings WHERE project_id=?1 AND stage_id IS ?2 AND id NOT LIKE 'external-progress:%' AND binding_type IN ('word','scrivener')",
        params![project_id, stage_id],
    ).map_err(|e| e.to_string())?;
    Ok(())
}

fn sync_source(
    binding_type: &str,
    path: &Path,
    source_id: Option<&str>,
) -> Result<(usize, String), String> {
    if binding_type == "word" {
        let source = read_stable_source(path)?;
        let symbols = parse_docx(&source.bytes)?.1;
        return Ok((symbols, source.hash));
    }
    if binding_type == "scrivener" {
        let item_id = source_id.ok_or_else(|| "Документ Scrivener не выбран.".to_string())?;
        let xml_path = find_scrivener_xml(path)?;
        let xml = read_stable_source(&xml_path)?;
        if !contains_item(&parse_scrivener_xml(&xml_path)?, item_id) {
            return Err("sync_source_stale".into());
        }
        let content = read_scrivener_item(path, item_id)?;
        let mut hash_input = xml.bytes;
        hash_input.extend_from_slice(&content);
        return Ok((count_rtf_symbols(&content), sha256(&hash_input)));
    }
    Err("Неизвестный тип синхронизации.".to_string())
}

pub fn run_sync(command: SyncScopeCommand) -> Result<SyncRunResult, String> {
    let mut connection = open()?.0;
    let (_, payload) = validate_sync_scope(
        &connection,
        &command.project_id,
        command.stage_id.as_deref(),
    )?;
    if payload.get("work_method").and_then(Value::as_str) != Some("sync") {
        return Err("Для синхронизации выберите метод «Синхронизация».".to_string());
    }
    let document_id = document_id_for_scope(
        &connection,
        &command.project_id,
        command.stage_id.as_deref(),
    )?;
    let (binding, legacy_id) = binding_with_legacy_id(
        &connection,
        &command.project_id,
        command.stage_id.as_deref(),
    )?
    .ok_or_else(|| "Синхронизация не настроена.".to_string())?;
    if external_reattach::connected(&connection, &command.project_id)? {
        return external_reattach::progress_proposal(
            &mut connection,
            &command.project_id,
            command.stage_id.as_deref(),
            false,
        );
    }
    if let Some(id) = legacy_id {
        return run_legacy_sync(&mut connection, command, &payload, binding, &id);
    }
    let (symbols, hash) = match sync_source(&binding.0, Path::new(&binding.1), binding.2.as_deref())
    {
        Ok(result) => result,
        Err(error) if !Path::new(&binding.1).exists() => {
            connection
                .execute(
                    "UPDATE document_bindings SET sync_state='missing_external' WHERE document_id=?",
                    [document_id.as_str()],
                )
                .map_err(|update_error| update_error.to_string())?;
            return Err(error);
        }
        Err(error) => return Err(error),
    };
    let previous_hash: Option<String> = connection
        .query_row(
            "SELECT last_external_hash FROM document_bindings WHERE document_id=?",
            [document_id.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| error.to_string())?
        .flatten();
    let (current_revision, last_synced_revision): (i64, i64) = connection
        .query_row(
            "SELECT d.revision,b.last_synced_revision FROM documents d JOIN document_bindings b ON b.document_id=d.id WHERE d.id=?",
            [document_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|error| error.to_string())?;
    let internal_changed = current_revision != last_synced_revision;
    if internal_changed
        && previous_hash.is_some()
        && previous_hash.as_deref() != Some(hash.as_str())
    {
        connection
            .execute(
                "UPDATE document_bindings SET sync_state='conflict' WHERE document_id=?",
                [document_id.as_str()],
            )
            .map_err(|error| error.to_string())?;
        return Err(
            "Внутренний документ и внешний файл изменены: сначала разрешите конфликт.".to_string(),
        );
    }
    let total = normalize_total(
        symbols as f64,
        payload
            .get("unit")
            .and_then(Value::as_str)
            .unwrap_or("symbols"),
    )?;
    let previous = payload.get("total").and_then(Value::as_f64).unwrap_or(0.0);
    let changed = (total - previous).abs() >= 0.009;
    let progress = if changed && previous_hash.as_deref() != Some(hash.as_str()) {
        let (_, entry) = record_sync_progress(
            &mut connection,
            &command.project_id,
            command.stage_id.as_deref(),
            total,
            &hash,
            &now(),
            None,
        )?;
        if internal_changed {
            connection
                .execute(
                    "UPDATE document_bindings SET sync_state='local_changed',last_synced_revision=? WHERE document_id=?",
                    params![last_synced_revision, document_id],
                )
                .map_err(|error| error.to_string())?;
        }
        Some(progress_result(
            crate::project_payload(&mut connection, &command.project_id)?,
            entry,
        ))
    } else {
        let state = if internal_changed {
            "local_changed"
        } else {
            "synced"
        };
        let synced_revision = if internal_changed {
            last_synced_revision
        } else {
            current_revision
        };
        connection.execute("UPDATE document_bindings SET last_external_hash=?,last_synced_hash=?,last_synced_revision=?,last_synced_at=?,sync_state=?,expected_external_hash=? WHERE document_id=?", params![hash,hash,synced_revision,now(),state,hash,document_id]).map_err(|error| error.to_string())?;
        None
    };
    Ok(SyncRunResult {
        changed: changed && progress.is_some(),
        symbols,
        sync: sync_summary(
            &connection,
            &command.project_id,
            command.stage_id.as_deref(),
        )?,
        progress,
    })
}

fn run_legacy_sync(
    db: &mut rusqlite::Connection,
    scope: SyncScopeCommand,
    entity: &Value,
    binding: SyncBinding,
    id: &str,
) -> Result<SyncRunResult, String> {
    let (kind, path, source_id, _, _) = binding;
    let (symbols, hash) = sync_source(&kind, Path::new(&path), source_id.as_deref())?;
    let total = normalize_total(symbols as f64, entity["unit"].as_str().unwrap_or("symbols"))?;
    let (project, entry, _) = record_document_or_sync_progress(
        db,
        &scope.project_id,
        scope.stage_id.as_deref(),
        total,
        &hash,
        &now(),
        None,
        None,
        Some(id),
    )?;
    Ok(SyncRunResult {
        changed: !entry.is_null(),
        symbols,
        sync: sync_summary(db, &scope.project_id, scope.stage_id.as_deref())?,
        progress: (!entry.is_null()).then(|| progress_result(project, entry)),
    })
}

fn update_legacy_sync_hash(db: &rusqlite::Connection, id: &str, hash: &str) -> Result<(), String> {
    let updated = db
        .execute(
            "UPDATE project_bindings SET content_hash=?1,last_synced_at=?2 WHERE id=?3",
            params![hash, now(), id],
        )
        .map_err(|e| e.to_string())?;
    if updated != 1 {
        return Err("sync_source_stale".into());
    }
    Ok(())
}

fn progress_result(project: Value, entry: Value) -> Value {
    serde_json::json!({
        "project": project,
        "entry": entry,
        "added_symbols": entry.get("added_symbols").and_then(Value::as_f64).unwrap_or(0.0),
        "game": Value::Null,
        "warning": Value::Null,
    })
}

fn run_all_sync_filtered(project_filter: Option<&str>) -> Result<SyncBatchResult, String> {
    let connection = open()?.0;
    let mut targets = Vec::new();
    let mut statement = connection.prepare("SELECT d.project_id,d.stage_id FROM documents d JOIN document_bindings b ON b.document_id=d.id JOIN projects p ON p.id=d.project_id LEFT JOIN stages s ON s.id=d.stage_id WHERE NOT EXISTS(SELECT 1 FROM cloud_sync_project_bindings cb WHERE cb.project_id=d.project_id) AND (?1 IS NULL OR d.project_id=?1) AND ((d.stage_id IS NULL AND json_extract(p.payload_json,'$.status')='активен' AND json_extract(p.payload_json,'$.work_method')='sync') OR (d.stage_id IS NOT NULL AND json_extract(s.payload_json,'$.status')='активен' AND json_extract(s.payload_json,'$.work_method')='sync')) UNION SELECT b.project_id,b.stage_id FROM project_bindings b JOIN projects p ON p.id=b.project_id LEFT JOIN stages s ON s.id=b.stage_id WHERE (b.id LIKE 'external-progress:%' OR (b.binding_type IN ('word','scrivener') AND b.external_path IS NOT NULL AND NOT EXISTS(SELECT 1 FROM cloud_sync_project_bindings cb WHERE cb.project_id=b.project_id))) AND (?1 IS NULL OR b.project_id=?1) AND ((b.stage_id IS NULL AND json_extract(p.payload_json,'$.status')='активен' AND json_extract(p.payload_json,'$.work_method')='sync') OR (b.stage_id IS NOT NULL AND json_extract(s.payload_json,'$.status')='активен' AND json_extract(s.payload_json,'$.work_method')='sync')) ORDER BY 1,2").map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([project_filter], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })
        .map_err(|error| error.to_string())?;
    for row in rows {
        targets.push(row.map_err(|error| error.to_string())?);
    }
    drop(statement);
    let mut items = Vec::new();
    let mut changed = 0;
    for (project_id, stage_id) in targets {
        match run_sync(SyncScopeCommand {
            project_id: project_id.clone(),
            stage_id: stage_id.clone(),
        }) {
            Ok(result) => {
                changed += usize::from(result.changed);
                items.push(SyncBatchItem {
                    project_id,
                    stage_id,
                    ok: true,
                    changed: result.changed,
                    symbols: Some(result.symbols),
                    progress: result.progress,
                    error: None,
                });
            }
            Err(error) => items.push(SyncBatchItem {
                project_id,
                stage_id,
                ok: false,
                changed: false,
                symbols: None,
                progress: None,
                error: Some(serde_json::json!({"code":"sync_failed","message":error})),
            }),
        }
    }
    Ok(SyncBatchResult {
        checked: items.len(),
        changed,
        failed: items.iter().filter(|item| !item.ok).count(),
        items,
    })
}

pub fn run_all_sync() -> Result<SyncBatchResult, String> {
    run_all_sync_filtered(None)
}

pub fn run_project_syncs(project_id: String) -> Result<SyncBatchResult, String> {
    run_all_sync_filtered(Some(&project_id))
}

pub(crate) fn migrate_legacy_documents(
    connection: &mut rusqlite::Connection,
    root: &Path,
) -> Result<(), String> {
    let marker: Option<String> = connection
        .query_row(
            "SELECT value_json FROM document_metadata WHERE key='documents_json_migration'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| error.to_string())?;
    if marker.is_some() {
        return Ok(());
    }
    let path = root.join("documents.json");
    if !path.exists() {
        connection.execute("INSERT OR REPLACE INTO document_metadata(key,value_json) VALUES('documents_json_migration','{\"status\":\"complete\",\"source\":null}')", []).map_err(|error| error.to_string())?;
        return Ok(());
    }
    let metadata = fs::metadata(&path).map_err(|error| error.to_string())?;
    if metadata.len() > 100 * 1024 * 1024 {
        return Err("documents.json превышает допустимый размер 100 МБ.".to_string());
    }
    let bytes = fs::read(&path).map_err(|error| error.to_string())?;
    let raw: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("documents.json повреждён: {error}"))?;
    let records = raw
        .as_object()
        .ok_or_else(|| "documents.json должен содержать объект документов.".to_string())?;
    let tx = connection
        .transaction()
        .map_err(|error| error.to_string())?;
    for (source_key, value) in records {
        let Some(record) = value.as_object() else {
            tx.execute("INSERT OR REPLACE INTO document_migration_orphans(source_key,payload_json,reason) VALUES(?1,?2,'record is not an object')", params![source_key,value.to_string()]).map_err(|error| error.to_string())?;
            continue;
        };
        let project_id = record
            .get("project_id")
            .and_then(Value::as_str)
            .or_else(|| source_key.strip_suffix(":project"));
        let stage_id = record.get("stage_id").and_then(Value::as_str);
        let Some(project_id) = project_id.filter(|value| !value.is_empty()) else {
            tx.execute("INSERT OR REPLACE INTO document_migration_orphans(source_key,payload_json,reason) VALUES(?1,?2,'missing project_id')", params![source_key,Value::Object(record.clone()).to_string()]).map_err(|error| error.to_string())?;
            continue;
        };
        let valid = if let Some(stage_id) = stage_id {
            tx.query_row(
                "SELECT 1 FROM stages WHERE id=?1 AND project_id=?2",
                params![stage_id, project_id],
                |_| Ok(()),
            )
            .optional()
            .map_err(|error| error.to_string())?
            .is_some()
        } else {
            tx.query_row("SELECT 1 FROM projects WHERE id=?1", [project_id], |_| {
                Ok(())
            })
            .optional()
            .map_err(|error| error.to_string())?
            .is_some()
        };
        if !valid {
            tx.execute("INSERT OR REPLACE INTO document_migration_orphans(source_key,payload_json,reason) VALUES(?1,?2,'project or stage does not exist')", params![source_key,Value::Object(record.clone()).to_string()]).map_err(|error| error.to_string())?;
            continue;
        }
        let scope = scope_key(project_id, stage_id);
        let id = record
            .get("document_id")
            .and_then(Value::as_str)
            .or_else(|| record.get("id").and_then(Value::as_str))
            .filter(|id| !id.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| stable_id(&scope));
        let content = record
            .get("content")
            .cloned()
            .unwrap_or_else(|| serde_json::from_str(EMPTY_DOCUMENT).unwrap());
        if validate_content(&content).is_err() {
            tx.execute("INSERT OR REPLACE INTO document_migration_orphans(source_key,payload_json,reason) VALUES(?1,?2,'invalid Tiptap content')", params![source_key,Value::Object(record.clone()).to_string()]).map_err(|error| error.to_string())?;
            continue;
        }
        let known = [
            "document_id",
            "id",
            "project_id",
            "stage_id",
            "title",
            "content",
            "content_format",
            "created_at",
            "updated_at",
            "exists",
            "docx_path",
            "sync_state",
            "last_synced_hash",
            "last_synced_at",
            "local_dirty",
            "word_dirty",
            "symbols",
            "has_content",
        ];
        let extensions: Map<String, Value> = record
            .iter()
            .filter(|(key, _)| !known.contains(&key.as_str()))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        let title = record
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("Текст");
        tx.execute("INSERT OR IGNORE INTO documents(id,scope_key,project_id,stage_id,title,content_json,content_format,created_at,updated_at,revision,extensions_json) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,0,?10)", params![id,scope,project_id,stage_id,title,content.to_string(),record.get("content_format").and_then(Value::as_str).unwrap_or("tiptap-json/v1"),record.get("created_at").and_then(Value::as_str),record.get("updated_at").and_then(Value::as_str),Value::Object(extensions).to_string()]).map_err(|error| error.to_string())?;
        if let Some(external_path) = record.get("docx_path").and_then(Value::as_str) {
            tx.execute("INSERT OR IGNORE INTO document_bindings(id,document_id,binding_type,external_path,source_id,last_external_hash,last_synced_at,sync_state,payload_json) VALUES(?1,?2,'word',?3,NULL,?4,?5,?6,?7)", params![binding_id(&id),id,external_path,record.get("last_synced_hash").and_then(Value::as_str),record.get("last_synced_at").and_then(Value::as_str),record.get("sync_state").and_then(Value::as_str).unwrap_or("unlinked"),serde_json::json!({"legacy_source_key":source_key}).to_string()]).map_err(|error| error.to_string())?;
        }
    }
    let marker = serde_json::json!({"status":"complete","source_checksum":format!("sha256:{}",sha256(&bytes))}).to_string();
    tx.execute("INSERT OR REPLACE INTO document_metadata(key,value_json) VALUES('documents_json_migration',?1)", [marker]).map_err(|error| error.to_string())?;
    tx.commit().map_err(|error| error.to_string())
}

fn canonical_existing_file(path: &str, kind: &str) -> Result<PathBuf, String> {
    let value = expand_user(Path::new(path));
    if !value.is_file() {
        return Err(format!("Документ {kind} не найден."));
    }
    fs::canonicalize(value).map_err(|error| format!("Не удалось открыть файл {kind}: {error}"))
}

fn canonical_existing_directory(path: &str) -> Result<PathBuf, String> {
    let value = expand_user(Path::new(path));
    if !value.is_dir() {
        return Err("Проект Scrivener не найден.".to_string());
    }
    fs::canonicalize(value).map_err(|error| format!("Не удалось открыть проект Scrivener: {error}"))
}

fn canonical_existing_or_parent(path: &str) -> Result<PathBuf, String> {
    let value = PathBuf::from(path);
    let parent = value
        .parent()
        .ok_or_else(|| "Некорректный путь к файлу.".to_string())?;
    if !parent.is_dir() {
        return Err("Папка назначения не найдена.".to_string());
    }
    if value.exists() {
        fs::canonicalize(value).map_err(|error| error.to_string())
    } else {
        let parent = fs::canonicalize(parent).map_err(|error| error.to_string())?;
        Ok(parent.join(
            value
                .file_name()
                .ok_or_else(|| "Некорректное имя файла.".to_string())?,
        ))
    }
}

fn expand_user(path: &Path) -> PathBuf {
    if path.to_string_lossy().starts_with("~/") {
        if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
            return PathBuf::from(home).join(path.strip_prefix("~/").unwrap_or(path));
        }
    }
    path.to_path_buf()
}

fn read_stable_source(path: &Path) -> Result<SourceSnapshot, String> {
    let before = fs::metadata(path).map_err(|_| "Источник синхронизации не найден.".to_string())?;
    if before.len() > MAX_DOCX_BYTES as u64 {
        return Err("Источник синхронизации слишком велик.".to_string());
    }
    let bytes =
        fs::read(path).map_err(|_| "Не удалось прочитать источник синхронизации.".to_string())?;
    let after = fs::metadata(path)
        .map_err(|_| "Источник синхронизации исчез во время чтения.".to_string())?;
    if before.len() != after.len() || before.modified().ok() != after.modified().ok() {
        return Err("Источник синхронизации изменился во время чтения.".to_string());
    }
    let modified_at = before
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map_or_else(String::new, |value| value.as_secs().to_string());
    let hash = sha256(&bytes);
    Ok(SourceSnapshot {
        bytes,
        hash,
        modified_at,
    })
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    atomic_write_guarded(path, bytes, None)
}
fn atomic_write_guarded(
    path: &Path,
    bytes: &[u8],
    expected_hash: Option<&str>,
) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "Некорректный путь к файлу.".to_string())?;
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| "Некорректное имя файла.".to_string())?;
    let temporary = parent.join(format!(".nfprogress-{name}.tmp-{}", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&temporary)
        .map_err(|error| format!("Не удалось записать файл: {error}"))?;
    file.write_all(bytes).map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())?;
    drop(file);
    if let Some(expected) = expected_hash {
        if read_stable_source(path).map(|s| s.hash).ok().as_deref() != Some(expected) {
            let _ = fs::remove_file(&temporary);
            return Err("external_hash_stale".into());
        }
    }
    fs::rename(&temporary, path).map_err(|error| {
        let _ = fs::remove_file(&temporary);
        format!("Не удалось заменить файл: {error}")
    })
}

fn ensure_external_write_safe(
    connection: &mut rusqlite::Connection,
    document_id: &str,
    path: &Path,
) -> Result<(), String> {
    let raw: String = connection
        .query_row(
            "SELECT payload_json FROM document_bindings WHERE document_id=?1",
            [document_id],
            |r| r.get(0),
        )
        .map_err(|_| "external_reattach_required")?;
    let local: Value = serde_json::from_str(&raw).map_err(|_| "external_revalidation_required")?;
    if local.get("reattach_v1").is_some() {
        return Err("external_explicit_decision_required".into());
    }
    let connected: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM cloud_document_migrations WHERE entity_id=?1)",
            [document_id],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if connected {
        return Err("external_revalidation_required".into());
    }
    let expected: Option<String> = connection
        .query_row(
            "SELECT last_synced_hash FROM document_bindings WHERE document_id=?",
            [document_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| error.to_string())?
        .flatten();
    let Some(expected) = expected else {
        return Ok(());
    };
    let current = match read_stable_source(path) {
        Ok(source) => source.hash,
        Err(error) if !path.is_file() => {
            connection
                .execute(
                    "UPDATE document_bindings SET sync_state='missing_external' WHERE document_id=?",
                    [document_id],
                )
                .map_err(|update_error| update_error.to_string())?;
            return Err(error);
        }
        Err(error) => return Err(error),
    };
    if current != expected {
        connection
            .execute(
                "UPDATE document_bindings SET sync_state='conflict' WHERE document_id=?",
                [document_id],
            )
            .map_err(|error| error.to_string())?;
        return Err("Внешний файл изменён: сначала разрешите конфликт синхронизации.".to_string());
    }
    Ok(())
}

pub(crate) fn decode_base64(value: &str) -> Result<Vec<u8>, String> {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = Vec::with_capacity(value.len() * 3 / 4);
    let mut buffer = 0u32;
    let mut bits = 0u8;
    for byte in value.bytes() {
        if byte == b'=' {
            break;
        }
        let digit = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return Err("Некорректные данные DOCX.".to_string()),
        };
        buffer = (buffer << 6) | u32::from(digit);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            output.push((buffer >> bits) as u8);
        }
        if output.len() > MAX_DOCX_BYTES {
            return Err("Документ превышает допустимый размер 100 МБ.".to_string());
        };
    }
    let _ = TABLE;
    Ok(output)
}

pub(crate) fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        let a = bytes[i];
        let b = bytes.get(i + 1).copied().unwrap_or(0);
        let c = bytes.get(i + 2).copied().unwrap_or(0);
        out.push(TABLE[(a >> 2) as usize] as char);
        out.push(TABLE[((a & 3) << 4 | b >> 4) as usize] as char);
        out.push(if i + 1 < bytes.len() {
            TABLE[((b & 15) << 2 | c >> 6) as usize] as char
        } else {
            '='
        });
        out.push(if i + 2 < bytes.len() {
            TABLE[(c & 63) as usize] as char
        } else {
            '='
        });
        i += 3;
    }
    out
}

fn zip_safety(bytes: &[u8]) -> Result<ZipArchive<Cursor<&[u8]>>, String> {
    if bytes.is_empty() || bytes.len() > MAX_DOCX_BYTES {
        return Err("Документ превышает допустимый размер 100 МБ.".to_string());
    }
    let mut archive =
        ZipArchive::new(Cursor::new(bytes)).map_err(|_| "Документ .docx повреждён.".to_string())?;
    if archive.len() > MAX_DOCX_ENTRIES {
        return Err("Распакованный документ слишком велик.".to_string());
    }
    let mut expanded = 0u64;
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .map_err(|_| "Документ .docx повреждён.".to_string())?;
        if unsafe_zip_name(entry.name()) {
            return Err("Архив содержит небезопасный путь.".to_string());
        }
        expanded = expanded.saturating_add(entry.size());
        if expanded > MAX_DOCX_EXPANDED_BYTES {
            return Err("Распакованный документ слишком велик.".to_string());
        }
    }
    Ok(archive)
}

fn unsafe_zip_name(name: &str) -> bool {
    let path = Path::new(name);
    name.starts_with('/')
        || name.contains('\\')
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
}

fn parse_docx(bytes: &[u8]) -> Result<(Value, usize), String> {
    let mut archive = zip_safety(bytes)?;
    let mut xml = Vec::new();
    archive
        .by_name("word/document.xml")
        .map_err(|_| "В DOCX отсутствует основной документ.".to_string())?
        .read_to_end(&mut xml)
        .map_err(|_| "Документ .docx повреждён.".to_string())?;
    if xml.len() > MAX_XML_BYTES
        || xml.windows(9).any(|w| w.eq_ignore_ascii_case(b"<!doctype"))
        || xml.windows(8).any(|w| w.eq_ignore_ascii_case(b"<!entity"))
    {
        return Err("DOCX содержит запрещённые XML-конструкции.".to_string());
    }
    let mut reader = Reader::from_reader(xml.as_slice());
    let mut buffer = Vec::new();
    let mut paragraphs = Vec::new();
    let mut current: Option<Map<String, Value>> = None;
    let mut paragraph_content = Vec::new();
    let mut run_text = String::new();
    let mut run_marks: Vec<Value> = Vec::new();
    let mut heading = None;
    let mut line_height: Option<f64> = None;
    let mut in_text = false;
    loop {
        match reader
            .read_event_into(&mut buffer)
            .map_err(|_| "Документ .docx повреждён.".to_string())?
        {
            Event::Start(event) => {
                let name = local_name(event.name().as_ref()).to_string();
                match name.as_str() {
                    "p" => {
                        current = Some(Map::new());
                        paragraph_content.clear();
                        run_text.clear();
                        run_marks.clear();
                        heading = None;
                        line_height = None;
                    }
                    "r" => {
                        run_text.clear();
                        run_marks.clear();
                    }
                    "pStyle" => {
                        if let Some(value) = attr(&event, "val") {
                            heading = value
                                .strip_prefix("Heading")
                                .and_then(|v| v.parse::<u64>().ok())
                                .map(|v| v.clamp(1, 6))
                        }
                    }
                    "spacing" => {
                        line_height = word_line_height(&event);
                    }
                    "tab" => run_text.push('\t'),
                    "t" => in_text = true,
                    _ => {}
                }
            }
            Event::Empty(event) => match local_name(event.name().as_ref()) {
                "br" => paragraph_content.push(serde_json::json!({"type":"hardBreak"})),
                "tab" => run_text.push('\t'),
                "b" => run_marks.push(serde_json::json!({"type":"bold"})),
                "i" => run_marks.push(serde_json::json!({"type":"italic"})),
                "u" => run_marks.push(serde_json::json!({"type":"underline"})),
                "strike" => run_marks.push(serde_json::json!({"type":"strike"})),
                "pStyle" => {
                    if let Some(value) = attr(&event, "val") {
                        heading = value
                            .strip_prefix("Heading")
                            .and_then(|v| v.parse::<u64>().ok())
                            .map(|v| v.clamp(1, 6));
                    }
                }
                "spacing" => line_height = word_line_height(&event),
                _ => {}
            },
            Event::Text(event) => {
                if current.is_some() && in_text {
                    run_text.push_str(
                        &event
                            .decode()
                            .map_err(|_| "Документ .docx повреждён.".to_string())?,
                    )
                }
            }
            Event::End(event) => {
                let end_name = event.name();
                let name = local_name(end_name.as_ref());
                match name {
                    "t" => in_text = false,
                    "r" => {
                        if !run_text.is_empty() {
                            let mut node = Map::new();
                            node.insert("type".into(), Value::String("text".into()));
                            node.insert("text".into(), Value::String(run_text.clone()));
                            if !run_marks.is_empty() {
                                node.insert("marks".into(), Value::Array(run_marks.clone()));
                            }
                            paragraph_content.push(Value::Object(node));
                        }
                        run_text.clear();
                        run_marks.clear();
                    }
                    "p" => {
                        if let Some(mut paragraph) = current.take() {
                            paragraph.insert(
                                "type".into(),
                                heading.map_or(Value::String("paragraph".into()), |_| {
                                    Value::String("heading".into())
                                }),
                            );
                            if heading.is_some() || line_height.is_some() {
                                let mut attrs = Map::new();
                                if let Some(level) = heading {
                                    attrs.insert("level".into(), serde_json::json!(level));
                                }
                                if let Some(value) = line_height {
                                    attrs.insert(
                                        "lineHeight".into(),
                                        Value::String(line_height_string(value)),
                                    );
                                }
                                paragraph.insert("attrs".into(), Value::Object(attrs));
                            }
                            if !paragraph_content.is_empty() {
                                paragraph.insert(
                                    "content".into(),
                                    Value::Array(paragraph_content.clone()),
                                );
                            }
                            paragraphs.push(Value::Object(paragraph));
                        }
                    }
                    _ => {}
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear()
    }
    let content = serde_json::json!({"type":"doc","content":paragraphs});
    Ok((content.clone(), symbols(&content)))
}
fn local_name(name: &[u8]) -> &str {
    std::str::from_utf8(name)
        .unwrap_or("")
        .rsplit(':')
        .next()
        .unwrap_or("")
}
fn attr(event: &quick_xml::events::BytesStart<'_>, wanted: &str) -> Option<String> {
    event.attributes().flatten().find_map(|item| {
        (local_name(item.key.as_ref()) == wanted)
            .then(|| String::from_utf8_lossy(&item.value).to_string())
    })
}

/// Tiptap stores line height as a unitless CSS multiplier.  Word uses 240ths
/// of a line for ``auto`` spacing, so conversion is explicit at this boundary.
/// Exact/at-least spacing is an absolute twip value and cannot safely be
/// reused as a multiplier without font metrics; retain the editor default for
/// those documents rather than turning (for example) 360 twips into `360`.
fn word_line_height(event: &quick_xml::events::BytesStart<'_>) -> Option<f64> {
    let rule = attr(event, "lineRule").unwrap_or_else(|| "auto".to_string());
    if !rule.eq_ignore_ascii_case("auto") {
        return None;
    }
    let line = attr(event, "line")?.parse::<f64>().ok()?;
    let multiplier = line / 240.0;
    (multiplier.is_finite()
        && (MIN_LINE_HEIGHT_MULTIPLIER..=MAX_LINE_HEIGHT_MULTIPLIER).contains(&multiplier))
    .then_some(multiplier)
}

fn line_height_string(value: f64) -> String {
    let rounded = (value * 100.0).round() / 100.0;
    let mut result = format!("{rounded:.2}");
    while result.ends_with('0') {
        result.pop();
    }
    if result.ends_with('.') {
        result.pop();
    }
    result
}

fn line_height_twips(node: &Value) -> Option<i64> {
    let value = node
        .get("attrs")
        .and_then(|attrs| attrs.get("lineHeight"))
        .and_then(Value::as_str)
        .and_then(|value| value.parse::<f64>().ok())?;
    (value.is_finite()
        && (MIN_LINE_HEIGHT_MULTIPLIER..=MAX_LINE_HEIGHT_MULTIPLIER).contains(&value))
    .then(|| (value * 240.0).round() as i64)
}

fn build_docx(content: &Value) -> Result<Vec<u8>, String> {
    let mut output = Cursor::new(Vec::new());
    let mut archive = zip::ZipWriter::new(&mut output);
    let options = SimpleFileOptions::default();
    archive
        .start_file("[Content_Types].xml", options)
        .map_err(|e| e.to_string())?;
    archive.write_all(br#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#).map_err(|e|e.to_string())?;
    archive
        .start_file("_rels/.rels", options)
        .map_err(|e| e.to_string())?;
    archive.write_all(br#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#).map_err(|e|e.to_string())?;
    archive
        .start_file("word/document.xml", options)
        .map_err(|e| e.to_string())?;
    let mut xml = String::from(
        r#"<?xml version="1.0" encoding="UTF-8"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>"#,
    );
    if let Some(items) = content.get("content").and_then(Value::as_array) {
        for item in items {
            write_paragraph(&mut xml, item)
        }
    }
    xml.push_str("<w:sectPr><w:pgSz w:w=\"11906\" w:h=\"16838\"/><w:pgMar w:top=\"1440\" w:right=\"1440\" w:bottom=\"1440\" w:left=\"1440\"/></w:sectPr></w:body></w:document>");
    archive
        .write_all(xml.as_bytes())
        .map_err(|e| e.to_string())?;
    archive.finish().map_err(|e| e.to_string())?;
    Ok(output.into_inner())
}
fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
fn write_paragraph(xml: &mut String, node: &Value) {
    xml.push_str("<w:p>");
    let line_height = line_height_twips(node);
    if node.get("type").and_then(Value::as_str) == Some("heading") || line_height.is_some() {
        xml.push_str("<w:pPr>");
        if let Some(level) = node
            .get("attrs")
            .and_then(|v| v.get("level"))
            .and_then(Value::as_u64)
        {
            xml.push_str(&format!("<w:pStyle w:val=\"Heading{level}\"/>"))
        }
        if let Some(twips) = line_height {
            xml.push_str(&format!(
                "<w:spacing w:line=\"{twips}\" w:lineRule=\"auto\"/>"
            ));
        }
        xml.push_str("</w:pPr>");
    }
    if let Some(children) = node.get("content").and_then(Value::as_array) {
        for child in children {
            write_run(xml, child)
        }
    }
    xml.push_str("</w:p>")
}
fn write_run(xml: &mut String, node: &Value) {
    if node.get("type").and_then(Value::as_str) == Some("hardBreak") {
        xml.push_str("<w:br/>");
        return;
    }
    if node.get("type").and_then(Value::as_str) != Some("text") {
        if let Some(children) = node.get("content").and_then(Value::as_array) {
            for child in children {
                write_run(xml, child)
            }
        }
        return;
    }
    let mut props = String::new();
    if let Some(marks) = node.get("marks").and_then(Value::as_array) {
        for mark in marks {
            match mark.get("type").and_then(Value::as_str) {
                Some("bold") => props.push_str("<w:b/>"),
                Some("italic") => props.push_str("<w:i/>"),
                Some("underline") => props.push_str("<w:u w:val=\"single\"/>"),
                Some("strike") => props.push_str("<w:strike/>"),
                _ => {}
            }
        }
    }
    xml.push_str("<w:r>");
    if !props.is_empty() {
        xml.push_str("<w:rPr>");
        xml.push_str(&props);
        xml.push_str("</w:rPr>")
    }
    let text = node.get("text").and_then(Value::as_str).unwrap_or("");
    for (index, part) in text.split('\t').enumerate() {
        if index > 0 {
            xml.push_str("<w:tab/>");
        }
        if !part.is_empty() {
            xml.push_str("<w:t xml:space=\"preserve\">");
            xml.push_str(&xml_escape(part));
            xml.push_str("</w:t>");
        }
    }
    xml.push_str("</w:r>")
}

fn find_scrivener_xml(root: &Path) -> Result<PathBuf, String> {
    let preferred = root.join("project.scrivx");
    if preferred.is_file() {
        if fs::symlink_metadata(&preferred)
            .map(|metadata| metadata.file_type().is_symlink())
            .unwrap_or(false)
        {
            return Err("Проект Scrivener содержит небезопасную ссылку.".to_string());
        }
        return Ok(preferred);
    }
    let old = root.join("binder.scrivproj");
    if old.is_file() {
        if fs::symlink_metadata(&old)
            .map(|metadata| metadata.file_type().is_symlink())
            .unwrap_or(false)
        {
            return Err("Проект Scrivener содержит небезопасную ссылку.".to_string());
        }
        return Ok(old);
    }
    for entry in
        fs::read_dir(root).map_err(|_| "Не удалось прочитать проект Scrivener.".to_string())?
    {
        let path = entry
            .map_err(|_| "Не удалось прочитать проект Scrivener.".to_string())?
            .path();
        if path.extension().and_then(|v| v.to_str()) == Some("scrivx") && path.is_file() {
            if fs::symlink_metadata(&path)
                .map(|metadata| metadata.file_type().is_symlink())
                .unwrap_or(false)
            {
                return Err("Проект Scrivener содержит небезопасную ссылку.".to_string());
            }
            return Ok(path);
        }
    }
    Err("Не найден файл проекта Scrivener.".to_string())
}
fn parse_scrivener_xml(path: &Path) -> Result<Vec<ScrivenerItem>, String> {
    let bytes = fs::read(path).map_err(|_| "Не удалось прочитать проект Scrivener.".to_string())?;
    if bytes.len() > MAX_XML_BYTES
        || bytes
            .windows(9)
            .any(|w| w.eq_ignore_ascii_case(b"<!doctype"))
        || bytes
            .windows(8)
            .any(|w| w.eq_ignore_ascii_case(b"<!entity"))
    {
        return Err("Структура проекта Scrivener повреждена.".to_string());
    }
    let mut reader = Reader::from_reader(bytes.as_slice());
    let mut buffer = Vec::new();
    let mut roots = Vec::new();
    let mut stack: Vec<ScrivenerItem> = Vec::new();
    let mut title = false;
    loop {
        match reader
            .read_event_into(&mut buffer)
            .map_err(|_| "Структура проекта Scrivener повреждена.".to_string())?
        {
            Event::Start(event) => match local_name(event.name().as_ref()) {
                "BinderItem" => {
                    let id = attr(&event, "UUID")
                        .or_else(|| attr(&event, "Uuid"))
                        .or_else(|| attr(&event, "uuid"))
                        .or_else(|| attr(&event, "ID"))
                        .ok_or_else(|| "Структура проекта Scrivener повреждена.".to_string())?;
                    stack.push(ScrivenerItem {
                        id,
                        title: "Без названия".into(),
                        children: Vec::new(),
                    })
                }
                "Title" => title = true,
                _ => {}
            },
            Event::Text(event) => {
                if title {
                    if let Some(item) = stack.last_mut() {
                        item.title = event
                            .decode()
                            .map_err(|_| "Структура проекта Scrivener повреждена.".to_string())?
                            .into_owned()
                    }
                }
            }
            Event::End(event) => match local_name(event.name().as_ref()) {
                "Title" => title = false,
                "BinderItem" => {
                    let item = stack
                        .pop()
                        .ok_or_else(|| "Структура проекта Scrivener повреждена.".to_string())?;
                    if let Some(parent) = stack.last_mut() {
                        parent.children.push(item)
                    } else {
                        roots.push(item)
                    }
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
        buffer.clear()
    }
    Ok(roots)
}
fn contains_item(items: &[ScrivenerItem], id: &str) -> bool {
    items.iter().any(|item| {
        item.id
            .trim_matches('{')
            .trim_matches('}')
            .eq_ignore_ascii_case(id.trim_matches('{').trim_matches('}'))
            || contains_item(&item.children, id)
    })
}

fn read_scrivener_item(root: &Path, item_id: &str) -> Result<Vec<u8>, String> {
    let docs_root = [
        root.join("Files").join("Docs"),
        root.join("Files").join("Data"),
    ]
    .into_iter()
    .find(|path| path.is_dir())
    .ok_or_else(|| "Источник синхронизации не найден.".to_string())?;
    let expected = item_id
        .trim_matches('{')
        .trim_matches('}')
        .to_ascii_lowercase();
    let mut pending = vec![docs_root];
    let mut visited = 0usize;
    while let Some(directory) = pending.pop() {
        visited += 1;
        if visited > MAX_DOCX_ENTRIES {
            return Err("Проект Scrivener слишком велик.".to_string());
        }
        let metadata = fs::symlink_metadata(&directory)
            .map_err(|_| "Не удалось прочитать проект Scrivener.".to_string())?;
        if metadata.file_type().is_symlink() {
            return Err("Проект Scrivener содержит небезопасную ссылку.".to_string());
        }
        for entry in fs::read_dir(&directory)
            .map_err(|_| "Не удалось прочитать проект Scrivener.".to_string())?
        {
            let entry = entry.map_err(|_| "Не удалось прочитать проект Scrivener.".to_string())?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)
                .map_err(|_| "Не удалось прочитать проект Scrivener.".to_string())?;
            if metadata.file_type().is_symlink() {
                return Err("Проект Scrivener содержит небезопасную ссылку.".to_string());
            }
            if metadata.is_dir() {
                if entry
                    .file_name()
                    .to_string_lossy()
                    .trim_matches('{')
                    .trim_matches('}')
                    .eq_ignore_ascii_case(&expected)
                {
                    let mut rtf_files = fs::read_dir(&path)
                        .map_err(|_| "Не удалось прочитать документ Scrivener.".to_string())?
                        .filter_map(Result::ok)
                        .map(|value| value.path())
                        .filter(|value| {
                            value
                                .extension()
                                .and_then(|extension| extension.to_str())
                                .is_some_and(|extension| extension.eq_ignore_ascii_case("rtf"))
                        })
                        .collect::<Vec<_>>();
                    rtf_files.sort_by_key(|value| {
                        value
                            .file_name()
                            .map(|name| name.to_string_lossy().to_ascii_lowercase())
                    });
                    let file = rtf_files
                        .into_iter()
                        .next()
                        .ok_or_else(|| "Документ Scrivener не содержит текста.".to_string())?;
                    let file_metadata = fs::metadata(&file)
                        .map_err(|_| "Не удалось прочитать документ Scrivener.".to_string())?;
                    if file_metadata.len() > MAX_DOCX_BYTES as u64 {
                        return Err("Документ Scrivener слишком велик.".to_string());
                    }
                    return fs::read(file)
                        .map_err(|_| "Не удалось прочитать документ Scrivener.".to_string());
                }
                pending.push(path);
            }
        }
    }
    Err("Документ Scrivener не содержит текста.".to_string())
}

fn count_rtf_symbols(bytes: &[u8]) -> usize {
    let text = String::from_utf8_lossy(bytes);
    let mut count = 0usize;
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        match character {
            '{' | '}' | '\\' => {
                if character == '\\' {
                    if let Some(next) = chars.peek().copied() {
                        if next == '\\' || next == '{' || next == '}' {
                            count += 1;
                            chars.next();
                        } else if next == '\'' {
                            chars.next();
                            chars.next();
                        } else {
                            while chars
                                .peek()
                                .is_some_and(|value| value.is_ascii_alphabetic())
                            {
                                chars.next();
                            }
                            if chars.peek().is_some_and(|value| *value == '-') {
                                chars.next();
                            }
                            while chars.peek().is_some_and(|value| value.is_ascii_digit()) {
                                chars.next();
                            }
                            if chars.peek().is_some_and(|value| *value == ' ') {
                                chars.next();
                            }
                        }
                    }
                }
            }
            '\n' | '\r' => {}
            _ => count += 1,
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static DATA_DIR_ENV_LOCK: Mutex<()> = Mutex::new(());

    fn rich_tiptap_document() -> Value {
        serde_json::json!({
            "type": "doc",
            "content": [
                {
                    "type": "heading",
                    "attrs": {"level": 2, "textAlign": "center", "lineHeight": "1.5"},
                    "content": [{"type": "text", "text": "Rich heading", "marks": [{"type": "bold"}]}]
                },
                {
                    "type": "paragraph",
                    "attrs": {"textAlign": "justify", "lineHeight": "1.15"},
                    "content": [
                        {
                            "type": "text",
                            "text": "Styled",
                            "marks": [
                                {"type": "bold"},
                                {"type": "italic"},
                                {"type": "underline"},
                                {"type": "strike"},
                                {"type": "textStyle", "attrs": {"fontFamily": "Georgia", "fontSize": "18px", "color": "#123456"}},
                                {"type": "highlight", "attrs": {"color": "#ffee66"}}
                            ]
                        },
                        {"type": "text", "text": " sub", "marks": [{"type": "subscript"}]},
                        {"type": "text", "text": " super", "marks": [{"type": "superscript"}]},
                        {"type": "hardBreak"},
                        {"type": "text", "text": "\tliteral tab"}
                    ]
                },
                {
                    "type": "bulletList",
                    "content": [{"type": "listItem", "content": [{"type": "paragraph", "content": [{"type": "text", "text": "Bullet"}]}]}]
                },
                {
                    "type": "orderedList",
                    "attrs": {"start": 3},
                    "content": [{"type": "listItem", "content": [{"type": "paragraph", "content": [{"type": "text", "text": "Ordered"}]}]}]
                },
                {
                    "type": "blockquote",
                    "content": [{"type": "paragraph", "content": [{"type": "text", "text": "Quote"}]}]
                }
            ]
        })
    }

    fn insert_app_project(connection: &rusqlite::Connection, project_id: &str) {
        connection
            .execute(
                "INSERT INTO projects(id,name,goal,infinite,unit,status,payload_json) VALUES(?1,'Book',100,0,'symbols','активен',?2)",
                params![project_id, serde_json::json!({"name":"Book","status":"активен","work_method":"app","stages":[]}).to_string()],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO project_order(project_id,position) VALUES(?1,0)",
                [project_id],
            )
            .unwrap();
    }

    #[test]
    fn sqlite_round_trip_preserves_complete_tiptap_json_and_metadata() {
        let _env_guard = DATA_DIR_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = std::env::temp_dir().join(format!(
            "nfprogress-rich-document-round-trip-{}",
            std::process::id()
        ));
        let previous_root = std::env::var_os("NFPROGRESS_DATA_DIR");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        std::env::set_var("NFPROGRESS_DATA_DIR", &root);

        let result = (|| -> Result<(), String> {
            let connection = crate::sqlite::open_database(&root.join("nfprogress.db"))
                .map_err(|error| error.to_string())?;
            insert_app_project(&connection, "rich-project");
            drop(connection);
            let content = rich_tiptap_document();

            let saved = save_document(DocumentSaveCommand {
                project_id: "rich-project".into(),
                stage_id: None,
                content: content.clone(),
                expected_heads: None,
            })?;
            let document_id = saved["document_id"].as_str().unwrap().to_string();
            assert_eq!(saved["content"], content);
            assert_eq!(saved["content_format"], "tiptap-json/v1");
            assert!(saved["updated_at"].as_str().is_some());

            let connection = rusqlite::Connection::open(root.join("nfprogress.db"))
                .map_err(|error| error.to_string())?;
            let row: (String, String, String, i64, Option<String>) = connection
                .query_row(
                    "SELECT id,scope_key,content_json,revision,updated_at FROM documents WHERE scope_key='rich-project:project'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
                )
                .map_err(|error| error.to_string())?;
            assert_eq!(row.0, document_id);
            assert_eq!(row.1, "rich-project:project");
            assert_eq!(serde_json::from_str::<Value>(&row.2).unwrap(), content);
            assert_eq!(row.3, 0);
            assert!(row.4.is_some());
            drop(connection);

            let loaded = get_document(DocumentScope {
                project_id: "rich-project".into(),
                stage_id: None,
                expected_heads: None,
            })?;
            assert_eq!(loaded["content"], content);
            assert_eq!(loaded["document_id"], document_id);
            assert_eq!(loaded["content_format"], "tiptap-json/v1");
            Ok(())
        })();

        if let Some(previous_root) = previous_root {
            std::env::set_var("NFPROGRESS_DATA_DIR", previous_root);
        } else {
            std::env::remove_var("NFPROGRESS_DATA_DIR");
        }
        let _ = fs::remove_dir_all(&root);
        result.unwrap();
    }

    #[test]
    fn legacy_documents_json_rich_content_reaches_native_get_unchanged() {
        let _env_guard = DATA_DIR_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = std::env::temp_dir().join(format!(
            "nfprogress-rich-document-migration-{}",
            std::process::id()
        ));
        let previous_root = std::env::var_os("NFPROGRESS_DATA_DIR");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let content = rich_tiptap_document();
        fs::write(
            root.join("documents.json"),
            serde_json::json!({
                "legacy:project": {
                    "document_id": "legacy-rich-document",
                    "project_id": "legacy",
                    "content_format": "tiptap-json/v1",
                    "content": content
                }
            })
            .to_string(),
        )
        .unwrap();
        let connection = crate::sqlite::open_database(&root.join("nfprogress.db")).unwrap();
        insert_app_project(&connection, "legacy");
        drop(connection);
        std::env::set_var("NFPROGRESS_DATA_DIR", &root);

        let result = get_document(DocumentScope {
            project_id: "legacy".into(),
            stage_id: None,
            expected_heads: None,
        });

        if let Some(previous_root) = previous_root {
            std::env::set_var("NFPROGRESS_DATA_DIR", previous_root);
        } else {
            std::env::remove_var("NFPROGRESS_DATA_DIR");
        }
        let loaded = result.unwrap();
        assert_eq!(loaded["document_id"], "legacy-rich-document");
        assert_eq!(loaded["content_format"], "tiptap-json/v1");
        assert_eq!(loaded["content"], rich_tiptap_document());
        let raw = rusqlite::Connection::open(root.join("nfprogress.db"))
            .unwrap()
            .query_row(
                "SELECT content_json FROM documents WHERE id='legacy-rich-document'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&raw).unwrap(),
            rich_tiptap_document()
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn migration_ids_are_stable_for_a_scope() {
        assert_eq!(stable_id("project:stage"), stable_id("project:stage"));
        assert_ne!(stable_id("project:stage"), stable_id("project:other"));
    }

    #[test]
    fn generated_docx_round_trips_supported_text_marks() {
        let content = serde_json::json!({
            "type": "doc",
            "content": [{"type":"heading","attrs":{"level":2},"content":[{"type":"text","text":"Заголовок","marks":[{"type":"bold"}]}]}, {"type":"paragraph","content":[{"type":"text","text":"текст"}]}]
        });
        let bytes = build_docx(&content).unwrap();
        let (parsed, symbols_count) = parse_docx(&bytes).unwrap();
        assert_eq!(symbols_count, 14, "{parsed}");
        assert_eq!(symbols(&parsed), 14, "{parsed}");
        assert_eq!(parsed["content"][0]["type"], "heading");
        assert_eq!(
            parsed["content"][0]["content"][0]["marks"][0]["type"],
            "bold"
        );
    }

    #[test]
    fn docx_round_trip_preserves_tabs_and_supported_line_height_multipliers() {
        let content = serde_json::json!({
            "type": "doc",
            "content": [
                {
                    "type": "paragraph",
                    "attrs": {"lineHeight": "1"},
                    "content": [{"type": "text", "text": "before\tmiddle\tafter"}]
                },
                {"type": "paragraph", "attrs": {"lineHeight": "1.15"}, "content": [{"type": "text", "text": "1.15"}]},
                {"type": "paragraph", "attrs": {"lineHeight": "1.5"}, "content": [{"type": "text", "text": "1.5"}]},
                {"type": "paragraph", "attrs": {"lineHeight": "2"}, "content": [{"type": "text", "text": "2"}]}
            ]
        });

        let bytes = build_docx(&content).unwrap();
        let mut archive = ZipArchive::new(Cursor::new(bytes.as_slice())).unwrap();
        let mut document_xml = String::new();
        archive
            .by_name("word/document.xml")
            .unwrap()
            .read_to_string(&mut document_xml)
            .unwrap();
        assert!(document_xml.contains("before</w:t><w:tab/><w:t xml:space=\"preserve\">middle</w:t><w:tab/><w:t xml:space=\"preserve\">after"));
        assert!(!document_xml.contains("before\tmiddle\tafter"));
        for twips in [240, 276, 360, 480] {
            assert!(document_xml.contains(&format!("w:line=\"{twips}\" w:lineRule=\"auto\"")));
        }
        drop(archive);
        let (parsed, _) = parse_docx(&bytes).unwrap();
        assert_eq!(
            parsed["content"][0]["content"][0]["text"],
            "before\tmiddle\tafter"
        );
        for (index, expected) in ["1", "1.15", "1.5", "2"].iter().enumerate() {
            assert_eq!(parsed["content"][index]["attrs"]["lineHeight"], *expected);
        }

        let bytes = build_docx(&parsed).unwrap();
        let (reopened, _) = parse_docx(&bytes).unwrap();
        assert_eq!(reopened, parsed);
    }

    #[test]
    fn stage_document_daily_goal_keeps_legacy_history_and_records_each_writing_day_once() {
        use serde_json::json;

        let _guard = DATA_DIR_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let root =
            std::env::temp_dir().join(format!("worta-stage-writing-day-{}", std::process::id()));
        let previous = std::env::var_os("NFPROGRESS_DATA_DIR");
        fs::create_dir_all(&root).unwrap();
        std::env::set_var("NFPROGRESS_DATA_DIR", &root);
        let result = (|| -> Result<(), String> {
            let (db, _) = open()?;
            crate::initialize_fresh_desktop_database(&db, &root)?;
            let history = json!([{"__type__":"date","value":"2026-09-17"},"freeze"]);
            let project = json!({"id":"p","name":"Book","status":"активен","work_method":"manual","unit":"symbols","goal":100,"total":10,"streak_enabled":true});
            let stage = json!({"id":"s","name":"Chapter","status":"активен","work_method":"app","unit":"symbols","goal":100,"total":10,"deadline":"2026-10-06","personal_goal":5,"planning_date":"2026-09-18","today_goal":10,"plan_daily_goal":5,"streak_enabled":true});
            db.execute("INSERT INTO projects(id,name,goal,infinite,unit,status,payload_json) VALUES('p','Book',100,0,'symbols','активен',?1)", [project.to_string()]).map_err(|e| e.to_string())?;
            db.execute("INSERT INTO stages(id,project_id,name,goal,infinite,unit,status,payload_json) VALUES('s','p','Chapter',100,0,'symbols','активен',?1)", [stage.to_string()]).map_err(|e| e.to_string())?;
            db.execute_batch(r#"INSERT INTO project_order(project_id,position) VALUES('p',0);
                INSERT INTO stage_order(stage_id,project_id,position) VALUES('s','p',0);
                INSERT INTO settings(key,value_json) VALUES('global_streak','true'),('game_mode','false'),('start_day_time','"04:00:00"');
                INSERT INTO progress_entries(id,project_id,stage_id,created_at,added_symbols,payload_json)
                  VALUES('legacy','p','s','2026-09-18T02:00:00',10,'{"id":"legacy","new_total":10,"added_symbols":10,"created_at":"2026-09-18T02:00:00"}');
                INSERT INTO progress_order(entry_id,position) VALUES('legacy',0);
            "#).map_err(|e| e.to_string())?;
            db.execute("UPDATE game_state SET payload_json=?1 WHERE id=1", [json!({
                "gamer": {"coins":42},
                "game": {},
                "project_game_state": {"stage:p:s":{"streaks":history,"streak_status":"Freeze","max_streak":7,"last_streak_bonus":{"__type__":"date","value":"2026-09-17"},"legacy_extra":"keep"}},
                "global_streak": {"global_streaks":history,"global_streak_status":"Freeze","max_global_streak":8},
            }).to_string()]).map_err(|e| e.to_string())?;
            drop(db);

            let set_clock = |time: &str| -> Result<(), String> {
                let (db, _) = open()?;
                db.execute("UPDATE game_state SET payload_json=json_set(payload_json,'$.game.extensions.developer_clock',json(?1)) WHERE id=1",
                    [json!({"enabled":true,"datetime":time}).to_string()]).map_err(|e| e.to_string())?;
                Ok(())
            };
            let content = |size: usize| {
                if size == 0 {
                    json!({"type":"doc","content":[{"type":"paragraph"}]})
                } else {
                    json!({"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"x".repeat(size)}]}]})
                }
            };
            let record = |size| {
                record_document_progress(DocumentProgressCommand {
                    project_id: "p".into(),
                    stage_id: Some("s".into()),
                    content: Some(content(size)),
                    expected_heads: None,
                })
            };
            let stage_view = || -> Result<Value, String> {
                let model = crate::read_sqlite_projects()?;
                serde_json::from_str(&model.stages[0].payload_json).map_err(|e| e.to_string())
            };

            set_clock("2026-09-20T03:59:59")?;
            let initial = stage_view()?;
            assert_eq!(initial["planning_date"], "2026-09-19");
            assert_eq!(initial["today_goal"], 15.0);
            assert_eq!(initial["added_today"], 0.0);
            assert_eq!(initial["streak_status"], "Active");
            assert_eq!(initial["streaks"], history);

            let partial = record(12)?;
            assert_eq!(partial["progress"]["entry"]["writing_day"], "2026-09-19");
            assert_eq!(stage_view()?["added_today"], 2.0);
            assert_eq!(stage_view()?["streak_length"], 2);
            let done = record(15)?;
            assert_eq!(done["progress"]["project"]["stages"][0]["streak_length"], 3);
            assert_eq!(stage_view()?["added_today"], 5.0);
            assert_eq!(stage_view()?["streak_status"], "Go");
            assert!(record(15)?["progress"].is_null());

            set_clock("2026-09-20T04:00:00")?;
            assert_eq!(stage_view()?["today_goal"], 20.0);
            assert_eq!(stage_view()?["added_today"], 0.0);
            assert_eq!(stage_view()?["streak_status"], "Active");
            assert_eq!(
                record(20)?["progress"]["entry"]["writing_day"],
                "2026-09-20"
            );
            assert_eq!(stage_view()?["streak_length"], 4);

            set_clock("2026-09-22T11:00:00")?;
            assert_eq!(stage_view()?["streak_status"], "Lose 4");
            assert_eq!(stage_view()?["streak_length"], 0);
            assert_eq!(stage_view()?["today_goal"], 30.0);
            record(30)?;
            assert_eq!(stage_view()?["streak_status"], "Lose 4 Start");
            assert_eq!(stage_view()?["streak_length"], 1);
            assert_eq!(stage_view()?["max_streak"], 7);
            assert_eq!(record(28)?["progress"]["entry"]["added_symbols"], -2.0);
            assert_eq!(stage_view()?["added_today"], 8.0);
            assert_eq!(record(0)?["progress"]["entry"]["added_symbols"], -28.0);
            assert_eq!(stage_view()?["added_today"], -20.0);
            assert_eq!(stage_view()?["today_goal"], 30.0);
            assert!(record(0)?["progress"].is_null());
            assert_eq!(
                get_document(DocumentScope {
                    project_id: "p".into(),
                    stage_id: Some("s".into()),
                    expected_heads: None
                })?["content"],
                content(0)
            );

            let (db, _) = open()?;
            let state = crate::daily_goals::game_state(&db)?;
            assert_eq!(
                state["global_streak"]["global_streak_status"],
                "Lose 4 Start"
            );
            assert_eq!(state["global_streak"]["max_global_streak"], 8);
            assert_eq!(
                state["project_game_state"]["stage:p:s"]["legacy_extra"],
                "keep"
            );
            assert_eq!(state["gamer"]["coins"], 42);
            assert_eq!(crate::read_sqlite_projects()?.progress_entries.len(), 7);

            db.execute(
                "UPDATE settings SET value_json='true' WHERE key='game_mode'",
                [],
            )
            .map_err(|e| e.to_string())?;
            db.execute("UPDATE game_state SET payload_json=json_set(payload_json,'$.gamer.items',json(?1)) WHERE id=1",
                [json!({"Предметы":{"Заморозка":1}}).to_string()]).map_err(|e| e.to_string())?;
            drop(db);
            set_clock("2026-09-24T11:00:00")?;
            let (db, _) = open()?;
            assert!(!crate::game::refresh_global_loss(
                &db,
                "2026-09-24",
                "2026-09-24T11:00:00Z"
            )?);
            assert_eq!(stage_view()?["streak_status"], "Active");
            drop(db);
            let result = record(40)?;
            let rewards = &result["progress"]["game"]["result"]["progress_rewards"];
            assert_eq!(
                rewards,
                &json!([
                    {"kind":"writing","coins":4.0,"experience":200.0},
                    {"kind":"local_streak_continued","coins":30.0,"experience":300.0},
                    {"kind":"global_streak_continued","coins":30.0,"experience":3000.0}
                ])
            );
            assert_eq!(
                result["progress"]["game"]["state"]["profile"]["coins"],
                106.0
            );
            let (mut db, _) = open()?;
            assert_eq!(crate::game::process_pending_events(&mut db, 100)?.failed, 0);
            let frozen = crate::daily_goals::game_state(&db)?;
            assert_eq!(
                frozen["global_streak"]["global_streaks"]
                    .as_array()
                    .unwrap()
                    .len(),
                3
            );
            assert_eq!(frozen["global_streak"]["global_streaks"][1], "freeze");
            assert_eq!(
                frozen["project_game_state"]["stage:p:s"]["streaks"][1],
                "freeze"
            );
            assert_eq!(frozen["gamer"]["items"]["Предметы"]["Заморозка"], 0);
            assert_eq!(frozen["gamer"]["coins"], 106.0);
            drop(db);
            let same_day = record(41)?;
            assert_eq!(
                same_day["progress"]["game"]["result"]["progress_rewards"],
                json!([{"kind":"writing","coins":0.1,"experience":5.0}])
            );

            set_clock("2026-09-26T11:00:00")?;
            record(50)?;
            assert!(record(50)?["progress"].is_null());
            let (db, _) = open()?;
            let restarted = crate::daily_goals::game_state(&db)?;
            assert_eq!(
                restarted["global_streak"]["global_streak_status"],
                "Lose 3 Start"
            );
            assert_eq!(restarted["gamer"]["health"], 85.0);
            assert_eq!(restarted["gamer"]["coins"], 127.0);
            Ok(())
        })();
        if let Some(previous) = previous {
            std::env::set_var("NFPROGRESS_DATA_DIR", previous);
        } else {
            std::env::remove_var("NFPROGRESS_DATA_DIR");
        }
        fs::remove_dir_all(root).unwrap();
        result.unwrap();
    }

    #[test]
    fn migrated_scrivener_binding_opens_its_tree_and_synchronizes_once() {
        let _guard = DATA_DIR_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let root =
            std::env::temp_dir().join(format!("worta-scrivener-legacy-{}", std::process::id()));
        let previous = std::env::var_os("NFPROGRESS_DATA_DIR");
        fs::create_dir(&root).unwrap();
        std::env::set_var("NFPROGRESS_DATA_DIR", &root);
        let result = (|| -> Result<(), String> {
            let source = root.join("Book.scriv");
            let item = source.join("Files/Data/CHAPTER");
            fs::create_dir_all(&item).map_err(|e| e.to_string())?;
            fs::write(source.join("Book.scrivx"),
                r#"<ScrivenerProject><Binder><BinderItem UUID="DRAFT"><Title>Draft</Title><Children><BinderItem UUID="CHAPTER"><Title>Chapter</Title></BinderItem></Children></BinderItem></Binder></ScrivenerProject>"#
            ).map_err(|e| e.to_string())?;
            fs::write(item.join("content.rtf"), br"{\rtf1 Hello world}")
                .map_err(|e| e.to_string())?;
            let path = source.to_string_lossy().into_owned();
            let tree = inspect_scrivener(ScrivenerInspectCommand { path: path.clone() })?;
            assert_eq!(tree[0].children[0].id, "CHAPTER");
            assert_eq!(tree[0].children[0].title, "Chapter");

            let (db, _) = open()?;
            crate::initialize_fresh_desktop_database(&db, &root)?;
            let project = serde_json::json!({"id":"book","name":"Book","status":"активен","unit":"symbols","goal":100,"total":0,"work_method":"sync"});
            db.execute("INSERT INTO projects(id,name,goal,infinite,unit,status,payload_json) VALUES('book','Book',100,0,'symbols','активен',?1)", [project.to_string()]).map_err(|e| e.to_string())?;
            db.execute("INSERT INTO project_order VALUES('book',0)", [])
                .map_err(|e| e.to_string())?;
            db.execute("INSERT INTO project_bindings(id,project_id,binding_type,external_path,source_id,payload_json) VALUES('legacy','book','scrivener',?1,'CHAPTER','{}')", [&path]).map_err(|e| e.to_string())?;
            drop(db);

            let scope = || SyncScopeCommand {
                project_id: "book".into(),
                stage_id: None,
            };
            let summary = get_sync(scope())?;
            assert!(summary.configured);
            assert_eq!(summary.item_id.as_deref(), Some("CHAPTER"));

            let batch = run_project_syncs("book".into())?;
            assert_eq!((batch.checked, batch.changed, batch.failed), (1, 1, 0));
            assert_eq!(batch.items[0].symbols, Some(11));
            let repeat = run_sync(scope())?;
            assert!(!repeat.changed);
            assert!(repeat.sync.last_synced_at.is_some());
            assert!(!remove_sync(scope())?.configured);
            let (db, _) = open()?;
            assert_eq!(
                db.query_row("SELECT COUNT(*) FROM progress_entries", [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                1
            );
            Ok(())
        })();
        if let Some(previous) = previous {
            std::env::set_var("NFPROGRESS_DATA_DIR", previous);
        } else {
            std::env::remove_var("NFPROGRESS_DATA_DIR");
        }
        fs::remove_dir_all(root).unwrap();
        result.unwrap();
    }

    #[test]
    fn linked_word_record_progress_survives_restart_and_repeat() {
        let _guard = DATA_DIR_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let root = std::env::temp_dir().join(format!("worta-record-word-{}", std::process::id()));
        let previous = std::env::var_os("NFPROGRESS_DATA_DIR");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        std::env::set_var("NFPROGRESS_DATA_DIR", &root);
        let result = (|| -> Result<(), String> {
            let (db, _) = open()?;
            db.execute("INSERT INTO projects(id,name,goal,infinite,unit,status,payload_json) VALUES('p','Book',100,0,'symbols','активен',?)", [serde_json::json!({"name":"Book","status":"активен","work_method":"app","unit":"symbols","goal":100,"total":0,"stages":[]}).to_string()]).map_err(|e| e.to_string())?;
            db.execute(
                "INSERT INTO project_order(project_id,position) VALUES('p',0)",
                [],
            )
            .map_err(|e| e.to_string())?;
            drop(db);
            let initial = serde_json::json!({"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"Initial"}]}]});
            let edited = serde_json::json!({"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"Edited manuscript"}]}]});
            save_document(DocumentSaveCommand {
                project_id: "p".into(),
                stage_id: None,
                content: initial.clone(),
                expected_heads: None,
            })?;
            let word = root.join("test.docx");
            fs::write(&word, build_docx(&initial)?).map_err(|e| e.to_string())?;
            bind_document_file(DocumentFileCommand {
                project_id: "p".into(),
                stage_id: None,
                path: word.to_string_lossy().into_owned(),
            })?;
            let saved = save_document(DocumentSaveCommand {
                project_id: "p".into(),
                stage_id: None,
                content: edited.clone(),
                expected_heads: None,
            })?;
            let recorded = record_document_progress(DocumentProgressCommand {
                project_id: "p".into(),
                stage_id: None,
                content: Some(edited.clone()),
                expected_heads: Some(saved["expected_heads"].clone()),
            })?;
            assert!(recorded["progress"].is_object());
            let repeat = record_document_progress(DocumentProgressCommand {
                project_id: "p".into(),
                stage_id: None,
                content: Some(edited.clone()),
                expected_heads: Some(recorded["document"]["expected_heads"].clone()),
            })?;
            assert!(repeat["progress"].is_null());
            let reopened = get_document(DocumentScope {
                project_id: "p".into(),
                stage_id: None,
                expected_heads: None,
            })?;
            assert_eq!(reopened["content"], edited);
            assert_eq!(
                symbols(&parse_docx(&fs::read(&word).map_err(|e| e.to_string())?)?.0),
                symbols(&initial)
            );
            let db = rusqlite::Connection::open(root.join("nfprogress.db"))
                .map_err(|e| e.to_string())?;
            let count: i64 = db
                .query_row(
                    "SELECT COUNT(*) FROM progress_entries WHERE project_id='p'",
                    [],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            assert_eq!(count, 1);
            drop(db);
            let proposal = resolve_external_binding(ExternalDecisionCommand {
                project_id: "p".into(),
                stage_id: None,
                choice: "compare".into(),
            })?;
            assert_eq!(proposal["sync_state"], "external_proposal");
            resolve_external_binding(ExternalDecisionCommand {
                project_id: "p".into(),
                stage_id: None,
                choice: "cloud".into(),
            })?;
            assert_eq!(
                symbols(&parse_docx(&fs::read(&word).map_err(|e| e.to_string())?)?.0),
                symbols(&edited)
            );
            fs::rename(&word, root.join("missing.docx")).map_err(|e| e.to_string())?;
            assert!(resolve_external_binding(ExternalDecisionCommand {
                project_id: "p".into(),
                stage_id: None,
                choice: "cloud".into()
            })
            .is_err());
            assert_eq!(
                get_document(DocumentScope {
                    project_id: "p".into(),
                    stage_id: None,
                    expected_heads: None
                })?["content"],
                edited
            );
            Ok(())
        })();
        if let Some(value) = previous {
            std::env::set_var("NFPROGRESS_DATA_DIR", value);
        } else {
            std::env::remove_var("NFPROGRESS_DATA_DIR");
        }
        let _ = fs::remove_dir_all(&root);
        result.unwrap();
    }

    #[test]
    fn linked_word_self_write_is_immediately_reported_as_synced() {
        let _env_guard = DATA_DIR_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = std::env::temp_dir().join(format!(
            "nfprogress-linked-word-self-write-{}",
            std::process::id()
        ));
        let previous_root = std::env::var_os("NFPROGRESS_DATA_DIR");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        std::env::set_var("NFPROGRESS_DATA_DIR", &root);

        let result = (|| -> Result<(), String> {
            let (connection, _) = open()?;
            connection
                .execute(
                    "INSERT INTO projects(id,name,goal,infinite,unit,status,payload_json) VALUES('p1','Book',100,0,'symbols','активен',?)",
                    [serde_json::json!({"name":"Book","status":"активен","work_method":"app","stages":[]}).to_string()],
                )
                .map_err(|error| error.to_string())?;
            connection
                .execute(
                    "INSERT INTO project_order(project_id,position) VALUES('p1',0)",
                    [],
                )
                .map_err(|error| error.to_string())?;
            drop(connection);

            let path = root.join("linked.docx");
            fs::write(
                &path,
                build_docx(&serde_json::json!({"type":"doc","content":[{"type":"paragraph"}]}))?,
            )
            .map_err(|error| error.to_string())?;
            let content = serde_json::json!({
                "type": "doc",
                "content": [{
                    "type": "paragraph",
                    "attrs": {"lineHeight": "1.5"},
                    "content": [{"type": "text", "text": "before\tafter"}]
                }]
            });
            save_document(DocumentSaveCommand {
                project_id: "p1".into(),
                stage_id: None,
                content: content.clone(),
                expected_heads: None,
            })?;
            bind_document_file(DocumentFileCommand {
                project_id: "p1".into(),
                stage_id: None,
                path: path.to_string_lossy().into_owned(),
            })?;
            let written = resolve_external_binding(ExternalDecisionCommand {
                project_id: "p1".into(),
                stage_id: None,
                choice: "cloud".into(),
            })?;
            let external = read_external(DocumentScope {
                project_id: "p1".into(),
                stage_id: None,
                expected_heads: None,
            })?;

            assert_eq!(written["sync_state"], "synced");
            let written_hash = written["last_synced_hash"]
                .as_str()
                .ok_or_else(|| "Missing written hash".to_string())?
                .to_string();
            assert_eq!(external.state, "synced");
            assert!(external.content_base64.is_none());
            assert!(external.hash.is_none());

            fs::write(
                &path,
                build_docx(&serde_json::json!({
                    "type":"doc",
                    "content":[{"type":"paragraph","content":[{"type":"text","text":"External edit"}]}]
                }))?,
            )
            .map_err(|error| error.to_string())?;
            let changed = read_external(DocumentScope {
                project_id: "p1".into(),
                stage_id: None,
                expected_heads: None,
            })?;
            let observed_hash = changed
                .hash
                .clone()
                .ok_or_else(|| "Missing observed hash".to_string())?;
            assert_eq!(changed.state, "external_changed");
            assert!(changed.content_base64.is_some());

            let pending = get_document(DocumentScope {
                project_id: "p1".into(),
                stage_id: None,
                expected_heads: None,
            })?;
            assert_eq!(pending["last_external_hash"], observed_hash);
            assert_eq!(pending["last_synced_hash"], written_hash);

            let unsafe_overwrite = write_document_word_content("p1".into(), None, content.clone());
            assert!(unsafe_overwrite.is_err());

            let repeated = read_external(DocumentScope {
                project_id: "p1".into(),
                stage_id: None,
                expected_heads: None,
            })?;
            assert_eq!(repeated.state, "external_changed");
            assert!(repeated.content_base64.is_some());
            Ok(())
        })();

        if let Some(previous_root) = previous_root {
            std::env::set_var("NFPROGRESS_DATA_DIR", previous_root);
        } else {
            std::env::remove_var("NFPROGRESS_DATA_DIR");
        }
        let _ = fs::remove_dir_all(&root);
        result.unwrap();
    }

    #[test]
    fn accepted_word_conflict_does_not_reappear_after_test_profile_restart() {
        let _env_guard = DATA_DIR_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let real_root = std::env::temp_dir().join(format!(
            "nfprogress-word-conflict-restart-{}",
            std::process::id()
        ));
        let test_root = real_root.join("test_data");
        let previous_root = std::env::var_os("NFPROGRESS_DATA_DIR");
        let _ = fs::remove_dir_all(&real_root);
        fs::create_dir_all(&test_root).unwrap();

        let real_connection =
            crate::sqlite::open_database(&real_root.join("nfprogress.db")).unwrap();
        insert_app_project(&real_connection, "p1");
        real_connection.execute(
            "INSERT INTO documents(id,scope_key,project_id,title,content_json,content_format,revision,extensions_json) VALUES('real-document','p1:project','p1','Book',?1,'tiptap-json/v1',0,'{}')",
            [serde_json::json!({"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"real profile"}]}]}).to_string()],
        ).unwrap();
        drop(real_connection);

        let test_connection =
            crate::sqlite::open_database(&test_root.join("nfprogress.db")).unwrap();
        insert_app_project(&test_connection, "p1");
        drop(test_connection);
        std::env::set_var("NFPROGRESS_DATA_DIR", &test_root);

        let result = (|| -> Result<(), String> {
            let initial = serde_json::json!({
                "type":"doc",
                "content":[{"type":"paragraph","content":[{"type":"text","text":"initial test"}]}]
            });
            let accepted = serde_json::json!({
                "type":"doc",
                "content":[{"type":"paragraph","content":[{"type":"text","text":"accepted test version"}]}]
            });
            let word_path = test_root.join("linked.docx");
            fs::write(&word_path, build_docx(&initial)?).map_err(|error| error.to_string())?;
            save_document(DocumentSaveCommand {
                project_id: "p1".into(),
                stage_id: None,
                content: initial.clone(),
                expected_heads: None,
            })?;
            bind_document_file(DocumentFileCommand {
                project_id: "p1".into(),
                stage_id: None,
                path: word_path.to_string_lossy().into_owned(),
            })?;
            resolve_external_binding(ExternalDecisionCommand {
                project_id: "p1".into(),
                stage_id: None,
                choice: "cloud".into(),
            })?;

            save_document(DocumentSaveCommand {
                project_id: "p1".into(),
                stage_id: None,
                content: serde_json::json!({
                    "type":"doc",
                    "content":[{"type":"paragraph","content":[{"type":"text","text":"local edit"}]}]
                }),
                expected_heads: None,
            })?;
            fs::write(&word_path, build_docx(&accepted)?).map_err(|error| error.to_string())?;
            let conflict = read_external(DocumentScope {
                project_id: "p1".into(),
                stage_id: None,
                expected_heads: None,
            })?;
            assert_eq!(conflict.state, "conflict");
            let accepted_hash = conflict
                .hash
                .ok_or_else(|| "Missing conflict hash".to_string())?;
            let resolved = accept_external(DocumentExternalAcceptCommand {
                project_id: "p1".into(),
                stage_id: None,
                content: accepted.clone(),
                source_hash: accepted_hash.clone(),
                expected_heads: None,
            })?;
            assert_eq!(resolved["content"], accepted);
            assert_eq!(resolved["last_synced_hash"], accepted_hash);

            // A normal next startup has no transfer request and must open the
            // existing test SQLite profile without rebuilding it from real.
            crate::profile_transfer::process_pending(&test_root)?;
            crate::prepare_startup_storage(&test_root)?;

            let reopened = get_document(DocumentScope {
                project_id: "p1".into(),
                stage_id: None,
                expected_heads: None,
            })?;
            assert_eq!(reopened["content"], accepted);
            assert_eq!(reopened["last_synced_hash"], accepted_hash);
            assert_eq!(reopened["sync_state"], "synced");
            let unchanged_word = read_external(DocumentScope {
                project_id: "p1".into(),
                stage_id: None,
                expected_heads: None,
            })?;
            assert_eq!(unchanged_word.state, "synced");
            assert!(unchanged_word.hash.is_none());

            let real_content: String = rusqlite::Connection::open(real_root.join("nfprogress.db"))
                .map_err(|error| error.to_string())?
                .query_row(
                    "SELECT content_json FROM documents WHERE id='real-document'",
                    [],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())?;
            assert!(real_content.contains("real profile"));
            Ok(())
        })();

        if let Some(previous_root) = previous_root {
            std::env::set_var("NFPROGRESS_DATA_DIR", previous_root);
        } else {
            std::env::remove_var("NFPROGRESS_DATA_DIR");
        }
        let _ = fs::remove_dir_all(&real_root);
        result.unwrap();
    }

    #[test]
    fn docx_archive_rejects_traversal_entry() {
        let mut output = Cursor::new(Vec::new());
        let mut archive = zip::ZipWriter::new(&mut output);
        archive
            .start_file("../document.xml", SimpleFileOptions::default())
            .unwrap();
        archive.write_all(b"bad").unwrap();
        archive.finish().unwrap();
        assert!(zip_safety(&output.into_inner()).is_err());
    }

    #[test]
    fn rtf_counter_ignores_control_words() {
        assert_eq!(count_rtf_symbols(br"{\rtf1\ansi Hello \b world}"), 11);
    }

    #[test]
    fn legacy_documents_migration_is_idempotent_and_sqlite_wins() {
        let root = std::env::temp_dir().join(format!(
            "nfprogress-document-migration-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("documents.json"),
            serde_json::json!({
                "p1:project": {
                    "id": "legacy-document",
                    "project_id": "p1",
                    "content": {"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"old"}]}]},
                    "legacy_flag": true
                }
            })
            .to_string(),
        )
        .unwrap();
        let mut connection = rusqlite::Connection::open_in_memory().unwrap();
        crate::sqlite::apply_migrations(&connection).unwrap();
        connection
            .execute(
                "INSERT INTO projects(id,name,goal,infinite,unit,status,payload_json) VALUES('p1','Book',100,0,'symbols','активен',?)",
                [serde_json::json!({"name":"Book","status":"активен","work_method":"app","stages":[]}).to_string()],
            )
            .unwrap();
        migrate_legacy_documents(&mut connection, &root).unwrap();
        connection
            .execute(
                "UPDATE documents SET content_json=? WHERE id='legacy-document'",
                [serde_json::json!({"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"new"}]}]}).to_string()],
            )
            .unwrap();
        fs::write(
            root.join("documents.json"),
            serde_json::json!({"p1:project":{"project_id":"p1","content":{"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"stale"}]}]}}}).to_string(),
        )
        .unwrap();
        migrate_legacy_documents(&mut connection, &root).unwrap();
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM documents", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(connection.query_row("SELECT content_json FROM documents WHERE id='legacy-document'", [], |row| row.get::<_, String>(0)).unwrap(), serde_json::json!({"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"new"}]}]}).to_string());
        assert_eq!(connection.query_row("SELECT json_extract(extensions_json,'$.legacy_flag') FROM documents WHERE id='legacy-document'", [], |row| row.get::<_, i64>(0)).unwrap(), 1);
        let _ = fs::remove_dir_all(root);
    }
}

fn normalize_total(symbols: f64, unit: &str) -> Result<f64, String> {
    let factor = match unit {
        "symbols" => 1.0,
        "A4" => 1800.0,
        "author_list" => 40000.0,
        "ficbook_pages" => 4500.0,
        _ => return Err("Неизвестная единица прогресса.".to_string()),
    };
    let value = symbols / factor;
    Ok(if unit == "author_list" {
        value
    } else {
        value.ceil()
    })
}

fn record_sync_progress(
    connection: &mut rusqlite::Connection,
    project_id: &str,
    stage_id: Option<&str>,
    total: f64,
    source_hash: &str,
    created_at: &str,
    proposal: Option<&Value>,
) -> Result<(Value, Value), String> {
    record_document_or_sync_progress(
        connection,
        project_id,
        stage_id,
        total,
        source_hash,
        created_at,
        proposal,
        None,
        None,
    )
    .map(|(project, entry, _)| (project, entry))
}

fn record_document_or_sync_progress(
    connection: &mut rusqlite::Connection,
    project_id: &str,
    stage_id: Option<&str>,
    total: f64,
    source_hash: &str,
    created_at: &str,
    proposal: Option<&Value>,
    document_revision: Option<i64>,
    legacy_binding_id: Option<&str>,
) -> Result<
    (
        Value,
        Value,
        Result<Option<crate::game::GameCommandResponse>, String>,
    ),
    String,
> {
    let mut tx = connection
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    if let Some(proposal) = proposal {
        external_reattach::assert_progress_proposal(&tx, project_id, stage_id, proposal)?;
    }
    let document_id = document_id_for_scope(&tx, project_id, stage_id)?;
    let local_document =
        document_revision.is_some() && crate::daily_goals::local_project(&tx, project_id)?;
    if let Some(revision) = document_revision {
        let current: i64 = tx
            .query_row(
                "SELECT revision FROM documents WHERE id=?1",
                [&document_id],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;
        if revision != current {
            return Err("Не удалось сохранить".to_string());
        }
    }
    let last: Option<String> = if let Some(id) = legacy_binding_id {
        tx.query_row(
            "SELECT content_hash FROM project_bindings WHERE id=?1 AND project_id=?2 AND stage_id IS ?3",
            params![id, project_id, stage_id],
            |row| row.get(0),
        ).map_err(|_| "sync_source_stale")?
    } else {
        tx.query_row(
            "SELECT last_external_hash FROM document_bindings WHERE document_id=?",
            [document_id.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .flatten()
    };
    if proposal.is_none() && last.as_deref() == Some(source_hash) {
        tx.commit().map_err(|e| e.to_string())?;
        return Ok((
            crate::project_payload(connection, project_id)?,
            Value::Null,
            Ok(None),
        ));
    }
    let (entity_id, payload, _) = project_entity(&tx, project_id, stage_id)?;
    let previous = payload.get("total").and_then(Value::as_f64).unwrap_or(0.0);
    let unit = payload
        .get("unit")
        .and_then(Value::as_str)
        .unwrap_or("symbols");
    let factor = match unit {
        "symbols" => 1.0,
        "A4" => 1800.0,
        "author_list" => 40000.0,
        "ficbook_pages" => 4500.0,
        _ => return Err("Неизвестная единица прогресса.".to_string()),
    };
    let delta = (total - previous) * factor;
    if delta.abs() < 0.009 {
        if let Some(id) = legacy_binding_id {
            update_legacy_sync_hash(&tx, id, source_hash)?;
        } else if proposal.is_none() {
            tx.execute("UPDATE document_bindings SET last_external_hash=?,last_synced_at=?,sync_state='synced' WHERE document_id=?",params![source_hash,now(),document_id]).map_err(|e|e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())?;
        return Ok((
            crate::project_payload(connection, project_id)?,
            Value::Null,
            Ok(None),
        ));
    }
    let entry_id = crate::new_note_id()?;
    let goal = payload.get("goal").and_then(Value::as_f64).unwrap_or(0.0);
    let goal_symbols = goal * factor;
    let added_progress = if goal_symbols <= 0.0 {
        0.0
    } else {
        delta / goal_symbols * 100.0
    };
    let mut entry = serde_json::json!({"id":entry_id,"new_total":total,"new_total_symbols":total*factor,"added":total-previous,"added_symbols":delta,"added_progress":added_progress,"created_at":created_at});
    let day = local_document
        .then(|| crate::streaks::logical_writing_day_at(&tx, created_at))
        .transpose()?;
    if let Some(day) = &day {
        entry["writing_day"] = serde_json::json!(day);
    }
    let mut next = payload.clone();
    let mut entries = next
        .get("progress_entries")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    entries.push(entry.clone());
    next["progress_entries"] = Value::Array(entries);
    next["total"] = total.into();
    if local_document {
        next["progress"] = serde_json::json!(if goal > 0.0 {
            total / goal * 100.0
        } else {
            0.0
        });
        next["remaining"] = if goal > 0.0 {
            serde_json::json!((goal - total).max(0.0))
        } else {
            Value::Null
        };
    }
    next["updated_at"] = created_at.into();

    let table = if stage_id.is_some() {
        "stages"
    } else {
        "projects"
    };
    let heads = crate::progress_sync::local_heads(&tx, project_id, stage_id)?;
    let handled = crate::progress_sync::normal(
        &tx,
        project_id,
        stage_id,
        &entry,
        heads.as_deref(),
        created_at,
    )?;
    if !handled {
        tx.execute("INSERT INTO progress_entries(id,project_id,stage_id,created_at,added_symbols,added_progress,payload_json) VALUES(?,?,?,?,?,?,?)",params![entry_id,project_id,stage_id,created_at,delta,added_progress,entry.to_string()]).map_err(|e|e.to_string())?;
        let pos: i64 = tx
            .query_row(
                "SELECT COALESCE(MAX(position),-1)+1 FROM progress_order",
                [],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT INTO progress_order(entry_id,position) VALUES(?,?)",
            params![entry_id, pos],
        )
        .map_err(|e| e.to_string())?;
        tx.execute(
            &format!("UPDATE {table} SET updated_at=?,payload_json=? WHERE id=?"),
            params![created_at, next.to_string(), entity_id],
        )
        .map_err(|e| e.to_string())?;
        if stage_id.is_some() {
            crate::refresh_project_totals_in_transaction(&tx, project_id)?
        }
    }
    let local_day = day.filter(|_| !handled);
    let rewards = if let Some(day) = &local_day {
        crate::daily_goals::record_from_storage(
            &tx,
            project_id,
            stage_id,
            day,
            created_at,
            delta > 0.0,
        )?
    } else {
        Vec::new()
    };
    let event_id = format!("sync-progress:{document_id}:{source_hash}");
    tx.execute("INSERT OR IGNORE INTO domain_events(event_id,event_type,project_id,stage_id,progress_id,delta_symbols,context_json,created_at) VALUES(?,'ProgressAdded',?,?,?,?,?,?)",params![event_id,project_id,stage_id,entry_id,delta,serde_json::json!({"source":"trusted_document_sync","version":1,"key":format!("document:{}:{}",project_id,stage_id.unwrap_or("project"))}).to_string(),created_at]).map_err(|e|e.to_string())?;
    let feedback = if local_day.is_some() {
        crate::game::settle_local_progress(&mut tx, &event_id, project_id, delta, rewards)
    } else {
        Ok(None)
    };
    if let Some(id) = legacy_binding_id {
        update_legacy_sync_hash(&tx, id, source_hash)?;
    } else if proposal.is_none() {
        tx.execute("UPDATE document_bindings SET last_external_hash=?,last_synced_at=?,last_synced_revision=(SELECT revision FROM documents WHERE id=?),sync_state='synced',expected_external_hash=? WHERE document_id=?",params![source_hash,now(),document_id,source_hash,document_id]).map_err(|e|e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok((
        crate::project_payload(connection, project_id)?,
        entry,
        feedback,
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExternalDecisionCommand {
    pub project_id: String,
    pub stage_id: Option<String>,
    pub choice: String,
}
pub fn resolve_external_binding(command: ExternalDecisionCommand) -> Result<Value, String> {
    let mut db = open()?.0;
    validate_scope(&db, &command.project_id, command.stage_id.as_deref())?;
    external_reattach::resolve(
        &mut db,
        &command.project_id,
        command.stage_id.as_deref(),
        &command.choice,
    )
}
pub fn poll_external_binding(scope: DocumentScope) -> Result<Value, String> {
    let mut db = open()?.0;
    validate_scope(&db, &scope.project_id, scope.stage_id.as_deref())?;
    external_reattach::poll(&mut db, &scope.project_id, scope.stage_id.as_deref())
}

pub fn confirm_external_progress(command: SyncScopeCommand) -> Result<SyncRunResult, String> {
    let mut db = open()?.0;
    validate_sync_scope(&db, &command.project_id, command.stage_id.as_deref())?;
    external_reattach::progress_proposal(
        &mut db,
        &command.project_id,
        command.stage_id.as_deref(),
        true,
    )
}

pub fn refresh_external_progress(command: SyncScopeCommand) -> Result<SyncRunResult, String> {
    let mut db = open()?.0;
    validate_sync_scope(&db, &command.project_id, command.stage_id.as_deref())?;
    if !external_reattach::connected(&db, &command.project_id)? {
        drop(db);
        return run_sync(command);
    }
    let id =
        external_reattach::project_binding_id(&command.project_id, command.stage_id.as_deref());
    let raw: String = db
        .query_row(
            "SELECT payload_json FROM project_bindings WHERE id=?1",
            [&id],
            |r| r.get(0),
        )
        .map_err(|_| "external_reattach_required")?;
    let mut local: Value = serde_json::from_str(&raw).map_err(|_| "external_invalid_evidence")?;
    local
        .as_object_mut()
        .ok_or("external_invalid_evidence")?
        .remove("progress_reattach_v1");
    db.execute(
        "UPDATE project_bindings SET payload_json=?1 WHERE id=?2",
        params![local.to_string(), id],
    )
    .map_err(|e| e.to_string())?;
    external_reattach::progress_proposal(
        &mut db,
        &command.project_id,
        command.stage_id.as_deref(),
        false,
    )
}
