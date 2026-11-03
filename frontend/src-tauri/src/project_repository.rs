//! Typed repository for the Projects aggregate.
//!
//! This module is deliberately not registered as a Tauri command in F1.  It
//! is an internal, owner-guarded storage capability for migration and the F2
//! cutover.  Business validation/orchestration is kept in the Tauri service
//! layer; these methods only persist validated records.

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::note_sync::{self, NoteDeleteScope, NoteSyncError};
use crate::sqlite::StorageError;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProjectRecord {
    pub id: String,
    pub name: String,
    pub goal: Option<f64>,
    pub infinite: bool,
    pub unit: String,
    pub status: String,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StageRecord {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub goal: Option<f64>,
    pub infinite: bool,
    pub unit: String,
    pub status: String,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProgressRecord {
    pub id: String,
    pub project_id: String,
    pub stage_id: Option<String>,
    pub created_at: Option<String>,
    pub added_symbols: Option<f64>,
    pub added_progress: Option<f64>,
    pub payload: Value,
}

#[derive(Debug, Clone)]
pub struct ProjectAggregate {
    pub project: ProjectRecord,
    pub stages: Vec<StageRecord>,
    pub progress: Vec<ProgressRecord>,
    pub order_position: i64,
}

#[derive(Debug, Clone, Default)]
pub struct ProjectMetadataUpdate {
    pub name: Option<String>,
    pub goal: Option<f64>,
    pub unit: Option<String>,
    pub status: Option<String>,
    pub infinite: Option<bool>,
    pub deadline: Option<Option<String>>,
}

#[derive(Debug, Clone)]
pub struct StageUpdate {
    pub name: String,
    pub goal: Option<f64>,
    pub infinite: bool,
    pub unit: String,
    pub status: String,
    pub payload: Value,
}

#[derive(Debug)]
pub enum RepositoryError {
    NotFound { entity: &'static str, id: String },
    Constraint(String),
    InvalidRelation(String),
    MigrationConflict(String),
    CorruptPayload(String),
    UnsupportedSchema(i64),
    NoteSync(&'static str),
    Database(rusqlite::Error),
}

impl std::fmt::Display for RepositoryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound { entity, id } => write!(formatter, "{entity} not found: {id}"),
            Self::Constraint(message) => write!(formatter, "constraint violation: {message}"),
            Self::InvalidRelation(message) => write!(formatter, "invalid relation: {message}"),
            Self::MigrationConflict(message) => write!(formatter, "migration conflict: {message}"),
            Self::CorruptPayload(message) => write!(formatter, "corrupt payload: {message}"),
            Self::UnsupportedSchema(version) => write!(formatter, "unsupported schema: {version}"),
            Self::NoteSync(message) => write!(formatter, "note sync error: {message}"),
            Self::Database(error) => write!(formatter, "SQLite error: {error}"),
        }
    }
}

impl std::error::Error for RepositoryError {}

impl From<rusqlite::Error> for RepositoryError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Database(error)
    }
}

impl From<StorageError> for RepositoryError {
    fn from(error: StorageError) -> Self {
        match error {
            StorageError::UnsupportedSchema(version) => Self::UnsupportedSchema(version),
            StorageError::CorruptSchema(message) => Self::MigrationConflict(message),
            StorageError::RemoteApplyAuthorization(message) => Self::MigrationConflict(message),
            StorageError::Database(error) => Self::Database(error),
        }
    }
}

impl From<NoteSyncError> for RepositoryError {
    fn from(error: NoteSyncError) -> Self {
        Self::NoteSync(error.user_message())
    }
}

pub struct ProjectsRepository<'connection> {
    connection: &'connection mut Connection,
}

/// Singular alias used by the future Tauri service layer.
pub type ProjectRepository<'connection> = ProjectsRepository<'connection>;

impl<'connection> ProjectsRepository<'connection> {
    pub fn new(connection: &'connection mut Connection) -> Self {
        Self { connection }
    }

    pub fn get_project(&self, id: &str) -> Result<Option<ProjectRecord>, RepositoryError> {
        let row = self
            .connection
            .query_row(
                "SELECT id, name, goal, infinite, unit, status, created_at, updated_at, payload_json FROM projects WHERE id = ?1",
                [id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get(2)?,
                        row.get::<_, i64>(3)? != 0, row.get(4)?, row.get(5)?,
                        row.get(6)?, row.get(7)?, row.get::<_, String>(8)?,
                    ))
                },
            )
            .optional()?;
        row.map(|row| self.project_from_row(row)).transpose()
    }

    pub fn list_projects(&self) -> Result<Vec<ProjectRecord>, RepositoryError> {
        let mut statement = self.connection.prepare(
            "SELECT p.id, p.name, p.goal, p.infinite, p.unit, p.status, p.created_at, p.updated_at, p.payload_json FROM projects p JOIN project_order o ON o.project_id = p.id ORDER BY o.position, p.id",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get(2)?,
                    row.get::<_, i64>(3)? != 0,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get::<_, String>(8)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|row| self.project_from_row(row))
            .collect()
    }

    pub fn get_stage(&self, id: &str) -> Result<Option<StageRecord>, RepositoryError> {
        let row = self
            .connection
            .query_row(
                "SELECT id, project_id, name, goal, infinite, unit, status, created_at, updated_at, payload_json FROM stages WHERE id = ?1",
                [id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get(2)?,
                        row.get(3)?, row.get::<_, i64>(4)? != 0, row.get(5)?, row.get(6)?,
                        row.get(7)?, row.get(8)?, row.get::<_, String>(9)?,
                    ))
                },
            )
            .optional()?;
        row.map(|row| self.stage_from_row(row)).transpose()
    }

    pub fn list_stages(&self, project_id: &str) -> Result<Vec<StageRecord>, RepositoryError> {
        let mut statement = self.connection.prepare(
            "SELECT s.id, s.project_id, s.name, s.goal, s.infinite, s.unit, s.status, s.created_at, s.updated_at, s.payload_json FROM stages s JOIN stage_order o ON o.stage_id = s.id WHERE s.project_id = ?1 ORDER BY o.position, s.id",
        )?;
        let rows = statement
            .query_map([project_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get::<_, i64>(4)? != 0,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get::<_, String>(9)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|row| self.stage_from_row(row))
            .collect()
    }

    pub fn list_progress(
        &self,
        project_id: &str,
        stage_id: Option<&str>,
    ) -> Result<Vec<ProgressRecord>, RepositoryError> {
        let mut statement = self.connection.prepare(
            "SELECT p.id, p.project_id, p.stage_id, p.created_at, p.added_symbols, p.added_progress, p.payload_json FROM progress_entries p JOIN progress_order o ON o.entry_id = p.id WHERE p.project_id = ?1 AND (?2 IS NULL OR p.stage_id = ?2) ORDER BY o.position, p.id",
        )?;
        let rows = statement
            .query_map(params![project_id, stage_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get::<_, String>(6)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|row| self.progress_from_row(row))
            .collect()
    }

    pub fn get_project_order(&self) -> Result<Vec<String>, RepositoryError> {
        let mut statement = self
            .connection
            .prepare("SELECT project_id FROM project_order ORDER BY position, project_id")?;
        let result = statement
            .query_map([], |row| row.get(0))?
            .collect::<Result<Vec<String>, _>>()?;
        Ok(result)
    }

    pub fn insert_project(&mut self, project: &ProjectRecord) -> Result<(), RepositoryError> {
        validate_payload(&project.payload)?;
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "INSERT INTO projects(id, name, goal, infinite, unit, status, created_at, updated_at, payload_json) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![project.id, project.name, project.goal, project.infinite, project.unit, project.status, project.created_at, project.updated_at, project.payload.to_string()],
        ).map_err(map_constraint)?;
        append_project_order(&transaction, &project.id)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn insert_stage(&mut self, stage: &StageRecord) -> Result<(), RepositoryError> {
        validate_payload(&stage.payload)?;
        ensure_project(&self.connection, &stage.project_id)?;
        self.connection.execute(
            "INSERT INTO stages(id, project_id, name, goal, infinite, unit, status, created_at, updated_at, payload_json) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![stage.id, stage.project_id, stage.name, stage.goal, stage.infinite, stage.unit, stage.status, stage.created_at, stage.updated_at, stage.payload.to_string()],
        ).map_err(map_constraint)?;
        let position: i64 = self.connection.query_row(
            "SELECT COALESCE(MAX(position), -1) + 1 FROM stage_order WHERE project_id=?1",
            [&stage.project_id],
            |row| row.get(0),
        )?;
        self.connection
            .execute(
                "INSERT INTO stage_order(stage_id, project_id, position) VALUES(?1, ?2, ?3)",
                params![stage.id, stage.project_id, position],
            )
            .map_err(map_constraint)?;
        Ok(())
    }

    pub fn insert_progress(&mut self, progress: &ProgressRecord) -> Result<(), RepositoryError> {
        validate_payload(&progress.payload)?;
        ensure_project(&self.connection, &progress.project_id)?;
        if let Some(stage_id) = &progress.stage_id {
            ensure_stage(&self.connection, stage_id, &progress.project_id)?;
        }
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "INSERT INTO progress_entries(id, project_id, stage_id, created_at, added_symbols, added_progress, payload_json) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![progress.id, progress.project_id, progress.stage_id, progress.created_at, progress.added_symbols, progress.added_progress, progress.payload.to_string()],
        ).map_err(map_constraint)?;
        let position: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(position), -1) + 1 FROM progress_order",
            [],
            |row| row.get(0),
        )?;
        transaction
            .execute(
                "INSERT INTO progress_order(entry_id, position) VALUES(?1, ?2)",
                params![progress.id, position],
            )
            .map_err(map_constraint)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn update_project_metadata(
        &mut self,
        project_id: &str,
        update: &ProjectMetadataUpdate,
    ) -> Result<ProjectRecord, RepositoryError> {
        let current = self
            .get_project(project_id)?
            .ok_or_else(|| RepositoryError::NotFound {
                entity: "project",
                id: project_id.to_string(),
            })?;
        let name = update.name.as_deref().unwrap_or(&current.name);
        let infinite = update.infinite.unwrap_or(current.infinite);
        let goal = if infinite {
            None
        } else {
            update.goal.or(current.goal)
        };
        let unit = update.unit.as_deref().unwrap_or(&current.unit);
        let status = update.status.as_deref().unwrap_or(&current.status);
        let mut payload = current.payload.clone();
        let object = payload.as_object_mut().ok_or_else(|| {
            RepositoryError::CorruptPayload("project payload is not an object".to_string())
        })?;
        object.insert("name".to_string(), Value::String(name.to_string()));
        object.insert("goal".to_string(), goal.map_or(Value::Null, Value::from));
        object.insert("unit".to_string(), Value::String(unit.to_string()));
        object.insert("status".to_string(), Value::String(status.to_string()));
        object.insert("infinite".to_string(), Value::Bool(infinite));
        object.insert("goal".to_string(), goal.map_or(Value::Null, Value::from));
        if let Some(deadline) = &update.deadline {
            object.insert(
                "deadline".to_string(),
                deadline
                    .as_ref()
                    .map_or(Value::Null, |value| Value::String(value.clone())),
            );
        }
        validate_payload(&payload)?;
        let transaction = self.connection.transaction()?;
        let changed = transaction.execute(
            "UPDATE projects SET name=?1, goal=?2, infinite=?3, unit=?4, status=?5, updated_at=strftime('%Y-%m-%dT%H:%M:%SZ','now'), payload_json=?6 WHERE id=?7",
            params![name, goal, infinite, unit, status, payload.to_string(), project_id],
        )?;
        if changed != 1 {
            return Err(RepositoryError::NotFound {
                entity: "project",
                id: project_id.to_string(),
            });
        }
        transaction.commit()?;
        self.get_project(project_id)?
            .ok_or_else(|| RepositoryError::NotFound {
                entity: "project",
                id: project_id.to_string(),
            })
    }

    pub fn update_project_payload(
        &mut self,
        project_id: &str,
        payload: &Value,
    ) -> Result<ProjectRecord, RepositoryError> {
        validate_payload(payload)?;
        let current = self
            .get_project(project_id)?
            .ok_or_else(|| RepositoryError::NotFound {
                entity: "project",
                id: project_id.to_string(),
            })?;
        let object = payload.as_object().ok_or_else(|| {
            RepositoryError::CorruptPayload("project payload is not an object".to_string())
        })?;
        let name = object
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(&current.name);
        let goal = object.get("goal").and_then(Value::as_f64);
        let infinite = object
            .get("infinite")
            .and_then(Value::as_bool)
            .unwrap_or(current.infinite);
        let unit = object
            .get("unit")
            .and_then(Value::as_str)
            .unwrap_or(&current.unit);
        let status = object
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or(&current.status);
        let changed = self.connection.execute(
            "UPDATE projects SET name=?1, goal=?2, infinite=?3, unit=?4, status=?5, updated_at=strftime('%Y-%m-%dT%H:%M:%SZ','now'), payload_json=?6 WHERE id=?7",
            params![name, if infinite { None } else { goal }, infinite, unit, status, payload.to_string(), project_id],
        )?;
        if changed != 1 {
            return Err(RepositoryError::NotFound {
                entity: "project",
                id: project_id.to_string(),
            });
        }
        self.get_project(project_id)?
            .ok_or_else(|| RepositoryError::NotFound {
                entity: "project",
                id: project_id.to_string(),
            })
    }

    pub fn update_stage(
        &mut self,
        stage_id: &str,
        update: &StageUpdate,
    ) -> Result<StageRecord, RepositoryError> {
        validate_payload(&update.payload)?;
        let transaction = self.connection.transaction()?;
        let changed = transaction.execute(
            "UPDATE stages SET name=?1, goal=?2, infinite=?3, unit=?4, status=?5, updated_at=strftime('%Y-%m-%dT%H:%M:%SZ','now'), payload_json=?6 WHERE id=?7",
            params![update.name, update.goal, update.infinite, update.unit, update.status, update.payload.to_string(), stage_id],
        )?;
        if changed != 1 {
            return Err(RepositoryError::NotFound {
                entity: "stage",
                id: stage_id.to_string(),
            });
        }
        transaction.commit()?;
        self.get_stage(stage_id)?
            .ok_or_else(|| RepositoryError::NotFound {
                entity: "stage",
                id: stage_id.to_string(),
            })
    }

    pub fn append_domain_event(
        &mut self,
        event_id: &str,
        event_type: &str,
        project_id: &str,
        stage_id: Option<&str>,
        progress_id: Option<&str>,
        effective_date: Option<&str>,
        delta_symbols: Option<f64>,
        context: &Value,
    ) -> Result<(), RepositoryError> {
        validate_payload(context)?;
        self.connection.execute(
            "INSERT OR IGNORE INTO domain_events(event_id,event_type,project_id,stage_id,progress_id,effective_date,delta_symbols,context_json,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,strftime('%Y-%m-%dT%H:%M:%SZ','now'))",
            params![event_id, event_type, project_id, stage_id, progress_id, effective_date, delta_symbols, context.to_string()],
        )?;
        Ok(())
    }

    pub fn project_folder_exists(&self, folder_id: &str) -> Result<bool, RepositoryError> {
        Ok(self
            .connection
            .query_row(
                "SELECT 1 FROM project_folders WHERE id=?1",
                [folder_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    pub fn has_sync_binding(
        &self,
        project_id: &str,
        stage_id: Option<&str>,
    ) -> Result<bool, RepositoryError> {
        Ok(self
            .connection
            .query_row(
                "SELECT 1 FROM project_bindings WHERE project_id=?1 AND (?2 IS NULL OR stage_id=?2) LIMIT 1",
                params![project_id, stage_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    pub fn set_project_folder(
        &mut self,
        project_id: &str,
        folder_id: Option<&str>,
    ) -> Result<(), RepositoryError> {
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "DELETE FROM project_folder_members WHERE project_id=?1",
            [project_id],
        )?;
        if let Some(folder_id) = folder_id {
            transaction.execute(
                "INSERT INTO project_folder_members(project_id,folder_id) VALUES(?1,?2)",
                params![project_id, folder_id],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn delete_progress(&mut self, entry_id: &str) -> Result<(), RepositoryError> {
        let transaction = self.connection.transaction()?;
        let deleted =
            transaction.execute("DELETE FROM progress_entries WHERE id=?1", [entry_id])?;
        if deleted != 1 {
            return Err(RepositoryError::NotFound {
                entity: "progress",
                id: entry_id.to_string(),
            });
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn delete_stage_storage(&mut self, stage_id: &str) -> Result<(), RepositoryError> {
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "DELETE FROM document_bindings WHERE document_id IN (SELECT id FROM documents WHERE stage_id=?1)",
            [stage_id],
        )?;
        transaction.execute("DELETE FROM documents WHERE stage_id=?1", [stage_id])?;
        transaction.execute("DELETE FROM stage_order WHERE stage_id=?1", [stage_id])?;
        transaction.execute("DELETE FROM project_bindings WHERE stage_id=?1", [stage_id])?;
        transaction.execute(
            "DELETE FROM project_extensions WHERE entity_type='stage' AND entity_id=?1",
            [stage_id],
        )?;
        let deleted = transaction
            .execute("DELETE FROM stages WHERE id=?1", [stage_id])
            .map_err(map_constraint)?;
        if deleted != 1 {
            return Err(RepositoryError::NotFound {
                entity: "stage",
                id: stage_id.to_string(),
            });
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn delete_stage_with_notes(
        &mut self,
        stage_id: &str,
        project_id: &str,
    ) -> Result<(), RepositoryError> {
        let transaction = self.connection.transaction()?;
        let deleted_at: String =
            transaction.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')", [], |row| {
                row.get(0)
            })?;
        note_sync::prepare_note_delete_intents_for_scope(
            &transaction,
            project_id,
            NoteDeleteScope::Stage(stage_id),
            &deleted_at,
        )?;
        transaction.execute(
            "DELETE FROM notes WHERE project_id=?1 AND stage_id=?2",
            params![project_id, stage_id],
        )?;
        transaction.execute(
            "DELETE FROM document_bindings WHERE document_id IN (SELECT id FROM documents WHERE stage_id=?1)",
            [stage_id],
        )?;
        transaction.execute("DELETE FROM documents WHERE stage_id=?1", [stage_id])?;
        transaction.execute("DELETE FROM stage_order WHERE stage_id=?1", [stage_id])?;
        transaction.execute("DELETE FROM project_bindings WHERE stage_id=?1", [stage_id])?;
        transaction.execute(
            "DELETE FROM project_extensions WHERE entity_type='stage' AND entity_id=?1",
            [stage_id],
        )?;
        let deleted = transaction
            .execute(
                "DELETE FROM stages WHERE id=?1 AND project_id=?2",
                params![stage_id, project_id],
            )
            .map_err(map_constraint)?;
        if deleted != 1 {
            return Err(RepositoryError::NotFound {
                entity: "stage",
                id: stage_id.to_string(),
            });
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn delete_project_storage(&mut self, project_id: &str) -> Result<(), RepositoryError> {
        let transaction = self.connection.transaction()?;
        if transaction
            .query_row("SELECT 1 FROM projects WHERE id=?1", [project_id], |_| {
                Ok(())
            })
            .optional()?
            .is_none()
        {
            return Err(RepositoryError::NotFound {
                entity: "project",
                id: project_id.to_string(),
            });
        }
        transaction.execute(
            "DELETE FROM project_order WHERE project_id=?1",
            [project_id],
        )?;
        transaction.execute(
            "DELETE FROM document_bindings WHERE document_id IN (SELECT id FROM documents WHERE project_id=?1)",
            [project_id],
        )?;
        transaction.execute("DELETE FROM documents WHERE project_id=?1", [project_id])?;
        transaction.execute(
            "DELETE FROM project_bindings WHERE project_id=?1",
            [project_id],
        )?;
        transaction.execute(
            "DELETE FROM project_extensions WHERE entity_type='project' AND entity_id=?1",
            [project_id],
        )?;
        transaction.execute("DELETE FROM project_extensions WHERE entity_type='stage' AND entity_id IN (SELECT id FROM stages WHERE project_id=?1)", [project_id])?;
        transaction.execute(
            "DELETE FROM project_folder_members WHERE project_id=?1",
            [project_id],
        )?;
        // Notes use ON DELETE RESTRICT.  This statement therefore fails and
        // rolls back before any project data can be committed when the
        // cross-domain lifecycle has not explicitly handled Notes.
        transaction
            .execute("DELETE FROM projects WHERE id=?1", [project_id])
            .map_err(map_constraint)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn delete_project(&mut self, project_id: &str) -> Result<(), RepositoryError> {
        self.delete_project_storage(project_id)
    }

    pub fn delete_project_with_notes(&mut self, project_id: &str) -> Result<(), RepositoryError> {
        let transaction = self.connection.transaction()?;
        let exists = transaction
            .query_row("SELECT 1 FROM projects WHERE id=?1", [project_id], |_| {
                Ok(())
            })
            .optional()?
            .is_some();
        if !exists {
            return Err(RepositoryError::NotFound {
                entity: "project",
                id: project_id.to_string(),
            });
        }
        if transaction
            .query_row(
                "SELECT 1 FROM cloud_sync_project_bindings WHERE project_id=?1",
                [project_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some()
        {
            return Err(RepositoryError::InvalidRelation(
                "cloud-bound project must be paused and detached by an explicit cloud workflow"
                    .to_string(),
            ));
        }
        let deleted_at: String =
            transaction.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')", [], |row| {
                row.get(0)
            })?;
        note_sync::prepare_note_delete_intents_for_scope(
            &transaction,
            project_id,
            NoteDeleteScope::Project,
            &deleted_at,
        )?;
        transaction.execute("DELETE FROM notes WHERE project_id=?1", [project_id])?;
        transaction.execute(
            "INSERT OR IGNORE INTO domain_events(event_id,event_type,project_id,context_json,created_at) VALUES(?1,'ProjectDeleted',?2,?3,strftime('%Y-%m-%dT%H:%M:%SZ','now'))",
            params![format!("project-deleted:{project_id}"), project_id, serde_json::json!({"source":"desktop","version":1}).to_string()],
        )?;
        transaction.execute(
            "DELETE FROM project_order WHERE project_id=?1",
            [project_id],
        )?;
        transaction.execute(
            "DELETE FROM document_bindings WHERE document_id IN (SELECT id FROM documents WHERE project_id=?1)",
            [project_id],
        )?;
        transaction.execute("DELETE FROM documents WHERE project_id=?1", [project_id])?;
        transaction.execute(
            "DELETE FROM project_bindings WHERE project_id=?1",
            [project_id],
        )?;
        transaction.execute(
            "DELETE FROM project_extensions WHERE entity_type='project' AND entity_id=?1",
            [project_id],
        )?;
        transaction.execute("DELETE FROM project_extensions WHERE entity_type='stage' AND entity_id IN (SELECT id FROM stages WHERE project_id=?1)", [project_id])?;
        transaction.execute(
            "DELETE FROM project_folder_members WHERE project_id=?1",
            [project_id],
        )?;
        // Keep the binding present while Note guard triggers validate all
        // tombstones, then remove only the project association. Durable sync
        // events and their typed intents intentionally remain.
        transaction.execute(
            "DELETE FROM cloud_sync_project_bindings WHERE project_id=?1",
            [project_id],
        )?;
        transaction
            .execute("DELETE FROM projects WHERE id=?1", [project_id])
            .map_err(map_constraint)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn update_project_order(&mut self, project_ids: &[String]) -> Result<(), RepositoryError> {
        let existing = self.get_project_order()?;
        let expected: std::collections::HashSet<&str> =
            existing.iter().map(String::as_str).collect();
        let supplied: std::collections::HashSet<&str> =
            project_ids.iter().map(String::as_str).collect();
        if expected.len() != project_ids.len() || expected != supplied {
            return Err(RepositoryError::InvalidRelation(
                "project order must contain every project exactly once".to_string(),
            ));
        }
        let transaction = self.connection.transaction()?;
        transaction.execute("DELETE FROM project_order", [])?;
        for (position, project_id) in project_ids.iter().enumerate() {
            transaction.execute(
                "INSERT INTO project_order(project_id, position) VALUES(?1, ?2)",
                params![project_id, position as i64],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn update_stage_order(
        &mut self,
        project_id: &str,
        stage_ids: &[String],
    ) -> Result<(), RepositoryError> {
        let existing = self
            .list_stages(project_id)?
            .into_iter()
            .map(|stage| stage.id)
            .collect::<Vec<_>>();
        let expected: std::collections::HashSet<&str> =
            existing.iter().map(String::as_str).collect();
        let supplied: std::collections::HashSet<&str> =
            stage_ids.iter().map(String::as_str).collect();
        if expected.len() != stage_ids.len() || expected != supplied {
            return Err(RepositoryError::InvalidRelation(
                "stage order must contain every stage exactly once".to_string(),
            ));
        }
        let transaction = self.connection.transaction()?;
        for (position, stage_id) in stage_ids.iter().enumerate() {
            transaction.execute(
                "UPDATE stage_order SET position=?1 WHERE stage_id=?2 AND project_id=?3",
                params![position as i64, stage_id, project_id],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn insert_aggregate(
        &mut self,
        aggregate: &ProjectAggregate,
    ) -> Result<(), RepositoryError> {
        let transaction = self.connection.transaction()?;
        validate_payload(&aggregate.project.payload)?;
        transaction.execute(
            "INSERT INTO projects(id, name, goal, infinite, unit, status, created_at, updated_at, payload_json) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![aggregate.project.id, aggregate.project.name, aggregate.project.goal, aggregate.project.infinite, aggregate.project.unit, aggregate.project.status, aggregate.project.created_at, aggregate.project.updated_at, aggregate.project.payload.to_string()],
        ).map_err(map_constraint)?;
        transaction
            .execute(
                "INSERT INTO project_order(project_id, position) VALUES(?1, ?2)",
                params![aggregate.project.id, aggregate.order_position],
            )
            .map_err(map_constraint)?;
        for stage in &aggregate.stages {
            validate_payload(&stage.payload)?;
            if stage.project_id != aggregate.project.id {
                return Err(RepositoryError::InvalidRelation(
                    "stage belongs to another project".to_string(),
                ));
            }
            transaction.execute(
                "INSERT INTO stages(id, project_id, name, goal, infinite, unit, status, created_at, updated_at, payload_json) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![stage.id, stage.project_id, stage.name, stage.goal, stage.infinite, stage.unit, stage.status, stage.created_at, stage.updated_at, stage.payload.to_string()],
            ).map_err(map_constraint)?;
            let stage_position: i64 = transaction.query_row(
                "SELECT COALESCE(MAX(position), -1) + 1 FROM stage_order WHERE project_id=?1",
                [&aggregate.project.id],
                |row| row.get(0),
            )?;
            transaction
                .execute(
                    "INSERT INTO stage_order(stage_id, project_id, position) VALUES(?1, ?2, ?3)",
                    params![stage.id, stage.project_id, stage_position],
                )
                .map_err(map_constraint)?;
        }
        for progress in &aggregate.progress {
            if progress.project_id != aggregate.project.id {
                return Err(RepositoryError::InvalidRelation(
                    "progress belongs to another project".to_string(),
                ));
            }
            validate_payload(&progress.payload)?;
            if let Some(stage_id) = &progress.stage_id {
                ensure_stage(&transaction, stage_id, &progress.project_id)?;
            }
            let position: i64 = transaction.query_row(
                "SELECT COALESCE(MAX(position), -1) + 1 FROM progress_order",
                [],
                |row| row.get(0),
            )?;
            transaction.execute("INSERT INTO progress_entries(id, project_id, stage_id, created_at, added_symbols, added_progress, payload_json) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)", params![progress.id, progress.project_id, progress.stage_id, progress.created_at, progress.added_symbols, progress.added_progress, progress.payload.to_string()]).map_err(map_constraint)?;
            transaction
                .execute(
                    "INSERT INTO progress_order(entry_id, position) VALUES(?1, ?2)",
                    params![progress.id, position],
                )
                .map_err(map_constraint)?;
        }
        transaction.commit()?;
        Ok(())
    }

    fn project_from_row(
        &self,
        row: (
            String,
            String,
            Option<f64>,
            bool,
            String,
            String,
            Option<String>,
            Option<String>,
            String,
        ),
    ) -> Result<ProjectRecord, RepositoryError> {
        Ok(ProjectRecord {
            id: row.0,
            name: row.1,
            goal: row.2,
            infinite: row.3,
            unit: row.4,
            status: row.5,
            created_at: row.6,
            updated_at: row.7,
            payload: parse_payload(row.8)?,
        })
    }

    fn stage_from_row(
        &self,
        row: (
            String,
            String,
            String,
            Option<f64>,
            bool,
            String,
            String,
            Option<String>,
            Option<String>,
            String,
        ),
    ) -> Result<StageRecord, RepositoryError> {
        Ok(StageRecord {
            id: row.0,
            project_id: row.1,
            name: row.2,
            goal: row.3,
            infinite: row.4,
            unit: row.5,
            status: row.6,
            created_at: row.7,
            updated_at: row.8,
            payload: parse_payload(row.9)?,
        })
    }

    fn progress_from_row(
        &self,
        row: (
            String,
            String,
            Option<String>,
            Option<String>,
            Option<f64>,
            Option<f64>,
            String,
        ),
    ) -> Result<ProgressRecord, RepositoryError> {
        Ok(ProgressRecord {
            id: row.0,
            project_id: row.1,
            stage_id: row.2,
            created_at: row.3,
            added_symbols: row.4,
            added_progress: row.5,
            payload: parse_payload(row.6)?,
        })
    }
}

fn parse_payload(value: String) -> Result<Value, RepositoryError> {
    serde_json::from_str(&value).map_err(|error| RepositoryError::CorruptPayload(error.to_string()))
}

fn validate_payload(value: &Value) -> Result<(), RepositoryError> {
    serde_json::to_string(value)
        .map(|_| ())
        .map_err(|error| RepositoryError::CorruptPayload(error.to_string()))
}

fn ensure_project(connection: &Connection, id: &str) -> Result<(), RepositoryError> {
    if connection
        .query_row("SELECT 1 FROM projects WHERE id=?1", [id], |_| Ok(()))
        .optional()?
        .is_none()
    {
        return Err(RepositoryError::InvalidRelation(format!(
            "project does not exist: {id}"
        )));
    }
    Ok(())
}

fn ensure_stage(
    connection: &Connection,
    stage_id: &str,
    project_id: &str,
) -> Result<(), RepositoryError> {
    let parent: Option<String> = connection
        .query_row(
            "SELECT project_id FROM stages WHERE id=?1",
            [stage_id],
            |row| row.get(0),
        )
        .optional()?;
    match parent {
        Some(parent) if parent == project_id => Ok(()),
        Some(_) => Err(RepositoryError::InvalidRelation(format!(
            "stage does not belong to project: {stage_id}"
        ))),
        None => Err(RepositoryError::InvalidRelation(format!(
            "stage does not exist: {stage_id}"
        ))),
    }
}

fn append_project_order(connection: &Connection, project_id: &str) -> Result<(), RepositoryError> {
    let position: i64 = connection.query_row(
        "SELECT COALESCE(MAX(position), -1) + 1 FROM project_order",
        [],
        |row| row.get(0),
    )?;
    connection
        .execute(
            "INSERT INTO project_order(project_id, position) VALUES(?1, ?2)",
            params![project_id, position],
        )
        .map_err(map_constraint)?;
    Ok(())
}

fn map_constraint(error: rusqlite::Error) -> RepositoryError {
    RepositoryError::Constraint(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqlite::apply_migrations;

    const TEST_DEVICE_ID: &str = "123e4567-e89b-42d3-a456-426614174000";

    fn project(id: &str) -> ProjectRecord {
        ProjectRecord {
            id: id.to_string(),
            name: id.to_string(),
            goal: Some(100.0),
            infinite: false,
            unit: "symbols".to_string(),
            status: "активен".to_string(),
            created_at: None,
            updated_at: None,
            payload: serde_json::json!({"id": id, "name": id}),
        }
    }

    #[test]
    fn repository_reads_writes_order_and_rejects_note_destructive_delete() {
        let mut connection = Connection::open_in_memory().unwrap();
        apply_migrations(&connection).unwrap();
        {
            let mut repository = ProjectsRepository::new(&mut connection);
            repository.insert_project(&project("p1")).unwrap();
            assert_eq!(repository.get_project_order().unwrap(), vec!["p1"]);
        }
        connection
            .execute(
                "UPDATE storage_ownership SET owner='sqlite' WHERE subsystem='notes'",
                [],
            )
            .unwrap();
        connection.execute("INSERT INTO notes(id,project_id,updated_at,payload_json) VALUES('n1','p1','now','{}')", []).unwrap();
        let mut repository = ProjectsRepository::new(&mut connection);
        assert!(repository.delete_project_storage("p1").is_err());
        assert!(repository.get_project("p1").unwrap().is_some());
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM notes", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            1
        );
    }

    fn note_payload(id: &str, stage_id: Option<&str>, mindmap: bool) -> String {
        serde_json::json!({
            "id":id,"project_id":"p","stage_id":stage_id,"title":"","content":"text",
            "content_format":if mindmap { "plain" } else { "html" },"checklist":[],
            "color":"default","pinned":false,"archived":false,"sort_order":0,"tags":[],
            "source_type":if mindmap { "mindmap" } else { "project" },
            "source_map_id":if mindmap { Some("map") } else { None },
            "source_node_id":if mindmap { Some(id) } else { None },
            "created_at":"2026-09-22T00:00:00Z","updated_at":"2026-09-22T00:00:00Z",
            "revision":0,"metadata":{}
        })
        .to_string()
    }

    fn configure_delete_database(connection: &Connection, bound: bool) {
        connection
            .execute(
                "INSERT INTO projects(id,name,infinite,unit,status,payload_json)
                 VALUES('p','Project',0,'symbols','active','{}')",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO project_order(project_id,position) VALUES('p',0)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO stages(id,project_id,name,infinite,unit,status,payload_json)
                 VALUES('s','p','Stage',0,'symbols','active','{}')",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO stage_order(stage_id,project_id,position) VALUES('s','p',0)",
                [],
            )
            .unwrap();
        if bound {
            connection.execute("INSERT INTO cloud_sync_state(account_id,device_id,pull_cursor,ack_cursor,created_at,updated_at) VALUES('account',?1,0,0,'now','now')", [TEST_DEVICE_ID]).unwrap();
            connection.execute("INSERT INTO cloud_sync_project_bindings(project_id,account_id,created_at,updated_at) VALUES('p','account','now','now')", []).unwrap();
        }
    }

    fn insert_test_note(connection: &Connection, id: &str, stage_id: Option<&str>, mindmap: bool) {
        connection
            .execute(
                "INSERT INTO notes(id,project_id,stage_id,updated_at,payload_json)
                 VALUES(?1,'p',?2,'2026-09-22T00:00:00Z',?3)",
                params![id, stage_id, note_payload(id, stage_id, mindmap)],
            )
            .unwrap();
    }

    fn temp_database(label: &str) -> (std::path::PathBuf, Connection) {
        let root = std::env::temp_dir().join(format!(
            "nfprogress-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let connection = crate::sqlite::open_database(&root.join("nfprogress.db")).unwrap();
        (root, connection)
    }

    #[test]
    fn note_sync_unmigrated_stage_delete_keeps_notes_out_of_cloud_history() {
        let (root, mut connection) = temp_database("stage-note-delete");
        configure_delete_database(&connection, false);
        insert_test_note(&connection, "stage-one", Some("s"), false);
        insert_test_note(&connection, "stage-two", Some("s"), true);
        connection.execute("INSERT INTO cloud_sync_state(account_id,device_id,pull_cursor,ack_cursor,created_at,updated_at) VALUES('account',?1,0,0,'now','now')", [TEST_DEVICE_ID]).unwrap();
        connection.execute("INSERT INTO cloud_sync_project_bindings(project_id,account_id,created_at,updated_at) VALUES('p','account','now','now')", []).unwrap();

        ProjectsRepository::new(&mut connection)
            .delete_stage_with_notes("s", "p")
            .unwrap();
        drop(connection);

        let connection = crate::sqlite::open_database(&root.join("nfprogress.db")).unwrap();
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM stages WHERE id='s'", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            0
        );
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM notes", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM cloud_sync_project_bindings WHERE project_id='p'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM cloud_sync_outbox AS event
                     JOIN cloud_sync_note_intents AS intent USING(event_id)
                     WHERE event.operation='delete'
                       AND json_extract(intent.snapshot_json,'$.stage_id')='s'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        drop(connection);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn note_sync_project_delete_refuses_cloud_bound_project_without_data_loss() {
        let (root, mut connection) = temp_database("project-note-delete-guard");
        configure_delete_database(&connection, false);
        insert_test_note(&connection, "ordinary", None, false);
        insert_test_note(&connection, "stage-note", Some("s"), false);
        insert_test_note(&connection, "map-note", None, true);
        connection.execute("INSERT INTO cloud_sync_state(account_id,device_id,pull_cursor,ack_cursor,created_at,updated_at) VALUES('account',?1,0,0,'now','now')", [TEST_DEVICE_ID]).unwrap();
        connection.execute("INSERT INTO cloud_sync_project_bindings(project_id,account_id,created_at,updated_at) VALUES('p','account','now','now')", []).unwrap();

        let result = ProjectsRepository::new(&mut connection).delete_project_with_notes("p");
        assert!(matches!(result, Err(RepositoryError::InvalidRelation(_))));
        drop(connection);

        let connection = crate::sqlite::open_database(&root.join("nfprogress.db")).unwrap();
        for (table, expected) in [
            ("projects", 1_i64),
            ("notes", 3),
            ("cloud_sync_project_bindings", 1),
            ("cloud_sync_outbox", 0),
            ("cloud_sync_note_intents", 0),
            ("domain_events", 0),
        ] {
            let count: i64 = connection
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(count, expected, "cloud-bound delete changed {table}");
        }
        drop(connection);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn note_sync_project_delete_failure_rolls_back_notes_binding_and_intents() {
        let mut connection = Connection::open_in_memory().unwrap();
        apply_migrations(&connection).unwrap();
        configure_delete_database(&connection, false);
        insert_test_note(&connection, "one", None, false);
        insert_test_note(&connection, "two", Some("s"), false);
        connection.execute("INSERT INTO cloud_sync_state(account_id,device_id,pull_cursor,ack_cursor,created_at,updated_at) VALUES('account',?1,0,0,'now','now')", [TEST_DEVICE_ID]).unwrap();
        connection.execute("INSERT INTO cloud_sync_project_bindings(project_id,account_id,created_at,updated_at) VALUES('p','account','now','now')", []).unwrap();
        connection.execute_batch("CREATE TRIGGER note_sync_test_fail_project_delete BEFORE DELETE ON projects BEGIN SELECT RAISE(ABORT,'injected_project_failure'); END;").unwrap();

        assert!(ProjectsRepository::new(&mut connection)
            .delete_project_with_notes("p")
            .is_err());
        connection
            .execute_batch("DROP TRIGGER note_sync_test_fail_project_delete")
            .unwrap();
        for (table, expected) in [
            ("projects", 1_i64),
            ("notes", 2),
            ("cloud_sync_project_bindings", 1),
            ("cloud_sync_outbox", 0),
            ("cloud_sync_note_intents", 0),
            ("domain_events", 0),
        ] {
            let count: i64 = connection
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(count, expected, "unexpected rollback state for {table}");
        }
    }

    #[test]
    fn note_sync_local_only_stage_and_project_delete_create_no_outbox() {
        let mut connection = Connection::open_in_memory().unwrap();
        apply_migrations(&connection).unwrap();
        configure_delete_database(&connection, false);
        insert_test_note(&connection, "stage-local", Some("s"), false);
        ProjectsRepository::new(&mut connection)
            .delete_stage_with_notes("s", "p")
            .unwrap();
        insert_test_note(&connection, "project-local", None, false);
        ProjectsRepository::new(&mut connection)
            .delete_project_with_notes("p")
            .unwrap();
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM cloud_sync_outbox", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            0
        );
    }
}
