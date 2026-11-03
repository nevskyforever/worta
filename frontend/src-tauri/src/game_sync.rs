//! C18.5.06 immutable Game history. Runtime activation is gated separately.
//! No compatibility snapshot can manufacture authenticated source history.
use crate::{
    document_codec::canonical, game_codec as codec, progress_codec,
    project_metadata_sync as metadata,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
type Result<T> = std::result::Result<T, String>;
pub const MAX_ANCESTRY: usize = 65536;
pub const MAX_HISTORY_BYTES: usize = 64 * 1024 * 1024;
fn sql(_: rusqlite::Error) -> String {
    "game_storage_error".into()
}
fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key].as_str().ok_or("invalid_game_payload".into())
}
fn ids(e: &Value) -> Result<Vec<String>> {
    e["header"]["parents"]
        .as_array()
        .ok_or("invalid_game_payload")?
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .ok_or("invalid_game_payload".into())
        })
        .collect()
}
pub fn owner(e: &Value) -> String {
    if e["header"]["scope"] == "account" {
        "account".into()
    } else {
        canonical(&json!([e["header"]["project_id"], e["header"]["stage_id"]]))
    }
}
/// Length-delimited canonical tuple and a fixed domain avoid ambiguous concatenation.
/// Identity depends only on authenticated semantic source, never insertion or retry time.
pub fn stable_action_id(account: &str, kind: &str, source: &Value) -> String {
    let material = canonical(&json!(["WORTA/C18/game/action/1", account, kind, source]));
    let hash = Sha256::digest(material.as_bytes());
    let mut b: [u8; 16] = hash[..16].try_into().unwrap();
    b[6] = (b[6] & 0x0f) | 0x50;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|v| format!("{v:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    )
}
pub fn loss_id(account: &str, before: &Value) -> String {
    stable_action_id(account, "global_streak_loss", before)
}
pub fn tips(db: &Connection, account: &str, key: &str) -> Result<Vec<String>> {
    let mut q = db.prepare("SELECT event_id FROM cloud_game_tips WHERE account_id=?1 AND owner_key=?2 ORDER BY event_id").map_err(sql)?;
    let rows = q
        .query_map(params![account, key], |r| r.get(0))
        .map_err(sql)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(sql)?;
    Ok(rows)
}
fn event(db: &Connection, account: &str, id: &str) -> Result<Value> {
    let (frame, scope): (Vec<u8>,String) = db.query_row("SELECT canonical_frame,scope FROM cloud_game_events WHERE account_id=?1 AND event_id=?2",params![account,id],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(sql)?.ok_or("game_parent_unknown")?;
    codec::unframe(&frame, scope == "project")
}
/// Caller supplies a transaction. An existing identity is reusable only with exact bytes.
pub fn preserve(db: &Connection, account: &str, e: &Value, state: &str) -> Result<bool> {
    if !matches!(state, "unsealed" | "waiting") {
        return Err("game_scope_mismatch".into());
    }
    let frame = codec::frame(e)?;
    let h = &e["header"];
    let id = text(h, "event_id")?;
    let prior: Option<Vec<u8>> = db
        .query_row(
            "SELECT canonical_frame FROM cloud_game_events WHERE account_id=?1 AND event_id=?2",
            params![account, id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sql)?;
    if let Some(prior) = prior {
        return if prior == frame {
            Ok(false)
        } else {
            Err("game_exact_replay_mismatch".into())
        };
    }
    db.execute("INSERT INTO cloud_game_events(account_id,event_id,scope,owner_key,project_id,stage_id,entity_id,canonical_frame,parents_json,revision,state) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",params![account,id,text(h,"scope")?,owner(e),h["project_id"].as_str(),h["stage_id"].as_str(),text(h,"entity_id")?,frame,canonical(&h["parents"]),h["revision"].as_i64().ok_or("invalid_game_payload")?,state]).map_err(sql)?;
    Ok(true)
}
fn amount(v: &Value) -> Result<i128> {
    progress_codec::micros(v.as_str().ok_or("invalid_game_payload")?)
        .map_err(|_| "game_invalid_rule".into())
}
fn change(v: &mut Value, field: &str, delta: i128) -> Result<()> {
    let n = amount(&v[field])?
        .checked_add(delta)
        .ok_or("game_resource_limit")?;
    if !(0..=progress_codec::MAX_AMOUNT).contains(&n) {
        return Err("game_invalid_rule".into());
    }
    v[field] = json!(progress_codec::decimal(n));
    Ok(())
}
fn float(v: &Value) -> Result<f64> {
    Ok(amount(v)? as f64 / 1_000_000.0)
}
fn decimal(n: f64) -> Result<Value> {
    if !n.is_finite() || n < 0.0 || n > 1e12 {
        return Err("game_resource_limit".into());
    }
    let raw = format!("{n:.6}");
    amount(&json!(raw))?;
    Ok(json!(raw))
}
fn tenth(n: f64) -> f64 {
    (((n - 1e-9) * 10.0).ceil() / 10.0).max(0.0)
}
/// Legacy loss damage and GUI revival semantics, in the replayable account state.
pub fn loss_health(state: &mut Value, length: u64) -> Result<()> {
    let health = amount(&state["health"])?;
    let max = amount(&state["max_health"])?;
    let remaining = (health.min(max) - i128::from(length) * 5_000_000).max(0);
    state["health"] = json!(progress_codec::decimal(remaining));
    if remaining == 0 {
        let inventory = state["inventory"]
            .as_array_mut()
            .ok_or("invalid_game_payload")?;
        if let Some(i) = inventory.iter().position(|v| {
            v["category"] == "Зелья"
                && v["item_id"] == "Зелье воскрешения"
                && v["count"].as_u64().is_some_and(|n| n > 0)
        }) {
            let count = inventory[i]["count"]
                .as_u64()
                .ok_or("invalid_game_payload")?;
            if count == 1 {
                inventory.remove(i);
            } else {
                inventory[i]["count"] = json!(count - 1);
            }
            state["health"] = state["max_health"].clone();
        }
    }
    Ok(())
}
/// Frozen native v1 inputs, including its historical float rounding convention.
pub fn writing_reward(a: &Value, rule: &str) -> Result<Value> {
    let delta = float(&a["fact"]["delta"])?;
    let multiplier =
        (1.0 + float(&a["inspiration"])? / 100.0 * 0.1) * (1.0 + float(&a["writing_bonus"])?);
    let coins = delta / 100.0 * 10.0 * float(&a["coin_coefficient"])? * multiplier;
    let xp = delta / 100.0 * 500.0 * float(&a["experience_coefficient"])? * multiplier;
    match rule {
        "native-game-v1" | "python-game-v1" => {
            Ok(json!({"coins":decimal(tenth(coins))?,"experience":decimal(tenth(xp))?}))
        }
        _ => Err("game_invalid_rule".into()),
    }
}
fn verify_effect(e: &Value) -> Result<()> {
    let a = &e["action"];
    let h = &e["header"];
    match a["kind"].as_str() {
        Some("writing") => {
            if a["reward"] != writing_reward(a, text(h, "rule")?)? {
                return Err("game_invalid_rule".into());
            }
        }
        Some("completion") => {
            if !matches!(
                h["rule"].as_str(),
                Some("native-game-v1" | "python-game-v1")
            ) {
                return Err("game_invalid_rule".into());
            }
            let factor = if h["stage_id"].is_null() { 1.0 } else { 0.25 };
            let input = float(&a["total_symbols"])? / 1000.0 + 0.5;
            let rounded = if h["rule"] == "python-game-v1" {
                input.round_ties_even()
            } else {
                input.round()
            };
            let coins = rounded * 100.0 * factor;
            if a["reward"] != json!({"coins":decimal(coins)?,"experience":decimal(coins*100.0)?}) {
                return Err("game_invalid_rule".into());
            }
        }
        _ => {}
    }
    Ok(())
}
/// Dependency proof uses authenticated ledgers, never a mutable Progress projection.
pub fn dependencies(db: &Connection, account: &str, user: &str, e: &Value) -> Result<()> {
    codec::validate(e)?;
    let h = &e["header"];
    let a = &e["action"];
    if h["account_id"] != user {
        return Err("game_scope_mismatch".into());
    }
    let identity = match a["kind"].as_str() {
        Some("writing") => Some(stable_action_id(
            user,
            "writing",
            &json!([
                h["project_id"],
                h["stage_id"],
                a["progress_event_id"],
                a["fact"]["entry_id"]
            ]),
        )),
        Some("completion") => Some(stable_action_id(
            user,
            "completion",
            &json!([h["project_id"], h["stage_id"], a["completion_id"]]),
        )),
        Some("reward") => Some(stable_action_id(user, "reward", &a["project_action_id"])),
        Some("global_streak") if e["version"] == 2 => Some(loss_id(user, &a["before"])),
        Some("compensation") => Some(stable_action_id(
            user,
            "compensation",
            &a["target_action_id"],
        )),
        _ => None,
    };
    if identity.as_ref().is_some_and(|id| h["event_id"] != *id) {
        return Err("game_reward_duplicate_mismatch".into());
    }
    if h["scope"] == "project" {
        let p = text(h, "project_id")?;
        crate::account_catalog::project_reference_ready(
            db,
            account,
            p,
            text(h, "bootstrap_id")?,
            text(h, "metadata_event_id")?,
        )
        .map_err(|_| "game_project_authority_unresolved")?;
        if let Some(s) = h["stage_id"].as_str() {
            let refs: Vec<String> = serde_json::from_value(h["stage_event_ids"].clone())
                .map_err(|_| "invalid_game_payload")?;
            crate::stage_sync::content_reference_ready(db, account, p, s, &refs)
                .map_err(|_| "game_stage_authority_unresolved")?;
        }
        if matches!(a["kind"].as_str(), Some("writing" | "completion")) {
            let eid = text(a, "progress_event_id")?;
            let entity = text(a, "progress_entity_id")?;
            let frame: Vec<u8> = db.query_row("SELECT e.canonical_frame FROM cloud_progress_events e JOIN cloud_progress_apply_ledger l USING(account_id,event_id) WHERE e.account_id=?1 AND e.project_id=?2 AND e.entity_id=?3 AND e.event_id=?4 AND e.state=l.outcome AND l.outcome IN ('applied','conflict_preserved') AND e.server_sequence=l.server_sequence",params![account,p,entity,eid],|r|r.get(0)).optional().map_err(sql)?.ok_or("game_dependency_progress_missing")?;
            let source =
                progress_codec::decode(&frame).map_err(|_| "game_dependency_progress_missing")?;
            if source.header.account_id != user
                || source.header.stage_id.as_deref() != h["stage_id"].as_str()
            {
                return Err("game_scope_mismatch".into());
            }
            let c = crate::progress_sync::chain(db, account, p, entity, eid)
                .map_err(|_| "game_dependency_progress_missing")?;
            if !c.migration_complete() {
                return Err("game_dependency_progress_missing".into());
            }
            if a["kind"] == "writing" {
                let fact: progress_codec::Fact = serde_json::from_value(a["fact"].clone())
                    .map_err(|_| "invalid_game_payload")?;
                // Prove the exact introducing action, not merely a similar later value.
                if source.header.operation != "append"
                    || source.entries != vec![fact.clone()]
                    || !c.entries.contains(&fact)
                {
                    return Err("game_dependency_progress_missing".into());
                }
            } else {
                if c.total().map_err(|_| "game_dependency_progress_missing")?
                    != amount(&a["total_symbols"])?
                {
                    return Err("game_dependency_progress_missing".into());
                }
                // Completion status belongs to the authenticated structural source,
                // independently of the mutable current Progress total or card.
                let completed = if let Some(s) = h["stage_id"].as_str() {
                    let refs = h["stage_event_ids"]
                        .as_array()
                        .ok_or("invalid_game_payload")?;
                    if refs.len() != 1 {
                        return Err("game_stage_authority_unresolved".into());
                    }
                    let raw: Vec<u8> = db.query_row("SELECT canonical_frame FROM cloud_sync_structural_events WHERE account_id=?1 AND project_id=?2 AND entity_type='stage' AND entity_id=?3 AND event_id=?4 AND state='applied'", params![account,p,s,refs[0].as_str()], |r|r.get(0)).map_err(|_| "game_stage_authority_unresolved")?;
                    crate::stage_sync::unframe(&raw)
                        .map_err(|_| "game_stage_authority_unresolved")?
                        .stage["status"]
                        == "завершен"
                } else {
                    let raw: String = db.query_row("SELECT payload_json FROM cloud_sync_metadata_events WHERE account_id=?1 AND project_id=?2 AND event_id=?3 AND state='applied'",params![account,p,h["metadata_event_id"].as_str()], |r|r.get(0)).map_err(|_| "game_project_authority_unresolved")?;
                    serde_json::from_str::<Value>(&raw)
                        .map_err(|_| "game_project_authority_unresolved")?["status"]
                        == "завершен"
                };
                if !completed {
                    return Err("game_invalid_rule".into());
                }
            }
        }
    } else if a["kind"] == "reward" {
        let source_id = text(a, "project_action_id")?;
        let source =
            event(db, account, source_id).map_err(|_| "game_dependency_project_action_missing")?;
        let proven: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM cloud_game_apply_ledger l JOIN cloud_game_events e USING(account_id,event_id) WHERE l.account_id=?1 AND l.event_id=?2 AND l.outcome='applied' AND e.state=l.outcome AND e.server_sequence=l.server_sequence AND e.canonical_frame=l.canonical_frame AND e.nonce=l.nonce AND e.ciphertext=l.ciphertext)",params![account,source_id],|r|r.get(0)).map_err(sql)?;
        if !proven {
            return Err("game_dependency_project_action_missing".into());
        }
        if source["header"]["scope"] != "project"
            || source["header"]["account_id"] != user
            || source["header"]["project_id"] != a["project_id"]
            || source["header"]["rule"] != h["rule"]
            || source["action"]["reward"] != a["reward"]
            || !matches!(
                source["action"]["kind"].as_str(),
                Some("writing" | "completion" | "streak")
            )
        {
            return Err("game_reward_duplicate_mismatch".into());
        }
        verify_effect(&source)?;
    } else if a["kind"] == "compensation" {
        let target = text(a, "target_action_id")?;
        let proven:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM cloud_game_apply_ledger l JOIN cloud_game_events e USING(account_id,event_id) WHERE l.account_id=?1 AND l.event_id=?2 AND e.scope='account' AND e.state='applied' AND l.outcome=e.state AND e.server_sequence=l.server_sequence AND e.canonical_frame=l.canonical_frame AND e.nonce=l.nonce AND e.ciphertext=l.ciphertext)",params![account,target],|r|r.get(0)).map_err(sql)?;
        if !proven
            || !covers(
                db,
                account,
                "account",
                h["parents"][0].as_str().ok_or("invalid_game_payload")?,
                target,
            )?
        {
            return Err("game_parent_unknown".into());
        }
    }
    verify_effect(e)
}
/// Pure causal transition. Deferred action families fail closed until their complete
/// semantic effects (including health/challenges) have an admitted typed projection.
pub fn transition(e: &Value, parents: &BTreeMap<String, Value>) -> Result<Value> {
    transition_with_source(e, parents, None)
}
fn transition_with_source(
    e: &Value,
    parents: &BTreeMap<String, Value>,
    source: Option<&Value>,
) -> Result<Value> {
    codec::validate(e)?;
    verify_effect(e)?;
    let a = &e["action"];
    let h = &e["header"];
    let kind = text(a, "kind")?;
    if matches!(kind, "genesis" | "adopt_local") {
        return Ok(if h["scope"] == "project" {
            json!({"streak":a["base"],"completion_claimed":a["completion_claimed"]})
        } else {
            a["base"].clone()
        });
    }
    let parent_ids = ids(e)?;
    let selected = if kind == "resolution" {
        text(a, "selected_event_id")?
    } else {
        &parent_ids[0]
    };
    let mut state = parents.get(selected).ok_or("game_parent_unknown")?.clone();
    if kind == "resolution" {
        return Ok(state);
    }
    if h["scope"] == "project" {
        match kind {
            "writing" => {}
            "completion" => {
                if state["completion_claimed"] != false {
                    return Err("game_invalid_rule".into());
                }
                state["completion_claimed"] = json!(true);
            }
            _ => return Err("game_unsupported_local_mutation".into()),
        }
    } else {
        match kind {
            "reward" => {
                let source = source.ok_or("game_dependency_project_action_missing")?;
                if source["header"]["scope"] != "project"
                    || source["header"]["event_id"] != a["project_action_id"]
                    || source["header"]["project_id"] != a["project_id"]
                    || source["header"]["account_id"] != h["account_id"]
                    || source["header"]["rule"] != h["rule"]
                    || source["action"]["reward"] != a["reward"]
                {
                    return Err("game_reward_duplicate_mismatch".into());
                }
                verify_effect(source)?;
                let writing = source["action"]["kind"] == "writing";
                if !matches!(
                    h["rule"].as_str(),
                    Some("native-game-v1" | "python-game-v1")
                ) || !writing && source["action"]["kind"] != "completion"
                {
                    return Err("game_invalid_rule".into());
                }
                if writing {
                    for (input, field) in [
                        ("inspiration", "inspiration"),
                        ("writing_bonus", "writing_bonus"),
                        ("coin_coefficient", "coin_coefficient"),
                        ("experience_coefficient", "experience_coefficient"),
                    ] {
                        if source["action"][input] != state[field] {
                            return Err("game_invalid_rule".into());
                        }
                    }
                    state["coins"] = decimal(tenth(
                        float(&state["coins"])? + float(&a["reward"]["coins"])?,
                    ))?;
                    state["writing_bonus"] = json!("0.000000");
                    if h["rule"] == "native-game-v1" {
                        let productive = state["productive_actions"]
                            .as_u64()
                            .ok_or("invalid_game_payload")?
                            + 1;
                        if productive >= 3 && state["creative_event_pending"] == "absent" {
                            state["creative_event_pending"] = json!("unexpected_idea");
                            state["productive_actions"] = json!(0);
                        } else {
                            state["productive_actions"] = json!(productive);
                        }
                    }
                } else {
                    change(&mut state, "coins", amount(&a["reward"]["coins"])?)?;
                    // Keep the established compatibility marker. Canonical
                    // action identity is independently scoped by the tuple ID.
                    let sh = &source["header"];
                    let claim = json!(sh["stage_id"]
                        .as_str()
                        .map(|s| format!("stage:{}:{s}", sh["project_id"].as_str().unwrap()))
                        .unwrap_or_else(|| format!(
                            "project:{}",
                            sh["project_id"].as_str().unwrap()
                        )));
                    let claims = state["completion_claims"]
                        .as_array_mut()
                        .ok_or("invalid_game_payload")?;
                    if claims.contains(&claim) {
                        return Err("game_invalid_rule".into());
                    }
                    claims.push(claim);
                    claims.sort_by(|a, b| {
                        a.as_str()
                            .unwrap()
                            .encode_utf16()
                            .cmp(b.as_str().unwrap().encode_utf16())
                    });
                }
                if h["rule"] == "python-game-v1" {
                    // The Python consumer historically adds raw XP. It does
                    // not execute the native level/health/productive rules.
                    change(
                        &mut state,
                        "experience",
                        amount(&a["reward"]["experience"])?,
                    )?;
                    return Ok(state);
                }
                change(
                    &mut state,
                    "experience",
                    amount(&decimal(tenth(float(&a["reward"]["experience"])?))?)?,
                )?;
                loop {
                    let level = state["level"].as_u64().ok_or("invalid_game_payload")?;
                    let threshold = 8000 * level * level * 1_000_000;
                    if level >= 99 || amount(&state["experience"])? < threshold as i128 {
                        break;
                    }
                    change(&mut state, "experience", -(threshold as i128))?;
                    change(
                        &mut state,
                        "coins",
                        (level * 250 * (100 + (level - 1) * 15) * 10_000) as i128,
                    )?;
                    state["level"] = json!(level + 1);
                    state["available_skill_points"] = json!(
                        state["available_skill_points"]
                            .as_u64()
                            .ok_or("invalid_game_payload")?
                            + 2
                    );
                    state["max_health"] = json!(progress_codec::decimal(
                        ((100 + level / 5 * 10) * 1_000_000) as i128
                    ));
                    state["health"] = state["max_health"].clone();
                }
            }
            "global_streak" if e["version"] == 2 => {
                if state["global_streak"] != a["before"]
                    || h["event_id"] != loss_id(text(h, "account_id")?, &a["before"])
                {
                    return Err("game_invalid_rule".into());
                }
                state["global_streak"] =
                    codec::loss_after(&state["global_streak"], text(a, "writing_day")?)?;
                loss_health(
                    &mut state,
                    a["after"]["lost_length"]
                        .as_u64()
                        .ok_or("invalid_game_payload")?,
                )?;
            }
            "compensation" => {
                let target = source.ok_or("game_parent_unknown")?;
                if target["header"]["scope"] != "account"
                    || target["header"]["account_id"] != h["account_id"]
                    || target["header"]["event_id"] != a["target_action_id"]
                    || target["action"]["kind"] != "reward"
                {
                    return Err("game_scope_mismatch".into());
                }
                // Explicit debit reverses the recorded grant, without reinterpreting
                // historical level advancement or silently reclaiming a deleted source.
                for field in ["coins", "experience"] {
                    let debit = amount(&a["reward"][field])?;
                    if debit != -amount(&target["action"]["reward"][field])? {
                        return Err("game_invalid_rule".into());
                    }
                    change(&mut state, field, debit)?;
                }
            }
            "inventory" => {
                if h["rule"] != "native-game-v1"
                    || !matches!(a["operation"].as_str(), Some("buy" | "sell"))
                {
                    return Err("game_unsupported_local_mutation".into());
                }
                let expected = amount(&a["unit_price"])?
                    * a["count"].as_i64().unwrap() as i128
                    * if a["operation"] == "buy" { -1 } else { 1 };
                if expected != amount(&a["coins_delta"])? {
                    return Err("game_invalid_rule".into());
                }
                let items = state["inventory"]
                    .as_array_mut()
                    .ok_or("invalid_game_payload")?;
                let position = items
                    .iter()
                    .position(|v| v["category"] == a["category"] && v["item_id"] == a["item_id"]);
                let before = position
                    .map(|p| items[p]["count"].clone())
                    .unwrap_or(json!(0));
                if before != a["before_count"] {
                    return Err("game_invalid_rule".into());
                }
                if let Some(p) = position {
                    items.remove(p);
                }
                if a["after_count"] != 0 {
                    items.push(json!({"category":a["category"],"item_id":a["item_id"],"count":a["after_count"]}));
                }
                items.sort_by(|a, b| {
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
                change(&mut state, "coins", expected)?;
            }
            _ => return Err("game_unsupported_local_mutation".into()),
        }
    }
    if h["scope"] == "account" {
        let mut check = e.clone();
        check["version"] = json!(1);
        check["header"]["parents"] = json!([]);
        check["header"]["revision"] = json!(1);
        check["header"]["rule"] = json!("legacy-game-v1");
        check["action"] = json!({"kind":"genesis","base":state});
        codec::validate(&check)?;
    }
    Ok(state)
}
/// Iterative reconstruction verifies every parent owner and max-parent revision.
/// Cached snapshots are evidence/read optimization only, never reconstruction authority.
pub fn rebuild_chain(db: &Connection, account: &str, key: &str, id: &str) -> Result<Value> {
    let mut events = HashMap::new();
    let mut pending = vec![id.to_string()];
    let mut bytes = 0usize;
    let root_account = event(db, account, id)?["header"]["account_id"].clone();
    while let Some(id) = pending.pop() {
        if events.contains_key(&id) {
            continue;
        }
        if events.len() >= MAX_ANCESTRY {
            return Err("game_resource_limit".into());
        }
        let e = event(db, account, &id)?;
        if owner(&e) != key || e["header"]["account_id"] != root_account {
            return Err("game_scope_mismatch".into());
        }
        bytes = bytes
            .checked_add(canonical(&e).len())
            .ok_or("game_resource_limit")?;
        if bytes > MAX_HISTORY_BYTES {
            return Err("game_resource_limit".into());
        }
        pending.extend(ids(&e)?);
        events.insert(id, e);
    }
    let mut order: Vec<_> = events.values().collect();
    order.sort_by_key(|e| e["header"]["revision"].as_u64());
    let mut uses: HashMap<String, usize> = HashMap::new();
    for e in events.values() {
        for parent in ids(e)? {
            *uses.entry(parent).or_default() += 1;
        }
    }
    let mut states: BTreeMap<String, Value> = BTreeMap::new();
    for e in order {
        let mut parents = BTreeMap::new();
        let mut revision = 0;
        for p in ids(e)? {
            revision = revision.max(
                events[&p]["header"]["revision"]
                    .as_u64()
                    .ok_or("invalid_game_payload")?,
            );
            let count = uses.get_mut(&p).ok_or("game_parent_unknown")?;
            *count -= 1;
            let state = if *count == 0 {
                states.remove(&p)
            } else {
                states.get(&p).cloned()
            }
            .ok_or("game_parent_unknown")?;
            parents.insert(p, state);
        }
        if e["header"]["revision"].as_u64() != Some(revision + 1) {
            return Err("game_parent_unknown".into());
        }
        let source = match e["action"]["kind"].as_str() {
            Some("reward") => Some(event(
                db,
                account,
                text(&e["action"], "project_action_id")?,
            )?),
            Some("compensation") => {
                Some(event(db, account, text(&e["action"], "target_action_id")?)?)
            }
            _ => None,
        };
        states.insert(
            text(&e["header"], "event_id")?.to_owned(),
            transition_with_source(e, &parents, source.as_ref())?,
        );
        if states.values().map(|v| canonical(v).len()).sum::<usize>() > MAX_HISTORY_BYTES {
            return Err("game_resource_limit".into());
        }
    }
    states.remove(id).ok_or("game_parent_unknown".into())
}
/// Parent reachability prevents an old self echo from rewinding a local descendant.
pub fn covers(
    db: &Connection,
    account: &str,
    key: &str,
    descendant: &str,
    ancestor: &str,
) -> Result<bool> {
    let mut pending = vec![descendant.to_owned()];
    let mut seen = HashSet::new();
    let mut bytes = 0usize;
    while let Some(id) = pending.pop() {
        if !seen.insert(id.clone()) {
            continue;
        }
        if seen.len() > MAX_ANCESTRY {
            return Err("game_resource_limit".into());
        }
        let e = event(db, account, &id)?;
        if owner(&e) != key {
            return Err("game_scope_mismatch".into());
        }
        bytes = bytes
            .checked_add(canonical(&e).len())
            .ok_or("game_resource_limit")?;
        if bytes > MAX_HISTORY_BYTES {
            return Err("game_resource_limit".into());
        }
        if id == ancestor {
            return Ok(true);
        }
        pending.extend(ids(&e)?);
    }
    Ok(false)
}

/// Sealing is a one-time operation; retry reads the original envelope verbatim.
pub fn seal(
    db: &Connection,
    account: &str,
    id: &str,
    frame: &[u8],
    nonce: &[u8],
    ciphertext: &[u8],
) -> Result<()> {
    if nonce.len() != 24 || !(16..=codec::MAX_FRAME_BYTES + 16).contains(&ciphertext.len()) {
        return Err("invalid_game_payload".into());
    }
    let compression_view =
        crate::frame_compression::canonical_view(frame).map_err(str::to_owned)?;
    let frame = compression_view.as_ref();
    let (stored,n,c,state): (Vec<u8>,Option<Vec<u8>>,Option<Vec<u8>>,String) = db.query_row("SELECT canonical_frame,nonce,ciphertext,state FROM cloud_game_events WHERE account_id=?1 AND event_id=?2",params![account,id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).map_err(sql)?;
    if stored != frame
        || n.as_ref().is_some_and(|v| v != nonce)
        || c.as_ref().is_some_and(|v| v != ciphertext)
    {
        return Err("game_exact_replay_mismatch".into());
    }
    if n.is_some() {
        return Ok(());
    }
    if state != "unsealed" {
        return Err("game_scope_mismatch".into());
    }
    db.execute("UPDATE cloud_game_events SET nonce=?1,ciphertext=?2,state='sealed' WHERE account_id=?3 AND event_id=?4",params![nonce,ciphertext,account,id]).map_err(sql)?;
    Ok(())
}
pub fn receipt(db: &Connection, account: &str, id: &str, sequence: i64) -> Result<()> {
    if !(1..=9007199254740991).contains(&sequence) {
        return Err("invalid_game_payload".into());
    }
    let (nonce,prior): (Option<Vec<u8>>,Option<i64>) = db.query_row("SELECT nonce,receipt_sequence FROM cloud_game_events WHERE account_id=?1 AND event_id=?2",params![account,id],|r|Ok((r.get(0)?,r.get(1)?))).map_err(sql)?;
    if nonce.is_none() {
        return Err("game_scope_mismatch".into());
    }
    if prior.is_some_and(|p| p != sequence) {
        return Err("game_exact_replay_mismatch".into());
    }
    if prior.is_none() {
        db.execute(
            "UPDATE cloud_game_events SET receipt_sequence=?1 WHERE account_id=?2 AND event_id=?3",
            params![sequence, account, id],
        )
        .map_err(sql)?;
    }
    // No state='applied': upload success is not authenticated self echo.
    Ok(())
}
fn reward_identity(db: &Connection, account: &str, e: &Value) -> Result<()> {
    let a = &e["action"];
    let frame = codec::frame(e)?;
    if e["header"]["scope"] != "account" || a["kind"] != "reward" {
        return Err("game_scope_mismatch".into());
    }
    let source = event(db, account, text(a, "project_action_id")?)?;
    if source["header"]["scope"] != "project"
        || source["header"]["account_id"] != e["header"]["account_id"]
        || source["header"]["project_id"] != a["project_id"]
        || source["header"]["rule"] != e["header"]["rule"]
        || source["action"]["reward"] != a["reward"]
        || !matches!(
            source["action"]["kind"].as_str(),
            Some("writing" | "completion" | "streak")
        )
    {
        return Err("game_reward_duplicate_mismatch".into());
    }
    let prior: Option<(String,String,Vec<u8>)> = db.query_row("SELECT event_id,project_action_id,canonical_frame FROM cloud_game_rewards WHERE account_id=?1 AND (reward_id=?2 OR project_action_id=?3 OR event_id=?4)",params![account,text(a,"reward_id")?,text(a,"project_action_id")?,text(&e["header"],"event_id")?],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(sql)?;
    if let Some((id, source, prior)) = prior {
        if id != text(&e["header"], "event_id")?
            || source != text(a, "project_action_id")?
            || prior != frame
        {
            return Err("game_reward_duplicate_mismatch".into());
        }
    } else {
        db.execute(
            "INSERT INTO cloud_game_rewards VALUES(?1,?2,?3,?4,?5)",
            params![
                account,
                text(a, "reward_id")?,
                text(&e["header"], "event_id")?,
                text(a, "project_action_id")?,
                frame
            ],
        )
        .map_err(sql)?;
    }
    Ok(())
}
/// Durable paired intents. This internal boundary accepts only the two frozen
/// action variants and validates stable IDs before any insertion.
/// The caller owns the enclosing local semantic transaction.
fn queue_reward_pair_inner(
    db: &Connection,
    account: &str,
    source_key: &str,
    project: &Value,
    reward: &Value,
) -> Result<bool> {
    codec::validate(project)?;
    codec::validate(reward)?;
    verify_effect(project)?;
    let ph = &project["header"];
    let pa = &project["action"];
    let rh = &reward["header"];
    let ra = &reward["action"];
    if ph["scope"] != "project"
        || rh["scope"] != "account"
        || ra["kind"] != "reward"
        || ph["account_id"] != rh["account_id"]
        || !matches!(pa["kind"].as_str(), Some("writing" | "completion"))
    {
        return Err("game_scope_mismatch".into());
    }
    let source = if pa["kind"] == "writing" {
        json!([
            ph["project_id"],
            ph["stage_id"],
            pa["progress_event_id"],
            pa["fact"]["entry_id"]
        ])
    } else {
        json!([ph["project_id"], ph["stage_id"], pa["completion_id"]])
    };
    let pid = stable_action_id(text(ph, "account_id")?, text(pa, "kind")?, &source);
    let rid = stable_action_id(text(ph, "account_id")?, "reward", &json!(pid));
    if ph["event_id"] != pid || rh["event_id"] != rid || ra["project_action_id"] != pid {
        return Err("game_scope_mismatch".into());
    }
    let old: Option<(String,Option<String>)>=db.query_row("SELECT project_action_id,reward_action_id FROM cloud_game_sources WHERE account_id=?1 AND source_key=?2",params![account,source_key],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(sql)?;
    if old
        .as_ref()
        .is_some_and(|(p, r)| p != &pid || r.as_deref() != Some(&rid))
    {
        return Err("game_reward_duplicate_mismatch".into());
    }
    // A savepoint also protects callers that handle an error without rolling back
    // their outer transaction: never leave half of the source/reward pair behind.
    db.execute_batch("SAVEPOINT game_reward_pair")
        .map_err(sql)?;
    let result = (|| {
        preserve(db, account, project, "unsealed")?;
        preserve(db, account, reward, "unsealed")?;
        reward_identity(db, account, reward)?;
        if old.is_none() {
            db.execute(
                "INSERT INTO cloud_game_sources VALUES(?1,?2,?3,?4)",
                params![account, source_key, pid, rid],
            )
            .map_err(sql)?;
        }
        Ok(old.is_none())
    })();
    if result.is_err() {
        db.execute_batch("ROLLBACK TO game_reward_pair")
            .map_err(sql)?;
    }
    db.execute_batch("RELEASE game_reward_pair").map_err(sql)?;
    result
}

fn blocker(db: &Connection, account: &str, key: &str, id: &str, code: &str) -> Result<()> {
    db.execute("INSERT INTO cloud_game_blockers VALUES(?1,?2,?3,?4) ON CONFLICT(account_id,owner_key,event_id) DO UPDATE SET code=excluded.code",params![account,key,id,code]).map_err(sql)?;
    Ok(())
}
pub fn queue_reward_pair(
    db: &Connection,
    account: &str,
    source_key: &str,
    project: &Value,
    reward: &Value,
) -> Result<bool> {
    let result = queue_reward_pair_inner(db, account, source_key, project, reward);
    if let Err(code) = &result {
        if matches!(
            code.as_str(),
            "game_exact_replay_mismatch" | "game_reward_duplicate_mismatch"
        ) {
            blocker(
                db,
                account,
                "account",
                text(&reward["header"], "event_id")?,
                code,
            )?;
        }
    }
    result
}

/// No timestamp, insertion order or server sequence participates in choosing tips.
pub fn next_tips(db: &Connection, account: &str, e: &Value) -> Result<Vec<String>> {
    let key = owner(e);
    let id = text(&e["header"], "event_id")?;
    let old = tips(db, account, &key)?;
    let mut next = Vec::new();
    let mut descended = false;
    for tip in old {
        if tip == id {
            next.push(tip);
            continue;
        }
        if covers(db, account, &key, &tip, id)? {
            descended = true;
            next.push(tip);
        } else if !covers(db, account, &key, id, &tip)? {
            next.push(tip);
        }
    }
    if !descended {
        next.push(id.to_owned());
    }
    next.sort();
    next.dedup();
    if next.len() > 64 {
        return Err("game_resource_limit".into());
    }
    Ok(next)
}
#[derive(Debug)]
struct ApplyError(String);
impl From<String> for ApplyError {
    fn from(s: String) -> Self {
        Self(s)
    }
}
impl From<&str> for ApplyError {
    fn from(s: &str) -> Self {
        Self(s.into())
    }
}
impl From<rusqlite::Error> for ApplyError {
    fn from(_: rusqlite::Error) -> Self {
        Self("game_storage_error".into())
    }
}
impl From<crate::sqlite::StorageError> for ApplyError {
    fn from(_: crate::sqlite::StorageError) -> Self {
        Self("game_storage_error".into())
    }
}
struct ApplyPlan {
    sequence: i64,
    outcome: String,
    snapshot: Value,
    tips: Vec<String>,
    payload: Option<String>,
}
fn retain_blocker(db: &Connection, a: &str, key: &str, id: &str, code: &str) -> Result<()> {
    db.execute("INSERT INTO cloud_game_blockers VALUES(?1,?2,?3,?4) ON CONFLICT(account_id,owner_key,event_id) DO UPDATE SET code=excluded.code",params![a,key,id,code]).map_err(sql)?;
    Ok(())
}
/// Match the entire authenticated envelope against the retained transport row.
/// No sequence-only receipt or compatibility snapshot is accepted as proof.
fn inbox_sequence(db: &Connection, a: &str, e: &Value, n: &[u8], c: &[u8]) -> Result<i64> {
    let h = &e["header"];
    let seq: Option<i64> = if h["scope"] == "account" {
        db.query_row("SELECT server_sequence FROM cloud_sync_account_inbox WHERE account_id=?1 AND event_id=?2 AND canonical_user_id=?3 AND scope='account' AND device_id=?4 AND entity_id=?5 AND entity_type='account_game' AND operation='upsert' AND sync_revision=?6 AND updated_at=?7 AND deleted_at IS NULL AND crypto_version=2 AND aad_version=2 AND nonce=?8 AND ciphertext=?9",params![a,text(h,"event_id")?,text(h,"account_id")?,text(h,"device_id")?,text(h,"entity_id")?,h["revision"].as_i64(),text(h,"updated_at")?,n,c],|r|r.get(0)).optional().map_err(sql)?
    } else {
        db.query_row("SELECT i.server_sequence FROM cloud_sync_inbox i JOIN cloud_sync_event_objects o USING(account_id,event_id) WHERE i.account_id=?1 AND i.event_id=?2 AND i.project_id=?3 AND i.device_id=?4 AND i.entity_id=?5 AND i.entity_type='project_game' AND i.operation='event' AND i.sync_revision=?6 AND i.updated_at=?7 AND i.deleted_at IS NULL AND o.crypto_version=1 AND o.aad_version=1 AND o.nonce=?8 AND o.ciphertext=?9",params![a,text(h,"event_id")?,text(h,"project_id")?,text(h,"device_id")?,text(h,"entity_id")?,h["revision"].as_i64(),text(h,"updated_at")?,n,c],|r|r.get(0)).optional().map_err(sql)?
    };
    seq.ok_or("game_scope_mismatch".into())
}
fn local_source(db: &Connection) -> Result<(Option<String>, Value)> {
    let raw: Option<String> = db
        .query_row("SELECT payload_json FROM game_state WHERE id=1", [], |r| {
            r.get(0)
        })
        .optional()
        .map_err(sql)?;
    let value = match &raw {
        Some(raw) => serde_json::from_str(raw).map_err(|_| "game_legacy_extension_unsupported")?,
        None => {
            json!({"gamer":serde_json::from_str::<Value>(include_str!("../../src/cloud/__fixtures__/gameLegacyDefaultsV1.json")).map_err(|_|"game_storage_error")?})
        }
    };
    Ok((raw, value))
}
fn local_matches(db: &Connection, a: &str, e: &Value, current: &Value) -> Result<bool> {
    let key = owner(e);
    let prior: Option<String> = db
        .query_row(
            "SELECT snapshot_json FROM cloud_game_projection WHERE account_id=?1 AND owner_key=?2",
            params![a, key],
            |r| r.get(0),
        )
        .optional()
        .map_err(sql)?;
    let actual = if e["header"]["scope"] == "account" {
        crate::game_projection::account_base(current)
    } else {
        let p = text(&e["header"], "project_id")?;
        let s = e["header"]["stage_id"].as_str();
        let src = crate::game_migration::owner_source(db, current, p, s)?;
        crate::game_projection::streak_base(&src,false).map(|streak| {
            let legacy=s.map(|s|format!("stage:{p}:{s}")).unwrap_or_else(||format!("project:{p}"));
            let claims=&current["gamer"]["complete_bonus_projects"];
            json!({"streak":streak,"completion_claimed":claims.as_array().is_some_and(|v|v.contains(&json!(legacy))||v.contains(&json!(format!("completion:{key}"))))})
        })
    };
    let Ok(actual) = actual else { return Ok(false) };
    if let Some(raw) = prior {
        return Ok(
            actual == serde_json::from_str::<Value>(&raw).map_err(|_| "invalid_game_payload")?
        );
    }
    let candidate:Option<String>=db.query_row("SELECT c.base_json FROM cloud_game_migrations m JOIN cloud_game_candidates c USING(candidate_id) WHERE m.account_id=?1 AND m.owner_key=?2",params![a,key],|r|r.get(0)).optional().map_err(sql)?.flatten();
    if let Some(raw) = candidate {
        let base: Value = serde_json::from_str(&raw).map_err(|_| "invalid_game_payload")?;
        // Account candidate stores base; Project candidate stores the typed event.
        let expected = if e["header"]["scope"] == "account" {
            base
        } else {
            transition(&base, &BTreeMap::new())?
        };
        return Ok(actual == expected);
    }
    // A clean second device may import. Nonempty independent local state needs
    // an explicit full-tip decision, with recovery evidence retained below.
    let default = json!({"gamer":serde_json::from_str::<Value>(include_str!("../../src/cloud/__fixtures__/gameLegacyDefaultsV1.json")).map_err(|_|"game_storage_error")?});
    let baseline = if e["header"]["scope"] == "account" {
        crate::game_projection::account_base(&default)?
    } else {
        json!({"streak":crate::game_projection::streak_base(&json!({}),false)?,"completion_claimed":false})
    };
    Ok(actual == baseline)
}
/// Atomic native apply for both cryptographic domains. This boundary is internal
/// until the paired production readers and capability declaration are activated.
pub fn apply(
    db: &mut crate::sqlite::PrivilegedRemoteApplyConnection,
    scope: &metadata::MetadataScope,
    frame: &[u8],
    nonce: &[u8],
    ciphertext: &[u8],
    project: bool,
) -> Result<String> {
    let compression_view =
        crate::frame_compression::canonical_view(frame).map_err(str::to_owned)?;
    let frame = compression_view.as_ref();
    let e = codec::unframe(frame, project)?;
    let h = &e["header"];
    let a = &scope.account_id;
    let id = text(h, "event_id")?;
    let key = owner(&e);
    db.execute_planned_many_once(|tx|->std::result::Result<(_, (String,Option<ApplyPlan>)),ApplyError>{
        metadata::assert_runtime_scope(tx,a,&scope.canonical_user_id,&scope.device_id).map_err(|_|"game_scope_mismatch")?;
        if h["account_id"]!=scope.canonical_user_id{return Err("game_scope_mismatch".into())}
        let sequence=inbox_sequence(tx,a,&e,nonce,ciphertext)?;
        let prior:Option<(Vec<u8>,Option<Vec<u8>>,Option<Vec<u8>>,Option<i64>,Option<i64>)>=tx.query_row("SELECT canonical_frame,nonce,ciphertext,server_sequence,receipt_sequence FROM cloud_game_events WHERE account_id=?1 AND event_id=?2",params![a,id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
        if prior.as_ref().is_some_and(|(f,n,c,s,r)|f!=frame||n.as_ref().is_some_and(|n|n!=nonce)||c.as_ref().is_some_and(|c|c!=ciphertext)||s.is_some_and(|s|s!=sequence)||r.is_some_and(|s|s!=sequence)) {
            retain_blocker(tx,a,&key,id,"game_exact_replay_mismatch")?;
            return Ok((vec![],("waiting".into(),None)));
        }
        let proof:Option<String>=tx.query_row("SELECT outcome FROM cloud_game_apply_ledger WHERE account_id=?1 AND event_id=?2 AND server_sequence=?3 AND nonce=?4 AND ciphertext=?5 AND canonical_frame=?6",params![a,id,sequence,nonce,ciphertext,frame],|r|r.get(0)).optional()?;
        if let Some(outcome)=proof{return Ok((vec![],(outcome,None)))}
        preserve(tx,a,&e,"waiting")?;
        tx.execute("UPDATE cloud_game_events SET nonce=?1,ciphertext=?2,server_sequence=?3 WHERE account_id=?4 AND event_id=?5",params![nonce,ciphertext,sequence,a,id])?;
        let ready=(||->Result<Value>{
            dependencies(tx,a,&scope.canonical_user_id,&e)?;
            for parent in ids(&e)? {
                let proven:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM cloud_game_apply_ledger l JOIN cloud_game_events e USING(account_id,event_id) WHERE l.account_id=?1 AND l.event_id=?2 AND e.owner_key=?3 AND e.state=l.outcome AND e.server_sequence=l.server_sequence AND e.nonce=l.nonce AND e.ciphertext=l.ciphertext AND e.canonical_frame=l.canonical_frame)",params![a,parent,key],|r|r.get(0)).map_err(sql)?;
                if !proven{return Err("game_parent_unknown".into())}
            }
            rebuild_chain(tx,a,&key,id)
        })();
        let snapshot=match ready {Ok(v)=>v,Err(code)=>{if code=="game_storage_error"{return Err(code.into())}retain_blocker(tx,a,&key,id,&code)?;return Ok((vec![],("waiting".into(),None)))}};
        if e["action"]["kind"]=="reward" {
            tx.execute_batch("SAVEPOINT game_reward_validation")?;
            let valid=reward_identity(tx,a,&e);
            tx.execute_batch("ROLLBACK TO game_reward_validation; RELEASE game_reward_validation")?;
            if let Err(code)=valid {if code=="game_storage_error"{return Err(code.into())}retain_blocker(tx,a,&key,id,&code)?;return Ok((vec![],("waiting".into(),None)))}
        }
        if e["action"]["kind"]=="compensation" {queue_compensation(tx,a,&e)?;}
        let next=next_tips(tx,a,&e)?;
        let (raw,current)=local_source(tx)?;
        let own=h["device_id"]==scope.device_id&&prior.as_ref().is_some_and(|(_,n,_,_,_)|n.is_some());
        let current_head:Option<String>=tx.query_row("SELECT head_event_id FROM cloud_game_projection WHERE account_id=?1 AND owner_key=?2",params![a,key],|r|r.get(0)).optional()?;
        let descended=match &current_head {Some(head)=>covers(tx,a,&key,head,id)?,None=>false};
        let matching=local_matches(tx,a,&e,&current)?;
        let decision:Option<String>=tx.query_row("SELECT expected_payload_json FROM cloud_game_decisions WHERE account_id=?1 AND event_id=?2",params![a,id],|r|r.get(0)).optional()?;
        let explicit=decision.as_ref().is_some_and(|v|serde_json::from_str::<Value>(v).ok().as_ref()==Some(&current));
        let conflict=next.len()>1||!matching&&!explicit;
        if !matching&&!explicit {
            tx.execute("INSERT OR IGNORE INTO cloud_game_candidates VALUES(?1,?2,?3,?4,NULL,?5)",params![id,a,key,canonical(&current),text(h,"updated_at")?])?;
        }
        let payload=if !conflict&&!descended {
            let mut value=if project {crate::game_projection::materialize_project(&current,text(h,"project_id")?,h["stage_id"].as_str(),&snapshot)?} else {crate::game_projection::materialize_account(&current,&snapshot)?};
            if e["version"]==2 && e["action"]["kind"]=="global_streak" {
                value["gamer"]["last_health_recovery_at"]=json!({"__type__":"datetime","value":h["updated_at"]});
                crate::game::append_loss_notifications(&current,&mut value,id,text(h,"updated_at")?)?;
            }
            Some(canonical(&value))
        } else {None};
        if own&&!matching&&!explicit{retain_blocker(tx,a,&key,id,"game_legacy_local_conflict")?;}
        let auth=payload.as_ref().map(|payload|crate::sqlite::OwnedRemoteApplyAuthorization{event_id:id.into(),account_id:a.clone(),project_id:h["project_id"].as_str().unwrap_or("account").into(),entity_id:"game_state".into(),operation:"upsert".into(),payload_json:Some(payload.clone()),prior_payload_json:raw}).into_iter().collect();
        let outcome=if conflict{"conflict_preserved"}else{"applied"}.to_string();
        Ok((auth,(outcome.clone(),Some(ApplyPlan{sequence,outcome,snapshot,tips:next,payload}))))
    },|tx,(result,plan)|->std::result::Result<String,ApplyError>{
        let Some(plan)=plan else{return Ok(result)};
        if e["action"]["kind"]=="reward"{reward_identity(tx,a,&e)?;}
        if let Some(payload)=plan.payload {
            tx.execute("INSERT INTO cloud_game_projection VALUES(?1,?2,?3,?4,?5) ON CONFLICT(account_id,owner_key) DO UPDATE SET head_event_id=excluded.head_event_id,snapshot_json=excluded.snapshot_json,generation=excluded.generation",params![a,key,id,canonical(&plan.snapshot),h["revision"].as_i64()])?;
            tx.execute("INSERT INTO game_state(id,schema_version,payload_json,updated_at) VALUES(1,2,?1,?2) ON CONFLICT(id) DO UPDATE SET payload_json=excluded.payload_json,updated_at=excluded.updated_at",params![payload,text(h,"updated_at")?])?;
            tx.execute("DELETE FROM cloud_sync_remote_apply_authorizations WHERE event_id=?1",[id])?;
        }
        tx.execute("DELETE FROM cloud_game_tips WHERE account_id=?1 AND owner_key=?2",params![a,key])?;
        for tip in plan.tips {tx.execute("INSERT INTO cloud_game_tips VALUES(?1,?2,?3)",params![a,key,tip])?;}
        tx.execute("INSERT INTO cloud_game_snapshots VALUES(?1,?2,?3)",params![a,id,canonical(&plan.snapshot)])?;
        tx.execute("UPDATE cloud_game_events SET state=?1 WHERE account_id=?2 AND event_id=?3",params![plan.outcome,a,id])?;
        tx.execute("INSERT INTO cloud_game_apply_ledger VALUES(?1,?2,?3,?4,?5,?6,?7)",params![a,id,plan.sequence,plan.outcome,nonce,ciphertext,frame])?;
        if project {
            tx.execute("UPDATE cloud_sync_inbox SET state=?1,applied_at=?2,error_code=NULL WHERE account_id=?3 AND event_id=?4",params![if plan.outcome=="applied"{"applied"}else{"conflict"},text(h,"updated_at")?,a,id])?;
        } else {
            tx.execute("UPDATE cloud_sync_account_inbox SET state='received',error_code=NULL WHERE account_id=?1 AND event_id=?2",params![a,id])?;
        }
        // A pre-decryption failure has no authenticated owner yet. Exact inbox
        // authentication above now proves the event, including that earlier row.
        tx.execute("DELETE FROM cloud_game_blockers WHERE account_id=?1 AND event_id=?2",params![a,id])?;
        if plan.outcome=="applied" && e["action"]["kind"]=="resolution" {
            // The immutable branch/conflict receipts remain. Only the visible
            // blocker closed by this authenticated full-tip decision is cleared.
            tx.execute("DELETE FROM cloud_game_blockers WHERE account_id=?1 AND owner_key=?2 AND code='game_noncommutative_conflict'",params![a,key])?;
        }
        if plan.outcome=="conflict_preserved"{retain_blocker(tx,a,&key,id,"game_noncommutative_conflict")?;}
        tx.execute("INSERT OR IGNORE INTO cloud_game_migrations VALUES(?1,?2,?3,'self_echo_pending',NULL)",params![a,key,id])?;
        tx.execute("UPDATE cloud_game_migrations SET lifecycle=?1,blocker=?2 WHERE account_id=?3 AND owner_key=?4",params![if plan.outcome=="applied"{"self_echo_pending"}else{"conflict"},if plan.outcome=="applied"{None}else{Some("game_noncommutative_conflict")},a,key])?;
        // One accepted genesis cannot prematurely activate a multi-owner capture.
        let complete:bool=tx.query_row("SELECT NOT EXISTS(SELECT 1 FROM cloud_game_migrations m LEFT JOIN cloud_game_events e ON e.account_id=m.account_id AND e.event_id=m.candidate_id LEFT JOIN cloud_game_apply_ledger l ON l.account_id=e.account_id AND l.event_id=e.event_id WHERE m.account_id=?1 AND (m.lifecycle IN ('blocked','conflict') OR e.state IS NOT 'applied' OR l.outcome IS NOT 'applied')) AND EXISTS(SELECT 1 FROM cloud_game_migrations WHERE account_id=?1 AND owner_key='account') AND NOT EXISTS(SELECT 1 FROM cloud_sync_project_bindings b WHERE b.account_id=?1 AND NOT EXISTS(SELECT 1 FROM cloud_game_migrations m WHERE m.account_id=b.account_id AND m.owner_key=json_array(b.project_id,NULL))) AND NOT EXISTS(SELECT 1 FROM stages s JOIN cloud_sync_project_bindings b ON b.project_id=s.project_id WHERE b.account_id=?1 AND NOT EXISTS(SELECT 1 FROM cloud_game_migrations m WHERE m.account_id=b.account_id AND m.owner_key=json_array(b.project_id,s.id)))",[a],|r|r.get(0))?;
        if complete {tx.execute("UPDATE cloud_game_migrations SET lifecycle='active',blocker=NULL WHERE account_id=?1 AND lifecycle='self_echo_pending'",[a])?;}
        Ok(plan.outcome)
    }).map_err(|error:ApplyError|error.0)
}

/// Refresh a derived compatibility view from complete authenticated history.
/// Local recovery evidence and unsupported extensions are never overwritten.
pub fn rebuild(
    db: &mut crate::sqlite::PrivilegedRemoteApplyConnection,
    scope: &metadata::MetadataScope,
    key: &str,
) -> Result<()> {
    db.execute_planned_many_once(|tx|->std::result::Result<(_, (Value,Value,String)),ApplyError>{
        metadata::assert_runtime_scope(tx,&scope.account_id,&scope.canonical_user_id,&scope.device_id).map_err(|_|"game_scope_mismatch")?;
        let heads=tips(tx,&scope.account_id,key)?;
        if heads.len()!=1{return Err("game_noncommutative_conflict".into())}
        let e=event(tx,&scope.account_id,&heads[0])?;let h=&e["header"];
        if owner(&e)!=key||h["account_id"]!=scope.canonical_user_id{return Err("game_scope_mismatch".into())}
        let seq:Option<i64>=tx.query_row("SELECT server_sequence FROM cloud_game_apply_ledger WHERE account_id=?1 AND event_id=?2 AND outcome='applied'",params![scope.account_id,heads[0]],|r|r.get(0)).optional()?;
        let seq=seq.ok_or("game_parent_unknown")?;
        if !ack_proven(tx,&scope.account_id,seq)?{return Err("game_parent_unknown".into())}
        let snapshot=rebuild_chain(tx,&scope.account_id,key,&heads[0])?;
        let (raw,current)=local_source(tx)?;
        let refreshed=if h["scope"]=="account"{crate::game_projection::materialize_account(&current,&snapshot)?}else{crate::game_projection::materialize_project(&current,text(h,"project_id")?,h["stage_id"].as_str(),&snapshot)?};
        let payload=canonical(&refreshed);
        let auth=crate::sqlite::OwnedRemoteApplyAuthorization{event_id:heads[0].clone(),account_id:scope.account_id.clone(),project_id:h["project_id"].as_str().unwrap_or("account").into(),entity_id:"game_state".into(),operation:"upsert".into(),payload_json:Some(payload.clone()),prior_payload_json:raw};
        Ok((vec![auth],(e,snapshot,payload)))
    },|tx,(e,snapshot,payload)|->std::result::Result<(),ApplyError>{
        let h=&e["header"];let id=text(h,"event_id")?;
        tx.execute("INSERT INTO cloud_game_projection VALUES(?1,?2,?3,?4,?5) ON CONFLICT(account_id,owner_key) DO UPDATE SET head_event_id=excluded.head_event_id,snapshot_json=excluded.snapshot_json,generation=excluded.generation",params![scope.account_id,key,id,canonical(&snapshot),h["revision"].as_i64()])?;
        tx.execute("UPDATE game_state SET payload_json=?1 WHERE id=1",[payload])?;
        tx.execute("DELETE FROM cloud_sync_remote_apply_authorizations WHERE event_id=?1",[id])?;
        Ok(())
    }).map_err(|e:ApplyError|e.0)
}

/// A single account-wide cursor consumes this exact retained proof for either
/// Game domain. Waiting/mismatched envelopes never become prefix receipts.
pub fn ack_proven(db: &Connection, a: &str, sequence: i64) -> rusqlite::Result<bool> {
    let row:Option<(String,Vec<u8>,Vec<u8>,Vec<u8>,String,String)>=db.query_row("SELECT e.event_id,e.canonical_frame,l.nonce,l.ciphertext,e.scope,l.outcome FROM cloud_game_events e JOIN cloud_game_apply_ledger l USING(account_id,event_id) JOIN cloud_game_snapshots s USING(account_id,event_id) WHERE e.account_id=?1 AND e.server_sequence=?2 AND l.server_sequence=e.server_sequence AND e.state=l.outcome AND l.canonical_frame=e.canonical_frame AND l.nonce=e.nonce AND l.ciphertext=e.ciphertext AND NOT EXISTS(SELECT 1 FROM cloud_game_blockers b WHERE b.account_id=e.account_id AND b.event_id=e.event_id AND b.code<>'game_noncommutative_conflict')",params![a,sequence],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).optional()?;
    let Some((id, frame, n, c, scope, outcome)) = row else {
        return Ok(false);
    };
    let Ok(e) = codec::unframe(&frame, scope == "project") else {
        return Ok(false);
    };
    if e["header"]["event_id"] != id || inbox_sequence(db, a, &e, &n, &c).ok() != Some(sequence) {
        return Ok(false);
    }
    let user: Option<String> = db
        .query_row(
            "SELECT canonical_user_id FROM cloud_account_bindings WHERE local_account_id=?1",
            [a],
            |r| r.get(0),
        )
        .optional()?;
    if user.as_deref() != e["header"]["account_id"].as_str() {
        return Ok(false);
    }
    if scope == "account" {
        db.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_account_inbox WHERE account_id=?1 AND event_id=?2 AND server_sequence=?3 AND state='received' AND error_code IS NULL)",params![a,id,sequence],|r|r.get(0))
    } else {
        db.query_row("SELECT EXISTS(SELECT 1 FROM cloud_sync_inbox WHERE account_id=?1 AND event_id=?2 AND server_sequence=?3 AND state=?4 AND error_code IS NULL)",params![a,id,sequence,if outcome=="applied"{"applied"}else{"conflict"}],|r|r.get(0))
    }
}

/// Exact full-tip CAS. Queueing never marks the decision authoritative before echo.
/// An outer immediate transaction serializes this check with incoming branches.
pub fn queue_resolution(
    db: &Connection,
    account: &str,
    e: &Value,
    expected_tips: &[String],
    expected_local: &Value,
) -> Result<()> {
    codec::validate(e)?;
    let key = owner(e);
    let h = &e["header"];
    let a = &e["action"];
    if a["kind"] != "resolution"
        || ids(e)? != expected_tips
        || tips(db, account, &key)? != expected_tips
    {
        return Err("game_noncommutative_conflict".into());
    }
    let prior: Option<String> = db
        .query_row(
            "SELECT snapshot_json FROM cloud_game_projection WHERE account_id=?1 AND owner_key=?2",
            params![account, key],
            |r| r.get(0),
        )
        .optional()
        .map_err(sql)?;
    let local = prior
        .map(|v| serde_json::from_str::<Value>(&v).map_err(|_| "invalid_game_payload"))
        .transpose()?
        .unwrap_or(Value::Null);
    if local != *expected_local {
        return Err("game_legacy_local_conflict".into());
    }
    let pending:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM cloud_game_events WHERE account_id=?1 AND owner_key=?2 AND state IN ('unsealed','sealed') AND json_extract(CAST(substr(canonical_frame,21) AS TEXT),'$.action.kind')='resolution')",params![account,key],|r|r.get(0)).map_err(sql)?;
    if pending {
        return Err("game_noncommutative_conflict".into());
    }
    let mut revision = 0u64;
    for parent in expected_tips {
        let p = event(db, account, parent)?;
        if owner(&p) != key || p["header"]["account_id"] != h["account_id"] {
            return Err("game_scope_mismatch".into());
        }
        revision = revision.max(
            p["header"]["revision"]
                .as_u64()
                .ok_or("invalid_game_payload")?,
        );
    }
    if h["revision"].as_u64() != Some(revision + 1) {
        return Err("game_parent_unknown".into());
    }
    // Validate the selected branch before retaining an irreversible decision.
    rebuild_chain(db, account, &key, text(a, "selected_event_id")?)?;
    db.execute_batch("SAVEPOINT game_resolution").map_err(sql)?;
    let result = (|| {
        preserve(db, account, e, "unsealed")?;
        db.execute(
            "INSERT INTO cloud_game_decisions VALUES(?1,?2,?3,?4,?5)",
            params![
                account,
                text(h, "event_id")?,
                canonical(&json!(expected_tips)),
                canonical(expected_local),
                canonical(&local_source(db)?.1)
            ],
        )
        .map_err(sql)?;
        Ok(())
    })();
    if result.is_err() {
        db.execute_batch("ROLLBACK TO game_resolution")
            .map_err(sql)?;
    }
    db.execute_batch("RELEASE game_resolution").map_err(sql)?;
    result
}
/// Stable compensation identity and database uniqueness make a repeated explicit
/// reversal a retry of the same fact. Original reward bytes are never modified.
pub fn queue_compensation(db: &Connection, account: &str, e: &Value) -> Result<bool> {
    codec::validate(e)?;
    let h = &e["header"];
    let a = &e["action"];
    let key = owner(e);
    if h["scope"] != "account" || a["kind"] != "compensation" {
        return Err("game_scope_mismatch".into());
    }
    let target = text(a, "target_action_id")?;
    let id = text(h, "event_id")?;
    if stable_action_id(text(h, "account_id")?, "compensation", &json!(target)) != id {
        return Err("game_scope_mismatch".into());
    }
    let parents = ids(e)?;
    if !covers(db, account, &key, &parents[0], target)? {
        return Err("game_parent_unknown".into());
    }
    let source = event(db, account, target)?;
    let base = rebuild_chain(db, account, &key, &parents[0])?;
    transition_with_source(
        e,
        &BTreeMap::from([(parents[0].clone(), base)]),
        Some(&source),
    )?;
    db.execute_batch("SAVEPOINT game_compensation")
        .map_err(sql)?;
    let result = (|| {
        let fresh = preserve(db, account, e, "unsealed")?;
        let prior:Option<String>=db.query_row("SELECT event_id FROM cloud_game_compensations WHERE account_id=?1 AND target_action_id=?2",params![account,target],|r|r.get(0)).optional().map_err(sql)?;
        if prior.as_deref().is_some_and(|v| v != id) {
            return Err("game_reward_duplicate_mismatch".into());
        }
        if prior.is_none() {
            db.execute(
                "INSERT INTO cloud_game_compensations VALUES(?1,?2,?3)",
                params![account, target, id],
            )
            .map_err(sql)?;
        }
        Ok(fresh)
    })();
    if result.is_err() {
        db.execute_batch("ROLLBACK TO game_compensation")
            .map_err(sql)?;
    }
    db.execute_batch("RELEASE game_compensation").map_err(sql)?;
    result
}

pub fn record_developer_override(db: &Connection, source: &Value, now: &str) -> Result<()> {
    db.execute("INSERT OR IGNORE INTO cloud_game_local_restrictions VALUES('game_developer_state_restricted',?1,?2)",params![canonical(source),now]).map_err(sql)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    const ACCOUNT: &str = "local-game-account";
    fn vectors() -> Vec<Value> {
        serde_json::from_str::<Value>(include_str!(
            "../../src/cloud/__fixtures__/gameCodecV1.json"
        ))
        .unwrap()["examples"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["event"].clone())
            .collect()
    }
    fn loss_fixture() -> (Value, Value, String) {
        let mut genesis = vectors()
            .into_iter()
            .find(|v| v["header"]["scope"] == "account" && v["action"]["kind"] == "genesis")
            .unwrap();
        genesis["action"]["base"]["global_streak"]["history"] = json!([
            {"day":"2026-10-01","frozen":false},{"day":"2026-10-02","frozen":false},{"day":"2026-10-03","frozen":false}]);
        genesis["action"]["base"]["global_streak"]["maximum"] = json!(3);
        let base = genesis["action"]["base"].clone();
        let parent = genesis["header"]["event_id"].as_str().unwrap().to_string();
        let mut e = genesis.clone();
        let id = loss_id(
            e["header"]["account_id"].as_str().unwrap(),
            &base["global_streak"],
        );
        e["version"] = json!(2);
        e["header"]["rule"] = json!("native-game-v1");
        e["header"]["event_id"] = json!(id);
        e["header"]["entity_id"] = json!(format!("game:{id}"));
        e["header"]["parents"] = json!([parent]);
        e["header"]["revision"] = json!(2);
        e["action"] = json!({"kind":"global_streak","writing_day":"2026-10-05","project_action_ids":[],"before":base["global_streak"],"after":codec::loss_after(&base["global_streak"],"2026-10-05").unwrap(),"reward":{"coins":"0.000000","experience":"0.000000"}});
        (genesis, e, parent)
    }
    #[test]
    fn global_loss_replay_health_and_inventory_follow_the_same_rule() {
        let (g, e, parent) = loss_fixture();
        for (health, potions, expected, left) in [
            ("100.000000", 0, "85.000000", 0),
            ("10.000000", 2, "100.000000", 1),
            ("10.000000", 0, "0.000000", 0),
        ] {
            let mut base = g["action"]["base"].clone();
            base["health"] = json!(health);
            base["inventory"] = if potions > 0 {
                json!([{"category":"Зелья","item_id":"Зелье воскрешения","count":potions}])
            } else {
                json!([])
            };
            let next = transition(&e, &BTreeMap::from([(parent.clone(), base)])).unwrap();
            assert_eq!(next["health"], expected);
            let actual = next["inventory"]
                .as_array()
                .unwrap()
                .iter()
                .find(|v| v["item_id"] == "Зелье воскрешения")
                .map(|v| v["count"].as_u64().unwrap())
                .unwrap_or(0);
            assert_eq!(actual, left);
        }
    }
    #[test]
    fn standalone_loss_is_causal_versioned_and_replay_safe() {
        let (g, e, parent) = loss_fixture();
        let base = g["action"]["base"].clone();
        let states = BTreeMap::from([(parent.clone(), base.clone())]);
        let next = transition(&e, &states).unwrap();
        assert_eq!(next["global_streak"]["lost_length"], 3);
        assert_eq!(next["global_streak"]["history"], json!([]));
        assert_eq!(
            codec::unframe(&codec::frame(&e).unwrap(), false).unwrap(),
            e
        );
        assert!(transition(&e, &BTreeMap::new()).is_err());
        assert!(transition(&e, &BTreeMap::from([(parent, next)])).is_err());
        for field in ["before", "after", "reward"] {
            let mut bad = e.clone();
            bad["action"][field] = json!({});
            assert!(transition(&bad, &states).is_err());
        }
        let mut old = e.clone();
        old["version"] = json!(1);
        assert!(codec::validate(&old).is_err());
        let mut early = e.clone();
        early["action"]["writing_day"] = json!("2026-10-04");
        assert!(transition(&early, &states).is_err());
        let mut wrong = e.clone();
        wrong["header"]["event_id"] = g["header"]["device_id"].clone();
        wrong["header"]["entity_id"] = json!(format!(
            "game:{}",
            wrong["header"]["event_id"].as_str().unwrap()
        ));
        assert!(transition(&wrong, &states).is_err());
        let (db, path) = setup();
        preserve(&db, ACCOUNT, &g, "unsealed").unwrap();
        assert!(preserve(&db, ACCOUNT, &e, "unsealed").unwrap());
        assert!(!preserve(&db, ACCOUNT, &e, "unsealed").unwrap());
        assert_eq!(
            rebuild_chain(
                &db,
                ACCOUNT,
                "account",
                e["header"]["event_id"].as_str().unwrap()
            )
            .unwrap(),
            transition(&e, &states).unwrap()
        );
        assert!(
            dependencies(&db, ACCOUNT, g["header"]["device_id"].as_str().unwrap(), &e).is_err()
        );
        drop(db);
        let _ = std::fs::remove_file(path);
    }
    fn setup() -> (Connection, std::path::PathBuf) {
        let path =
            std::env::temp_dir().join(format!("c18506-{}.db", metadata::new_event_id().unwrap()));
        let db = crate::sqlite::open_database(&path).unwrap();
        db.execute("INSERT INTO cloud_sync_state(account_id,device_id,created_at,updated_at) VALUES(?1,?2,'now','now')",params![ACCOUNT,vectors()[0]["header"]["device_id"].as_str().unwrap()]).unwrap();
        (db, path)
    }
    fn pair() -> (Value, Value) {
        let v = vectors();
        let mut p = v
            .iter()
            .find(|v| v["action"]["kind"] == "writing")
            .unwrap()
            .clone();
        p["action"]["reward"] = writing_reward(&p["action"], "native-game-v1").unwrap();
        let h = &p["header"];
        let a = &p["action"];
        let id = stable_action_id(
            h["account_id"].as_str().unwrap(),
            "writing",
            &json!([
                h["project_id"],
                h["stage_id"],
                a["progress_event_id"],
                a["fact"]["entry_id"]
            ]),
        );
        p["header"]["event_id"] = json!(id);
        p["header"]["entity_id"] = json!(format!("game:project:{id}"));
        let mut r = v
            .iter()
            .find(|v| v["action"]["kind"] == "reward")
            .unwrap()
            .clone();
        let rid = stable_action_id(
            p["header"]["account_id"].as_str().unwrap(),
            "reward",
            &json!(id),
        );
        r["header"]["event_id"] = json!(rid);
        r["header"]["entity_id"] = json!(format!("game:{rid}"));
        r["header"]["rule"] = p["header"]["rule"].clone();
        r["action"]["project_action_id"] = json!(id);
        r["action"]["reward_id"] = json!(format!("reward:{id}"));
        r["action"]["project_id"] = p["header"]["project_id"].clone();
        r["action"]["reward"] = p["action"]["reward"].clone();
        (p, r)
    }
    #[test]
    fn game_pair_restart_retry_and_mismatch_are_durable() {
        let (mut db, path) = setup();
        let (p, r) = pair();
        let tx = db.transaction().unwrap();
        assert!(queue_reward_pair(&tx, ACCOUNT, "local-progress:E", &p, &r).unwrap());
        tx.commit().unwrap();
        let pf = codec::frame(&p).unwrap();
        let rf = codec::frame(&r).unwrap();
        for (e, f) in [(&p, &pf), (&r, &rf)] {
            let id = e["header"]["event_id"].as_str().unwrap();
            seal(&db, ACCOUNT, id, f, &[4; 24], &[8; 32]).unwrap();
            seal(&db, ACCOUNT, id, f, &[4; 24], &[8; 32]).unwrap();
            assert_eq!(
                seal(&db, ACCOUNT, id, f, &[5; 24], &[8; 32]).unwrap_err(),
                "game_exact_replay_mismatch"
            );
        }
        receipt(&db, ACCOUNT, r["header"]["event_id"].as_str().unwrap(), 42).unwrap();
        drop(db);
        let mut db = crate::sqlite::open_database(&path).unwrap();
        let tx = db.transaction().unwrap();
        assert!(!queue_reward_pair(&tx, ACCOUNT, "local-progress:E", &p, &r).unwrap());
        tx.commit().unwrap();
        assert_eq!(
            db.query_row("SELECT count(*) FROM cloud_game_sources", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM cloud_game_rewards", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM cloud_game_events WHERE state='sealed'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            2
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM cloud_game_apply_ledger", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        let mut bad = r.clone();
        bad["action"]["reward"]["coins"] = json!("999.000000");
        assert_eq!(
            queue_reward_pair(&db, ACCOUNT, "local-progress:E", &p, &bad).unwrap_err(),
            "game_exact_replay_mismatch"
        );
        assert_eq!(
            db.query_row(
                "SELECT canonical_frame FROM cloud_game_events WHERE event_id=?1",
                [r["header"]["event_id"].as_str().unwrap()],
                |r| r.get::<_, Vec<u8>>(0)
            )
            .unwrap(),
            rf
        );
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn game_pair_failure_rolls_back_both_intents_even_when_caller_continues() {
        let (db, path) = setup();
        let (p, mut r) = pair();
        r["action"]["reward"]["coins"] = json!("1.000000");
        assert_eq!(
            queue_reward_pair(&db, ACCOUNT, "local-progress:E", &p, &r).unwrap_err(),
            "game_reward_duplicate_mismatch"
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM cloud_game_events", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM cloud_game_sources", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn game_source_ids_are_scope_separated_and_rename_independent() {
        let a = "123e4567-e89b-42d3-a456-426614174000";
        let p = json!(["P", null, "E", "entry"]);
        let s = json!(["P", "S", "E", "entry"]);
        let x = stable_action_id(a, "writing", &p);
        assert!(metadata::uuid(&x));
        assert_eq!(x, stable_action_id(a, "writing", &p));
        assert_ne!(x, stable_action_id(a, "writing", &s));
        assert_ne!(x, stable_action_id("other-account", "writing", &p));
        assert_ne!(x, stable_action_id(a, "reward", &p));
        assert_ne!(
            stable_action_id(a, "writing", &json!(["ab", "c"])),
            stable_action_id(a, "writing", &json!(["a", "bc"]))
        );
    }
    #[test]
    fn game_native_rule_projection_matches_existing_reward_and_level_effects() {
        let (mut p, mut r) = pair();
        let v = vectors();
        let mut base = v
            .iter()
            .find(|v| v["header"]["scope"] == "account" && v["action"]["kind"] == "genesis")
            .unwrap()["action"]["base"]
            .clone();
        base["completion_claims"] = json!([]);
        base["experience"] = json!("7900.000000");
        base["health"] = json!("25.000000");
        base["writing_bonus"] = json!("0.250000");
        base["inspiration"] = json!("50.000000");
        base["productive_actions"] = json!(2);
        base["creative_event_pending"] = json!("absent");
        p["action"]["inspiration"] = base["inspiration"].clone();
        p["action"]["writing_bonus"] = base["writing_bonus"].clone();
        p["action"]["reward"] = writing_reward(&p["action"], "native-game-v1").unwrap();
        r["action"]["reward"] = p["action"]["reward"].clone();
        let parents = BTreeMap::from([(
            r["header"]["parents"][0].as_str().unwrap().to_string(),
            base.clone(),
        )]);
        let projected = transition_with_source(&r, &parents, Some(&p)).unwrap();
        let mut old = json!({"gamer":{"coins":float(&base["coins"]).unwrap(),"exp":float(&base["experience"]).unwrap(),"level":1,"available_skill_points":0,"health":25,"max_health":100,"inspiration":50,"writing_reward_bonus":0.25,"productive_actions_since_event":2,"cf":{"coins":1,"exp":1}}});
        crate::game::apply_ledger_rule_fixture(
            &mut old,
            "ProgressAdded",
            100.0,
            &serde_json::Map::new(),
        )
        .unwrap();
        for (canonical, legacy) in [
            ("coins", "coins"),
            ("experience", "exp"),
            ("health", "health"),
            ("max_health", "max_health"),
            ("writing_bonus", "writing_reward_bonus"),
        ] {
            assert_eq!(
                projected[canonical],
                decimal(old["gamer"][legacy].as_f64().unwrap()).unwrap(),
                "{canonical}"
            );
        }
        for (canonical, legacy) in [
            ("level", "level"),
            ("available_skill_points", "available_skill_points"),
            ("productive_actions", "productive_actions_since_event"),
        ] {
            assert_eq!(projected[canonical], old["gamer"][legacy]);
        }
        assert_eq!(
            projected["creative_event_pending"],
            old["gamer"]["pending_creative_event"]
        );
        let mut forged = p.clone();
        forged["action"]["reward"]["coins"] = json!("999.000000");
        let mut forged_r = r.clone();
        forged_r["action"]["reward"] = forged["action"]["reward"].clone();
        assert_eq!(
            transition_with_source(&forged_r, &parents, Some(&forged)).unwrap_err(),
            "game_invalid_rule"
        );
    }
    #[test]
    fn game_causal_rebuild_ignores_corrupt_snapshot_and_requires_owner_revision() {
        let (db, path) = setup();
        let v = vectors();
        let genesis = v
            .iter()
            .find(|v| v["header"]["scope"] == "account" && v["action"]["kind"] == "genesis")
            .unwrap()
            .clone();
        preserve(&db, ACCOUNT, &genesis, "waiting").unwrap();
        let id = genesis["header"]["event_id"].as_str().unwrap();
        db.execute(
            "INSERT INTO cloud_game_snapshots VALUES(?1,?2,'{\"coins\":\"999.000000\"}')",
            params![ACCOUNT, id],
        )
        .unwrap();
        assert_eq!(
            rebuild_chain(&db, ACCOUNT, "account", id).unwrap(),
            genesis["action"]["base"]
        );
        assert_eq!(
            rebuild_chain(&db, ACCOUNT, "[\"foreign\",null]", id).unwrap_err(),
            "game_scope_mismatch"
        );
        let mut child = v
            .iter()
            .find(|v| v["action"]["kind"] == "inventory" && v["action"]["operation"] == "buy")
            .unwrap()
            .clone();
        child["header"]["parents"] = json!([id]);
        child["header"]["revision"] = json!(7);
        preserve(&db, ACCOUNT, &child, "waiting").unwrap();
        assert_eq!(
            rebuild_chain(
                &db,
                ACCOUNT,
                "account",
                child["header"]["event_id"].as_str().unwrap()
            )
            .unwrap_err(),
            "game_parent_unknown"
        );
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn game_domain_processing_failure_rolls_back_reward_before_retry_and_restart() {
        let (mut db, path) = setup();
        db.execute("INSERT INTO game_state VALUES(1,2,'{\"gamer\":{\"coins\":0,\"exp\":0,\"level\":1}}','now')",[]).unwrap();
        db.execute("INSERT INTO domain_events(event_id,event_type,project_id,delta_symbols,context_json,created_at) VALUES('local-action','ProgressAdded','p',100,'{}','now')",[]).unwrap();
        db.execute_batch("CREATE TRIGGER fail_game_marker BEFORE UPDATE OF processed_at ON domain_events WHEN NEW.processed_at IS NOT NULL BEGIN SELECT RAISE(ABORT,'injected marker failure'); END;").unwrap();
        let result = crate::game::process_pending_events(&mut db, 10).unwrap();
        assert_eq!(result.failed, 1);
        let payload: String = db
            .query_row("SELECT payload_json FROM game_state", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&payload).unwrap()["gamer"]["coins"],
            0
        );
        db.execute_batch("DROP TRIGGER fail_game_marker").unwrap();
        drop(db);
        let mut db = crate::sqlite::open_database(&path).unwrap();
        assert_eq!(
            crate::game::process_pending_events(&mut db, 10)
                .unwrap()
                .processed,
            1
        );
        assert_eq!(
            crate::game::process_pending_events(&mut db, 10)
                .unwrap()
                .processed,
            0
        );
        let payload: String = db
            .query_row("SELECT payload_json FROM game_state", [], |r| r.get(0))
            .unwrap();
        let gamer = &serde_json::from_str::<Value>(&payload).unwrap()["gamer"];
        assert_eq!(gamer["coins"], 10.0);
        assert_eq!(gamer["exp"], 500.0);
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    fn set_tips(db: &Connection, key: &str, ids: &[String]) {
        db.execute(
            "DELETE FROM cloud_game_tips WHERE account_id=?1 AND owner_key=?2",
            params![ACCOUNT, key],
        )
        .unwrap();
        for id in ids {
            db.execute(
                "INSERT INTO cloud_game_tips VALUES(?1,?2,?3)",
                params![ACCOUNT, key, id],
            )
            .unwrap();
        }
    }
    #[test]
    fn game_concurrent_spends_preserve_tips_and_resolution_uses_full_tip_cas() {
        let (db, path) = setup();
        let v = vectors();
        let g = v
            .iter()
            .find(|v| v["header"]["scope"] == "account" && v["action"]["kind"] == "genesis")
            .unwrap();
        preserve(&db, ACCOUNT, g, "waiting").unwrap();
        let gid = g["header"]["event_id"].as_str().unwrap();
        set_tips(&db, "account", &[gid.to_string()]);
        db.execute(
            "INSERT INTO cloud_game_projection VALUES(?1,'account',?2,?3,1)",
            params![ACCOUNT, gid, canonical(&g["action"]["base"])],
        )
        .unwrap();
        let mut a = v
            .iter()
            .find(|v| v["action"]["kind"] == "inventory" && v["action"]["operation"] == "buy")
            .unwrap()
            .clone();
        a["header"]["parents"] = json!([gid]);
        a["header"]["revision"] = json!(2);
        a["action"] = json!({"kind":"inventory","operation":"buy","category":"Предметы","item_id":"Заморозка","count":1,"unit_price":"20.000000","before_count":2,"after_count":3,"coins_delta":"-20.000000"});
        let mut b = a.clone();
        let bid = metadata::new_event_id().unwrap();
        b["header"]["event_id"] = json!(bid);
        b["header"]["entity_id"] = json!(format!("game:{bid}"));
        b["action"]["unit_price"] = json!("30.000000");
        b["action"]["coins_delta"] = json!("-30.000000");
        preserve(&db, ACCOUNT, &a, "waiting").unwrap();
        let atips = next_tips(&db, ACCOUNT, &a).unwrap();
        set_tips(&db, "account", &atips);
        preserve(&db, ACCOUNT, &b, "waiting").unwrap();
        let both = next_tips(&db, ACCOUNT, &b).unwrap();
        assert_eq!(both.len(), 2);
        set_tips(&db, "account", &both);
        assert_eq!(
            rebuild_chain(
                &db,
                ACCOUNT,
                "account",
                a["header"]["event_id"].as_str().unwrap()
            )
            .unwrap()["coins"],
            "230.000000"
        );
        assert_eq!(
            rebuild_chain(&db, ACCOUNT, "account", &bid).unwrap()["coins"],
            "220.000000"
        );
        let mut r = a.clone();
        let rid = metadata::new_event_id().unwrap();
        r["header"]["event_id"] = json!(rid);
        r["header"]["entity_id"] = json!(format!("game:{rid}"));
        r["header"]["parents"] = json!(both);
        r["header"]["revision"] = json!(3);
        r["action"] = json!({"kind":"resolution","selected_event_id":a["header"]["event_id"]});
        assert_eq!(
            queue_resolution(&db, ACCOUNT, &r, &both, &json!({"coins":"wrong"})).unwrap_err(),
            "game_legacy_local_conflict"
        );
        queue_resolution(&db, ACCOUNT, &r, &both, &g["action"]["base"]).unwrap();
        assert_eq!(
            rebuild_chain(&db, ACCOUNT, "account", &rid).unwrap()["coins"],
            "230.000000"
        );
        assert_eq!(
            queue_resolution(&db, ACCOUNT, &r, &both, &g["action"]["base"]).unwrap_err(),
            "game_noncommutative_conflict"
        );
        let mut c = b.clone();
        let cid = metadata::new_event_id().unwrap();
        c["header"]["event_id"] = json!(cid);
        c["header"]["entity_id"] = json!(format!("game:{cid}"));
        preserve(&db, ACCOUNT, &c, "waiting").unwrap();
        let three = next_tips(&db, ACCOUNT, &c).unwrap();
        set_tips(&db, "account", &three);
        assert_eq!(next_tips(&db, ACCOUNT, &r).unwrap().len(), 2);
        assert_eq!(
            queue_resolution(&db, ACCOUNT, &r, &both, &g["action"]["base"]).unwrap_err(),
            "game_noncommutative_conflict"
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM cloud_game_events", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            5
        );
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn game_earn_spend_and_compensation_spend_keep_both_causal_branches() {
        for compensation in [false, true] {
            let (db, path) = setup();
            let v = vectors();
            let g = v
                .iter()
                .find(|e| e["header"]["scope"] == "account" && e["action"]["kind"] == "genesis")
                .unwrap();
            preserve(&db, ACCOUNT, g, "waiting").unwrap();
            preserve(&db, ACCOUNT, &v[0], "waiting").unwrap();
            let (mut p, mut r) = pair();
            p["header"]["parents"] = json!([v[0]["header"]["event_id"]]);
            r["header"]["parents"] = json!([g["header"]["event_id"]]);
            queue_reward_pair(&db, ACCOUNT, "source-E", &p, &r).unwrap();
            let parent = if compensation {
                r["header"]["event_id"].as_str().unwrap()
            } else {
                g["header"]["event_id"].as_str().unwrap()
            };
            let revision = if compensation { 3 } else { 2 };
            let base = rebuild_chain(&db, ACCOUNT, "account", parent).unwrap();
            db.execute(
                "INSERT INTO cloud_game_projection VALUES(?1,'account',?2,?3,?4)",
                params![ACCOUNT, parent, canonical(&base), revision - 1],
            )
            .unwrap();
            set_tips(&db, "account", &[parent.into()]);
            let mut left = r.clone();
            if compensation {
                let id = stable_action_id(
                    r["header"]["account_id"].as_str().unwrap(),
                    "compensation",
                    &r["header"]["event_id"],
                );
                left["header"]["event_id"] = json!(id);
                left["header"]["entity_id"] = json!(format!("game:{id}"));
                left["header"]["revision"] = json!(revision);
                left["header"]["parents"] = json!([parent]);
                left["action"] = json!({"kind":"compensation","target_action_id":parent,"reward":{"coins":"-10.000000","experience":"-500.000000"}});
                queue_compensation(&db, ACCOUNT, &left).unwrap();
            }
            let left_tips = next_tips(&db, ACCOUNT, &left).unwrap();
            set_tips(&db, "account", &left_tips);
            let mut spend = v
                .iter()
                .find(|e| e["action"]["kind"] == "inventory" && e["action"]["operation"] == "buy")
                .unwrap()
                .clone();
            spend["header"]["parents"] = json!([parent]);
            spend["header"]["revision"] = json!(revision);
            spend["action"] = json!({"kind":"inventory","operation":"buy","category":"Предметы","item_id":"Заморозка","count":1,"unit_price":"20.000000","before_count":2,"after_count":3,"coins_delta":"-20.000000"});
            preserve(&db, ACCOUNT, &spend, "waiting").unwrap();
            let both = next_tips(&db, ACCOUNT, &spend).unwrap();
            assert_eq!(both.len(), 2);
            assert_ne!(
                rebuild_chain(
                    &db,
                    ACCOUNT,
                    "account",
                    left["header"]["event_id"].as_str().unwrap()
                )
                .unwrap(),
                rebuild_chain(
                    &db,
                    ACCOUNT,
                    "account",
                    spend["header"]["event_id"].as_str().unwrap()
                )
                .unwrap()
            );
            assert_eq!(
                db.query_row("SELECT snapshot_json FROM cloud_game_projection", [], |r| r
                    .get::<_, String>(0))
                    .unwrap(),
                canonical(&base)
            );
            assert_eq!(
                db.query_row("SELECT count(*) FROM cloud_game_rewards", [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                1
            );
            drop(db);
            std::fs::remove_file(path).unwrap();
        }
    }
    #[test]
    fn game_compensation_is_immutable_once_and_never_deletes_reward() {
        let (db, path) = setup();
        let v = vectors();
        let g = v
            .iter()
            .find(|v| v["header"]["scope"] == "account" && v["action"]["kind"] == "genesis")
            .unwrap();
        preserve(&db, ACCOUNT, g, "waiting").unwrap();
        let (mut p, mut r) = pair();
        p["header"]["parents"] = json!([v[0]["header"]["event_id"]]);
        preserve(&db, ACCOUNT, &v[0], "waiting").unwrap();
        r["header"]["parents"] = json!([g["header"]["event_id"]]);
        queue_reward_pair(&db, ACCOUNT, "source-E", &p, &r).unwrap();
        let mut c = r.clone();
        let target = r["header"]["event_id"].as_str().unwrap();
        let id = stable_action_id(
            r["header"]["account_id"].as_str().unwrap(),
            "compensation",
            &json!(target),
        );
        c["header"]["event_id"] = json!(id);
        c["header"]["entity_id"] = json!(format!("game:{id}"));
        c["header"]["parents"] = json!([target]);
        c["header"]["revision"] = json!(3);
        c["action"] = json!({"kind":"compensation","target_action_id":target,"reward":{"coins":"-10.000000","experience":"-500.000000"}});
        assert!(queue_compensation(&db, ACCOUNT, &c).unwrap());
        assert!(!queue_compensation(&db, ACCOUNT, &c).unwrap());
        let projected = rebuild_chain(&db, ACCOUNT, "account", &id).unwrap();
        assert_eq!(projected["coins"], g["action"]["base"]["coins"]);
        assert_eq!(projected["experience"], g["action"]["base"]["experience"]);
        assert_eq!(
            db.query_row("SELECT count(*) FROM cloud_game_compensations", [], |r| r
                .get::<_, i64>(
                0
            ))
            .unwrap(),
            1
        );
        assert_eq!(event(&db, ACCOUNT, target).unwrap(), r);
        let mut bad = c.clone();
        bad["action"]["reward"]["coins"] = json!("-9.000000");
        assert_eq!(
            queue_compensation(&db, ACCOUNT, &bad).unwrap_err(),
            "game_invalid_rule"
        );
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn game_developer_restriction_retains_first_local_provenance_without_cloud_event() {
        let (db, path) = setup();
        record_developer_override(&db, &json!({"coins":123,"test_clock":true}), "now").unwrap();
        record_developer_override(&db, &json!({"coins":456,"test_clock":false}), "later").unwrap();
        assert_eq!(
            db.query_row(
                "SELECT source_json FROM cloud_game_local_restrictions",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            canonical(&json!({"coins":123,"test_clock":true}))
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM cloud_game_events", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    fn bind(db: &Connection) -> metadata::MetadataScope {
        let v = vectors();
        let h = &v[0]["header"];
        db.execute(
            "INSERT INTO cloud_account_bindings VALUES(?1,?2,'now','now')",
            params![ACCOUNT, h["account_id"].as_str()],
        )
        .unwrap();
        metadata::MetadataScope {
            account_id: ACCOUNT.into(),
            canonical_user_id: h["account_id"].as_str().unwrap().into(),
            device_id: h["device_id"].as_str().unwrap().into(),
        }
    }
    fn receive_account(db: &Connection, e: &Value, sequence: i64, n: &[u8], c: &[u8]) {
        let h = &e["header"];
        db.execute("INSERT INTO cloud_sync_account_inbox(account_id,event_id,canonical_user_id,scope,server_sequence,device_id,entity_id,entity_type,operation,sync_revision,updated_at,crypto_version,aad_version,nonce,ciphertext,received_at) VALUES(?1,?2,?3,'account',?4,?5,?6,'account_game','upsert',?7,?8,2,2,?9,?10,'now')",params![ACCOUNT,h["event_id"].as_str(),h["account_id"].as_str(),sequence,h["device_id"].as_str(),h["entity_id"].as_str(),h["revision"].as_i64(),h["updated_at"].as_str(),n,c]).unwrap();
    }
    #[test]
    fn game_account_atomic_genesis_import_restart_replay_preserves_local_notifications() {
        let (db, path) = setup();
        let scope = bind(&db);
        let source = json!({"gamer":serde_json::from_str::<Value>(include_str!("../../src/cloud/__fixtures__/gameLegacyDefaultsV1.json")).unwrap(),"notifications":{"read":["local-only"]}});
        db.execute(
            "INSERT INTO game_state VALUES(1,2,?1,'now')",
            [canonical(&source)],
        )
        .unwrap();
        let mut e = vectors()
            .into_iter()
            .find(|e| e["header"]["scope"] == "account" && e["action"]["kind"] == "genesis")
            .unwrap();
        let mut legacy = source.clone();
        legacy["gamer"]["coins"] = json!(765.4);
        e["action"]["base"] = crate::game_projection::account_base(&legacy).unwrap();
        let frame = codec::frame(&e).unwrap();
        let n = vec![7; 24];
        let c = vec![8; 32];
        receive_account(&db, &e, 1, &n, &c);
        assert!(crate::account_sync::received(&db, &scope, 8, 0)
            .unwrap()
            .is_empty());
        drop(db);
        let mut privileged = crate::sqlite::open_privileged_remote_apply_database(&path).unwrap();
        assert_eq!(
            apply(&mut privileged, &scope, &frame, &n, &c, false).unwrap(),
            "applied"
        );
        assert_eq!(
            apply(&mut privileged, &scope, &frame, &n, &c, false).unwrap(),
            "applied"
        );
        let projected: String = privileged
            .connection()
            .query_row("SELECT payload_json FROM game_state", [], |r| r.get(0))
            .unwrap();
        let projected: Value = serde_json::from_str(&projected).unwrap();
        assert_eq!(projected["gamer"]["coins"], 765.4);
        assert_eq!(projected["notifications"], source["notifications"]);
        assert_eq!(
            privileged
                .connection()
                .query_row("SELECT count(*) FROM cloud_game_local_mutations", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        drop(privileged);
        let mut privileged = crate::sqlite::open_privileged_remote_apply_database(&path).unwrap();
        assert_eq!(
            apply(&mut privileged, &scope, &frame, &n, &c, false).unwrap(),
            "applied"
        );
        assert_eq!(
            privileged
                .connection()
                .query_row("SELECT count(*) FROM cloud_game_apply_ledger", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        let mut damaged = projected.clone();
        damaged["gamer"]["coins"] = json!(123.0);
        privileged
            .connection()
            .execute(
                "UPDATE game_state SET payload_json=?1",
                [canonical(&damaged)],
            )
            .unwrap();
        privileged
            .connection()
            .execute(
                "UPDATE cloud_game_projection SET snapshot_json=?1",
                [canonical(&json!({"coins":"123.000000"}))],
            )
            .unwrap();
        rebuild(&mut privileged, &scope, "account").unwrap();
        let restored: String = privileged
            .connection()
            .query_row("SELECT payload_json FROM game_state", [], |r| r.get(0))
            .unwrap();
        assert_eq!(serde_json::from_str::<Value>(&restored).unwrap(), projected);
        assert_eq!(
            privileged
                .connection()
                .query_row("SELECT canonical_frame FROM cloud_game_events", [], |r| r
                    .get::<_, Vec<
                    u8,
                >>(
                    0
                ))
                .unwrap(),
            frame
        );
        assert_eq!(
            privileged
                .connection()
                .query_row("SELECT count(*) FROM cloud_game_apply_ledger", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            privileged
                .connection()
                .query_row("SELECT count(*) FROM cloud_game_local_mutations", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        let mut forged = e;
        forged["action"]["base"]["coins"] = json!("999.000000");
        assert_eq!(
            apply(
                &mut privileged,
                &scope,
                &codec::frame(&forged).unwrap(),
                &n,
                &c,
                false
            )
            .unwrap(),
            "waiting"
        );
        assert_eq!(
            privileged
                .connection()
                .query_row(
                    "SELECT code FROM cloud_game_blockers WHERE event_id=?1",
                    [forged["header"]["event_id"].as_str().unwrap()],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
            "game_exact_replay_mismatch"
        );
        drop(privileged);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn game_account_reward_before_source_retains_envelope_without_projection_or_receipt() {
        let (db, path) = setup();
        let scope = bind(&db);
        let (_, r) = pair();
        let frame = codec::frame(&r).unwrap();
        let n = vec![9; 24];
        let c = vec![10; 32];
        receive_account(&db, &r, 2, &n, &c);
        drop(db);
        let mut privileged = crate::sqlite::open_privileged_remote_apply_database(&path).unwrap();
        for _ in 0..2 {
            assert_eq!(
                apply(&mut privileged, &scope, &frame, &n, &c, false).unwrap(),
                "waiting"
            );
        }
        let db = privileged.connection();
        assert_eq!(
            db.query_row("SELECT count(*) FROM game_state", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM cloud_game_rewards", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM cloud_game_apply_ledger", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            db.query_row("SELECT canonical_frame FROM cloud_game_events", [], |r| r
                .get::<_, Vec<
                u8,
            >>(
                0
            ))
            .unwrap(),
            frame
        );
        assert_eq!(
            db.query_row("SELECT code FROM cloud_game_blockers", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "game_dependency_project_action_missing"
        );
        drop(privileged);
        let db = crate::sqlite::open_database(&path).unwrap();
        assert_eq!(
            db.query_row("SELECT nonce FROM cloud_game_events", [], |r| r
                .get::<_, Vec<u8>>(0))
                .unwrap(),
            n
        );
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn game_account_apply_failure_rolls_back_projection_and_receipt_together() {
        let (db, path) = setup();
        let scope = bind(&db);
        let e = vectors()
            .into_iter()
            .find(|e| e["header"]["scope"] == "account" && e["action"]["kind"] == "genesis")
            .unwrap();
        let frame = codec::frame(&e).unwrap();
        let n = vec![7; 24];
        let c = vec![8; 32];
        receive_account(&db, &e, 1, &n, &c);
        db.execute_batch("CREATE TRIGGER fail_game_apply BEFORE INSERT ON cloud_game_apply_ledger BEGIN SELECT RAISE(ABORT,'injected receipt failure'); END;").unwrap();
        drop(db);
        let mut privileged = crate::sqlite::open_privileged_remote_apply_database(&path).unwrap();
        assert_eq!(
            apply(&mut privileged, &scope, &frame, &n, &c, false).unwrap_err(),
            "game_storage_error"
        );
        for table in [
            "game_state",
            "cloud_game_events",
            "cloud_game_tips",
            "cloud_game_projection",
            "cloud_game_apply_ledger",
            "cloud_sync_remote_apply_authorizations",
        ] {
            assert_eq!(
                privileged
                    .connection()
                    .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r
                        .get::<_, i64>(0))
                    .unwrap(),
                0,
                "{table}"
            );
        }
        privileged
            .connection()
            .execute_batch("DROP TRIGGER fail_game_apply")
            .unwrap();
        assert_eq!(
            apply(&mut privileged, &scope, &frame, &n, &c, false).unwrap(),
            "applied"
        );
        drop(privileged);
        std::fs::remove_file(path).unwrap();
    }
}
