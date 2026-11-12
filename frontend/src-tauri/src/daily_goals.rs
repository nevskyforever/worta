//! Standalone Desktop's cumulative writing plan and streak updates.
use std::collections::{HashMap, HashSet};

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

use crate::streaks::{date_days, streak_summary};

pub(crate) fn local_project(db: &Connection, project_id: &str) -> Result<bool, String> {
    db.query_row(
        "SELECT NOT EXISTS(SELECT 1 FROM cloud_sync_project_bindings WHERE project_id=?1)",
        [project_id],
        |row| row.get(0),
    )
    .map_err(|error| error.to_string())
}

fn number(value: &Value, field: &str) -> f64 {
    value[field].as_f64().unwrap_or_default()
}

fn deadline(entity: &Value) -> Option<i64> {
    entity["deadline"].as_str().and_then(date_days)
}

fn has_own_plan(entity: &Value) -> bool {
    deadline(entity).is_some() || number(entity, "personal_goal") > 0.0
}

struct WritingDay<'a> {
    date: &'a str,
    utc_start: String,
    utc_end: String,
}

impl<'a> WritingDay<'a> {
    fn new(db: &Connection, date: &'a str) -> Result<Self, String> {
        let start = crate::streaks::start_day_time(db);
        let tomorrow =
            crate::streaks::date_from_days(date_days(date).expect("validated writing day") + 1);
        db.query_row(
            "SELECT strftime('%Y-%m-%dT%H:%M:%S',?1,'utc'), strftime('%Y-%m-%dT%H:%M:%S',?2,'utc')",
            params![format!("{date}T{start}"), format!("{tomorrow}T{start}")],
            |row| {
                Ok(Self {
                    date,
                    utc_start: row.get(0)?,
                    utc_end: row.get(1)?,
                })
            },
        )
        .map_err(|error| error.to_string())
    }

    fn contains(&self, entry: &Value) -> bool {
        if let Some(date) = entry["writing_day"].as_str() {
            return date == self.date;
        }
        let Some(timestamp) = entry["created_at"].as_str() else {
            return false;
        };
        // Old native entries have a UTC instant. Migrated legacy entries have
        // an already-adjusted local date and must not be shifted a second time.
        if timestamp.ends_with('Z') {
            return timestamp.get(..19).is_some_and(|time| {
                self.utc_start.as_str() <= time && time < self.utc_end.as_str()
            });
        }
        timestamp.get(..10) == Some(self.date)
    }
}

fn added_today(entity: &Value, day: &WritingDay<'_>) -> f64 {
    let entries = entity["progress_entries"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|entry| day.contains(entry));
    sum_in_unit(entries, entity["unit"].as_str().unwrap_or("symbols"))
}

fn sum_in_unit<'a>(entries: impl Iterator<Item = &'a Value>, unit: &str) -> f64 {
    let factor = crate::unit_factor(unit).unwrap_or(1.0);
    entries
        .map(|entry| {
            entry["added_symbols"]
                .as_f64()
                .map_or_else(|| number(entry, "added"), |symbols| symbols / factor)
        })
        .sum()
}

fn project_goal(entity: &mut Value, day: &str, added: f64) {
    let today = date_days(day).expect("validated writing day");
    let total = number(entity, "total");
    let personal = number(entity, "personal_goal");
    let goal = entity["goal"].as_f64();
    let deadline = deadline(entity);
    let baseline = (total - added).max(0.0);
    // The migrated snapshot is also the anchor for subsequent days. Advancing
    // its target by the original rate carries missed volume into the next day.
    let anchor = entity["planning_date"]
        .as_str()
        .and_then(date_days)
        .filter(|date| *date <= today)
        .zip(entity["today_goal"].as_f64())
        .zip(
            entity["plan_daily_goal"]
                .as_f64()
                .filter(|rate| *rate > 0.0),
        );

    let plan = if entity["status"] == "активен" && (deadline.is_some() || personal > 0.0) {
        let rate = anchor.map(|(_, rate)| rate).unwrap_or_else(|| {
            if personal > 0.0 {
                personal
            } else {
                let remaining = (goal.unwrap_or(total) - baseline).max(0.0);
                remaining / (deadline.unwrap_or(today) - today + 1).max(1) as f64
            }
        });
        let target = match (deadline, goal) {
            (Some(end), Some(goal)) if today >= end => goal,
            _ => {
                let target = anchor.map_or(baseline + rate, |((date, target), _)| {
                    target + (today - date) as f64 * rate
                });
                if deadline.is_some() {
                    target.min(goal.unwrap_or(target))
                } else {
                    target
                }
            }
        };
        Some((target, rate))
    } else {
        None
    };
    entity["planning_date"] = json!(day);
    entity["added_today"] = json!(added);
    entity["today_goal"] = json!(plan.map(|(target, _)| target));
    entity["plan_daily_goal"] = json!(plan.map(|(_, rate)| rate));
}

pub(crate) fn project(db: &Connection, entity: &mut Value, day: &str) -> Result<(), String> {
    project_for_day(entity, &WritingDay::new(db, day)?);
    Ok(())
}

fn project_for_day(entity: &mut Value, day: &WritingDay<'_>) {
    let added = added_today(entity, day);
    let mut stages = StageTotals::default();
    for stage in entity["stages"].as_array_mut().into_iter().flatten() {
        let stage_added = added_today(stage, day);
        project_goal(stage, day.date, stage_added);
        stages.include(stage);
    }
    project_parent(entity, day.date, added, &stages);
}

#[derive(Default)]
struct StageTotals {
    count: usize,
    added: f64,
    target: f64,
    rate: f64,
}

impl StageTotals {
    fn include(&mut self, stage: &Value) {
        self.count += 1;
        self.added += number(stage, "added_today");
        if stage["status"] != "в архиве" {
            self.target += stage["today_goal"]
                .as_f64()
                .unwrap_or_else(|| number(stage, "total"));
            self.rate += number(stage, "plan_daily_goal");
        }
    }
}

fn project_parent(entity: &mut Value, day: &str, added: f64, stages: &StageTotals) {
    project_goal(
        entity,
        day,
        if stages.count == 0 {
            added
        } else {
            stages.added
        },
    );
    if entity["status"] == "активен" && !has_own_plan(entity) && stages.rate > 0.0 {
        entity["today_goal"] = json!(stages.target);
        entity["plan_daily_goal"] = json!(stages.rate);
    }
}

pub(crate) fn reset_if_changed(previous: &Value, next: &mut Value) {
    if [
        "deadline",
        "personal_goal",
        "goal",
        "infinite",
        "unit",
        "total",
    ]
    .iter()
    .any(|key| previous[*key] != next[*key])
    {
        next["planning_date"] = Value::Null;
    }
}

pub(crate) fn game_state(db: &Connection) -> Result<Value, String> {
    let raw: Option<String> = db
        .query_row(
            "SELECT payload_json FROM game_state WHERE id=1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| error.to_string())?;
    raw.map(|raw| serde_json::from_str(&raw).map_err(|error| error.to_string()))
        .transpose()
        .map(|state| state.unwrap_or(Value::Null))
}

pub(crate) fn overlay_streak(entity: &mut Value, state: &Value, key: &str, day: &str) {
    let Some(payload) = entity.as_object_mut() else {
        return;
    };
    let empty = serde_json::Map::new();
    let streak = state["project_game_state"][key]
        .as_object()
        .unwrap_or(&empty);
    let status = crate::streaks::canonical_local_status(streak, day, payload);
    for field in [
        "streaks",
        "max_streak",
        "last_streak_bonus",
        "last_streak_lost_date",
        "freezes",
    ] {
        if let Some(value) = streak.get(field) {
            payload.insert(field.into(), value.clone());
        }
    }
    let length = streak_summary(payload.get("streaks")).1;
    if length > 0 || !streak.is_empty() {
        let max = payload
            .get("max_streak")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        payload.insert("max_streak".into(), json!(max.max(length as u64)));
        payload.insert(
            "streak_length".into(),
            json!(
                if status.starts_with("Lose ") && !status.ends_with(" Start") {
                    0
                } else {
                    length
                }
            ),
        );
    }
    payload.insert("streak_status".into(), json!(status));
}

pub(crate) fn project_rows(
    db: &Connection,
    projects: &mut [crate::SqliteEntityRow],
    stages: &mut [crate::SqliteEntityRow],
    progress: &[crate::SqliteProgressRow],
) -> Result<(), String> {
    let mut query = db
        .prepare("SELECT project_id FROM cloud_sync_project_bindings")
        .map_err(|e| e.to_string())?;
    let bound = query
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|e| e.to_string())?
        .collect::<Result<HashSet<_>, _>>()
        .map_err(|e| e.to_string())?;
    let date = crate::streaks::logical_writing_day(db)?;
    let day = WritingDay::new(db, &date)?;
    let state = game_state(db)?;
    let mut entries = HashMap::<&str, Vec<Value>>::new();
    for entry in progress
        .iter()
        .filter(|entry| !bound.contains(&entry.project_id))
    {
        let mut value = serde_json::from_str::<Value>(&entry.payload_json)
            .ok()
            .filter(Value::is_object)
            .unwrap_or_else(|| json!({}));
        value["added_symbols"] = json!(entry.added_symbols.or(value["added_symbols"].as_f64()));
        value["created_at"] = json!(entry.created_at.as_deref().or(value["created_at"].as_str()));
        if day.contains(&value) {
            entries
                .entry(entry.stage_id.as_deref().unwrap_or(&entry.project_id))
                .or_default()
                .push(value);
        }
    }
    let mut totals = HashMap::<&str, StageTotals>::new();
    let empty_totals = StageTotals::default();
    for row in stages {
        let Some(parent) = row.project_id.as_deref() else {
            continue;
        };
        if bound.contains(parent) {
            continue;
        }
        let mut stage: Value =
            serde_json::from_str(&row.payload_json).map_err(|e| e.to_string())?;
        let added = sum_in_unit(entries.remove(row.id.as_str()).iter().flatten(), &row.unit);
        project_goal(&mut stage, day.date, added);
        overlay_streak(
            &mut stage,
            &state,
            &format!("stage:{parent}:{}", row.id),
            day.date,
        );
        totals.entry(parent).or_default().include(&stage);
        row.payload_json = stage.to_string();
    }
    for row in projects.iter_mut().filter(|row| !bound.contains(&row.id)) {
        let mut project: Value =
            serde_json::from_str(&row.payload_json).map_err(|e| e.to_string())?;
        let added = sum_in_unit(entries.remove(row.id.as_str()).iter().flatten(), &row.unit);
        project_parent(
            &mut project,
            day.date,
            added,
            totals.get(row.id.as_str()).unwrap_or(&empty_totals),
        );
        overlay_streak(
            &mut project,
            &state,
            &format!("project:{}", row.id),
            day.date,
        );
        row.payload_json = project.to_string();
    }
    Ok(())
}

pub(crate) fn record_from_storage(
    db: &Connection,
    project_id: &str,
    stage_id: Option<&str>,
    day: &str,
    now: &str,
    positive: bool,
) -> Result<(), String> {
    // The transaction has already updated the entity, its entries and, for a
    // stage write, the parent's aggregate. Read that snapshot before commit.
    let raw: String = db
        .query_row(
            "SELECT payload_json FROM projects WHERE id=?1",
            [project_id],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;
    let mut payload: Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
    project(db, &mut payload, day)?;
    record(db, &payload, project_id, stage_id, day, now, positive)
}

fn store_plan(db: &Connection, table: &str, id: &str, entity: &Value) -> Result<(), String> {
    db.execute(
        &format!("UPDATE {table} SET payload_json=json_set(payload_json,'$.planning_date',json(?1),'$.today_goal',json(?2),'$.plan_daily_goal',json(?3)) WHERE id=?4 AND (json_extract(payload_json,'$.planning_date') IS NULL OR json_extract(payload_json,'$.plan_daily_goal') IS NULL OR json_extract(payload_json,'$.plan_daily_goal') <= 0)"),
        params![entity["planning_date"].to_string(), entity["today_goal"].to_string(), entity["plan_daily_goal"].to_string(), id],
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

fn record(
    db: &Connection,
    project: &Value,
    project_id: &str,
    stage_id: Option<&str>,
    day: &str,
    now: &str,
    positive: bool,
) -> Result<(), String> {
    // The parent without a deadline only displays the aggregate stage plan.
    // It has no independent streak or plan anchor.
    if stage_id.is_none() || has_own_plan(project) {
        store_plan(db, "projects", project_id, project)?;
    }
    let stage = stage_id.and_then(|id| {
        project["stages"]
            .as_array()?
            .iter()
            .find(|stage| stage["id"] == id)
    });
    if let (Some(id), Some(stage)) = (stage_id, stage) {
        store_plan(db, "stages", id, stage)?;
    }

    if !positive || !setting(db, "global_streak")? {
        return Ok(());
    }
    let mut state = game_state(db)?;
    if state.is_null() {
        return Ok(());
    }
    let before = state.clone();
    let portable_account = portable_account(db)?;
    let game_mode = !portable_account && setting(db, "game_mode")?;
    if game_mode {
        refresh_freezes(db, &mut state, day)?;
    }
    let mut global = global_fields(&state);
    let parent_is_primary =
        stage_id.is_none() || (has_own_plan(project) && project["streak_enabled"] != false);
    let mut completed = false;
    for (key, entity, primary) in [
        (
            format!("project:{project_id}"),
            parent_is_primary.then_some(project),
            parent_is_primary,
        ),
        (
            format!("stage:{project_id}:{}", stage_id.unwrap_or_default()),
            stage,
            !parent_is_primary,
        ),
    ] {
        let Some(entity) = entity else { continue };
        if entity["status"] != "активен"
            || entity["infinite"] == true
            || entity["streak_enabled"] == false
            || !entity["today_goal"]
                .as_f64()
                .is_some_and(|target| target > 0.0 && number(entity, "total") + 1e-9 >= target)
        {
            continue;
        }
        let mut fields = state["project_game_state"][&key].clone();
        if fields.is_null() {
            fields = json!({});
        }
        for field in [
            "streaks",
            "max_streak",
            "last_streak_bonus",
            "streak_status",
            "last_streak_lost_date",
            "last_streak_lose_len",
            "freezes",
        ] {
            if fields[field].is_null() && !entity[field].is_null() {
                fields[field] = entity[field].clone();
            }
        }
        if fields["streak_status"] == "Off" && entity["streak_enabled"] == true {
            fields["streak_status"] = json!("No");
        }
        if fields["streak_status"] == "Off" {
            continue;
        }
        if let Some((length, lost)) = complete(&mut fields, false, day) {
            if game_mode && primary {
                crate::game::award_desktop_streak(
                    &mut state["gamer"],
                    &mut fields,
                    &format!("local:{key}"),
                    day,
                    length,
                    lost,
                );
            }
        }
        state["project_game_state"][&key] = fields;
        if primary {
            completed |= streak_summary(state["project_game_state"][&key].get("streaks")).0
                == date_days(day);
        }
    }

    if completed && !portable_account {
        if game_mode {
            let last = streak_summary(global.get("global_streaks")).0;
            if last
                .zip(date_days(day))
                .is_some_and(|(last, today)| today - last >= 2)
            {
                state["global_streak"] = global;
                crate::game::apply_desktop_global_loss(&mut state, day, now)?;
                global = state["global_streak"].clone();
            }
        }
        if let Some((length, lost)) = complete(&mut global, true, day) {
            if game_mode {
                crate::game::award_desktop_streak(
                    &mut state["gamer"],
                    &mut global,
                    "global",
                    day,
                    length,
                    lost,
                );
            }
        }
        state["global_streak"] = global;
    }
    if state != before {
        db.execute(
            "UPDATE game_state SET payload_json=?1,updated_at=?2 WHERE id=1",
            params![state.to_string(), now],
        )
        .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn global_fields(state: &Value) -> Value {
    if state["global_streak"].is_object() {
        return state["global_streak"].clone();
    }
    let mut fields = json!({});
    for key in [
        "global_streaks",
        "max_global_streak",
        "global_streak_status",
        "last_global_streak_bonus",
        "last_global_streak_lost_date",
        "last_global_streak_lose_len",
    ] {
        if let Some(value) = state["game"].get(key) {
            fields[key] = value.clone();
        }
    }
    fields
}

fn portable_account(db: &Connection) -> Result<bool, String> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM cloud_game_migrations WHERE owner_key='account')",
        [],
        |row| row.get(0),
    )
    .map_err(|error| error.to_string())
}

pub(crate) fn refresh_freezes(
    db: &Connection,
    state: &mut Value,
    day: &str,
) -> Result<bool, String> {
    if freeze_count(&state["gamer"]) == 0 || !setting(db, "global_streak")? || portable_account(db)?
    {
        return Ok(false);
    }
    let mut global = global_fields(state);
    let mut changed = false;
    // A parent with its own enabled plan owns the series. Otherwise each
    // active stage with a plan owns its own series.
    let mut query = db.prepare(
        "SELECT p.id, NULL, p.payload_json, p.payload_json FROM projects p
         WHERE p.status='активен' AND p.infinite=0 AND NOT EXISTS(SELECT 1 FROM cloud_sync_project_bindings b WHERE b.project_id=p.id)
         UNION ALL
         SELECT p.id, s.id, s.payload_json, p.payload_json FROM stages s JOIN projects p ON p.id=s.project_id
         WHERE p.status='активен' AND s.status='активен' AND s.infinite=0 AND NOT EXISTS(SELECT 1 FROM cloud_sync_project_bindings b WHERE b.project_id=p.id)
         ORDER BY 1, 2"
    ).map_err(|e| e.to_string())?;
    let candidates = query
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    for candidate in candidates {
        let (project_id, stage_id, raw, parent) = candidate.map_err(|e| e.to_string())?;
        let entity: Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
        let parent: Value = serde_json::from_str(&parent).map_err(|e| e.to_string())?;
        if entity["auto_freeze"] == false
            || entity["streak_enabled"] == false
            || !has_own_plan(&entity)
            || (stage_id.is_some() && parent["streak_enabled"] != false && has_own_plan(&parent))
        {
            continue;
        }
        let key = stage_id.map_or_else(
            || format!("project:{project_id}"),
            |stage| format!("stage:{project_id}:{stage}"),
        );
        let mut fields = state["project_game_state"][&key].clone();
        if fields["streak_status"] == "Off" || !fields["streaks"].is_array() {
            continue;
        }
        let first = streak_summary(fields.get("streaks")).0;
        let count = fill_gap(&mut fields["streaks"], &mut state["gamer"], day);
        if count == 0 {
            continue;
        }
        fields["freezes"] = json!(fields["freezes"].as_u64().unwrap_or(0) + u64::from(count));
        fields["streak_status"] = json!("Freeze");
        fields["max_streak"] = json!(fields["max_streak"]
            .as_u64()
            .unwrap_or(0)
            .max(streak_summary(fields.get("streaks")).1 as u64));
        state["project_game_state"][&key] = fields;
        changed = true;

        if first == streak_summary(global.get("global_streaks")).0 {
            if let Some(history) = global["global_streaks"].as_array_mut() {
                history.extend((0..count).map(|_| json!("freeze")));
            }
        }
    }
    if fill_gap(&mut global["global_streaks"], &mut state["gamer"], day) > 0 {
        changed = true;
    }
    if changed {
        let length = streak_summary(global.get("global_streaks")).1 as u64;
        if length > 0 {
            global["max_global_streak"] = json!(global["max_global_streak"]
                .as_u64()
                .unwrap_or(0)
                .max(length));
            state["global_streak"] = global;
        }
    }
    Ok(changed)
}

fn freeze_count(gamer: &Value) -> u64 {
    let items = &gamer["items"]["Предметы"];
    items["Заморозка"]
        .as_u64()
        .unwrap_or(0)
        .max(items["❄️Заморозка"].as_u64().unwrap_or(0))
}

fn setting(db: &Connection, key: &str) -> Result<bool, String> {
    db.query_row(
        "SELECT value_json FROM settings WHERE key=?1",
        [key],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .map(|value| value.as_deref() == Some("true"))
    .map_err(|error| error.to_string())
}

fn fill_gap(history: &mut Value, gamer: &mut Value, day: &str) -> u32 {
    let (Some(last), _, _) = streak_summary(Some(history)) else {
        return 0;
    };
    let missed = date_days(day).unwrap_or(last) - last - 1;
    let Some(items) = gamer
        .get_mut("items")
        .and_then(|items| items.get_mut("Предметы"))
        .and_then(Value::as_object_mut)
    else {
        return 0;
    };
    let key = if items.get("Заморозка").and_then(Value::as_u64).unwrap_or(0) > 0 {
        "Заморозка"
    } else {
        "❄️Заморозка"
    };
    let count = items.get(key).and_then(Value::as_u64).unwrap_or(0);
    if missed <= 0 || missed as u64 > count || missed > 10_000 {
        return 0;
    }
    if let Some(history) = history.as_array_mut() {
        for _ in 0..missed {
            history.push(json!("freeze"));
        }
        items.insert(key.into(), json!(count - missed as u64));
    }
    missed as u32
}

pub(crate) fn complete(fields: &mut Value, global: bool, day: &str) -> Option<(usize, usize)> {
    let [history, maximum, status, lost_date, lost_length] = if global {
        [
            "global_streaks",
            "max_global_streak",
            "global_streak_status",
            "last_global_streak_lost_date",
            "last_global_streak_lose_len",
        ]
    } else {
        [
            "streaks",
            "max_streak",
            "streak_status",
            "last_streak_lost_date",
            "last_streak_lose_len",
        ]
    };
    let today = date_days(day)?;
    let (last, length, _) = streak_summary(fields.get(history));
    if last.is_some_and(|last| last >= today) {
        return None;
    }
    let continuing = last == Some(today - 1);
    let lost = if last.is_some() && !continuing {
        length
    } else if crate::streaks::streak_date_days(&fields[lost_date]) == Some(today) {
        fields[lost_length].as_u64().unwrap_or(0) as usize
    } else {
        0
    };
    if !continuing {
        fields[history] = json!([]);
    }
    fields[history]
        .as_array_mut()?
        .push(json!({"__type__":"date","value":day}));
    let next_length = if continuing { length + 1 } else { 1 };
    fields[maximum] = json!(fields[maximum]
        .as_u64()
        .unwrap_or(0)
        .max(length.max(next_length) as u64));
    fields[status] = json!(if continuing {
        "Go".to_owned()
    } else if lost > 0 {
        format!("Lose {lost} Start")
    } else {
        "Start".to_owned()
    });
    fields[lost_date] = Value::Null;
    fields[lost_length] = json!(0);
    Some((next_length, lost))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_timestamps_and_explicit_writing_days_use_the_correct_boundary() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE settings(key TEXT PRIMARY KEY,value_json TEXT);
            INSERT INTO settings VALUES('start_day_time','\"04:00:00\"');",
        )
        .unwrap();
        let day = WritingDay::new(&db, "2026-09-20").unwrap();
        assert!(day.contains(&json!({"created_at":format!("{}.001Z", day.utc_start)})));
        assert!(!day.contains(&json!({"created_at":format!("{}Z", day.utc_end)})));
        assert!(day.contains(&json!({"created_at":"2026-09-20T02:00:00"})));
        assert!(
            !day.contains(&json!({"created_at":"2026-09-20T02:00:00","writing_day":"2026-09-19"}))
        );

        let previous_second: String = db
            .query_row(
                "SELECT strftime('%Y-%m-%dT%H:%M:%SZ',?1,'-1 second')",
                [&day.utc_start],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!day.contains(&json!({"created_at":previous_second})));
        assert_eq!(
            crate::streaks::logical_writing_day_at(&db, &previous_second).unwrap(),
            "2026-09-19"
        );
        assert_eq!(
            crate::streaks::logical_writing_day_at(&db, &day.utc_start).unwrap(),
            day.date
        );
    }

    #[test]
    fn deadline_only_plan_keeps_its_original_rate_after_a_missed_day() {
        let db = Connection::open_in_memory().unwrap();
        let mut stage = json!({"unit":"symbols","status":"активен","total":10,"goal":100,"deadline":"2026-09-22"});
        project(&db, &mut stage, "2026-09-20").unwrap();
        assert_eq!(stage["plan_daily_goal"], 30.0);
        assert_eq!(stage["today_goal"], 40.0);
        project(&db, &mut stage, "2026-09-21").unwrap();
        assert_eq!(stage["plan_daily_goal"], 30.0);
        assert_eq!(stage["today_goal"], 70.0);
        project(&db, &mut stage, "2026-09-22").unwrap();
        assert_eq!(stage["today_goal"], 100.0);
    }
}
