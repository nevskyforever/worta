//! Deterministic Game event application at the trusted desktop boundary.
//!
//! The event consumer deliberately accepts the versioned JSON DTO rather than
//! a Rust-struct serialization.  This keeps migration payloads compatible with
//! the Python oracle and makes unknown Game fields round-trip untouched.

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Map, Value};

const EVENT_VERSION: i64 = 1;
const STATE_VERSION: i64 = 2;
const MAX_ITEM_COUNT: i64 = 10_000;
const MAX_MONEY: f64 = 1_000_000_000_000.0;

/// The only randomness used by the native Game application layer is supplied
/// through this small port. Production uses the OS provider; unit tests can
/// pass a fixed sequence without touching a global RNG.
pub trait GameRng {
    fn uniform_u32(&mut self, upper_exclusive: u32) -> u32;
}

#[derive(Default)]
pub struct OsGameRng;

impl GameRng for OsGameRng {
    fn uniform_u32(&mut self, upper_exclusive: u32) -> u32 {
        if upper_exclusive == 0 {
            return 0;
        }
        let mut bytes = [0_u8; 4];
        if getrandom::fill(&mut bytes).is_err() {
            return 0;
        }
        u32::from_le_bytes(bytes) % upper_exclusive
    }
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "code", content = "message")]
pub enum GameError {
    InsufficientFunds(String),
    InvalidQuantity(String),
    AlreadyClaimed(String),
    PrerequisiteMissing(String),
    NotFound(String),
    CooldownActive(String),
    InvalidState(String),
    Database(String),
    Validation(String),
}

impl std::fmt::Display for GameError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InsufficientFunds(message)
            | Self::InvalidQuantity(message)
            | Self::AlreadyClaimed(message)
            | Self::PrerequisiteMissing(message)
            | Self::NotFound(message)
            | Self::CooldownActive(message)
            | Self::InvalidState(message)
            | Self::Database(message)
            | Self::Validation(message) => formatter.write_str(message),
        }
    }
}

type GameResult<T> = Result<T, GameError>;

#[derive(Debug, Default, serde::Serialize)]
pub struct ProcessSummary {
    pub processed: usize,
    pub failed: usize,
}

fn number(value: Option<&Value>) -> f64 {
    value.and_then(Value::as_f64).unwrap_or(0.0)
}

fn gamer_cf(gamer: &Map<String, Value>, key: &str) -> f64 {
    gamer
        .get("cf")
        .and_then(Value::as_object)
        .and_then(|coefficients| coefficients.get(key))
        .map(|value| value.get("value").unwrap_or(value))
        .and_then(Value::as_f64)
        .unwrap_or(1.0)
}

fn rounded_increment(value: f64) -> f64 {
    ((value - 1e-9) * 10.0).ceil() / 10.0
}

fn apply_event(
    state: &mut Value,
    event_type: &str,
    event_id: &str,
    delta: f64,
    context: &Map<String, Value>,
    project_id: &str,
) -> Result<(), String> {
    let root = state
        .as_object_mut()
        .ok_or_else(|| "canonical Game state is not an object".to_string())?;
    let gamer = root
        .entry("gamer")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| "canonical Gamer state is not an object".to_string())?;
    match event_type {
        "ProgressAdded" if delta > 0.0 => {
            let multiplier = (1.0 + number(gamer.get("inspiration")) / 100.0 * 0.1)
                * (1.0 + number(gamer.get("writing_reward_bonus")));
            let coins = rounded_increment(
                number(gamer.get("coins"))
                    + rounded_increment(
                        delta / 100.0 * 10.0 * gamer_cf(gamer, "coins") * multiplier,
                    ),
            );
            gamer.insert("coins".to_string(), json!(coins));
            add_experience(
                gamer,
                rounded_increment(delta / 100.0 * 500.0 * gamer_cf(gamer, "exp") * multiplier),
            );
            gamer.insert("writing_reward_bonus".to_string(), json!(0.0));
            if let Some(session) = gamer
                .get_mut("writing_session")
                .and_then(Value::as_object_mut)
            {
                let progress = number_field(session, "progress", 0.0) + delta;
                session.insert("progress".into(), json!(progress));
            }
            if let Some(daily) = gamer
                .get_mut("daily_challenge")
                .and_then(Value::as_object_mut)
            {
                if daily
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("symbols")
                    == "symbols"
                    && daily.get("completed").and_then(Value::as_bool) != Some(true)
                {
                    let progress = number_field(daily, "progress", 0.0) + delta;
                    let target = number_field(daily, "target", f64::MAX);
                    daily.insert("progress".into(), json!(progress));
                    if progress >= target {
                        daily.insert("completed".into(), json!(true));
                        add_number(gamer, "coins", 100.0);
                        add_experience(gamer, 300.0);
                        add_capped(gamer, "inspiration", 10.0, 100.0);
                    }
                }
            }
            if let Some(weekly) = gamer
                .get_mut("weekly_challenge")
                .and_then(Value::as_object_mut)
            {
                if weekly.get("key").and_then(Value::as_str).unwrap_or("") == "symbols"
                    && weekly.get("completed").and_then(Value::as_bool) != Some(true)
                {
                    let progress = number_field(weekly, "progress", 0.0) + delta;
                    let target = 10_000.0;
                    weekly.insert("progress".into(), json!(progress));
                    if progress >= target {
                        weekly.insert("completed".into(), json!(true));
                        add_number(gamer, "coins", 500.0);
                        add_experience(gamer, 1500.0);
                        add_capped(gamer, "inspiration", 20.0, 100.0);
                    }
                }
            }
            let productive = integer_field(gamer, "productive_actions_since_event", 0) + 1;
            if productive >= 3 && gamer.get("pending_creative_event").is_none() {
                gamer.insert("pending_creative_event".into(), json!("unexpected_idea"));
                gamer.insert("productive_actions_since_event".into(), json!(0));
            } else {
                gamer.insert("productive_actions_since_event".into(), json!(productive));
            }
        }
        "ProgressDeleted" => {
            root.entry("extensions")
                .or_insert_with(|| Value::Object(Map::new()))
                .as_object_mut()
                .ok_or_else(|| "Game extensions are not an object".to_string())?
                .entry("progress_deletions")
                .or_insert_with(|| Value::Array(Vec::new()))
                .as_array_mut()
                .ok_or_else(|| "Game deletion journal is not an array".to_string())?
                .push(Value::String(event_id.to_string()));
        }
        "ProjectCompleted" | "StageCompleted" => {
            let key = context
                .get("key")
                .and_then(Value::as_str)
                .unwrap_or(project_id);
            let claimed = gamer
                .entry("complete_bonus_projects")
                .or_insert_with(|| Value::Array(Vec::new()))
                .as_array_mut()
                .ok_or_else(|| "completion marker is not an array".to_string())?;
            if !claimed.iter().any(|value| value.as_str() == Some(key)) {
                claimed.push(Value::String(key.to_string()));
                let total = number(context.get("total_symbols"));
                let multiplier = if event_type == "StageCompleted" {
                    0.25
                } else {
                    1.0
                };
                let reward = (total / 1000.0 + 0.5).round() * 100.0 * multiplier;
                gamer.insert(
                    "coins".to_string(),
                    json!(number(gamer.get("coins")) + reward),
                );
                add_experience(gamer, reward * 100.0);
            }
        }
        "ProjectStatusChanged" | "ProjectDeleted" => {
            root.entry("extensions")
                .or_insert_with(|| Value::Object(Map::new()))
                .as_object_mut()
                .ok_or_else(|| "Game extensions are not an object".to_string())?
                .entry("lifecycle_events")
                .or_insert_with(|| Value::Array(Vec::new()))
                .as_array_mut()
                .ok_or_else(|| "Game lifecycle journal is not an array".to_string())?
                .push(Value::String(event_id.to_string()));
        }
        other => return Err(format!("unsupported Game event: {other}")),
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn apply_ledger_rule_fixture(
    state: &mut Value,
    event_type: &str,
    delta: f64,
    context: &Map<String, Value>,
) -> Result<(), String> {
    apply_event(
        state,
        event_type,
        "ledger-rule-fixture",
        delta,
        context,
        "p",
    )
}

/// The immutable loss identity also keys notifications, including remote replay.
pub(crate) fn append_loss_notifications(
    before: &Value,
    after: &mut Value,
    id: &str,
    now: &str,
) -> Result<(), String> {
    let length = after["global_streak"]["last_global_streak_lose_len"]
        .as_u64()
        .ok_or("invalid_game_payload")?;
    let old = before["gamer"]["health"]
        .as_f64()
        .ok_or("invalid_game_payload")?;
    let new = after["gamer"]["health"]
        .as_f64()
        .ok_or("invalid_game_payload")?;
    let damage = old.max(0.0).min(length as f64 * 5.0);
    let old_potions = before["gamer"]["items"]["Зелья"]["Зелье воскрешения"]
        .as_u64()
        .unwrap_or(0);
    let new_potions = after["gamer"]["items"]["Зелья"]["Зелье воскрешения"]
        .as_u64()
        .unwrap_or(0);
    let revived = old_potions == new_potions + 1 && new > 0.0;
    let root = after.as_object_mut().ok_or("invalid_game_payload")?;
    let notices = root
        .entry("notifications")
        .or_insert_with(|| json!({"new":[],"read":[]}));
    let existing = notices["new"]
        .as_array()
        .into_iter()
        .flatten()
        .chain(notices["read"].as_array().into_iter().flatten())
        .filter_map(|v| v["fields"]["notification_id"].as_str())
        .map(str::to_owned)
        .collect::<std::collections::HashSet<_>>();
    let unread = notices["new"]
        .as_array_mut()
        .ok_or("invalid_game_payload")?;
    let messages = [
        (
            format!("streak-loss:{id}"),
            format!("🥺 СТРИК ПОТЕРЯН: {length} д. Урон: {damage} HP. Здоровье: {old} → {new} HP."),
            true,
        ),
        (
            format!("streak-revival:{id}"),
            format!("💀 Зелье воскрешения спасло вас! Здоровье восстановлено до {new} HP."),
            revived,
        ),
    ];
    for (key, text, emit) in messages {
        if emit && !existing.contains(&key) {
            unread.push(json!({"__type__":"Notification","fields":{"notification_id":key,"text":text,"tag":"game","date_create":now}}));
        }
    }
    Ok(())
}

/// Calendar maintenance is also invoked when no Progress event exists.
/// An emptied series is the durable processed-loss marker; the immutable cloud
/// action adds its stable identity when Game synchronization is active.
pub(crate) fn refresh_global_loss(
    connection: &Connection,
    day: &str,
    now: &str,
) -> Result<bool, String> {
    if !GameApplicationService::enabled(connection) {
        return Ok(false);
    }
    let raw = connection
        .query_row("SELECT payload_json FROM game_state WHERE id=1", [], |r| {
            r.get::<_, String>(0)
        })
        .optional()
        .map_err(|e| e.to_string())?;
    let Some(raw) = raw else {
        return Ok(false);
    };
    let before: Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
    let streak = crate::game_projection::streak_base(&before["global_streak"], true)?;
    if streak["enabled"] != true || streak["history"].as_array().is_none_or(|h| h.is_empty()) {
        return Ok(false);
    }
    let last = streak["history"]
        .as_array()
        .and_then(|h| h.last())
        .and_then(|v| v["day"].as_str())
        .and_then(crate::streaks::date_days)
        .ok_or("invalid_game_payload")?;
    if crate::streaks::date_days(day).is_none_or(|today| today < last + 2) {
        return Ok(false);
    }
    let lost = crate::game_codec::loss_after(&streak, day)?;
    let mut after = if let Some(after) =
        crate::game_writer::capture_loss(connection, &before, day, now)?
    {
        after
    } else {
        let mut after = before.clone();
        let g = after["gamer"]
            .as_object_mut()
            .ok_or("invalid_game_payload")?;
        let mut inventory = Vec::new();
        if let Some(categories) = g.get("items").and_then(Value::as_object) {
            for (category, items) in categories {
                if let Some(items) = items.as_object() {
                    for (key, count) in items {
                        if count.as_u64().is_some_and(|n| n > 0) {
                            inventory
                                .push(json!({"category":category,"item_id":key,"count":count}));
                        }
                    }
                }
            }
        }
        let max = number_field(g, "max_health", 100.0).max(0.0);
        let mut base = json!({"health":format!("{:.6}",number_field(g,"health",max).clamp(0.0,max)),"max_health":format!("{max:.6}"),"inventory":inventory});
        crate::game_sync::loss_health(
            &mut base,
            lost["lost_length"].as_u64().ok_or("invalid_game_payload")?,
        )?;
        g.insert(
            "health".into(),
            json!(base["health"]
                .as_str()
                .ok_or("invalid_game_payload")?
                .parse::<f64>()
                .map_err(|_| "invalid_game_payload")?),
        );
        let potion = base["inventory"]
            .as_array()
            .and_then(|v| {
                v.iter()
                    .find(|v| v["category"] == "Зелья" && v["item_id"] == "Зелье воскрешения")
            })
            .map(|v| v["count"].clone())
            .unwrap_or(json!(0));
        // Preserve every unrelated legacy inventory entry and extension.
        if g.get("items")
            .and_then(|v| v.get("Зелья"))
            .and_then(Value::as_object)
            .is_some_and(|v| v.contains_key("Зелье воскрешения"))
        {
            g["items"]["Зелья"]["Зелье воскрешения"] = potion;
        }
        after["global_streak"] = crate::game_projection::streak_fields(&lost, true)?;
        after
    };
    // capture_loss already froze its exact payload; timestamp is local-only and
    // maintained by the common materializer below, before an intent is retained.
    after["gamer"]["last_health_recovery_at"] = json!({"__type__":"datetime","value":now});
    let id = if let Some(account) = connection
        .query_row(
            "SELECT canonical_user_id FROM cloud_account_bindings LIMIT 1",
            [],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .map_err(|e| e.to_string())?
    {
        crate::game_sync::loss_id(&account, &streak)
    } else {
        format!(
            "{}:{}",
            lost["lost_day"].as_str().unwrap_or(day),
            lost["lost_length"].as_u64().unwrap_or(0)
        )
    };
    append_loss_notifications(&before, &mut after, &id, now)?;
    connection
        .execute(
            "UPDATE game_state SET schema_version=2,payload_json=?1,updated_at=?2 WHERE id=1",
            params![after.to_string(), now],
        )
        .map_err(|e| e.to_string())?;
    Ok(true)
}

pub fn process_pending_events(
    connection: &mut Connection,
    limit: i64,
) -> Result<ProcessSummary, String> {
    let transaction = connection
        .transaction()
        .map_err(|error| error.to_string())?;
    let day = crate::streaks::logical_writing_day(&transaction)?;
    let now = GameApplicationService::now(&transaction).map_err(|e| e.to_string())?;
    let now = format!("{}.000000Z", now.get(..19).ok_or("invalid_game_payload")?);
    refresh_global_loss(&transaction, &day, &now)?;
    let mut statement = transaction
        .prepare("SELECT event_id,event_type,project_id,delta_symbols,context_json,version,attempt_count,stage_id,progress_id FROM domain_events WHERE consumer='game' AND processed_at IS NULL AND status != 'failed' ORDER BY created_at,event_id LIMIT ?1")
        .map_err(|error| error.to_string())?;
    let rows: Vec<(
        String,
        String,
        String,
        Option<f64>,
        String,
        i64,
        i64,
        Option<String>,
        Option<String>,
    )> = statement
        .query_map([limit.max(1)], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
                row.get(8)?,
            ))
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<_, _>>()
        .map_err(|error| error.to_string())?;
    drop(statement);

    let mut summary = ProcessSummary::default();
    for (
        event_id,
        event_type,
        project_id,
        delta,
        context_json,
        version,
        attempts,
        stage_id,
        progress_id,
    ) in rows
    {
        transaction
            .execute_batch("SAVEPOINT game_domain_action")
            .map_err(|error| error.to_string())?;
        let result = (|| {
            if version != EVENT_VERSION {
                return Err(format!("unsupported event version: {version}"));
            }
            let context: Value =
                serde_json::from_str(&context_json).map_err(|error| error.to_string())?;
            let context = context
                .as_object()
                .ok_or_else(|| "event context is not an object".to_string())?;
            let raw_state: String = transaction
                .query_row(
                    "SELECT payload_json FROM game_state WHERE id=1",
                    [],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())?;
            let mut state: Value =
                serde_json::from_str(&raw_state).map_err(|error| error.to_string())?;
            if !crate::game_writer::already_recorded(
                &transaction,
                &event_id,
                &project_id,
                stage_id.as_deref(),
                progress_id.as_deref(),
                delta.unwrap_or(0.0),
            )? {
                apply_event(
                    &mut state,
                    &event_type,
                    &event_id,
                    delta.unwrap_or(0.0),
                    context,
                    &project_id,
                )?;
                let now: String = transaction
                    .query_row(
                        "SELECT strftime('%Y-%m-%dT%H:%M:%S.000000Z','now')",
                        [],
                        |r| r.get(0),
                    )
                    .map_err(|e| e.to_string())?;
                crate::game_writer::capture_domain(
                    &transaction,
                    &event_id,
                    &event_type,
                    &project_id,
                    stage_id.as_deref(),
                    progress_id.as_deref(),
                    delta.unwrap_or(0.0),
                    &raw_state,
                    &state,
                    context,
                    &now,
                )?;
            }
            transaction.execute(
                "UPDATE game_state SET schema_version=?1,payload_json=?2,updated_at=strftime('%Y-%m-%dT%H:%M:%SZ','now') WHERE id=1",
                params![STATE_VERSION, state.to_string()],
            ).map_err(|error| error.to_string())?;
            transaction.execute(
                "UPDATE domain_events SET processed_at=strftime('%Y-%m-%dT%H:%M:%SZ','now'),status='processed',last_error=NULL WHERE event_id=?1 AND processed_at IS NULL",
                [&event_id],
            ).map_err(|error| error.to_string())?;
            Ok(())
        })();
        if result.is_err() {
            transaction
                .execute_batch("ROLLBACK TO game_domain_action")
                .map_err(|error| error.to_string())?;
        }
        transaction
            .execute_batch("RELEASE game_domain_action")
            .map_err(|error| error.to_string())?;
        match result {
            Ok(()) => summary.processed += 1,
            Err(error) => {
                let next_attempt = attempts + 1;
                let poison = next_attempt >= 3;
                transaction.execute(
                    "UPDATE domain_events SET attempt_count=?1,last_error=?2,failed_at=CASE WHEN ?3 THEN strftime('%Y-%m-%dT%H:%M:%SZ','now') ELSE failed_at END,status=CASE WHEN ?3 THEN 'failed' ELSE 'pending' END WHERE event_id=?4",
                    params![next_attempt, error.chars().take(1000).collect::<String>(), poison as i64, event_id],
                ).map_err(|db_error| db_error.to_string())?;
                summary.failed += 1;
            }
        }
    }
    transaction.commit().map_err(|error| error.to_string())?;
    Ok(summary)
}

#[derive(Clone, Copy)]
struct CatalogItem {
    category: &'static str,
    key: &'static str,
    price: f64,
    level: i64,
    maximum: Option<i64>,
    usable: bool,
    buyable: bool,
    sellable: bool,
    credit_allowed: bool,
    apply_inflation: bool,
}

// Catalog keys are save-format data. Display metadata may evolve, while this
// list intentionally remains a Rust-owned, immutable catalog during F4.
const CATALOG: &[CatalogItem] = &[
    CatalogItem {
        category: "Зелья",
        key: "Микро зелье здоровья",
        price: 29.0,
        level: 1,
        maximum: None,
        usable: true,
        buyable: true,
        sellable: true,
        credit_allowed: true,
        apply_inflation: false,
    },
    CatalogItem {
        category: "Зелья",
        key: "Малое зелье здоровья",
        price: 58.0,
        level: 1,
        maximum: None,
        usable: true,
        buyable: true,
        sellable: true,
        credit_allowed: true,
        apply_inflation: false,
    },
    CatalogItem {
        category: "Зелья",
        key: "Среднее зелье здоровья",
        price: 145.0,
        level: 1,
        maximum: None,
        usable: true,
        buyable: true,
        sellable: true,
        credit_allowed: true,
        apply_inflation: false,
    },
    CatalogItem {
        category: "Зелья",
        key: "Большое зелье здоровья",
        price: 290.0,
        level: 1,
        maximum: None,
        usable: true,
        buyable: true,
        sellable: true,
        credit_allowed: true,
        apply_inflation: false,
    },
    CatalogItem {
        category: "Зелья",
        key: "Зелье воскрешения",
        price: 580.0,
        level: 3,
        maximum: None,
        usable: true,
        buyable: true,
        sellable: true,
        credit_allowed: true,
        apply_inflation: false,
    },
    CatalogItem {
        category: "Зелья",
        key: "Зелье вдохновения",
        price: 217.5,
        level: 2,
        maximum: None,
        usable: true,
        buyable: true,
        sellable: true,
        credit_allowed: true,
        apply_inflation: false,
    },
    CatalogItem {
        category: "Зелья",
        key: "Искра вдохновения",
        price: 101.5,
        level: 1,
        maximum: Some(10),
        usable: true,
        buyable: true,
        sellable: true,
        credit_allowed: true,
        apply_inflation: false,
    },
    CatalogItem {
        category: "Зелья",
        key: "Большое зелье вдохновения",
        price: 507.5,
        level: 5,
        maximum: None,
        usable: true,
        buyable: true,
        sellable: true,
        credit_allowed: true,
        apply_inflation: false,
    },
    CatalogItem {
        category: "Зелья",
        key: "Эликсир вдохновения",
        price: 942.5,
        level: 8,
        maximum: Some(3),
        usable: true,
        buyable: true,
        sellable: true,
        credit_allowed: true,
        apply_inflation: false,
    },
    CatalogItem {
        category: "Зелья",
        key: "Часовое зелье познания",
        price: 290.0,
        level: 2,
        maximum: None,
        usable: true,
        buyable: true,
        sellable: true,
        credit_allowed: true,
        apply_inflation: false,
    },
    CatalogItem {
        category: "Зелья",
        key: "Суточное зелье познания",
        price: 3480.0,
        level: 4,
        maximum: None,
        usable: true,
        buyable: true,
        sellable: true,
        credit_allowed: true,
        apply_inflation: false,
    },
    CatalogItem {
        category: "Зелья",
        key: "Недельное зелье познания",
        price: 15660.0,
        level: 6,
        maximum: None,
        usable: true,
        buyable: true,
        sellable: true,
        credit_allowed: true,
        apply_inflation: false,
    },
    CatalogItem {
        category: "Зелья",
        key: "Часовое зелье доходности",
        price: 3185.7,
        level: 2,
        maximum: None,
        usable: true,
        buyable: true,
        sellable: true,
        credit_allowed: true,
        apply_inflation: false,
    },
    CatalogItem {
        category: "Зелья",
        key: "Суточное зелье доходности",
        price: 4411.0,
        level: 4,
        maximum: None,
        usable: true,
        buyable: true,
        sellable: true,
        credit_allowed: true,
        apply_inflation: false,
    },
    CatalogItem {
        category: "Зелья",
        key: "Недельное зелье доходности",
        price: 29759.1,
        level: 6,
        maximum: None,
        usable: true,
        buyable: true,
        sellable: true,
        credit_allowed: true,
        apply_inflation: false,
    },
    CatalogItem {
        category: "Зелья",
        key: "Часовое зелье просвещения",
        price: 2900.0,
        level: 8,
        maximum: None,
        usable: true,
        buyable: true,
        sellable: true,
        credit_allowed: true,
        apply_inflation: false,
    },
    CatalogItem {
        category: "Зелья",
        key: "Суточное зелье просвещения",
        price: 34800.0,
        level: 10,
        maximum: None,
        usable: true,
        buyable: true,
        sellable: true,
        credit_allowed: true,
        apply_inflation: false,
    },
    CatalogItem {
        category: "Зелья",
        key: "Недельное зелье просвещения",
        price: 156600.0,
        level: 12,
        maximum: None,
        usable: true,
        buyable: true,
        sellable: true,
        credit_allowed: true,
        apply_inflation: false,
    },
    CatalogItem {
        category: "Зелья",
        key: "Часовое зелье супердоходности",
        price: 63713.2,
        level: 15,
        maximum: None,
        usable: true,
        buyable: true,
        sellable: true,
        credit_allowed: true,
        apply_inflation: false,
    },
    CatalogItem {
        category: "Зелья",
        key: "Суточное зелье супердоходности",
        price: 88218.3,
        level: 18,
        maximum: None,
        usable: true,
        buyable: true,
        sellable: true,
        credit_allowed: true,
        apply_inflation: false,
    },
    CatalogItem {
        category: "Зелья",
        key: "Недельное зелье супердоходности",
        price: 595180.3,
        level: 21,
        maximum: None,
        usable: true,
        buyable: true,
        sellable: true,
        credit_allowed: true,
        apply_inflation: false,
    },
    CatalogItem {
        category: "Предметы",
        key: "Заморозка",
        price: 85074.3,
        level: 3,
        maximum: Some(2),
        usable: true,
        buyable: true,
        sellable: true,
        credit_allowed: true,
        apply_inflation: false,
    },
    CatalogItem {
        category: "Предметы",
        key: "Чернильница потока",
        price: 435.0,
        level: 3,
        maximum: None,
        usable: true,
        buyable: true,
        sellable: true,
        credit_allowed: true,
        apply_inflation: false,
    },
    CatalogItem {
        category: "Предметы",
        key: "Компас рукописи",
        price: 652.5,
        level: 4,
        maximum: None,
        usable: true,
        buyable: true,
        sellable: true,
        credit_allowed: true,
        apply_inflation: false,
    },
    CatalogItem {
        category: "Предметы",
        key: "Учебник мастерства",
        price: 696.0,
        level: 4,
        maximum: Some(5),
        usable: true,
        buyable: true,
        sellable: true,
        credit_allowed: true,
        apply_inflation: false,
    },
    CatalogItem {
        category: "Предметы",
        key: "Жетон новой цели",
        price: 261.0,
        level: 2,
        maximum: Some(3),
        usable: true,
        buyable: true,
        sellable: true,
        credit_allowed: true,
        apply_inflation: false,
    },
    CatalogItem {
        category: "Предметы",
        key: "Нить ритуала",
        price: 522.0,
        level: 3,
        maximum: Some(3),
        usable: true,
        buyable: true,
        sellable: true,
        credit_allowed: true,
        apply_inflation: false,
    },
    CatalogItem {
        category: "Предметы",
        key: "Медаль качества",
        price: 754.0,
        level: 5,
        maximum: Some(2),
        usable: true,
        buyable: true,
        sellable: true,
        credit_allowed: true,
        apply_inflation: false,
    },
    CatalogItem {
        category: "Предметы",
        key: "Лотерейный билет",
        price: 29.0,
        level: 3,
        maximum: None,
        usable: true,
        buyable: true,
        sellable: true,
        credit_allowed: false,
        apply_inflation: false,
    },
    CatalogItem {
        category: "Предметы",
        key: "Печатная машинка Хемингуэя",
        price: 14500.0,
        level: 5,
        maximum: Some(1),
        usable: false,
        buyable: true,
        sellable: true,
        credit_allowed: true,
        apply_inflation: false,
    },
    CatalogItem {
        category: "Предметы",
        key: "Ноутбук Роалинг",
        price: 72500.0,
        level: 10,
        maximum: Some(1),
        usable: false,
        buyable: true,
        sellable: true,
        credit_allowed: true,
        apply_inflation: false,
    },
    CatalogItem {
        category: "Предметы",
        key: "Литературный раб",
        price: 145000.0,
        level: 20,
        maximum: None,
        usable: false,
        buyable: true,
        sellable: true,
        credit_allowed: true,
        apply_inflation: false,
    },
    CatalogItem {
        category: "Предметы",
        key: "Амулет восстановления",
        price: 29000.0,
        level: 10,
        maximum: Some(1),
        usable: false,
        buyable: true,
        sellable: true,
        credit_allowed: true,
        apply_inflation: false,
    },
    CatalogItem {
        category: "Парфюм",
        key: "First Cherry",
        price: 50000.0,
        level: 1,
        maximum: Some(1),
        usable: false,
        buyable: true,
        sellable: true,
        credit_allowed: true,
        apply_inflation: true,
    },
    CatalogItem {
        category: "Парфюм",
        key: "My Dance",
        price: 75000.0,
        level: 1,
        maximum: Some(1),
        usable: false,
        buyable: true,
        sellable: true,
        credit_allowed: true,
        apply_inflation: true,
    },
    CatalogItem {
        category: "Парфюм",
        key: "Marocco&Bejuin",
        price: 125000.0,
        level: 1,
        maximum: Some(1),
        usable: false,
        buyable: true,
        sellable: true,
        credit_allowed: true,
        apply_inflation: true,
    },
];

fn game_object(value: &mut Value) -> GameResult<&mut Map<String, Value>> {
    value
        .as_object_mut()
        .ok_or_else(|| GameError::InvalidState("Game state is not an object".into()))
}

fn gamer_object(root: &mut Value) -> GameResult<&mut Map<String, Value>> {
    let root = game_object(root)?;
    let gamer = root
        .entry("gamer")
        .or_insert_with(|| Value::Object(Map::new()));
    game_object(gamer)
}

fn number_field(object: &Map<String, Value>, key: &str, default: f64) -> f64 {
    object
        .get(key)
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
        .unwrap_or(default)
}

fn integer_field(object: &Map<String, Value>, key: &str, default: i64) -> i64 {
    number_field(object, key, default as f64).round() as i64
}

const MAX_DEVELOPER_STREAK_LENGTH: i64 = 10_000;

fn developer_streak_key(
    kind: &str,
    project_id: Option<&str>,
    stage_id: Option<&str>,
) -> GameResult<Option<String>> {
    match kind {
        "global" if project_id.is_none() && stage_id.is_none() => Ok(None),
        "project" if project_id.is_some() && stage_id.is_none() => {
            Ok(Some(format!("project:{}", project_id.unwrap())))
        }
        "stage" if project_id.is_some() && stage_id.is_some() => Ok(Some(format!(
            "stage:{}:{}",
            project_id.unwrap(),
            stage_id.unwrap()
        ))),
        _ => Err(GameError::Validation("Некорректная цель стрика.".into())),
    }
}

fn tagged_streak_day(day: i64) -> Value {
    json!({"__type__": "date", "value": crate::streaks::date_from_days(day)})
}

fn streak_series_ending_at(last_day: i64, length: i64) -> Vec<Value> {
    ((last_day - length + 1)..=last_day)
        .map(tagged_streak_day)
        .collect()
}

fn current_streak_length(fields: &Map<String, Value>, history_key: &str) -> i64 {
    crate::streaks::streak_summary(fields.get(history_key)).1 as i64
}

fn restore_streak_length(
    fields: &Map<String, Value>,
    history_key: &str,
    loss_key: &str,
) -> GameResult<i64> {
    let current = current_streak_length(fields, history_key);
    if current > 0 {
        return Ok(current);
    }
    let lost = integer_field(fields, loss_key, 0);
    if lost > 0 {
        return Ok(lost);
    }
    Err(GameError::Validation(
        "Невозможно восстановить стрик: нет сохранённой длины.".into(),
    ))
}

fn rounded_money(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

// ``_price`` remains the persisted base price. A missing opt-in flag keeps
// prices of legacy rewards stable at their already saved value.
fn custom_award_base_price(fields: &Map<String, Value>) -> f64 {
    number_field(fields, "_price", 0.0).max(0.0)
}

fn custom_award_uses_inflation(fields: &Map<String, Value>) -> bool {
    fields.get("apply_inflation").and_then(Value::as_bool) == Some(true)
}

fn custom_award_price(gamer: &Map<String, Value>, fields: &Map<String, Value>) -> f64 {
    let multiplier = if custom_award_uses_inflation(fields) {
        1.0 + (integer_field(gamer, "level", 1).max(1) - 1) as f64 * 0.15
    } else {
        1.0
    };
    rounded_money(custom_award_base_price(fields) * multiplier)
}

fn checked_positive(value: f64, label: &str) -> GameResult<f64> {
    if value.is_finite() && value > 0.0 && value <= MAX_MONEY {
        Ok(rounded_money(value))
    } else {
        Err(GameError::Validation(format!(
            "{label} must be finite, positive and bounded"
        )))
    }
}

fn checked_nonnegative(value: f64, label: &str) -> GameResult<f64> {
    if value.is_finite() && value >= 0.0 && value <= MAX_MONEY {
        Ok(rounded_money(value))
    } else {
        Err(GameError::Validation(format!(
            "{label} must be finite, non-negative and bounded"
        )))
    }
}

fn checked_count(value: i64) -> GameResult<i64> {
    if (1..=MAX_ITEM_COUNT).contains(&value) {
        Ok(value)
    } else {
        Err(GameError::InvalidQuantity(
            "Количество должно быть от 1 до 10000.".into(),
        ))
    }
}

fn checked_inventory_total(
    current: i64,
    requested: i64,
    item_maximum: Option<i64>,
) -> GameResult<i64> {
    let total = current
        .checked_add(requested)
        .filter(|total| *total <= MAX_ITEM_COUNT)
        .ok_or_else(|| {
            GameError::InvalidQuantity("Достигнут лимит предмета в инвентаре.".into())
        })?;
    if item_maximum.is_some_and(|maximum| total > maximum) {
        return Err(GameError::InvalidQuantity(
            "Достигнут лимит предмета в инвентаре.".into(),
        ));
    }
    Ok(total)
}

fn object_map<'a>(object: &'a mut Map<String, Value>, key: &str) -> &'a mut Map<String, Value> {
    object
        .entry(key.to_string())
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .expect("new object")
}

fn item_map<'a>(gamer: &'a mut Map<String, Value>, category: &str) -> &'a mut Map<String, Value> {
    object_map(object_map(gamer, "items"), category)
}

fn item_count(gamer: &Map<String, Value>, category: &str, key: &str) -> i64 {
    gamer
        .get("items")
        .and_then(Value::as_object)
        .and_then(|items| items.get(category))
        .and_then(Value::as_object)
        .and_then(|items| items.get(key))
        .and_then(Value::as_i64)
        .unwrap_or(0)
        .max(0)
}

fn tagged_fields<'a>(value: &'a mut Value) -> Option<&'a mut Map<String, Value>> {
    value.as_object_mut()?.get_mut("fields")?.as_object_mut()
}

fn tagged_field<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    value.as_object()?.get("fields")?.as_object()?.get(key)
}

fn tagged_fields_immutable(value: &Value) -> Option<&Map<String, Value>> {
    value.as_object()?.get("fields")?.as_object()
}

fn text_value(value: Option<&Value>, default: &str) -> String {
    value
        .and_then(|value| {
            value
                .as_str()
                .or_else(|| value.get("value").and_then(Value::as_str))
        })
        .unwrap_or(default)
        .to_string()
}

fn optional_field_text(object: &Map<String, Value>, key: &str) -> Option<String> {
    object
        .get(key)
        .filter(|value| !value.is_null())
        .map(|value| text_value(Some(value), ""))
}

fn normalized_item_identifier(value: &str) -> String {
    value
        .replace('👑', "")
        .replace('💎', "")
        .replace("❄️", "")
        .replace('💸', "")
        .replace('🧪', "")
        .replace("⭐️", "")
        .replace('⭐', "")
        .to_lowercase()
        .trim()
        .to_string()
}

fn item_identifier_matches(value: &str, canonical: &str) -> bool {
    value == canonical || normalized_item_identifier(value) == normalized_item_identifier(canonical)
}

fn iso_epoch_seconds(value: &str) -> Option<i64> {
    let date_time = value.get(..19)?;
    let year = date_time.get(0..4)?.parse::<i64>().ok()?;
    let month = date_time.get(5..7)?.parse::<i64>().ok()?;
    let day = date_time.get(8..10)?.parse::<i64>().ok()?;
    let hour = date_time.get(11..13)?.parse::<i64>().ok()?;
    let minute = date_time.get(14..16)?.parse::<i64>().ok()?;
    let second = date_time.get(17..19)?.parse::<i64>().ok()?;
    let adjusted_year = year - i64::from(month <= 2);
    let era = (if adjusted_year >= 0 {
        adjusted_year
    } else {
        adjusted_year - 399
    }) / 400;
    let year_of_era = adjusted_year - era * 400;
    let month_prime = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * month_prime + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146097 + day_of_era - 719468;
    Some(days * 86_400 + hour * 3_600 + minute * 60 + second)
}

fn iso_epoch_millis(value: &str) -> Option<i64> {
    let seconds = iso_epoch_seconds(value)?;
    let mut fraction_millis = 0;
    let mut fraction_digits = 0;
    if let Some(fraction) = value.get(19..).and_then(|tail| tail.strip_prefix('.')) {
        for digit in fraction.bytes().take(3) {
            if !digit.is_ascii_digit() {
                break;
            }
            fraction_millis = fraction_millis * 10 + i64::from(digit - b'0');
            fraction_digits += 1;
        }
    }
    if fraction_digits == 1 {
        fraction_millis *= 100;
    } else if fraction_digits == 2 {
        fraction_millis *= 10;
    }
    Some(seconds * 1_000 + fraction_millis)
}

fn iso_date_from_days(days: i64) -> String {
    let z = days + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    let year = year + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

fn iso_timestamp_from_epoch(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400);
    let day_seconds = seconds.rem_euclid(86_400);
    format!(
        "{}T{:02}:{:02}:{:02}Z",
        iso_date_from_days(days),
        day_seconds / 3_600,
        day_seconds / 60 % 60,
        day_seconds % 60
    )
}

fn session_projection(value: Option<&Value>, now: &str) -> Value {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Value::Null;
    };
    let Some(object) = value.as_object() else {
        return Value::Null;
    };
    let started_at = text_value(object.get("started_at"), "");
    let duration = integer_field(object, "duration_minutes", 0).max(0);
    let started_epoch = iso_epoch_seconds(&started_at);
    let remaining = started_epoch
        .zip(iso_epoch_seconds(now))
        .map(|(started, current)| (started + duration * 60 - current).max(0))
        .unwrap_or(duration * 60);
    json!({
        "started_at": if started_at.is_empty() { Value::Null } else { json!(started_at) },
        "ends_at": started_epoch
            .map(|started| iso_timestamp_from_epoch(started + duration * 60))
            .map_or(Value::Null, Value::String),
        "duration_minutes": duration,
        "target_symbols": integer_field(object, "target_symbols", 0).max(0),
        "progress": number_field(object, "progress", 0.0).max(0.0),
        "intention": text_value(object.get("intention"), ""),
        "mode": text_value(object.get("mode"), "flow"),
        "remaining_seconds": remaining,
    })
}

fn bank_projection(gamer: &Map<String, Value>, level: i64) -> Value {
    let account = gamer.get("bank_account").and_then(tagged_fields_immutable);
    let credit = account
        .and_then(|fields| fields.get("credit"))
        .filter(|value| !value.is_null())
        .and_then(tagged_fields_immutable)
        .map(|fields| {
            let principal = number_field(fields, "credit_sum", 0.0).max(0.0);
            let paid = number_field(fields, "paid_amount", 0.0).max(0.0);
            let rate = number_field(fields, "interest_rate_on_loan", 2.0).max(0.0);
            let days = integer_field(fields, "days_until_return", 1).max(1);
            let daily_rate: f64 = rate / 100.0;
            let interest = if daily_rate == 0.0 {
                0.0
            } else {
                let growth = (1.0 + daily_rate).powi(days as i32);
                rounded_money(
                    (rounded_money(principal * daily_rate * growth / (growth - 1.0))
                        * days as f64)
                        - principal,
                )
            };
            let total = rounded_money(principal + interest);
            json!({"principal":principal,"interest_rate":rate,"interest":interest,"total":total,"remaining":(total-paid).max(0.0),"daily_payment":rounded_money((total/days as f64).max(0.0)),"status":"OK","opened_at":fields.get("take_date").map(|value| text_value(Some(value), "")),"return_date":Value::Null,"paid_amount":paid,"overdue_days":integer_field(fields,"total_overdue_days",0).max(0)})
        });
    let deposit = account
        .and_then(|fields| fields.get("deposit"))
        .filter(|value| !value.is_null())
        .and_then(tagged_fields_immutable)
        .map(|fields| {
            let principal = number_field(fields, "deposit_sum", 0.0).max(0.0);
            let rate = number_field(fields, "interest_rate_on_deposit", 1.0).max(0.0);
            let days = integer_field(fields, "days_until_return", 1).max(1);
            let interest = rounded_money(principal * ((1.0 + rate / 100.0).powi(days as i32) - 1.0));
            let available = number_field(fields, "interest", interest).max(0.0);
            json!({"principal":principal,"interest_rate":rate,"interest":interest,"total":rounded_money(principal+interest),"available_interest":available,"allow_interest_withdrawal":fields.get("allow_interest_withdrawal").and_then(Value::as_bool).unwrap_or(true),"status":"OK","opened_at":fields.get("give_date").map(|value| text_value(Some(value), "")),"return_date":Value::Null})
        });
    let credit_score = account
        .and_then(|fields| fields.get("credit_score"))
        .and_then(Value::as_i64)
        .unwrap_or(600);
    json!({"credit_score":credit_score,"credit_limit":0,"max_credit_days":36500,"credit_rate":2,"deposit_rate":1,"can_open_credit":level>=3 && credit.is_none(),"can_open_deposit":deposit.is_none() && number_field(gamer,"coins",0.0)>0.0,"credit":credit,"deposit":deposit,"credit_history_count":0,"deposit_history_count":0,"overdue_days_total":0})
}

fn skills_projection(gamer: &Map<String, Value>) -> Value {
    let names = [
        ("productivity", "Продуктивность", "exp"),
        ("profitability", "Доходность", "coins"),
        ("endurance", "Выносливость", "health_recovery"),
    ];
    let skills = gamer.get("skills").and_then(Value::as_object);
    let coefficients = gamer.get("cf").and_then(Value::as_object);
    let items = names
        .iter()
        .map(|(key, name, target)| {
            let points = skills
                .and_then(|values| values.get(*key))
                .and_then(Value::as_i64)
                .unwrap_or(0)
                .max(0);
            json!({"key":key,"name":name,"points":points,"target":target,"bonus":points as f64 * 0.25})
        })
        .collect::<Vec<_>>();
    let coefficient_items = ["coins", "exp", "health_recovery"]
        .iter()
        .map(|key| {
            let value = coefficients
                .and_then(|values| values.get(*key))
                .and_then(|value| value.get("value").or(Some(value)))
                .and_then(Value::as_f64)
                .unwrap_or(if *key == "health_recovery" { 0.0 } else { 1.0 });
            json!({"key":key,"name":key,"description":"","value":value,"base_value":value})
        })
        .collect::<Vec<_>>();
    json!({"available_points":integer_field(gamer,"available_skill_points",0).max(0),"points_per_level":2,"items":items,"coefficients":coefficient_items})
}

fn quest_projection(gamer: &Map<String, Value>) -> Value {
    let mut items = Vec::new();
    if let Some(quests) = gamer.get("quests").and_then(Value::as_array) {
        for (index, quest) in quests.iter().enumerate() {
            let fields = tagged_fields_immutable(quest).or_else(|| quest.as_object());
            let Some(fields) = fields else { continue };
            let id = text_value(fields.get("quest_id"), &format!("quest-{index}"));
            let status = text_value(fields.get("status"), "available").to_lowercase();
            items.push(json!({"id":id,"name":text_value(fields.get("name"),&id),"description":text_value(fields.get("description"),""),"status":status,"required_level":integer_field(fields,"level",1).max(1),"started_at":fields.get("start_date").map(|value| text_value(Some(value), "")),"finished_at":fields.get("end_date").map(|value| text_value(Some(value), "")),"reward":{"coins":number_field(fields,"reward_coins",0.0).max(0.0),"experience":number_field(fields,"reward_exp",0.0).max(0.0),"items":fields.get("reward_items").cloned().unwrap_or(json!([])),"buffs":fields.get("reward_buffs").cloned().unwrap_or(json!([]))}}));
        }
    }
    let mut by_status = Map::new();
    for status in ["available", "active", "completed"] {
        by_status.insert(
            status.into(),
            Value::Array(
                items
                    .iter()
                    .filter(|item| item["status"].as_str() == Some(status))
                    .cloned()
                    .collect(),
            ),
        );
    }
    json!({"items":items,"by_status":by_status})
}

fn custom_awards_projection(gamer: &Map<String, Value>) -> Value {
    let inventory = gamer
        .get("custom_awards_inventory")
        .and_then(Value::as_object);
    let items = gamer
        .get("custom_awards")
        .and_then(Value::as_array)
        .into_iter()
        .flat_map(|values| values.iter())
        .enumerate()
        .filter_map(|(index, award)| {
            let fields = tagged_fields_immutable(award)?;
            let id = text_value(fields.get("award_id"), &format!("custom-{index}"));
            let name = text_value(fields.get("name"), "Награда");
            let base_price = custom_award_base_price(fields);
            let apply_inflation = custom_award_uses_inflation(fields);
            let price = custom_award_price(gamer, fields);
            let count = inventory
                .and_then(|values| values.get(&name))
                .and_then(Value::as_i64)
                .unwrap_or(0)
                .max(0);
            Some(json!({"id":id,"name":name,"description":text_value(fields.get("description"),"Кастомная награда без эффекта"),"base_price":base_price,"apply_inflation":apply_inflation,"price":price,"sell_price":rounded_money(price*0.75),"count":count,"available_in_shop":fields.get("available_in_shop").and_then(Value::as_bool).unwrap_or(true),"sellable":fields.get("sellable").and_then(Value::as_bool).unwrap_or(true),"usable":true,"can_buy":number_field(gamer,"coins",0.0)>=price}))
        })
        .collect::<Vec<_>>();
    json!({"items":items})
}

#[derive(Clone)]
struct ProjectedBuff {
    name: String,
    description: String,
    buff_type: String,
    target: String,
    value: f64,
    stacks: i64,
    duration_minutes: Option<i64>,
    started_at: Option<String>,
    expires_at: Option<String>,
    remaining_seconds: Option<i64>,
    source: Option<String>,
    stackable: bool,
}

impl ProjectedBuff {
    fn into_value(self) -> Value {
        json!({
            "name": self.name,
            "description": self.description,
            "type": self.buff_type,
            "target": self.target,
            "value": self.value,
            "stacks": self.stacks,
            "duration_minutes": self.duration_minutes,
            "started_at": self.started_at,
            "expires_at": self.expires_at,
            "remaining_seconds": self.remaining_seconds,
            "source": self.source,
            "stackable": self.stackable,
        })
    }
}

fn effect_fields(value: &Value) -> Option<&Map<String, Value>> {
    tagged_fields_immutable(value).or_else(|| value.as_object())
}

fn project_saved_buff(value: &Value, now: &str) -> Option<ProjectedBuff> {
    let fields = effect_fields(value)?;
    let expires_at = optional_field_text(fields, "end_time");
    let remaining_seconds = match expires_at.as_deref() {
        Some(expires_at) => {
            let expiration = iso_epoch_millis(expires_at)?;
            let current = iso_epoch_millis(now)?;
            let remaining_millis = (expiration - current).max(0);
            if remaining_millis == 0 {
                return None;
            }
            Some((remaining_millis + 999) / 1_000)
        }
        None => None,
    };

    Some(ProjectedBuff {
        name: text_value(fields.get("name"), "Эффект"),
        description: text_value(fields.get("description"), ""),
        buff_type: text_value(fields.get("buff_type"), "positive"),
        target: text_value(fields.get("target_cf"), ""),
        value: number_field(fields, "value", 0.0),
        stacks: 1,
        duration_minutes: fields
            .get("duration_minutes")
            .filter(|value| !value.is_null())
            .map(|_| integer_field(fields, "duration_minutes", 0).max(0)),
        started_at: optional_field_text(fields, "start_time"),
        expires_at,
        remaining_seconds,
        source: optional_field_text(fields, "source"),
        stackable: fields
            .get("stackable")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

#[derive(Clone)]
struct InventoryBuffDefinition {
    name: String,
    description: String,
    buff_type: &'static str,
    target: &'static str,
    value: f64,
    stackable: bool,
}

fn inventory_buff_definition(
    name: impl Into<String>,
    description: impl Into<String>,
    target: &'static str,
    value: f64,
    stackable: bool,
) -> InventoryBuffDefinition {
    InventoryBuffDefinition {
        name: name.into(),
        description: description.into(),
        buff_type: "positive",
        target,
        value,
        stackable,
    }
}

// These are the permanent quest-award effects created by game_data.make_quest_award_buff.
// The table keeps the canonical item keys and values separate from their display text.
const QUEST_REWARD_BUFFS: &[(&str, &str, &str, f64)] = &[
    (
        "⭐️ Знак заботы о здоровье",
        "health_recovery",
        "0.025",
        0.025,
    ),
    ("⭐️ Знак дисциплины", "health_recovery", "0.025", 0.025),
    ("⭐️ Банковский жетон", "coins", "0.025", 0.025),
    ("⭐️ Знак коллекционера", "coins", "0.025", 0.025),
    ("⭐️ Знак недельной практики", "exp", "0.025", 0.025),
    ("⭐️ Знак быстрого финала", "exp", "0.025", 0.025),
    ("⭐️ Знак теплого стрика", "health_recovery", "0.025", 0.025),
    ("⭐️ Знак чистого финиша", "health_recovery", "0.025", 0.025),
    ("⭐️ Знак двух недель", "exp", "0.025", 0.025),
    ("⭐️ Знак без льда", "health_recovery", "0.025", 0.025),
    ("⭐️ Знак глубоких дней", "exp", "0.025", 0.025),
    ("⭐️ Знак трех финалов", "exp", "0.025", 0.025),
    ("⭐️ Знак большой формы", "exp", "0.025", 0.025),
    ("⭐️ Знак марафонца", "health_recovery", "0.025", 0.025),
    ("⭐️ Знак двух линий", "health_recovery", "0.025", 0.025),
    ("⭐️ Знак зала славы", "coins", "0.025", 0.025),
    ("⭐️ Знак трех фронтов", "exp", "0.025", 0.025),
    ("⭐️ Знак короткой победы", "exp", "0.025", 0.025),
    ("⭐️ Знак пяти сильных дней", "exp", "0.025", 0.025),
    ("⭐️ Знак дневника рукописи", "exp", "0.025", 0.025),
    ("⭐️ Знак серьезного вклада", "coins", "0.025", 0.025),
    ("⭐️ Знак умного вклада", "coins", "0.025", 0.025),
    ("⭐️ Знак пяти завершений", "exp", "0.025", 0.025),
    ("⭐️ Знак ста тысяч", "exp", "0.025", 0.025),
    ("⭐️ Знак семи рукописей", "exp", "0.05", 0.05),
    ("⭐️ Знак полутора месяцев", "health_recovery", "0.05", 0.05),
    ("⭐️ Знак месяца без льда", "health_recovery", "0.05", 0.05),
    ("⭐️ Знак десяти дней", "exp", "0.05", 0.05),
    ("⭐️ Знак четверти миллиона", "exp", "0.075", 0.075),
    ("⭐️ Знак недели мастера", "coins", "0.05", 0.05),
    ("⭐️ Знак половины миллиона", "exp", "0.1", 0.1),
    ("⭐️ Знак пятнадцати завершений", "exp", "0.075", 0.075),
    ("⭐️ Знак завершённого романа", "exp", "0.1", 0.1),
    ("⭐️ Знак легенды мастерства", "coins", "0.1", 0.1),
    (
        "⭐️ Знак глобальной недели",
        "health_recovery",
        "0.025",
        0.025,
    ),
    (
        "⭐️ Знак глобальных двух недель",
        "health_recovery",
        "0.025",
        0.025,
    ),
    (
        "⭐️ Знак глобальной привычки",
        "health_recovery",
        "0.025",
        0.025,
    ),
    (
        "⭐️ Знак глобального месяца",
        "health_recovery",
        "0.025",
        0.025,
    ),
    (
        "⭐️ Знак чистого глобального месяца",
        "health_recovery",
        "0.025",
        0.025,
    ),
    (
        "⭐️ Знак глобального сезона",
        "health_recovery",
        "0.025",
        0.025,
    ),
    (
        "⭐️ Знак глобального квартала",
        "health_recovery",
        "0.025",
        0.025,
    ),
    (
        "⭐️ Знак глобального полугодия",
        "health_recovery",
        "0.025",
        0.025,
    ),
    (
        "⭐️ Знак глобального года",
        "health_recovery",
        "0.025",
        0.025,
    ),
];

fn quest_reward_inventory_buff(item_key: &str) -> Option<InventoryBuffDefinition> {
    let (_, target, value_text, value) = QUEST_REWARD_BUFFS
        .iter()
        .find(|(canonical, _, _, _)| item_identifier_matches(item_key, canonical))?;
    let (name, prefix) = match *target {
        "exp" => (
            "⭐️ Квестовая специализация: опыт",
            "Постоянный бонус к коэффициенту опыта от квестовых наград.",
        ),
        "coins" => (
            "⭐️ Квестовая специализация: монеты",
            "Постоянный бонус к коэффициенту монет от квестовых наград.",
        ),
        "health_recovery" => (
            "⭐️ Квестовая специализация: восстановление",
            "Постоянный бонус к восстановлению здоровья от квестовых наград.",
        ),
        _ => return None,
    };
    Some(inventory_buff_definition(
        name,
        format!("{prefix} +{value_text} к параметру."),
        target,
        *value,
        true,
    ))
}

fn inventory_buff_definitions(category: &str, item_key: &str) -> Vec<InventoryBuffDefinition> {
    if category == "Предметы" {
        if item_identifier_matches(item_key, "Печатная машинка Хемингуэя") {
            return vec![inventory_buff_definition(
                "📠 Почерк Хемингуэя",
                "Постоянный бонус к коэффициенту опыта.",
                "exp",
                0.5,
                false,
            )];
        }
        if item_identifier_matches(item_key, "Ноутбук Роалинг") {
            return vec![inventory_buff_definition(
                "💻 Вдохновение Роалинг",
                "Постоянный бонус к коэффициенту опыта.",
                "exp",
                1.0,
                false,
            )];
        }
        if item_identifier_matches(item_key, "Литературный раб") {
            return vec![
                inventory_buff_definition(
                    "📃 Литературная поддержка",
                    "Постоянный бонус к коэффициенту опыта.",
                    "exp",
                    0.25,
                    false,
                ),
                inventory_buff_definition(
                    "📃 Литературная поддержка дохода",
                    "Постоянный бонус к коэффициенту монет.",
                    "coins",
                    0.25,
                    false,
                ),
            ];
        }
        if item_identifier_matches(item_key, "Амулет восстановления") {
            return vec![inventory_buff_definition(
                "❤️ Исцеление амулетом",
                "Постоянный бонус к коэффициенту восстановления здоровья.",
                "health_recovery",
                1.0,
                false,
            )];
        }
    }

    if category == "Парфюм" {
        if item_identifier_matches(item_key, "First Cherry") {
            return vec![inventory_buff_definition(
                "🍒 Почерк вишни",
                "Постоянный бонус к коэффициенту опыта.",
                "exp",
                1.0,
                false,
            )];
        }
        if item_identifier_matches(item_key, "My Dance") {
            return vec![inventory_buff_definition(
                "💃 Ритм танца",
                "Постоянный бонус к коэффициенту восстановления здоровья.",
                "health_recovery",
                1.0,
                false,
            )];
        }
        if item_identifier_matches(item_key, "Marocco&Bejuin") {
            return vec![inventory_buff_definition(
                "🍂 Восточный шлейф",
                "Постоянный бонус к коэффициенту монет.",
                "coins",
                1.0,
                false,
            )];
        }
    }

    if category == "Награды" {
        if item_identifier_matches(item_key, "👑  Корона Первой Эпохи") {
            return vec![inventory_buff_definition(
                "👑 Опыт миллионера",
                "+1 к коэффициенту опыта",
                "exp",
                1.0,
                false,
            )];
        }
        if item_identifier_matches(item_key, "💎  Перо Миллионера") {
            return vec![inventory_buff_definition(
                "💎 Удача миллионера",
                "+1 к коэффициенту заработка",
                "coins",
                1.0,
                false,
            )];
        }
        if let Some(buff) = quest_reward_inventory_buff(item_key) {
            return vec![buff];
        }
    }

    Vec::new()
}

fn append_inventory_buff(
    buffs: &mut Vec<ProjectedBuff>,
    merged_buffs: &mut Vec<ProjectedBuff>,
    definition: InventoryBuffDefinition,
    stacks: i64,
    source: String,
) {
    let stacks = stacks.max(1);
    if definition.stackable {
        if let Some(existing) = merged_buffs.iter_mut().find(|buff| {
            buff.name == definition.name
                && buff.target == definition.target
                && buff.buff_type == definition.buff_type
        }) {
            let previous_stacks = existing.stacks.max(1);
            let total_stacks = previous_stacks.saturating_add(stacks);
            existing.value = (existing.value * previous_stacks as f64
                + definition.value * stacks as f64)
                / total_stacks as f64;
            existing.stacks = total_stacks;
            return;
        }
    }

    let projected = ProjectedBuff {
        name: definition.name,
        description: definition.description,
        buff_type: definition.buff_type.to_string(),
        target: definition.target.to_string(),
        value: definition.value,
        stacks,
        duration_minutes: None,
        started_at: None,
        expires_at: None,
        remaining_seconds: None,
        source: Some(source),
        stackable: definition.stackable,
    };
    if definition.stackable {
        merged_buffs.push(projected);
    } else {
        buffs.push(projected);
    }
}

fn buffs_projection(gamer: &Map<String, Value>, now: &str) -> Value {
    let mut positive = Vec::new();
    let mut negative = Vec::new();
    let mut inventory_positive = Vec::new();
    let mut inventory_negative = Vec::new();
    let mut merged_positive = Vec::new();
    let mut merged_negative = Vec::new();

    if let Some(buffs) = gamer.get("buffs").and_then(Value::as_array) {
        for buff in buffs {
            if let Some(projected) = project_saved_buff(buff, now) {
                positive.push(projected);
            }
        }
    }
    if let Some(debuffs) = gamer.get("debuffs").and_then(Value::as_array) {
        for debuff in debuffs {
            if let Some(projected) = project_saved_buff(debuff, now) {
                negative.push(projected);
            }
        }
    }

    if let Some(items) = gamer.get("items").and_then(Value::as_object) {
        for (category, values) in items {
            let Some(values) = values.as_object() else {
                continue;
            };
            for (item_key, count) in values {
                let count = count
                    .as_i64()
                    .or_else(|| count.as_f64().map(|value| value.round() as i64))
                    .unwrap_or(0);
                if count <= 0 {
                    continue;
                }
                let source = catalog_metadata(item_key).0.to_string();
                for definition in inventory_buff_definitions(category, item_key) {
                    if definition.buff_type == "negative" {
                        append_inventory_buff(
                            &mut inventory_negative,
                            &mut merged_negative,
                            definition,
                            count,
                            source.clone(),
                        );
                    } else {
                        append_inventory_buff(
                            &mut inventory_positive,
                            &mut merged_positive,
                            definition,
                            count,
                            source.clone(),
                        );
                    }
                }
            }
        }
    }

    // Legacy get_all_buffs keeps saved effects separate from the freshly
    // materialized inventory effects, even when their names are identical.
    inventory_positive.extend(merged_positive);
    inventory_negative.extend(merged_negative);
    positive.extend(inventory_positive);
    negative.extend(inventory_negative);

    json!({
        "server_time": now,
        "positive": positive.into_iter().map(ProjectedBuff::into_value).collect::<Vec<_>>(),
        "negative": negative.into_iter().map(ProjectedBuff::into_value).collect::<Vec<_>>(),
    })
}

fn catalog_item(category: &str, key: &str) -> Option<CatalogItem> {
    CATALOG
        .iter()
        .copied()
        .find(|item| item.category == category && item.key == key)
}

fn catalog_item_json(gamer: &Map<String, Value>, item: CatalogItem, include_count: bool) -> Value {
    let count = item_count(gamer, item.category, item.key);
    let (name, description, effect) = catalog_metadata(item.key);
    let price = if item.apply_inflation {
        let multiplier = 1.0 + (integer_field(gamer, "level", 1).max(1) - 1) as f64 * 0.15;
        rounded_money(item.price * multiplier)
    } else {
        item.price
    };
    let mut result = json!({
        "id": format!("{}:{}", item.category, item.key), "key": item.key,
        "category": item.category, "name": name,
        "description": description, "effect": effect,
        "price": price, "sell_price": rounded_money(price * 0.75),
        "level": item.level, "sellable": item.sellable, "usable": item.usable,
        "buy": item.buyable, "credit_allowed": item.credit_allowed,
        "maximum_quantity": item.maximum, "available_for_level": integer_field(gamer, "level", 1) >= item.level,
        "can_buy": item.buyable && integer_field(gamer, "level", 1) >= item.level
            && number_field(gamer, "coins", 0.0) >= price
            && count < MAX_ITEM_COUNT
            && item.maximum.map_or(true, |maximum| count < maximum), "known": true,
    });
    if include_count {
        result["count"] = json!(count);
    }
    result
}

// These are the canonical native display definitions copied from the legacy
// ITEM_REGISTRY. Save keys remain the emoji-free Russian keys used by Gamer.
const ITEM_METADATA: &[(&str, &str, &str, Option<&str>)] = &[
    ("Микро зелье здоровья", "🧪  Микро зелье здоровья", "🧪  Восстанавливает здоровье на 5 единиц", Some("Восстанавливает здоровье")),
    ("Малое зелье здоровья", "🧪  Малое зелье здоровья", "🧪  Восстанавливает здоровье на 10 единиц", Some("Восстанавливает здоровье")),
    ("Среднее зелье здоровья", "🧪  Среднее зелье здоровья", "🧪  Восстанавливает здоровье на 25 единиц", Some("Восстанавливает здоровье")),
    ("Большое зелье здоровья", "🧪  Большое зелье здоровья", "🧪  Восстанавливает здоровье на 50 единиц", Some("Восстанавливает здоровье")),
    ("Зелье воскрешения", "🧪  Зелье воскрешения", "🧪  Полностью восстанавливает здоровье", Some("Восстанавливает здоровье")),
    ("Зелье вдохновения", "✨ Зелье вдохновения", "Восстанавливает 25 вдохновения.", Some("Восстанавливает 25 вдохновения")),
    ("Искра вдохновения", "💫 Искра вдохновения", "Восстанавливает 10 вдохновения.", Some("Восстанавливает 25 вдохновения")),
    ("Большое зелье вдохновения", "🌟 Большое зелье вдохновения", "Восстанавливает 50 вдохновения.", Some("Восстанавливает 25 вдохновения")),
    ("Эликсир вдохновения", "🌌 Эликсир вдохновения", "Полностью восстанавливает вдохновение.", Some("Восстанавливает 25 вдохновения")),
    ("Часовое зелье познания", "🧪⚡️  Часовое зелье познания", "Увеличивает коэффициент опыта на 1 на один час", None),
    ("Суточное зелье познания", "🧪⚡️  Суточное зелье познания", "Увеличивает коэффициент опыта на 1 на один день", None),
    ("Недельное зелье познания", "🧪⚡️  Недельное зелье познания", "Увеличивает коэффициент опыта на 1 на одну неделю", None),
    ("Часовое зелье доходности", "🧪⚡️  Часовое зелье доходности", "Увеличивает коэффициент монет на 0.5 на один час", None),
    ("Суточное зелье доходности", "🧪⚡️  Суточное зелье доходности", "Увеличивает коэффициент монет на 0.5 на один день", None),
    ("Недельное зелье доходности", "🧪⚡️  Недельное зелье доходности", "Увеличивает коэффициент монет на 0.5 на одну неделю", None),
    ("Часовое зелье просвещения", "🧪⚡️  Часовое зелье просвещения", "Увеличивает коэффициент опыта на 10 на один час", None),
    ("Суточное зелье просвещения", "🧪⚡️  Суточное зелье просвещения", "Увеличивает коэффициент опыта на 10 на один день", None),
    ("Недельное зелье просвещения", "🧪⚡️  Недельное зелье просвещения", "Увеличивает коэффициент опыта на 10 на одну неделю", None),
    ("Часовое зелье супердоходности", "🧪⚡️  Часовое зелье супердоходности", "Увеличивает коэффициент монет на 10 на один час", None),
    ("Суточное зелье супердоходности", "🧪⚡️  Суточное зелье супердоходности", "Увеличивает коэффициент монет на 10 на один день", None),
    ("Недельное зелье супердоходности", "🧪⚡️  Недельное зелье супердоходности", "Увеличивает коэффициент монет на 10 на одну неделю", None),
    ("Заморозка", "❄️ Заморозка", "Заморозка позволяет пропустить один день стрика в проекте с дедлайном и активным стриком️\n⚠️ Важно: чем больше заморозок вы используете, тем дороже они становятся.\nМожно иметь не более 2 заморозок в инвентаре и купить до 2 за раз.", None),
    ("Чернильница потока", "🖋️ Чернильница потока", "Увеличивает награду за следующую успешную писательскую сессию на 25%.", Some("Увеличивает награду за следующую успешную сессию на 25%")),
    ("Компас рукописи", "🧭 Компас рукописи", "Увеличивает награду за следующие достигнутые рубежи одной рукописи на 25%.", Some("Увеличивает награду за следующие рубежи рукописи на 25%")),
    ("Учебник мастерства", "📘 Учебник мастерства", "Добавляет 2 опыта мастерства выбранной специализации.", Some("Добавляет 2 опыта мастерства выбранной специализации")),
    ("Жетон новой цели", "🎲 Жетон новой цели", "Бесплатно меняет текущую цель дня на другой вариант.", Some("Бесплатно меняет текущую цель дня на следующий вариант")),
    ("Нить ритуала", "🧵 Нить ритуала", "Один раз защищает серию успешных сессий при неудаче.", Some("Один раз сохраняет серию при неудачной сессии (максимум 3)")),
    ("Медаль качества", "🏅 Медаль качества", "Повышает результат следующей успешной сессии на одну ступень.", Some("Повышает результат следующей успешной сессии на одну ступень")),
    ("Лотерейный билет", "🎟️ Лотерейный билет", "Лотерейный билет \"5 из 30\". Угадайте числа и сорвите джекпот!", Some("Разыграйте 5 чисел из 30 и получите приз за 2 или больше совпадений")),
    ("Печатная машинка Хемингуэя", "📠  Печатная машинка Хемингуэя", "Постоянно увеличивает коэффициент опыта на 0,5.", None),
    ("Ноутбук Роалинг", "💻  Ноутбук Роалинг", "Постоянно увеличивает коэффициент опыта на 1.", None),
    ("Литературный раб", "📃  Литературный раб", "Постоянно увеличивает коэффициенты опыта и монет на 0,25 за экземпляр.", None),
    ("Амулет восстановления", "❤️  Амулет восстановления", "Постоянно увеличивает коэффициент восстановления здоровья на 1.", None),
    ("First Cherry", "First Cherry", "Сочная чёрная вишня, терпкий миндаль и тёплая ваниль сливаются в соблазнительную композицию. Аромат смелых решений, незабываемых приключений и бесценного опыта.", Some("Постоянно увеличивает коэффициент опыта на 1.")),
    ("My Dance", "My Dance", "Нежность розы, сладость спелой груши и сливочные оттенки сандала окутывают мягким теплом. Словно объятие после долгого дня, этот аромат возвращает силы и дарит ощущение гармонии.", Some("Постоянно увеличивает коэффициент восстановления здоровья на 1.")),
    ("Marocco&Bejuin", "Marocco&Bejuin", "Благородный табак, пряная ваниль, какао и сухофрукты создают роскошный восточный аромат. Его тёплый шлейф напоминает о богатстве, власти и бесконечных возможностей.", Some("Постоянно увеличивает коэффициент монет на 1.")),
];

const REWARD_METADATA: &[(&str, &str, &str)] = &[
    ("👑  Корона Первой Эпохи", "👑  Корона Первой Эпохи", "Корона выдается игрокам, которые прошли первую экономическую реформу в игре"),
    ("💎  Перо Миллионера", "💎  Перо Миллионера", "Перо выдается игрокам, которые заработали больше миллиона монет до первой экономической реформы в игре"),
    ("⭐️ Знак заботы о здоровье", "⭐️ Знак заботы о здоровье", "Награда за подготовку аптечки автора."),
    ("⭐️ Знак дисциплины", "⭐️ Знак дисциплины", "Награда за запасной день и заботу о стрике."),
    ("⭐️ Банковский жетон", "⭐️ Банковский жетон", "Награда за первый вклад в банке."),
    ("⭐️ Знак коллекционера", "⭐️ Знак коллекционера", "Награда за собранную коллекцию предметов."),
    ("⭐️ Знак недельной практики", "⭐️ Знак недельной практики", "Награда за семь дней записей подряд."),
    ("⭐️ Знак быстрого финала", "⭐️ Знак быстрого финала", "Награда за завершение текста за две недели."),
    ("⭐️ Знак теплого стрика", "⭐️ Знак теплого стрика", "Награда за неделю без заморозки."),
    ("⭐️ Знак чистого финиша", "⭐️ Знак чистого финиша", "Награда за завершение текста без заморозок."),
    ("⭐️ Знак двух недель", "⭐️ Знак двух недель", "Награда за четырнадцать дней записей подряд."),
    ("⭐️ Знак без льда", "⭐️ Знак без льда", "Награда за длительную серию без заморозок."),
    ("⭐️ Знак глубоких дней", "⭐️ Знак глубоких дней", "Награда за несколько продуктивных дней."),
    ("⭐️ Знак трех финалов", "⭐️ Знак трех финалов", "Награда за три завершенных текста."),
    ("⭐️ Знак большой формы", "⭐️ Знак большой формы", "Награда за завершение крупного текста."),
    ("⭐️ Знак марафонца", "⭐️ Знак марафонца", "Награда за длинный стрик до финиша."),
    ("⭐️ Знак двух линий", "⭐️ Знак двух линий", "Награда за параллельную дисциплину в двух текстах."),
    ("⭐️ Знак зала славы", "⭐️ Знак зала славы", "Награда за большую коллекцию наград."),
    ("⭐️ Знак трех фронтов", "⭐️ Знак трех фронтов", "Награда за три активных текста одновременно."),
    ("⭐️ Знак короткой победы", "⭐️ Знак короткой победы", "Награда за завершение короткого текста."),
    ("⭐️ Знак пяти сильных дней", "⭐️ Знак пяти сильных дней", "Награда за пять продуктивных дней."),
    ("⭐️ Знак дневника рукописи", "⭐️ Знак дневника рукописи", "Награда за регулярные записи в одном тексте."),
    ("⭐️ Знак серьезного вклада", "⭐️ Знак серьезного вклада", "Награда за крупный активный вклад."),
    ("⭐️ Знак умного вклада", "⭐️ Знак умного вклада", "Награда за вклад с полученными процентами."),
    ("⭐️ Знак пяти завершений", "⭐️ Знак пяти завершений", "Награда за пять завершенных текстов."),
    ("⭐️ Знак ста тысяч", "⭐️ Знак ста тысяч", "Награда за сто тысяч написанных символов."),
    ("⭐️ Знак семи рукописей", "⭐️ Знак семи рукописей", "Награда за семь завершенных текстов."),
    ("⭐️ Знак полутора месяцев", "⭐️ Знак полутора месяцев", "Награда за сорок пять дней глобального стрика."),
    ("⭐️ Знак месяца без льда", "⭐️ Знак месяца без льда", "Награда за месяц дисциплины без заморозок."),
    ("⭐️ Знак десяти дней", "⭐️ Знак десяти дней", "Награда за десять продуктивных дней."),
    ("⭐️ Знак четверти миллиона", "⭐️ Знак четверти миллиона", "Награда за четверть миллиона символов."),
    ("⭐️ Знак недели мастера", "⭐️ Знак недели мастера", "Награда за особенно продуктивную неделю."),
    ("⭐️ Знак половины миллиона", "⭐️ Знак половины миллиона", "Награда за половину миллиона символов."),
    ("⭐️ Знак пятнадцати завершений", "⭐️ Знак пятнадцати завершений", "Награда за пятнадцать завершенных текстов."),
    ("⭐️ Знак завершённого романа", "⭐️ Знак завершённого романа", "Награда за завершение большого романа."),
    ("⭐️ Знак легенды мастерства", "⭐️ Знак легенды мастерства", "Награда за достижение 30 уровня."),
    ("⭐️ Знак глобальной недели", "⭐️ Знак глобальной недели", "Награда за семь дней глобального стрика."),
    ("⭐️ Знак глобальных двух недель", "⭐️ Знак глобальных двух недель", "Награда за четырнадцать дней глобального стрика."),
    ("⭐️ Знак глобальной привычки", "⭐️ Знак глобальной привычки", "Награда за двадцать один день глобального стрика."),
    ("⭐️ Знак глобального месяца", "⭐️ Знак глобального месяца", "Награда за тридцать дней глобального стрика."),
    ("⭐️ Знак чистого глобального месяца", "⭐️ Знак чистого глобального месяца", "Награда за тридцать дней глобального стрика без заморозок."),
    ("⭐️ Знак глобального сезона", "⭐️ Знак глобального сезона", "Награда за шестьдесят дней глобального стрика."),
    ("⭐️ Знак глобального квартала", "⭐️ Знак глобального квартала", "Награда за девяносто дней глобального стрика."),
    ("⭐️ Знак глобального полугодия", "⭐️ Знак глобального полугодия", "Награда за сто восемьдесят дней глобального стрика."),
    ("⭐️ Знак глобального года", "⭐️ Знак глобального года", "Награда за год глобального стрика."),
];

fn catalog_metadata(key: &str) -> (&'static str, &'static str, Option<&'static str>) {
    if let Some((_, name, description, effect)) = ITEM_METADATA
        .iter()
        .find(|(item_key, _, _, _)| item_identifier_matches(key, item_key))
    {
        return (*name, *description, *effect);
    }
    REWARD_METADATA
        .iter()
        .find(|(item_key, _, _)| item_identifier_matches(key, item_key))
        .map(|(_, name, description)| (*name, *description, None))
        .unwrap_or(("Неизвестный предмет", "Нет описания", None))
}

fn notification_json(value: &Value, status: &str, index: usize) -> Value {
    let fields = value
        .as_object()
        .and_then(|object| object.get("fields"))
        .and_then(Value::as_object);
    json!({"id": text_value(fields.and_then(|fields| fields.get("notification_id")).or_else(|| fields.and_then(|fields| fields.get("id"))), &format!("legacy-{status}-{index}")), "text": text_value(fields.and_then(|fields| fields.get("text")), ""), "tag": fields.and_then(|fields| fields.get("tag")).and_then(Value::as_str), "created_at": fields.and_then(|fields| fields.get("date_create")).map(|value| value.to_string().trim_matches('"').to_string()), "status": status})
}

fn notifications_state(root: &Map<String, Value>) -> Value {
    let source = root.get("notifications").and_then(Value::as_object);
    let empty: Vec<Value> = Vec::new();
    let unread = source
        .and_then(|value| value.get("new"))
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    let read = source
        .and_then(|value| value.get("read"))
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    json!({"unread": unread.iter().enumerate().map(|(i, value)| notification_json(value, "new", i)).collect::<Vec<_>>(), "read": read.iter().enumerate().map(|(i, value)| notification_json(value, "read", i)).collect::<Vec<_>>(), "unread_count": unread.len()})
}

fn catalog_state(gamer: &Map<String, Value>, include_count: bool) -> Value {
    let mut categories: Vec<Value> = Vec::new();
    for category in ["Зелья", "Предметы", "Парфюм"] {
        let items: Vec<Value> = CATALOG
            .iter()
            .filter(|item| item.category == category)
            .map(|item| catalog_item_json(gamer, *item, include_count))
            .collect();
        categories.push(json!({"key": category, "name": category, "items": items}));
    }
    json!({"categories": categories, "custom_awards": {"items": []}})
}

struct SpecializationDefinition {
    key: &'static str,
    name: &'static str,
    description: &'static str,
    base_bonus: f64,
    mastery_step: f64,
    ability_name: &'static str,
    ability_description: &'static str,
}

const SPECIALIZATION_DEFINITIONS: &[SpecializationDefinition] = &[
    SpecializationDefinition {
        key: "marathoner",
        name: "Марафонец",
        description: "Даёт +15% монет и опыта за записи от 3 000 символов.",
        base_bonus: 0.15,
        mastery_step: 0.025,
        ability_name: "Длинное дыхание",
        ability_description: "Даёт +30% к следующей записи объёмом не менее 3 000 символов.",
    },
    SpecializationDefinition {
        key: "ritualist",
        name: "Ритуалист",
        description: "Даёт +25% к награде за успешную писательскую сессию.",
        base_bonus: 0.25,
        mastery_step: 0.025,
        ability_name: "Сила ритуала",
        ability_description:
            "Даёт +30% к следующей успешной сессии или один раз сохраняет серию при неудаче.",
    },
    SpecializationDefinition {
        key: "finisher",
        name: "Финишер",
        description: "Даёт +20% к награде за завершение этапа или проекта.",
        base_bonus: 0.20,
        mastery_step: 0.025,
        ability_name: "Рывок к финалу",
        ability_description: "Даёт +30% к наградам за следующие достигнутые рубежи одной рукописи.",
    },
    SpecializationDefinition {
        key: "explorer",
        name: "Исследователь",
        description: "Даёт +20% к наградам за дневные и недельные испытания.",
        base_bonus: 0.20,
        mastery_step: 0.025,
        ability_name: "Новый маршрут",
        ability_description: "Бесплатно заменяет текущее недельное испытание другим.",
    },
    SpecializationDefinition {
        key: "editor",
        name: "Редактор",
        description: "Даёт +25% к награде за успешную редакторскую сессию.",
        base_bonus: 0.25,
        mastery_step: 0.025,
        ability_name: "Точный взгляд",
        ability_description: "Даёт +30% к награде за следующую успешную редакторскую сессию.",
    },
];
const SPECIALIZATION_MASTERY_THRESHOLDS: &[i64] = &[0, 3, 8, 15, 25];

struct RelicDefinition {
    key: &'static str,
    name: &'static str,
    description: &'static str,
    condition: &'static str,
    required_progress: i64,
    required_projects: i64,
    effect_type: &'static str,
    bonus: f64,
    effect_description: &'static str,
}

const RELIC_DEFINITIONS: &[RelicDefinition] = &[
    RelicDefinition {
        key: "ink_candle",
        name: "Чернильная свеча",
        description: "Маленький огонь, зажжённый первой настоящей работой над рукописью.",
        condition: "Достигните рубежа 10% в одном тексте.",
        required_progress: 10,
        required_projects: 1,
        effect_type: "writing",
        bonus: 0.01,
        effect_description: "Даёт +1% к наградам за написанный текст.",
    },
    RelicDefinition {
        key: "plot_map",
        name: "Карта сюжетных поворотов",
        description: "Карта пройденного пути через середину большой истории.",
        condition: "Достигните рубежа 50% в одном тексте.",
        required_progress: 50,
        required_projects: 1,
        effect_type: "challenge",
        bonus: 0.03,
        effect_description: "Даёт +3% к наградам за испытания.",
    },
    RelicDefinition {
        key: "first_binding",
        name: "Переплёт первой рукописи",
        description: "Память о тексте, который прошёл весь путь от замысла до финала.",
        condition: "Доведите один текст до 100%.",
        required_progress: 100,
        required_projects: 1,
        effect_type: "completion",
        bonus: 0.05,
        effect_description: "Даёт +5% к наградам за завершение этапов и проектов.",
    },
    RelicDefinition {
        key: "chapter_shelf",
        name: "Полка первых глав",
        description: "Место для историй, каждая из которых уже обрела собственный голос.",
        condition: "Доведите три разных текста минимум до 25%.",
        required_progress: 25,
        required_projects: 3,
        effect_type: "session",
        bonus: 0.03,
        effect_description: "Даёт +3% к наградам за успешные сессии.",
    },
    RelicDefinition {
        key: "turning_quill",
        name: "Перо переломного момента",
        description: "Перо, которым история была проведена через самый трудный поворот.",
        condition: "Достигните рубежа 75% в одном тексте.",
        required_progress: 75,
        required_projects: 1,
        effect_type: "milestone",
        bonus: 0.05,
        effect_description: "Даёт +5% к наградам за рубежи рукописи.",
    },
    RelicDefinition {
        key: "final_lamp",
        name: "Лампа финишной прямой",
        description:
            "Её свет помогает не потерять дорогу, когда до финала остаётся совсем немного.",
        condition: "Достигните рубежа 90% в одном тексте.",
        required_progress: 90,
        required_projects: 1,
        effect_type: "inspiration",
        bonus: 0.10,
        effect_description: "Увеличивает получаемое за работу вдохновение на 10%.",
    },
    RelicDefinition {
        key: "triple_map",
        name: "Атлас трёх миров",
        description: "Три истории на одной карте — доказательство широты авторской вселенной.",
        condition: "Доведите три разных текста минимум до 50%.",
        required_progress: 50,
        required_projects: 3,
        effect_type: "writing",
        bonus: 0.02,
        effect_description: "Даёт +2% к наградам за написанный текст.",
    },
    RelicDefinition {
        key: "finished_shelf",
        name: "Полка завершённых рукописей",
        description: "Полка для историй, которым автор подарил настоящий финал.",
        condition: "Доведите три разных текста до 100%.",
        required_progress: 100,
        required_projects: 3,
        effect_type: "completion",
        bonus: 0.10,
        effect_description: "Даёт +10% к наградам за завершение этапов и проектов.",
    },
];

struct CabinetSetDefinition {
    key: &'static str,
    name: &'static str,
    description: &'static str,
    relics: &'static [&'static str],
    effect_type: &'static str,
    bonus: f64,
}

const MANUSCRIPT_PATH_RELICS: &[&str] = &[
    "ink_candle",
    "plot_map",
    "turning_quill",
    "final_lamp",
    "first_binding",
];
const AUTHORS_LIBRARY_RELICS: &[&str] = &["chapter_shelf", "triple_map", "finished_shelf"];
const CABINET_SET_DEFINITIONS: &[CabinetSetDefinition] = &[
    CabinetSetDefinition {
        key: "manuscript_path",
        name: "Путь большой рукописи",
        description: "Пять реликвий пути дают +3% ко всем наградам монетами и опытом.",
        relics: MANUSCRIPT_PATH_RELICS,
        effect_type: "all_rewards",
        bonus: 0.03,
    },
    CabinetSetDefinition {
        key: "authors_library",
        name: "Авторская библиотека",
        description: "Три реликвии библиотеки увеличивают получаемое вдохновение на 20%.",
        relics: AUTHORS_LIBRARY_RELICS,
        effect_type: "inspiration",
        bonus: 0.20,
    },
];

const SESSION_MODE_DEFINITIONS: &[(&str, &str, &str, f64)] = &[
    ("sprint", "Спринт", "15 минут: +15% к награде за быстрый результат.", 0.15),
    ("flow", "Поток", "Свободный сбалансированный режим без дополнительных условий.", 0.0),
    ("deep", "Глубокая работа", "45 или 60 минут: +25% к награде за длительную концентрацию.", 0.25),
    ("editing", "Редакторский проход", "Учитывает добавленные и удалённые символы по модулю изменения объёма и даёт +20% к награде за успешную сессию.", 0.20),
];

// Legacy exposed these as a fixed combo-box order. The descriptions retain
// the semantics already documented by the old session workflow.
const SESSION_INTENTION_DEFINITIONS: &[(&str, &str)] = &[
    (
        "Написать новую сцену",
        "Создать новую сцену и продвинуть текст вперёд.",
    ),
    (
        "Продолжить черновик",
        "Продолжить работу над уже начатым фрагментом текста.",
    ),
    (
        "Отредактировать текст",
        "Обработать текст: учитываются добавленные и удалённые символы по модулю.",
    ),
    (
        "Составить план",
        "Сформулировать план дальнейшей работы над текстом.",
    ),
];

const DAILY_CHALLENGE_TYPE_DEFINITIONS: &[(&str, &str, &str)] = &[
    (
        "symbols",
        "Объём дня",
        "Написать выбранный объём текста за день.",
    ),
    (
        "sessions",
        "Сессионный ритм",
        "Завершить несколько успешных писательских сессий.",
    ),
    (
        "editing",
        "День редактора",
        "Завершить успешные сессии с намерением отредактировать текст.",
    ),
];
const DAILY_CHALLENGE_DIFFICULTY_DEFINITIONS: &[(&str, &str)] =
    &[("easy", "Легко"), ("normal", "Обычно"), ("hard", "Сложно")];
const WEEKLY_CHALLENGE_DEFINITIONS: &[(&str, &str, &str, i64, f64, f64)] = &[
    (
        "symbols",
        "Марафон",
        "Написать 10 000 символов за неделю.",
        10_000,
        500.0,
        1_500.0,
    ),
    (
        "days",
        "Ритм",
        "Писать в четыре разных дня за неделю.",
        4,
        400.0,
        1_200.0,
    ),
    (
        "sessions",
        "Чистый поток",
        "Завершить пять успешных писательских сессий.",
        5,
        450.0,
        1_350.0,
    ),
    (
        "editing",
        "Редакторская неделя",
        "Завершить три успешные сессии с намерением отредактировать текст.",
        3,
        425.0,
        1_300.0,
    ),
];

const MANUSCRIPT_MILESTONE_DEFINITIONS: &[(i64, &str, i64, i64, i64)] = &[
    (10, "Искра замысла", 25, 250, 2),
    (25, "Первые главы", 50, 500, 3),
    (50, "Переломная точка", 100, 1000, 5),
    (75, "Финишная прямая", 150, 1500, 7),
    (90, "Почти готово", 200, 2000, 8),
    (100, "Рукопись завершена", 300, 3000, 10),
];

fn specializations_projection(gamer: &Map<String, Value>, now: &str) -> Value {
    let selected = gamer.get("specialization").and_then(Value::as_str);
    let mastery = gamer
        .get("specialization_mastery")
        .and_then(Value::as_object);
    let ready_at = gamer
        .get("specialization_ability_ready_at")
        .and_then(Value::as_object);
    let effects = gamer
        .get("specialization_ability_effects")
        .and_then(Value::as_object);
    let items = SPECIALIZATION_DEFINITIONS.iter().map(|definition| {
        let experience = mastery.and_then(|values| values.get(definition.key)).map_or(0, |value| integer_value(value).max(0));
        let rank = SPECIALIZATION_MASTERY_THRESHOLDS.iter().enumerate().rev().find(|(_, threshold)| experience >= **threshold).map_or(0, |(index, _)| index);
        let remaining_seconds = ready_at.and_then(|values| values.get(definition.key)).and_then(Value::as_str).and_then(iso_epoch_seconds).zip(iso_epoch_seconds(now)).map_or(0, |(started, current)| (started + 24 * 3600 - current).max(0));
        json!({"key":definition.key,"name":definition.name,"description":definition.description,"selected":selected == Some(definition.key),"mastery_experience":experience,"mastery_rank":rank,"passive_bonus":definition.base_bonus + rank as f64 * definition.mastery_step,"ability":{"name":definition.ability_name,"description":definition.ability_description,"cooldown_hours":24,"remaining_seconds":remaining_seconds,"pending":effects.and_then(|values| values.get(definition.key)).and_then(Value::as_bool).unwrap_or(false)}})
    }).collect::<Vec<_>>();
    let changed_at = gamer
        .get("specialization_changed_at")
        .and_then(Value::as_str)
        .and_then(iso_epoch_seconds)
        .zip(iso_epoch_seconds(now));
    let change_days_remaining = changed_at.map_or(0, |(changed, current)| {
        ((changed + 14 * 86_400 - current + 86_399) / 86_400).max(0)
    });
    json!({"selected":selected,"unlocks_at_level":3,"change_cooldown_days":14,"change_days_remaining":change_days_remaining,"mastery_thresholds":[0,3,8,15,25],"items":items})
}

fn integer_value(value: &Value) -> i64 {
    value
        .as_i64()
        .or_else(|| value.as_f64().map(|number| number.round() as i64))
        .unwrap_or(0)
}

fn relic_progress(gamer: &Map<String, Value>, definition: &RelicDefinition) -> i64 {
    let unlocked_projects = gamer
        .get("manuscript_journeys")
        .and_then(Value::as_object)
        .map_or(0, |journeys| {
            journeys
                .values()
                .filter(|milestones| {
                    milestones.as_array().is_some_and(|values| {
                        values
                            .iter()
                            .any(|value| integer_value(value) >= definition.required_progress)
                    })
                })
                .count() as i64
        });
    unlocked_projects.min(definition.required_projects)
}

fn manuscripts_projection(gamer: &Map<String, Value>) -> Value {
    let journeys = gamer.get("manuscript_journeys").and_then(Value::as_object).map(|values| values.iter().map(|(key, milestones)| json!({"owner_key":key,"owner_name":Value::Null,"received_milestones":milestones.as_array().map(|items| items.iter().map(integer_value).collect::<Vec<_>>()).unwrap_or_default()})).collect::<Vec<_>>()).unwrap_or_default();
    let unlocked_relics = gamer.get("cabinet_relics").and_then(Value::as_array);
    let relics = RELIC_DEFINITIONS.iter().map(|definition| {
        let unlocked = unlocked_relics.is_some_and(|values| values.iter().any(|value| value.as_str() == Some(definition.key)));
        let progress = relic_progress(gamer, definition);
        json!({"key":definition.key,"unlocked":unlocked,"name":if unlocked { json!(definition.name) } else { Value::Null },"description":if unlocked { json!(definition.description) } else { Value::Null },"condition":definition.condition,"progress":progress,"required":definition.required_projects,"effect_type":if unlocked { json!(definition.effect_type) } else { Value::Null },"bonus":if unlocked { json!(definition.bonus) } else { Value::Null },"effect_description":if unlocked { json!(definition.effect_description) } else { Value::Null }})
    }).collect::<Vec<_>>();
    let sets = CABINET_SET_DEFINITIONS.iter().map(|definition| {
        let unlocked = unlocked_relics.is_some_and(|values| definition.relics.iter().all(|relic| values.iter().any(|value| value.as_str() == Some(*relic))));
        json!({"key":definition.key,"name":definition.name,"description":definition.description,"relics":definition.relics,"unlocked":unlocked,"effect_type":definition.effect_type,"bonus":definition.bonus})
    }).collect::<Vec<_>>();
    let milestones = MANUSCRIPT_MILESTONE_DEFINITIONS.iter().map(|(progress, name, coins, exp, inspiration)| json!({"progress":progress,"name":name,"coins":coins,"exp":exp,"inspiration":inspiration})).collect::<Vec<_>>();
    json!({"journeys":journeys,"milestones":milestones,"cabinet":{"relics":relics,"sets":sets}})
}

fn daily_challenge_option_projection(value: &Value) -> Value {
    let Some(fields) = value.as_object() else {
        return json!({
            "option_id": "",
            "date": "",
            "type": "symbols",
            "name": "Объём дня",
            "description": "Написать выбранный объём текста за день.",
            "difficulty": "normal",
            "difficulty_name": "Обычно",
            "target": 0,
            "progress": 0,
            "completed": false,
            "reward": {"coins": 0.0, "experience": 0.0, "inspiration": 10},
        });
    };
    let challenge_type = text_value(fields.get("type"), "symbols");
    let difficulty = text_value(fields.get("difficulty"), "normal");
    let (name, description) = DAILY_CHALLENGE_TYPE_DEFINITIONS
        .iter()
        .find(|(key, _, _)| *key == challenge_type)
        .map_or((challenge_type.as_str(), ""), |(_, name, description)| {
            (*name, *description)
        });
    let difficulty_name = DAILY_CHALLENGE_DIFFICULTY_DEFINITIONS
        .iter()
        .find(|(key, _)| *key == difficulty)
        .map_or(difficulty.as_str(), |(_, name)| *name);
    let option_id = fields
        .get("option_id")
        .map(|value| text_value(Some(value), ""))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| format!("{challenge_type}:{difficulty}"));
    json!({
        "option_id": option_id,
        "date": text_value(fields.get("date"), ""),
        "type": challenge_type,
        "name": name,
        "description": description,
        "difficulty": difficulty,
        "difficulty_name": difficulty_name,
        "target": integer_field(fields, "target", 0).max(0),
        "progress": integer_field(fields, "progress", 0).max(0),
        "completed": fields.get("completed").and_then(Value::as_bool).unwrap_or(false),
        "reward": {
            "coins": number_field(fields, "reward_coins", 0.0).max(0.0),
            "experience": number_field(fields, "reward_exp", 0.0).max(0.0),
            "inspiration": 10,
        },
    })
}

fn daily_challenge_projection(gamer: &Map<String, Value>) -> Value {
    let current = gamer
        .get("daily_challenge")
        .filter(|value| !value.is_null())
        .map(daily_challenge_option_projection)
        .unwrap_or(Value::Null);
    let options = gamer
        .get("daily_challenge_options")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .map(daily_challenge_option_projection)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    json!({
        "change_cost": 15,
        "current": current,
        "options": options,
        "history": gamer.get("daily_challenge_history").cloned().unwrap_or(json!([])),
    })
}

fn weekly_challenge_payload(value: &Value) -> Option<Value> {
    let fields = value.as_object()?;
    let key = text_value(fields.get("key"), "");
    let (_, name, description, target, reward_coins, reward_exp) = WEEKLY_CHALLENGE_DEFINITIONS
        .iter()
        .find(|(definition_key, _, _, _, _, _)| *definition_key == key)?;
    Some(json!({
        "key": key,
        "name": name,
        "description": description,
        "week_start": text_value(fields.get("week_start"), ""),
        "target": target,
        "progress": integer_field(fields, "progress", 0).max(0),
        "writing_days": fields.get("writing_days").cloned().unwrap_or(json!([])),
        "completed": fields.get("completed").and_then(Value::as_bool).unwrap_or(false),
        "reward": {"coins": reward_coins, "experience": reward_exp, "inspiration": 20},
    }))
}

fn weekly_challenge_projection(gamer: &Map<String, Value>) -> Value {
    let current = gamer
        .get("weekly_challenge")
        .filter(|value| !value.is_null())
        .and_then(weekly_challenge_payload)
        .unwrap_or(Value::Null);
    let catalog = WEEKLY_CHALLENGE_DEFINITIONS
        .iter()
        .map(
            |(key, name, description, target, reward_coins, reward_exp)| {
                json!({
                    "key": key,
                    "name": name,
                    "description": description,
                    "target": target,
                    "reward": {"coins": reward_coins, "experience": reward_exp, "inspiration": 20},
                })
            },
        )
        .collect::<Vec<_>>();
    json!({"current": current, "catalog": catalog})
}

fn project_state(
    root: &Value,
    now: &str,
    effects_now: &str,
    logical_day: &str,
    enabled: bool,
) -> GameResult<Value> {
    let root_object = root
        .as_object()
        .ok_or_else(|| GameError::InvalidState("Game state is not an object".into()))?;
    let gamer = root_object
        .get("gamer")
        .and_then(Value::as_object)
        .ok_or_else(|| GameError::InvalidState("Gamer state is not an object".into()))?;
    let level = integer_field(gamer, "level", 1).clamp(1, 999);
    let max_health = integer_field(gamer, "max_health", 100).max(100);
    let next_level_experience = if level < 99 {
        Some(8_000.0 * (level as f64).powi(2))
    } else {
        None
    };
    let inventory = catalog_state(gamer, true);
    let notifications = notifications_state(root_object);
    let global = root_object
        .get("global_streak")
        .and_then(Value::as_object)
        .or_else(|| root_object.get("game").and_then(Value::as_object));
    let global_status = global
        .map(|value| crate::streaks::canonical_global_status(value, logical_day))
        .unwrap_or_else(|| "No".to_string());
    let global_length = global
        .and_then(|value| value.get("global_streaks"))
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let global_max = global
        .and_then(|value| value.get("max_global_streak"))
        .and_then(Value::as_i64)
        .unwrap_or(global_length as i64)
        .max(global_length as i64);
    let mut state = json!({
        "enabled": enabled, "server_time": now,
        "profile": {"level": level, "experience": number_field(gamer, "exp", 0.0).max(0.0), "next_level_experience": next_level_experience, "coins": number_field(gamer, "coins", 0.0).max(0.0), "inflation": number_field(gamer, "inflation", 1.0).max(1.0), "health": number_field(gamer, "health", max_health as f64).clamp(0.0, max_health as f64), "max_health": max_health, "inspiration": number_field(gamer, "inspiration", 0.0).clamp(0.0, 100.0), "max_inspiration": 100, "writing_session_streak": integer_field(gamer, "writing_session_streak", 0).max(0), "session_streak_shields": integer_field(gamer, "session_streak_shields", 0).clamp(0, 3), "session_grade_boosts": integer_field(gamer, "session_grade_boosts", 0).clamp(0, 1), "pending_bonuses": {"writing": number_field(gamer, "writing_reward_bonus", 0.0), "session": number_field(gamer, "session_reward_bonus", 0.0), "challenge": number_field(gamer, "challenge_reward_bonus", 0.0), "manuscript": number_field(gamer, "manuscript_reward_bonus", 0.0)}},
        "skills": skills_projection(gamer),
        "buffs": buffs_projection(gamer, effects_now), "streak_freezes": {"date": now.get(..10).unwrap_or(now), "inventory_count": item_count(gamer, "Предметы", "Заморозка"), "global_available": false, "projects": []},
        "notifications": notifications, "inventory": inventory, "quests": quest_projection(gamer), "daily_challenge": daily_challenge_projection(gamer),
        "weekly_challenge": weekly_challenge_projection(gamer),
        "writing_session": {"server_time": now, "active": session_projection(gamer.get("writing_session"), now), "streak": integer_field(gamer, "writing_session_streak", 0), "history": gamer.get("writing_session_history").cloned().unwrap_or(json!([])), "modes": SESSION_MODE_DEFINITIONS.iter().map(|(key,name,description,reward_bonus)| json!({"key":key,"name":name,"description":description,"reward_bonus":reward_bonus})).collect::<Vec<_>>(), "intentions": SESSION_INTENTION_DEFINITIONS.iter().map(|(key,description)| json!({"key":key,"name":key,"description":description})).collect::<Vec<_>>(), "grades": [{"key":"gold","name":"Золото","target_ratio":1.5,"reward_multiplier":1.3},{"key":"silver","name":"Серебро","target_ratio":1.25,"reward_multiplier":1.15},{"key":"bronze","name":"Бронза","target_ratio":1.0,"reward_multiplier":1.0}], "allowed_durations_minutes":[15,25,45,60]},
        "inspiration": {"abilities": [{"key":"creative_surge","name":"Творческий импульс","description":"+25% к следующей записи.","cost":30,"bonus":0.25,"active":number_field(gamer,"writing_reward_bonus",0.0)>0.0},{"key":"session_spark","name":"Искра сессии","description":"+25% к следующей сессии.","cost":25,"bonus":0.25,"active":number_field(gamer,"session_reward_bonus",0.0)>0.0},{"key":"challenge_focus","name":"Фокус испытания","description":"+25% к следующему испытанию.","cost":40,"bonus":0.25,"active":number_field(gamer,"challenge_reward_bonus",0.0)>0.0}], "creative_event": gamer.get("pending_creative_event").cloned().unwrap_or(Value::Null), "creative_event_history": gamer.get("creative_event_history").cloned().unwrap_or(json!([]))},
        "specializations": specializations_projection(gamer, now), "manuscripts": manuscripts_projection(gamer), "bank":bank_projection(gamer, level), "custom_awards":custom_awards_projection(gamer), "shop": catalog_state(gamer, true)
    });
    state["global_streak"] = json!({
        "status": global_status,
        "length": global_length,
        "max_length": global_max,
    });
    if let Some(items) = gamer.get("items").and_then(Value::as_object) {
        let inventory_categories = state["inventory"]["categories"]
            .as_array_mut()
            .expect("catalog categories");
        for (category, values) in items {
            if inventory_categories
                .iter()
                .any(|value| value["key"].as_str() == Some(category))
            {
                continue;
            }
            let rows: Vec<Value> = values
                .as_object()
                .into_iter()
                .flat_map(|values| values.iter())
                .filter(|(_, count)| count.as_i64().unwrap_or(0) > 0)
                .map(|(key, count)| {
                    let (name, description, effect) = catalog_metadata(key);
                    let known = description != "Нет описания";
                    json!({"id":format!("{category}:{key}"),"key":key,"category":category,"name":name,"description":if known { Some(description) } else { None::<&str> },"effect":effect,"count":count,"known":known,"usable":false,"buy":false,"sellable":false})
                })
                .collect();
            inventory_categories.push(json!({"key":category,"name":category,"items":rows}));
        }
    }
    Ok(state)
}

pub struct GameApplicationService;

impl GameApplicationService {
    fn owner(connection: &Connection) -> GameResult<()> {
        let owner: String = connection
            .query_row(
                "SELECT owner FROM storage_ownership WHERE subsystem='game'",
                [],
                |row| row.get(0),
            )
            .map_err(|error| GameError::Database(error.to_string()))?;
        if owner == "sqlite" {
            Ok(())
        } else {
            Err(GameError::InvalidState(
                "Игра ещё не переведена в SQLite authoritative storage.".into(),
            ))
        }
    }

    fn load(connection: &Connection) -> GameResult<Value> {
        let raw: String = connection
            .query_row(
                "SELECT payload_json FROM game_state WHERE id=1",
                [],
                |row| row.get(0),
            )
            .map_err(|error| GameError::Database(error.to_string()))?;
        serde_json::from_str(&raw).map_err(|error| {
            GameError::InvalidState(format!("Некорректное состояние игры: {error}"))
        })
    }

    fn configured_now(connection: &Connection) -> Option<String> {
        if !crate::developer_mode_available() {
            return None;
        }
        let raw_state = connection
            .query_row(
                "SELECT payload_json FROM game_state WHERE id=1",
                [],
                |row| row.get::<_, String>(0),
            )
            .ok()?;
        let state = serde_json::from_str::<Value>(&raw_state).ok()?;
        let datetime = state
            .get("game")
            .and_then(|value| value.get("extensions"))
            .or_else(|| state.get("extensions"))
            .and_then(|value| value.get("developer_clock"))
            .filter(|value| value.get("enabled").and_then(Value::as_bool) == Some(true))
            .and_then(|value| value.get("datetime"))
            .and_then(Value::as_str)
            .filter(|datetime| iso_epoch_seconds(datetime).is_some())?;
        Some(datetime.to_string())
    }

    fn now(connection: &Connection) -> GameResult<String> {
        if let Some(datetime) = Self::configured_now(connection) {
            return Ok(datetime);
        }
        connection
            .query_row("SELECT strftime('%Y-%m-%dT%H:%M:%SZ','now')", [], |row| {
                row.get(0)
            })
            .map_err(|error| GameError::Database(error.to_string()))
    }

    fn local_now(connection: &Connection) -> GameResult<String> {
        if let Some(datetime) = Self::configured_now(connection) {
            return Ok(datetime);
        }
        // Legacy datetime fields are naive local timestamps (datetime.now()).
        // Use the same representation when comparing them with end_time.
        connection
            .query_row(
                "SELECT strftime('%Y-%m-%dT%H:%M:%f','now','localtime')",
                [],
                |row| row.get(0),
            )
            .map_err(|error| GameError::Database(error.to_string()))
    }

    fn enabled(connection: &Connection) -> bool {
        connection
            .query_row(
                "SELECT value_json FROM settings WHERE key='game_mode'",
                [],
                |row| row.get::<_, String>(0),
            )
            .ok()
            .and_then(|value| serde_json::from_str::<Value>(&value).ok())
            .and_then(|value| value.as_bool())
            .unwrap_or(true)
    }

    fn mutate<F>(mutator: F) -> GameResult<GameCommandResponse>
    where
        F: FnOnce(&mut Value, &mut OsGameRng, &str) -> GameResult<(Option<String>, Option<Value>)>,
    {
        Self::mutate_classified(false, mutator)
    }

    fn mutate_classified<F>(developer_override: bool, mutator: F) -> GameResult<GameCommandResponse>
    where
        F: FnOnce(&mut Value, &mut OsGameRng, &str) -> GameResult<(Option<String>, Option<Value>)>,
    {
        Self::mutate_portable(developer_override, None, mutator)
    }

    fn mutate_portable<F>(
        developer_override: bool,
        inventory: Option<crate::game_writer::InventoryIntent>,
        mutator: F,
    ) -> GameResult<GameCommandResponse>
    where
        F: FnOnce(&mut Value, &mut OsGameRng, &str) -> GameResult<(Option<String>, Option<Value>)>,
    {
        let mut connection = crate::open_projects_database().map_err(GameError::Database)?;
        Self::mutate_portable_sqlite(&mut connection, developer_override, inventory, mutator)
    }

    pub(crate) fn mutate_portable_sqlite<F>(
        connection: &mut Connection,
        developer_override: bool,
        inventory: Option<crate::game_writer::InventoryIntent>,
        mutator: F,
    ) -> GameResult<GameCommandResponse>
    where
        F: FnOnce(&mut Value, &mut OsGameRng, &str) -> GameResult<(Option<String>, Option<Value>)>,
    {
        Self::owner(connection)?;
        process_pending_events(connection, 100).map_err(GameError::Database)?;
        if !Self::enabled(connection) {
            return Err(GameError::PrerequisiteMissing(
                "Игровой режим отключён.".into(),
            ));
        }
        let tx = connection
            .transaction()
            .map_err(|error| GameError::Database(error.to_string()))?;
        let mut state = Self::load(&tx)?;
        let before_raw: String = tx
            .query_row("SELECT payload_json FROM game_state WHERE id=1", [], |r| {
                r.get(0)
            })
            .map_err(|e| GameError::Database(e.to_string()))?;
        let now = Self::now(&tx)?;
        let effects_now = Self::local_now(&tx)?;
        let mut rng = OsGameRng;
        let (message, result) = mutator(&mut state, &mut rng, &now)?;
        if developer_override {
            crate::game_sync::record_developer_override(&tx, &state, &now)
                .map_err(GameError::Database)?;
        }
        if let Some(intent) = inventory {
            let portable_now: String = tx
                .query_row(
                    "SELECT strftime('%Y-%m-%dT%H:%M:%S.000000Z','now')",
                    [],
                    |r| r.get(0),
                )
                .map_err(|e| GameError::Database(e.to_string()))?;
            crate::game_writer::capture_inventory(
                &tx,
                &intent,
                &before_raw,
                &state,
                result.as_ref(),
                &portable_now,
            )
            .map_err(GameError::Database)?;
        }
        tx.execute(
            "UPDATE game_state SET schema_version=?1,payload_json=?2,updated_at=?3 WHERE id=1",
            params![STATE_VERSION, state.to_string(), now],
        )
        .map_err(|error| GameError::Database(error.to_string()))?;
        let enabled = Self::enabled(&tx);
        tx.commit()
            .map_err(|error| GameError::Database(error.to_string()))?;
        let logical_day =
            crate::streaks::logical_writing_day(connection).map_err(GameError::Database)?;
        let state = project_state(&state, &now, &effects_now, &logical_day, enabled)?;
        let messages = message.clone().into_iter().collect::<Vec<_>>();
        Ok(GameCommandResponse {
            ok: true,
            message,
            messages,
            result,
            state,
        })
    }

    pub fn state() -> GameResult<Value> {
        let mut connection = crate::open_projects_database().map_err(GameError::Database)?;
        Self::owner(&connection)?;
        process_pending_events(&mut connection, 100).map_err(GameError::Database)?;
        let now = Self::now(&connection)?;
        let effects_now = Self::local_now(&connection)?;
        let logical_day =
            crate::streaks::logical_writing_day(&connection).map_err(GameError::Database)?;
        let enabled = Self::enabled(&connection);
        project_state(
            &Self::load(&connection)?,
            &now,
            &effects_now,
            &logical_day,
            enabled,
        )
    }

    pub fn notifications() -> GameResult<Value> {
        let state = Self::state()?;
        Ok(state["notifications"].clone())
    }

    pub fn process_bank_events(_auto_pay: bool) -> GameResult<GameCommandResponse> {
        Self::mutate(|state, _rng, now| {
            let gamer = gamer_object(state)?;
            let matured = gamer
                .get("bank_account")
                .and_then(tagged_fields_immutable)
                .and_then(|fields| fields.get("deposit"))
                .filter(|value| !value.is_null())
                .and_then(tagged_fields_immutable)
                .and_then(|fields| {
                    let opened = text_value(fields.get("give_date"), "");
                    let days = integer_field(fields, "days_until_return", 1).max(1);
                    let principal = number_field(fields, "deposit_sum", 0.0).max(0.0);
                    let rate = number_field(fields, "interest_rate_on_deposit", 1.0).max(0.0);
                    let interest =
                        rounded_money(principal * ((1.0 + rate / 100.0).powi(days as i32) - 1.0));
                    iso_epoch_seconds(&opened)
                        .zip(iso_epoch_seconds(now))
                        .filter(|(opened, current)| *current >= *opened + days * 86_400)
                        .map(|_| (principal, interest))
                });

            let Some((principal, interest)) = matured else {
                return Ok((None, None));
            };
            let total = rounded_money(principal + interest);
            add_number(gamer, "coins", total);
            let bank = gamer
                .get_mut("bank_account")
                .and_then(tagged_fields)
                .ok_or_else(|| GameError::InvalidState("Некорректный банковский счёт.".into()))?;
            bank.insert("deposit".into(), Value::Null);
            array_mut(bank, "deposit_history").push(json!({
                "sum": principal,
                "interest": interest,
                "status": "auto_returned",
                "returned_date": now,
            }));
            Ok((
                Some(format!(
                    "Срок вклада завершен. На счет возвращено {total} монет."
                )),
                Some(json!({"amount":total,"interest":interest})),
            ))
        })
    }

    pub fn preview_bank_product(request: BankProductRequest) -> GameResult<GameCommandResponse> {
        let state = Self::state()?;
        let profile = state
            .get("profile")
            .and_then(Value::as_object)
            .ok_or_else(|| GameError::InvalidState("Профиль игры не найден.".into()))?;
        let amount = checked_positive(request.amount, "Сумма")?;
        if request.days <= 0 || request.days > 36_500 {
            return Err(GameError::Validation("Некорректный срок продукта.".into()));
        }
        let product_type = request.product_type.as_deref().unwrap_or("deposit");
        let rate = if product_type == "credit" { 2.0 } else { 1.0 };
        let interest = if product_type == "credit" {
            let daily_rate: f64 = rate / 100.0;
            let growth = (1.0 + daily_rate).powi(request.days as i32);
            rounded_money(
                rounded_money(amount * daily_rate * growth / (growth - 1.0)) * request.days as f64
                    - amount,
            )
        } else {
            rounded_money(amount * ((1.0 + rate / 100.0).powi(request.days as i32) - 1.0))
        };
        Ok(GameCommandResponse {
            ok: true,
            message: None,
            messages: Vec::new(),
            result: Some(
                json!({"amount":amount,"rate":rate,"interest":interest,"total":rounded_money(amount+interest),"limit":if product_type == "credit" { Some(number_field(profile,"coins",0.0)*10.0) } else { None }}),
            ),
            state,
        })
    }

    pub fn inventory(
        category: String,
        item_id: String,
        count: i64,
        operation: &str,
    ) -> GameResult<GameCommandResponse> {
        let mut connection = crate::open_projects_database().map_err(GameError::Database)?;
        Self::inventory_sqlite(&mut connection, category, item_id, count, operation)
    }

    pub(crate) fn inventory_sqlite(
        connection: &mut Connection,
        category: String,
        item_id: String,
        count: i64,
        operation: &str,
    ) -> GameResult<GameCommandResponse> {
        let count = checked_count(count)?;
        let intent = crate::game_writer::InventoryIntent {
            operation: operation.into(),
            category: category.clone(),
            item_id: item_id.clone(),
            count,
        };
        Self::mutate_portable_sqlite(connection, false, Some(intent), move |state, rng, now| {
            let gamer = gamer_object(state)?;
            let item = catalog_item(&category, &item_id)
                .ok_or_else(|| GameError::NotFound("Предмет не найден.".into()))?;
            let current = item_count(gamer, &category, &item_id);
            match operation {
                "buy" => {
                    if !item.buyable {
                        return Err(GameError::PrerequisiteMissing(
                            "Этот предмет нельзя купить.".into(),
                        ));
                    }
                    if integer_field(gamer, "level", 1) < item.level {
                        return Err(GameError::PrerequisiteMissing(format!(
                            "Предмет доступен с {} уровня.",
                            item.level
                        )));
                    }
                    let new_count = checked_inventory_total(current, count, item.maximum)?;
                    let item_price = if item.apply_inflation {
                        let multiplier = 1.0 + (integer_field(gamer, "level", 1).max(1) - 1) as f64 * 0.15;
                        rounded_money(item.price * multiplier)
                    } else {
                        item.price
                    };
                    let total = rounded_money(item_price * count as f64);
                    let coins = number_field(gamer, "coins", 0.0);
                    if coins < total {
                        return Err(GameError::InsufficientFunds("Недостаточно монет!".into()));
                    }
                    gamer.insert("coins".into(), json!(rounded_money(coins - total)));
                    item_map(gamer, &category).insert(item_id.clone(), json!(new_count));
                    Ok((
                        Some(format!("Куплено: {} x{}.", item.key, count)),
                        Some(
                            json!({"category":category,"item_key":item.key,"count":count,"unit_price":item.price,"total_price":total}),
                        ),
                    ))
                }
                "sell" => {
                    if !item.sellable {
                        return Err(GameError::PrerequisiteMissing(
                            "Этот предмет нельзя продать.".into(),
                        ));
                    }
                    if current < count {
                        return Err(GameError::InvalidQuantity(format!(
                            "В инвентаре только {current} шт."
                        )));
                    }
                    let total = rounded_money(item.price * 0.75 * count as f64);
                    gamer.insert(
                        "coins".into(),
                        json!(rounded_money(number_field(gamer, "coins", 0.0) + total)),
                    );
                    let items = item_map(gamer, &category);
                    if current == count {
                        items.remove(&item_id);
                    } else {
                        items.insert(item_id.clone(), json!(current - count));
                    }
                    Ok((
                        Some(format!("Продано: {} x{}.", item.key, count)),
                        Some(
                            json!({"category":category,"item_key":item.key,"count":count,"unit_price":rounded_money(item.price*0.75),"total_price":total}),
                        ),
                    ))
                }
                "use" => Self::use_item_mutation(gamer, item, count, rng, now),
                _ => Err(GameError::Validation("Unknown inventory operation".into())),
            }
        })
    }

    fn use_item_mutation<R: GameRng>(
        gamer: &mut Map<String, Value>,
        item: CatalogItem,
        count: i64,
        rng: &mut R,
        now: &str,
    ) -> GameResult<(Option<String>, Option<Value>)> {
        let current = item_count(gamer, item.category, item.key);
        if !item.usable {
            return Err(GameError::PrerequisiteMissing(
                "Этот предмет нельзя использовать напрямую.".into(),
            ));
        }
        if current < count {
            return Err(GameError::InvalidQuantity(format!(
                "В инвентаре только {current} шт."
            )));
        }
        let items = item_map(gamer, item.category);
        if current == count {
            items.remove(item.key);
        } else {
            items.insert(item.key.into(), json!(current - count));
        }
        let mut result = json!({"category":item.category,"item_key":item.key,"count":count});
        for _ in 0..count {
            match item.key {
                "Микро зелье здоровья"
                | "Малое зелье здоровья"
                | "Среднее зелье здоровья"
                | "Большое зелье здоровья"
                | "Зелье воскрешения" => {
                    let amount = match item.key {
                        "Микро зелье здоровья" => 5.0,
                        "Малое зелье здоровья" => 10.0,
                        "Среднее зелье здоровья" => 25.0,
                        "Большое зелье здоровья" => 50.0,
                        _ => number_field(gamer, "max_health", 100.0),
                    };
                    let max = number_field(gamer, "max_health", 100.0);
                    gamer.insert(
                        "health".into(),
                        json!((number_field(gamer, "health", max) + amount).min(max)),
                    );
                }
                "Зелье вдохновения" => {
                    add_capped(gamer, "inspiration", 25.0, 100.0)
                }
                "Искра вдохновения" => {
                    add_capped(gamer, "inspiration", 10.0, 100.0)
                }
                "Большое зелье вдохновения" => {
                    add_capped(gamer, "inspiration", 50.0, 100.0)
                }
                "Эликсир вдохновения" => {
                    add_capped(gamer, "inspiration", 100.0, 100.0)
                }
                "Часовое зелье познания"
                | "Суточное зелье познания"
                | "Недельное зелье познания"
                | "Часовое зелье просвещения"
                | "Суточное зелье просвещения"
                | "Недельное зелье просвещения"
                | "Часовое зелье доходности"
                | "Суточное зелье доходности"
                | "Недельное зелье доходности"
                | "Часовое зелье супердоходности"
                | "Суточное зелье супердоходности"
                | "Недельное зелье супердоходности" => {
                    let (target, value) = if item.key.contains("доходности") {
                        if item.key.contains("супер") {
                            ("coins", 10.0)
                        } else {
                            ("coins", 0.5)
                        }
                    } else if item.key.contains("просвещения") {
                        ("exp", 10.0)
                    } else {
                        ("exp", 1.0)
                    };
                    let duration_minutes = if item.key.contains("Часовое") {
                        60
                    } else if item.key.contains("Суточное") {
                        1_440
                    } else {
                        10_080
                    };
                    array_mut(gamer, "buffs").push(json!({"__type__":"game_data.Buff","fields":{"name":item.key,"description":"Временный бонус предмета.","buff_type":"positive","target_cf":target,"value":value,"duration_minutes":duration_minutes,"start_time":now,"end_time":Value::Null,"source":item.key,"stackable":false}}));
                }
                "Чернильница потока" => {
                    add_capped(gamer, "session_reward_bonus", 0.25, 1.0)
                }
                "Компас рукописи" => {
                    add_capped(gamer, "manuscript_reward_bonus", 0.25, 1.0)
                }
                "Нить ритуала" => add_capped(gamer, "session_streak_shields", 1.0, 3.0),
                "Медаль качества" => {
                    add_capped(gamer, "session_grade_boosts", 1.0, 1.0)
                }
                "Учебник мастерства" => {
                    add_capped(gamer, "specialization_mastery_book_bonus", 2.0, 1000000.0)
                }
                "Лотерейный билет" => {
                    if !result.get("lottery_draws").is_some_and(Value::is_array) {
                        result["lottery_draws"] = json!([]);
                    }
                    let draw = lottery_draw(rng);
                    let prize = draw["prize"].as_f64().unwrap_or(0.0);
                    if prize > 0.0 {
                        add_number(gamer, "coins", prize);
                    }
                    result["lottery_draws"]
                        .as_array_mut()
                        .expect("array")
                        .push(draw.clone());
                    array_mut(gamer, "lottery_history").push(draw);
                }
                _ => {}
            }
        }
        let message = format!("Использовано: {} x{}.", item.key, count);
        Ok((Some(message), Some(result)))
    }

    pub fn run_lottery() -> GameResult<GameCommandResponse> {
        Self::inventory("Предметы".into(), "Лотерейный билет".into(), 1, "use")
    }
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct WritingSessionRequest {
    pub duration_minutes: i64,
    pub target_symbols: i64,
    pub intention: String,
    pub mode: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct InventoryRequest {
    pub category: String,
    pub item_id: String,
    pub count: i64,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DailyChallengeRequest {
    pub option_id: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct WeeklyChallengeRequest {
    pub challenge_id: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ChoiceRequest {
    pub choice: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct NotificationRequest {
    pub notification_id: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AbilityRequest {
    pub ability_id: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SpecializationRequest {
    pub specialization_id: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct QuestRequest {
    pub quest_id: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AwardCountRequest {
    pub award_id: String,
    pub count: i64,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AwardIdRequest {
    pub award_id: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SkillRequest {
    pub skill_id: String,
    pub points: i64,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct FreezeRequest {
    pub target: String,
    pub project_id: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct BankProductRequest {
    pub product_type: Option<String>,
    pub amount: f64,
    pub days: i64,
    pub allow_interest_withdrawal: Option<bool>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct BankAmountRequest {
    pub amount: f64,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct BankProcessRequest {
    pub auto_pay: bool,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct BankWithdrawRequest {
    pub allow_early: bool,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CustomAwardRequest {
    pub name: String,
    pub price: f64,
    #[serde(default)]
    pub apply_inflation: bool,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CustomAwardUpdateRequest {
    pub award_id: String,
    pub name: Option<String>,
    pub price: Option<f64>,
    pub apply_inflation: Option<bool>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DeveloperProfileRequest {
    pub level: i64,
    pub health: f64,
    pub coins: f64,
    pub exp: f64,
    pub test_date_enabled: bool,
    pub test_datetime: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DeveloperStreakRequest {
    #[serde(rename = "type")]
    pub kind: String,
    pub project_id: Option<String>,
    pub stage_id: Option<String>,
    pub length: Option<i64>,
}

fn array_mut<'a>(object: &'a mut Map<String, Value>, key: &str) -> &'a mut Vec<Value> {
    object
        .entry(key.to_string())
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .expect("new array")
}

fn add_number(gamer: &mut Map<String, Value>, key: &str, amount: f64) {
    let value = number_field(gamer, key, 0.0) + amount;
    gamer.insert(key.to_string(), json!(value));
}

fn add_experience(gamer: &mut Map<String, Value>, amount: f64) {
    let accrued = ((amount.max(0.0) - 1e-9) * 10.0).ceil() / 10.0;
    add_number(gamer, "exp", accrued.max(0.0));
    loop {
        let level = integer_field(gamer, "level", 1).clamp(1, 99);
        let threshold = 8_000.0 * (level as f64).powi(2);
        if level >= 99 || number_field(gamer, "exp", 0.0) < threshold {
            break;
        }
        gamer.insert("level".into(), json!(level + 1));
        add_number(gamer, "exp", -threshold);
        let inflation = 1.0 + (level as f64 - 1.0) * 0.15;
        add_number(gamer, "coins", level as f64 * 250.0 * inflation);
        let points = integer_field(gamer, "available_skill_points", 0) + 2;
        gamer.insert("available_skill_points".into(), json!(points));
        let max_health = 100 + ((level / 5) * 10);
        gamer.insert("max_health".into(), json!(max_health));
        gamer.insert("health".into(), json!(max_health));
    }
}

fn validate_developer_streak_target(
    connection: &Connection,
    request: &DeveloperStreakRequest,
) -> GameResult<Option<String>> {
    let key = developer_streak_key(
        &request.kind,
        request.project_id.as_deref(),
        request.stage_id.as_deref(),
    )?;
    match request.kind.as_str() {
        "global" => Ok(key),
        "project" => {
            let exists = connection
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM projects WHERE id=?1)",
                    [request.project_id.as_deref().unwrap()],
                    |row| row.get::<_, bool>(0),
                )
                .map_err(|error| GameError::Database(error.to_string()))?;
            exists
                .then_some(key)
                .ok_or_else(|| GameError::NotFound("Проект не найден.".into()))
        }
        "stage" => {
            let exists = connection
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM stages WHERE id=?1 AND project_id=?2)",
                    [
                        request.stage_id.as_deref().unwrap(),
                        request.project_id.as_deref().unwrap(),
                    ],
                    |row| row.get::<_, bool>(0),
                )
                .map_err(|error| GameError::Database(error.to_string()))?;
            exists.then_some(key).ok_or_else(|| {
                GameError::NotFound("Источник не найден в выбранном проекте.".into())
            })
        }
        _ => unreachable!("developer_streak_key validates the target type"),
    }
}

fn developer_streak_fields_mut<'a>(
    root: &'a mut Value,
    key: Option<&str>,
) -> GameResult<(
    &'a mut Map<String, Value>,
    &'static str,
    &'static str,
    &'static str,
)> {
    let root = game_object(root)?;
    if let Some(key) = key {
        let states = root
            .entry("project_game_state")
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
            .ok_or_else(|| {
                GameError::InvalidState("Состояние локальных стриков некорректно.".into())
            })?;
        let fields = states
            .get_mut(key)
            .and_then(Value::as_object_mut)
            .ok_or_else(|| GameError::NotFound("Состояние выбранного стрика не найдено.".into()))?;
        Ok((fields, "streaks", "streak_status", "last_streak_lose_len"))
    } else {
        let fields = root
            .entry("global_streak")
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
            .ok_or_else(|| {
                GameError::InvalidState("Состояние глобального стрика некорректно.".into())
            })?;
        Ok((
            fields,
            "global_streaks",
            "global_streak_status",
            "last_global_streak_lose_len",
        ))
    }
}

impl GameApplicationService {
    pub fn developer_streak_state() -> GameResult<Value> {
        if !crate::developer_mode_available() {
            return Err(GameError::PrerequisiteMissing(
                "Режим разработчика недоступен в release-сборке.".into(),
            ));
        }
        let connection = crate::open_projects_database().map_err(GameError::Database)?;
        Self::owner(&connection)?;
        let root = Self::load(&connection)?;
        let logical_day =
            crate::streaks::logical_writing_day(&connection).map_err(GameError::Database)?;
        let root_map = root
            .as_object()
            .ok_or_else(|| GameError::InvalidState("Game state is not an object".into()))?;
        let states = root_map
            .get("project_game_state")
            .and_then(Value::as_object);
        let global = root_map.get("global_streak").and_then(Value::as_object);
        let mut targets = Vec::new();
        if let Some(fields) = global {
            let (last, length, _) = crate::streaks::streak_summary(fields.get("global_streaks"));
            targets.push(json!({
                "id":"global", "type":"global", "name":"Глобальный",
                "status":crate::streaks::canonical_global_status(fields, &logical_day),
                "length":length, "max_length":integer_field(fields, "max_global_streak", length as i64).max(length as i64),
                "last_effective_day":last.map(crate::streaks::date_from_days),
            }));
        }
        let mut statement = connection.prepare(
            "SELECT id, NULL, name FROM projects UNION ALL SELECT id, project_id, name FROM stages ORDER BY project_id, name",
        ).map_err(|error| GameError::Database(error.to_string()))?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(|error| GameError::Database(error.to_string()))?;
        for row in rows {
            let (id, parent_id, name) =
                row.map_err(|error| GameError::Database(error.to_string()))?;
            let (kind, key, project_id, stage_id) = match parent_id {
                Some(project_id) => (
                    "stage",
                    format!("stage:{project_id}:{id}"),
                    project_id,
                    Some(id),
                ),
                None => ("project", format!("project:{id}"), id, None),
            };
            let fields = states
                .and_then(|states| states.get(&key))
                .and_then(Value::as_object);
            let empty = Map::new();
            let fields = fields.unwrap_or(&empty);
            let (last, length, _) = crate::streaks::streak_summary(fields.get("streaks"));
            targets.push(json!({
                "id":key, "type":kind, "name":name, "project_id":project_id, "stage_id":stage_id,
                "status":crate::streaks::canonical_local_status(fields, &logical_day, &Map::new()),
                "length":length, "max_length":integer_field(fields, "max_streak", length as i64).max(length as i64),
                "last_effective_day":last.map(crate::streaks::date_from_days),
            }));
        }
        Ok(json!({"logical_day":logical_day,"targets":targets}))
    }

    pub fn developer_create_streak_series(
        request: DeveloperStreakRequest,
    ) -> GameResult<GameCommandResponse> {
        if !crate::developer_mode_available() {
            return Err(GameError::PrerequisiteMissing(
                "Режим разработчика недоступен в release-сборке.".into(),
            ));
        }
        let length = request.length.unwrap_or(0);
        if !(1..=MAX_DEVELOPER_STREAK_LENGTH).contains(&length) {
            return Err(GameError::Validation(
                "Количество дней должно быть от 1 до 10000.".into(),
            ));
        }
        let connection = crate::open_projects_database().map_err(GameError::Database)?;
        Self::owner(&connection)?;
        let key = validate_developer_streak_target(&connection, &request)?;
        let logical_day =
            crate::streaks::logical_writing_day(&connection).map_err(GameError::Database)?;
        let yesterday = crate::streaks::date_days(&logical_day)
            .ok_or_else(|| GameError::Validation("Некорректный писательский день.".into()))?
            - 1;
        Self::mutate_classified(true, move |state, _rng, _now| {
            let (fields, history_key, status_key, _) =
                developer_streak_fields_mut(state, key.as_deref())?;
            fields.insert(
                history_key.into(),
                Value::Array(streak_series_ending_at(yesterday, length)),
            );
            fields.insert(status_key.into(), Value::String("Active".into()));
            let max_key = if key.is_some() {
                "max_streak"
            } else {
                "max_global_streak"
            };
            fields.insert(
                max_key.into(),
                json!(integer_field(fields, max_key, 0).max(length)),
            );
            if key.is_some() {
                fields.insert("last_streak_lost_date".into(), Value::Null);
                fields.insert("last_streak_lose_len".into(), json!(0));
            } else {
                fields.insert("last_global_streak_lost_date".into(), Value::Null);
                fields.insert("last_global_streak_lose_len".into(), json!(0));
            }
            Ok((Some(format!("Создана серия стрика на {length} дн.")), None))
        })
    }

    pub fn developer_restore_streak(
        request: DeveloperStreakRequest,
    ) -> GameResult<GameCommandResponse> {
        if !crate::developer_mode_available() {
            return Err(GameError::PrerequisiteMissing(
                "Режим разработчика недоступен в release-сборке.".into(),
            ));
        }
        let connection = crate::open_projects_database().map_err(GameError::Database)?;
        Self::owner(&connection)?;
        let key = validate_developer_streak_target(&connection, &request)?;
        let root = Self::load(&connection)?;
        let fields = if let Some(key) = key.as_deref() {
            root.get("project_game_state")
                .and_then(Value::as_object)
                .and_then(|states| states.get(key))
                .and_then(Value::as_object)
        } else {
            root.get("global_streak").and_then(Value::as_object)
        }
        .ok_or_else(|| GameError::NotFound("Состояние выбранного стрика не найдено.".into()))?;
        let (history_key, loss_key) = if key.is_some() {
            ("streaks", "last_streak_lose_len")
        } else {
            ("global_streaks", "last_global_streak_lose_len")
        };
        let length = restore_streak_length(fields, history_key, loss_key)?;
        let logical_day =
            crate::streaks::logical_writing_day(&connection).map_err(GameError::Database)?;
        let yesterday = crate::streaks::date_days(&logical_day)
            .ok_or_else(|| GameError::Validation("Некорректный писательский день.".into()))?
            - 1;
        Self::mutate_classified(true, move |state, _rng, _now| {
            let (fields, history_key, status_key, _) =
                developer_streak_fields_mut(state, key.as_deref())?;
            fields.insert(
                history_key.into(),
                Value::Array(streak_series_ending_at(yesterday, length)),
            );
            fields.insert(status_key.into(), Value::String("Active".into()));
            let max_key = if key.is_some() {
                "max_streak"
            } else {
                "max_global_streak"
            };
            fields.insert(
                max_key.into(),
                json!(integer_field(fields, max_key, 0).max(length)),
            );
            if key.is_some() {
                fields.insert("last_streak_lost_date".into(), Value::Null);
                fields.insert("last_streak_lose_len".into(), json!(0));
            } else {
                fields.insert("last_global_streak_lost_date".into(), Value::Null);
                fields.insert("last_global_streak_lose_len".into(), json!(0));
            }
            Ok((Some(format!("Стрик восстановлен: {length} дн.")), None))
        })
    }

    pub fn developer_state() -> GameResult<Value> {
        if !crate::developer_mode_available() {
            return Err(GameError::PrerequisiteMissing(
                "Режим разработчика недоступен в release-сборке.".into(),
            ));
        }
        let connection = crate::open_projects_database().map_err(GameError::Database)?;
        Self::owner(&connection)?;
        let payload = Self::load(&connection)?;
        let clock = payload
            .get("game")
            .and_then(|value| value.get("extensions"))
            .or_else(|| payload.get("extensions"))
            .and_then(|value| value.get("developer_clock"));
        let enabled = clock
            .and_then(|value| value.get("enabled"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let datetime = clock
            .and_then(|value| value.get("datetime"))
            .cloned()
            .unwrap_or(Value::Null);
        Ok(json!({"state":Self::state()?,"test_date_enabled":enabled,"test_datetime":datetime}))
    }

    pub fn update_developer(request: DeveloperProfileRequest) -> GameResult<GameCommandResponse> {
        if !crate::developer_mode_available() {
            return Err(GameError::PrerequisiteMissing(
                "Режим разработчика недоступен в release-сборке.".into(),
            ));
        }
        let coins = checked_nonnegative(request.coins, "Монеты")?;
        if !request.health.is_finite()
            || !request.exp.is_finite()
            || request.health < 0.0
            || request.exp < 0.0
        {
            return Err(GameError::Validation(
                "Значения режима разработчика некорректны.".into(),
            ));
        }
        if request.test_date_enabled && request.test_datetime.is_none() {
            return Err(GameError::Validation(
                "Укажите дату и время для тестового режима.".into(),
            ));
        }
        let test_date_enabled = request.test_date_enabled;
        let test_datetime = request.test_datetime.clone();
        Self::mutate_classified(true, move |state, _rng, _now| {
            let gamer = gamer_object(state)?;
            gamer.insert("level".into(), json!(request.level.clamp(1, 999)));
            gamer.insert("coins".into(), json!(coins));
            gamer.insert("exp".into(), json!(request.exp));
            let max = 100.0 + (((request.level.max(1) - 1) / 5) * 10) as f64;
            gamer.insert("max_health".into(), json!(max));
            gamer.insert("health".into(), json!(request.health.min(max)));
            let root = game_object(state)?;
            object_map(root, "extensions").insert(
                "developer_clock".into(),
                json!({"enabled":test_date_enabled,"datetime":test_datetime}),
            );
            Ok((
                Some("Настройки режима разработчика сохранены.".into()),
                None,
            ))
        })
    }

    pub fn grant_inventory(request: InventoryRequest) -> GameResult<GameCommandResponse> {
        if !crate::developer_mode_available() {
            return Err(GameError::PrerequisiteMissing(
                "Режим разработчика недоступен в этой сборке.".into(),
            ));
        }
        let count = checked_count(request.count)?;
        Self::mutate_classified(true, move |state, _rng, _now| {
            let gamer = gamer_object(state)?;
            let item = catalog_item(&request.category, &request.item_id)
                .ok_or_else(|| GameError::NotFound("Предмет не найден.".into()))?;
            let current = item_count(gamer, &request.category, &request.item_id);
            item_map(gamer, &request.category)
                .insert(request.item_id.clone(), json!(current + count));
            Ok((
                Some(format!("Получено: {} x{}.", item.key, count)),
                Some(json!({"category":request.category,"item_key":item.key,"count":count})),
            ))
        })
    }

    pub fn start_session(request: WritingSessionRequest) -> GameResult<GameCommandResponse> {
        if ![15, 25, 45, 60].contains(&request.duration_minutes)
            || request.target_symbols <= 0
            || request.target_symbols > 100_000_000
            || request.intention.chars().count() > 200
        {
            return Err(GameError::Validation(
                "Выберите длительность и положительную цель сессии.".into(),
            ));
        }
        if !["flow", "sprint", "deep", "editing"].contains(&request.mode.as_str()) {
            return Err(GameError::NotFound(
                "Неизвестный режим писательской сессии.".into(),
            ));
        }
        if request.mode == "sprint" && request.duration_minutes != 15 {
            return Err(GameError::Validation(
                "Спринт рассчитан только на 15 минут.".into(),
            ));
        }
        if request.mode == "deep" && ![45, 60].contains(&request.duration_minutes) {
            return Err(GameError::Validation(
                "Глубокая работа рассчитана на 45 или 60 минут.".into(),
            ));
        }
        if request.mode == "editing" && request.intention != "Отредактировать текст"
        {
            return Err(GameError::Validation(
                "Редакторский проход требует намерения отредактировать текст.".into(),
            ));
        }
        Self::mutate(move |state, _rng, now| {
            let gamer = gamer_object(state)?;
            if gamer
                .get("writing_session")
                .is_some_and(|value| !value.is_null())
            {
                return Err(GameError::InvalidState(
                    "Сначала завершите текущую писательскую сессию.".into(),
                ));
            }
            gamer.insert("writing_session".into(), json!({"started_at":now,"clock_source":"wall","duration_minutes":request.duration_minutes,"target_symbols":request.target_symbols,"progress":0,"intention":request.intention,"mode":request.mode}));
            Ok((Some("Писательская сессия начата.".into()), None))
        })
    }

    pub fn finish_session() -> GameResult<GameCommandResponse> {
        Self::mutate(|state, _rng, now| {
            let gamer = gamer_object(state)?;
            let session = gamer
                .remove("writing_session")
                .filter(|value| !value.is_null())
                .ok_or_else(|| {
                    GameError::InvalidState("Нет активной писательской сессии.".into())
                })?;
            let session_object = session
                .as_object()
                .ok_or_else(|| GameError::InvalidState("Некорректная сессия.".into()))?;
            let progress = number_field(session_object, "progress", 0.0).max(0.0);
            let target = number_field(session_object, "target_symbols", 1.0).max(1.0);
            let ratio = progress / target;
            let (grade, multiplier, successful) = if ratio >= 1.5 {
                ("gold", 1.30, true)
            } else if ratio >= 1.25 {
                ("silver", 1.15, true)
            } else if ratio >= 1.0 {
                ("bronze", 1.0, true)
            } else {
                ("failed", 0.0, false)
            };
            let (grade, multiplier) = if successful
                && integer_field(gamer, "session_grade_boosts", 0) > 0
                && grade != "gold"
            {
                gamer.insert("session_grade_boosts".into(), json!(0));
                if grade == "bronze" {
                    ("silver", 1.15)
                } else {
                    ("gold", 1.30)
                }
            } else {
                (grade, multiplier)
            };
            let mut protected = false;
            if !successful {
                let ritualist_protection = gamer.get("specialization").and_then(Value::as_str)
                    == Some("ritualist")
                    && gamer
                        .get("specialization_ability_effects")
                        .and_then(Value::as_object)
                        .and_then(|effects| effects.get("ritualist"))
                        .and_then(Value::as_bool)
                        == Some(true);
                if ritualist_protection {
                    object_map(gamer, "specialization_ability_effects")
                        .insert("ritualist".into(), json!(false));
                    protected = true;
                } else {
                    let shields = integer_field(gamer, "session_streak_shields", 0);
                    if shields > 0 {
                        gamer.insert("session_streak_shields".into(), json!(shields - 1));
                        protected = true;
                    } else {
                        gamer.insert("writing_session_streak".into(), json!(0));
                    }
                }
            }
            let mut coins = 0.0;
            let mut exp = 0.0;
            if successful {
                let mode = text_value(session_object.get("mode"), "flow");
                let mode_bonus = match mode.as_str() {
                    "sprint" => 0.15,
                    "deep" => 0.25,
                    "editing" => 0.20,
                    _ => 0.0,
                };
                let streak = integer_field(gamer, "writing_session_streak", 0) + 1;
                let streak_multiplier = 1.0 + (streak.saturating_sub(1).min(5) as f64) * 0.03;
                let bonus = number_field(gamer, "session_reward_bonus", 0.0);
                let specialization = text_value(gamer.get("specialization"), "");
                let ability_active = gamer
                    .get("specialization_ability_effects")
                    .and_then(Value::as_object)
                    .and_then(|effects| effects.get(&specialization))
                    .and_then(Value::as_bool)
                    == Some(true)
                    && matches!(specialization.as_str(), "ritualist" | "editor");
                let ability_multiplier = if ability_active { 1.30 } else { 1.0 };
                if ability_active {
                    object_map(gamer, "specialization_ability_effects")
                        .insert(specialization, json!(false));
                }
                let reward = (1.0 + bonus)
                    * (1.0 + mode_bonus)
                    * multiplier
                    * streak_multiplier
                    * ability_multiplier;
                coins = rounded_money(25.0 * reward);
                exp = (250.0 * reward).round();
                add_number(gamer, "coins", coins);
                add_experience(gamer, exp);
                add_capped(gamer, "inspiration", 10.0, 100.0);
                gamer.insert("writing_session_streak".into(), json!(streak));
                gamer.insert("session_reward_bonus".into(), json!(0.0));
            }
            let history = array_mut(gamer, "writing_session_history");
            history.push(json!({"finished_at":now,"mode":session_object.get("mode").cloned().unwrap_or(json!("flow")),"intention":session_object.get("intention").cloned().unwrap_or(json!("")),"duration_minutes":session_object.get("duration_minutes").cloned().unwrap_or(json!(0)),"target_symbols":session_object.get("target_symbols").cloned().unwrap_or(json!(0)),"progress":progress,"grade":grade,"successful":successful,"coins":coins,"exp":exp}));
            if history.len() > 20 {
                history.drain(0..history.len() - 20);
            }
            let message = if successful {
                format!(
                    "Сессия завершена! Получено {} монет, {} опыта и 10 вдохновения.",
                    coins, exp
                )
            } else if protected {
                "Сессия завершена. Цель не достигнута — Нить ритуала сохранила серию.".into()
            } else {
                "Сессия завершена. Цель не достигнута — штрафа нет.".into()
            };
            Ok((
                Some(message),
                Some(json!({"successful":successful,"grade":grade,"coins":coins,"experience":exp})),
            ))
        })
    }

    pub fn cancel_session() -> GameResult<GameCommandResponse> {
        Self::mutate(|state, _rng, _now| {
            gamer_object(state)?
                .remove("writing_session")
                .filter(|value| !value.is_null())
                .ok_or_else(|| {
                    GameError::InvalidState("Нет активной писательской сессии.".into())
                })?;
            Ok((
                Some("Писательская сессия отменена без штрафа.".into()),
                None,
            ))
        })
    }

    pub fn select_daily(request: DailyChallengeRequest) -> GameResult<GameCommandResponse> {
        Self::mutate(move |state, _rng, _now| {
            let gamer = gamer_object(state)?;
            let options = gamer
                .get("daily_challenge_options")
                .and_then(Value::as_array)
                .ok_or_else(|| GameError::NotFound("Неизвестный вариант цели дня.".into()))?;
            let option = options
                .iter()
                .find(|value| {
                    value.get("option_id").and_then(Value::as_str)
                        == Some(request.option_id.as_str())
                })
                .cloned()
                .ok_or_else(|| GameError::NotFound("Неизвестный вариант цели дня.".into()))?;
            let current = gamer.get("daily_challenge").and_then(Value::as_object);
            if current
                .and_then(|value| value.get("completed"))
                .and_then(Value::as_bool)
                == Some(true)
            {
                return Err(GameError::AlreadyClaimed(
                    "Выполненную цель дня заменить нельзя.".into(),
                ));
            }
            if current
                .and_then(|value| value.get("option_id"))
                .and_then(Value::as_str)
                == Some(request.option_id.as_str())
            {
                return Err(GameError::InvalidState(
                    "Этот вариант цели дня уже выбран.".into(),
                ));
            }
            if number_field(gamer, "inspiration", 0.0) < 15.0 {
                return Err(GameError::InsufficientFunds(
                    "Недостаточно вдохновения для смены цели дня.".into(),
                ));
            }
            add_number(gamer, "inspiration", -15.0);
            gamer.insert("daily_challenge".into(), option);
            Ok((Some("Выбрана новая цель дня.".into()), None))
        })
    }

    pub fn start_weekly(request: WeeklyChallengeRequest) -> GameResult<GameCommandResponse> {
        if !["symbols", "days", "sessions", "editing"].contains(&request.challenge_id.as_str()) {
            return Err(GameError::NotFound(
                "Неизвестное недельное испытание.".into(),
            ));
        }
        Self::mutate(move |state, _rng, now| {
            let gamer = gamer_object(state)?;
            if gamer
                .get("weekly_challenge")
                .is_some_and(|value| !value.is_null())
            {
                return Err(GameError::AlreadyClaimed(
                    "Недельное испытание уже выбрано.".into(),
                ));
            }
            let week_start = iso_epoch_seconds(now)
                .map(|seconds| {
                    let days = seconds.div_euclid(86_400);
                    let monday = days - (days + 3).rem_euclid(7);
                    iso_date_from_days(monday)
                })
                .unwrap_or_else(|| now.get(..10).unwrap_or(now).to_string());
            gamer.insert("weekly_challenge".into(),json!({"key":request.challenge_id,"week_start":week_start,"progress":0,"writing_days":[],"completed":false}));
            Ok((Some("Начато недельное испытание.".into()), None))
        })
    }

    pub fn activate_inspiration(ability_id: String) -> GameResult<GameCommandResponse> {
        let (cost, field) = match ability_id.as_str() {
            "creative_surge" => (30, "writing_reward_bonus"),
            "session_spark" => (25, "session_reward_bonus"),
            "challenge_focus" => (40, "challenge_reward_bonus"),
            _ => {
                return Err(GameError::NotFound(
                    "Неизвестная способность вдохновения.".into(),
                ))
            }
        };
        Self::mutate(move |state, _rng, _now| {
            let gamer = gamer_object(state)?;
            if number_field(gamer, field, 0.0) > 0.0 {
                return Err(GameError::AlreadyClaimed(
                    "Этот эффект вдохновения уже активен.".into(),
                ));
            }
            if number_field(gamer, "inspiration", 0.0) < cost as f64 {
                return Err(GameError::InsufficientFunds(
                    "Недостаточно вдохновения для этой способности.".into(),
                ));
            }
            add_number(gamer, "inspiration", -(cost as f64));
            gamer.insert(field.into(), json!(0.25));
            Ok((Some("Способность вдохновения активирована.".into()), None))
        })
    }

    pub fn resolve_creative(request: ChoiceRequest) -> GameResult<GameCommandResponse> {
        if request.choice != "safe" && request.choice != "risk" {
            return Err(GameError::Validation(
                "Неизвестный выбор творческого события.".into(),
            ));
        }
        Self::mutate(move |state, rng, now| {
            let gamer = gamer_object(state)?;
            let event = gamer
                .get("pending_creative_event")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    GameError::InvalidState("Нет творческого события, ожидающего решения.".into())
                })?
                .to_string();
            let success = request.choice == "safe" || rng.uniform_u32(100) < 55;
            if success {
                add_capped(
                    gamer,
                    "inspiration",
                    if request.choice == "safe" { 10.0 } else { 5.0 },
                    100.0,
                );
            } else {
                add_number(
                    gamer,
                    "inspiration",
                    -number_field(gamer, "inspiration", 0.0).min(5.0),
                );
            }
            let history = array_mut(gamer, "creative_event_history");
            history.push(
                json!({"event":event,"choice":request.choice,"success":success,"resolved_at":now}),
            );
            if history.len() > 20 {
                history.drain(0..history.len() - 20);
            }
            gamer.remove("pending_creative_event");
            Ok((
                Some("Творческое событие завершено.".into()),
                Some(json!({"success":success})),
            ))
        })
    }

    pub fn select_specialization(id: String) -> GameResult<GameCommandResponse> {
        if !["marathoner", "ritualist", "finisher", "explorer", "editor"].contains(&id.as_str()) {
            return Err(GameError::NotFound("Неизвестная специализация.".into()));
        }
        Self::mutate(move |state, _rng, now| {
            let gamer = gamer_object(state)?;
            if integer_field(gamer, "level", 1) < 3 {
                return Err(GameError::PrerequisiteMissing(
                    "Специализации открываются на 3 уровне.".into(),
                ));
            }
            if let Some(changed_at) = gamer
                .get("specialization_changed_at")
                .map(|value| text_value(Some(value), ""))
                .filter(|value| !value.is_empty())
            {
                if iso_epoch_seconds(now)
                    .zip(iso_epoch_seconds(&changed_at))
                    .is_some_and(|(current, previous)| current < previous + 14 * 86_400)
                {
                    return Err(GameError::CooldownActive(
                        "Сменить специализацию можно через 14 дней.".into(),
                    ));
                }
            }
            if gamer.get("specialization").and_then(Value::as_str) == Some(id.as_str()) {
                return Err(GameError::AlreadyClaimed(
                    "Эта специализация уже выбрана.".into(),
                ));
            }
            gamer.insert("specialization".into(), json!(id));
            gamer.insert("specialization_changed_at".into(), json!(now));
            object_map(gamer, "specialization_mastery")
                .entry(id.clone())
                .or_insert(json!(0));
            Ok((Some("Специализация выбрана.".into()), None))
        })
    }

    pub fn activate_specialization() -> GameResult<GameCommandResponse> {
        Self::mutate(|state, _rng, now| {
            let gamer = gamer_object(state)?;
            let specialization = text_value(gamer.get("specialization"), "");
            if specialization.is_empty() {
                return Err(GameError::PrerequisiteMissing(
                    "Сначала выберите специализацию.".into(),
                ));
            }
            if let Some(ready_at) = gamer
                .get("specialization_ability_ready_at")
                .and_then(Value::as_object)
                .and_then(|ready| ready.get(&specialization))
                .map(|value| text_value(Some(value), ""))
                .filter(|value| !value.is_empty())
            {
                if iso_epoch_seconds(now)
                    .zip(iso_epoch_seconds(&ready_at))
                    .is_some_and(|(current, activated)| current < activated + 86_400)
                {
                    return Err(GameError::CooldownActive(
                        "Умение специализации ещё не восстановилось.".into(),
                    ));
                }
            }
            let effects = object_map(gamer, "specialization_ability_effects");
            if effects
                .get(&specialization)
                .is_some_and(|value| value.as_bool().unwrap_or(false))
            {
                return Err(GameError::AlreadyClaimed(
                    "Эффект активного умения уже ожидает применения.".into(),
                ));
            }
            effects.insert(specialization.clone(), json!(true));
            object_map(gamer, "specialization_ability_ready_at").insert(specialization, json!(now));
            Ok((Some("Умение специализации активировано.".into()), None))
        })
    }

    pub fn increase_skill(request: SkillRequest) -> GameResult<GameCommandResponse> {
        let points = checked_count(request.points)?;
        Self::mutate(move |state, _rng, _now| {
            let gamer = gamer_object(state)?;
            if !["productivity", "profitability", "endurance"].contains(&request.skill_id.as_str()) {
                return Err(GameError::NotFound("Неизвестное умение.".into()));
            }
            if integer_field(gamer, "available_skill_points", 0) < points {
                return Err(GameError::InsufficientFunds(
                    "Недостаточно доступных баллов умений.".into(),
                ));
            }
            let skills = object_map(gamer, "skills");
            let old = skills
                .get(&request.skill_id)
                .and_then(Value::as_i64)
                .unwrap_or(0);
            skills.insert(request.skill_id.clone(), json!(old + points));
            gamer.insert(
                "available_skill_points".into(),
                json!(integer_field(gamer, "available_skill_points", 0) - points),
            );
            Ok((Some(format!("Умение увеличено на {points}.")), None))
        })
    }

    pub fn quest(id: String, start: bool) -> GameResult<GameCommandResponse> {
        Self::mutate(move |state, _rng, now| {
            let gamer = gamer_object(state)?;
            let quests = array_mut(gamer, "quests");
            let quest = quests
                .iter_mut()
                .find(|value| {
                    tagged_field(value, "quest_id").and_then(Value::as_str) == Some(id.as_str())
                })
                .ok_or_else(|| GameError::NotFound("Квест не найден.".into()))?;
            let fields = tagged_fields(quest)
                .ok_or_else(|| GameError::InvalidState("Некорректный квест.".into()))?;
            let status = text_value(fields.get("status"), "Available");
            if start {
                if status != "Available" {
                    return Err(GameError::AlreadyClaimed(
                        "Квест уже запущен или завершен.".into(),
                    ));
                }
                fields.insert("status".into(), json!("Active"));
                fields.insert("start_date".into(), json!(now));
                Ok((Some("Квест начат.".into()), None))
            } else {
                if status != "Active" {
                    return Err(GameError::InvalidState("Активный квест не найден.".into()));
                }
                fields.insert("status".into(), json!("Available"));
                fields.insert("start_date".into(), Value::Null);
                Ok((Some("Квест возвращен в доступные.".into()), None))
            }
        })
    }

    pub fn freeze(request: FreezeRequest) -> GameResult<GameCommandResponse> {
        if request.target != "global" && request.target != "project" {
            return Err(GameError::Validation(
                "Неизвестная цель заморозки стрика.".into(),
            ));
        }
        if request.target == "project" && request.project_id.as_deref().unwrap_or("").is_empty() {
            return Err(GameError::Validation(
                "Выберите проект для заморозки.".into(),
            ));
        }
        Self::mutate(move |state, _rng, now| {
            let gamer = gamer_object(state)?;
            let count = item_count(gamer, "Предметы", "Заморозка");
            if count < 1 {
                return Err(GameError::InsufficientFunds(
                    "В инвентаре нет заморозки.".into(),
                ));
            }
            let items = item_map(gamer, "Предметы");
            if count == 1 {
                items.remove("Заморозка")
            } else {
                items.insert("Заморозка".into(), json!(count - 1))
            };
            Ok((
                Some(if request.target == "global" {
                    "Глобальный стрик заморожен!".into()
                } else {
                    "Проект заморожен!".into()
                }),
                Some(
                    json!({"target":request.target,"project_id":request.project_id,"date":now.get(..10).unwrap_or(now)}),
                ),
            ))
        })
    }

    pub fn mark_notification(id: String, all: bool) -> GameResult<Value> {
        let response = Self::mutate(move |state, _rng, _now| {
            let root = game_object(state)?;
            let notifications = object_map(root, "notifications");
            let mut unread_values = std::mem::take(array_mut(notifications, "new"));
            let read = array_mut(notifications, "read");
            if all {
                while let Some(value) = unread_values.pop() {
                    read.insert(0, value);
                }
            } else {
                let index = unread_values
                    .iter()
                    .enumerate()
                    .find(|(index, value)| {
                        notification_json(value, "new", *index)["id"].as_str() == Some(id.as_str())
                    })
                    .map(|(index, _)| index)
                    .ok_or_else(|| GameError::NotFound("Уведомление не найдено.".into()))?;
                let value = unread_values.remove(index);
                read.insert(0, value);
            }
            *array_mut(notifications, "new") = unread_values;
            Ok((None, None))
        })?;
        Ok(response.state["notifications"].clone())
    }

    pub fn custom_award(request: CustomAwardRequest) -> GameResult<GameCommandResponse> {
        if request.name.trim().is_empty() || request.name.chars().count() > 200 {
            return Err(GameError::Validation(
                "Название награды должно содержать от 1 до 200 символов.".into(),
            ));
        }
        let price = checked_positive(request.price, "Цена награды")?;
        Self::mutate(move |state, _rng, _now| {
            let gamer = gamer_object(state)?;
            let awards = array_mut(gamer, "custom_awards");
            let id = format!("custom-{}", awards.len() + 1);
            awards.push(json!({"__type__":"game_data.Item","fields":{"award_id":id,"name":request.name,"_price":price,"apply_inflation":request.apply_inflation,"description":"Кастомная награда без эффекта","available_in_shop":true,"sellable":true,"item_type":"Награды"}}));
            object_map(gamer, "custom_awards_inventory")
                .entry(request.name.clone())
                .or_insert(json!(0));
            Ok((
                Some("Награда создана.".into()),
                Some(json!({"award_id":id,"name":request.name,"price":price})),
            ))
        })
    }

    pub fn update_custom_award(
        request: CustomAwardUpdateRequest,
    ) -> GameResult<GameCommandResponse> {
        let price = match request.price {
            Some(value) => Some(checked_positive(value, "Цена награды")?),
            None => None,
        };
        if request
            .name
            .as_ref()
            .is_some_and(|name| name.trim().is_empty())
        {
            return Err(GameError::Validation(
                "Название награды не может быть пустым.".into(),
            ));
        }
        Self::mutate(move |state, _rng, _now| {
            let gamer = gamer_object(state)?;
            let awards = array_mut(gamer, "custom_awards");
            let award = awards
                .iter_mut()
                .find(|value| {
                    tagged_field(value, "award_id").and_then(Value::as_str)
                        == Some(request.award_id.as_str())
                })
                .ok_or_else(|| GameError::NotFound("Кастомная награда не найдена.".into()))?;
            let fields = tagged_fields(award)
                .ok_or_else(|| GameError::InvalidState("Некорректная кастомная награда.".into()))?;
            if let Some(name) = request.name {
                fields.insert("name".into(), json!(name));
            }
            if let Some(price) = price {
                fields.insert("_price".into(), json!(price));
            }
            if let Some(apply_inflation) = request.apply_inflation {
                fields.insert("apply_inflation".into(), json!(apply_inflation));
            }
            Ok((Some("Награда изменена.".into()), None))
        })
    }

    pub fn delete_custom_award(id: String) -> GameResult<GameCommandResponse> {
        Self::mutate(move |state, _rng, _now| {
            let gamer = gamer_object(state)?;
            let awards = array_mut(gamer, "custom_awards");
            let index = awards
                .iter()
                .position(|value| {
                    tagged_field(value, "award_id").and_then(Value::as_str) == Some(id.as_str())
                })
                .ok_or_else(|| GameError::NotFound("Кастомная награда не найдена.".into()))?;
            awards.remove(index);
            Ok((Some("Награда удалена.".into()), None))
        })
    }

    pub fn custom_inventory(
        id: String,
        count: i64,
        operation: &str,
    ) -> GameResult<GameCommandResponse> {
        let count = checked_count(count)?;
        Self::mutate(move |state, _rng, _now| {
            let gamer = gamer_object(state)?;
            let (name, price) = gamer
                .get("custom_awards")
                .and_then(Value::as_array)
                .and_then(|items| {
                    items.iter().find(|value| {
                        tagged_field(value, "award_id").and_then(Value::as_str) == Some(id.as_str())
                    })
                })
                .map(|value| {
                    let fields = tagged_fields_immutable(value)
                        .expect("custom award fields were validated when saved");
                    (
                        text_value(fields.get("name"), ""),
                        custom_award_price(gamer, fields),
                    )
                })
                .ok_or_else(|| GameError::NotFound("Кастомная награда не найдена.".into()))?;
            let current = gamer
                .get("custom_awards_inventory")
                .and_then(Value::as_object)
                .and_then(|inventory| inventory.get(&name))
                .and_then(Value::as_i64)
                .unwrap_or(0);
            match operation {
                "buy" => {
                    let total = rounded_money(price * count as f64);
                    if number_field(gamer, "coins", 0.0) < total {
                        return Err(GameError::InsufficientFunds("Недостаточно монет!".into()));
                    }
                    add_number(gamer, "coins", -total);
                    object_map(gamer, "custom_awards_inventory")
                        .insert(name, json!(current + count));
                }
                "sell" => {
                    if current < count {
                        return Err(GameError::InvalidQuantity(
                            "Недостаточно наград в инвентаре.".into(),
                        ));
                    }
                    object_map(gamer, "custom_awards_inventory")
                        .insert(name, json!(current - count));
                    add_number(gamer, "coins", rounded_money(price * 0.75 * count as f64));
                }
                "use" => {
                    if current < count {
                        return Err(GameError::InvalidQuantity(
                            "Недостаточно наград в инвентаре.".into(),
                        ));
                    }
                    object_map(gamer, "custom_awards_inventory")
                        .insert(name, json!(current - count));
                }
                _ => {
                    return Err(GameError::Validation(
                        "Unknown custom award operation".into(),
                    ))
                }
            }
            Ok((Some("Операция с наградой выполнена.".into()), None))
        })
    }

    pub fn bank_open(
        request: BankProductRequest,
        deposit: bool,
    ) -> GameResult<GameCommandResponse> {
        let amount = checked_positive(request.amount, "Сумма")?;
        if request.days <= 0 || request.days > 36500 {
            return Err(GameError::Validation(
                "Срок должен быть от 1 до 36500 дней.".into(),
            ));
        }
        Self::mutate(move |state, _rng, now| {
            let gamer = gamer_object(state)?;
            if deposit && number_field(gamer, "coins", 0.0) < amount {
                return Err(GameError::InsufficientFunds("Недостаточно монет.".into()));
            }
            if !deposit && integer_field(gamer, "level", 1) < 3 {
                return Err(GameError::PrerequisiteMissing(
                    "Кредиты доступны с 3 уровня.".into(),
                ));
            }
            let key = if deposit { "deposit" } else { "credit" };
            let active = gamer
                .get("bank_account")
                .and_then(tagged_fields_immutable)
                .and_then(|fields| fields.get(key))
                .is_some_and(|value| !value.is_null());
            if active {
                return Err(GameError::AlreadyClaimed(
                    "Банковский продукт уже активен.".into(),
                ));
            }
            add_number(gamer, "coins", if deposit { -amount } else { amount });
            let product = if deposit {
                json!({"__type__":"game_data.Deposit","fields":{"give_date":now,"deposit_sum":amount,"days_until_return":request.days,"interest_rate_on_deposit":1.0,"allow_interest_withdrawal":request.allow_interest_withdrawal.unwrap_or(true),"interest":0.0}})
            } else {
                json!({"__type__":"game_data.Credit","fields":{"take_date":now,"credit_sum":amount,"days_until_return":request.days,"interest_rate_on_loan":2.0,"paid_amount":0.0,"accrued_penalty":0.0}})
            };
            let account = gamer
                .entry("bank_account")
                .or_insert_with(|| json!({"__type__":"game_data.BankAccount","fields":{}}));
            tagged_fields(account)
                .ok_or_else(|| GameError::InvalidState("Некорректный банковский счёт.".into()))?
                .insert(key.into(), product);
            Ok((
                Some(if deposit {
                    "Вклад открыт.".into()
                } else {
                    "Кредит открыт.".into()
                }),
                Some(json!({"amount":amount,"days":request.days})),
            ))
        })
    }

    pub fn bank_amount(
        request: BankAmountRequest,
        deposit: bool,
    ) -> GameResult<GameCommandResponse> {
        let amount = checked_positive(request.amount, "Сумма")?;
        Self::mutate(move |state, _rng, _now| {
            let gamer = gamer_object(state)?;
            if number_field(gamer, "coins", 0.0) < amount {
                return Err(GameError::InsufficientFunds("Недостаточно монет.".into()));
            }
            let key = if deposit { "deposit" } else { "credit" };
            let new_value = {
                let bank = gamer
                    .get_mut("bank_account")
                    .and_then(tagged_fields)
                    .ok_or_else(|| GameError::InvalidState("Банковский счёт не найден.".into()))?;
                let product = bank
                    .get_mut(key)
                    .filter(|value| !value.is_null())
                    .ok_or_else(|| {
                        GameError::InvalidState("Активный банковский продукт не найден.".into())
                    })?;
                let fields = tagged_fields(product).ok_or_else(|| {
                    GameError::InvalidState("Некорректный банковский продукт.".into())
                })?;
                if deposit {
                    rounded_money(number_field(fields, "deposit_sum", 0.0) + amount)
                } else {
                    rounded_money(number_field(fields, "paid_amount", 0.0) + amount)
                }
            };
            if deposit {
                add_number(gamer, "coins", -amount);
                let fields = gamer
                    .get_mut("bank_account")
                    .and_then(tagged_fields)
                    .expect("bank fields");
                let product = fields.get_mut(key).expect("bank product");
                tagged_fields(product)
                    .expect("product fields")
                    .insert("deposit_sum".into(), json!(new_value));
            } else {
                add_number(gamer, "coins", -amount);
                let fields = gamer
                    .get_mut("bank_account")
                    .and_then(tagged_fields)
                    .expect("bank fields");
                let product = fields.get_mut(key).expect("bank product");
                tagged_fields(product)
                    .expect("product fields")
                    .insert("paid_amount".into(), json!(new_value));
            }
            Ok((
                Some("Банковская операция выполнена.".into()),
                Some(json!({"amount":amount})),
            ))
        })
    }

    pub fn bank_close(
        deposit: bool,
        interest_only: bool,
        allow_early: bool,
    ) -> GameResult<GameCommandResponse> {
        Self::mutate(move |state, _rng, now| {
            let gamer = gamer_object(state)?;
            let key = if deposit { "deposit" } else { "credit" };
            let value = {
                let bank = gamer
                    .get_mut("bank_account")
                    .and_then(tagged_fields)
                    .ok_or_else(|| GameError::InvalidState("Банковский счёт не найден.".into()))?;
                let product = bank
                    .get_mut(key)
                    .filter(|value| !value.is_null())
                    .ok_or_else(|| {
                        GameError::InvalidState("Активный банковский продукт не найден.".into())
                    })?;
                let fields = tagged_fields(product).ok_or_else(|| {
                    GameError::InvalidState("Некорректный банковский продукт.".into())
                })?;
                if deposit {
                    let opened = text_value(fields.get("give_date"), "");
                    let days = integer_field(fields, "days_until_return", 1).max(1);
                    let matured = iso_epoch_seconds(&opened)
                        .zip(iso_epoch_seconds(now))
                        .is_some_and(|(opened, current)| current >= opened + days * 86_400);
                    if !interest_only && !matured && !allow_early {
                        return Err(GameError::CooldownActive(
                            "Срок вклада еще не наступил.".into(),
                        ));
                    }
                    if interest_only {
                        number_field(fields, "interest", 0.0)
                    } else if allow_early && !matured {
                        number_field(fields, "deposit_sum", 0.0)
                    } else {
                        number_field(fields, "deposit_sum", 0.0)
                            + number_field(fields, "interest", 0.0)
                    }
                } else {
                    let principal = number_field(fields, "credit_sum", 0.0);
                    let days = integer_field(fields, "days_until_return", 1).max(1);
                    let rate = number_field(fields, "interest_rate_on_loan", 2.0).max(0.0);
                    let growth = (1.0 + rate / 100.0).powi(days as i32);
                    let interest = rounded_money(
                        rounded_money(principal * (rate / 100.0) * growth / (growth - 1.0))
                            * days as f64
                            - principal,
                    );
                    principal + interest + number_field(fields, "accrued_penalty", 0.0)
                        - number_field(fields, "paid_amount", 0.0)
                }
            };
            if !deposit && number_field(gamer, "coins", 0.0) < value {
                return Err(GameError::InsufficientFunds(
                    "Недостаточно монет для погашения.".into(),
                ));
            }
            if !deposit {
                add_number(gamer, "coins", -value);
            } else {
                add_number(gamer, "coins", value);
            }
            if !interest_only {
                gamer
                    .get_mut("bank_account")
                    .and_then(tagged_fields)
                    .expect("bank fields")
                    .insert(key.into(), Value::Null);
            }
            Ok((
                Some("Банковская операция выполнена.".into()),
                Some(json!({"amount":value})),
            ))
        })
    }
}

fn add_capped(gamer: &mut Map<String, Value>, key: &str, amount: f64, cap: f64) {
    let value = (number_field(gamer, key, 0.0) + amount).min(cap);
    gamer.insert(
        key.into(),
        if key.contains("streak") || key.contains("boost") || key.contains("mastery") {
            json!(value as i64)
        } else {
            json!(value)
        },
    );
}

fn lottery_draw<R: GameRng>(rng: &mut R) -> Value {
    fn draw_numbers<R: GameRng>(rng: &mut R) -> Vec<u32> {
        let mut numbers = Vec::new();
        while numbers.len() < 5 {
            let value = rng.uniform_u32(30) + 1;
            if !numbers.contains(&value) {
                numbers.push(value);
            }
        }
        numbers.sort_unstable();
        numbers
    }

    let player_numbers = draw_numbers(rng);
    let winning_numbers = draw_numbers(rng);
    let matches = player_numbers
        .iter()
        .filter(|number| winning_numbers.contains(number))
        .count() as i64;
    let multiplier = match matches {
        2 => 2,
        3 => 10,
        4 => 250,
        5 => 10_000,
        _ => 0,
    };
    let prize = 10 * multiplier;
    json!({
        "player_numbers": player_numbers,
        "winning_numbers": winning_numbers,
        "matches": matches,
        "prize": prize,
    })
}

#[derive(Debug, serde::Serialize)]
pub struct GameCommandResponse {
    pub ok: bool,
    pub message: Option<String>,
    pub messages: Vec<String>,
    pub result: Option<Value>,
    pub state: Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FixedRng {
        values: Vec<u32>,
        index: usize,
    }

    fn loss_database(health: f64, potions: u64) -> (Connection, std::path::PathBuf) {
        let path = std::env::temp_dir().join(format!(
            "game-loss-{}.db",
            crate::project_metadata_sync::new_event_id().unwrap()
        ));
        let db = crate::sqlite::open_database(&path).unwrap();
        let mut source = json!({"gamer":serde_json::from_str::<Value>(include_str!("../../src/cloud/__fixtures__/gameLegacyDefaultsV1.json")).unwrap(),"global_streak":{"global_streaks":["2026-10-01","2026-10-02","2026-10-03"],"max_global_streak":3,"global_streak_status":"Go"}});
        source["gamer"]["health"] = json!(health);
        source["gamer"]["items"]["Зелья"] =
            json!({"Зелье воскрешения":potions,"Микро зелье здоровья":2});
        db.execute(
            "INSERT INTO game_state VALUES(1,2,?1,'now')",
            [source.to_string()],
        )
        .unwrap();
        (db, path)
    }
    fn loss_state(db: &Connection) -> Value {
        serde_json::from_str(
            &db.query_row("SELECT payload_json FROM game_state", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap(),
        )
        .unwrap()
    }
    #[test]
    fn global_loss_calendar_once_restart_and_new_independent_series() {
        let (mut db, path) = loss_database(100.0, 0);
        for day in ["2026-10-03", "2026-10-04"] {
            assert!(!refresh_global_loss(&db, day, "2026-10-04T00:00:00.000000Z").unwrap());
        }
        {
            let tx = db.transaction().unwrap();
            assert!(refresh_global_loss(&tx, "2026-10-05", "2026-10-05T00:00:00.000000Z").unwrap());
            tx.commit().unwrap();
        }
        assert_eq!(loss_state(&db)["gamer"]["health"], 85.0);
        assert_eq!(
            loss_state(&db)["global_streak"]["last_global_streak_lose_len"],
            3
        );
        assert_eq!(
            loss_state(&db)["notifications"]["new"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert!(loss_state(&db)["notifications"]["new"][0]["fields"]["text"]
            .as_str()
            .unwrap()
            .contains("Урон: 15 HP. Здоровье: 100 → 85 HP"));
        let fields = loss_state(&db)["global_streak"]
            .as_object()
            .unwrap()
            .clone();
        assert_eq!(
            crate::streaks::canonical_global_status(&fields, "2026-10-05"),
            "Lose 3"
        );
        assert_eq!(
            crate::streaks::canonical_global_status(&fields, "2026-10-06"),
            "No"
        );
        for day in ["2026-10-05", "2026-10-06", "2026-10-10"] {
            assert!(!refresh_global_loss(&db, day, "2026-10-10T00:00:00.000000Z").unwrap());
        }
        drop(db);
        let mut db = crate::sqlite::open_database(&path).unwrap();
        assert_eq!(loss_state(&db)["gamer"]["health"], 85.0);
        assert_eq!(
            loss_state(&db)["notifications"]["new"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let mut next = loss_state(&db);
        next["global_streak"]["global_streaks"] = json!(["2026-10-11", "2026-10-12"]);
        db.execute("UPDATE game_state SET payload_json=?1", [next.to_string()])
            .unwrap();
        {
            let tx = db.transaction().unwrap();
            assert!(refresh_global_loss(&tx, "2026-10-14", "2026-10-14T00:00:00.000000Z").unwrap());
            tx.commit().unwrap();
        }
        assert_eq!(loss_state(&db)["gamer"]["health"], 75.0);
        assert_eq!(
            loss_state(&db)["gamer"]["items"]["Зелья"]["Микро зелье здоровья"],
            2
        );
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn global_loss_lethal_damage_revives_once_or_clamps_to_zero() {
        for (health, potions, expected, left) in [
            (10.0, 2, 100.0, 1),
            (15.0, 1, 100.0, 0),
            (10.0, 0, 0.0, 0),
            (0.0, 0, 0.0, 0),
        ] {
            let (mut db, path) = loss_database(health, potions);
            {
                let tx = db.transaction().unwrap();
                assert!(
                    refresh_global_loss(&tx, "2026-10-05", "2026-10-05T00:00:00.000000Z").unwrap()
                );
                tx.commit().unwrap();
            }
            assert_eq!(loss_state(&db)["gamer"]["health"], expected);
            assert_eq!(
                loss_state(&db)["notifications"]["new"]
                    .as_array()
                    .unwrap()
                    .len(),
                if potions > 0 { 2 } else { 1 }
            );
            if potions > 0 {
                assert!(loss_state(&db)["notifications"]["new"][1]["fields"]["text"]
                    .as_str()
                    .unwrap()
                    .contains("Зелье воскрешения"));
            }
            assert_eq!(
                loss_state(&db)["gamer"]["items"]["Зелья"]["Зелье воскрешения"],
                left
            );
            assert!(
                !refresh_global_loss(&db, "2026-10-06", "2026-10-06T00:00:00.000000Z").unwrap()
            );
            assert_eq!(
                loss_state(&db)["gamer"]["items"]["Зелья"]["Зелье воскрешения"],
                left
            );
            drop(db);
            std::fs::remove_file(path).unwrap();
        }
    }
    #[test]
    fn developer_restore_uses_lost_length_and_ends_on_logical_yesterday() {
        let global = json!({
            "global_streaks": [], "last_global_streak_lose_len": 149,
            "last_global_streak_lost_date": tagged_streak_day(20_000),
            "max_global_streak": 120,
        })
        .as_object()
        .unwrap()
        .clone();
        let length =
            restore_streak_length(&global, "global_streaks", "last_global_streak_lose_len")
                .unwrap();
        let history = Value::Array(streak_series_ending_at(20_100, length));
        let (last, actual_length, freeze) = crate::streaks::streak_summary(Some(&history));

        assert_eq!(length, 149);
        assert_eq!(last, Some(20_100));
        assert_eq!(actual_length, 149);
        assert!(!freeze);
    }

    #[test]
    fn developer_series_is_continuous_and_never_creates_today_or_freezes() {
        let yesterday = crate::streaks::date_days("2026-09-18").unwrap();
        let history = Value::Array(streak_series_ending_at(yesterday, 10));
        let entries = history.as_array().unwrap();
        let (last, length, freeze) = crate::streaks::streak_summary(Some(&history));

        assert_eq!(entries.len(), 10);
        assert!(entries
            .iter()
            .all(|entry| entry.get("__type__") == Some(&json!("date"))));
        assert_eq!(last, Some(yesterday));
        assert_eq!(length, 10);
        assert!(!freeze);
    }

    #[test]
    fn developer_local_targets_use_stable_project_and_stage_keys() {
        assert_eq!(
            developer_streak_key("project", Some("project-1"), None).unwrap(),
            Some("project:project-1".into()),
        );
        assert_eq!(
            developer_streak_key("stage", Some("project-1"), Some("stage-2")).unwrap(),
            Some("stage:project-1:stage-2".into()),
        );
        assert!(developer_streak_key("stage", Some("project-1"), None).is_err());
    }

    #[test]
    fn custom_award_price_keeps_base_price_and_matches_purchase_price() {
        let gamer = serde_json::json!({"level": 4}).as_object().unwrap().clone();
        let award = serde_json::json!({
            "_price": 40.0,
            "apply_inflation": true,
        })
        .as_object()
        .unwrap()
        .clone();

        assert_eq!(custom_award_base_price(&award), 40.0);
        assert_eq!(custom_award_price(&gamer, &award), 58.0);
        assert_eq!(custom_award_base_price(&award), 40.0);
    }

    impl GameRng for FixedRng {
        fn uniform_u32(&mut self, upper_exclusive: u32) -> u32 {
            let value = self.values[self.index % self.values.len()];
            self.index += 1;
            value % upper_exclusive
        }
    }

    #[test]
    fn progress_event_is_deterministic_and_completion_is_idempotent() {
        let mut state = json!({"gamer":{"coins":0,"exp":0,"complete_bonus_projects":[]}});
        apply_event(
            &mut state,
            "ProgressAdded",
            "p",
            100.0,
            &Map::new(),
            "project",
        )
        .unwrap();
        let context =
            serde_json::from_value(json!({"key":"project","total_symbols":1000})).unwrap();
        apply_event(
            &mut state,
            "ProjectCompleted",
            "c",
            0.0,
            &context,
            "project",
        )
        .unwrap();
        let once = state.clone();
        apply_event(
            &mut state,
            "ProjectCompleted",
            "c",
            0.0,
            &context,
            "project",
        )
        .unwrap();
        assert_eq!(state, once);
    }

    #[test]
    fn lottery_uses_injected_rng_and_never_duplicates_numbers() {
        let mut rng = FixedRng {
            values: vec![0, 1, 2, 3, 4, 0, 1, 2, 3, 4],
            index: 0,
        };
        let draw = lottery_draw(&mut rng);
        let player_numbers = draw["player_numbers"].as_array().expect("player numbers");
        let winning_numbers = draw["winning_numbers"].as_array().expect("winning numbers");
        assert_eq!(player_numbers.len(), 5);
        assert_eq!(winning_numbers.len(), 5);
        assert_eq!(
            player_numbers
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len(),
            5
        );
        assert_eq!(draw["matches"], json!(5));
        assert_eq!(draw["prize"], json!(100_000));
        assert!(player_numbers.iter().all(|number| number
            .as_u64()
            .is_some_and(|number| (1..=30).contains(&number))));
    }

    #[test]
    fn purchase_sqlite_is_atomic_and_survives_reopen() {
        let (mut db, path) = loss_database(100.0, 0);
        db.execute(
            "UPDATE storage_ownership SET owner='sqlite' WHERE subsystem='game'",
            [],
        )
        .unwrap();
        let mut state = loss_state(&db);
        state["gamer"]["coins"] = json!(10000.0);
        state["gamer"]["level"] = json!(100);
        state["gamer"]["items"]["Предметы"] = json!({});
        db.execute("UPDATE game_state SET payload_json=?1", [state.to_string()])
            .unwrap();
        let item = catalog_item("Предметы", "Лотерейный билет").unwrap();
        GameApplicationService::inventory_sqlite(
            &mut db,
            "Предметы".into(),
            item.key.into(),
            1,
            "buy",
        )
        .unwrap();
        let purchased = loss_state(&db);
        assert_eq!(purchased["gamer"]["coins"], json!(10000.0 - item.price));
        assert_eq!(purchased["gamer"]["items"]["Предметы"][item.key], json!(1));
        for count in [0, -1, i64::MAX] {
            assert!(GameApplicationService::inventory_sqlite(
                &mut db,
                "Предметы".into(),
                item.key.into(),
                count,
                "buy"
            )
            .is_err());
            assert_eq!(loss_state(&db), purchased);
        }
        for (coins, quantity, expected_funds) in [(0.0, 1, true), (10000.0, 10000, false)] {
            let mut blocked = purchased.clone();
            blocked["gamer"]["coins"] = json!(coins);
            blocked["gamer"]["items"]["Предметы"][item.key] = json!(quantity);
            db.execute(
                "UPDATE game_state SET payload_json=?1",
                [blocked.to_string()],
            )
            .unwrap();
            let error = GameApplicationService::inventory_sqlite(
                &mut db,
                "Предметы".into(),
                item.key.into(),
                1,
                "buy",
            )
            .unwrap_err();
            assert!(if expected_funds {
                matches!(error, GameError::InsufficientFunds(_))
            } else {
                matches!(error, GameError::InvalidQuantity(_))
            });
            assert_eq!(loss_state(&db), blocked);
        }
        for payload in [
            json!({}),
            json!({"category":"Предметы","item_id":item.key,"count":1}),
            json!({"category":"Предметы","itemId":item.key,"count":"1"}),
        ] {
            assert!(serde_json::from_value::<InventoryRequest>(payload).is_err());
        }
        db.execute(
            "UPDATE game_state SET payload_json=?1",
            [purchased.to_string()],
        )
        .unwrap();
        drop(db);
        let db = crate::sqlite::open_database(&path).unwrap();
        assert_eq!(loss_state(&db), purchased);
        drop(db);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn inventory_total_is_bounded_without_integer_overflow() {
        assert_eq!(checked_inventory_total(9_999, 1, None).unwrap(), 10_000);
        assert!(matches!(
            checked_inventory_total(10_000, 1, None),
            Err(GameError::InvalidQuantity(_))
        ));
        assert!(matches!(
            checked_inventory_total(i64::MAX, 1, None),
            Err(GameError::InvalidQuantity(_))
        ));
        assert!(matches!(
            checked_inventory_total(9, 2, Some(10)),
            Err(GameError::InvalidQuantity(_))
        ));
    }

    #[test]
    fn inventory_mutation_preserves_unknown_gamer_fields() {
        let mut state = json!({"gamer":{"coins":1000.0,"level":3,"items":{"Предметы":{"Лотерейный билет":1}},"future_extension":{"kept":true}}});
        let item = catalog_item("Предметы", "Лотерейный билет").expect("catalog item");
        let mut rng = FixedRng {
            values: vec![1, 2, 3, 4, 5],
            index: 0,
        };
        let gamer = gamer_object(&mut state).expect("gamer");
        GameApplicationService::use_item_mutation(gamer, item, 1, &mut rng, "2026-01-01T00:00:00Z")
            .expect("use ticket");
        assert_eq!(state["gamer"]["future_extension"], json!({"kept":true}));
        assert_eq!(
            state["gamer"]["items"]["Предметы"]["Лотерейный билет"],
            Value::Null
        );
        assert_eq!(
            state["gamer"]["lottery_history"].as_array().map(Vec::len),
            Some(1)
        );
    }

    #[test]
    fn buffs_projection_matches_legacy_effect_semantics() {
        fn target_total(values: &[Value], target: &str) -> f64 {
            values
                .iter()
                .filter(|value| value["target"].as_str() == Some(target))
                .map(|value| {
                    value["value"].as_f64().unwrap_or(0.0)
                        * value["stacks"].as_i64().unwrap_or(1) as f64
                })
                .sum()
        }

        let raw = json!({
            "buffs": [
                {
                    "__type__": "game_data.Buff",
                    "fields": {
                        "name": "⭐️ Квестовая специализация: опыт",
                        "description": "Постоянный бонус к коэффициенту опыта за писательские и учебные квесты. +0.02 к параметру за завершение квеста.",
                        "buff_type": "positive",
                        "target_cf": "exp",
                        "value": 0.465,
                        "duration_minutes": null,
                        "start_time": {"__type__": "datetime", "value": "2026-07-14T23:47:49.846175"},
                        "end_time": null,
                        "source": "Квест",
                        "stackable": true
                    }
                },
                {
                    "__type__": "game_data.Buff",
                    "fields": {
                        "name": "🧪⚡️ Супер бустер опыта",
                        "description": "Применено зелье познания",
                        "buff_type": "positive",
                        "target_cf": "exp",
                        "value": 10.0,
                        "duration_minutes": 60,
                        "start_time": {"__type__": "datetime", "value": "2026-09-04T04:06:31.149420"},
                        "end_time": {"__type__": "datetime", "value": "2026-09-11T05:08:03.149420"},
                        "source": null,
                        "stackable": false
                    }
                },
                {
                    "__type__": "game_data.Buff",
                    "fields": {
                        "name": "Истёкший эффект",
                        "description": "Не должен быть активен",
                        "buff_type": "positive",
                        "target_cf": "exp",
                        "value": 99.0,
                        "duration_minutes": 60,
                        "start_time": "2026-09-06T17:00:00",
                        "end_time": "2026-09-06T18:59:59",
                        "source": "Тест",
                        "stackable": false
                    }
                }
            ],
            "debuffs": [
                {
                    "__type__": "game_data.Buff",
                    "fields": {
                        "name": "Ограничение",
                        "description": "Активное ограничение",
                        "buff_type": "negative",
                        "target_cf": "coins",
                        "value": 0.2,
                        "duration_minutes": null,
                        "start_time": null,
                        "end_time": null,
                        "source": null,
                        "stackable": false
                    }
                }
            ],
            "items": {
                "Предметы": {
                    "Печатная машинка Хемингуэя": 1,
                    "Ноутбук Роалинг": 1,
                    "Литературный раб": 5,
                    "Амулет восстановления": 1
                },
                "Награды": {
                    "👑  Корона Первой Эпохи": 1,
                    "💎  Перо Миллионера": 1,
                    "⭐️ Знак недельной практики": 12,
                    "⭐️ Банковский жетон": 10,
                    "⭐️ Знак дисциплины": 12
                },
                "Зелья": {"Часовое зелье просвещения": 1}
            }
        });
        let gamer = raw.as_object().expect("gamer fixture");
        let projection = buffs_projection(gamer, "2026-09-06T19:00:00");
        let positive = projection["positive"].as_array().expect("positive buffs");
        let negative = projection["negative"].as_array().expect("negative buffs");

        assert_eq!(positive.len(), 12);
        assert_eq!(negative.len(), 1);
        assert!(!positive
            .iter()
            .any(|value| value["name"] == json!("Истёкший эффект")));
        assert_eq!(negative[0]["type"], json!("negative"));

        let permanent = positive
            .iter()
            .find(|value| value["name"] == json!("⭐️ Квестовая специализация: опыт"))
            .expect("permanent specialization buff");
        assert_eq!(permanent["duration_minutes"], Value::Null);
        assert_eq!(permanent["expires_at"], Value::Null);
        assert_eq!(permanent["remaining_seconds"], Value::Null);
        assert_eq!(permanent["source"], json!("Квест"));

        let timed = positive
            .iter()
            .find(|value| value["name"] == json!("🧪⚡️ Супер бустер опыта"))
            .expect("timed consumable buff");
        assert_eq!(timed["description"], json!("Применено зелье познания"));
        assert_eq!(timed["duration_minutes"], json!(60));
        assert_eq!(timed["started_at"], json!("2026-09-04T04:06:31.149420"));
        assert_eq!(timed["expires_at"], json!("2026-09-11T05:08:03.149420"));
        assert_eq!(timed["remaining_seconds"], json!(382084));
        assert_eq!(timed["source"], Value::Null);

        assert!((target_total(positive, "exp") - 14.515).abs() < 1e-9);
        assert!((target_total(positive, "health_recovery") - 1.3).abs() < 1e-9);
        assert!((target_total(positive, "coins") - 2.5).abs() < 1e-9);
        assert_eq!(
            positive
                .iter()
                .filter(|value| value["name"] == json!("⭐️ Квестовая специализация: опыт"))
                .count(),
            2
        );
        assert_eq!(
            positive
                .iter()
                .find(
                    |value| value["name"] == json!("⭐️ Квестовая специализация: опыт")
                        && value["stacks"] == json!(12)
                )
                .map(|value| value["description"].clone()),
            Some(json!(
                "Постоянный бонус к коэффициенту опыта от квестовых наград. +0.025 к параметру."
            ))
        );
    }

    #[test]
    fn clock_projection_and_week_boundary_are_deterministic() {
        assert_eq!(iso_date_from_days(0), "1970-01-01");
        assert_eq!(iso_epoch_seconds("1970-01-01T00:00:00Z"), Some(0));
        let session = session_projection(
            Some(&json!({
                "started_at":"2026-09-04T12:00:00Z",
                "duration_minutes":15,
                "target_symbols":100
            })),
            "2026-09-04T12:05:00Z",
        );
        assert_eq!(session["remaining_seconds"], json!(600));
        let day = iso_epoch_seconds("2026-09-04T00:00:00Z")
            .expect("date")
            .div_euclid(86_400);
        let monday = day - (day + 3).rem_euclid(7);
        assert_eq!(iso_date_from_days(monday), "2026-08-31");
    }

    #[test]
    fn game_request_dto_rejects_unknown_fields() {
        let request = serde_json::from_value::<InventoryRequest>(json!({
            "category":"Предметы",
            "itemId":"Лотерейный билет",
            "count":1,
            "inject":"not allowed"
        }));
        assert!(request.is_err());
    }

    #[test]
    fn native_projection_preserves_legacy_game_catalog_and_progression_domains() {
        assert_eq!(RELIC_DEFINITIONS.len(), 8);
        assert_eq!(SPECIALIZATION_DEFINITIONS.len(), 5);
        assert_eq!(SESSION_MODE_DEFINITIONS.len(), 4);
        assert_eq!(SESSION_INTENTION_DEFINITIONS.len(), 4);
        assert!(ITEM_METADATA
            .iter()
            .all(|(_, _, description, _)| !description.is_empty()));
        assert_eq!(REWARD_METADATA.len(), 45);
        assert!(REWARD_METADATA
            .iter()
            .all(|(_, _, description)| !description.is_empty()));
        assert!(RELIC_DEFINITIONS
            .iter()
            .all(|definition| !definition.description.is_empty()));

        let state = project_state(
            &json!({
                "gamer": {
                    "level": 40,
                    "specialization": "ritualist",
                    "specialization_mastery": {"ritualist": 8},
                    "specialization_ability_ready_at": {},
                    "specialization_ability_effects": {},
                    "items": {"Награды": {"⭐️ Знак дисциплины": 1}},
                    "daily_challenge": {"option_id":"sessions:normal","date":"2026-09-05","type":"sessions","difficulty":"normal","target":2,"progress":0,"completed":false,"reward_coins":240,"reward_exp":1000},
                    "daily_challenge_options": [{"option_id":"sessions:normal","date":"2026-09-05","type":"sessions","difficulty":"normal","target":2,"progress":0,"completed":false,"reward_coins":240,"reward_exp":1000}],
                    "daily_challenge_history": [],
                    "weekly_challenge": {"key":"symbols","week_start":"2026-08-31","progress":0,"writing_days":[],"completed":false},
                    "cabinet_relics": ["ink_candle", "plot_map"],
                    "manuscript_journeys": {"project:one": [10, 25, 50]}
                }
            }),
            "2026-09-05T12:00:00Z",
            "2026-09-05T12:00:00Z",
            "2026-09-05",
            true,
        )
        .expect("projection");

        assert_eq!(
            state["inventory"]["categories"][0]["items"]
                .as_array()
                .map(Vec::len),
            Some(21)
        );
        assert_eq!(
            state["specializations"]["items"].as_array().map(Vec::len),
            Some(5)
        );
        assert_eq!(state["specializations"]["selected"], json!("ritualist"));
        assert_eq!(
            state["daily_challenge"]["current"]["name"],
            json!("Сессионный ритм")
        );
        assert_eq!(
            state["daily_challenge"]["current"]["reward"]["coins"],
            json!(240.0)
        );
        assert_eq!(
            state["daily_challenge"]["current"]["description"],
            json!("Завершить несколько успешных писательских сессий.")
        );
        assert_eq!(
            state["weekly_challenge"]["current"]["name"],
            json!("Марафон")
        );
        assert_eq!(
            state["inventory"]["categories"][3]["items"][0]["description"],
            json!("Награда за запасной день и заботу о стрике.")
        );
        assert_eq!(
            state["writing_session"]["modes"][0]["description"],
            json!("15 минут: +15% к награде за быстрый результат.")
        );
        assert_eq!(
            state["writing_session"]["intentions"]
                .as_array()
                .map(Vec::len),
            Some(4)
        );
        assert_eq!(
            state["manuscripts"]["cabinet"]["relics"]
                .as_array()
                .map(Vec::len),
            Some(8)
        );
        assert_eq!(
            state["manuscripts"]["cabinet"]["relics"][0]["name"],
            json!("Чернильная свеча")
        );
        assert_eq!(
            state["manuscripts"]["cabinet"]["relics"][2]["name"],
            Value::Null
        );
        assert_eq!(
            state["manuscripts"]["journeys"].as_array().map(Vec::len),
            Some(1)
        );
    }
}
