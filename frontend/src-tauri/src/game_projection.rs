//! Typed legacy admission and compatibility projection. Recovery JSON stays local.
use crate::{game_codec, progress_codec};
use serde_json::{json, Map, Value};
type Result<T> = std::result::Result<T, String>;
fn unsupported<T>() -> Result<T> {
    Err("game_legacy_extension_unsupported".into())
}
fn defaults() -> Value {
    serde_json::from_str(include_str!(
        "../../src/cloud/__fixtures__/gameLegacyDefaultsV1.json"
    ))
    .expect("audited static Game defaults")
}
fn number(v: &Value) -> Result<String> {
    let n = v.as_f64().ok_or("game_legacy_extension_unsupported")?;
    if !n.is_finite() || n < 0.0 || n > 1e12 {
        return Err("game_resource_limit".into());
    }
    let raw = format!("{n:.6}");
    progress_codec::micros(&raw).map_err(|_| "game_resource_limit")?;
    // The frozen contract has six decimal places. Do not silently change a
    // legacy coefficient/balance whose actual float needs greater precision.
    if raw.parse::<f64>().map_err(|_| "game_resource_limit")? != n {
        return unsupported();
    }
    Ok(raw)
}
fn integer(v: &Value) -> Result<u64> {
    v.as_u64()
        .filter(|n| *n <= 1_000_000)
        .ok_or("game_legacy_extension_unsupported".into())
}
fn date(v: &Value) -> Result<Option<String>> {
    if v.is_null() {
        return Ok(None);
    }
    let raw = if let Some(s) = v.as_str() {
        s
    } else if matches!(v["__type__"].as_str(), Some("date" | "datetime")) {
        v["value"]
            .as_str()
            .ok_or("game_legacy_extension_unsupported")?
    } else {
        return unsupported();
    };
    let raw = raw.get(..10).ok_or("game_legacy_extension_unsupported")?;
    if !crate::project_metadata_sync::timestamp(&format!("{raw}T00:00:00.000000Z")) {
        return unsupported();
    }
    Ok(Some(raw.to_string()))
}
pub fn streak_base(source: &Value, global: bool) -> Result<Value> {
    let empty = Map::new();
    let o = if source.is_null() {
        &empty
    } else {
        source
            .as_object()
            .ok_or("game_legacy_extension_unsupported")?
    };
    let fields = if global {
        [
            "global_streaks",
            "max_global_streak",
            "global_streak_status",
            "last_global_streak_bonus",
            "last_global_streak_lost_date",
            "last_global_streak_lose_len",
        ]
    } else {
        [
            "streaks",
            "max_streak",
            "streak_status",
            "last_streak_bonus",
            "last_streak_lost_date",
            "last_streak_lose_len",
        ]
    };
    if o.keys().any(|k| {
        !fields.contains(&k.as_str()) && !(!global && matches!(k.as_str(), "freezes" | "name"))
    }) {
        return unsupported();
    }
    let mut days = Vec::new();
    let mut previous: Option<String> = None;
    let entries = match o.get(fields[0]) {
        Some(v) => v
            .as_array()
            .ok_or("game_legacy_extension_unsupported")?
            .as_slice(),
        None => &[],
    };
    if entries.len() > 4096 {
        return Err("game_resource_limit".into());
    }
    for entry in entries {
        let frozen = entry == "freeze";
        let day = if frozen {
            let old = previous
                .as_deref()
                .ok_or("game_legacy_extension_unsupported")?;
            crate::streaks::date_from_days(
                crate::streaks::date_days(old).ok_or("game_legacy_extension_unsupported")? + 1,
            )
        } else {
            date(entry)?.ok_or("game_legacy_extension_unsupported")?
        };
        if previous.as_ref().is_some_and(|p| p >= &day) {
            return unsupported();
        }
        previous = Some(day.clone());
        days.push(json!({"day":day,"frozen":frozen}));
    }
    let maximum = o
        .get(fields[1])
        .map(integer)
        .transpose()?
        .unwrap_or(days.len() as u64);
    let freezes = if global {
        days.iter().filter(|v| v["frozen"] == true).count() as u64
    } else {
        o.get("freezes").map(integer).transpose()?.unwrap_or(0)
    };
    Ok(
        json!({"history":days,"maximum":maximum,"freezes":freezes,"enabled":o.get(fields[2]).is_none_or(|v|v!="Off"),"last_reward_day":date(o.get(fields[3]).unwrap_or(&Value::Null))?,"lost_day":date(o.get(fields[4]).unwrap_or(&Value::Null))?,"lost_length":o.get(fields[5]).map(integer).transpose()?.unwrap_or(0)}),
    )
}
/// Default template objects are admitted only by exact comparison to the frozen
/// source definitions. Any nondefault deferred family is retained and blocked.
pub fn account_base(source: &Value) -> Result<Value> {
    let d = defaults();
    if source.as_object().is_none_or(|o| {
        o.keys().any(|k| {
            !matches!(
                k.as_str(),
                "gamer"
                    | "notifications"
                    | "global_streak"
                    | "project_game_state"
                    | "extensions"
                    | "dto_version"
                    | "state_schema_version"
            )
        })
    }) {
        return unsupported();
    }
    let g = source["gamer"]
        .as_object()
        .ok_or("game_legacy_extension_unsupported")?;
    let admitted = [
        "coins",
        "exp",
        "level",
        "health",
        "max_health",
        "skills",
        "available_skill_points",
        "skill_points_awarded_for_level",
        "cf",
        "inspiration",
        "items",
        "complete_bonus_projects",
        "productive_actions_since_event",
        "pending_creative_event",
        "writing_reward_bonus",
    ];
    let local = ["last_health_recovery_at", "writing_session"];
    for (k, v) in g {
        if admitted.contains(&k.as_str()) || local.contains(&k.as_str()) {
            continue;
        }
        if d.get(k).is_none_or(|default| {
            crate::document_codec::canonical(default) != crate::document_codec::canonical(v)
        }) {
            return unsupported();
        }
    }
    if let Some(ext) = source.get("extensions").and_then(Value::as_object) {
        if ext
            .keys()
            .any(|k| !matches!(k.as_str(), "progress_deletions" | "lifecycle_events"))
        {
            return unsupported();
        }
    }
    let get = |key: &str| g.get(key).unwrap_or(&d[key]);
    let cf = get("cf")
        .as_object()
        .ok_or("game_legacy_extension_unsupported")?;
    if cf
        .keys()
        .any(|k| !matches!(k.as_str(), "coins" | "exp" | "health_recovery"))
    {
        return unsupported();
    }
    let coefficient = |key: &str| -> Result<String> {
        let v = cf.get(key).unwrap_or(&d["cf"][key]);
        if let Some(o) = v.as_object() {
            if o.keys()
                .any(|k| !matches!(k.as_str(), "value" | "base_value" | "name" | "description"))
            {
                return unsupported();
            }
            for field in ["base_value", "name", "description"] {
                if o.get(field).is_some_and(|v| {
                    crate::document_codec::canonical(v)
                        != crate::document_codec::canonical(&d["cf"][key][field])
                }) {
                    return unsupported();
                }
            }
            number(&v["value"])
        } else {
            number(v)
        }
    };
    let mut inventory = Vec::new();
    for (category, items) in get("items")
        .as_object()
        .ok_or("game_legacy_extension_unsupported")?
    {
        for (item, count) in items
            .as_object()
            .ok_or("game_legacy_extension_unsupported")?
        {
            let count = integer(count)?;
            if count > 0 {
                inventory.push(json!({"category":category,"item_id":item,"count":count}));
            }
        }
    }
    inventory.sort_by(|a, b| {
        format!(
            "{}\0{}",
            a["category"].as_str().unwrap(),
            a["item_id"].as_str().unwrap()
        )
        .encode_utf16()
        .cmp(
            format!(
                "{}\0{}",
                b["category"].as_str().unwrap(),
                b["item_id"].as_str().unwrap()
            )
            .encode_utf16(),
        )
    });
    let mut claims = get("complete_bonus_projects")
        .as_array()
        .ok_or("game_legacy_extension_unsupported")?
        .clone();
    if claims.iter().any(|v| !v.is_string()) {
        return unsupported();
    }
    claims.sort_by(|a, b| {
        a.as_str()
            .unwrap()
            .encode_utf16()
            .cmp(b.as_str().unwrap().encode_utf16())
    });
    claims.dedup();
    let pending = match g.get("pending_creative_event") {
        None => "absent",
        Some(Value::Null) => "none",
        Some(v) if v == "unexpected_idea" => "unexpected_idea",
        _ => return unsupported(),
    };
    let base = json!({"coins":number(get("coins"))?,"experience":number(get("exp"))?,"level":integer(get("level"))?,"health":number(get("health"))?,"max_health":number(get("max_health"))?,"available_skill_points":integer(get("available_skill_points"))?,"skill_points_awarded_for_level":integer(get("skill_points_awarded_for_level"))?,"skills":get("skills"),"coin_coefficient":coefficient("coins")?,"experience_coefficient":coefficient("exp")?,"health_recovery_coefficient":coefficient("health_recovery")?,"inspiration":number(get("inspiration"))?,"inventory":inventory,"completion_claims":claims,"productive_actions":integer(get("productive_actions_since_event"))?,"creative_event_pending":pending,"writing_bonus":number(get("writing_reward_bonus"))?,"global_streak":streak_base(&source["global_streak"],true)?});
    validate_base(&base)?;
    Ok(base)
}
fn validate_base(base: &Value) -> Result<()> {
    game_codec::validate(
        &json!({"version":1,"header":{"account_id":"123e4567-e89b-42d3-a456-426614174000","device_id":"123e4567-e89b-42d3-a456-426614174001","event_id":"123e4567-e89b-42d3-a456-426614174002","entity_id":"game:123e4567-e89b-42d3-a456-426614174002","entity_type":"account_game","scope":"account","parents":[],"revision":1,"updated_at":"2026-10-04T00:00:00.000000Z","rule":"legacy-game-v1"},"action":{"kind":"genesis","base":base}}),
    )
}
fn numeric(v: &Value) -> Result<Value> {
    Ok(json!(
        progress_codec::micros(v.as_str().ok_or("invalid_game_payload")?)
            .map_err(|_| "invalid_game_payload")? as f64
            / 1_000_000.0
    ))
}
pub(crate) fn streak_fields(base: &Value, global: bool) -> Result<Value> {
    let history = base["history"].as_array().ok_or("invalid_game_payload")?;
    let mut entries = Vec::new();
    let mut previous: Option<&str> = None;
    for entry in history {
        let day = entry["day"].as_str().ok_or("invalid_game_payload")?;
        if entry["frozen"] == true {
            if previous
                .and_then(crate::streaks::date_days)
                .zip(crate::streaks::date_days(day))
                .is_none_or(|(p, d)| d != p + 1)
            {
                return Err("game_invalid_rule".into());
            }
            entries.push(json!("freeze"));
        } else {
            entries.push(json!({"__type__":"date","value":day}));
        }
        previous = Some(day);
    }
    let date = |v: &Value| {
        if v.is_null() {
            Value::Null
        } else {
            json!({"__type__":"date","value":v})
        }
    };
    let fields = if global {
        [
            "global_streaks",
            "max_global_streak",
            "global_streak_status",
            "last_global_streak_bonus",
            "last_global_streak_lost_date",
            "last_global_streak_lose_len",
        ]
    } else {
        [
            "streaks",
            "max_streak",
            "streak_status",
            "last_streak_bonus",
            "last_streak_lost_date",
            "last_streak_lose_len",
        ]
    };
    let mut o = Map::new();
    o.insert(fields[0].into(), json!(entries));
    o.insert(fields[1].into(), base["maximum"].clone());
    o.insert(
        fields[2].into(),
        json!(if base["enabled"] == false {
            "Off".to_string()
        } else if history.is_empty()
            && !base["lost_day"].is_null()
            && base["lost_length"].as_u64().unwrap_or(0) > 0
        {
            format!("Lose {}", base["lost_length"].as_u64().unwrap_or(0))
        } else if history.is_empty() {
            "No".to_string()
        } else {
            "Active".to_string()
        }),
    );
    o.insert(fields[3].into(), date(&base["last_reward_day"]));
    o.insert(fields[4].into(), date(&base["lost_day"]));
    o.insert(fields[5].into(), base["lost_length"].clone());
    if !global {
        o.insert("freezes".into(), base["freezes"].clone());
    }
    Ok(Value::Object(o))
}
/// Replace only admitted compatibility fields; notifications/execution stay local.
pub fn materialize_account(source: &Value, base: &Value) -> Result<Value> {
    validate_base(base)?;
    let mut out = source.clone();
    let root = out.as_object_mut().ok_or("invalid_game_payload")?;
    let mut g = root.get("gamer").cloned().unwrap_or_else(defaults);
    let gmap = g.as_object_mut().ok_or("invalid_game_payload")?;
    for (to, from) in [
        ("coins", "coins"),
        ("exp", "experience"),
        ("health", "health"),
        ("max_health", "max_health"),
        ("inspiration", "inspiration"),
        ("writing_reward_bonus", "writing_bonus"),
    ] {
        gmap.insert(to.into(), numeric(&base[from])?);
    }
    for (to, from) in [
        ("level", "level"),
        ("skills", "skills"),
        ("available_skill_points", "available_skill_points"),
        (
            "skill_points_awarded_for_level",
            "skill_points_awarded_for_level",
        ),
        ("complete_bonus_projects", "completion_claims"),
        ("productive_actions_since_event", "productive_actions"),
    ] {
        gmap.insert(to.into(), base[from].clone());
    }
    match base["creative_event_pending"].as_str() {
        Some("absent") => {
            gmap.remove("pending_creative_event");
        }
        Some("none") => {
            gmap.insert("pending_creative_event".into(), Value::Null);
        }
        Some("unexpected_idea") => {
            gmap.insert("pending_creative_event".into(), json!("unexpected_idea"));
        }
        _ => return Err("invalid_game_payload".into()),
    }
    let d = defaults();
    let cf = gmap
        .entry("cf")
        .or_insert_with(|| d["cf"].clone())
        .as_object_mut()
        .ok_or("invalid_game_payload")?;
    for (key, field) in [
        ("coins", "coin_coefficient"),
        ("exp", "experience_coefficient"),
        ("health_recovery", "health_recovery_coefficient"),
    ] {
        let v = cf.entry(key).or_insert_with(|| d["cf"][key].clone());
        if v.is_object() {
            v["value"] = numeric(&base[field])?;
        } else {
            *v = numeric(&base[field])?;
        }
    }
    let mut items = Map::new();
    for entry in base["inventory"].as_array().ok_or("invalid_game_payload")? {
        items
            .entry(entry["category"].as_str().unwrap().to_string())
            .or_insert_with(|| json!({}))[entry["item_id"].as_str().unwrap()] =
            entry["count"].clone();
    }
    gmap.insert("items".into(), json!(items));
    root.insert("gamer".into(), g);
    root.insert(
        "global_streak".into(),
        streak_fields(&base["global_streak"], true)?,
    );
    Ok(out)
}
pub fn materialize_project(
    source: &Value,
    project: &str,
    stage: Option<&str>,
    snapshot: &Value,
) -> Result<Value> {
    let mut out = source.clone();
    let root = out.as_object_mut().ok_or("invalid_game_payload")?;
    let key = stage
        .map(|s| format!("stage:{project}:{s}"))
        .unwrap_or_else(|| format!("project:{project}"));
    let owners = root
        .entry("project_game_state")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("invalid_game_payload")?;
    let old = owners
        .entry(key)
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("invalid_game_payload")?;
    old.extend(
        streak_fields(&snapshot["streak"], false)?
            .as_object()
            .unwrap()
            .clone(),
    );
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn game_legacy_admission_preserves_base_and_blocks_unknown_or_nondefault_extensions() {
        let source = json!({"gamer":defaults(),"notifications":{"read":["local"]},"global_streak":{"global_streaks":[{"__type__":"date","value":"2026-10-01"},"freeze"],"max_global_streak":20}});
        let base = account_base(&source).unwrap();
        assert_eq!(
            base["global_streak"]["history"][1],
            json!({"day":"2026-10-02","frozen":true})
        );
        let projected = materialize_account(&source, &base).unwrap();
        assert_eq!(account_base(&projected).unwrap(), base);
        assert_eq!(projected["notifications"], source["notifications"]);
        for (key, value) in [
            ("unknown", json!({"secret":"local"})),
            ("bank_account", json!({"credit":10})),
            ("daily_challenge", json!({"active":true})),
        ] {
            let mut bad = source.clone();
            bad["gamer"][key] = value;
            assert_eq!(
                account_base(&bad).unwrap_err(),
                "game_legacy_extension_unsupported"
            );
        }
        let mut bad = source.clone();
        bad["gamer"]["quests"][0]["fields"]["reward_coins"] = json!(999);
        assert_eq!(
            account_base(&bad).unwrap_err(),
            "game_legacy_extension_unsupported"
        );
        let mut precise = source.clone();
        precise["gamer"]["cf"]["coins"]["value"] = json!(1.0000001);
        assert_eq!(
            account_base(&precise).unwrap_err(),
            "game_legacy_extension_unsupported"
        );
    }
    #[test]
    fn game_legacy_projection_is_idempotent_and_preserves_portable_fields_and_local_session() {
        let mut source =
            json!({"gamer":defaults(),"notifications":{"new":["notice"]},"global_streak":{}});
        source["gamer"]["coins"] = json!(765.4);
        source["gamer"]["exp"] = json!(555);
        source["gamer"]["items"] = json!({"Предметы":{"Заморозка":2}});
        source["gamer"]["writing_session"] = json!({"started_at":"local","progress":30});
        source["gamer"]["complete_bonus_projects"] = json!(["project:P"]);
        let base = account_base(&source).unwrap();
        let projected = materialize_account(&source, &base).unwrap();
        assert_eq!(projected["gamer"]["coins"], 765.4);
        assert_eq!(projected["gamer"]["items"], source["gamer"]["items"]);
        assert_eq!(
            projected["gamer"]["writing_session"],
            source["gamer"]["writing_session"]
        );
        assert_eq!(materialize_account(&projected, &base).unwrap(), projected);
        assert_eq!(base["completion_claims"], json!(["project:P"]));
        assert_eq!(base["experience"], "555.000000");
    }
}
