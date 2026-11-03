//! Draft C18.5.06 codec boundary. No writer/reader capability is advertised here.
use crate::{document_codec::canonical, progress_codec, project_metadata_sync as metadata};
use serde_json::{json, Value};
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;
pub const PROJECT_CODEC: u8 = 12;
pub const ACCOUNT_CODEC: u8 = 13;
type Result<T> = std::result::Result<T, String>;
fn bad<T>() -> Result<T> {
    Err("invalid_game_payload".into())
}
fn exact(v: &Value, keys: &[&str]) -> Result<()> {
    let o = v.as_object().ok_or("invalid_game_payload")?;
    if o.len() != keys.len() || keys.iter().any(|k| !o.contains_key(*k)) {
        return bad();
    }
    Ok(())
}
fn id(v: &Value) -> bool {
    v.as_str()
        .is_some_and(|s| !s.is_empty() && s.len() <= 512 && !s.contains('\0'))
}
fn uuid(v: &Value) -> bool {
    v.as_str().is_some_and(metadata::uuid)
}
fn integer(v: &Value, max: u64) -> bool {
    v.as_u64().is_some_and(|n| n <= max)
}
fn time(v: &Value) -> bool {
    v.as_str().is_some_and(metadata::timestamp)
}
fn day(v: &Value) -> bool {
    v.as_str()
        .is_some_and(|s| s.len() == 10 && metadata::timestamp(&format!("{s}T00:00:00.000000Z")))
}
fn amount(v: &Value, signed: bool) -> Result<i128> {
    let n = progress_codec::micros(v.as_str().ok_or("invalid_game_payload")?)
        .map_err(|_| "invalid_game_payload")?;
    if !signed && n < 0 {
        return bad();
    }
    Ok(n)
}
fn heads(v: &Value, nonempty: bool) -> Result<()> {
    let a = v.as_array().ok_or("invalid_game_payload")?;
    if a.len() > 64
        || nonempty && a.is_empty()
        || a.iter().any(|s| !uuid(s))
        || a.windows(2).any(|w| w[0].as_str() >= w[1].as_str())
    {
        return bad();
    }
    Ok(())
}
fn reward(v: &Value, signed: bool) -> Result<()> {
    exact(v, &["coins", "experience"])?;
    amount(&v["coins"], signed)?;
    amount(&v["experience"], signed)?;
    Ok(())
}
fn streak(v: &Value) -> Result<()> {
    exact(
        v,
        &[
            "history",
            "maximum",
            "freezes",
            "enabled",
            "last_reward_day",
            "lost_day",
            "lost_length",
        ],
    )?;
    let days = v["history"].as_array().ok_or("game_resource_limit")?;
    if days.len() > 4096 {
        return Err("game_resource_limit".into());
    }
    let mut previous = "";
    for d in days {
        exact(d, &["day", "frozen"])?;
        if !day(&d["day"]) || !d["frozen"].is_boolean() || d["day"].as_str().unwrap() <= previous {
            return bad();
        }
        previous = d["day"].as_str().unwrap();
    }
    if !integer(&v["maximum"], 1_000_000)
        || !integer(&v["freezes"], 1_000_000)
        || !integer(&v["lost_length"], 1_000_000)
        || !v["enabled"].is_boolean()
        || !v["last_reward_day"].is_null() && !day(&v["last_reward_day"])
        || !v["lost_day"].is_null() && !day(&v["lost_day"])
    {
        return bad();
    }
    Ok(())
}
fn base(v: &Value) -> Result<()> {
    exact(
        v,
        &[
            "coins",
            "experience",
            "level",
            "available_skill_points",
            "skill_points_awarded_for_level",
            "skills",
            "inspiration",
            "inventory",
            "completion_claims",
            "global_streak",
            "health",
            "max_health",
            "coin_coefficient",
            "experience_coefficient",
            "health_recovery_coefficient",
            "writing_bonus",
            "productive_actions",
            "creative_event_pending",
        ],
    )?;
    amount(&v["coins"], false)?;
    amount(&v["experience"], false)?;
    if !integer(&v["level"], 99)
        || v["level"] == 0
        || !integer(&v["available_skill_points"], 1_000_000)
        || !integer(&v["skill_points_awarded_for_level"], 99)
        || v["skill_points_awarded_for_level"] == 0
        || amount(&v["inspiration"], false)? > 100_000_000
    {
        return bad();
    }
    for key in [
        "health",
        "max_health",
        "coin_coefficient",
        "experience_coefficient",
        "health_recovery_coefficient",
        "writing_bonus",
    ] {
        amount(&v[key], false)?;
    }
    if amount(&v["health"], false)? > amount(&v["max_health"], false)?
        || !integer(&v["productive_actions"], 1_000_000)
        || !matches!(
            v["creative_event_pending"].as_str(),
            Some("absent" | "none" | "unexpected_idea")
        )
    {
        return bad();
    }
    exact(
        &v["skills"],
        &["productivity", "profitability", "endurance"],
    )?;
    if v["skills"]
        .as_object()
        .unwrap()
        .values()
        .any(|n| !integer(n, 1_000_000))
    {
        return bad();
    }
    let inventory = v["inventory"].as_array().ok_or("game_resource_limit")?;
    let claims = v["completion_claims"]
        .as_array()
        .ok_or("game_resource_limit")?;
    if inventory.len() > 512 || claims.len() > 4096 {
        return Err("game_resource_limit".into());
    }
    let mut previous = String::new();
    for i in inventory {
        exact(i, &["category", "item_id", "count"])?;
        if !id(&i["category"]) || !id(&i["item_id"]) || !integer(&i["count"], 10000) {
            return bad();
        }
        let key = format!(
            "{}\0{}",
            i["category"].as_str().unwrap(),
            i["item_id"].as_str().unwrap()
        );
        if key.encode_utf16().cmp(previous.encode_utf16()) != std::cmp::Ordering::Greater {
            return bad();
        }
        previous = key;
    }
    if claims.iter().any(|v| !id(v))
        || claims.windows(2).any(|w| {
            w[0].as_str()
                .unwrap()
                .encode_utf16()
                .cmp(w[1].as_str().unwrap().encode_utf16())
                != std::cmp::Ordering::Less
        })
    {
        return bad();
    }
    streak(&v["global_streak"])
}
/// A loss consumes the prior series. Its identity/day is the first day on which
/// the missed writing day is final, independent of when a device notices it.
pub fn loss_after(before: &Value, writing_day: &str) -> Result<Value> {
    streak(before)?;
    let history = before["history"].as_array().ok_or("invalid_game_payload")?;
    let last = history
        .last()
        .and_then(|v| v["day"].as_str())
        .and_then(crate::streaks::date_days)
        .ok_or("game_invalid_rule")?;
    let today = crate::streaks::date_days(writing_day).ok_or("invalid_game_payload")?;
    if before["enabled"] != true || last + 2 > today {
        return Err("game_invalid_rule".into());
    }
    if before["lost_day"]
        .as_str()
        .and_then(crate::streaks::date_days)
        .is_some_and(|d| d >= last)
    {
        return Err("game_invalid_rule".into());
    }
    let mut after = before.clone();
    after["history"] = json!([]);
    after["lost_day"] = json!(crate::streaks::date_from_days(last + 2));
    after["lost_length"] = json!(history.len());
    after["freezes"] = json!(0);
    after["maximum"] = json!(before["maximum"]
        .as_u64()
        .unwrap_or(0)
        .max(history.len() as u64));
    Ok(after)
}
pub fn validate(e: &Value) -> Result<()> {
    exact(e, &["version", "header", "action"])?;
    let h = &e["header"];
    let project = h["scope"] == "project";
    let mut keys = vec![
        "account_id",
        "device_id",
        "event_id",
        "entity_id",
        "parents",
        "revision",
        "updated_at",
        "rule",
        "scope",
    ];
    if project {
        keys.extend([
            "project_id",
            "stage_id",
            "bootstrap_id",
            "metadata_event_id",
            "stage_event_ids",
        ]);
    } else {
        keys.push("entity_type");
    }
    exact(h, &keys)?;
    if !(e["version"] == 1
        || e["version"] == 2
            && !project
            && e["action"]["kind"] == "global_streak"
            && h["rule"] == "native-game-v1")
        || !uuid(&h["account_id"])
        || !uuid(&h["device_id"])
        || !uuid(&h["event_id"])
        || !id(&h["entity_id"])
        || !time(&h["updated_at"])
        || !integer(&h["revision"], 9007199254740991)
        || h["revision"] == 0
        || !matches!(
            h["rule"].as_str(),
            Some("legacy-game-v1" | "native-game-v1" | "python-game-v1")
        )
    {
        return bad();
    }
    heads(&h["parents"], false)?;
    let parents = h["parents"].as_array().unwrap();
    if parents.contains(&h["event_id"]) {
        return bad();
    }
    if project {
        heads(&h["stage_event_ids"], false)?;
        let owner = if h["stage_id"].is_null() {
            "project".into()
        } else {
            format!("stage:{}", h["stage_id"].as_str().unwrap_or(""))
        };
        if !id(&h["project_id"])
            || !h["stage_id"].is_null() && !id(&h["stage_id"])
            || !uuid(&h["bootstrap_id"])
            || !uuid(&h["metadata_event_id"])
            || h["stage_id"].is_null() != h["stage_event_ids"].as_array().unwrap().is_empty()
            || h["entity_id"] != format!("game:{owner}:{}", h["event_id"].as_str().unwrap())
        {
            return bad();
        }
    } else if h["scope"] != "account"
        || h["entity_type"] != "account_game"
        || h["entity_id"] != format!("game:{}", h["event_id"].as_str().unwrap())
    {
        return bad();
    }
    let a = &e["action"];
    let kind = a["kind"].as_str().ok_or("invalid_game_payload")?;
    let genesis = kind == "genesis";
    let adoption = kind == "adopt_local";
    let resolution = kind == "resolution";
    if if genesis {
        !parents.is_empty() || h["revision"] != 1
    } else {
        parents.is_empty() || h["revision"].as_u64().unwrap() < 2
    } {
        return bad();
    }
    if (genesis || adoption) != (h["rule"] == "legacy-game-v1") {
        return bad();
    }
    if !genesis && !adoption && !resolution && parents.len() != 1 {
        return bad();
    }
    if resolution {
        exact(a, &["kind", "selected_event_id"])?;
        if !uuid(&a["selected_event_id"]) || !parents.contains(&a["selected_event_id"]) {
            return bad();
        }
        return Ok(());
    }
    if kind == "compensation" {
        exact(a, &["kind", "target_action_id", "reward"])?;
        if !uuid(&a["target_action_id"]) {
            return bad();
        }
        return reward(&a["reward"], true);
    }
    match (project, kind) {
        (true, "genesis" | "adopt_local") => {
            exact(a, &["kind", "base", "completion_claimed"])?;
            streak(&a["base"])?;
            if !a["completion_claimed"].is_boolean() {
                return bad();
            }
        }
        (false, "genesis" | "adopt_local") => {
            exact(a, &["kind", "base"])?;
            base(&a["base"])?;
        }
        (true, "writing") => {
            exact(
                a,
                &[
                    "kind",
                    "progress_event_id",
                    "progress_entity_id",
                    "fact",
                    "inspiration",
                    "writing_bonus",
                    "coin_coefficient",
                    "experience_coefficient",
                    "reward",
                ],
            )?;
            let owner = progress_codec::scope_id(h["stage_id"].as_str());
            if !uuid(&a["progress_event_id"]) || a["progress_entity_id"] != owner {
                return bad();
            }
            let f = &a["fact"];
            exact(
                f,
                &[
                    "entry_id",
                    "new_total",
                    "delta",
                    "unit",
                    "occurred_at",
                    "writing_time",
                    "writing_day",
                ],
            )?;
            if !id(&f["entry_id"])
                || !matches!(
                    f["unit"].as_str(),
                    Some("symbols" | "A4" | "author_list" | "ficbook_pages")
                )
                || !day(&f["writing_day"])
                || !time(&f["occurred_at"])
                || !f["writing_time"].is_null()
                || amount(&f["delta"], false)? <= 0
            {
                return bad();
            }
            amount(&f["new_total"], false)?;
            if amount(&a["inspiration"], false)? > 100_000_000 {
                return bad();
            }
            for k in [
                "writing_bonus",
                "coin_coefficient",
                "experience_coefficient",
            ] {
                amount(&a[k], false)?;
            }
            reward(&a["reward"], false)?;
        }
        (true, "completion") => {
            exact(
                a,
                &[
                    "kind",
                    "completion_id",
                    "progress_event_id",
                    "progress_entity_id",
                    "total_symbols",
                    "reward",
                ],
            )?;
            if !uuid(&a["progress_event_id"])
                || a["progress_entity_id"] != progress_codec::scope_id(h["stage_id"].as_str())
                || a["completion_id"]
                    != format!(
                        "completion:{}",
                        canonical(&json!([h["project_id"], h["stage_id"]]))
                    )
            {
                return bad();
            }
            amount(&a["total_symbols"], false)?;
            reward(&a["reward"], false)?;
        }
        (true, "streak") | (false, "global_streak") => {
            let refs = if project {
                "progress_event_ids"
            } else {
                "project_action_ids"
            };
            exact(
                a,
                &["kind", "writing_day", refs, "before", "after", "reward"],
            )?;
            if !day(&a["writing_day"]) {
                return bad();
            }
            heads(&a[refs], e["version"] != 2)?;
            if e["version"] == 2 {
                if a[refs] != json!([])
                    || a["reward"] != json!({"coins":"0.000000","experience":"0.000000"})
                {
                    return bad();
                }
                let expected = loss_after(
                    &a["before"],
                    a["writing_day"].as_str().ok_or("invalid_game_payload")?,
                )?;
                if a["after"] != expected {
                    return bad();
                }
            }
            streak(&a["before"])?;
            streak(&a["after"])?;
            reward(&a["reward"], false)?;
        }
        (true, "freeze") => {
            exact(
                a,
                &[
                    "kind",
                    "writing_day",
                    "account_action_id",
                    "before",
                    "after",
                ],
            )?;
            if !day(&a["writing_day"]) || !uuid(&a["account_action_id"]) {
                return bad();
            }
            streak(&a["before"])?;
            streak(&a["after"])?;
        }
        (false, "reward") => {
            exact(
                a,
                &[
                    "kind",
                    "reward_id",
                    "project_id",
                    "project_action_id",
                    "reward",
                ],
            )?;
            if !id(&a["project_id"])
                || !uuid(&a["project_action_id"])
                || a["reward_id"] != format!("reward:{}", a["project_action_id"].as_str().unwrap())
            {
                return bad();
            }
            reward(&a["reward"], false)?;
        }
        (false, "inventory") => {
            exact(
                a,
                &[
                    "kind",
                    "operation",
                    "category",
                    "item_id",
                    "count",
                    "unit_price",
                    "before_count",
                    "after_count",
                    "coins_delta",
                ],
            )?;
            if !matches!(a["operation"].as_str(), Some("buy" | "sell" | "use"))
                || !id(&a["category"])
                || !id(&a["item_id"])
                || !integer(&a["count"], 10000)
                || a["count"] == 0
                || !integer(&a["before_count"], 10000)
                || !integer(&a["after_count"], 10000)
            {
                return bad();
            }
            let delta = a["count"].as_i64().unwrap() * if a["operation"] == "buy" { 1 } else { -1 };
            if a["after_count"].as_i64().unwrap() != a["before_count"].as_i64().unwrap() + delta {
                return bad();
            }
            amount(&a["unit_price"], false)?;
            amount(&a["coins_delta"], true)?;
        }
        (false, "freeze") => {
            exact(
                a,
                &[
                    "kind",
                    "writing_day",
                    "project_id",
                    "project_action_id",
                    "before_count",
                    "after_count",
                    "before",
                    "after",
                ],
            )?;
            if !day(&a["writing_day"])
                || !integer(&a["before_count"], 10000)
                || a["before_count"] == 0
                || a["after_count"].as_u64() != Some(a["before_count"].as_u64().unwrap() - 1)
            {
                return bad();
            }
            if a["project_id"].is_null() {
                if !a["project_action_id"].is_null()
                    || a["before"].is_null()
                    || a["after"].is_null()
                {
                    return bad();
                }
                streak(&a["before"])?;
                streak(&a["after"])?;
            } else if !id(&a["project_id"])
                || !uuid(&a["project_action_id"])
                || !a["before"].is_null()
                || !a["after"].is_null()
            {
                return bad();
            }
        }
        _ => return Err("game_action_unsupported".into()),
    }
    Ok(())
}
pub fn frame(e: &Value) -> Result<Vec<u8>> {
    validate(e)?;
    let bytes = canonical(e).into_bytes();
    if bytes.len() + 20 > MAX_FRAME_BYTES {
        return Err("game_resource_limit".into());
    }
    let codec = if e["header"]["scope"] == "project" {
        PROJECT_CODEC
    } else {
        ACCOUNT_CODEC
    };
    let mut out = b"WORTA-C1".to_vec();
    out.extend([1, codec, 1, 0]);
    out.extend((bytes.len() as u32).to_be_bytes());
    out.extend((bytes.len() as u32).to_be_bytes());
    out.extend(bytes);
    Ok(out)
}
pub fn unframe(f: &[u8], project: bool) -> Result<Value> {
    let normalized;
    let f = if f.len() >= 20
        && f[11] != 0
        && [if project { 12 } else { 13 }].contains(&f[9])
        && [1].contains(&f[10])
    {
        normalized = crate::frame_compression::normalize_authenticated_frame(
            f,
            &[if project { 12 } else { 13 }],
            &[1],
            MAX_FRAME_BYTES - 20,
        )
        .map_err(str::to_string)?;
        normalized.as_slice()
    } else {
        f
    };

    if f.len() > MAX_FRAME_BYTES {
        return Err("game_resource_limit".into());
    }
    let codec = if project {
        PROJECT_CODEC
    } else {
        ACCOUNT_CODEC
    };
    if f.len() < 20 || &f[..8] != b"WORTA-C1" || f[8..12] != [1, codec, 1, 0] {
        return Err("game_codec_unsupported".into());
    }
    if u32::from_be_bytes(f[12..16].try_into().unwrap()) as usize != f.len() - 20
        || f[12..16] != f[16..20]
    {
        return bad();
    }
    let e: Value = serde_json::from_slice(&f[20..]).map_err(|_| "invalid_game_payload")?;
    validate(&e)?;
    if (e["header"]["scope"] == "project") != project || canonical(&e).as_bytes() != &f[20..] {
        return bad();
    }
    Ok(e)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn examples() -> Vec<Value> {
        let f: Value = serde_json::from_str(include_str!(
            "../../src/cloud/__fixtures__/gameCodecV1.json"
        ))
        .unwrap();
        f["examples"].as_array().unwrap().clone()
    }
    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }
    #[test]
    fn game_codec_cross_language_vectors() {
        for v in examples() {
            let e = &v["event"];
            let project = e["header"]["scope"] == "project";
            let f = frame(e).unwrap();
            assert_eq!(
                canonical(e),
                v["canonical_json"].as_str().unwrap(),
                "{}",
                v["name"]
            );
            assert_eq!(hex(&f), v["frame_hex"].as_str().unwrap(), "{}", v["name"]);
            assert_eq!(&unframe(&f, project).unwrap(), e);
            assert_eq!(unframe(&f, !project).unwrap_err(), "game_codec_unsupported");
            for id in 1..=11 {
                let mut bad = f.clone();
                bad[9] = id;
                assert!(unframe(&bad, project).is_err());
            }
            for path in [None, Some("header"), Some("action")] {
                let mut e = e.clone();
                let node = if let Some(p) = path {
                    &mut e[p]
                } else {
                    &mut e
                };
                node["extra"] = json!({"raw_snapshot": true});
                assert!(validate(&e).is_err());
            }
            let mut wrong = e.clone();
            wrong["header"]["entity_id"] = json!("foreign");
            assert!(validate(&wrong).is_err());
        }
    }
    #[test]
    fn game_codec_malformed_resource_negatives() {
        let mut e = examples()[0]["event"].clone();
        let f = frame(&e).unwrap();
        for offset in [8, 10, 11, 12, 16, 21] {
            let mut bad = f.clone();
            bad[offset] ^= 255;
            assert!(unframe(&bad, true).is_err());
        }
        assert_eq!(
            unframe(&vec![0; MAX_FRAME_BYTES + 1], true).unwrap_err(),
            "game_resource_limit"
        );
        e["action"]["base"]["history"] =
            json!(vec![json!({"day":"2026-10-01","frozen":false}); 4097]);
        assert_eq!(validate(&e).unwrap_err(), "game_resource_limit");
        let mut writing = examples()[2]["event"].clone();
        writing["action"]["fact"]["delta"] = json!("-0.000000");
        assert!(validate(&writing).is_err());
        writing["action"]["fact"]["delta"] = json!("1000000000001.000000");
        assert!(validate(&writing).is_err());
        let deep = format!("{}0{}", "[".repeat(129), "]".repeat(129));
        let body = deep.as_bytes();
        let mut f = b"WORTA-C1".to_vec();
        f.extend([1, 12, 1, 0]);
        f.extend((body.len() as u32).to_be_bytes());
        f.extend((body.len() as u32).to_be_bytes());
        f.extend(body);
        assert!(unframe(&f, true).is_err());
    }
    #[test]
    fn game_codec_inventory_claim_order_and_scope() {
        let mut e = examples()
            .into_iter()
            .find(|v| v["name"] == "account-genesis")
            .unwrap()["event"]
            .clone();
        // Ordering is UTF-16 in both runtimes, including non-BMP identities.
        e["action"]["base"]["completion_claims"] = json!(["😀", "\u{e000}"]);
        assert!(validate(&e).is_ok());
        e["action"]["base"]["completion_claims"] = json!(["\u{e000}", "😀"]);
        assert!(validate(&e).is_err());
        let mut e = examples()
            .into_iter()
            .find(|v| v["name"] == "stage-completion")
            .unwrap()["event"]
            .clone();
        e["header"]["stage_id"] = json!("project");
        e["action"]["progress_entity_id"] = json!("stage:project");
        e["header"]["entity_id"] = json!(format!(
            "game:stage:project:{}",
            e["header"]["event_id"].as_str().unwrap()
        ));
        e["action"]["completion_id"] = json!("completion:[\"p\",null]");
        assert!(validate(&e).is_err());
        e["action"]["completion_id"] = json!("completion:[\"p\",\"project\"]");
        assert!(validate(&e).is_ok());
    }
}
