use std::collections::HashSet;
use std::fmt::Write as _;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use rusqlite::OptionalExtension;
use serde::de::{self, Visitor};
use serde::{Deserialize, Serialize};
use tauri::{Emitter, Manager, PhysicalPosition, PhysicalSize, RunEvent, State};

mod account_binding;
mod account_catalog;
mod account_sync;
#[cfg(test)]
mod c15_headless_test_bridge;
mod content_note_sync;
mod content_note_writer;
mod daily_goals;
mod diagnostics;
mod document_codec;
mod document_sync;
mod documents;
mod frame_compression;
mod game;
mod game_codec;
mod game_control;
mod game_migration;
mod game_projection;
mod game_sync;
mod game_transport;
mod game_writer;
mod map_codec;
mod map_sync;
mod mindmap;
#[allow(dead_code)]
mod note_sync;
mod note_sync_plaintext;
mod profile_transfer;
mod progress_codec;
mod progress_sync;
mod project_cover_sync;
#[allow(dead_code)]
mod project_metadata_sync;
#[allow(dead_code)]
mod project_repository;
mod sqlite;
mod stage_sync;
mod streaks;

use project_repository::{
    ProgressRecord, ProjectAggregate, ProjectMetadataUpdate, ProjectRecord, ProjectsRepository,
    StageRecord, StageUpdate,
};

#[derive(Serialize)]
struct SqliteEntityRow {
    id: String,
    project_id: Option<String>,
    name: Option<String>,
    goal: Option<f64>,
    infinite: i64,
    unit: String,
    status: String,
    created_at: Option<String>,
    updated_at: Option<String>,
    payload_json: String,
}

#[derive(Serialize)]
struct SqliteProgressRow {
    id: String,
    project_id: String,
    stage_id: Option<String>,
    created_at: Option<String>,
    added_symbols: Option<f64>,
    added_progress: Option<f64>,
    payload_json: String,
}

#[derive(Serialize)]
struct SqliteProjectReadModel {
    mirror_status: String,
    project_order: Vec<String>,
    projects: Vec<SqliteEntityRow>,
    stages: Vec<SqliteEntityRow>,
    progress_entries: Vec<SqliteProgressRow>,
}

fn overlay_game_streak_state(
    rows: &mut [SqliteEntityRow],
    game_state: &serde_json::Value,
    is_stage: bool,
    logical_day: &str,
) {
    let Some(states) = game_state
        .get("project_game_state")
        .and_then(serde_json::Value::as_object)
    else {
        return;
    };
    for row in rows {
        let key = if is_stage {
            format!(
                "stage:{}:{}",
                row.project_id.as_deref().unwrap_or_default(),
                row.id
            )
        } else {
            format!("project:{}", row.id)
        };
        let Some(streak) = states.get(&key).and_then(serde_json::Value::as_object) else {
            continue;
        };
        let Ok(mut payload) = serde_json::from_str::<serde_json::Value>(&row.payload_json) else {
            continue;
        };
        let Some(payload) = payload.as_object_mut() else {
            continue;
        };
        let canonical_status = streaks::canonical_local_status(streak, logical_day, payload);
        for field in [
            "streaks",
            "max_streak",
            "streak_status",
            "last_streak_bonus",
            "last_streak_lost_date",
            "freezes",
        ] {
            if field == "streak_status" {
                payload.insert(field.to_string(), canonical_status.clone().into());
            } else if let Some(value) = streak.get(field) {
                payload.insert(field.to_string(), value.clone());
            }
        }
        if let Ok(serialized) = serde_json::to_string(&payload) {
            row.payload_json = serialized;
        }
    }
}

pub(crate) fn open_projects_database() -> Result<rusqlite::Connection, String> {
    sqlite::open_database(&sqlite_data_root()?.join("nfprogress.db"))
        .map_err(|error| error.to_string())
}

fn initialize_fresh_desktop_database(
    connection: &rusqlite::Connection,
    data_root: &Path,
) -> Result<(), String> {
    let legacy_files = ["data.pkl", "gamer.pkl", "settings.pkl", "documents.json"];
    if legacy_files
        .iter()
        .any(|name| data_root.join(name).is_file())
    {
        let prepared: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM storage_ownership WHERE owner='sqlite'",
                [],
                |row| row.get(0),
            )
            .map_err(|error| format!("migration_required: {error}"))?;
        let marker: Option<String> = connection
            .query_row(
                "SELECT value_json FROM game_metadata WHERE key='migration_status'",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| format!("migration_required: {error}"))?;
        let documents_complete: Option<String> = connection
            .query_row(
                "SELECT value_json FROM document_metadata WHERE key='documents_json_migration'",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| format!("migration_required: {error}"))?;
        let markers_absent = marker.is_none() && documents_complete.is_none();
        let marker_ready = marker
            .as_deref()
            .and_then(|value| serde_json::from_str::<serde_json::Value>(value).ok())
            .and_then(|value| {
                value
                    .get("status")
                    .and_then(|status| status.as_str())
                    .map(str::to_owned)
            })
            == Some("ready_for_tauri".to_string());
        let documents_ready = documents_complete
            .as_deref()
            .and_then(|value| serde_json::from_str::<serde_json::Value>(value).ok())
            .and_then(|value| {
                value
                    .get("status")
                    .and_then(|status| status.as_str())
                    .map(str::to_owned)
            })
            == Some("complete".to_string());
        if prepared != 4 || !(markers_absent || (marker_ready && documents_ready)) {
            return Err("migration_required: legacy_data_detected".to_string());
        }
        return Ok(());
    }
    let populated: i64 = connection
        .query_row(
            "SELECT (SELECT COUNT(*) FROM projects) + (SELECT COUNT(*) FROM stages) + (SELECT COUNT(*) FROM progress_entries) + (SELECT COUNT(*) FROM notes) + (SELECT COUNT(*) FROM settings) + (SELECT COUNT(*) FROM game_state) + (SELECT COUNT(*) FROM documents)",
            [],
            |row| row.get(0),
        )
        .map_err(|error| format!("Не удалось проверить новую SQLite базу: {error}"))?;
    if populated != 0 {
        return Ok(());
    }

    connection
        .execute_batch(&format!(
            "BEGIN IMMEDIATE;
             UPDATE storage_ownership SET owner='sqlite', schema_version={schema_version}, updated_at=datetime('now');
             INSERT INTO mirror_state(id,source_format,source_schema_version,last_full_sync_at,last_successful_sync_at,sync_status,last_error)
             VALUES(1,'sqlite','{schema_version}',datetime('now'),datetime('now'),'healthy',NULL)
             ON CONFLICT(id) DO UPDATE SET source_format='sqlite',source_schema_version='{schema_version}',last_full_sync_at=datetime('now'),last_successful_sync_at=datetime('now'),sync_status='healthy',last_error=NULL;
             INSERT INTO game_state(id,schema_version,payload_json,updated_at)
             VALUES(1,2,'{{\"gamer\":{{}},\"game\":{{}}}}',datetime('now'))
             ON CONFLICT(id) DO NOTHING;
             COMMIT;",
            schema_version = sqlite::CURRENT_SCHEMA_VERSION,
        ))
        .map_err(|error| format!("Не удалось инициализировать новую SQLite базу: {error}"))
}

#[tauri::command]
fn process_game_events() -> Result<game::ProcessSummary, String> {
    let mut connection = open_projects_database()?;
    let owner: String = connection
        .query_row(
            "SELECT owner FROM storage_ownership WHERE subsystem='game'",
            [],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    if owner != "sqlite" {
        return Err("Игра ещё не переведена в SQLite authoritative storage.".to_string());
    }
    game::process_pending_events(&mut connection, 100)
}

#[tauri::command]
fn list_documents() -> Result<Vec<serde_json::Value>, String> {
    documents::list_documents()
}

#[tauri::command]
fn get_document(scope: documents::DocumentScope) -> Result<serde_json::Value, String> {
    documents::get_document(scope)
}

#[tauri::command]
fn save_document(command: documents::DocumentSaveCommand) -> Result<serde_json::Value, String> {
    documents::save_document(command)
}

#[tauri::command]
fn record_document_progress(
    command: documents::DocumentProgressCommand,
) -> Result<serde_json::Value, String> {
    documents::record_document_progress(command)
}

#[tauri::command]
fn rename_document(command: documents::DocumentRenameCommand) -> Result<serde_json::Value, String> {
    documents::rename_document(command)
}

#[tauri::command]
fn delete_document(scope: documents::DocumentScope) -> Result<(), String> {
    documents::delete_document(scope)
}

#[tauri::command]
fn resolve_external_binding(
    command: documents::ExternalDecisionCommand,
) -> Result<serde_json::Value, String> {
    documents::resolve_external_binding(command)
}
#[tauri::command]
fn poll_external_binding(scope: documents::DocumentScope) -> Result<serde_json::Value, String> {
    documents::poll_external_binding(scope)
}

#[tauri::command]
fn bind_document_file(
    command: documents::DocumentFileCommand,
) -> Result<serde_json::Value, String> {
    documents::bind_document_file(command)
}

#[tauri::command]
fn write_document_word(
    command: documents::DocumentWordWriteCommand,
) -> Result<serde_json::Value, String> {
    documents::write_document_word(command)
}

#[tauri::command]
fn read_document_external(
    scope: documents::DocumentScope,
) -> Result<documents::ExternalDocumentResult, String> {
    documents::read_external(scope)
}

#[tauri::command]
fn accept_document_external(
    command: documents::DocumentExternalAcceptCommand,
) -> Result<serde_json::Value, String> {
    documents::accept_external(command)
}

#[tauri::command]
fn count_word_document(command: documents::WordImportCommand) -> Result<usize, String> {
    documents::count_word(command)
}

#[tauri::command]
fn parse_word_document(
    command: documents::WordImportCommand,
) -> Result<documents::WordImportResult, String> {
    documents::parse_word(command)
}

#[tauri::command]
fn import_word_document(
    command: documents::WordImportCommand,
) -> Result<documents::WordImportResult, String> {
    documents::import_word(command)
}

#[tauri::command]
fn export_word_document(command: documents::WordExportCommand) -> Result<(), String> {
    documents::export_word(command)
}

#[tauri::command]
fn write_document_word_content(
    project_id: String,
    stage_id: Option<String>,
    content: serde_json::Value,
) -> Result<serde_json::Value, String> {
    documents::write_document_word_content(project_id, stage_id, content)
}

#[tauri::command]
fn inspect_scrivener(
    command: documents::ScrivenerInspectCommand,
) -> Result<Vec<documents::ScrivenerItem>, String> {
    documents::inspect_scrivener(command)
}

#[tauri::command]
fn configure_document_sync(
    command: documents::SyncConfigureCommand,
) -> Result<documents::SyncSummary, String> {
    documents::configure_sync(command)
}

#[tauri::command]
fn get_document_sync(
    command: documents::SyncScopeCommand,
) -> Result<documents::SyncSummary, String> {
    documents::get_sync(command)
}

#[tauri::command]
fn get_project_document_syncs(project_id: String) -> Result<serde_json::Value, String> {
    documents::get_project_syncs(project_id)
}

#[tauri::command]
fn remove_document_sync(
    command: documents::SyncScopeCommand,
) -> Result<documents::SyncSummary, String> {
    documents::remove_sync(command)
}

#[tauri::command]
fn refresh_external_progress(
    project_id: String,
    stage_id: Option<String>,
) -> Result<documents::SyncRunResult, String> {
    documents::refresh_external_progress(documents::SyncScopeCommand {
        project_id,
        stage_id,
    })
}

#[tauri::command]
fn confirm_external_progress(
    project_id: String,
    stage_id: Option<String>,
) -> Result<documents::SyncRunResult, String> {
    documents::confirm_external_progress(documents::SyncScopeCommand {
        project_id,
        stage_id,
    })
}

#[tauri::command]
fn run_document_sync(
    command: documents::SyncScopeCommand,
) -> Result<documents::SyncRunResult, String> {
    documents::run_sync(command)
}

#[tauri::command]
fn run_all_document_sync() -> Result<documents::SyncBatchResult, String> {
    documents::run_all_sync()
}

#[tauri::command]
fn run_project_document_syncs(project_id: String) -> Result<documents::SyncBatchResult, String> {
    documents::run_project_syncs(project_id)
}

// Explicit Game commands.  These are deliberately one command per use case;
// the frontend cannot inject an arbitrary operation name or JSON payload.
#[tauri::command]
fn game_state() -> Result<serde_json::Value, game::GameError> {
    game::GameApplicationService::state()
}

#[tauri::command]
fn game_notifications() -> Result<serde_json::Value, game::GameError> {
    game::GameApplicationService::notifications()
}

#[tauri::command]
fn mark_game_notification_read(
    payload: game::NotificationRequest,
) -> Result<serde_json::Value, game::GameError> {
    game::GameApplicationService::mark_notification(payload.notification_id, false)
}

#[tauri::command]
fn mark_all_game_notifications_read() -> Result<serde_json::Value, game::GameError> {
    game::GameApplicationService::mark_notification(String::new(), true)
}

fn diagnostic_path() -> Result<PathBuf, String> {
    Ok(sqlite_data_root()?.join("diagnostics").join("support.db"))
}
async fn diagnostic_task<T: Send + 'static>(
    action: impl FnOnce(PathBuf) -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let path = diagnostic_path()?;
    tauri::async_runtime::spawn_blocking(move || action(path))
        .await
        .map_err(|_| "diagnostic_storage_unavailable".to_string())?
}
#[tauri::command]
async fn append_diagnostics(events: Vec<serde_json::Value>) -> Result<(), String> {
    diagnostic_task(move |path| diagnostics::append(&path, &events)).await
}
#[tauri::command]
async fn diagnostic_stats() -> Result<serde_json::Value, String> {
    diagnostic_task(|path| diagnostics::stats(&path)).await
}
#[tauri::command]
async fn diagnostic_text(copy: bool) -> Result<String, String> {
    diagnostic_task(move |path| diagnostics::text(&path, copy)).await
}
#[tauri::command]
async fn clear_diagnostics() -> Result<(), String> {
    diagnostic_task(|path| diagnostics::clear(&path)).await
}
#[tauri::command]
async fn export_diagnostics(path: String) -> Result<(), String> {
    diagnostic_task(move |source| diagnostics::export(&source, Path::new(&path))).await
}

#[tauri::command]
fn game_catalog() -> Result<serde_json::Value, game::GameError> {
    let state = game::GameApplicationService::state()?;
    Ok(state["shop"].clone())
}

#[tauri::command]
fn game_developer_state() -> Result<serde_json::Value, game::GameError> {
    game::GameApplicationService::developer_state()
}

#[tauri::command]
fn game_update_developer_profile(
    payload: game::DeveloperProfileRequest,
) -> Result<game::GameCommandResponse, game::GameError> {
    game::GameApplicationService::update_developer(payload)
}

#[tauri::command]
fn game_grant_developer_inventory_item(
    payload: game::InventoryRequest,
) -> Result<game::GameCommandResponse, game::GameError> {
    game::GameApplicationService::grant_inventory(payload)
}

#[tauri::command]
fn game_developer_streak_state() -> Result<serde_json::Value, game::GameError> {
    game::GameApplicationService::developer_streak_state()
}

#[tauri::command]
fn game_developer_restore_streak(
    payload: game::DeveloperStreakRequest,
    correlation_id: Option<String>,
) -> Result<game::GameCommandResponse, game::GameError> {
    let correlation = correlation_id
        .filter(|s| project_metadata_sync::uuid(s))
        .unwrap_or_else(|| project_metadata_sync::new_event_id().unwrap_or_default());
    let mut events = vec![
        diagnostics::native_streak_event("native_validation", &correlation, None),
        diagnostics::native_streak_event("native_attempt", &correlation, None),
    ];
    let result = game::GameApplicationService::developer_restore_streak(payload);
    let code = match &result {
        Ok(_) => None,
        Err(game::GameError::Validation(message))
            if message == "Невозможно восстановить стрик: нет сохранённой длины." =>
        {
            Some("streak_restore_no_history")
        }
        Err(game::GameError::Validation(_)) => Some("Validation"),
        Err(game::GameError::Database(_)) => Some("Database"),
        Err(game::GameError::NotFound(_)) => Some("NotFound"),
        Err(game::GameError::PrerequisiteMissing(_)) => Some("PrerequisiteMissing"),
        Err(game::GameError::InvalidState(_)) => Some("InvalidState"),
        Err(_) => Some("unknown_error"),
    };
    events.push(diagnostics::native_streak_event(
        if result.is_ok() {
            "native_succeeded"
        } else {
            "native_failed"
        },
        &correlation,
        code,
    ));
    if let Ok(path) = diagnostic_path() {
        let _ = diagnostics::append(&path, &events);
    } // Logging never changes the game result.
    result
}

#[tauri::command]
fn game_developer_create_streak_series(
    payload: game::DeveloperStreakRequest,
) -> Result<game::GameCommandResponse, game::GameError> {
    game::GameApplicationService::developer_create_streak_series(payload)
}

#[tauri::command]
fn game_start_writing_session(
    payload: game::WritingSessionRequest,
) -> Result<game::GameCommandResponse, game::GameError> {
    game::GameApplicationService::start_session(payload)
}
#[tauri::command]
fn game_finish_writing_session() -> Result<game::GameCommandResponse, game::GameError> {
    game::GameApplicationService::finish_session()
}
#[tauri::command]
fn game_cancel_writing_session() -> Result<game::GameCommandResponse, game::GameError> {
    game::GameApplicationService::cancel_session()
}
#[tauri::command]
fn game_select_daily_challenge(
    payload: game::DailyChallengeRequest,
) -> Result<game::GameCommandResponse, game::GameError> {
    game::GameApplicationService::select_daily(payload)
}
#[tauri::command]
fn game_start_weekly_challenge(
    payload: game::WeeklyChallengeRequest,
) -> Result<game::GameCommandResponse, game::GameError> {
    game::GameApplicationService::start_weekly(payload)
}
#[tauri::command]
fn game_activate_inspiration_ability(
    payload: game::AbilityRequest,
) -> Result<game::GameCommandResponse, game::GameError> {
    game::GameApplicationService::activate_inspiration(payload.ability_id)
}
#[tauri::command]
fn game_resolve_creative_event(
    payload: game::ChoiceRequest,
) -> Result<game::GameCommandResponse, game::GameError> {
    game::GameApplicationService::resolve_creative(payload)
}
#[tauri::command]
fn game_select_specialization(
    payload: game::SpecializationRequest,
) -> Result<game::GameCommandResponse, game::GameError> {
    game::GameApplicationService::select_specialization(payload.specialization_id)
}
#[tauri::command]
fn game_activate_specialization_ability() -> Result<game::GameCommandResponse, game::GameError> {
    game::GameApplicationService::activate_specialization()
}
#[tauri::command]
fn game_increase_skill(
    payload: game::SkillRequest,
) -> Result<game::GameCommandResponse, game::GameError> {
    game::GameApplicationService::increase_skill(payload)
}
#[tauri::command]
fn game_start_quest(
    payload: game::QuestRequest,
) -> Result<game::GameCommandResponse, game::GameError> {
    game::GameApplicationService::quest(payload.quest_id, true)
}
#[tauri::command]
fn game_abandon_quest(
    payload: game::QuestRequest,
) -> Result<game::GameCommandResponse, game::GameError> {
    game::GameApplicationService::quest(payload.quest_id, false)
}
#[tauri::command]
fn game_buy_item(
    payload: game::InventoryRequest,
) -> Result<game::GameCommandResponse, game::GameError> {
    game::GameApplicationService::inventory(payload.category, payload.item_id, payload.count, "buy")
}
#[tauri::command]
fn game_sell_item(
    payload: game::InventoryRequest,
) -> Result<game::GameCommandResponse, game::GameError> {
    game::GameApplicationService::inventory(
        payload.category,
        payload.item_id,
        payload.count,
        "sell",
    )
}
#[tauri::command]
fn game_use_item(
    payload: game::InventoryRequest,
) -> Result<game::GameCommandResponse, game::GameError> {
    game::GameApplicationService::inventory(payload.category, payload.item_id, payload.count, "use")
}
#[tauri::command]
fn game_apply_streak_freeze(
    payload: game::FreezeRequest,
) -> Result<game::GameCommandResponse, game::GameError> {
    game::GameApplicationService::freeze(payload)
}
#[tauri::command]
fn game_run_lottery() -> Result<game::GameCommandResponse, game::GameError> {
    game::GameApplicationService::run_lottery()
}
#[tauri::command]
fn game_create_custom_award(
    payload: game::CustomAwardRequest,
) -> Result<game::GameCommandResponse, game::GameError> {
    game::GameApplicationService::custom_award(payload)
}
#[tauri::command]
fn game_update_custom_award(
    payload: game::CustomAwardUpdateRequest,
) -> Result<game::GameCommandResponse, game::GameError> {
    game::GameApplicationService::update_custom_award(payload)
}
#[tauri::command]
fn game_delete_custom_award(
    payload: game::AwardIdRequest,
) -> Result<game::GameCommandResponse, game::GameError> {
    game::GameApplicationService::delete_custom_award(payload.award_id)
}
#[tauri::command]
fn game_buy_custom_award(
    payload: game::AwardCountRequest,
) -> Result<game::GameCommandResponse, game::GameError> {
    game::GameApplicationService::custom_inventory(payload.award_id, payload.count, "buy")
}
#[tauri::command]
fn game_sell_custom_award(
    payload: game::AwardCountRequest,
) -> Result<game::GameCommandResponse, game::GameError> {
    game::GameApplicationService::custom_inventory(payload.award_id, payload.count, "sell")
}
#[tauri::command]
fn game_use_custom_award(
    payload: game::AwardCountRequest,
) -> Result<game::GameCommandResponse, game::GameError> {
    game::GameApplicationService::custom_inventory(payload.award_id, payload.count, "use")
}
#[tauri::command]
fn game_preview_bank_product(
    payload: game::BankProductRequest,
) -> Result<game::GameCommandResponse, game::GameError> {
    game::GameApplicationService::preview_bank_product(payload)
}
#[tauri::command]
fn game_open_bank_credit(
    payload: game::BankProductRequest,
) -> Result<game::GameCommandResponse, game::GameError> {
    game::GameApplicationService::bank_open(payload, false)
}
#[tauri::command]
fn game_open_bank_deposit(
    payload: game::BankProductRequest,
) -> Result<game::GameCommandResponse, game::GameError> {
    game::GameApplicationService::bank_open(payload, true)
}
#[tauri::command]
fn game_process_bank_events(
    payload: game::BankProcessRequest,
) -> Result<game::GameCommandResponse, game::GameError> {
    game::GameApplicationService::process_bank_events(payload.auto_pay)
}
#[tauri::command]
fn game_make_bank_loan_payment() -> Result<game::GameCommandResponse, game::GameError> {
    game::GameApplicationService::bank_close(false, false, false)
}
#[tauri::command]
fn game_partially_repay_bank_credit(
    payload: game::BankAmountRequest,
) -> Result<game::GameCommandResponse, game::GameError> {
    game::GameApplicationService::bank_amount(payload, false)
}
#[tauri::command]
fn game_repay_bank_credit() -> Result<game::GameCommandResponse, game::GameError> {
    game::GameApplicationService::bank_close(false, false, false)
}
#[tauri::command]
fn game_top_up_bank_deposit(
    payload: game::BankAmountRequest,
) -> Result<game::GameCommandResponse, game::GameError> {
    game::GameApplicationService::bank_amount(payload, true)
}
#[tauri::command]
fn game_withdraw_bank_deposit(
    payload: game::BankWithdrawRequest,
) -> Result<game::GameCommandResponse, game::GameError> {
    game::GameApplicationService::bank_close(true, false, payload.allow_early)
}
#[tauri::command]
fn game_withdraw_bank_interest() -> Result<game::GameCommandResponse, game::GameError> {
    game::GameApplicationService::bank_close(true, true, false)
}

fn require_projects_owner(connection: &rusqlite::Connection) -> Result<(), String> {
    let owner: String = connection
        .query_row(
            "SELECT owner FROM storage_ownership WHERE subsystem='projects'",
            [],
            |row| row.get(0),
        )
        .map_err(|error| format!("Не удалось проверить ownership проектов: {error}"))?;
    if owner != "sqlite" {
        return Err("Проекты ещё не переведены в SQLite authoritative storage.".to_string());
    }
    Ok(())
}

fn projects_read_status(connection: &rusqlite::Connection) -> Result<String, String> {
    require_projects_owner(connection)?;
    // mirror_state describes the legacy shadow mirror. SQLite-owned project
    // rows are authoritative, so a stale mirror marker cannot block reads.
    Ok("healthy".to_string())
}

fn now_from_database(connection: &rusqlite::Connection) -> Result<String, String> {
    connection
        .query_row("SELECT strftime('%Y-%m-%dT%H:%M:%SZ','now')", [], |row| {
            row.get(0)
        })
        .map_err(|error| error.to_string())
}

fn project_payload(
    connection: &mut rusqlite::Connection,
    project_id: &str,
) -> Result<serde_json::Value, String> {
    // Refresh derived rows from immutable facts after metadata/rule changes.
    // This does not capture or publish any local history.
    let scopes: Vec<(String, String, String, Option<String>)> = {
        let mut stmt=connection.prepare("SELECT m.account_id,b.canonical_user_id,s.device_id,m.stage_id FROM cloud_progress_migrations m JOIN cloud_account_bindings b ON b.local_account_id=m.account_id JOIN cloud_sync_state s ON s.account_id=m.account_id WHERE m.project_id=?1 AND m.lifecycle='active'").map_err(|_|"progress_storage_unavailable")?;
        let rows = stmt
            .query_map([project_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .map_err(|_| "progress_storage_unavailable")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|_| "progress_storage_unavailable")?;
        rows
    };
    if !scopes.is_empty() {
        let mut privileged = sqlite::open_privileged_remote_apply_database(
            &sqlite_data_root()?.join("nfprogress.db"),
        )
        .map_err(|_| "progress_storage_unavailable")?;
        for (account_id, canonical_user_id, device_id, stage_id) in scopes {
            let scope = project_metadata_sync::MetadataScope {
                account_id,
                canonical_user_id,
                device_id,
            };
            match progress_sync::rebuild(&mut privileged, &scope, project_id, stage_id.as_deref()) {
                Ok(()) => {}
                Err(code) if code == "progress_publication_pending" => {}
                Err(code) if code == "project_metadata_authority_unresolved" => {
                    let view = project_metadata_sync::authority_view(
                        connection,
                        &scope.account_id,
                        project_id,
                    )
                    .map_err(|e| e.to_string())?;
                    let local_edit:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_metadata_decisions WHERE account_id=?1 AND project_id=?2 AND event_id=?3 AND kind='edit')",rusqlite::params![scope.account_id,project_id,view.pending_event_id],|r|r.get(0)).map_err(|_|"progress_storage_unavailable")?;
                    if view.state != "resolution_pending" || !local_edit {
                        return Err(code);
                    }
                    // A local Metadata edit awaits echo. Keep the last proven
                    // Progress view; this read does not apply or ACK that edit.
                }
                Err(code) if code == "stage_dependency_missing" && stage_id.is_some() => {
                    let pending_completion:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_structural_events e JOIN cloud_sync_structural_decisions d USING(account_id,event_id) JOIN cloud_sync_outbox o USING(account_id,event_id) JOIN stages s ON s.id=e.entity_id AND s.project_id=e.project_id WHERE e.account_id=?1 AND e.project_id=?2 AND e.entity_id=?3 AND e.entity_type='stage' AND e.operation='update' AND e.state='unsealed' AND o.device_id=?4 AND json_extract(CAST(substr(e.canonical_frame,21) AS TEXT),'$.stage.status')='завершен' AND s.status='завершен')",rusqlite::params![scope.account_id,project_id,stage_id,scope.device_id],|r|r.get(0)).map_err(|_|"progress_storage_unavailable")?;
                    if !pending_completion {
                        return Err(code);
                    }
                }
                Err(code) => return Err(code),
            }
        }
    }
    let mut repository = ProjectsRepository::new(connection);
    let project = repository
        .get_project(project_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "Проект не найден.".to_string())?;
    let stages = repository
        .list_stages(project_id)
        .map_err(|error| error.to_string())?;
    let mut payload = project.payload;
    let stages_payload = stages
        .into_iter()
        .map(|stage| {
            let mut value = stage.payload;
            value["id"] = serde_json::json!(stage.id);
            let entries = repository
                .list_progress(project_id, Some(&stage.id))
                .map_err(|error| error.to_string())?;
            value["progress_entries"] =
                serde_json::Value::Array(entries.into_iter().map(|entry| entry.payload).collect());
            value["parent_project_id"] = serde_json::Value::String(project_id.to_string());
            Ok(value)
        })
        .collect::<Result<Vec<_>, String>>()?;
    let project_entries = repository
        .list_progress(project_id, None)
        .map_err(|error| error.to_string())?;
    payload["progress_entries"] = serde_json::Value::Array(
        project_entries
            .into_iter()
            .map(|entry| entry.payload)
            .collect(),
    );
    payload["stages"] = serde_json::Value::Array(stages_payload);
    drop(repository);
    if let Some(heads) = progress_sync::local_heads(connection, project_id, None)? {
        payload["progress_heads"] = serde_json::json!(heads);
    }
    if let Some(stages) = payload["stages"].as_array_mut() {
        for stage in stages {
            let id = stage["id"]
                .as_str()
                .ok_or("progress_scope_mismatch")?
                .to_string();
            if let Some(heads) = progress_sync::local_heads(connection, project_id, Some(&id))? {
                stage["progress_heads"] = serde_json::json!(heads);
            }
        }
    }
    if daily_goals::local_project(connection, project_id)? {
        let day = streaks::logical_writing_day(connection)?;
        daily_goals::project(connection, &mut payload, &day)?;
        let state = daily_goals::game_state(connection)?;
        daily_goals::overlay_streak(&mut payload, &state, &format!("project:{project_id}"), &day);
        for stage in payload["stages"].as_array_mut().into_iter().flatten() {
            let key = format!(
                "stage:{project_id}:{}",
                stage["id"].as_str().unwrap_or_default()
            );
            daily_goals::overlay_streak(stage, &state, &key, &day);
        }
    }
    Ok(payload)
}

fn refresh_project_totals(
    connection: &mut rusqlite::Connection,
    project_id: &str,
) -> Result<(), String> {
    let mut repository = ProjectsRepository::new(connection);
    let project = repository
        .get_project(project_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "Проект не найден.".to_string())?;
    let stages = repository
        .list_stages(project_id)
        .map_err(|error| error.to_string())?;
    let total: f64 = stages
        .iter()
        .map(|stage| {
            stage
                .payload
                .get("total")
                .and_then(|value| value.as_f64())
                .unwrap_or(0.0)
        })
        .sum();
    let mut payload = project.payload;
    payload["stages"] =
        serde_json::Value::Array(stages.into_iter().map(|stage| stage.payload).collect());
    let stages_enabled = payload
        .get("stages")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|stages| !stages.is_empty());
    payload["stages_enabled"] = stages_enabled.into();
    payload["total"] = total.into();
    payload["progress"] = if project.infinite || project.goal.unwrap_or(0.0) <= 0.0 {
        0.0.into()
    } else {
        (total / project.goal.unwrap_or(1.0) * 100.0).into()
    };
    repository
        .update_project_payload(project_id, &payload)
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn refresh_project_totals_in_transaction(
    transaction: &rusqlite::Transaction<'_>,
    project_id: &str,
) -> Result<(), String> {
    let (payload_json, goal, infinite): (String, Option<f64>, bool) = transaction
        .query_row(
            "SELECT payload_json, goal, infinite FROM projects WHERE id=?1",
            [project_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(|error| error.to_string())?;
    let mut payload: serde_json::Value = serde_json::from_str(&payload_json)
        .map_err(|error| format!("Некорректный payload проекта: {error}"))?;
    let mut stages = transaction
        .prepare("SELECT payload_json FROM stages WHERE project_id=?1 ORDER BY id")
        .map_err(|error| error.to_string())?
        .query_map([project_id], |row| row.get::<_, String>(0))
        .map_err(|error| error.to_string())?
        .map(|row| {
            row.map_err(|error| error.to_string())
                .and_then(|json| serde_json::from_str(&json).map_err(|error| error.to_string()))
        })
        .collect::<Result<Vec<serde_json::Value>, String>>()?;
    let total = stages.iter().fold(0.0, |sum, stage| {
        sum + stage
            .get("total")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0)
    });
    payload["stages"] = serde_json::Value::Array(std::mem::take(&mut stages));
    payload["stages_enabled"] = payload
        .get("stages")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|items| !items.is_empty())
        .into();
    payload["remaining"] = if infinite {
        serde_json::Value::Null
    } else {
        serde_json::json!((goal.unwrap_or(total) - total).max(0.0))
    };
    payload["added_today"] = serde_json::json!(payload["stages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["added_today"].as_f64().unwrap_or(0.0))
        .sum::<f64>());
    payload["total"] = total.into();
    payload["progress"] = if infinite || goal.unwrap_or(0.0) <= 0.0 {
        0.0.into()
    } else {
        (total / goal.unwrap_or(1.0) * 100.0).into()
    };
    transaction
        .execute(
            "UPDATE projects SET updated_at=strftime('%Y-%m-%dT%H:%M:%SZ','now'), payload_json=?1 WHERE id=?2",
            rusqlite::params![payload.to_string(), project_id],
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn append_event(
    repository: &mut ProjectsRepository<'_>,
    event_id: &str,
    event_type: &str,
    project_id: &str,
    stage_id: Option<&str>,
    progress_id: Option<&str>,
    delta_symbols: Option<f64>,
) -> Result<(), String> {
    append_event_with_context(
        repository,
        event_id,
        event_type,
        project_id,
        stage_id,
        progress_id,
        delta_symbols,
        serde_json::json!({"source": "desktop", "version": 1}),
    )
}

fn append_event_with_context(
    repository: &mut ProjectsRepository<'_>,
    event_id: &str,
    event_type: &str,
    project_id: &str,
    stage_id: Option<&str>,
    progress_id: Option<&str>,
    delta_symbols: Option<f64>,
    context: serde_json::Value,
) -> Result<(), String> {
    repository
        .append_domain_event(
            event_id,
            event_type,
            project_id,
            stage_id,
            progress_id,
            None,
            delta_symbols,
            &context,
        )
        .map_err(|error| error.to_string())
}

fn open_settings_database() -> Result<rusqlite::Connection, String> {
    let database = sqlite_data_root()?.join("nfprogress.db");
    sqlite::open_database(&database).map_err(|error| error.to_string())
}

fn open_notes_database(write: bool) -> Result<rusqlite::Connection, String> {
    let database = sqlite_data_root()?.join("nfprogress.db");
    let _ = write;
    sqlite::open_database(&database)
        .map_err(|error| format!("Не удалось открыть базу данных Notes: {error}"))
}

fn require_sqlite_notes_owner(connection: &rusqlite::Connection) -> Result<(), String> {
    let owner: String = connection
        .query_row(
            "SELECT owner FROM storage_ownership WHERE subsystem = 'notes'",
            [],
            |row| row.get(0),
        )
        .map_err(|error| format!("Не удалось проверить ownership заметок: {error}"))?;
    if owner != "sqlite" {
        return Err("Прямая запись заметок недоступна до завершения миграции.".to_string());
    }
    Ok(())
}

fn require_note_relation(
    connection: &rusqlite::Connection,
    project_id: &str,
    stage_id: Option<&str>,
) -> Result<(), String> {
    let projects_owner: String = connection
        .query_row(
            "SELECT owner FROM storage_ownership WHERE subsystem = 'projects'",
            [],
            |row| row.get(0),
        )
        .map_err(|error| format!("Не удалось проверить ownership проектов: {error}"))?;
    if projects_owner != "sqlite" {
        let status: String = connection
            .query_row(
                "SELECT sync_status FROM mirror_state WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .map_err(|error| format!("Не удалось проверить состояние SQLite mirror: {error}"))?;
        if status != "healthy" {
            return Err(
                "SQLite mirror проектов недоступен для проверки связи заметки.".to_string(),
            );
        }
    }
    connection
        .query_row(
            "SELECT 1 FROM projects WHERE id = ?1",
            [project_id],
            |_row| Ok(()),
        )
        .map_err(|_| "Проект больше не существует.".to_string())?;
    if let Some(stage_id) = stage_id {
        let parent: String = connection
            .query_row(
                "SELECT project_id FROM stages WHERE id = ?1",
                [stage_id],
                |row| row.get(0),
            )
            .map_err(|_| "Этап больше не существует.".to_string())?;
        if parent != project_id {
            return Err("Этап не относится к указанному проекту.".to_string());
        }
    }
    Ok(())
}

fn require_note_writable(
    connection: &rusqlite::Connection,
    project_id: &str,
    stage_id: Option<&str>,
) -> Result<(), String> {
    let project_status: String = connection
        .query_row(
            "SELECT status FROM projects WHERE id = ?1",
            [project_id],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    if project_status == "завершен" {
        return Err("Заметки завершённого проекта доступны только для просмотра.".to_string());
    }
    if let Some(stage_id) = stage_id {
        let stage_status: String = connection
            .query_row(
                "SELECT status FROM stages WHERE id = ?1",
                [stage_id],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        if stage_status == "завершен" {
            return Err("Заметки завершённого этапа доступны только для просмотра.".to_string());
        }
    }
    Ok(())
}

fn note_time(connection: &rusqlite::Connection) -> Result<String, String> {
    connection
        .query_row("SELECT strftime('%Y-%m-%dT%H:%M:%SZ', 'now')", [], |row| {
            row.get(0)
        })
        .map_err(|error| error.to_string())
}

fn stored_map(payload: &serde_json::Value) -> Option<serde_json::Value> {
    let value = payload.get("mindmap")?;
    if value.is_null() {
        None
    } else {
        Some(value.clone())
    }
}

fn new_stored_map(name: &str, id: &str) -> serde_json::Value {
    serde_json::json!({"nodeData": {"id": id, "topic": name, "children": []}})
}

const MAP_INTERNAL_KEYS: [&str; 5] = [
    "nfprogressStageId",
    "nfprogressStageRoot",
    "nfprogressSourceId",
    "nfprogressReadOnly",
    "nfprogressEmptyStageMap",
];

fn strip_map_internal(value: &serde_json::Value) -> serde_json::Value {
    let mut cleaned = value.clone();
    if let Some(object) = cleaned.as_object_mut() {
        for key in MAP_INTERNAL_KEYS {
            object.remove(key);
        }
    }
    cleaned
}

fn copy_project_map_node(value: &serde_json::Value) -> Option<serde_json::Value> {
    if value
        .get("nfprogressStageRoot")
        .and_then(|item| item.as_bool())
        == Some(true)
    {
        return None;
    }
    let mut node = strip_map_internal(value);
    if let Some(object) = node.as_object_mut() {
        object.remove("parent");
    }
    let children = value
        .get("children")
        .and_then(|item| item.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(copy_project_map_node)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    node["children"] = serde_json::Value::Array(children);
    Some(node)
}

fn copy_stage_map_node(
    value: &serde_json::Value,
    stage_id: &str,
    is_root: bool,
    read_only: bool,
    source_path: &str,
    ids: &mut std::collections::HashMap<String, String>,
) -> Option<serde_json::Value> {
    let source_id = value
        .get("id")
        .and_then(|item| item.as_str())
        .filter(|item| !item.is_empty())
        .unwrap_or(source_path);
    let combined_id = if is_root {
        format!("nfprogress-stage-{stage_id}")
    } else {
        format!("nfprogress-stage-{stage_id}-{source_id}")
    };
    ids.insert(source_id.to_string(), combined_id.clone());
    let mut node = strip_map_internal(value);
    if let Some(object) = node.as_object_mut() {
        object.remove("parent");
        object.remove("root");
    }
    node["id"] = combined_id.into();
    node["nfprogressStageId"] = stage_id.into();
    node["nfprogressSourceId"] = source_id.into();
    if is_root {
        node["nfprogressStageRoot"] = true.into();
    }
    if read_only {
        node["nfprogressReadOnly"] = true.into();
    }
    let children = value
        .get("children")
        .and_then(|item| item.as_array())
        .map(|items| {
            items
                .iter()
                .enumerate()
                .filter_map(|(index, item)| {
                    copy_stage_map_node(
                        item,
                        stage_id,
                        false,
                        read_only,
                        &format!("{source_path}/{index}"),
                        ids,
                    )
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    node["children"] = serde_json::Value::Array(children);
    Some(node)
}

fn compose_combined_map(
    project: &ProjectRecord,
    stages: &[StageRecord],
) -> Result<serde_json::Value, String> {
    let mut combined = stored_map(&project.payload)
        .map(mindmap::normalize)
        .transpose()?
        .unwrap_or_else(|| new_stored_map(&project.name, &format!("project-map-{}", project.id)));
    let root = copy_project_map_node(&combined["nodeData"])
        .ok_or_else(|| "У карты проекта отсутствует корневой узел.".to_string())?;
    combined["nodeData"] = root;
    let mut children = combined["nodeData"]["children"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    combined["arrows"] = serde_json::Value::Array(
        combined
            .get("arrows")
            .and_then(|items| items.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter(|item| item.get("nfprogressStageId").is_none())
                    .map(strip_map_internal)
                    .collect()
            })
            .unwrap_or_default(),
    );
    combined["summaries"] = serde_json::Value::Array(
        combined
            .get("summaries")
            .and_then(|items| items.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter(|item| item.get("nfprogressStageId").is_none())
                    .map(strip_map_internal)
                    .collect()
            })
            .unwrap_or_default(),
    );
    for stage in stages {
        let stage_map = stored_map(&stage.payload)
            .map(mindmap::normalize)
            .transpose()?
            .unwrap_or_else(|| new_stored_map(&stage.name, &format!("stage-map-{}", stage.id)));
        let read_only = stage.status == "завершен";
        let has_content = mindmap::has_content(Some(&stage_map), &stage.name);
        let mut ids = std::collections::HashMap::new();
        let mut stage_root = copy_stage_map_node(
            &stage_map["nodeData"],
            &stage.id,
            true,
            read_only,
            "generated-root",
            &mut ids,
        )
        .ok_or_else(|| "У карты этапа отсутствует корневой узел.".to_string())?;
        stage_root["topic"] = if read_only
            && !stage_root
                .get("topic")
                .and_then(|item| item.as_str())
                .unwrap_or_default()
                .starts_with("✅ ")
        {
            format!("✅ {}", stage.name).into()
        } else {
            stage.name.clone().into()
        };
        if read_only && !has_content {
            stage_root["nfprogressEmptyStageMap"] = true.into();
        }
        children.push(stage_root);
        for item in stage_map
            .get("arrows")
            .and_then(|items| items.as_array())
            .into_iter()
            .flatten()
        {
            let Some(from) = item
                .get("from")
                .and_then(|item| item.as_str())
                .and_then(|id| ids.get(id))
            else {
                continue;
            };
            let Some(to) = item
                .get("to")
                .and_then(|item| item.as_str())
                .and_then(|id| ids.get(id))
            else {
                continue;
            };
            let mut arrow = strip_map_internal(item);
            arrow["id"] = format!(
                "nfprogress-arrow-{}-{}",
                stage.id,
                item.get("id")
                    .and_then(|item| item.as_str())
                    .unwrap_or("generated")
            )
            .into();
            arrow["from"] = from.clone().into();
            arrow["to"] = to.clone().into();
            arrow["nfprogressStageId"] = stage.id.clone().into();
            combined["arrows"]
                .as_array_mut()
                .expect("arrows array")
                .push(arrow);
        }
        for item in stage_map
            .get("summaries")
            .and_then(|items| items.as_array())
            .into_iter()
            .flatten()
        {
            let Some(parent) = item
                .get("parent")
                .and_then(|item| item.as_str())
                .and_then(|id| ids.get(id))
            else {
                continue;
            };
            let mut summary = strip_map_internal(item);
            summary["id"] = format!(
                "nfprogress-summary-{}-{}",
                stage.id,
                item.get("id")
                    .and_then(|item| item.as_str())
                    .unwrap_or("generated")
            )
            .into();
            summary["parent"] = parent.clone().into();
            summary["nfprogressStageId"] = stage.id.clone().into();
            combined["summaries"]
                .as_array_mut()
                .expect("summaries array")
                .push(summary);
        }
    }
    combined["nodeData"]["children"] = serde_json::Value::Array(children);
    mindmap::normalize(combined)
}

fn collect_stage_roots(
    value: &serde_json::Value,
    roots: &mut std::collections::HashMap<String, serde_json::Value>,
) {
    if value
        .get("nfprogressStageRoot")
        .and_then(|item| item.as_bool())
        == Some(true)
    {
        if let Some(id) = value
            .get("nfprogressStageId")
            .and_then(|item| item.as_str())
        {
            roots.entry(id.to_string()).or_insert_with(|| value.clone());
        }
    }
    if let Some(children) = value.get("children").and_then(|item| item.as_array()) {
        for child in children {
            collect_stage_roots(child, roots);
        }
    }
}

fn restore_stage_map_node(
    value: &serde_json::Value,
    is_root: bool,
    ids: &mut std::collections::HashMap<String, String>,
) -> Option<serde_json::Value> {
    let mut node = strip_map_internal(value);
    if let Some(object) = node.as_object_mut() {
        object.remove("parent");
        object.remove("root");
    }
    let current_id = value.get("id").and_then(|item| item.as_str())?;
    let source_id = value
        .get("nfprogressSourceId")
        .and_then(|item| item.as_str())
        .or_else(|| value.get("id").and_then(|item| item.as_str()))?;
    ids.insert(current_id.to_string(), source_id.to_string());
    node["id"] = source_id.into();
    let children = value
        .get("children")
        .and_then(|item| item.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| restore_stage_map_node(item, false, ids))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    node["children"] = serde_json::Value::Array(children);
    if is_root {
        node["id"] = source_id.into();
    }
    Some(node)
}

fn split_combined_map(
    _project: &ProjectRecord,
    stages: &[StageRecord],
    combined: &serde_json::Value,
) -> Result<
    (
        serde_json::Value,
        std::collections::HashMap<String, serde_json::Value>,
    ),
    String,
> {
    let mut roots = std::collections::HashMap::new();
    collect_stage_roots(&combined["nodeData"], &mut roots);
    let project_root = copy_project_map_node(&combined["nodeData"])
        .ok_or_else(|| "У объединённой карты отсутствует корневой узел.".to_string())?;
    let mut project_map = combined.clone();
    project_map["nodeData"] = project_root;
    project_map["arrows"] = serde_json::Value::Array(
        combined
            .get("arrows")
            .and_then(|items| items.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter(|item| item.get("nfprogressStageId").is_none())
                    .map(strip_map_internal)
                    .collect()
            })
            .unwrap_or_default(),
    );
    project_map["summaries"] = serde_json::Value::Array(
        combined
            .get("summaries")
            .and_then(|items| items.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter(|item| item.get("nfprogressStageId").is_none())
                    .map(strip_map_internal)
                    .collect()
            })
            .unwrap_or_default(),
    );
    let mut stage_maps = std::collections::HashMap::new();
    for stage in stages {
        if let Some(root) = roots.get(&stage.id) {
            let mut existing = stored_map(&stage.payload)
                .map(mindmap::normalize)
                .transpose()?
                .unwrap_or_else(|| new_stored_map(&stage.name, &format!("stage-map-{}", stage.id)));
            let original_topic = existing
                .get("nodeData")
                .and_then(|node_data| node_data.get("topic"))
                .cloned();
            let mut ids = std::collections::HashMap::new();
            existing["nodeData"] = restore_stage_map_node(root, true, &mut ids)
                .ok_or_else(|| "Повреждённая ветвь карты этапа.".to_string())?;
            if original_topic
                .as_ref()
                .and_then(|value| value.as_str())
                .map_or(true, |value| {
                    value == stage.name || value.starts_with("✅ ")
                })
            {
                existing["nodeData"]["topic"] = stage.name.clone().into();
            }
            existing["arrows"] = serde_json::Value::Array(
                combined
                    .get("arrows")
                    .and_then(|items| items.as_array())
                    .into_iter()
                    .flatten()
                    .filter(|item| {
                        item.get("nfprogressStageId")
                            .and_then(|value| value.as_str())
                            == Some(stage.id.as_str())
                    })
                    .map(|item| {
                        let mut restored = strip_map_internal(item);
                        if let Some(value) = item
                            .get("from")
                            .and_then(|value| value.as_str())
                            .and_then(|id| ids.get(id))
                        {
                            restored["from"] = value.clone().into();
                        }
                        if let Some(value) = item
                            .get("to")
                            .and_then(|value| value.as_str())
                            .and_then(|id| ids.get(id))
                        {
                            restored["to"] = value.clone().into();
                        }
                        restored
                    })
                    .collect(),
            );
            existing["summaries"] = serde_json::Value::Array(
                combined
                    .get("summaries")
                    .and_then(|items| items.as_array())
                    .into_iter()
                    .flatten()
                    .filter(|item| {
                        item.get("nfprogressStageId")
                            .and_then(|value| value.as_str())
                            == Some(stage.id.as_str())
                    })
                    .map(|item| {
                        let mut restored = strip_map_internal(item);
                        if let Some(value) = item
                            .get("parent")
                            .and_then(|value| value.as_str())
                            .and_then(|id| ids.get(id))
                        {
                            restored["parent"] = value.clone().into();
                        }
                        restored
                    })
                    .collect(),
            );
            stage_maps.insert(stage.id.clone(), existing);
        }
    }
    Ok((project_map, stage_maps))
}

fn map_writable(project_status: &str, stage_status: Option<&str>) -> Result<(), String> {
    if project_status == "завершен" {
        return Err("Заметки завершённого проекта доступны только для просмотра.".to_string());
    }
    if stage_status == Some("завершен") {
        return Err("Заметки завершённого этапа доступны только для просмотра.".to_string());
    }
    Ok(())
}

fn update_stored_map(
    transaction: &rusqlite::Transaction<'_>,
    project_id: &str,
    stage_id: Option<&str>,
    map: &serde_json::Value,
    now: &str,
    notes_changed: bool,
) -> Result<(), String> {
    let mut payload: serde_json::Value = if let Some(stage_id) = stage_id {
        transaction
            .query_row(
                "SELECT payload_json FROM stages WHERE id=?1 AND project_id=?2",
                rusqlite::params![stage_id, project_id],
                |row| row.get::<_, String>(0),
            )
            .map_err(|error| error.to_string())
            .and_then(|raw| serde_json::from_str(&raw).map_err(|error| error.to_string()))?
    } else {
        transaction
            .query_row(
                "SELECT payload_json FROM projects WHERE id=?1",
                [project_id],
                |row| row.get::<_, String>(0),
            )
            .map_err(|error| error.to_string())
            .and_then(|raw| serde_json::from_str(&raw).map_err(|error| error.to_string()))?
    };
    let object = payload
        .as_object_mut()
        .ok_or_else(|| "Некорректный payload владельца карты.".to_string())?;
    object.insert("mindmap".to_string(), map.clone());
    object.insert(
        "mindmap_updated_at".to_string(),
        serde_json::Value::String(now.to_string()),
    );
    if notes_changed {
        object.insert(
            "notes_updated_at".to_string(),
            serde_json::Value::String(now.to_string()),
        );
    }
    let table = if stage_id.is_some() {
        "stages"
    } else {
        "projects"
    };
    let changed = if let Some(stage_id) = stage_id {
        transaction
            .execute(
                "UPDATE stages SET updated_at=?1,payload_json=?2 WHERE id=?3 AND project_id=?4",
                rusqlite::params![now, payload.to_string(), stage_id, project_id],
            )
            .map_err(|error| error.to_string())?
    } else {
        transaction
            .execute(
                "UPDATE projects SET updated_at=?1,payload_json=?2 WHERE id=?3",
                rusqlite::params![now, payload.to_string(), project_id],
            )
            .map_err(|error| error.to_string())?
    };
    if changed != 1 {
        return Err(format!("Не удалось сохранить карту в {table}."));
    }
    Ok(())
}

fn reconcile_map_notes(
    transaction: &rusqlite::Transaction<'_>,
    project_id: &str,
    stage_id: Option<&str>,
    map: &serde_json::Value,
    now: &str,
) -> Result<bool, String> {
    let map_id = mindmap::map_id(map).map(str::to_string);
    let map_notes = mindmap::extract_notes(map);
    let mut rows = Vec::new();
    {
        let mut statement = transaction.prepare(
            "SELECT id,payload_json FROM notes WHERE project_id=?1 AND (?2 IS NULL AND stage_id IS NULL OR stage_id=?2)",
        ).map_err(|error| error.to_string())?;
        for row in statement
            .query_map(rusqlite::params![project_id, stage_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|error| error.to_string())?
        {
            rows.push(row.map_err(|error| error.to_string())?);
        }
    }
    let mut existing = std::collections::HashMap::<String, (String, serde_json::Value)>::new();
    for (id, raw) in &rows {
        let payload: serde_json::Value =
            serde_json::from_str(raw).map_err(|error| error.to_string())?;
        if payload.get("source_type").and_then(|value| value.as_str()) == Some("mindmap") {
            if let Some(source_id) = payload
                .get("source_node_id")
                .and_then(|value| value.as_str())
            {
                existing.insert(source_id.to_string(), (id.clone(), payload));
            }
        }
    }
    let mut changed = false;
    let mut seen = HashSet::new();
    let next_order = rows
        .iter()
        .filter_map(|(_, raw)| serde_json::from_str::<serde_json::Value>(raw).ok())
        .filter_map(|value| value.get("sort_order").and_then(|value| value.as_i64()))
        .max()
        .unwrap_or(-1)
        + 1;
    let mut order = next_order;
    for (source_id, text) in map_notes {
        if !seen.insert(source_id.clone()) {
            continue;
        }
        if let Some((id, mut payload)) = existing.remove(&source_id) {
            let old_text = payload
                .get("content")
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            let old_map_id = payload
                .get("source_map_id")
                .and_then(|value| value.as_str());
            if old_text != text || old_map_id != map_id.as_deref() {
                let object = payload
                    .as_object_mut()
                    .ok_or_else(|| "Некорректный payload заметки карты.".to_string())?;
                object.insert("content".to_string(), serde_json::Value::String(text));
                object.insert(
                    "source_map_id".to_string(),
                    map_id
                        .clone()
                        .map_or(serde_json::Value::Null, serde_json::Value::String),
                );
                object.insert(
                    "updated_at".to_string(),
                    serde_json::Value::String(now.to_string()),
                );
                let revision = object
                    .get("revision")
                    .and_then(|value| value.as_i64())
                    .unwrap_or(0)
                    .saturating_add(1);
                object.insert("revision".to_string(), revision.into());
                let snapshot = payload.to_string();
                prepare_direct_note_intent(
                    transaction,
                    project_id,
                    &id,
                    note_sync::NoteSyncOperation::Upsert,
                    now,
                    None,
                    &snapshot,
                )?;
                transaction
                    .execute(
                        "UPDATE notes SET updated_at=?1,payload_json=?2 WHERE id=?3",
                        rusqlite::params![now, snapshot, id],
                    )
                    .map_err(|error| error.to_string())?;
                changed = true;
            }
        } else {
            let id = mindmap::linked_note_id(&source_id);
            let payload = serde_json::json!({
                "id": id, "project_id": project_id, "stage_id": stage_id,
                "title": "", "content": text, "content_format": "plain", "checklist": [],
                "color": "default", "pinned": false, "archived": false, "sort_order": order,
                "tags": [], "source_type": "mindmap", "source_map_id": map_id,
                "source_node_id": source_id, "created_at": now, "updated_at": now,
                "revision": 0, "metadata": {}
            });
            let snapshot = payload.to_string();
            prepare_direct_note_intent(
                transaction,
                project_id,
                &id,
                note_sync::NoteSyncOperation::Upsert,
                now,
                None,
                &snapshot,
            )?;
            transaction.execute("INSERT INTO notes(id,project_id,stage_id,updated_at,payload_json) VALUES(?1,?2,?3,?4,?5)", rusqlite::params![id, project_id, stage_id, now, snapshot]).map_err(|error| error.to_string())?;
            order += 1;
            changed = true;
        }
    }
    for (id, payload) in existing.into_values() {
        note_sync::prepare_note_delete_intent(transaction, project_id, &id, &payload, now)
            .map_err(|error| error.user_message().to_string())?;
        transaction
            .execute(
                "DELETE FROM notes WHERE id=?1 AND project_id=?2",
                rusqlite::params![id, project_id],
            )
            .map_err(|error| error.to_string())?;
        changed = true;
    }
    Ok(changed)
}

fn reconcile_loaded_map_view(project_id: &str, stage_id: Option<&str>) -> Result<(), String> {
    let mut connection = open_projects_database()?;
    require_projects_owner(&connection)?;
    reconcile_loaded_map_view_in_connection(&mut connection, project_id, stage_id)
}

fn reconcile_loaded_map_view_in_connection(
    connection: &mut rusqlite::Connection,
    project_id: &str,
    stage_id: Option<&str>,
) -> Result<(), String> {
    let (project, stages) = {
        let repository = ProjectsRepository::new(connection);
        let project = repository
            .get_project(project_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "Проект не найден.".to_string())?;
        let stages = repository
            .list_stages(project_id)
            .map_err(|error| error.to_string())?;
        (project, stages)
    };
    let combined = stage_id.is_none()
        && project
            .payload
            .get("combine_stage_mindmaps")
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
        && !stages.is_empty();
    let now = now_from_database(connection)?;
    let tx = connection
        .transaction()
        .map_err(|error| error.to_string())?;
    if combined {
        if !map_sync::admitted(&tx, project_id, None).map_err(|e| e.to_string())? {
            if let Some(map) = stored_map(&project.payload)
                .map(mindmap::normalize)
                .transpose()?
            {
                if reconcile_map_notes(&tx, project_id, None, &map, &now)? {
                    update_stored_map(&tx, project_id, None, &map, &now, true)?;
                }
            }
        }
        for stage in &stages {
            if map_sync::admitted(&tx, project_id, Some(&stage.id)).map_err(|e| e.to_string())? {
                continue;
            }
            if let Some(map) = stored_map(&stage.payload)
                .map(mindmap::normalize)
                .transpose()?
            {
                if reconcile_map_notes(&tx, project_id, Some(&stage.id), &map, &now)? {
                    update_stored_map(&tx, project_id, Some(&stage.id), &map, &now, true)?;
                }
            }
        }
    } else {
        let (map, owner_stage_id) = if let Some(stage_id) = stage_id {
            let stage = stages
                .iter()
                .find(|stage| stage.id == stage_id)
                .ok_or_else(|| "Этап не найден.".to_string())?;
            (stored_map(&stage.payload), Some(stage.id.as_str()))
        } else {
            (stored_map(&project.payload), None)
        };
        if !map_sync::admitted(&tx, project_id, owner_stage_id).map_err(|e| e.to_string())? {
            if let Some(map) = map.map(mindmap::normalize).transpose()? {
                if reconcile_map_notes(&tx, project_id, owner_stage_id, &map, &now)? {
                    update_stored_map(&tx, project_id, owner_stage_id, &map, &now, true)?;
                }
            }
        }
    }
    tx.commit().map_err(|error| error.to_string())
}

fn map_response(project_id: String, stage_id: Option<String>) -> Result<serde_json::Value, String> {
    reconcile_loaded_map_view(&project_id, stage_id.as_deref())?;
    let mut connection = open_projects_database()?;
    map_response_in_connection(&mut connection, project_id, stage_id)
}

fn map_response_in_connection(
    connection: &mut rusqlite::Connection,
    project_id: String,
    stage_id: Option<String>,
) -> Result<serde_json::Value, String> {
    require_projects_owner(connection)?;
    // Keep rendered owners and CAS evidence in one read snapshot.
    connection
        .execute_batch("BEGIN DEFERRED")
        .map_err(|e| e.to_string())?;
    let repository = ProjectsRepository::new(connection);
    let project = repository
        .get_project(&project_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "Проект не найден.".to_string())?;
    let stages = repository
        .list_stages(&project_id)
        .map_err(|error| error.to_string())?;
    let (name, status, data, combined) = if let Some(stage_id) = stage_id.as_deref() {
        let stage = stages
            .iter()
            .find(|stage| stage.id == stage_id)
            .ok_or_else(|| "Этап не найден.".to_string())?;
        (
            stage.name.clone(),
            stage.status.clone(),
            stored_map(&stage.payload),
            false,
        )
    } else {
        let combined = project
            .payload
            .get("combine_stage_mindmaps")
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
            && !stages.is_empty();
        (
            project.name.clone(),
            project.status.clone(),
            stored_map(&project.payload),
            combined,
        )
    };
    drop(repository);
    let data = if combined {
        Some(compose_combined_map(&project, &stages)?)
    } else {
        data.map(|value| mindmap::normalize(value)).transpose()?
    };
    let notes = list_notes_in_connection(connection, project_id.clone(), stage_id.clone())?
        .get("notes")
        .cloned()
        .unwrap_or_else(|| serde_json::json!([]));
    let empty_completed = combined
        && stages.iter().any(|stage| {
            stage.status == "завершен"
                && !mindmap::has_content(stored_map(&stage.payload).as_ref(), &stage.name)
        });
    let expected_heads = map_sync::expected(connection, &project_id, stage_id.as_deref(), combined)
        .map_err(|e| e.to_string())?;
    connection
        .execute_batch("COMMIT")
        .map_err(|e| e.to_string())?;
    Ok(
        serde_json::json!({"expected_heads":expected_heads,"project_id": project_id, "stage_id": stage_id, "name": name, "data": data, "combined": combined, "read_only": status == "завершен", "has_empty_completed_stage_map": empty_completed, "notes": notes}),
    )
}

#[tauri::command]
fn load_map(project_id: String, stage_id: Option<String>) -> Result<serde_json::Value, String> {
    map_response(project_id, stage_id)
}

#[tauri::command]
fn save_map(command: MapCommand) -> Result<serde_json::Value, String> {
    let normalized = mindmap::normalize(command.data.clone())?;
    let mut connection = open_projects_database()?;
    require_projects_owner(&connection)?;
    save_map_in_connection(&mut connection, &command, &normalized)?;
    map_response(command.project_id, command.stage_id)
}

fn save_map_in_connection(
    connection: &mut rusqlite::Connection,
    command: &MapCommand,
    normalized: &serde_json::Value,
) -> Result<(), String> {
    let project = {
        let repository = ProjectsRepository::new(connection);
        repository
            .get_project(&command.project_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "Проект не найден.".to_string())?
    };
    let stage_status = if let Some(stage_id) = command.stage_id.as_deref() {
        let repository = ProjectsRepository::new(connection);
        Some(
            repository
                .get_stage(stage_id)
                .map_err(|error| error.to_string())?
                .filter(|stage| stage.project_id == command.project_id)
                .ok_or_else(|| "Этап не относится к указанному проекту.".to_string())?
                .status,
        )
    } else {
        None
    };
    map_writable(&project.status, stage_status.as_deref())?;
    let now = now_from_database(connection)?;
    let stages = {
        let repository = ProjectsRepository::new(connection);
        repository
            .list_stages(&command.project_id)
            .map_err(|error| error.to_string())?
    };
    let tx = connection
        .transaction()
        .map_err(|error| error.to_string())?;
    let combined = command.stage_id.is_none()
        && project
            .payload
            .get("combine_stage_mindmaps")
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
        && !stages.is_empty();
    map_sync::check_expected(
        &tx,
        &command.project_id,
        command.stage_id.as_deref(),
        combined,
        command.expected_heads.as_ref(),
        &now,
    )
    .map_err(|e| e.to_string())?;
    if combined {
        let (project_map, stage_maps) = split_combined_map(&project, &stages, &command.data)?;
        if !map_sync::local_edit(&tx, &command.project_id, None, &project_map, None, &now)
            .map_err(|e| e.to_string())?
        {
            let project_map = mindmap::normalize(project_map)?;
            let changed = reconcile_map_notes(&tx, &command.project_id, None, &project_map, &now)?;
            update_stored_map(&tx, &command.project_id, None, &project_map, &now, changed)?;
        }
        for stage in &stages {
            if let Some(stage_map) = stage_maps.get(&stage.id) {
                if stage.status == "завершен" {
                    continue;
                }
                if map_sync::local_edit(
                    &tx,
                    &command.project_id,
                    Some(&stage.id),
                    stage_map,
                    None,
                    &now,
                )
                .map_err(|e| e.to_string())?
                {
                    continue;
                }
                let stage_map = mindmap::normalize(stage_map.clone())?;
                let changed = reconcile_map_notes(
                    &tx,
                    &command.project_id,
                    Some(&stage.id),
                    &stage_map,
                    &now,
                )?;
                update_stored_map(
                    &tx,
                    &command.project_id,
                    Some(&stage.id),
                    &stage_map,
                    &now,
                    changed,
                )?;
            }
        }
    } else {
        if map_sync::local_edit(
            &tx,
            &command.project_id,
            command.stage_id.as_deref(),
            &command.data,
            None,
            &now,
        )
        .map_err(|e| e.to_string())?
        {
            return tx.commit().map_err(|e| e.to_string());
        }
        let changed = reconcile_map_notes(
            &tx,
            &command.project_id,
            command.stage_id.as_deref(),
            normalized,
            &now,
        )?;
        update_stored_map(
            &tx,
            &command.project_id,
            command.stage_id.as_deref(),
            normalized,
            &now,
            changed,
        )?;
    }
    tx.commit().map_err(|error| error.to_string())
}

#[tauri::command]
fn import_xmind(command: XMindCommand) -> Result<serde_json::Value, String> {
    if command.project_id.is_empty() || command.stage_id.as_deref().is_some_and(str::is_empty) {
        return Err("Некорректная область карты.".to_string());
    }
    let mut connection = open_projects_database()?;
    require_projects_owner(&connection)?;
    let repository = ProjectsRepository::new(&mut connection);
    repository
        .get_project(&command.project_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "Проект не найден.".to_string())?;
    if let Some(stage_id) = command.stage_id.as_deref() {
        repository
            .get_stage(stage_id)
            .map_err(|error| error.to_string())?
            .filter(|stage| stage.project_id == command.project_id)
            .ok_or_else(|| "Этап не относится к указанному проекту.".to_string())?;
    }
    drop(repository);
    let sheets = mindmap::import_xmind(&command.bytes)?;
    Ok(serde_json::json!({"sheets": sheets}))
}

fn raw_note_id(note_id: &str, aggregate: bool) -> &str {
    if aggregate {
        note_id.rsplit(':').next().unwrap_or(note_id)
    } else {
        note_id
    }
}

fn new_note_id() -> Result<String, String> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|error| error.to_string())?;
    let mut value = String::with_capacity(32);
    for byte in bytes {
        write!(&mut value, "{byte:02x}").expect("writing to a String cannot fail");
    }
    Ok(value)
}

fn decorate_note(
    connection: &rusqlite::Connection,
    mut note: serde_json::Value,
    aggregate: bool,
) -> Result<serde_json::Value, String> {
    let object = note
        .as_object_mut()
        .ok_or_else(|| "Некорректный payload заметки.".to_string())?;
    let project_id = object
        .get("project_id")
        .and_then(|value| value.as_str())
        .ok_or_else(|| "У заметки отсутствует project_id.".to_string())?;
    let stage_id = object
        .get("stage_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let project_status: String = connection
        .query_row(
            "SELECT status FROM projects WHERE id = ?1",
            [project_id],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    let (owner_type, owner_id, stage_name, owner_order, stage_status) = if let Some(stage_id) =
        stage_id.as_deref()
    {
        let (name, status): (String, String) = connection
            .query_row(
                "SELECT name, status FROM stages WHERE id = ?1",
                [stage_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|error| error.to_string())?;
        let order: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM stages WHERE project_id = ?1 AND rowid <= (SELECT rowid FROM stages WHERE id = ?2)",
                rusqlite::params![project_id, stage_id],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        (
            "stage",
            stage_id.to_string(),
            Some(name),
            order,
            Some(status),
        )
    } else {
        ("project", project_id.to_string(), None, 0, None)
    };
    if aggregate {
        let raw_id = object
            .get("id")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        object.insert(
            "id".to_string(),
            serde_json::Value::String(format!("{owner_type}:{owner_id}:{raw_id}")),
        );
    }
    object.insert(
        "owner_type".to_string(),
        serde_json::Value::String(owner_type.to_string()),
    );
    object.insert("owner_id".to_string(), serde_json::Value::String(owner_id));
    object.insert(
        "owner_order".to_string(),
        serde_json::Value::Number(owner_order.into()),
    );
    object.insert(
        "stage_name".to_string(),
        stage_name.map_or(serde_json::Value::Null, serde_json::Value::String),
    );
    let content = object
        .get("content")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    if object.get("source_type").and_then(|value| value.as_str()) == Some("mindmap") {
        let title = object
            .get("title")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        let display = if title.is_empty() {
            content
                .lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or("Заметка карты")
                .chars()
                .take(100)
                .collect()
        } else {
            title.to_string()
        };
        object.insert(
            "display_title".to_string(),
            serde_json::Value::String(display),
        );
        object.insert("system_tags".to_string(), serde_json::json!(["карта"]));
    } else {
        object.insert(
            "display_title".to_string(),
            object
                .get("title")
                .cloned()
                .unwrap_or(serde_json::Value::String(String::new())),
        );
        object.insert("system_tags".to_string(), serde_json::json!([]));
    }
    object.insert(
        "read_only".to_string(),
        serde_json::Value::Bool(
            project_status == "завершен" || stage_status.as_deref() == Some("завершен"),
        ),
    );
    Ok(note)
}

#[tauri::command]
fn list_notes(project_id: String, stage_id: Option<String>) -> Result<serde_json::Value, String> {
    let connection = open_notes_database(false)?;
    list_notes_in_connection(&connection, project_id, stage_id)
}

fn list_notes_in_connection(
    connection: &rusqlite::Connection,
    project_id: String,
    stage_id: Option<String>,
) -> Result<serde_json::Value, String> {
    require_sqlite_notes_owner(connection)?;
    let mut notes = Vec::new();
    if let Some(stage_id) = stage_id.as_deref() {
        let mut statement = connection.prepare("SELECT payload_json FROM notes WHERE project_id = ?1 AND stage_id = ?2 ORDER BY json_extract(payload_json, '$.archived'), json_extract(payload_json, '$.pinned') DESC, json_extract(payload_json, '$.sort_order'), json_extract(payload_json, '$.created_at'), rowid").map_err(|error| error.to_string())?;
        for row in statement
            .query_map(rusqlite::params![project_id, stage_id], |row| {
                row.get::<_, String>(0)
            })
            .map_err(|error| error.to_string())?
        {
            let payload = row.map_err(|error| error.to_string())?;
            notes.push(
                serde_json::from_str::<serde_json::Value>(&payload)
                    .map_err(|error| error.to_string())?,
            );
        }
    } else {
        let mut statement = connection.prepare("SELECT payload_json FROM notes WHERE project_id = ?1 ORDER BY json_extract(payload_json, '$.archived'), json_extract(payload_json, '$.pinned') DESC, json_extract(payload_json, '$.sort_order'), json_extract(payload_json, '$.created_at'), rowid").map_err(|error| error.to_string())?;
        for row in statement
            .query_map(rusqlite::params![project_id], |row| row.get::<_, String>(0))
            .map_err(|error| error.to_string())?
        {
            let payload = row.map_err(|error| error.to_string())?;
            notes.push(
                serde_json::from_str::<serde_json::Value>(&payload)
                    .map_err(|error| error.to_string())?,
            );
        }
    }
    let aggregate = stage_id.is_none();
    let notes = notes
        .into_iter()
        .map(|note| decorate_note(connection, note, aggregate))
        .collect::<Result<Vec<_>, _>>()?;
    let stages = if aggregate {
        let mut rows = connection
            .prepare("SELECT id, name FROM stages WHERE project_id = ?1 ORDER BY rowid")
            .map_err(|error| error.to_string())?;
        let values = rows.query_map([&project_id], |row| Ok(serde_json::json!({"id": row.get::<_, String>(0)?, "name": row.get::<_, String>(1)?})))
            .map_err(|error| error.to_string())?.map(|row| row.map_err(|error| error.to_string())).collect::<Result<Vec<_>, _>>()?
            ;
        values
    } else {
        Vec::new()
    };
    let status: String = connection
        .query_row(
            "SELECT status FROM projects WHERE id = ?1",
            [&project_id],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    Ok(
        serde_json::json!({"notes": notes, "read_only": status == "завершен", "context": {"hasStages": !stages.is_empty(), "stages": stages}}),
    )
}

#[tauri::command]
fn get_note(
    project_id: String,
    note_id: String,
    stage_id: Option<String>,
) -> Result<Option<serde_json::Value>, String> {
    let response = list_notes(project_id, stage_id.clone())?;
    let notes = response
        .get("notes")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    Ok(notes.into_iter().find(|note| {
        let id = note.get("id").and_then(|value| value.as_str());
        id == Some(note_id.as_str())
            || (stage_id.is_none()
                && id.is_some_and(|value| value.ends_with(&format!(":{note_id}"))))
    }))
}

#[tauri::command]
fn list_unsealed_note_sync_intents(
    limit: u32,
    retry_blocked: bool,
) -> Result<Vec<note_sync::UnsealedNoteSyncIntent>, String> {
    let mut connection = open_notes_database(true)?;
    require_sqlite_notes_owner(&connection)?;
    note_sync::list_unsealed_note_sync_intents(&mut connection, limit, retry_blocked)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn preflight_cloud_project_bootstrap(
    project_id: String,
) -> Result<Vec<note_sync::NoteSyncPreflightIssue>, String> {
    let connection = open_projects_database()?;
    require_projects_owner(&connection)?;
    note_sync::preflight_note_sync_project(&connection, &project_id)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn prepare_cloud_project_bootstrap(
    command: note_sync::PrepareCloudProjectBootstrapCommand,
) -> Result<note_sync::CloudProjectBootstrapRecord, String> {
    let mut connection = open_projects_database()?;
    require_projects_owner(&connection)?;
    note_sync::prepare_cloud_project_bootstrap(&mut connection, &command)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn confirm_cloud_project_registration(
    command: note_sync::ConfirmCloudProjectRegistrationCommand,
) -> Result<note_sync::CloudProjectBootstrapRecord, String> {
    let mut connection = open_projects_database()?;
    require_projects_owner(&connection)?;
    note_sync::confirm_cloud_project_registration(&mut connection, &command)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn capture_initial_note_sync_intents(
    command: note_sync::CloudProjectBootstrapScopeCommand,
) -> Result<note_sync::CloudProjectBootstrapRecord, String> {
    let mut connection = open_projects_database()?;
    require_projects_owner(&connection)?;
    note_sync::capture_initial_note_sync_intents(&mut connection, &command)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn read_initial_note_cohort_status(
    command: note_sync::CloudProjectBootstrapScopeCommand,
) -> Result<note_sync::CloudProjectInitialCohortStatus, String> {
    let mut connection = open_projects_database()?;
    require_projects_owner(&connection)?;
    note_sync::read_initial_note_cohort_status(&mut connection, &command)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn mark_cloud_project_bootstrap_completing(
    command: note_sync::CloudProjectBootstrapScopeCommand,
) -> Result<note_sync::CloudProjectBootstrapRecord, String> {
    let mut connection = open_projects_database()?;
    require_projects_owner(&connection)?;
    note_sync::mark_cloud_project_bootstrap_completing(&mut connection, &command)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn mark_cloud_project_bootstrap_ready(
    command: note_sync::CloudProjectBootstrapScopeCommand,
) -> Result<note_sync::CloudProjectBootstrapRecord, String> {
    let mut connection = open_projects_database()?;
    require_projects_owner(&connection)?;
    note_sync::mark_cloud_project_bootstrap_ready(&mut connection, &command)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn set_cloud_project_bootstrap_paused(
    command: note_sync::CloudProjectBootstrapScopeCommand,
    paused: bool,
) -> Result<note_sync::CloudProjectBootstrapRecord, String> {
    let mut connection = open_projects_database()?;
    require_projects_owner(&connection)?;
    note_sync::set_cloud_project_bootstrap_paused(&mut connection, &command, paused)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn list_cloud_project_bootstraps(
    account_id: String,
) -> Result<Vec<note_sync::CloudProjectBootstrapRecord>, String> {
    let mut connection = open_projects_database()?;
    require_projects_owner(&connection)?;
    note_sync::list_cloud_project_bootstraps(&mut connection, &account_id)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn import_remote_cloud_project(
    command: note_sync::ImportRemoteCloudProjectCommand,
) -> Result<note_sync::CloudProjectBootstrapRecord, String> {
    let mut connection = open_projects_database()?;
    require_projects_owner(&connection)?;
    note_sync::import_remote_cloud_project(&mut connection, &command)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn list_sealed_note_sync_outbox(
    account_id: String,
    limit: u32,
) -> Result<Vec<note_sync::SealedNoteSyncOutboxItem>, String> {
    let mut connection = open_notes_database(true)?;
    require_sqlite_notes_owner(&connection)?;
    note_sync::list_sealed_note_sync_outbox(&mut connection, &account_id, limit)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn record_note_sync_seal_failure(
    command: note_sync::RecordNoteSyncSealFailureCommand,
) -> Result<note_sync::RecordNoteSyncSealFailureResult, String> {
    let mut connection = open_notes_database(true)?;
    require_sqlite_notes_owner(&connection)?;
    note_sync::record_note_sync_seal_failure(&mut connection, &command)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn commit_sealed_note_sync_event(
    command: note_sync::CommitSealedNoteSyncEventCommand,
) -> Result<note_sync::CommitSealedNoteSyncEventResult, String> {
    let mut connection = open_notes_database(true)?;
    require_sqlite_notes_owner(&connection)?;
    note_sync::commit_sealed_note_sync_event(&mut connection, &command)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn list_unsealed_note_resolution_intents(
    account_id: String,
    limit: u32,
) -> Result<Vec<note_sync::UnsealedNoteResolutionIntent>, String> {
    let mut connection = open_notes_database(true)?;
    require_sqlite_notes_owner(&connection)?;
    note_sync::list_unsealed_note_resolution_intents(&mut connection, &account_id, limit)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn commit_sealed_note_resolution_event(
    command: note_sync::CommitSealedNoteResolutionCommand,
) -> Result<note_sync::CommitSealedNoteResolutionResult, String> {
    let mut connection = open_notes_database(true)?;
    require_sqlite_notes_owner(&connection)?;
    note_sync::commit_sealed_note_resolution_event(&mut connection, &command)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn read_note_resolution_readiness(
    account_id: String,
    event_id: String,
) -> Result<note_sync::NoteResolutionReadiness, String> {
    let mut connection = open_notes_database(true)?;
    require_sqlite_notes_owner(&connection)?;
    note_sync::read_note_resolution_readiness(&mut connection, &account_id, &event_id)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn list_sealed_note_resolution_uploads(
    command: note_sync::ListSealedNoteResolutionUploadsCommand,
) -> Result<Vec<note_sync::SealedNoteResolutionUploadItem>, String> {
    let mut connection = open_notes_database(true)?;
    require_sqlite_notes_owner(&connection)?;
    note_sync::list_sealed_note_resolution_uploads(&mut connection, &command)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn commit_note_resolution_upload_acceptance(
    command: note_sync::CommitNoteResolutionUploadAcceptanceCommand,
) -> Result<Vec<note_sync::CommitNoteResolutionUploadAcceptanceResult>, String> {
    let mut connection = open_notes_database(true)?;
    require_sqlite_notes_owner(&connection)?;
    note_sync::commit_note_resolution_upload_acceptance(&mut connection, &command)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn commit_note_sync_upload_acceptance(
    command: note_sync::CommitNoteSyncUploadAcceptanceCommand,
) -> Result<Vec<note_sync::CommitNoteSyncUploadAcceptanceResult>, String> {
    let mut connection = open_notes_database(true)?;
    require_sqlite_notes_owner(&connection)?;
    note_sync::commit_note_sync_upload_acceptance(&mut connection, &command)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn record_note_sync_upload_failure(
    command: note_sync::RecordNoteSyncUploadFailureCommand,
) -> Result<(), String> {
    let mut connection = open_notes_database(true)?;
    require_sqlite_notes_owner(&connection)?;
    note_sync::record_note_sync_upload_failure(&mut connection, &command)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn ensure_cloud_account_binding(
    command: account_binding::EnsureCloudAccountBindingCommand,
) -> Result<account_binding::EnsureCloudAccountBindingResult, String> {
    let mut connection = open_notes_database(true)?;
    ensure_cloud_account_binding_on_connection(&mut connection, &command)
}

fn ensure_cloud_account_binding_on_connection(
    connection: &mut rusqlite::Connection,
    command: &account_binding::EnsureCloudAccountBindingCommand,
) -> Result<account_binding::EnsureCloudAccountBindingResult, String> {
    require_sqlite_notes_owner(connection)?;
    account_binding::ensure_cloud_account_binding(connection, command)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn provision_cloud_identity(
    command: account_binding::CloudIdentityCommand,
) -> Result<account_binding::CloudIdentity, String> {
    let mut connection = open_notes_database(true)?;
    provision_cloud_identity_on_connection(&mut connection, &command)
}

fn provision_cloud_identity_on_connection(
    connection: &mut rusqlite::Connection,
    command: &account_binding::CloudIdentityCommand,
) -> Result<account_binding::CloudIdentity, String> {
    require_sqlite_notes_owner(connection)?;
    account_binding::provision_cloud_identity(connection, &command.canonical_user_id)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn read_cloud_identity(
    command: account_binding::CloudIdentityCommand,
) -> Result<Option<account_binding::CloudIdentity>, String> {
    let mut connection = open_notes_database(true)?;
    read_cloud_identity_on_connection(&mut connection, &command)
}

fn read_cloud_identity_on_connection(
    connection: &mut rusqlite::Connection,
    command: &account_binding::CloudIdentityCommand,
) -> Result<Option<account_binding::CloudIdentity>, String> {
    require_sqlite_notes_owner(connection)?;
    account_binding::read_cloud_identity(connection, &command.canonical_user_id)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn read_note_sync_pull_state(
    command: note_sync::ReadNoteSyncPullStateCommand,
) -> Result<note_sync::NoteSyncPullState, String> {
    let mut connection = open_notes_database(true)?;
    require_sqlite_notes_owner(&connection)?;
    note_sync::read_note_sync_pull_state(&mut connection, &command)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn prepare_note_sync_ack(
    command: note_sync::PrepareNoteSyncAckCommand,
) -> Result<note_sync::NoteSyncAckCandidate, String> {
    let mut connection = open_notes_database(true)?;
    require_sqlite_notes_owner(&connection)?;
    note_sync::prepare_note_sync_ack(&mut connection, &command).map_err(|error| error.to_string())
}

#[tauri::command]
fn commit_note_sync_ack(
    command: note_sync::CommitNoteSyncAckCommand,
) -> Result<note_sync::CommitNoteSyncAckResult, String> {
    let mut connection = open_notes_database(true)?;
    require_sqlite_notes_owner(&connection)?;
    note_sync::commit_note_sync_ack(&mut connection, &command).map_err(|error| error.to_string())
}

#[tauri::command]
fn list_received_note_sync_inbox(
    command: note_sync::ListReceivedNoteSyncInboxCommand,
) -> Result<Vec<note_sync::ReceivedNoteSyncInboxItem>, String> {
    let connection = open_notes_database(true)?;
    require_sqlite_notes_owner(&connection)?;
    note_sync::list_received_note_sync_inbox(&connection, &command)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn list_received_note_sync_inbox_page(
    command: note_sync::ListReceivedNoteSyncInboxPageCommand,
) -> Result<Vec<note_sync::ReceivedNoteSyncInboxItem>, String> {
    let connection = open_notes_database(true)?;
    require_sqlite_notes_owner(&connection)?;
    note_sync::list_received_note_sync_inbox_page(&connection, &command)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn list_orphan_note_resolution_inbox(
    command: note_sync::ListOrphanNoteResolutionInboxCommand,
) -> Result<Vec<note_sync::ReceivedNoteSyncInboxItem>, String> {
    let connection = open_notes_database(true)?;
    require_sqlite_notes_owner(&connection)?;
    note_sync::list_orphan_note_resolution_inbox(&connection, &command)
        .map_err(|error| error.to_string())
}

/// Receives only short-lived, TypeScript-authenticated Note plaintext bytes.
/// The command has no SQL/capability/path inputs; Rust rechecks durable scope,
/// encrypted-object identity, plaintext structure and every mutable decision.
#[tauri::command]
fn apply_verified_received_note(
    command: note_sync::ApplyVerifiedReceivedNoteIpcCommand,
) -> Result<note_sync::ApplyVerifiedReceivedNoteResult, String> {
    let ordinary = open_notes_database(true)?;
    require_sqlite_notes_owner(&ordinary)?;
    drop(ordinary);
    let database = sqlite_data_root()?.join("nfprogress.db");
    let mut connection = sqlite::open_privileged_remote_apply_database(&database)
        .map_err(|error| error.to_string())?;
    note_sync::apply_verified_received_note_ipc(&mut connection, command)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn apply_verified_received_content_note(
    command: note_sync::ApplyVerifiedReceivedNoteIpcCommand,
) -> Result<String, String> {
    let ordinary = open_notes_database(true)?;
    require_sqlite_notes_owner(&ordinary)?;
    drop(ordinary);
    let mut connection =
        sqlite::open_privileged_remote_apply_database(&sqlite_data_root()?.join("nfprogress.db"))
            .map_err(|e| e.to_string())?;
    note_sync::apply_verified_received_content_note_ipc(&mut connection, command)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn record_content_note_blocker(
    command: note_sync::ApplyVerifiedReceivedNoteIpcCommand,
    code: String,
) -> Result<(), String> {
    let mut connection =
        sqlite::open_privileged_remote_apply_database(&sqlite_data_root()?.join("nfprogress.db"))
            .map_err(|e| e.to_string())?;
    note_sync::record_content_note_blocker(&mut connection, &command, &code)
        .map_err(|e| e.to_string())
}

/// Dormant peer-only resolution-v2 apply boundary. TypeScript supplies
/// plaintext only after authenticated AMK decryption; native code rechecks the
/// durable inbox, opaque object and causal proof before any protected write.
#[tauri::command]
fn apply_verified_received_resolution_v2(
    command: note_sync::ApplyVerifiedReceivedResolutionV2IpcCommand,
) -> Result<note_sync::ApplyVerifiedReceivedResolutionV2Result, String> {
    let ordinary = open_notes_database(true)?;
    require_sqlite_notes_owner(&ordinary)?;
    drop(ordinary);
    let database = sqlite_data_root()?.join("nfprogress.db");
    let mut connection = sqlite::open_privileged_remote_apply_database(&database)
        .map_err(|error| error.to_string())?;
    note_sync::apply_verified_received_resolution_v2_ipc(&mut connection, command)
        .map_err(|error| error.to_string())
}

/// Dormant self-echo boundary for a locally-created resolution returned by
/// pull. It records server acceptance and durable apply evidence without
/// repeating the already-completed local Note mutation.
#[tauri::command]
fn reconcile_verified_received_resolution_self_echo(
    command: note_sync::ReconcileVerifiedReceivedResolutionSelfEchoIpcCommand,
) -> Result<note_sync::ReconcileVerifiedReceivedResolutionSelfEchoResult, String> {
    let ordinary = open_notes_database(true)?;
    require_sqlite_notes_owner(&ordinary)?;
    drop(ordinary);
    let database = sqlite_data_root()?.join("nfprogress.db");
    let mut connection = sqlite::open_privileged_remote_apply_database(&database)
        .map_err(|error| error.to_string())?;
    note_sync::reconcile_verified_received_resolution_self_echo_ipc(&mut connection, command)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn commit_note_sync_inbound_page(
    command: note_sync::CommitNoteSyncInboundPageCommand,
) -> Result<note_sync::CommitNoteSyncInboundPageResult, String> {
    let mut connection = open_notes_database(true)?;
    require_sqlite_notes_owner(&connection)?;
    note_sync::commit_note_sync_inbound_page(&mut connection, &command)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn commit_v3_sync_inbound_page(
    command: note_sync::CommitMixedSyncInboundPageCommand,
) -> Result<note_sync::CommitNoteSyncInboundPageResult, String> {
    let mut connection = open_notes_database(true)?;
    require_sqlite_notes_owner(&connection)?;
    note_sync::commit_mixed_sync_inbound_page(&mut connection, &command).map_err(|e| e.to_string())
}

#[tauri::command]
fn map_sync_command(
    scope: project_metadata_sync::MetadataScope,
    request: map_sync::Request,
) -> Result<serde_json::Value, String> {
    let mut db = metadata_connection(&scope)?;
    use map_sync::Request::*;
    let result = (|| -> Result<serde_json::Value, map_sync::Error> {
        match request {
            View { project_id } => map_sync::view(&db, &scope, &project_id),
            Begin { project_id, now } => map_sync::begin(&mut db, &scope, &project_id, &now),
            Pending { sealed, now } => {
                map_sync::advance(&mut db, &scope.account_id, &now)?;
                Ok(serde_json::json!(map_sync::pending(
                    &db,
                    &scope.account_id,
                    &scope.device_id,
                    sealed
                )?))
            }
            Seal {
                event_id,
                frame,
                nonce,
                ciphertext,
            } => {
                map_sync::seal(
                    &mut db,
                    &scope.account_id,
                    &event_id,
                    &frame,
                    &nonce,
                    &ciphertext,
                )?;
                Ok(serde_json::Value::Null)
            }
            Receipt {
                event_id,
                server_sequence,
                duplicate,
                now,
            } => {
                map_sync::receipt(
                    &mut db,
                    &scope.account_id,
                    &scope.device_id,
                    &event_id,
                    server_sequence,
                    duplicate,
                    &now,
                )?;
                Ok(serde_json::Value::Null)
            }
            Block {
                event_id,
                nonce,
                ciphertext,
                code,
            } => {
                if !frame_compression::CODES.contains(&code.as_str())
                    && !matches!(code.as_str(), "invalid_map_payload" | "decrypt_failed")
                {
                    return Err(map_sync::Error::from("invalid_map_blocker".to_string()));
                }
                db.execute("UPDATE cloud_sync_inbox SET state='orphan',error_code=?1 WHERE account_id=?2 AND event_id=?3 AND entity_type='map' AND state IN ('received','orphan') AND EXISTS(SELECT 1 FROM cloud_sync_event_objects o WHERE o.account_id=?2 AND o.event_id=?3 AND o.nonce=?4 AND o.ciphertext=?5)",rusqlite::params![code,scope.account_id,event_id,nonce,ciphertext])?;
                Ok(serde_json::Value::Null)
            }
            Received { after, limit } => Ok(serde_json::json!(map_sync::received(
                &db,
                &scope.account_id,
                after,
                limit
            )?)),
            Apply {
                frame,
                nonce,
                ciphertext,
            } => {
                drop(db);
                let mut privileged = sqlite::open_privileged_remote_apply_database(
                    &sqlite_data_root()
                        .map_err(map_sync::Error::from)?
                        .join("nfprogress.db"),
                )?;
                Ok(serde_json::json!(map_sync::apply(
                    &mut privileged,
                    &scope,
                    &frame,
                    &nonce,
                    &ciphertext
                )?))
            }
            Import {
                decision,
                keep_local,
                now,
            } => {
                drop(db);
                let mut privileged = sqlite::open_privileged_remote_apply_database(
                    &sqlite_data_root()
                        .map_err(map_sync::Error::from)?
                        .join("nfprogress.db"),
                )?;
                Ok(serde_json::json!(map_sync::import_choice(
                    &mut privileged,
                    &scope,
                    &decision,
                    keep_local,
                    &now
                )?))
            }
            Decide { decision, now } => Ok(serde_json::json!(map_sync::decide(
                &mut db, &scope, &decision, &now
            )?)),
            Delete {
                project_id,
                stage_id,
                expected,
                now,
            } => {
                let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                map_sync::check_expected(
                    &tx,
                    &project_id,
                    stage_id.as_deref(),
                    false,
                    Some(&expected),
                    &now,
                )?;
                if !map_sync::local_edit(
                    &tx,
                    &project_id,
                    stage_id.as_deref(),
                    &serde_json::Value::Null,
                    None,
                    &now,
                )? {
                    return Err(map_sync::Error::from("map_authority_required".to_string()));
                }
                tx.commit()?;
                Ok(serde_json::Value::Null)
            }
        }
    })();
    result.map_err(|e| e.to_string())
}

#[tauri::command]
fn get_current_writing_day() -> Result<String, String> {
    let db = open_projects_database()?;
    streaks::logical_writing_day(&db)
}

#[tauri::command]
fn game_sync_command(
    scope: project_metadata_sync::MetadataScope,
    request: game_control::Request,
) -> Result<serde_json::Value, String> {
    let mut db = open_projects_database()?;
    require_projects_owner(&db)?;
    game_control::assert_scope(&db, &scope)?;
    match request {
        game_control::Request::Apply {
            frame,
            nonce,
            ciphertext,
            project,
        } => {
            drop(db);
            let mut privileged = sqlite::open_privileged_remote_apply_database(
                &sqlite_data_root()?.join("nfprogress.db"),
            )
            .map_err(|_| "game_storage_error")?;
            Ok(serde_json::json!(game_sync::apply(
                &mut privileged,
                &scope,
                &frame,
                &nonce,
                &ciphertext,
                project
            )?))
        }
        game_control::Request::Rebuild { owner_key } => {
            drop(db);
            let mut privileged = sqlite::open_privileged_remote_apply_database(
                &sqlite_data_root()?.join("nfprogress.db"),
            )
            .map_err(|_| "game_storage_error")?;
            game_sync::rebuild(&mut privileged, &scope, &owner_key)?;
            Ok(serde_json::Value::Null)
        }
        ordinary => game_control::ordinary(&mut db, &scope, ordinary),
    }
}

#[tauri::command]
fn progress_sync_command(
    scope: project_metadata_sync::MetadataScope,
    request: progress_sync::Request,
) -> Result<serde_json::Value, String> {
    let mut db = open_projects_database()?;
    require_projects_owner(&db)?;
    project_metadata_sync::assert_runtime_scope(
        &db,
        &scope.account_id,
        &scope.canonical_user_id,
        &scope.device_id,
    )
    .map_err(|_| "progress_scope_mismatch")?;
    use progress_sync::Request::*;
    match request {
        View { project_id } => progress_sync::view(&db, &scope, &project_id),
        Begin { project_id, now } => {
            progress_sync::capture(&mut db, &scope, &project_id, &now)?;
            progress_sync::view(&db, &scope, &project_id)
        }
        Pending { sealed, now } => {
            progress_sync::continue_capture(&mut db, &scope.account_id, &now)?;
            Ok(serde_json::json!(progress_sync::pending(
                &db,
                &scope.account_id,
                &scope.device_id,
                sealed
            )?))
        }
        Seal {
            event_id,
            frame,
            nonce,
            ciphertext,
        } => {
            progress_sync::seal(
                &mut db,
                &scope.account_id,
                &scope.device_id,
                &event_id,
                &frame,
                &nonce,
                &ciphertext,
            )?;
            Ok(serde_json::Value::Null)
        }
        Receipt {
            event_id,
            server_sequence,
            duplicate,
            now,
        } => {
            progress_sync::receipt(
                &mut db,
                &scope.account_id,
                &scope.device_id,
                &event_id,
                server_sequence,
                duplicate,
                &now,
            )?;
            Ok(serde_json::Value::Null)
        }
        Received { after, limit } => Ok(serde_json::json!(progress_sync::received(
            &db,
            &scope.account_id,
            after,
            limit
        )?)),
        Apply {
            frame,
            nonce,
            ciphertext,
        } => {
            drop(db);
            let mut privileged = sqlite::open_privileged_remote_apply_database(
                &sqlite_data_root()?.join("nfprogress.db"),
            )
            .map_err(|_| "progress_storage_unavailable")?;
            Ok(serde_json::json!(progress_sync::apply(
                &mut privileged,
                &scope,
                &frame,
                &nonce,
                &ciphertext
            )?))
        }
        Decide { decision, now } => Ok(serde_json::json!(progress_sync::decide(
            &mut db, &scope, &decision, &now
        )?)),
        Rebuild {
            project_id,
            stage_id,
        } => {
            drop(db);
            let mut privileged = sqlite::open_privileged_remote_apply_database(
                &sqlite_data_root()?.join("nfprogress.db"),
            )
            .map_err(|_| "progress_storage_unavailable")?;
            progress_sync::rebuild(&mut privileged, &scope, &project_id, stage_id.as_deref())?;
            Ok(serde_json::Value::Null)
        }
        Block {
            event_id,
            nonce,
            ciphertext,
            code,
        } => {
            if !frame_compression::CODES.contains(&code.as_str())
                && !matches!(
                    code.as_str(),
                    "invalid_progress_payload"
                        | "decrypt_failed"
                        | "progress_codec_unsupported"
                        | "progress_resource_limit"
                        | "progress_scope_mismatch"
                )
            {
                return Err("invalid_progress_blocker".into());
            }
            db.execute("UPDATE cloud_sync_inbox SET state='orphan',error_code=?1 WHERE account_id=?2 AND event_id=?3 AND entity_type='progress' AND state IN ('received','orphan') AND EXISTS(SELECT 1 FROM cloud_sync_event_objects o WHERE o.account_id=?2 AND o.event_id=?3 AND o.nonce=?4 AND o.ciphertext=?5)",rusqlite::params![code,scope.account_id,event_id,nonce,ciphertext]).map_err(|_|"progress_storage_unavailable")?;
            Ok(serde_json::Value::Null)
        }
    }
}

#[tauri::command]
fn document_sync_command(
    scope: project_metadata_sync::MetadataScope,
    request: document_sync::Request,
) -> Result<serde_json::Value, String> {
    let mut db = metadata_connection(&scope)?;
    use document_sync::Request::*;
    let result = (|| -> Result<serde_json::Value, document_sync::Error> {
        match request {
            View { project_id } => document_sync::view(&db, &scope, &project_id),
            Begin { project_id, now } => document_sync::begin(&mut db, &scope, &project_id, &now),
            Pending { sealed, now } => {
                document_sync::advance(&mut db, &scope.account_id, &now)?;
                Ok(serde_json::json!(document_sync::pending(
                    &db,
                    &scope.account_id,
                    &scope.device_id,
                    sealed
                )?))
            }
            Seal {
                event_id,
                frame,
                nonce,
                ciphertext,
            } => {
                document_sync::seal(
                    &mut db,
                    &scope.account_id,
                    &event_id,
                    &frame,
                    &nonce,
                    &ciphertext,
                )?;
                Ok(serde_json::Value::Null)
            }
            Receipt {
                event_id,
                server_sequence,
                duplicate,
                now,
            } => {
                document_sync::receipt(
                    &mut db,
                    &scope.account_id,
                    &scope.device_id,
                    &event_id,
                    server_sequence,
                    duplicate,
                    &now,
                )?;
                Ok(serde_json::Value::Null)
            }
            Block {
                event_id,
                nonce,
                ciphertext,
                code,
            } => {
                if !frame_compression::CODES.contains(&code.as_str())
                    && !matches!(
                        code.as_str(),
                        "invalid_document_payload"
                            | "decrypt_failed"
                            | "document_codec_unsupported"
                            | "document_resource_limit"
                            | "document_unsupported_extension"
                            | "document_unsupported_structure"
                            | "document_scope_mismatch"
                    )
                {
                    return Err(document_sync::Error::from(
                        "invalid_document_blocker".to_string(),
                    ));
                }
                db.execute("UPDATE cloud_sync_inbox SET state='orphan',error_code=?1 WHERE account_id=?2 AND event_id=?3 AND entity_type='document' AND state IN ('received','orphan') AND EXISTS(SELECT 1 FROM cloud_sync_event_objects o WHERE o.account_id=?2 AND o.event_id=?3 AND o.nonce=?4 AND o.ciphertext=?5)",rusqlite::params![code,scope.account_id,event_id,nonce,ciphertext])?;
                Ok(serde_json::Value::Null)
            }
            Received { after, limit } => Ok(serde_json::json!(document_sync::received(
                &db,
                &scope.account_id,
                after,
                limit
            )?)),
            Apply {
                frame,
                nonce,
                ciphertext,
            } => {
                drop(db);
                let mut privileged = sqlite::open_privileged_remote_apply_database(
                    &sqlite_data_root()
                        .map_err(document_sync::Error::from)?
                        .join("nfprogress.db"),
                )?;
                Ok(serde_json::json!(document_sync::apply(
                    &mut privileged,
                    &scope,
                    &frame,
                    &nonce,
                    &ciphertext
                )?))
            }
            Decide { decision, now } => Ok(serde_json::json!(document_sync::decide(
                &mut db, &scope, &decision, &now
            )?)),
            Move {
                project_id,
                document_id,
                stage_id,
                expected,
                now,
            } => {
                document_sync::assert_project_scope(&db, &scope, &project_id)?;
                document_sync::move_scope(
                    &mut db,
                    &project_id,
                    &document_id,
                    stage_id.as_deref(),
                    &expected,
                    &now,
                )?;
                Ok(serde_json::Value::Null)
            }
            Delete {
                project_id,
                document_id,
                expected,
                now,
            } => {
                document_sync::assert_project_scope(&db, &scope, &project_id)?;
                let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                if !document_sync::normal(
                    &tx,
                    &project_id,
                    &document_id,
                    serde_json::Value::Null,
                    Some(&expected),
                    &now,
                )? {
                    return Err(document_sync::Error::from(
                        "document_authority_required".to_string(),
                    ));
                }
                tx.commit()?;
                Ok(serde_json::Value::Null)
            }
        }
    })();
    result.map_err(|e| e.to_string())
}

#[tauri::command]
fn read_account_catalog(
    scope: project_metadata_sync::MetadataScope,
) -> Result<serde_json::Value, String> {
    let db = metadata_connection(&scope)?;
    account_catalog::authority(&db, &scope).map_err(|e| e.to_string())
}
#[tauri::command]
fn begin_account_catalog(
    scope: project_metadata_sync::MetadataScope,
    now: String,
) -> Result<serde_json::Value, String> {
    let mut db = metadata_connection(&scope)?;
    account_catalog::begin(&mut db, &scope, &now).map_err(|e| e.to_string())
}
#[tauri::command]
fn decide_account_catalog(
    scope: project_metadata_sync::MetadataScope,
    decision: account_catalog::Decision,
    now: String,
) -> Result<String, String> {
    let mut db = metadata_connection(&scope)?;
    account_catalog::decide(&mut db, &scope, &decision, &now).map_err(|e| e.to_string())
}
#[tauri::command]
fn pending_account_catalog(
    scope: project_metadata_sync::MetadataScope,
    sealed: bool,
    now: String,
) -> Result<Vec<serde_json::Value>, String> {
    let mut db = metadata_connection(&scope)?;
    account_catalog::reconcile_connections(&mut db, &scope, &now).map_err(|e| e.to_string())?;
    account_catalog::pending(&db, &scope, sealed).map_err(|e| e.to_string())
}
#[tauri::command]
fn seal_account_catalog(
    scope: project_metadata_sync::MetadataScope,
    event_id: String,
    frame: Vec<u8>,
    nonce: Vec<u8>,
    ciphertext: Vec<u8>,
) -> Result<(), String> {
    let mut db = metadata_connection(&scope)?;
    account_catalog::seal(&mut db, &scope, &event_id, &frame, &nonce, &ciphertext)
        .map_err(|e| e.to_string())
}
#[tauri::command]
fn receipt_account_catalog(
    scope: project_metadata_sync::MetadataScope,
    event_id: String,
    server_sequence: i64,
) -> Result<(), String> {
    let mut db = metadata_connection(&scope)?;
    account_catalog::receipt(&mut db, &scope, &event_id, server_sequence).map_err(|e| e.to_string())
}
#[tauri::command]
fn apply_account_catalog(
    scope: project_metadata_sync::MetadataScope,
    event_id: String,
    frame: Vec<u8>,
    nonce: Vec<u8>,
    ciphertext: Vec<u8>,
) -> Result<String, String> {
    let mut db = metadata_connection(&scope)?;
    account_catalog::apply(&mut db, &scope, &event_id, &frame, &nonce, &ciphertext)
        .map_err(|e| e.to_string())
}
#[tauri::command]
fn reorder_project_folders(folder_ids: Vec<String>) -> Result<(), String> {
    let mut db = open_projects_database()?;
    require_projects_owner(&db)?;
    let now: String = db
        .query_row("SELECT strftime('%Y-%m-%dT%H:%M:%f000Z','now')", [], |r| {
            r.get(0)
        })
        .map_err(|e| e.to_string())?;
    if !account_catalog::normal(
        &mut db,
        "folder_order",
        "folder_order",
        serde_json::json!({"ids":folder_ids}),
        &now,
    )
    .map_err(|e| e.to_string())?
    {
        return Err("catalog_authority_required".into());
    }
    Ok(())
}

#[tauri::command]
fn list_received_account_objects(
    scope: project_metadata_sync::MetadataScope,
    limit: u32,
    after: i64,
) -> Result<Vec<account_sync::ReceivedAccountItem>, String> {
    let connection = metadata_connection(&scope)?;
    account_sync::received(&connection, &scope, limit, after).map_err(|e| e.to_string())
}
#[tauri::command]
fn block_account_object(
    scope: project_metadata_sync::MetadataScope,
    event_id: String,
    nonce: Vec<u8>,
    ciphertext: Vec<u8>,
    code: String,
) -> Result<(), String> {
    let connection = metadata_connection(&scope)?;
    account_sync::block(&connection, &scope, &event_id, &nonce, &ciphertext, &code)
        .map_err(|e| e.to_string())
}

fn metadata_connection(
    scope: &project_metadata_sync::MetadataScope,
) -> Result<rusqlite::Connection, String> {
    let connection = open_notes_database(true)?;
    require_sqlite_notes_owner(&connection)?;
    project_metadata_sync::assert_runtime_scope(
        &connection,
        &scope.account_id,
        &scope.canonical_user_id,
        &scope.device_id,
    )
    .map_err(|e| e.to_string())?;
    Ok(connection)
}

#[tauri::command]
fn begin_content_note_migration(
    scope: project_metadata_sync::MetadataScope,
    project_id: String,
    now: String,
) -> Result<serde_json::Value, String> {
    let mut db = metadata_connection(&scope)?;
    content_note_writer::begin(&mut db, &scope, &project_id, &now)
}
#[tauri::command]
fn read_content_note_migration(
    scope: project_metadata_sync::MetadataScope,
    project_id: String,
) -> Result<serde_json::Value, String> {
    let db = metadata_connection(&scope)?;
    content_note_writer::view(&db, &scope, &project_id)
}
#[tauri::command]
fn read_content_note_conflicts(
    scope: project_metadata_sync::MetadataScope,
    project_id: String,
) -> Result<Vec<serde_json::Value>, String> {
    let db = metadata_connection(&scope)?;
    content_note_writer::conflicts(&db, &scope, &project_id)
}
#[tauri::command]
fn choose_content_note_version(
    scope: project_metadata_sync::MetadataScope,
    project_id: String,
    decision: serde_json::Value,
    selected: String,
    now: String,
) -> Result<(), String> {
    let mut db = metadata_connection(&scope)?;
    if decision["kind"] == "import" {
        let command = content_note_writer::prepare_import_choice(
            &mut db,
            &scope,
            &project_id,
            &decision,
            &selected,
            &now,
        )?;
        drop(db);
        let mut privileged = sqlite::open_privileged_remote_apply_database(
            &sqlite_data_root()?.join("nfprogress.db"),
        )
        .map_err(|e| e.to_string())?;
        note_sync::apply_verified_received_content_note_ipc(&mut privileged, command)
            .map_err(|e| e.to_string())?;
        return Ok(());
    }
    let command = content_note_writer::prepare_choice(
        &mut db,
        &scope,
        &project_id,
        &decision,
        &selected,
        &now,
    )?;
    drop(db);
    let mut privileged =
        sqlite::open_privileged_remote_apply_database(&sqlite_data_root()?.join("nfprogress.db"))
            .map_err(|e| e.to_string())?;
    note_sync::apply_prepared_note_conflict_resolution(&mut privileged, &command)
        .map_err(|_| "stale_note_resolution")?;
    Ok(())
}
#[tauri::command]
fn list_pending_content_notes(
    scope: project_metadata_sync::MetadataScope,
    sealed: bool,
) -> Result<Vec<serde_json::Value>, String> {
    let db = metadata_connection(&scope)?;
    content_note_writer::pending(&db, &scope, sealed)
}
#[tauri::command]
fn seal_content_note(
    scope: project_metadata_sync::MetadataScope,
    event_id: String,
    frame: Vec<u8>,
    envelope: note_sync::EncryptedNoteSyncEnvelope,
) -> Result<(), String> {
    let mut db = metadata_connection(&scope)?;
    content_note_writer::seal(&mut db, &scope, &event_id, &frame, envelope)
}
#[tauri::command]
fn receipt_content_note(
    scope: project_metadata_sync::MetadataScope,
    event_id: String,
    server_sequence: i64,
    duplicate: bool,
) -> Result<(), String> {
    let mut db = metadata_connection(&scope)?;
    content_note_writer::receipt(&mut db, &scope, &event_id, server_sequence, duplicate)
}

#[tauri::command]
fn read_stage_structural_authority(
    scope: project_metadata_sync::MetadataScope,
    project_id: String,
) -> Result<stage_sync::StructuralView, String> {
    let db = metadata_connection(&scope)?;
    stage_sync::authority(&db, &scope.account_id, &project_id).map_err(|e| e.to_string())
}
#[tauri::command]
fn begin_stage_structural_migration(
    scope: project_metadata_sync::MetadataScope,
    project_id: String,
    now: String,
) -> Result<stage_sync::StructuralView, String> {
    let mut db = metadata_connection(&scope)?;
    stage_sync::begin(&mut db, &scope.account_id, &project_id, &now).map_err(|e| e.to_string())
}
#[tauri::command]
fn prepare_stage_structural_decision(
    scope: project_metadata_sync::MetadataScope,
    project_id: String,
    decision: stage_sync::Decision,
    now: String,
) -> Result<String, String> {
    let mut db = metadata_connection(&scope)?;
    stage_sync::decide(&mut db, &scope.account_id, &project_id, &decision, &now)
        .map_err(|e| e.to_string())
}
#[tauri::command]
fn list_pending_stage_structural(
    scope: project_metadata_sync::MetadataScope,
    sealed: bool,
) -> Result<Vec<serde_json::Value>, String> {
    let db = metadata_connection(&scope)?;
    stage_sync::pending_events(&db, &scope.account_id, &scope.device_id, sealed)
        .map_err(|e| e.to_string())
}
#[tauri::command]
fn commit_sealed_stage_structural(
    scope: project_metadata_sync::MetadataScope,
    event_id: String,
    frame: Vec<u8>,
    nonce: Vec<u8>,
    ciphertext: Vec<u8>,
) -> Result<(), String> {
    let mut db = metadata_connection(&scope)?;
    let e = stage_sync::unframe(&frame).map_err(|e| e.to_string())?;
    if e.header.device_id != scope.device_id
        || e.header.account_id != scope.canonical_user_id
        || e.header.event_id != event_id
    {
        return Err("structural_scope_mismatch".into());
    }
    stage_sync::seal(
        &mut db,
        &scope.account_id,
        &event_id,
        &frame,
        &nonce,
        &ciphertext,
    )
    .map_err(|e| e.to_string())
}
#[tauri::command]
fn commit_stage_structural_receipt(
    scope: project_metadata_sync::MetadataScope,
    event_id: String,
    server_sequence: i64,
    duplicate: bool,
    now: String,
) -> Result<(), String> {
    let mut db = metadata_connection(&scope)?;
    let owned:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_outbox WHERE account_id=?1 AND device_id=?2 AND event_id=?3 AND entity_type IN ('stage','stage_order'))",rusqlite::params![scope.account_id,scope.device_id,event_id],|r|r.get(0)).map_err(|e|e.to_string())?;
    if !owned {
        return Err("structural_scope_mismatch".into());
    }
    stage_sync::commit_receipt(
        &mut db,
        &scope.account_id,
        &scope.device_id,
        &event_id,
        server_sequence,
        duplicate,
        &now,
    )
    .map_err(|e| e.to_string())
}
#[tauri::command]
fn list_received_stage_structural(
    scope: project_metadata_sync::MetadataScope,
    limit: i64,
    after_server_sequence: i64,
) -> Result<Vec<serde_json::Value>, String> {
    let db = metadata_connection(&scope)?;
    stage_sync::received(&db, &scope.account_id, limit, after_server_sequence)
        .map_err(|e| e.to_string())
}
#[tauri::command]
fn apply_authenticated_stage_structural(
    scope: project_metadata_sync::MetadataScope,
    event_id: String,
    frame: Vec<u8>,
    nonce: Vec<u8>,
    ciphertext: Vec<u8>,
) -> Result<String, String> {
    let mut db = metadata_connection(&scope)?;
    stage_sync::apply_received(
        &mut db,
        &scope.account_id,
        &event_id,
        &frame,
        &nonce,
        &ciphertext,
    )
    .map(str::to_string)
    .map_err(|e| e.to_string())
}
#[tauri::command]
fn record_stage_structural_blocker(
    scope: project_metadata_sync::MetadataScope,
    event_id: String,
    reason: String,
) -> Result<(), String> {
    let db = metadata_connection(&scope)?;
    stage_sync::reader_blocker(&db, &scope.account_id, &event_id, &reason)
        .map_err(|e| e.to_string())
}
#[tauri::command]
fn retry_stage_structural(scope: project_metadata_sync::MetadataScope) -> Result<(), String> {
    let mut db = metadata_connection(&scope)?;
    stage_sync::retry(&mut db, &scope.account_id, 8).map_err(|e| e.to_string())?;
    let projects = {
        let mut q=db.prepare("SELECT project_id FROM cloud_sync_structural_migrations WHERE account_id=?1 UNION SELECT project_id FROM cloud_sync_structural_order_intents WHERE account_id=?1 LIMIT 32").map_err(|e|e.to_string())?;
        let rows = q
            .query_map([&scope.account_id], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        rows
    };
    for p in projects {
        stage_sync::advance(&mut db, &scope.account_id, &p).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
fn capture_project_metadata_candidate(
    scope: project_metadata_sync::MetadataScope,
    project_id: String,
    now: String,
) -> Result<String, String> {
    let mut connection = metadata_connection(&scope)?;
    project_metadata_sync::capture_legacy_candidate(
        &mut connection,
        &scope.account_id,
        &project_id,
        &now,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
fn read_project_metadata_migration_status(
    scope: project_metadata_sync::MetadataScope,
    project_id: String,
) -> Result<project_metadata_sync::MetadataMigrationStatus, String> {
    let connection = metadata_connection(&scope)?;
    project_metadata_sync::migration_status(&connection, &scope.account_id, &project_id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn prepare_project_metadata_genesis(
    scope: project_metadata_sync::MetadataScope,
    candidate_id: String,
    now: String,
) -> Result<String, String> {
    let mut connection = metadata_connection(&scope)?;
    project_metadata_sync::prepare_metadata_genesis(
        &mut connection,
        &scope.account_id,
        &candidate_id,
        &now,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
fn list_unsealed_project_metadata_genesis(
    scope: project_metadata_sync::MetadataScope,
) -> Result<Vec<serde_json::Value>, String> {
    let connection = metadata_connection(&scope)?;
    project_metadata_sync::unsealed_genesis(&connection, &scope.account_id, &scope.device_id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn commit_sealed_project_metadata_genesis(
    scope: project_metadata_sync::MetadataScope,
    event_id: String,
    nonce: Vec<u8>,
    ciphertext: Vec<u8>,
) -> Result<(), String> {
    let mut connection = metadata_connection(&scope)?;
    project_metadata_sync::commit_sealed_genesis(
        &mut connection,
        &scope.account_id,
        &scope.device_id,
        &event_id,
        &nonce,
        &ciphertext,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
fn list_sealed_project_metadata_genesis(
    scope: project_metadata_sync::MetadataScope,
) -> Result<Vec<project_metadata_sync::MetadataUploadItem>, String> {
    let connection = metadata_connection(&scope)?;
    project_metadata_sync::sealed_genesis(&connection, &scope.account_id, &scope.device_id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn commit_project_metadata_upload_receipt(
    scope: project_metadata_sync::MetadataScope,
    event_id: String,
    server_sequence: i64,
    duplicate: bool,
    now: String,
) -> Result<(), String> {
    let mut connection = metadata_connection(&scope)?;
    project_metadata_sync::commit_upload_receipt(
        &mut connection,
        &scope.account_id,
        &scope.device_id,
        &event_id,
        server_sequence,
        duplicate,
        &now,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
fn project_cover_command(
    scope: project_metadata_sync::MetadataScope,
    project_id: String,
    action: String,
    data: serde_json::Value,
    now: String,
) -> Result<serde_json::Value, String> {
    let mut connection = metadata_connection(&scope)?;
    project_cover_sync::command(&mut connection, &scope, &project_id, &action, &data, &now)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn list_received_project_metadata(
    scope: project_metadata_sync::MetadataScope,
    limit: i64,
    after_server_sequence: i64,
) -> Result<Vec<project_metadata_sync::MetadataInboxItem>, String> {
    let connection = metadata_connection(&scope)?;
    project_metadata_sync::received_metadata(
        &connection,
        &scope.account_id,
        limit,
        after_server_sequence,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
fn record_project_metadata_compression_blocker(
    scope: project_metadata_sync::MetadataScope,
    event_id: String,
    nonce: Vec<u8>,
    ciphertext: Vec<u8>,
    code: String,
) -> Result<(), String> {
    let db = metadata_connection(&scope)?;
    project_metadata_sync::compression_reader_blocker(
        &db,
        &scope.account_id,
        &event_id,
        &nonce,
        &ciphertext,
        &code,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
fn apply_authenticated_project_metadata(
    scope: project_metadata_sync::MetadataScope,
    project_id: String,
    plaintext: Vec<u8>,
    nonce: Vec<u8>,
    ciphertext: Vec<u8>,
    now: String,
) -> Result<String, String> {
    let mut connection = metadata_connection(&scope)?;
    project_metadata_sync::preserve_authenticated_event_checked(
        &mut connection,
        &scope.account_id,
        &project_id,
        &plaintext,
        &now,
        Some((&nonce, &ciphertext)),
    )
    .map(str::to_string)
    .map_err(|e| e.to_string())
}

#[tauri::command]
fn read_project_metadata_import(
    scope: project_metadata_sync::MetadataScope,
    project_id: String,
    bootstrap_id: String,
) -> Result<project_metadata_sync::MetadataImportProgress, String> {
    let mut connection = metadata_connection(&scope)?;
    project_metadata_sync::read_metadata_import(
        &mut connection,
        &scope.account_id,
        &project_id,
        &bootstrap_id,
    )
    .map_err(|e| e.to_string())
}
#[tauri::command]
fn commit_project_metadata_import_page(
    scope: project_metadata_sync::MetadataScope,
    project_id: String,
    bootstrap_id: String,
    page: project_metadata_sync::MetadataImportPage,
) -> Result<project_metadata_sync::MetadataImportProgress, String> {
    let mut connection = metadata_connection(&scope)?;
    project_metadata_sync::commit_metadata_import_page(
        &mut connection,
        &scope.account_id,
        &scope.canonical_user_id,
        &project_id,
        &bootstrap_id,
        &page,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
fn read_project_metadata_authority(
    scope: project_metadata_sync::MetadataScope,
    project_id: String,
) -> Result<project_metadata_sync::MetadataAuthorityView, String> {
    let connection = metadata_connection(&scope)?;
    project_metadata_sync::authority_view(&connection, &scope.account_id, &project_id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn adopt_authenticated_project_metadata(
    scope: project_metadata_sync::MetadataScope,
    project_id: String,
    expected_head: String,
    expected_local: serde_json::Value,
    now: String,
) -> Result<project_metadata_sync::MetadataAuthorityView, String> {
    let mut connection = metadata_connection(&scope)?;
    project_metadata_sync::adopt_authenticated_metadata(
        &mut connection,
        &scope.account_id,
        &project_id,
        &expected_head,
        &expected_local,
        &now,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
fn prepare_project_metadata_change(
    scope: project_metadata_sync::MetadataScope,
    project_id: String,
    kind: String,
    selected_event_id: Option<String>,
    proposed: Option<serde_json::Value>,
    expected_local: serde_json::Value,
    expected_tips: Vec<String>,
    now: String,
) -> Result<String, String> {
    let mut connection = metadata_connection(&scope)?;
    project_metadata_sync::prepare_authoritative_change(
        &mut connection,
        &scope.account_id,
        &project_id,
        &scope.device_id,
        &kind,
        selected_event_id.as_deref(),
        proposed.as_ref(),
        &expected_local,
        &expected_tips,
        &now,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
fn create_note(
    project_id: String,
    stage_id: Option<String>,
    content_format: Option<String>,
) -> Result<serde_json::Value, String> {
    let mut connection = open_notes_database(true)?;
    require_sqlite_notes_owner(&connection)?;
    let id = new_note_id()?;
    create_note_with_format_in_connection(
        &mut connection,
        &project_id,
        stage_id.as_deref(),
        &id,
        content_format.as_deref().unwrap_or("html"),
    )?;
    get_note(project_id, id, stage_id)?.ok_or_else(|| "Созданная заметка не найдена.".to_string())
}

fn prepare_direct_note_intent(
    transaction: &rusqlite::Transaction<'_>,
    project_id: &str,
    note_id: &str,
    operation: note_sync::NoteSyncOperation,
    updated_at: &str,
    deleted_at: Option<&str>,
    snapshot_json: &str,
) -> Result<(), String> {
    note_sync::prepare_unsealed_note_intent(
        transaction,
        note_sync::PrepareNoteIntent {
            project_id,
            entity_id: note_id,
            operation,
            updated_at,
            deleted_at,
            snapshot_json,
            state_updated_at: updated_at,
        },
    )
    .map(|_| ())
    .map_err(|error| error.user_message().to_string())
}

fn create_note_in_connection(
    connection: &mut rusqlite::Connection,
    project_id: &str,
    stage_id: Option<&str>,
    note_id: &str,
) -> Result<String, String> {
    create_note_with_format_in_connection(connection, project_id, stage_id, note_id, "html")
}

fn create_note_with_format_in_connection(
    connection: &mut rusqlite::Connection,
    project_id: &str,
    stage_id: Option<&str>,
    note_id: &str,
    content_format: &str,
) -> Result<String, String> {
    if !matches!(content_format, "plain" | "html") {
        return Err("invalid_note_payload".into());
    }
    let transaction = connection
        .transaction()
        .map_err(|error| error.to_string())?;
    require_note_relation(&transaction, project_id, stage_id)?;
    require_note_writable(&transaction, project_id, stage_id)?;
    let now = note_time(&transaction)?;
    let sort_order: i64 = transaction.query_row(
        "SELECT COALESCE(MAX(json_extract(payload_json, '$.sort_order')), -1) + 1 FROM notes WHERE project_id = ?1",
        [project_id], |row| row.get(0),
    ).map_err(|error| error.to_string())?;
    let payload = serde_json::json!({"id": note_id, "project_id": project_id, "stage_id": stage_id, "title": "", "content": "", "content_format": content_format, "checklist": [], "color": "default", "pinned": false, "archived": false, "sort_order": sort_order, "tags": [], "source_type": "project", "source_map_id": null, "source_node_id": null, "created_at": now, "updated_at": now, "revision": 0, "metadata": {}});
    let snapshot = payload.to_string();
    prepare_direct_note_intent(
        &transaction,
        project_id,
        note_id,
        note_sync::NoteSyncOperation::Upsert,
        &now,
        None,
        &snapshot,
    )?;
    transaction
        .execute(
            "INSERT INTO notes(id,project_id,stage_id,updated_at,payload_json) \
             VALUES(?1,?2,?3,?4,?5)",
            rusqlite::params![note_id, project_id, stage_id, now, snapshot],
        )
        .map_err(|_| "Не удалось сохранить новую заметку.".to_string())?;
    transaction.commit().map_err(|error| error.to_string())?;
    Ok(snapshot)
}

#[tauri::command]
fn update_note(
    project_id: String,
    note_id: String,
    patch: serde_json::Value,
    stage_id: Option<String>,
) -> Result<serde_json::Value, String> {
    let mut connection = open_notes_database(true)?;
    require_sqlite_notes_owner(&connection)?;
    update_note_in_connection(
        &mut connection,
        &project_id,
        &note_id,
        &patch,
        stage_id.as_deref(),
    )?;
    get_note(project_id, note_id, stage_id)?
        .ok_or_else(|| "Изменённая заметка не найдена.".to_string())
}

fn update_note_in_connection(
    connection: &mut rusqlite::Connection,
    project_id: &str,
    note_id: &str,
    patch: &serde_json::Value,
    stage_id: Option<&str>,
) -> Result<String, String> {
    let transaction = connection
        .transaction()
        .map_err(|error| error.to_string())?;
    require_note_relation(&transaction, project_id, stage_id)?;
    require_note_writable(&transaction, project_id, stage_id)?;
    let raw_id = raw_note_id(&note_id, stage_id.is_none()).to_string();
    let mut note: serde_json::Value = transaction
        .query_row(
            "SELECT payload_json FROM notes WHERE id = ?1 AND project_id = ?2",
            rusqlite::params![raw_id, project_id],
            |row| row.get::<_, String>(0),
        )
        .map_err(|_| "Заметка больше не существует.".to_string())
        .and_then(|value| serde_json::from_str(&value).map_err(|error| error.to_string()))?;
    let note_stage_id = note
        .get("stage_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    require_note_relation(&transaction, project_id, note_stage_id.as_deref())?;
    require_note_writable(&transaction, project_id, note_stage_id.as_deref())?;
    let patch = patch
        .as_object()
        .ok_or_else(|| "Некорректные данные заметки.".to_string())?;
    let map_note = note.get("source_type").and_then(|value| value.as_str()) == Some("mindmap")
        && patch.contains_key("content");
    let map_node_id = note
        .get("source_node_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let target = note
        .as_object_mut()
        .ok_or_else(|| "Некорректный payload заметки.".to_string())?;
    for key in patch.keys() {
        if !matches!(
            key.as_str(),
            "title" | "content" | "tags" | "checklist" | "color" | "pinned" | "archived"
        ) {
            return Err(format!("Недопустимое поле заметки: {key}"));
        }
    }
    for (key, value) in patch {
        match key.as_str() {
            "title" | "content" | "color" => {
                let text = value
                    .as_str()
                    .unwrap_or_else(|| if value.is_null() { "" } else { "\0" });
                if text.contains('\0') {
                    return Err(format!("Поле {key} имеет неверный тип."));
                }
                let limit = if key == "title" { 500 } else { 300_000 };
                if text.chars().count() > limit {
                    return Err(if key == "content" {
                        "Текст заметки слишком длинный.".to_string()
                    } else {
                        "Заголовок заметки слишком длинный.".to_string()
                    });
                }
                let normalized = if key == "color"
                    && !matches!(
                        text,
                        "default"
                            | "coral"
                            | "orange"
                            | "yellow"
                            | "green"
                            | "teal"
                            | "blue"
                            | "purple"
                            | "pink"
                            | "brown"
                            | "gray"
                    ) {
                    "default"
                } else {
                    text
                };
                target.insert(
                    key.clone(),
                    serde_json::Value::String(normalized.to_string()),
                );
            }
            "tags" => {
                if value.is_null() {
                    target.insert(key.clone(), serde_json::json!([]));
                } else if let Some(tags) = value.as_array() {
                    if tags.iter().any(|tag| !tag.is_string()) {
                        return Err(format!("Поле {key} имеет неверный тип."));
                    }
                    target.insert(key.clone(), value.clone());
                } else {
                    return Err(format!("Поле {key} имеет неверный тип."));
                }
            }
            "checklist" => {
                if value.is_null() || value.is_array() {
                    target.insert(
                        key.clone(),
                        if value.is_null() {
                            serde_json::json!([])
                        } else {
                            value.clone()
                        },
                    );
                } else {
                    return Err(format!("Поле {key} имеет неверный тип."));
                }
            }
            "pinned" | "archived" => {
                if value.is_null() {
                    target.insert(key.clone(), serde_json::Value::Bool(false));
                } else if value.is_boolean() {
                    target.insert(key.clone(), value.clone());
                } else {
                    return Err(format!("Поле {key} имеет неверный тип."));
                }
            }
            _ => unreachable!("validated note patch key"),
        }
    }
    let now = note_time(&transaction)?;
    let revision = target
        .get("revision")
        .and_then(|value| value.as_i64())
        .unwrap_or(0)
        .saturating_add(1);
    target.insert("revision".to_string(), revision.into());
    target.insert(
        "updated_at".to_string(),
        serde_json::Value::String(now.clone()),
    );
    let snapshot = note.to_string();
    if note["source_type"] == "mindmap" {
        let owner_raw: String = if let Some(stage) = note_stage_id.as_deref() {
            transaction.query_row(
                "SELECT payload_json FROM stages WHERE project_id=?1 AND id=?2",
                rusqlite::params![project_id, stage],
                |r| r.get(0),
            )
        } else {
            transaction.query_row(
                "SELECT payload_json FROM projects WHERE id=?1",
                [project_id],
                |r| r.get(0),
            )
        }
        .map_err(|e| e.to_string())?;
        let owner: serde_json::Value =
            serde_json::from_str(&owner_raw).map_err(|e| e.to_string())?;
        let map = stored_map(&owner).ok_or("map_note_link_invalid")?;
        let updated = mindmap::set_note_text(
            &map,
            map_node_id.as_deref().ok_or("map_note_link_invalid")?,
            note["content"].as_str().unwrap_or(""),
        )
        .ok_or("map_note_link_invalid")?;
        if map_sync::local_edit(
            &transaction,
            project_id,
            note_stage_id.as_deref(),
            &updated,
            Some(&note),
            &now,
        )
        .map_err(|e| e.to_string())?
        {
            let raw: String = transaction
                .query_row(
                    "SELECT payload_json FROM notes WHERE id=?1",
                    [&raw_id],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            transaction.commit().map_err(|e| e.to_string())?;
            return Ok(raw);
        }
    }
    prepare_direct_note_intent(
        &transaction,
        project_id,
        &raw_id,
        note_sync::NoteSyncOperation::Upsert,
        &now,
        None,
        &snapshot,
    )?;
    let changed = transaction
        .execute(
            "UPDATE notes SET updated_at=?1,payload_json=?2 \
             WHERE id=?3 AND project_id=?4",
            rusqlite::params![now, snapshot, raw_id, project_id],
        )
        .map_err(|_| "Не удалось сохранить изменение заметки.".to_string())?;
    if changed != 1 {
        return Err("Заметка больше не существует.".to_string());
    }
    if map_note {
        let node_id = map_node_id
            .as_deref()
            .ok_or_else(|| "У заметки карты отсутствует source_node_id.".to_string())?;
        let raw_map: String = if let Some(stage_id) = note_stage_id.as_deref() {
            transaction.query_row(
                "SELECT payload_json FROM stages WHERE id=?1 AND project_id=?2",
                rusqlite::params![stage_id, project_id],
                |row| row.get(0),
            )
        } else {
            transaction.query_row(
                "SELECT payload_json FROM projects WHERE id=?1",
                [&project_id],
                |row| row.get(0),
            )
        }
        .map_err(|_| "Связанная карта больше не существует.".to_string())?;
        let owner_payload: serde_json::Value =
            serde_json::from_str(&raw_map).map_err(|error| error.to_string())?;
        let map = stored_map(&owner_payload)
            .ok_or_else(|| "Связанная карта больше не существует.".to_string())?;
        let content = note
            .get("content")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        let updated_map = mindmap::set_note_text(&map, node_id, content)
            .ok_or_else(|| "Связанная заметка карты больше не существует.".to_string())?;
        update_stored_map(
            &transaction,
            project_id,
            note_stage_id.as_deref(),
            &updated_map,
            &now,
            false,
        )?;
    }
    transaction.commit().map_err(|error| error.to_string())?;
    Ok(snapshot)
}

#[tauri::command]
fn delete_note(
    project_id: String,
    note_id: String,
    stage_id: Option<String>,
) -> Result<(), String> {
    let mut connection = open_notes_database(true)?;
    require_sqlite_notes_owner(&connection)?;
    delete_note_in_connection(&mut connection, &project_id, &note_id, stage_id.as_deref())
}

fn delete_note_in_connection(
    connection: &mut rusqlite::Connection,
    project_id: &str,
    note_id: &str,
    stage_id: Option<&str>,
) -> Result<(), String> {
    let transaction = connection
        .transaction()
        .map_err(|error| error.to_string())?;
    require_note_relation(&transaction, project_id, stage_id)?;
    require_note_writable(&transaction, project_id, stage_id)?;
    let raw_id = raw_note_id(note_id, stage_id.is_none()).to_string();
    let note_payload: String = transaction
        .query_row(
            "SELECT payload_json FROM notes WHERE id = ?1 AND project_id = ?2",
            rusqlite::params![raw_id, project_id],
            |row| row.get(0),
        )
        .map_err(|_| "Заметка больше не существует.".to_string())?;
    let note_stage_id = serde_json::from_str::<serde_json::Value>(&note_payload)
        .map_err(|error| error.to_string())?
        .get("stage_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    require_note_relation(&transaction, project_id, note_stage_id.as_deref())?;
    require_note_writable(&transaction, project_id, note_stage_id.as_deref())?;
    let now = note_time(&transaction)?;
    let parsed_note: serde_json::Value =
        serde_json::from_str(&note_payload).map_err(|error| error.to_string())?;
    let tombstone = note_sync::build_note_tombstone(&parsed_note, &now)
        .map_err(|error| error.user_message().to_string())?;
    prepare_direct_note_intent(
        &transaction,
        project_id,
        &raw_id,
        note_sync::NoteSyncOperation::Delete,
        &now,
        Some(&now),
        &tombstone,
    )?;
    if parsed_note
        .get("source_type")
        .and_then(|value| value.as_str())
        == Some("mindmap")
    {
        let node_id = parsed_note
            .get("source_node_id")
            .and_then(|value| value.as_str())
            .ok_or_else(|| "У заметки карты отсутствует source_node_id.".to_string())?;
        let raw_map: String = if let Some(stage_id) = note_stage_id.as_deref() {
            transaction.query_row(
                "SELECT payload_json FROM stages WHERE id=?1 AND project_id=?2",
                rusqlite::params![stage_id, project_id],
                |row| row.get(0),
            )
        } else {
            transaction.query_row(
                "SELECT payload_json FROM projects WHERE id=?1",
                [&project_id],
                |row| row.get(0),
            )
        }
        .map_err(|_| "Связанная карта больше не существует.".to_string())?;
        let owner_payload: serde_json::Value =
            serde_json::from_str(&raw_map).map_err(|error| error.to_string())?;
        let map = stored_map(&owner_payload)
            .ok_or_else(|| "Связанная карта больше не существует.".to_string())?;
        let updated_map = mindmap::remove_note(&map, node_id)
            .ok_or_else(|| "Связанная заметка карты больше не существует.".to_string())?;
        if map_sync::local_edit(
            &transaction,
            project_id,
            note_stage_id.as_deref(),
            &updated_map,
            None,
            &now,
        )
        .map_err(|e| e.to_string())?
        {
            transaction.commit().map_err(|e| e.to_string())?;
            return Ok(());
        }
        update_stored_map(
            &transaction,
            project_id,
            note_stage_id.as_deref(),
            &updated_map,
            &now,
            true,
        )?;
    }
    let deleted = transaction
        .execute(
            "DELETE FROM notes WHERE id = ?1 AND project_id = ?2",
            rusqlite::params![raw_id, project_id],
        )
        .map_err(|_| "Не удалось удалить заметку.".to_string())?;
    if deleted != 1 {
        return Err("Заметка больше не существует.".to_string());
    }
    transaction.commit().map_err(|error| error.to_string())
}

#[tauri::command]
fn reorder_notes(
    project_id: String,
    note_ids: Vec<String>,
    stage_id: Option<String>,
) -> Result<serde_json::Value, String> {
    let mut connection = open_notes_database(true)?;
    require_sqlite_notes_owner(&connection)?;
    reorder_notes_in_connection(&mut connection, &project_id, &note_ids, stage_id.as_deref())?;
    list_notes(project_id, stage_id)
}

fn reorder_notes_in_connection(
    connection: &mut rusqlite::Connection,
    project_id: &str,
    note_ids: &[String],
    stage_id: Option<&str>,
) -> Result<(), String> {
    let transaction = connection
        .transaction()
        .map_err(|error| error.to_string())?;
    require_note_relation(&transaction, project_id, stage_id)?;
    require_note_writable(&transaction, project_id, stage_id)?;
    let aggregate = stage_id.is_none();
    let now = note_time(&transaction)?;
    for (index, note_id) in note_ids.iter().enumerate() {
        let raw_id = raw_note_id(note_id, aggregate);
        let payload: Option<String> = transaction
            .query_row(
                "SELECT payload_json FROM notes WHERE id = ?1 AND project_id = ?2",
                rusqlite::params![raw_id, project_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| error.to_string())?;
        if let Some(payload) = payload {
            let mut note = serde_json::from_str::<serde_json::Value>(&payload)
                .map_err(|error| error.to_string())?;
            let note_stage_id = note
                .get("stage_id")
                .and_then(|value| value.as_str())
                .map(str::to_string);
            require_note_relation(&transaction, project_id, note_stage_id.as_deref())?;
            require_note_writable(&transaction, project_id, note_stage_id.as_deref())?;
            let target = note
                .as_object_mut()
                .ok_or_else(|| "Некорректный payload заметки.".to_string())?;
            target.insert("sort_order".to_string(), (index as i64).into());
            let revision = target
                .get("revision")
                .and_then(|value| value.as_i64())
                .unwrap_or(0)
                .saturating_add(1);
            target.insert("revision".to_string(), revision.into());
            target.insert(
                "updated_at".to_string(),
                serde_json::Value::String(now.clone()),
            );
            if note["source_type"] == "mindmap" {
                let map = map_sync::owner_data(&transaction, project_id, note_stage_id.as_deref())
                    .map_err(|e| e.to_string())?;
                if map_sync::local_edit(
                    &transaction,
                    project_id,
                    note_stage_id.as_deref(),
                    &map,
                    Some(&note),
                    &now,
                )
                .map_err(|e| e.to_string())?
                {
                    continue;
                }
            }
            let snapshot = note.to_string();
            prepare_direct_note_intent(
                &transaction,
                project_id,
                raw_id,
                note_sync::NoteSyncOperation::Upsert,
                &now,
                None,
                &snapshot,
            )?;
            let changed = transaction
                .execute(
                    "UPDATE notes SET updated_at=?1,payload_json=?2 \
                     WHERE id=?3 AND project_id=?4",
                    rusqlite::params![now, snapshot, raw_id, project_id],
                )
                .map_err(|_| "Не удалось изменить порядок заметок.".to_string())?;
            if changed != 1 {
                return Err("Заметка больше не существует.".to_string());
            }
        }
    }
    transaction.commit().map_err(|error| error.to_string())
}

fn require_sqlite_settings_owner(connection: &rusqlite::Connection) -> Result<(), String> {
    let owner: String = connection
        .query_row(
            "SELECT owner FROM storage_ownership WHERE subsystem = 'settings'",
            [],
            |row| row.get(0),
        )
        .map_err(|error| format!("Не удалось проверить ownership настроек: {error}"))?;
    if owner != "sqlite" {
        return Err("Прямая запись настроек недоступна до завершения миграции.".to_string());
    }
    Ok(())
}

#[tauri::command]
fn get_settings() -> Result<serde_json::Map<String, serde_json::Value>, String> {
    let connection = open_settings_database()?;
    let mut statement = connection
        .prepare("SELECT key, value_json FROM settings")
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([], |row| {
            let key: String = row.get(0)?;
            let value: String = row.get(1)?;
            Ok((key, value))
        })
        .map_err(|error| error.to_string())?;
    let mut values = serde_json::Map::new();
    for row in rows {
        let (key, value) = row.map_err(|error| error.to_string())?;
        let parsed = serde_json::from_str(&value)
            .map_err(|error| format!("Некорректное значение настройки {key}: {error}"))?;
        values.insert(key, parsed);
    }
    Ok(values)
}

#[tauri::command]
fn set_settings(values: serde_json::Map<String, serde_json::Value>) -> Result<(), String> {
    let mut connection = open_settings_database()?;
    require_sqlite_settings_owner(&connection)?;
    let transaction = connection
        .transaction()
        .map_err(|error| error.to_string())?;
    for (key, value) in values {
        let encoded = serde_json::to_string(&value).map_err(|error| error.to_string())?;
        transaction
            .execute(
                concat!(
                    "INSERT INTO settings(key, value_json) VALUES(?1, ?2) ",
                    "ON CONFLICT(key) DO UPDATE SET value_json=excluded.value_json",
                ),
                rusqlite::params![key, encoded],
            )
            .map_err(|error| error.to_string())?;
    }
    transaction.commit().map_err(|error| error.to_string())
}

const UPDATE_MANIFEST_URL: &str = "https://worta.ru/app/update_manifest.json";
const UPDATE_MANIFEST_TIMEOUT: Duration = Duration::from_secs(10);
const WINDOW_STATE_FILE: &str = "main-window.json";

#[derive(Clone, Deserialize, Serialize)]
struct MainWindowState {
    width: u32,
    height: u32,
    x: i32,
    y: i32,
}

impl MainWindowState {
    fn is_valid(&self) -> bool {
        (820..=10_000).contains(&self.width) && (600..=10_000).contains(&self.height)
    }
}

fn main_window_state_path(app: &tauri::AppHandle) -> Option<PathBuf> {
    app.path()
        .app_config_dir()
        .ok()
        .map(|directory| directory.join(WINDOW_STATE_FILE))
}

fn restore_main_window_state(app: &tauri::AppHandle) {
    let Some(path) = main_window_state_path(app) else {
        return;
    };
    let Ok(contents) = fs::read_to_string(path) else {
        return;
    };
    let Ok(state) = serde_json::from_str::<MainWindowState>(&contents) else {
        return;
    };
    if !state.is_valid() {
        return;
    }
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let _ = window.set_size(PhysicalSize::new(state.width, state.height));
    let _ = window.set_position(PhysicalPosition::new(state.x, state.y));
}

fn save_main_window_state(app: &tauri::AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let (Ok(size), Ok(position)) = (window.inner_size(), window.outer_position()) else {
        return;
    };
    let state = MainWindowState {
        width: size.width,
        height: size.height,
        x: position.x,
        y: position.y,
    };
    if !state.is_valid() {
        return;
    }
    let Some(path) = main_window_state_path(app) else {
        return;
    };
    if fs::create_dir_all(path.parent().unwrap_or_else(|| Path::new("."))).is_err() {
        return;
    }
    let Ok(contents) = serde_json::to_string(&state) else {
        return;
    };
    let _ = fs::write(path, contents);
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeInfo {
    version: String,
    native_updates: bool,
    architecture: String,
    development: bool,
    test_profile: bool,
    developer_mode_available: bool,
    startup_error: Option<String>,
}

#[derive(Clone, Default)]
struct StartupStatus {
    error: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
#[serde(deny_unknown_fields)]
struct ProjectMetadataPatch {
    name: Option<String>,
    goal: Option<f64>,
    unit: Option<String>,
    // None means omitted; Some(Null) is an explicit JSON null.
    deadline: MetadataDeadline,
    infinite: Option<bool>,
}

#[derive(Default)]
enum MetadataDeadline {
    #[default]
    Absent,
    Null,
    Value(String),
}

impl<'de> Deserialize<'de> for MetadataDeadline {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct DeadlineVisitor;
        impl<'de> Visitor<'de> for DeadlineVisitor {
            type Value = MetadataDeadline;
            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a nullable deadline string")
            }
            fn visit_unit<E>(self) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(MetadataDeadline::Null)
            }
            fn visit_none<E>(self) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(MetadataDeadline::Null)
            }
            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(MetadataDeadline::Value(value.to_string()))
            }
            fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(MetadataDeadline::Value(value))
            }
        }
        deserializer.deserialize_any(DeadlineVisitor)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AddProjectProgressCommand {
    project_id: String,
    new_total: f64,
    #[serde(default)]
    expected_heads: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AddStageProgressCommand {
    project_id: String,
    stage_id: String,
    new_total: f64,
    #[serde(default)]
    expected_heads: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DeleteProgressCommand {
    project_id: String,
    entry_id: String,
    #[serde(default)]
    stage_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateStageCommand {
    name: String,
    goal: Option<f64>,
    #[serde(default)]
    infinite: bool,
    #[serde(default)]
    total: f64,
    deadline: Option<String>,
    #[serde(default)]
    personal_goal: f64,
    #[serde(default = "default_true")]
    streak_enabled: bool,
    #[serde(default = "default_true")]
    auto_freeze: bool,
    #[serde(default)]
    work_method: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateProjectCommand {
    name: String,
    goal: Option<f64>,
    #[serde(default)]
    infinite: bool,
    #[serde(default)]
    total: f64,
    deadline: Option<String>,
    #[serde(default)]
    personal_goal: f64,
    #[serde(default = "default_true")]
    streak_enabled: bool,
    #[serde(default = "default_true")]
    auto_freeze: bool,
    #[serde(default)]
    work_method: String,
    #[serde(default = "default_unit")]
    unit: String,
    #[serde(default)]
    stages_enabled: bool,
    #[serde(default)]
    combine_stage_mindmaps: bool,
    cover_image: Option<String>,
    folder_id: Option<String>,
    #[serde(default)]
    stages: Vec<CreateStageCommand>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EntityUpdateCommand {
    name: Option<String>,
    goal: Option<f64>,
    infinite: Option<bool>,
    total: Option<f64>,
    deadline: Option<String>,
    personal_goal: Option<f64>,
    streak_enabled: Option<bool>,
    auto_freeze: Option<bool>,
    work_method: Option<String>,
    #[serde(default)]
    recalculate_plan: bool,
    #[serde(default)]
    confirm_daily_goal_increase: bool,
}

// Preserve absent vs explicit null for the catalog remove-membership writer.
fn deserialize_folder_patch<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(d).map(Some)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProjectUpdateCommand {
    name: Option<String>,
    goal: Option<f64>,
    infinite: Option<bool>,
    total: Option<f64>,
    deadline: Option<String>,
    personal_goal: Option<f64>,
    streak_enabled: Option<bool>,
    unit: Option<String>,
    auto_freeze: Option<bool>,
    work_method: Option<String>,
    stages_enabled: Option<bool>,
    combine_stage_mindmaps: Option<bool>,
    cover_image: Option<String>,
    #[serde(default, deserialize_with = "deserialize_folder_patch")]
    folder_id: Option<Option<String>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ArchiveProjectCommand {
    project_id: String,
    archived: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProjectIdCommand {
    project_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StageIdCommand {
    project_id: String,
    stage_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReorderStagesCommand {
    project_id: String,
    stage_ids: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MapCommand {
    project_id: String,
    #[serde(default)]
    stage_id: Option<String>,
    data: serde_json::Value,
    #[serde(default)]
    expected_heads: Option<serde_json::Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct XMindCommand {
    project_id: String,
    #[serde(default)]
    stage_id: Option<String>,
    bytes: Vec<u8>,
}

fn default_true() -> bool {
    true
}
fn default_unit() -> String {
    "symbols".to_string()
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct MacosUpdateProgress {
    downloaded_bytes: u64,
    total_bytes: u64,
}

#[tauri::command]
fn runtime_info(startup_status: State<'_, StartupStatus>) -> RuntimeInfo {
    RuntimeInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        native_updates: native_updates_enabled(),
        architecture: std::env::consts::ARCH.to_string(),
        development: cfg!(debug_assertions),
        test_profile: build_profile() == "test",
        developer_mode_available: developer_mode_available(),
        startup_error: startup_status.error.clone(),
    }
}

fn encode_path_segment(value: &str) -> String {
    value.bytes().fold(String::new(), |mut encoded, byte| {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            encoded.push(byte as char);
        } else {
            write!(&mut encoded, "%{byte:02X}").expect("writing to a String cannot fail");
        }
        encoded
    })
}

#[tauri::command]
fn update_project_metadata(
    project_id: String,
    patch: ProjectMetadataPatch,
) -> Result<serde_json::Value, String> {
    let mut connection = open_projects_database()?;
    require_projects_owner(&connection)?;
    if patch
        .name
        .as_deref()
        .is_some_and(|value| value.trim().is_empty())
    {
        return Err("Название проекта не может быть пустым.".to_string());
    }
    if patch
        .unit
        .as_deref()
        .is_some_and(|value| !valid_unit(value))
    {
        return Err("Неизвестная единица прогресса.".to_string());
    }
    if patch
        .goal
        .is_some_and(|value| !value.is_finite() || value <= 0.0)
    {
        return Err("Цель должна быть конечной и больше нуля.".to_string());
    }
    let deadline = match patch.deadline {
        MetadataDeadline::Absent => None,
        MetadataDeadline::Null => Some(None),
        MetadataDeadline::Value(value) => Some(Some(value)),
    };
    let mut proposed = project_payload(&mut connection, &project_id)?;
    if let Some(value) = patch.name.as_ref() {
        proposed["name"] = value.trim().into();
    }
    if let Some(value) = patch.goal {
        proposed["goal"] = value.into();
    }
    if let Some(value) = patch.unit.as_ref() {
        proposed["unit"] = value.clone().into();
    }
    if let Some(value) = patch.infinite {
        proposed["infinite"] = value.into();
        if value {
            proposed["goal"] = serde_json::Value::Null;
        }
    }
    if let Some(value) = deadline.as_ref() {
        proposed["deadline"] = value
            .clone()
            .map_or(serde_json::Value::Null, serde_json::Value::String);
    }
    if proposed["infinite"] == true {
        proposed["goal"] = serde_json::Value::Null;
    }
    let metadata_now = connection
        .query_row(
            "SELECT strftime('%Y-%m-%dT%H:%M:%f','now') || '000Z'",
            [],
            |r| r.get::<_, String>(0),
        )
        .map_err(|e| e.to_string())?;
    if project_metadata_sync::capture_normal_edit(
        &mut connection,
        &project_id,
        &proposed,
        &metadata_now,
    )
    .map_err(|e| e.to_string())?
    {
        return project_payload(&mut connection, &project_id);
    }
    let mut repository = ProjectsRepository::new(&mut connection);
    let updated = repository
        .update_project_metadata(
            &project_id,
            &ProjectMetadataUpdate {
                name: patch.name.map(|value| value.trim().to_string()),
                goal: patch.goal,
                unit: patch.unit,
                status: None,
                infinite: patch.infinite,
                deadline,
            },
        )
        .map_err(|error| error.to_string())?;
    drop(repository);
    let _ = updated;
    project_payload(&mut connection, &project_id)
}

#[tauri::command]
fn reorder_projects(project_ids: Vec<String>) -> Result<serde_json::Value, String> {
    let mut connection = open_projects_database()?;
    require_projects_owner(&connection)?;
    let mut repository = ProjectsRepository::new(&mut connection);
    if project_ids.is_empty()
        || project_ids
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len()
            != project_ids.len()
    {
        return Err("Порядок должен содержать известные проекты без повторений.".to_string());
    }
    let existing = repository
        .get_project_order()
        .map_err(|error| error.to_string())?;
    if project_ids.iter().any(|id| !existing.contains(id)) {
        return Err("Порядок должен содержать только известные проекты.".to_string());
    }
    let requested: std::collections::HashSet<&str> =
        project_ids.iter().map(String::as_str).collect();
    let mut normalized = existing.clone();
    let visible_positions = normalized
        .iter()
        .enumerate()
        .filter_map(|(index, id)| requested.contains(id.as_str()).then_some(index))
        .collect::<Vec<_>>();
    for (position, project_id) in visible_positions.into_iter().zip(project_ids.iter()) {
        normalized[position] = project_id.clone();
    }
    drop(repository);
    let now: String = connection
        .query_row("SELECT strftime('%Y-%m-%dT%H:%M:%f000Z','now')", [], |r| {
            r.get(0)
        })
        .map_err(|e| e.to_string())?;
    let mut q=connection.prepare("SELECT b.project_id FROM cloud_sync_project_bindings b JOIN cloud_catalog_state c ON c.account_id=b.account_id").map_err(|e|e.to_string())?;
    let bound = q
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    drop(q);
    let portable = normalized
        .iter()
        .filter(|id| bound.contains(id))
        .cloned()
        .collect::<Vec<_>>();
    let captured = account_catalog::normal(
        &mut connection,
        "project_order",
        "project_order",
        serde_json::json!({"ids":portable}),
        &now,
    )
    .map_err(|e| e.to_string())?;
    if captured {
        let old_cloud = existing
            .iter()
            .filter(|id| bound.contains(id))
            .cloned()
            .collect::<Vec<_>>();
        let slots = normalized
            .iter()
            .enumerate()
            .filter_map(|(i, id)| bound.contains(id).then_some(i))
            .collect::<Vec<_>>();
        for (slot, id) in slots.into_iter().zip(old_cloud) {
            normalized[slot] = id;
        }
    }
    let mut repository = ProjectsRepository::new(&mut connection);
    repository
        .update_project_order(&normalized)
        .map_err(|error| error.to_string())?;
    drop(repository);
    let ids = project_ids
        .iter()
        .map(|id| project_payload(&mut connection, id))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(serde_json::Value::Array(ids))
}

fn validate_progress_command(
    project_id: &str,
    new_total: f64,
    stage_id: Option<&str>,
) -> Result<(), String> {
    if project_id.is_empty() || stage_id.is_some_and(str::is_empty) {
        return Err("Идентификатор проекта или этапа не может быть пустым.".to_string());
    }
    if !new_total.is_finite() || new_total < 0.0 {
        return Err("Новое общее значение должно быть конечным и неотрицательным.".to_string());
    }
    Ok(())
}

fn valid_unit(value: &str) -> bool {
    matches!(value, "symbols" | "A4" | "author_list" | "ficbook_pages")
}

fn unit_factor(value: &str) -> Option<f64> {
    match value {
        "symbols" => Some(1.0),
        "A4" => Some(1800.0),
        "author_list" => Some(40000.0),
        "ficbook_pages" => Some(4500.0),
        _ => None,
    }
}

fn normalized_total(value: f64, unit: &str) -> Result<f64, String> {
    if !value.is_finite() || value < 0.0 {
        return Err("Значение должно быть конечным и неотрицательным.".to_string());
    }
    Ok(if unit == "author_list" {
        value
    } else {
        value.ceil()
    })
}

fn entity_payload(
    id: String,
    name: String,
    goal: Option<f64>,
    infinite: bool,
    total: f64,
    deadline: Option<String>,
    unit: String,
    work_method: String,
    personal_goal: f64,
    project_id: Option<String>,
) -> serde_json::Value {
    let progress = if infinite || goal.unwrap_or(0.0) <= 0.0 {
        0.0
    } else {
        total / goal.unwrap_or(1.0) * 100.0
    };
    serde_json::json!({
        "id": id, "name": name, "goal": if infinite { serde_json::Value::Null } else { goal.map_or(serde_json::Value::Null, serde_json::Value::from) },
        "infinite": infinite, "total": total, "progress": progress, "deadline": deadline,
        "status": "активен", "unit": unit, "created_at": null, "updated_at": null,
        "notes_updated_at": null, "mindmap_updated_at": null, "completed_at": null,
        "personal_goal": personal_goal, "today_goal": null, "planning_date": null,
        "plan_daily_goal": null, "added_today": 0, "remaining": if infinite { serde_json::Value::Null } else { serde_json::json!((goal.unwrap_or(total) - total).max(0.0)) },
        "streak_enabled": true, "streak_status": "No", "streak_length": 0, "max_streak": 0,
        "auto_freeze": true, "progress_entries": [], "project_notes": [], "mindmap": null,
        "stages": [], "stages_enabled": false, "combine_stage_mindmaps": false,
        "cover_image": null, "folder_id": null, "sync_available": work_method == "sync",
        "work_method": work_method, "parent_project_id": project_id
    })
}

#[tauri::command]
fn create_project(command: CreateProjectCommand) -> Result<serde_json::Value, String> {
    let mut connection = open_projects_database()?;
    require_projects_owner(&connection)?;
    let name = command.name.trim().to_string();
    if name.is_empty() {
        return Err("Название проекта не может быть пустым.".to_string());
    }
    if connection
        .query_row("SELECT 1 FROM projects WHERE name=?1", [&name], |_| Ok(()))
        .optional()
        .map_err(|error| error.to_string())?
        .is_some()
    {
        return Err("Проект с таким названием уже существует.".to_string());
    }
    if let Some(folder_id) = command.folder_id.as_deref() {
        if connection
            .query_row(
                "SELECT 1 FROM project_folders WHERE id=?1",
                [folder_id],
                |_| Ok(()),
            )
            .optional()
            .map_err(|error| error.to_string())?
            .is_none()
        {
            return Err("Папка не найдена.".to_string());
        }
    }
    if !valid_unit(&command.unit) {
        return Err("Неизвестная единица прогресса.".to_string());
    }
    if !matches!(command.work_method.as_str(), "" | "manual" | "sync" | "app") {
        return Err("Неизвестный метод работы с проектом.".to_string());
    }
    let work_method = if command.work_method.is_empty() {
        "manual".to_string()
    } else {
        command.work_method.clone()
    };
    let total = normalized_total(command.total, &command.unit)?;
    let goal = if command.infinite {
        None
    } else {
        Some(
            command
                .goal
                .ok_or_else(|| "Цель должна быть больше нуля.".to_string())?,
        )
    };
    let goal = goal
        .map(|value| normalized_total(value, &command.unit))
        .transpose()?;
    if goal.is_some_and(|value| value <= 0.0) {
        return Err("Цель должна быть больше нуля.".to_string());
    }
    let personal_goal = normalized_total(command.personal_goal, &command.unit)?;
    let id = new_note_id()?;
    let now = now_from_database(&connection)?;
    let day = streaks::logical_writing_day_at(&connection, &now)?;
    let mut payload = entity_payload(
        id.clone(),
        name.clone(),
        goal,
        command.infinite,
        total,
        command.deadline.clone(),
        command.unit.clone(),
        work_method.clone(),
        personal_goal,
        None,
    );
    payload["created_at"] = serde_json::Value::String(now.clone());
    payload["updated_at"] = serde_json::Value::String(now.clone());
    payload["streak_enabled"] = serde_json::Value::Bool(command.streak_enabled);
    payload["auto_freeze"] = serde_json::Value::Bool(command.auto_freeze);
    payload["stages_enabled"] =
        serde_json::Value::Bool(command.stages_enabled || !command.stages.is_empty());
    payload["combine_stage_mindmaps"] = serde_json::Value::Bool(
        command.combine_stage_mindmaps && (!command.stages.is_empty() || command.stages_enabled),
    );
    payload["cover_image"] = command
        .cover_image
        .clone()
        .map_or(serde_json::Value::Null, serde_json::Value::String);
    payload["folder_id"] = command
        .folder_id
        .clone()
        .map_or(serde_json::Value::Null, serde_json::Value::String);
    let mut stages = Vec::new();
    let mut stage_records = Vec::new();
    for stage in command.stages {
        let stage_name = stage.name.trim().to_string();
        if stage_name.is_empty() {
            return Err("Название этапа не может быть пустым.".to_string());
        }
        let stage_total = normalized_total(stage.total, &command.unit)?;
        let stage_goal = if stage.infinite {
            None
        } else {
            Some(normalized_total(
                stage
                    .goal
                    .ok_or_else(|| "Цель этапа должна быть больше нуля.".to_string())?,
                &command.unit,
            )?)
        };
        let stage_id = new_note_id()?;
        let mut stage_payload = entity_payload(
            stage_id.clone(),
            stage_name,
            stage_goal,
            stage.infinite,
            stage_total,
            stage.deadline,
            command.unit.clone(),
            if stage.work_method.is_empty() {
                "manual".to_string()
            } else {
                stage.work_method
            },
            stage.personal_goal,
            Some(id.clone()),
        );
        stage_payload["created_at"] = serde_json::Value::String(now.clone());
        stage_payload["updated_at"] = serde_json::Value::String(now.clone());
        stage_payload["streak_enabled"] = serde_json::Value::Bool(stage.streak_enabled);
        stage_payload["auto_freeze"] = serde_json::Value::Bool(stage.auto_freeze);
        daily_goals::project(&connection, &mut stage_payload, &day)?;
        stages.push(stage_payload.clone());
        stage_records.push(StageRecord {
            id: stage_id,
            project_id: id.clone(),
            name: stage_payload["name"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            goal: stage_goal,
            infinite: stage.infinite,
            unit: command.unit.clone(),
            status: "активен".to_string(),
            created_at: Some(now.clone()),
            updated_at: Some(now.clone()),
            payload: stage_payload,
        });
    }
    if !stages.is_empty() {
        payload["total"] = serde_json::json!(stages
            .iter()
            .filter_map(|stage| stage["total"].as_f64())
            .sum::<f64>());
    }
    payload["stages"] = serde_json::Value::Array(stages);
    if command.deadline.is_some() || personal_goal > 0.0 || stage_records.is_empty() {
        daily_goals::project(&connection, &mut payload, &day)?;
    }
    let order_position: i64 = connection
        .query_row(
            "SELECT COALESCE(MAX(position), -1) + 1 FROM project_order",
            [],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    let mut repository = ProjectsRepository::new(&mut connection);
    repository
        .insert_aggregate(&ProjectAggregate {
            project: ProjectRecord {
                id: id.clone(),
                name,
                goal,
                infinite: command.infinite,
                unit: command.unit,
                status: "активен".to_string(),
                created_at: Some(now.clone()),
                updated_at: Some(now),
                payload,
            },
            stages: stage_records,
            progress: Vec::<ProgressRecord>::new(),
            order_position,
        })
        .map_err(|error| error.to_string())?;
    if let Some(folder_id) = command.folder_id {
        repository
            .set_project_folder(&id, Some(&folder_id))
            .map_err(|error| error.to_string())?;
    }
    drop(repository);
    project_payload(&mut connection, &id)
}

#[tauri::command]
fn update_project(
    project_id: String,
    patch: ProjectUpdateCommand,
) -> Result<serde_json::Value, String> {
    let mut connection = open_projects_database()?;
    require_projects_owner(&connection)?;
    let mut repository = ProjectsRepository::new(&mut connection);
    let current = repository
        .get_project(&project_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "Проект не найден.".to_string())?;
    let folder_id = patch.folder_id;
    if let Some(Some(folder_id)) = folder_id.as_ref() {
        if !repository
            .project_folder_exists(folder_id)
            .map_err(|error| error.to_string())?
        {
            return Err("Папка не найдена.".to_string());
        }
    }
    let old_folder = current.payload["folder_id"].clone();
    let mut payload = current.payload.clone();
    let object = payload
        .as_object_mut()
        .ok_or_else(|| "Некорректный payload проекта.".to_string())?;
    if let Some(name) = patch.name {
        if name.trim().is_empty() {
            return Err("Название проекта не может быть пустым.".to_string());
        }
        object.insert("name".into(), name.trim().into());
    }
    if let Some(unit) = patch.unit {
        if !valid_unit(&unit) {
            return Err("Неизвестная единица прогресса.".to_string());
        }
        object.insert("unit".into(), unit.into());
    }
    for (key, value) in [
        ("goal", patch.goal.map(serde_json::Value::from)),
        ("total", patch.total.map(serde_json::Value::from)),
        (
            "personal_goal",
            patch.personal_goal.map(serde_json::Value::from),
        ),
    ] {
        if let Some(value) = value {
            if !value
                .as_f64()
                .is_some_and(|number| number.is_finite() && number >= 0.0)
            {
                return Err("Значение должно быть конечным и неотрицательным.".to_string());
            }
            object.insert(key.into(), value);
        }
    }
    if let Some(infinite) = patch.infinite {
        object.insert("infinite".into(), infinite.into());
        if infinite {
            object.insert("goal".into(), serde_json::Value::Null);
        }
    }
    if let Some(deadline) = patch.deadline {
        object.insert("deadline".into(), deadline.into());
    }
    if let Some(value) = patch.personal_goal {
        object.insert("personal_goal".into(), value.into());
    }
    if let Some(value) = patch.streak_enabled {
        object.insert("streak_enabled".into(), value.into());
    }
    if let Some(value) = patch.auto_freeze {
        object.insert("auto_freeze".into(), value.into());
    }
    if let Some(value) = patch.work_method {
        if !matches!(value.as_str(), "manual" | "sync" | "app") {
            return Err("Неизвестный метод работы с проектом.".to_string());
        }
        object.insert("work_method".into(), value.clone().into());
        object.insert("sync_available".into(), (value == "sync").into());
    }
    if let Some(value) = patch.stages_enabled {
        object.insert("stages_enabled".into(), value.into());
    }
    if let Some(value) = patch.combine_stage_mindmaps {
        object.insert("combine_stage_mindmaps".into(), value.into());
    }
    if let Some(value) = patch.cover_image {
        object.insert("cover_image".into(), value.into());
    }
    if let Some(folder_id) = folder_id.as_ref() {
        object.insert(
            "folder_id".into(),
            folder_id
                .clone()
                .map_or(serde_json::Value::Null, serde_json::Value::String),
        );
    }
    if payload["infinite"] == true {
        payload["goal"] = serde_json::Value::Null;
    }
    daily_goals::reset_if_changed(&current.payload, &mut payload);
    drop(repository);
    let metadata_now = connection
        .query_row(
            "SELECT strftime('%Y-%m-%dT%H:%M:%f','now') || '000Z'",
            [],
            |r| r.get::<_, String>(0),
        )
        .map_err(|e| e.to_string())?;
    let catalog_captured = if let Some(folder) = folder_id.as_ref() {
        account_catalog::normal(
            &mut connection,
            "folder_membership",
            &project_id,
            serde_json::json!({"folder_id":folder}),
            &metadata_now,
        )
        .map_err(|e| e.to_string())?
    } else {
        false
    };
    if catalog_captured {
        payload["folder_id"] = old_folder;
    }
    let captured = project_metadata_sync::capture_normal_edit(
        &mut connection,
        &project_id,
        &payload,
        &metadata_now,
    )
    .map_err(|e| e.to_string())?;

    let mut repository = ProjectsRepository::new(&mut connection);
    if !captured {
        repository
            .update_project_payload(&project_id, &payload)
            .map_err(|error| error.to_string())?;
    }
    if let Some(folder_id) = folder_id.filter(|_| !catalog_captured) {
        repository
            .set_project_folder(&project_id, folder_id.as_deref())
            .map_err(|error| error.to_string())?;
    }
    drop(repository);
    project_payload(&mut connection, &project_id)
}

#[tauri::command]
fn create_stage(
    project_id: String,
    command: CreateStageCommand,
) -> Result<serde_json::Value, String> {
    let mut connection = open_projects_database()?;
    require_projects_owner(&connection)?;
    let now = now_from_database(&connection)?;
    let day = streaks::logical_writing_day_at(&connection, &now)?;
    let local_project = daily_goals::local_project(&connection, &project_id)?;
    let mut repository = ProjectsRepository::new(&mut connection);
    let project = repository
        .get_project(&project_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "Проект не найден.".to_string())?;
    if project.status == "завершен" {
        return Err("Завершённый проект доступен только для просмотра.".to_string());
    }
    let name = command.name.trim().to_string();
    if name.is_empty() {
        return Err("Название этапа не может быть пустым.".to_string());
    }
    if repository
        .list_stages(&project_id)
        .map_err(|error| error.to_string())?
        .iter()
        .any(|stage| stage.name == name)
    {
        return Err("Этап с таким названием уже существует.".to_string());
    }
    if !valid_unit(&project.unit) {
        return Err("Неизвестная единица прогресса.".to_string());
    }
    let stage_id = new_note_id()?;
    let total = normalized_total(command.total, &project.unit)?;
    let goal = if command.infinite {
        None
    } else {
        Some(normalized_total(
            command
                .goal
                .ok_or_else(|| "Цель этапа должна быть больше нуля.".to_string())?,
            &project.unit,
        )?)
    };
    let method = if command.work_method.is_empty() {
        "manual".to_string()
    } else {
        command.work_method
    };
    if !matches!(method.as_str(), "manual" | "sync" | "app") {
        return Err("Неизвестный метод работы с этапом.".to_string());
    }
    let mut payload = entity_payload(
        stage_id.clone(),
        name.clone(),
        goal,
        command.infinite,
        total,
        command.deadline,
        project.unit.clone(),
        method.clone(),
        command.personal_goal,
        Some(project_id.clone()),
    );
    payload["created_at"] = now.clone().into();
    payload["updated_at"] = now.clone().into();
    payload["streak_enabled"] = command.streak_enabled.into();
    payload["auto_freeze"] = command.auto_freeze.into();
    drop(repository);
    if local_project {
        daily_goals::project(&connection, &mut payload, &day)?;
    }
    let stage = StageRecord {
        id: stage_id.clone(),
        project_id: project_id.clone(),
        name,
        goal,
        infinite: command.infinite,
        unit: project.unit.clone(),
        status: "активен".to_string(),
        created_at: Some(now.clone()),
        updated_at: Some(now),
        payload,
    };
    let portable = stage.payload.clone();
    let now = stage_sync::event_now(&connection).map_err(|e| e.to_string())?;
    if stage_sync::normal_edit(&mut connection, &project_id, &stage_id, portable, &now)
        .map_err(|e| e.to_string())?
    {
        documents::move_project_document_to_stage(&project_id, &stage_id)?;
        refresh_project_totals(&mut connection, &project_id)?;
        return project_payload(&mut connection, &project_id);
    }
    let mut repository = ProjectsRepository::new(&mut connection);
    repository
        .insert_stage(&stage)
        .map_err(|error| error.to_string())?;
    let mut project_payload_value = project.payload;
    project_payload_value["stages_enabled"] = true.into();
    project_payload_value["stages"] = serde_json::Value::Array(
        repository
            .list_stages(&project_id)
            .map_err(|error| error.to_string())?
            .into_iter()
            .map(|stage| stage.payload)
            .collect(),
    );
    repository
        .update_project_payload(&project_id, &project_payload_value)
        .map_err(|error| error.to_string())?;
    drop(repository);
    documents::move_project_document_to_stage(&project_id, &stage_id)?;
    refresh_project_totals(&mut connection, &project_id)?;
    project_payload(&mut connection, &project_id)
}

#[tauri::command]
fn update_stage(
    project_id: String,
    stage_id: String,
    patch: EntityUpdateCommand,
) -> Result<serde_json::Value, String> {
    let mut connection = open_projects_database()?;
    require_projects_owner(&connection)?;
    let mut repository = ProjectsRepository::new(&mut connection);
    let _project = repository
        .get_project(&project_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "Проект не найден.".to_string())?;
    let current = repository
        .get_stage(&stage_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "Этап не найден.".to_string())?;
    if current.project_id != project_id {
        return Err("Этап не относится к указанному проекту.".to_string());
    }
    let mut payload = current.payload.clone();
    if let Some(name) = patch.name {
        if name.trim().is_empty() {
            return Err("Название этапа не может быть пустым.".to_string());
        }
        payload["name"] = name.trim().into();
    }
    if let Some(goal) = patch.goal {
        payload["goal"] = normalized_total(goal, &current.unit)?.into();
    }
    if let Some(infinite) = patch.infinite {
        payload["infinite"] = infinite.into();
        if infinite {
            payload["goal"] = serde_json::Value::Null;
        }
    }
    if let Some(total) = patch.total {
        payload["total"] = normalized_total(total, &current.unit)?.into();
    }
    if let Some(value) = patch.deadline {
        payload["deadline"] = value.into();
    }
    if let Some(value) = patch.personal_goal {
        payload["personal_goal"] = normalized_total(value, &current.unit)?.into();
    }
    if let Some(value) = patch.streak_enabled {
        payload["streak_enabled"] = value.into();
    }
    if let Some(value) = patch.auto_freeze {
        payload["auto_freeze"] = value.into();
    }
    if let Some(value) = patch.work_method {
        if !matches!(value.as_str(), "manual" | "sync" | "app") {
            return Err("Неизвестный метод работы с этапом.".to_string());
        }
        payload["work_method"] = value.clone().into();
        payload["sync_available"] = (value == "sync").into();
    }
    let infinite = payload
        .get("infinite")
        .and_then(|value| value.as_bool())
        .unwrap_or(current.infinite);
    let goal = if infinite {
        None
    } else {
        payload
            .get("goal")
            .and_then(|value| value.as_f64())
            .or(current.goal)
    };
    daily_goals::reset_if_changed(&current.payload, &mut payload);
    let update = StageUpdate {
        name: payload
            .get("name")
            .and_then(|value| value.as_str())
            .unwrap_or(&current.name)
            .to_string(),
        goal,
        infinite,
        unit: current.unit.clone(),
        status: current.status.clone(),
        payload,
    };
    drop(repository);
    let portable = update.payload.clone();
    let now = stage_sync::event_now(&connection).map_err(|e| e.to_string())?;
    if stage_sync::normal_edit(&mut connection, &project_id, &stage_id, portable, &now)
        .map_err(|e| e.to_string())?
    {
        return project_payload(&mut connection, &project_id);
    }
    let mut repository = ProjectsRepository::new(&mut connection);
    repository
        .update_stage(&stage_id, &update)
        .map_err(|error| error.to_string())?;
    drop(repository);
    refresh_project_totals(&mut connection, &project_id)?;
    project_payload(&mut connection, &project_id)
}

#[tauri::command]
fn delete_stage(command: StageIdCommand) -> Result<(), String> {
    let mut connection = open_projects_database()?;
    require_projects_owner(&connection)?;
    stage_sync::guard_delete(&connection, &command.project_id).map_err(|e| e.to_string())?;
    let mut repository = ProjectsRepository::new(&mut connection);
    let stage = repository
        .get_stage(&command.stage_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "Этап не найден.".to_string())?;
    if stage.project_id != command.project_id {
        return Err("Этап не относится к указанному проекту.".to_string());
    }
    repository
        .delete_stage_with_notes(&command.stage_id, &command.project_id)
        .map_err(|error| error.to_string())?;
    drop(repository);
    refresh_project_totals(&mut connection, &command.project_id)
}

#[tauri::command]
fn reorder_stages(command: ReorderStagesCommand) -> Result<serde_json::Value, String> {
    let mut connection = open_projects_database()?;
    require_projects_owner(&connection)?;
    let now = stage_sync::event_now(&connection).map_err(|e| e.to_string())?;
    if stage_sync::normal_order(
        &mut connection,
        &command.project_id,
        &command.stage_ids,
        &now,
    )
    .map_err(|e| e.to_string())?
    {
        return project_payload(&mut connection, &command.project_id);
    }
    let mut repository = ProjectsRepository::new(&mut connection);
    repository
        .update_stage_order(&command.project_id, &command.stage_ids)
        .map_err(|error| error.to_string())?;
    drop(repository);
    project_payload(&mut connection, &command.project_id)
}

#[tauri::command]
fn set_project_archived(command: ArchiveProjectCommand) -> Result<serde_json::Value, String> {
    let mut connection = open_projects_database()?;
    require_projects_owner(&connection)?;
    let mut repository = ProjectsRepository::new(&mut connection);
    let current = repository
        .get_project(&command.project_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "Проект не найден.".to_string())?;
    if current.status == "завершен" {
        return Err("Завершённый проект нельзя архивировать или активировать.".to_string());
    }
    let mut payload = current.payload;
    payload["status"] = if command.archived {
        "в архиве".into()
    } else {
        "активен".into()
    };
    if command.archived {
        payload["deadline"] = serde_json::Value::Null;
        payload["streaks"] = serde_json::json!([]);
    }
    repository
        .update_project_payload(&command.project_id, &payload)
        .map_err(|error| error.to_string())?;
    append_event(
        &mut repository,
        &format!(
            "project-status:{}:{}",
            command.project_id,
            if command.archived {
                "archived"
            } else {
                "active"
            }
        ),
        "ProjectStatusChanged",
        &command.project_id,
        None,
        None,
        None,
    )?;
    drop(repository);
    project_payload(&mut connection, &command.project_id)
}

#[tauri::command]
fn complete_project(command: ProjectIdCommand) -> Result<serde_json::Value, String> {
    let mut connection = open_projects_database()?;
    require_projects_owner(&connection)?;
    complete_project_sqlite(&mut connection, command)
}

fn complete_project_sqlite(
    connection: &mut rusqlite::Connection,
    command: ProjectIdCommand,
) -> Result<serde_json::Value, String> {
    let now = now_from_database(connection)?;
    let mut repository = ProjectsRepository::new(connection);
    let current = repository
        .get_project(&command.project_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "Проект не найден.".to_string())?;
    if current.infinite {
        return Err("Бесконечный проект нельзя завершить по числовой цели.".to_string());
    }
    if current.goal.is_some_and(|goal| {
        current
            .payload
            .get("total")
            .and_then(|value| value.as_f64())
            .unwrap_or(0.0)
            < goal
    }) {
        return Err("Сначала достигните цели проекта.".to_string());
    }
    let mut payload = current.payload;
    payload["status"] = "завершен".into();
    payload["completed_at"] = now.clone().into();
    drop(repository);
    let portable_now = stage_sync::event_now(connection).map_err(|e| e.to_string())?;
    let captured = project_metadata_sync::capture_normal_edit(
        connection,
        &command.project_id,
        &payload,
        &portable_now,
    )
    .map_err(|e| e.to_string())?;
    let mut repository = ProjectsRepository::new(connection);
    if !captured {
        repository
            .update_project_payload(&command.project_id, &payload)
            .map_err(|e| e.to_string())?;
    }
    append_event_with_context(
        &mut repository,
        &format!("project-completed:{}", command.project_id),
        "ProjectCompleted",
        &command.project_id,
        None,
        None,
        None,
        serde_json::json!({
            "source": "desktop",
            "version": 1,
            "key": format!("project:{}", command.project_id),
            "total_symbols": payload.get("total").and_then(serde_json::Value::as_f64).unwrap_or(0.0),
        }),
    )?;
    drop(repository);
    project_payload(connection, &command.project_id)
}

#[tauri::command]
fn complete_stage(command: StageIdCommand) -> Result<serde_json::Value, String> {
    let mut connection = open_projects_database()?;
    require_projects_owner(&connection)?;
    complete_stage_sqlite(&mut connection, command)
}

fn complete_stage_sqlite(
    connection: &mut rusqlite::Connection,
    command: StageIdCommand,
) -> Result<serde_json::Value, String> {
    let now = now_from_database(connection)?;
    let mut repository = ProjectsRepository::new(connection);
    let stage = repository
        .get_stage(&command.stage_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "Этап не найден.".to_string())?;
    if stage.project_id != command.project_id {
        return Err("Этап не относится к указанному проекту.".to_string());
    }
    if stage.infinite {
        return Err("Бесконечный этап нельзя завершить по числовой цели.".to_string());
    }
    if stage.goal.is_some_and(|goal| {
        stage
            .payload
            .get("total")
            .and_then(|value| value.as_f64())
            .unwrap_or(0.0)
            < goal
    }) {
        return Err("Сначала достигните цели этапа.".to_string());
    }
    let mut payload = stage.payload;
    payload["status"] = "завершен".into();
    payload["completed_at"] = now.into();
    let total_symbols = payload
        .get("total")
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(0.0);
    let update = StageUpdate {
        name: payload["name"].as_str().unwrap_or_default().to_string(),
        goal: stage.goal,
        infinite: stage.infinite,
        unit: stage.unit,
        status: "завершен".to_string(),
        payload,
    };
    drop(repository);
    let portable = update.payload.clone();
    let now = stage_sync::event_now(connection).map_err(|e| e.to_string())?;
    let captured = stage_sync::normal_edit(
        connection,
        &command.project_id,
        &command.stage_id,
        portable,
        &now,
    )
    .map_err(|e| e.to_string())?;
    let mut repository = ProjectsRepository::new(connection);
    if !captured {
        repository
            .update_stage(&command.stage_id, &update)
            .map_err(|error| error.to_string())?;
    }
    append_event_with_context(
        &mut repository,
        &format!("stage-completed:{}", command.stage_id),
        "StageCompleted",
        &command.project_id,
        Some(&command.stage_id),
        None,
        None,
        serde_json::json!({
            "source": "desktop",
            "version": 1,
            "key": format!("stage:{}:{}", command.project_id, command.stage_id),
            "total_symbols": total_symbols,
        }),
    )?;
    drop(repository);
    project_payload(connection, &command.project_id)
}

#[tauri::command]
fn delete_project(command: ProjectIdCommand) -> Result<(), String> {
    let mut connection = open_projects_database()?;
    require_projects_owner(&connection)?;
    ProjectsRepository::new(&mut connection)
        .delete_project_with_notes(&command.project_id)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn list_project_folders() -> Result<Vec<serde_json::Value>, String> {
    let connection = open_projects_database()?;
    require_projects_owner(&connection)?;
    let mut statement = connection
        .prepare("SELECT id,name FROM project_folders f WHERE NOT EXISTS(SELECT 1 FROM cloud_catalog_projection p WHERE p.entity_type='folder' AND p.entity_id=f.id AND p.deleted=1) ORDER BY position,id")
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([], |row| {
            Ok(serde_json::json!({"id": row.get::<_, String>(0)?, "name": row.get::<_, String>(1)?}))
        })
        .map_err(|error| error.to_string())?
        .map(|row| row.map_err(|error| error.to_string()))
        .collect();
    rows
}

#[tauri::command]
fn create_project_folder(name: String) -> Result<serde_json::Value, String> {
    let mut connection = open_projects_database()?;
    require_projects_owner(&connection)?;
    let name = name.trim().to_string();
    if name.is_empty() || name.chars().count() > 120 {
        return Err("Название папки некорректно.".to_string());
    }
    let id = new_note_id()?;
    let now: String = connection
        .query_row("SELECT strftime('%Y-%m-%dT%H:%M:%f000Z','now')", [], |r| {
            r.get(0)
        })
        .map_err(|e| e.to_string())?;
    if account_catalog::normal(
        &mut connection,
        "folder",
        &id,
        serde_json::json!({"name":name}),
        &now,
    )
    .map_err(|e| e.to_string())?
    {
        return Ok(serde_json::json!({"id":id,"name":name}));
    }

    let position: i64 = connection
        .query_row(
            "SELECT COALESCE(MAX(position),-1)+1 FROM project_folders",
            [],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    connection
        .execute(
            "INSERT INTO project_folders(id,name,position,payload_json) VALUES(?1,?2,?3,?4)",
            rusqlite::params![
                id,
                name,
                position,
                serde_json::json!({"id":id,"name":name}).to_string()
            ],
        )
        .map_err(|error| error.to_string())?;
    Ok(serde_json::json!({"id": id, "name": name}))
}

#[tauri::command]
fn update_project_folder(folder_id: String, name: String) -> Result<serde_json::Value, String> {
    let mut connection = open_projects_database()?;
    require_projects_owner(&connection)?;
    let name = name.trim().to_string();
    if name.is_empty() || name.chars().count() > 120 {
        return Err("Название папки некорректно.".to_string());
    }

    let now: String = connection
        .query_row("SELECT strftime('%Y-%m-%dT%H:%M:%f000Z','now')", [], |r| {
            r.get(0)
        })
        .map_err(|e| e.to_string())?;
    if account_catalog::normal(
        &mut connection,
        "folder",
        &folder_id,
        serde_json::json!({"name":name}),
        &now,
    )
    .map_err(|e| e.to_string())?
    {
        return Ok(serde_json::json!({"id":folder_id,"name":name}));
    }
    let changed = connection.execute("UPDATE project_folders SET name=?1,payload_json=json_set(payload_json,'$.name',?1) WHERE id=?2", rusqlite::params![name,folder_id]).map_err(|error| error.to_string())?;
    if changed != 1 {
        return Err("Папка не найдена.".to_string());
    }
    Ok(serde_json::json!({"id":folder_id,"name":name}))
}

#[tauri::command]
fn delete_project_folder(folder_id: String) -> Result<(), String> {
    let mut connection = open_projects_database()?;
    require_projects_owner(&connection)?;

    let now: String = connection
        .query_row("SELECT strftime('%Y-%m-%dT%H:%M:%f000Z','now')", [], |r| {
            r.get(0)
        })
        .map_err(|e| e.to_string())?;
    if account_catalog::normal(
        &mut connection,
        "folder",
        &folder_id,
        serde_json::Value::Null,
        &now,
    )
    .map_err(|e| e.to_string())?
    {
        return Ok(());
    }
    let tx = connection
        .transaction()
        .map_err(|error| error.to_string())?;
    tx.execute("UPDATE projects SET payload_json=json_set(payload_json,'$.folder_id',NULL) WHERE id IN (SELECT project_id FROM project_folder_members WHERE folder_id=?1)", [&folder_id]).map_err(|error| error.to_string())?;
    tx.execute(
        "DELETE FROM project_folder_members WHERE folder_id=?1",
        [&folder_id],
    )
    .map_err(|error| error.to_string())?;
    let changed = tx
        .execute("DELETE FROM project_folders WHERE id=?1", [&folder_id])
        .map_err(|error| error.to_string())?;
    if changed != 1 {
        return Err("Папка не найдена.".to_string());
    }
    tx.commit().map_err(|error| error.to_string())
}

#[tauri::command]
fn add_project_progress(command: AddProjectProgressCommand) -> Result<serde_json::Value, String> {
    validate_progress_command(&command.project_id, command.new_total, None)?;
    add_progress_sqlite(
        command.project_id,
        None,
        command.new_total,
        command.expected_heads,
    )
}

#[tauri::command]
fn add_stage_progress(command: AddStageProgressCommand) -> Result<serde_json::Value, String> {
    validate_progress_command(
        &command.project_id,
        command.new_total,
        Some(&command.stage_id),
    )?;
    add_progress_sqlite(
        command.project_id,
        Some(command.stage_id),
        command.new_total,
        command.expected_heads,
    )
}

#[tauri::command]
fn delete_progress(command: DeleteProgressCommand) -> Result<serde_json::Value, String> {
    if command.project_id.is_empty() || command.entry_id.is_empty() {
        return Err("Идентификаторы проекта и записи не могут быть пустыми.".to_string());
    }
    if command.stage_id.as_deref().is_some_and(str::is_empty) {
        return Err("Идентификатор этапа не может быть пустым.".to_string());
    }
    delete_progress_sqlite(command.project_id, command.entry_id, command.stage_id)
}

fn convert_to_symbols(value: f64, unit: &str) -> Result<f64, String> {
    unit_factor(unit)
        .map(|factor| value * factor)
        .ok_or_else(|| "Неизвестная единица прогресса.".to_string())
}

fn add_progress_sqlite(
    project_id: String,
    stage_id: Option<String>,
    submitted_total: f64,
    expected_heads: Option<Vec<String>>,
) -> Result<serde_json::Value, String> {
    let mut connection = open_projects_database()?;
    require_projects_owner(&connection)?;
    let now = now_from_database(&connection)?;
    let repository = ProjectsRepository::new(&mut connection);
    let project = repository
        .get_project(&project_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "Проект не найден.".to_string())?;
    let entity = if let Some(stage_id) = stage_id.as_deref() {
        repository
            .get_stage(stage_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "Этап не найден.".to_string())?
    } else {
        StageRecord {
            id: project.id.clone(),
            project_id: project.id.clone(),
            name: project.name.clone(),
            goal: project.goal,
            infinite: project.infinite,
            unit: project.unit.clone(),
            status: project.status.clone(),
            created_at: project.created_at.clone(),
            updated_at: project.updated_at.clone(),
            payload: project.payload.clone(),
        }
    };
    if entity.project_id != project_id {
        return Err("Этап не относится к указанному проекту.".to_string());
    }
    if entity.status == "завершен" {
        return Err("Завершённая сущность доступна только для просмотра.".to_string());
    }
    let method = entity
        .payload
        .get("work_method")
        .and_then(|value| value.as_str())
        .unwrap_or("manual");
    let binding_exists = repository
        .has_sync_binding(&project_id, stage_id.as_deref())
        .map_err(|error| error.to_string())?;
    if method == "sync" || binding_exists {
        return Err("Включена синхронизация. Ручная запись прогресса недоступна.".to_string());
    }
    if method != "manual" && method != "app" {
        return Err("Этот способ добавления записей сейчас не выбран.".to_string());
    }
    let total = normalized_total(submitted_total, &entity.unit)?;
    let previous = entity
        .payload
        .get("total")
        .and_then(|value| value.as_f64())
        .unwrap_or(0.0);
    let next_symbols = convert_to_symbols(total, &entity.unit)?;
    let previous_symbols = convert_to_symbols(previous, &entity.unit)?;
    let delta = next_symbols - previous_symbols;
    if delta.abs() < 0.009 {
        return Err("Значение не изменилось.".to_string());
    }
    let entry_id = new_note_id()?;
    let goal_symbols = entity
        .goal
        .map(|goal| convert_to_symbols(goal, &entity.unit))
        .transpose()?
        .unwrap_or(0.0);
    let added_progress = if goal_symbols <= 0.0 {
        0.0
    } else {
        delta / goal_symbols * 100.0
    };
    drop(repository);
    let tx = connection
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|error| error.to_string())?;
    let day = daily_goals::local_project(&tx, &project_id)?
        .then(|| streaks::logical_writing_day_at(&tx, &now))
        .transpose()?;
    let mut entry = serde_json::json!({"id": entry_id, "new_total": total, "new_total_symbols": next_symbols, "added": total - previous, "added_symbols": delta, "added_progress": added_progress, "created_at": now});
    if let Some(day) = &day {
        entry["writing_day"] = serde_json::json!(day);
    }
    let mut payload = entity.payload;
    let entries = payload
        .get("progress_entries")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    let mut entries = entries;
    entries.push(entry.clone());
    payload["progress_entries"] = serde_json::Value::Array(entries);
    payload["total"] = total.into();
    payload["progress"] = if entity.infinite || entity.goal.unwrap_or(0.0) <= 0.0 {
        0.0.into()
    } else {
        (total / entity.goal.unwrap_or(1.0) * 100.0).into()
    };
    payload["updated_at"] = now.clone().into();

    if stage_id.is_none()
        && tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM stages WHERE project_id=?1)",
                [&project_id],
                |r| r.get::<_, bool>(0),
            )
            .map_err(|e| e.to_string())?
    {
        return Err("Выберите этап для записи прогресса.".into());
    }
    let handled = progress_sync::normal(
        &tx,
        &project_id,
        stage_id.as_deref(),
        &entry,
        expected_heads.as_deref(),
        &now,
    )?;
    if !handled {
        tx.execute("INSERT INTO progress_entries(id,project_id,stage_id,created_at,added_symbols,added_progress,payload_json) VALUES(?1,?2,?3,?4,?5,?6,?7)", rusqlite::params![entry_id, project_id, stage_id, now, delta, added_progress, entry.to_string()]).map_err(|error| error.to_string())?;
        let position: i64 = tx
            .query_row(
                "SELECT COALESCE(MAX(position),-1)+1 FROM progress_order",
                [],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        tx.execute(
            "INSERT INTO progress_order(entry_id,position) VALUES(?1,?2)",
            rusqlite::params![entry_id, position],
        )
        .map_err(|error| error.to_string())?;
        let table = if stage_id.is_some() {
            "stages"
        } else {
            "projects"
        };
        tx.execute(&format!("UPDATE {table} SET goal=?1, infinite=?2, unit=?3, updated_at=?4, payload_json=?5 WHERE id=?6"), rusqlite::params![entity.goal, entity.infinite, entity.unit, now, payload.to_string(), entity.id]).map_err(|error| error.to_string())?;
        if stage_id.is_some() {
            refresh_project_totals_in_transaction(&tx, &project_id)?;
        }
    }
    if let Some(day) = day.filter(|_| !handled) {
        daily_goals::record_from_storage(
            &tx,
            &project_id,
            stage_id.as_deref(),
            &day,
            &now,
            delta > 0.0,
        )?;
    }
    let game_key = stage_id
        .as_deref()
        .map(|stage| format!("stage:{project_id}:{stage}"))
        .unwrap_or_else(|| format!("project:{project_id}"));
    tx.execute("INSERT OR IGNORE INTO domain_events(event_id,event_type,project_id,stage_id,progress_id,delta_symbols,context_json,created_at) VALUES(?1,'ProgressAdded',?2,?3,?4,?5,?6,?7)", rusqlite::params![format!("progress-added:{entry_id}"), project_id, stage_id, entry_id, delta, serde_json::json!({"source":"desktop","version":1,"key":game_key,"project_progress":payload["progress"]}).to_string(), now]).map_err(|error| error.to_string())?;
    tx.commit().map_err(|error| error.to_string())?;
    let project_value = project_payload(&mut connection, &project_id)?;
    Ok(
        serde_json::json!({"project": project_value, "entry": entry, "added_symbols": delta, "game": null, "warning": null}),
    )
}

fn delete_progress_sqlite(
    project_id: String,
    entry_id: String,
    stage_id: Option<String>,
) -> Result<serde_json::Value, String> {
    let mut connection = open_projects_database()?;
    require_projects_owner(&connection)?;
    if progress_sync::local_heads(&connection, &project_id, stage_id.as_deref())?.is_some() {
        return Err("progress_descendant_rebase_required".into());
    }
    let now = now_from_database(&connection)?;
    let mut repository = ProjectsRepository::new(&mut connection);
    let project = repository
        .get_project(&project_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "Проект не найден.".to_string())?;
    let entity_id = stage_id.clone().unwrap_or_else(|| project_id.clone());
    let entity = if let Some(stage_id) = stage_id.as_deref() {
        repository
            .get_stage(stage_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "Этап не найден.".to_string())?
    } else {
        StageRecord {
            id: project.id.clone(),
            project_id: project.id.clone(),
            name: project.name.clone(),
            goal: project.goal,
            infinite: project.infinite,
            unit: project.unit.clone(),
            status: project.status.clone(),
            created_at: project.created_at.clone(),
            updated_at: project.updated_at.clone(),
            payload: project.payload.clone(),
        }
    };
    if entity.project_id != project_id {
        return Err("Этап не относится к указанному проекту.".to_string());
    }
    let mut entries = entity
        .payload
        .get("progress_entries")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    let before = entries.len();
    entries.retain(|entry| {
        entry.get("id").and_then(|value| value.as_str()) != Some(entry_id.as_str())
    });
    if entries.len() == before {
        return Err("Запись прогресса не найдена.".to_string());
    }
    let total = entries
        .last()
        .and_then(|entry| entry.get("new_total"))
        .and_then(|value| value.as_f64())
        .unwrap_or(0.0);
    let mut payload = entity.payload;
    payload["progress_entries"] = serde_json::Value::Array(entries);
    payload["total"] = total.into();
    payload["progress"] = if entity.infinite || entity.goal.unwrap_or(0.0) <= 0.0 {
        0.0.into()
    } else {
        (total / entity.goal.unwrap_or(1.0) * 100.0).into()
    };
    drop(repository);
    let tx = connection
        .transaction()
        .map_err(|error| error.to_string())?;
    let deleted = tx.execute("DELETE FROM progress_entries WHERE id=?1 AND project_id=?2 AND (?3 IS NULL OR stage_id=?3)", rusqlite::params![entry_id, project_id, stage_id]).map_err(|error| error.to_string())?;
    if deleted != 1 {
        return Err("Запись прогресса не найдена.".to_string());
    }
    let table = if stage_id.is_some() {
        "stages"
    } else {
        "projects"
    };
    tx.execute(
        &format!("UPDATE {table} SET updated_at=?1,payload_json=?2 WHERE id=?3"),
        rusqlite::params![now, payload.to_string(), entity_id],
    )
    .map_err(|error| error.to_string())?;
    if stage_id.is_some() {
        refresh_project_totals_in_transaction(&tx, &project_id)?;
    }
    tx.execute("INSERT OR IGNORE INTO domain_events(event_id,event_type,project_id,stage_id,progress_id,context_json,created_at) VALUES(?1,'ProgressDeleted',?2,?3,?4,?5,?6)", rusqlite::params![format!("progress-deleted:{entry_id}"), project_id, stage_id, entry_id, serde_json::json!({"source":"desktop","version":1}).to_string(), now]).map_err(|error| error.to_string())?;
    tx.commit().map_err(|error| error.to_string())?;
    Ok(project_payload(&mut connection, &project_id)?)
}

fn sqlite_data_root() -> Result<PathBuf, String> {
    if let Ok(path) = std::env::var("NFPROGRESS_DATA_DIR") {
        return Ok(PathBuf::from(path));
    }
    let home = dirs_fallback_home()?;
    let root = if cfg!(target_os = "windows") {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("AppData").join("Roaming"))
            .join("nfprogress")
    } else if cfg!(target_os = "macos") {
        home.join("Documents").join("nfprogress")
    } else {
        let documents = home.join("Documents");
        if documents.exists() {
            documents.join("nfprogress")
        } else {
            home.join(".local").join("share").join("nfprogress")
        }
    };
    if build_profile() == "test" {
        Ok(root.join("test_data"))
    } else {
        Ok(root)
    }
}

fn dirs_fallback_home() -> Result<PathBuf, String> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .ok_or_else(|| "Не удалось определить домашнюю директорию пользователя.".to_string())
}

fn read_sqlite_entity_rows(
    connection: &rusqlite::Connection,
    table: &str,
) -> Result<Vec<SqliteEntityRow>, String> {
    let sql = match table {
        "projects" => "SELECT p.id, NULL, p.name, p.goal, p.infinite, p.unit, p.status, p.created_at, p.updated_at, p.payload_json FROM projects p JOIN project_order o ON o.project_id = p.id ORDER BY o.position",
        "stages" => "SELECT s.id, s.project_id, s.name, s.goal, s.infinite, s.unit, s.status, s.created_at, s.updated_at, s.payload_json FROM stages s JOIN stage_order o ON o.stage_id=s.id ORDER BY o.project_id,o.position",
        _ => return Err("Недопустимая таблица SQLite.".to_string()),
    };
    let mut statement = connection.prepare(sql).map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([], |row| {
            Ok(SqliteEntityRow {
                id: row.get(0)?,
                project_id: row.get(1)?,
                name: row.get(2)?,
                goal: row.get(3)?,
                infinite: row.get(4)?,
                unit: row.get(5)?,
                status: row.get(6)?,
                created_at: row.get(7)?,
                updated_at: row.get(8)?,
                payload_json: row.get(9)?,
            })
        })
        .map_err(|error| error.to_string())?;
    rows.map(|row| row.map_err(|error| error.to_string()))
        .collect()
}

#[tauri::command]
fn read_sqlite_projects() -> Result<SqliteProjectReadModel, String> {
    let database = sqlite_data_root()?.join("nfprogress.db");
    let connection = sqlite::open_database(&database).map_err(|error| error.to_string())?;
    let status = projects_read_status(&connection)?;
    let mut progress = connection
        .prepare("SELECT p.id, p.project_id, p.stage_id, p.created_at, p.added_symbols, p.added_progress, p.payload_json FROM progress_entries p JOIN progress_order o ON o.entry_id=p.id ORDER BY o.position")
        .map_err(|error| error.to_string())?;
    let progress_entries = progress
        .query_map([], |row| {
            Ok(SqliteProgressRow {
                id: row.get(0)?,
                project_id: row.get(1)?,
                stage_id: row.get(2)?,
                created_at: row.get(3)?,
                added_symbols: row.get(4)?,
                added_progress: row.get(5)?,
                payload_json: row.get(6)?,
            })
        })
        .map_err(|error| error.to_string())?
        .map(|row| row.map_err(|error| error.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    let project_order_rows = connection
        .prepare("SELECT project_id, position FROM project_order ORDER BY position, project_id")
        .map_err(|error| error.to_string())?
        .query_map([], |row| {
            Ok((row.get::<_, i64>(1)?, row.get::<_, String>(0)?))
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    if project_order_rows
        .iter()
        .enumerate()
        .any(|(expected, (position, _))| *position != expected as i64)
    {
        return Err("SQLite project ordering positions are invalid.".to_string());
    }
    let project_order: Vec<String> = project_order_rows
        .into_iter()
        .map(|(_, project_id)| project_id)
        .collect();
    let project_ids: HashSet<&str> = project_order.iter().map(String::as_str).collect();
    if project_order.len() != project_ids.len() {
        return Err("SQLite project ordering contains duplicates.".to_string());
    }
    let mut project_rows = read_sqlite_entity_rows(&connection, "projects")?;
    if project_order.len() != project_rows.len()
        || project_rows
            .iter()
            .any(|row| !project_ids.contains(row.id.as_str()))
    {
        return Err("SQLite project ordering is incomplete.".to_string());
    }
    let mut stage_rows = read_sqlite_entity_rows(&connection, "stages")?;
    // Streaks are Game-owned data.  The web path overlays this exact
    // projection before serializing projects; doing so here keeps desktop from
    // rendering stale project-row copies or independently re-evaluating dates.
    let game_state = connection
        .query_row(
            "SELECT payload_json FROM game_state WHERE id=1",
            [],
            |row| row.get::<_, String>(0),
        )
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok());
    if let Some(game_state) = game_state.as_ref() {
        let logical_day = streaks::logical_writing_day(&connection)?;
        overlay_game_streak_state(&mut project_rows, game_state, false, &logical_day);
        overlay_game_streak_state(&mut stage_rows, game_state, true, &logical_day);
    }
    daily_goals::project_rows(
        &connection,
        &mut project_rows,
        &mut stage_rows,
        &progress_entries,
    )?;
    Ok(SqliteProjectReadModel {
        mirror_status: status,
        project_order,
        projects: project_rows,
        stages: stage_rows,
        progress_entries,
    })
}

#[tauri::command]
fn projects_storage_owner() -> Result<String, String> {
    let connection = open_projects_database()?;
    connection
        .query_row(
            "SELECT owner FROM storage_ownership WHERE subsystem='projects'",
            [],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())
}

fn configure_rustls_provider() {
    // The updater plugin installs a provider in release builds. Tauri dev does
    // not load that plugin, so install ring here before reqwest creates TLS.
    let _ = rustls::crypto::ring::default_provider().install_default();
}

#[tauri::command]
async fn fetch_update_manifest() -> Result<serde_json::Value, String> {
    configure_rustls_provider();
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(UPDATE_MANIFEST_TIMEOUT)
        .build()
        .map_err(|error| format!("Не удалось настроить проверку обновлений: {error}"))?;
    let response = client
        .get(UPDATE_MANIFEST_URL)
        .send()
        .await
        .map_err(|error| format!("Не удалось получить манифест обновления: {error}"))?
        .error_for_status()
        .map_err(|error| format!("Сервер обновлений вернул ошибку: {error}"))?;
    response
        .json::<serde_json::Value>()
        .await
        .map_err(|error| format!("Манифест обновления имеет неверный формат: {error}"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn account_catalog_membership_patch_distinguishes_absent_move_and_remove() {
        let absent: super::ProjectUpdateCommand =
            serde_json::from_value(serde_json::json!({})).unwrap();
        let remove: super::ProjectUpdateCommand =
            serde_json::from_value(serde_json::json!({"folderId":null})).unwrap();
        let assign: super::ProjectUpdateCommand =
            serde_json::from_value(serde_json::json!({"folderId":"F"})).unwrap();
        assert_eq!(absent.folder_id, None);
        assert_eq!(remove.folder_id, Some(None));
        assert_eq!(assign.folder_id, Some(Some("F".into())));
    }

    use std::fs;
    #[cfg(target_os = "macos")]
    use std::io::Write;
    use std::path::Path;
    use std::path::PathBuf;
    #[cfg(target_os = "macos")]
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicU64, Ordering};

    use rusqlite::Connection;

    use super::{
        build_macos_updater_script, configure_rustls_provider, encode_path_segment,
        macos_update_target, overlay_game_streak_state, reconcile_map_notes,
        AddProjectProgressCommand, AddStageProgressCommand, DeleteProgressCommand,
        ProjectMetadataPatch, SqliteEntityRow,
    };

    const CLOUD_USER_ONE: &str = "abcdefab-0000-0000-0000-000000000101";
    const CLOUD_USER_TWO: &str = "00000000-0000-0000-0000-000000000102";
    static LIFECYCLE_DATABASE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    fn lifecycle_database_root(label: &str) -> PathBuf {
        let sequence = LIFECYCLE_DATABASE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "nfprogress-note-sync-lifecycle-{label}-{}-{sequence}",
            std::process::id(),
        ))
    }

    #[test]
    fn headless_disk_profile_reopens_projects_notes_and_maps_after_failed_write() {
        let root = lifecycle_database_root("desktop-acceptance");
        fs::create_dir(&root).unwrap();
        let path = root.join("nfprogress.db");
        let mut db = super::sqlite::open_database(&path).unwrap();
        super::initialize_fresh_desktop_database(&db, &root).unwrap();
        let project = super::ProjectRecord {
            id: "acceptance-project".into(),
            name: "Книга".into(),
            goal: Some(1000.0),
            infinite: false,
            unit: "symbols".into(),
            status: "активен".into(),
            created_at: None,
            updated_at: None,
            payload: serde_json::json!({"id":"acceptance-project","name":"Книга"}),
        };
        super::ProjectsRepository::new(&mut db)
            .insert_project(&project)
            .unwrap();
        super::create_note_in_connection(&mut db, &project.id, None, "note-1").unwrap();
        super::update_note_in_connection(
            &mut db,
            &project.id,
            "note-1",
            &serde_json::json!({"title":"Идея","content":"Сохранённый текст"}),
            None,
        )
        .unwrap();
        let map = serde_json::json!({"nodeData":{"id":"map","topic":"Книга","children":[]},"freeNodes":[{"id":"node-1","topic":"Узел","children":[],"nfprogressNote":true}]});
        let normalized = super::mindmap::normalize(map.clone()).unwrap();
        let command = super::MapCommand {
            project_id: project.id.clone(),
            stage_id: None,
            data: map,
            expected_heads: None,
        };
        super::save_map_in_connection(&mut db, &command, &normalized).unwrap();
        let before: String = db
            .query_row(
                "SELECT payload_json FROM notes WHERE id='note-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(super::update_note_in_connection(
            &mut db,
            &project.id,
            "note-1",
            &serde_json::json!({"content":"не сохранять","invalid":true}),
            None
        )
        .is_err());
        db.execute_batch("CREATE TRIGGER acceptance_fail_note BEFORE UPDATE ON notes BEGIN SELECT RAISE(ABORT, 'injected storage failure'); END;").unwrap();
        assert!(super::update_note_in_connection(
            &mut db,
            &project.id,
            "note-1",
            &serde_json::json!({"content":"не сохранять"}),
            None
        )
        .is_err());
        db.execute_batch("DROP TRIGGER acceptance_fail_note;")
            .unwrap();
        drop(db);

        let mut db = super::sqlite::open_database(&path).unwrap();
        assert_eq!(
            super::ProjectsRepository::new(&mut db)
                .get_project(&project.id)
                .unwrap()
                .unwrap()
                .name,
            "Книга"
        );
        let after: String = db
            .query_row(
                "SELECT payload_json FROM notes WHERE id='note-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(before, after);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&after).unwrap()["content"],
            "Сохранённый текст"
        );
        let loaded = super::map_response_in_connection(&mut db, project.id.clone(), None).unwrap();
        assert_eq!(loaded["data"]["freeNodes"][0]["topic"], "Узел");
        db.execute(
            "INSERT INTO notes(id,project_id,payload_json) VALUES('damaged',?1,'null')",
            [&project.id],
        )
        .unwrap();
        assert!(super::update_note_in_connection(
            &mut db,
            &project.id,
            "damaged",
            &serde_json::json!({"content":"replacement"}),
            None,
        )
        .is_err());
        drop(db);
        let db = super::sqlite::open_database(&path).unwrap();
        let damaged: String = db
            .query_row(
                "SELECT payload_json FROM notes WHERE id='damaged'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let intact: String = db
            .query_row(
                "SELECT payload_json FROM notes WHERE id='note-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(damaged, "null");
        assert_eq!(intact, before);
        assert_eq!(
            db.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
        drop(db);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn fresh_desktop_database_becomes_native_authoritative() {
        let connection = Connection::open_in_memory().unwrap();
        super::sqlite::apply_migrations(&connection).unwrap();

        super::initialize_fresh_desktop_database(
            &connection,
            Path::new("/nfprogress/f7-fresh-install"),
        )
        .unwrap();

        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM storage_ownership WHERE owner='sqlite'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            4
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT sync_status FROM mirror_state WHERE id=1",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "healthy"
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT payload_json FROM game_state WHERE id=1",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "{\"gamer\":{},\"game\":{}}"
        );
    }

    #[test]
    fn sqlite_owned_projects_remain_readable_with_a_stale_dirty_mirror_marker() {
        let connection = Connection::open_in_memory().unwrap();
        super::sqlite::apply_migrations(&connection).unwrap();
        super::initialize_fresh_desktop_database(
            &connection,
            Path::new("/nfprogress/f7-fresh-install"),
        )
        .unwrap();
        connection
            .execute(
                "UPDATE mirror_state SET sync_status='dirty', last_error='stale legacy mirror error' WHERE id=1",
                [],
            )
            .unwrap();
        assert_eq!(super::projects_read_status(&connection).unwrap(), "healthy");

        let mut connection = connection;
        let project = super::ProjectRecord {
            id: "dirty-mirror-project".into(),
            name: "Локальный проект".into(),
            goal: None,
            infinite: true,
            unit: "symbols".into(),
            status: "активен".into(),
            created_at: None,
            updated_at: None,
            payload: serde_json::json!({"id":"dirty-mirror-project","name":"Локальный проект"}),
        };
        super::ProjectsRepository::new(&mut connection)
            .insert_project(&project)
            .unwrap();
        super::create_note_in_connection(&mut connection, &project.id, None, "dirty-mirror-note")
            .unwrap();
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM notes", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            1
        );
    }

    #[test]
    fn sqlite_file_backed_identity_command_helpers_preserve_sync_state_across_restart() {
        let root = lifecycle_database_root("restart");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("nfprogress.db");
        let command = super::account_binding::CloudIdentityCommand {
            canonical_user_id: CLOUD_USER_ONE.to_string(),
        };
        let identity = {
            let mut connection = super::sqlite::open_database(&path).unwrap();
            super::initialize_fresh_desktop_database(&connection, &root).unwrap();
            let identity =
                super::provision_cloud_identity_on_connection(&mut connection, &command).unwrap();
            connection
                .execute(
                    "UPDATE cloud_sync_state SET pull_cursor=2,ack_cursor=1 WHERE account_id=?1",
                    [&identity.local_account_id],
                )
                .unwrap();
            connection.execute(
                "INSERT INTO cloud_sync_outbox(event_id,account_id,device_id,project_id,entity_id,entity_type,operation,revision,updated_at,deleted_at,created_at,parent_event_id,local_ordinal,lifecycle)
                 VALUES('123e4567-e89b-42d3-a456-426614174002',?1,?2,'project','note','note','upsert',1,'2026-09-23T00:00:00Z',NULL,'2026-09-23T00:00:00Z',NULL,1,'sealed')",
                rusqlite::params![identity.local_account_id, identity.device_id],
            ).unwrap();
            connection.execute(
                "INSERT INTO cloud_sync_inbox(account_id,event_id,server_sequence,device_id,project_id,entity_id,entity_type,operation,sync_revision,updated_at,deleted_at,state,received_at)
                 VALUES(?1,'123e4567-e89b-42d3-a456-426614174003',2,'123e4567-e89b-42d3-a456-426614174004','project','remote-note','note','upsert',1,'2026-09-23T00:00:00Z',NULL,'applied','2026-09-23T00:00:00Z')",
                [&identity.local_account_id],
            ).unwrap();
            identity
        };
        let mut reopened = super::sqlite::open_database(&path).unwrap();
        assert_eq!(
            super::read_cloud_identity_on_connection(&mut reopened, &command).unwrap(),
            Some(identity.clone())
        );
        assert_eq!(
            super::provision_cloud_identity_on_connection(&mut reopened, &command).unwrap(),
            identity.clone()
        );
        assert_eq!(
            reopened
                .query_row(
                    "SELECT pull_cursor,ack_cursor FROM cloud_sync_state WHERE account_id=?1",
                    [&identity.local_account_id],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
                )
                .unwrap(),
            (2, 1)
        );
        assert_eq!(
            reopened
                .query_row(
                    "SELECT count(*) FROM cloud_sync_outbox WHERE account_id=?1",
                    [&identity.local_account_id],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
        assert_eq!(
            reopened
                .query_row(
                    "SELECT count(*) FROM cloud_sync_inbox WHERE account_id=?1",
                    [&identity.local_account_id],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
        let mismatch = super::account_binding::EnsureCloudAccountBindingCommand {
            local_account_id: identity.local_account_id.clone(),
            canonical_user_id: CLOUD_USER_TWO.to_string(),
        };
        assert!(
            super::ensure_cloud_account_binding_on_connection(&mut reopened, &mismatch).is_err()
        );
        drop(reopened);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn sqlite_identity_command_helpers_require_native_notes_ownership() {
        let mut connection = Connection::open_in_memory().unwrap();
        super::sqlite::apply_migrations(&connection).unwrap();
        let command = super::account_binding::CloudIdentityCommand {
            canonical_user_id: CLOUD_USER_ONE.to_string(),
        };
        assert!(super::provision_cloud_identity_on_connection(&mut connection, &command).is_err());
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM cloud_sync_state", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn sqlite_two_file_backed_desktop_fixtures_keep_distinct_devices_for_one_cloud_user() {
        let command = super::account_binding::CloudIdentityCommand {
            canonical_user_id: CLOUD_USER_ONE.to_string(),
        };
        let first_root = lifecycle_database_root("device-a");
        let second_root = lifecycle_database_root("device-b");
        let provision = |root: &Path| {
            fs::create_dir_all(root).unwrap();
            let mut connection = super::sqlite::open_database(&root.join("nfprogress.db")).unwrap();
            super::initialize_fresh_desktop_database(&connection, root).unwrap();
            super::provision_cloud_identity_on_connection(&mut connection, &command).unwrap()
        };
        let first = provision(&first_root);
        let second = provision(&second_root);
        assert_ne!(first.local_account_id, second.local_account_id);
        assert_ne!(first.device_id, second.device_id);
        fs::remove_dir_all(first_root).unwrap();
        fs::remove_dir_all(second_root).unwrap();
    }

    #[test]
    fn desktop_streak_projection_uses_the_same_game_owned_values_as_web() {
        let mut rows = vec![SqliteEntityRow {
            id: "source-1".into(),
            project_id: Some("project-1".into()),
            name: Some("Источник".into()),
            goal: None,
            infinite: 0,
            unit: "symbols".into(),
            status: "активен".into(),
            created_at: None,
            updated_at: None,
            payload_json: serde_json::json!({
                "streak_status": "No", "streak_length": 0,
            })
            .to_string(),
        }];
        let game_state = serde_json::json!({
            "project_game_state": {
                "stage:project-1:source-1": {
                    "streak_status": "Active",
                    "streaks": ["2026-09-08"],
                    "max_streak": 7,
                    "freezes": 1,
                }
            }
        });

        overlay_game_streak_state(&mut rows, &game_state, true, "2026-09-09");
        let desktop = serde_json::from_str::<serde_json::Value>(&rows[0].payload_json).unwrap();
        let web = &game_state["project_game_state"]["stage:project-1:source-1"];
        assert_eq!(desktop["streak_status"], web["streak_status"]);
        assert_eq!(desktop["streaks"], web["streaks"]);
        assert_eq!(desktop["max_streak"], web["max_streak"]);
        assert_eq!(desktop["freezes"], web["freezes"]);
    }

    #[test]
    fn desktop_streak_projection_uses_payload_history_when_game_state_omits_it() {
        let mut rows = vec![SqliteEntityRow {
            id: "project-1".into(),
            project_id: None,
            name: Some("Проект".into()),
            goal: None,
            infinite: 0,
            unit: "symbols".into(),
            status: "активен".into(),
            created_at: None,
            updated_at: None,
            payload_json: serde_json::json!({
                "streaks": ["2026-09-17", "freeze"],
                "streak_status": "Freeze",
            })
            .to_string(),
        }];
        let game_state = serde_json::json!({
            "project_game_state": {
                "project:project-1": { "streak_status": "Freeze", "max_streak": 2 }
            }
        });

        overlay_game_streak_state(&mut rows, &game_state, false, "2026-09-19");
        let desktop = serde_json::from_str::<serde_json::Value>(&rows[0].payload_json).unwrap();
        assert_eq!(desktop["streak_status"], "Active");
        assert_eq!(
            desktop["streaks"],
            serde_json::json!(["2026-09-17", "freeze"])
        );
    }

    #[test]
    fn legacy_files_require_explicit_migration_on_startup() {
        let root = std::env::temp_dir().join(format!(
            "nfprogress-f7-legacy-boundary-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("data.pkl"), b"legacy").unwrap();

        let connection = Connection::open_in_memory().unwrap();
        super::sqlite::apply_migrations(&connection).unwrap();
        let error = super::initialize_fresh_desktop_database(&connection, &root).unwrap_err();
        assert!(error.starts_with("migration_required:"));
        assert_eq!(
            connection
                .query_row(
                    "SELECT owner FROM storage_ownership WHERE subsystem='projects'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "pickle"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn startup_storage_check_reports_incomplete_project_order_without_panicking() {
        let root = std::env::temp_dir().join(format!(
            "nfprogress-startup-schema-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();

        let database = root.join("nfprogress.db");
        let connection = Connection::open(&database).unwrap();
        super::sqlite::apply_migrations(&connection).unwrap();
        connection
            .execute(
                "INSERT INTO projects(id,name,goal,infinite,unit,status,payload_json) VALUES('project-1','Project',NULL,1,'symbols','active','{}')",
                [],
            )
            .unwrap();
        drop(connection);

        assert_eq!(
            super::prepare_startup_storage(&root).unwrap_err(),
            "Corrupt SQLite schema: project ordering is incomplete"
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn project_metadata_payload_is_narrow_and_encodes_ids() {
        let patch: ProjectMetadataPatch =
            serde_json::from_value(serde_json::json!({"name": "Новый", "deadline": null}))
                .expect("allow-listed metadata must deserialize");
        assert_eq!(patch.name.as_deref(), Some("Новый"));
        assert!(matches!(patch.deadline, super::MetadataDeadline::Null));
        assert!(
            serde_json::from_value::<ProjectMetadataPatch>(serde_json::json!({
                "total": 10
            }))
            .is_err()
        );
        assert_eq!(encode_path_segment("id/ два"), "id%2F%20%D0%B4%D0%B2%D0%B0");
    }

    #[test]
    fn progress_commands_are_strict_and_keep_stage_target_explicit() {
        let project: AddProjectProgressCommand = serde_json::from_value(serde_json::json!({
            "projectId": "project/1",
            "newTotal": 42.5,
        }))
        .expect("project progress command must deserialize");
        assert_eq!(project.project_id, "project/1");
        assert!(
            serde_json::from_value::<AddProjectProgressCommand>(serde_json::json!({
                "projectId": "p",
                "newTotal": 1,
                "stageId": "unexpected",
            }))
            .is_err()
        );

        let stage: AddStageProgressCommand = serde_json::from_value(serde_json::json!({
            "projectId": "p",
            "stageId": "s",
            "newTotal": 42,
        }))
        .expect("stage progress command must deserialize");
        assert_eq!(stage.stage_id, "s");
        assert!(
            serde_json::from_value::<DeleteProgressCommand>(serde_json::json!({
                "projectId": "p",
                "entryId": "e",
                "arbitrarySql": "DELETE FROM progress_entries",
            }))
            .is_err()
        );
    }

    #[test]
    fn progress_command_validation_rejects_invalid_numeric_values() {
        assert!(super::validate_progress_command("p", -1.0, None).is_err());
        assert!(super::validate_progress_command("p", f64::NAN, None).is_err());
        assert!(super::validate_progress_command("p", f64::INFINITY, None).is_err());
        assert!(super::validate_progress_command("p", 1.0, Some("")).is_err());
        assert!(super::validate_progress_command("p", 1.0, Some("s")).is_ok());
    }

    #[test]
    fn map_response_reads_notes_without_opening_a_second_writer() {
        let root = lifecycle_database_root("map-read");
        fs::create_dir_all(&root).unwrap();
        let database = root.join("test.db");
        let mut connection = Connection::open(&database).unwrap();
        super::sqlite::apply_migrations(&connection).unwrap();
        super::initialize_fresh_desktop_database(&connection, &root).unwrap();
        connection.execute("INSERT INTO projects(id,name,goal,infinite,unit,status,payload_json) VALUES('p','Project',0,0,'symbols','active','{}')", []).unwrap();
        let map = serde_json::json!({"nodeData":{"id":"map","topic":"Project","children":[]},"freeNodes":[{"id":"note","topic":"First","children":[],"nfprogressNote":true}]});
        let transaction = connection.unchecked_transaction().unwrap();
        reconcile_map_notes(&transaction, "p", None, &map, "now").unwrap();
        transaction.commit().unwrap();

        let response =
            super::map_response_in_connection(&mut connection, "p".into(), None).unwrap();
        assert_eq!(response["notes"].as_array().unwrap().len(), 1);
        drop(connection);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn map_note_reconciliation_is_idempotent_and_removes_deleted_nodes() {
        let connection = Connection::open_in_memory().unwrap();
        super::sqlite::apply_migrations(&connection).unwrap();
        connection
            .execute(
                "INSERT INTO projects(id,name,goal,infinite,unit,status,payload_json) VALUES('p','Project',0,0,'symbols','active','{}')",
                [],
            )
            .unwrap();
        let map = serde_json::json!({
            "nodeData": {"id": "map", "topic": "Project", "children": []},
            "freeNodes": [{"id": "note", "topic": "First", "children": [], "nfprogressNote": true}]
        });
        let first = connection.unchecked_transaction().unwrap();
        assert!(reconcile_map_notes(&first, "p", None, &map, "now").unwrap());
        first.commit().unwrap();
        let second = connection.unchecked_transaction().unwrap();
        assert!(!reconcile_map_notes(&second, "p", None, &map, "now").unwrap());
        second.commit().unwrap();
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM notes WHERE project_id='p'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
        let empty = serde_json::json!({
            "nodeData": {"id": "map", "topic": "Project", "children": []}
        });
        let third = connection.unchecked_transaction().unwrap();
        assert!(reconcile_map_notes(&third, "p", None, &empty, "later").unwrap());
        third.commit().unwrap();
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM notes WHERE project_id='p'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
    }

    #[test]
    fn update_client_has_a_rustls_crypto_provider() {
        configure_rustls_provider();
        assert!(reqwest::Client::builder().build().is_ok());
    }

    #[test]
    fn macos_updater_can_install_an_app_from_a_nested_dmg() {
        let script = build_macos_updater_script(
            Path::new("/tmp/nfprogress-update.zip"),
            Path::new("/Applications/nfprogress.app"),
            "aabbcc",
            42,
            Path::new("/tmp/nfprogress-update.log"),
            123,
        );

        assert!(script.contains("DMG_PATH=$(find"));
        assert!(script.contains("hdiutil attach \"$DMG_PATH\""));
        assert!(script.contains("find \"$MOUNT_POINT\""));
        assert!(script.contains("ditto \"$NEW_APP\" \"$TARGET_PATH\""));
        assert!(!script.contains("curl "));
    }

    #[cfg(debug_assertions)]
    #[test]
    fn tauri_dev_updates_the_installed_macos_app() {
        assert_eq!(
            macos_update_target(Path::new("/project/target/debug/nfprogress-desktop")),
            Some(Path::new("/Applications/nfprogress.app").to_path_buf()),
        );
    }

    #[test]
    fn packaged_macos_app_is_resolved_from_the_current_executable() {
        assert_eq!(
            macos_update_target(Path::new(
                "/Applications/nfprogress.app/Contents/MacOS/nfprogress-desktop",
            )),
            Some(Path::new("/Applications/nfprogress.app").to_path_buf()),
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_updater_script_has_valid_shell_syntax() {
        let script = build_macos_updater_script(
            Path::new("/tmp/nfprogress-update.zip"),
            Path::new("/Applications/nfprogress.app"),
            "aabbcc",
            42,
            Path::new("/tmp/nfprogress-update.log"),
            123,
        );
        let mut child = Command::new("/bin/sh")
            .arg("-n")
            .stdin(Stdio::piped())
            .spawn()
            .expect("shell must start");
        child
            .stdin
            .take()
            .expect("shell stdin must be piped")
            .write_all(script.as_bytes())
            .expect("script must be written to shell");

        assert!(child.wait().expect("shell must finish").success());
    }
}

fn shell_literal(value: impl AsRef<str>) -> String {
    format!("'{}'", value.as_ref().replace('\'', "'\\''"))
}

fn macos_app_bundle(executable: &Path) -> Option<PathBuf> {
    executable
        .ancestors()
        .find(|path| path.extension().is_some_and(|extension| extension == "app"))
        .map(Path::to_path_buf)
}

fn macos_update_target(executable: &Path) -> Option<PathBuf> {
    macos_app_bundle(executable)
        .or_else(|| cfg!(debug_assertions).then(|| PathBuf::from("/Applications/nfprogress.app")))
}

fn build_macos_updater_script(
    archive_path: &Path,
    target: &Path,
    sha256: &str,
    size: u64,
    log_path: &Path,
    parent_pid: u32,
) -> String {
    let target_name = target.file_name().unwrap_or_default().to_string_lossy();
    format!(
        r#"#!/bin/sh
set -u
ARCHIVE_PATH={archive}
SHA256={sha256}
SIZE={size}
TARGET_PATH={target}
TARGET_NAME={target_name}
PARENT_PID={pid}
LOG_PATH={log_path}
MOUNT_POINT=""

log() {{
    printf '%s %s\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')" "$1" >> "$LOG_PATH"
}}

fail() {{
    log "failed: $1"
    if [ -n "$MOUNT_POINT" ]; then
        hdiutil detach "$MOUNT_POINT" >/dev/null 2>&1 || true
    fi
    osascript -e 'display alert "nfprogress" message "Не удалось установить обновление. Подробности: '"$LOG_PATH"'"' >/dev/null 2>&1 || true
    exit 1
}}

log "waiting for application process $PARENT_PID"
while kill -0 "$PARENT_PID" >/dev/null 2>&1; do sleep 1; done
sleep 1

WORK_DIR=$(mktemp -d "${{TMPDIR:-/tmp/}}nfprogress-update.XXXXXX") || fail "Не удалось создать временную папку."
EXTRACT_DIR="$WORK_DIR/extract"
mkdir -p "$EXTRACT_DIR" || fail "Не удалось создать папку распаковки."

log "using downloaded archive $ARCHIVE_PATH"
[ -f "$ARCHIVE_PATH" ] || fail "Скачанный архив обновления не найден."
[ "$(stat -f %z "$ARCHIVE_PATH")" = "$SIZE" ] || fail "Размер архива обновления не совпадает с манифестом."
[ "$(shasum -a 256 "$ARCHIVE_PATH" | awk '{{print $1}}')" = "$SHA256" ] || fail "Контрольная сумма архива обновления не совпадает с манифестом."
ditto -x -k "$ARCHIVE_PATH" "$EXTRACT_DIR" || unzip -q "$ARCHIVE_PATH" -d "$EXTRACT_DIR" || fail "Не удалось распаковать zip-архив."

NEW_APP=$(find "$EXTRACT_DIR" -maxdepth 4 -name "$TARGET_NAME" -type d -print -quit)
if [ -z "$NEW_APP" ]; then
    NEW_APP=$(find "$EXTRACT_DIR" -maxdepth 4 -name "*.app" -type d -print -quit)
fi

if [ -z "$NEW_APP" ]; then
    DMG_PATH=$(find "$EXTRACT_DIR" -maxdepth 4 -name "*.dmg" -type f -print -quit)
    if [ -n "$DMG_PATH" ]; then
        log "mounting $DMG_PATH"
        MOUNT_OUTPUT=$(hdiutil attach "$DMG_PATH" -nobrowse -readonly 2>>"$LOG_PATH") || fail "Не удалось смонтировать dmg."
        MOUNT_POINT=$(printf '%s\n' "$MOUNT_OUTPUT" | awk '/\/Volumes\// {{print substr($0, index($0, "/Volumes/")); exit}}')
        [ -n "$MOUNT_POINT" ] || fail "Не удалось определить точку монтирования dmg."
        NEW_APP=$(find "$MOUNT_POINT" -maxdepth 2 -name "$TARGET_NAME" -type d -print -quit)
        if [ -z "$NEW_APP" ]; then
            NEW_APP=$(find "$MOUNT_POINT" -maxdepth 2 -name "*.app" -type d -print -quit)
        fi
    fi
fi

[ -n "$NEW_APP" ] || fail "В архиве обновления не найден .app."

BACKUP_PATH="$TARGET_PATH.old"
rm -rf "$BACKUP_PATH" || fail "Не удалось удалить старую резервную копию."
if [ -d "$TARGET_PATH" ]; then
    mv "$TARGET_PATH" "$BACKUP_PATH" || fail "Не удалось убрать старое приложение."
fi
ditto "$NEW_APP" "$TARGET_PATH" || {{
    rm -rf "$TARGET_PATH"
    if [ -d "$BACKUP_PATH" ]; then
        mv "$BACKUP_PATH" "$TARGET_PATH"
    fi
    fail "Не удалось скопировать новую версию."
}}
rm -rf "$BACKUP_PATH"

if [ -n "$MOUNT_POINT" ]; then
    hdiutil detach "$MOUNT_POINT" >/dev/null 2>&1 || true
fi
rm -rf "$WORK_DIR"
rm -f "$ARCHIVE_PATH"
log "updated $TARGET_PATH"
open "$TARGET_PATH" || log "failed to reopen $TARGET_PATH"
"#,
        archive = shell_literal(archive_path.to_string_lossy()),
        sha256 = shell_literal(sha256),
        size = size,
        target = shell_literal(target.to_string_lossy()),
        target_name = shell_literal(target_name),
        pid = parent_pid,
        log_path = shell_literal(log_path.to_string_lossy()),
    )
}

fn archive_sha256(path: &Path) -> Result<String, String> {
    let output = Command::new("/usr/bin/shasum")
        .args(["-a", "256"])
        .arg(path)
        .output()
        .map_err(|error| format!("Не удалось проверить SHA-256 обновления: {error}"))?;
    if !output.status.success() {
        return Err("Не удалось проверить SHA-256 обновления.".to_string());
    }
    String::from_utf8(output.stdout)
        .map_err(|error| format!("Некорректный результат проверки SHA-256: {error}"))?
        .split_whitespace()
        .next()
        .map(str::to_string)
        .ok_or_else(|| "Не удалось получить SHA-256 обновления.".to_string())
}

async fn download_macos_update(
    app: &tauri::AppHandle,
    url: &str,
    archive_path: &Path,
    expected_size: u64,
) -> Result<(), String> {
    configure_rustls_provider();
    let result = async {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(20))
            .build()
            .map_err(|error| format!("Не удалось настроить загрузку обновления: {error}"))?;
        let mut response = client
            .get(url)
            .send()
            .await
            .map_err(|error| format!("Не удалось скачать обновление: {error}"))?
            .error_for_status()
            .map_err(|error| format!("Сервер обновлений вернул ошибку: {error}"))?;
        if response
            .content_length()
            .is_some_and(|content_length| content_length > expected_size)
        {
            return Err("Сервер объявил слишком большой размер обновления.".to_string());
        }

        let mut archive = fs::File::create(archive_path)
            .map_err(|error| format!("Не удалось создать архив обновления: {error}"))?;
        let mut downloaded = 0_u64;
        app.emit(
            "macos-update-progress",
            MacosUpdateProgress {
                downloaded_bytes: 0,
                total_bytes: expected_size,
            },
        )
        .map_err(|error| error.to_string())?;
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| format!("Ошибка загрузки обновления: {error}"))?
        {
            downloaded = downloaded.saturating_add(chunk.len() as u64);
            if downloaded > expected_size {
                return Err("Загрузка превышает размер из манифеста.".to_string());
            }
            archive
                .write_all(&chunk)
                .map_err(|error| format!("Не удалось записать архив обновления: {error}"))?;
            app.emit(
                "macos-update-progress",
                MacosUpdateProgress {
                    downloaded_bytes: downloaded,
                    total_bytes: expected_size,
                },
            )
            .map_err(|error| error.to_string())?;
        }
        archive
            .sync_all()
            .map_err(|error| format!("Не удалось сохранить архив обновления: {error}"))?;
        if downloaded != expected_size {
            return Err(format!(
                "Размер обновления не совпадает: ожидалось {expected_size}, получено {downloaded}."
            ));
        }
        Ok(())
    }
    .await;
    if result.is_err() {
        let _ = fs::remove_file(archive_path);
    }
    result
}

#[tauri::command]
async fn install_macos_update(
    app: tauri::AppHandle,
    url: String,
    sha256: String,
    size: u64,
) -> Result<bool, String> {
    if !url.starts_with("https://worta.ru/app/") {
        return Err("Недопустимый адрес обновления macOS.".to_string());
    }
    if sha256.len() != 64 || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) || size == 0 {
        return Err("В манифесте отсутствуют данные проверки архива macOS.".to_string());
    }
    // Tauri's PathResolver::executable_dir is the user's bin directory and is
    // unsupported on macOS; current_exe resolves the running app bundle.
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let Some(target) = macos_update_target(&executable) else {
        return Ok(false);
    };
    let update_dir = app
        .path()
        .app_local_data_dir()
        .map_err(|error| error.to_string())?;
    fs::create_dir_all(&update_dir).map_err(|error| error.to_string())?;
    let script_path = update_dir.join("install-update.sh");
    let log_path = update_dir.join("update.log");
    let archive_path = update_dir.join("update.zip");
    download_macos_update(&app, &url, &archive_path, size).await?;
    let actual_sha256 = match archive_sha256(&archive_path) {
        Ok(value) => value,
        Err(error) => {
            let _ = fs::remove_file(&archive_path);
            return Err(error);
        }
    };
    if !actual_sha256.eq_ignore_ascii_case(&sha256) {
        let _ = fs::remove_file(&archive_path);
        return Err("Контрольная сумма архива обновления не совпадает с манифестом.".to_string());
    }
    let script = build_macos_updater_script(
        &archive_path,
        &target,
        &sha256,
        size,
        &log_path,
        std::process::id(),
    );
    if let Err(error) = fs::write(&script_path, script) {
        let _ = fs::remove_file(&archive_path);
        return Err(error.to_string());
    }
    // Keep the updater alive after plugin-process exits the application, matching
    // the detached process used by the legacy macOS updater.
    if let Err(error) = Command::new("/usr/bin/nohup")
        .arg("/bin/sh")
        .arg(&script_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        let _ = fs::remove_file(&archive_path);
        return Err(error.to_string());
    }
    Ok(true)
}

fn native_updates_enabled() -> bool {
    matches!(
        option_env!("NFPROGRESS_UPDATER_ENABLED"),
        Some("1") | Some("true")
    )
}

fn build_profile() -> &'static str {
    match option_env!("NFPROGRESS_BUILD_PROFILE") {
        Some("test") => "test",
        _ => "production",
    }
}

fn developer_mode_available() -> bool {
    cfg!(debug_assertions)
        || build_profile() == "test"
        || std::env::var("NFPROGRESS_DEVELOPER_MODE").as_deref() == Ok("1")
}

#[tauri::command]
fn request_profile_transfer(
    direction: profile_transfer::TransferDirection,
) -> Result<profile_transfer::TransferRequestResponse, String> {
    profile_transfer::request(&sqlite_data_root()?, direction)
}

#[tauri::command]
fn take_profile_transfer_result() -> Result<Option<serde_json::Value>, String> {
    profile_transfer::take_result(&sqlite_data_root()?)
}

fn prepare_startup_storage(data_root: &Path) -> Result<(), String> {
    let connection = sqlite::open_database(&data_root.join("nfprogress.db"))
        .map_err(|error| error.to_string())?;
    initialize_fresh_desktop_database(&connection, data_root)
}

fn check_startup_storage() -> Result<(), String> {
    let data_root = sqlite_data_root()?;
    eprintln!(
        "nfprogress build_profile={} data_root={}",
        build_profile(),
        data_root.display()
    );
    profile_transfer::process_pending(&data_root)?;
    prepare_startup_storage(&data_root)
}

pub fn run() {
    let mut builder = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(
            |app, _arguments, _cwd| {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            },
        ))
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_process::init())
        .setup(|app| {
            restore_main_window_state(&app.handle());
            // Tauri invokes setup from a non-unwinding macOS callback. Keep
            // storage diagnostics in managed state so migration and schema
            // errors reach the frontend startup gate instead of becoming an
            // abort in tao's Objective-C callback.
            let startup_error = check_startup_storage().err();
            if let Some(error) = startup_error.as_deref() {
                eprintln!("nfprogress startup storage check failed: {error}");
            }
            app.manage(StartupStatus {
                error: startup_error,
            });
            Ok(())
        });

    if native_updates_enabled() {
        builder = builder.plugin(tauri_plugin_updater::Builder::new().build());
    }

    let app = builder
        .invoke_handler(tauri::generate_handler![
            runtime_info,
            request_profile_transfer,
            take_profile_transfer_result,
            process_game_events,
            list_documents,
            get_document,
            save_document,
            record_document_progress,
            rename_document,
            delete_document,
            bind_document_file,
            resolve_external_binding,
            poll_external_binding,
            write_document_word,
            read_document_external,
            accept_document_external,
            count_word_document,
            parse_word_document,
            import_word_document,
            export_word_document,
            write_document_word_content,
            inspect_scrivener,
            configure_document_sync,
            get_document_sync,
            get_project_document_syncs,
            remove_document_sync,
            run_document_sync,
            confirm_external_progress,
            refresh_external_progress,
            run_all_document_sync,
            run_project_document_syncs,
            game_state,
            game_notifications,
            mark_game_notification_read,
            mark_all_game_notifications_read,
            append_diagnostics,
            diagnostic_stats,
            diagnostic_text,
            clear_diagnostics,
            export_diagnostics,
            game_catalog,
            game_developer_state,
            game_update_developer_profile,
            game_grant_developer_inventory_item,
            game_developer_streak_state,
            game_developer_restore_streak,
            game_developer_create_streak_series,
            game_start_writing_session,
            game_finish_writing_session,
            game_cancel_writing_session,
            game_select_daily_challenge,
            game_start_weekly_challenge,
            game_activate_inspiration_ability,
            game_resolve_creative_event,
            game_select_specialization,
            game_activate_specialization_ability,
            game_increase_skill,
            game_start_quest,
            game_abandon_quest,
            game_buy_item,
            game_sell_item,
            game_use_item,
            game_apply_streak_freeze,
            game_run_lottery,
            game_create_custom_award,
            game_update_custom_award,
            game_delete_custom_award,
            game_buy_custom_award,
            game_sell_custom_award,
            game_use_custom_award,
            game_preview_bank_product,
            game_open_bank_credit,
            game_open_bank_deposit,
            game_process_bank_events,
            game_make_bank_loan_payment,
            game_partially_repay_bank_credit,
            game_repay_bank_credit,
            game_top_up_bank_deposit,
            game_withdraw_bank_deposit,
            game_withdraw_bank_interest,
            read_sqlite_projects,
            projects_storage_owner,
            create_project,
            update_project,
            delete_project,
            set_project_archived,
            complete_project,
            complete_stage,
            create_stage,
            update_stage,
            delete_stage,
            reorder_stages,
            list_project_folders,
            create_project_folder,
            update_project_folder,
            delete_project_folder,
            update_project_metadata,
            reorder_projects,
            add_project_progress,
            add_stage_progress,
            delete_progress,
            get_settings,
            get_current_writing_day,
            set_settings,
            list_notes,
            get_note,
            list_unsealed_note_sync_intents,
            preflight_cloud_project_bootstrap,
            prepare_cloud_project_bootstrap,
            confirm_cloud_project_registration,
            capture_initial_note_sync_intents,
            read_initial_note_cohort_status,
            mark_cloud_project_bootstrap_completing,
            mark_cloud_project_bootstrap_ready,
            set_cloud_project_bootstrap_paused,
            list_cloud_project_bootstraps,
            import_remote_cloud_project,
            list_sealed_note_sync_outbox,
            record_note_sync_seal_failure,
            commit_sealed_note_sync_event,
            list_unsealed_note_resolution_intents,
            commit_sealed_note_resolution_event,
            read_note_resolution_readiness,
            list_sealed_note_resolution_uploads,
            commit_note_resolution_upload_acceptance,
            commit_note_sync_upload_acceptance,
            record_note_sync_upload_failure,
            ensure_cloud_account_binding,
            provision_cloud_identity,
            read_cloud_identity,
            read_note_sync_pull_state,
            prepare_note_sync_ack,
            commit_note_sync_ack,
            list_received_note_sync_inbox,
            list_received_note_sync_inbox_page,
            list_orphan_note_resolution_inbox,
            apply_verified_received_note,
            apply_verified_received_content_note,
            record_content_note_blocker,
            apply_verified_received_resolution_v2,
            reconcile_verified_received_resolution_self_echo,
            commit_note_sync_inbound_page,
            commit_v3_sync_inbound_page,
            game_sync_command,
            progress_sync_command,
            document_sync_command,
            map_sync_command,
            read_account_catalog,
            begin_account_catalog,
            decide_account_catalog,
            pending_account_catalog,
            seal_account_catalog,
            receipt_account_catalog,
            apply_account_catalog,
            reorder_project_folders,
            list_received_account_objects,
            block_account_object,
            read_stage_structural_authority,
            begin_stage_structural_migration,
            begin_content_note_migration,
            read_content_note_conflicts,
            choose_content_note_version,
            read_content_note_migration,
            list_pending_content_notes,
            seal_content_note,
            receipt_content_note,
            prepare_stage_structural_decision,
            list_pending_stage_structural,
            commit_sealed_stage_structural,
            commit_stage_structural_receipt,
            list_received_stage_structural,
            apply_authenticated_stage_structural,
            record_stage_structural_blocker,
            retry_stage_structural,
            capture_project_metadata_candidate,
            read_project_metadata_migration_status,
            prepare_project_metadata_genesis,
            list_unsealed_project_metadata_genesis,
            commit_sealed_project_metadata_genesis,
            list_sealed_project_metadata_genesis,
            commit_project_metadata_upload_receipt,
            list_received_project_metadata,
            record_project_metadata_compression_blocker,
            project_cover_command,
            apply_authenticated_project_metadata,
            read_project_metadata_authority,
            read_project_metadata_import,
            commit_project_metadata_import_page,
            adopt_authenticated_project_metadata,
            prepare_project_metadata_change,
            create_note,
            update_note,
            delete_note,
            reorder_notes,
            load_map,
            save_map,
            import_xmind,
            fetch_update_manifest,
            install_macos_update
        ])
        .build(tauri::generate_context!())
        .expect("failed to build the nfprogress desktop application");

    app.run(|app_handle, event| {
        if matches!(event, RunEvent::Exit | RunEvent::ExitRequested { .. }) {
            save_main_window_state(app_handle);
        }
    });
}
