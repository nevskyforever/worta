//! Codec11: immutable progress actions; totals and ordering are projections.
use crate::{document_codec, project_metadata_sync as metadata};
use serde::{Deserialize, Serialize};
use serde_json::Value;
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_CHAIN: usize = 65536;
pub const MAX_OPERATION: usize = 1024;
pub const MAX_AMOUNT: i128 = 1_000_000_000_000_000_000;
type Result<T> = std::result::Result<T, String>;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Fact {
    pub entry_id: String,
    pub new_total: String,
    pub delta: String,
    pub unit: String,
    pub occurred_at: Option<String>,
    pub writing_time: Option<String>,
    pub writing_day: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MigrationProof {
    pub entry_count: usize,
    pub final_total: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub version: i64,
    pub migration: Option<MigrationProof>,
    pub header: document_codec::Header,
    pub base_total: Option<String>,
    pub entries: Vec<Fact>,
    pub selected_event_id: Option<String>,
    pub target_entry_id: Option<String>,
    pub rebased_from: Vec<String>,
}
pub fn scope_id(stage: Option<&str>) -> String {
    stage
        .map(|s| format!("stage:{s}"))
        .unwrap_or_else(|| "project".into())
}
pub fn factor(unit: &str) -> Result<i128> {
    match unit {
        "symbols" => Ok(1),
        "A4" => Ok(1800),
        "author_list" => Ok(40000),
        "ficbook_pages" => Ok(4500),
        _ => Err("invalid_progress_payload".into()),
    }
}
fn id(s: &str) -> bool {
    !s.is_empty() && s.len() <= 512 && !s.contains('\0')
}
fn bad<T>() -> Result<T> {
    Err("invalid_progress_payload".into())
}
pub fn micros(s: &str) -> Result<i128> {
    let body = s.strip_prefix('-').unwrap_or(s);
    let Some((whole, frac)) = body.split_once('.') else {
        return bad();
    };
    if whole.is_empty()
        || whole.len() > 13
        || whole.len() > 1 && whole.starts_with('0')
        || frac.len() != 6
        || !whole
            .bytes()
            .chain(frac.bytes())
            .all(|b| b.is_ascii_digit())
        || s == "-0.000000"
    {
        return bad();
    }
    let n = format!(
        "{}{}{}",
        if s.starts_with('-') { "-" } else { "" },
        whole,
        frac
    )
    .parse::<i128>()
    .map_err(|_| "invalid_progress_payload")?;
    if n.abs() > MAX_AMOUNT {
        return Err("progress_resource_limit".into());
    }
    Ok(n)
}
pub fn decimal(n: i128) -> String {
    format!(
        "{}{}.{:06}",
        if n < 0 { "-" } else { "" },
        n.abs() / 1_000_000,
        n.abs() % 1_000_000
    )
}
pub fn symbols(f: &Fact) -> Result<i128> {
    factor(&f.unit)?;
    let n = micros(&f.new_total)?;
    if !(0..=MAX_AMOUNT).contains(&n) {
        return bad();
    }
    Ok(n)
}
pub fn delta_symbols(f: &Fact) -> Result<i128> {
    factor(&f.unit)?;
    let n = micros(&f.delta)?;
    if n.abs() > MAX_AMOUNT {
        return bad();
    }
    Ok(n)
}
fn day(s: &str) -> bool {
    s.len() == 10 && metadata::timestamp(&format!("{s}T00:00:00.000000Z"))
}
fn writing_time(s: &str) -> bool {
    if !s.is_ascii() || s.len() < 19 || s.len() > 26 {
        return false;
    }
    let (seconds, fraction) = s.split_once('.').unwrap_or((s, ""));
    seconds.len() == 19
        && (!s.contains('.') || !fraction.is_empty())
        && fraction.len() <= 6
        && fraction.bytes().all(|b| b.is_ascii_digit())
        && metadata::timestamp(&format!("{seconds}.{:0<6}Z", fraction))
}
pub fn validate_fact(f: &Fact) -> Result<()> {
    if !id(&f.entry_id) || !day(&f.writing_day) {
        return bad();
    }
    symbols(f)?;
    delta_symbols(f)?;
    match (&f.occurred_at, &f.writing_time) {
        (Some(t), None) if metadata::timestamp(t) => {}
        (None, Some(t)) if writing_time(t) && t[..10] == f.writing_day => {}
        _ => return bad(),
    }
    Ok(())
}
pub fn validate(e: &Event) -> Result<()> {
    let h = &e.header;
    if e.version != 1
        || !metadata::uuid(&h.account_id)
        || !metadata::uuid(&h.device_id)
        || !metadata::uuid(&h.event_id)
        || !metadata::uuid(&h.bootstrap_id)
        || !metadata::uuid(&h.metadata_event_id)
        || !id(&h.project_id)
        || h.stage_id.as_deref().is_some_and(|s| !id(s))
        || h.entity_id.len() > 512
        || h.entity_id != scope_id(h.stage_id.as_deref())
        || !metadata::timestamp(&h.updated_at)
        || h.revision < 1
        || h.revision > 9_007_199_254_740_991
        || h.generation != h.revision
    {
        return bad();
    }
    for refs in [&h.parents, &h.stage_event_ids] {
        if refs.len() > 64
            || refs.iter().any(|s| !metadata::uuid(s) || s == &h.event_id)
            || refs.windows(2).any(|s| s[0] >= s[1])
        {
            return bad();
        }
    }
    if h.stage_id.is_none() != h.stage_event_ids.is_empty() {
        return bad();
    }
    if e.entries.len() > MAX_OPERATION || e.rebased_from.len() > MAX_OPERATION {
        return Err("progress_resource_limit".into());
    }
    let mut seen = std::collections::HashSet::new();
    for f in &e.entries {
        validate_fact(f)?;
        if !seen.insert(&f.entry_id) {
            return bad();
        }
    }
    let mut seen = std::collections::HashSet::new();
    if e.rebased_from.iter().any(|s| !id(s) || !seen.insert(s)) {
        return bad();
    }
    if matches!(h.operation.as_str(), "genesis" | "adopt_local") {
        let proof = e.migration.as_ref().ok_or("invalid_progress_payload")?;
        if proof.entry_count > MAX_CHAIN
            || proof.entry_count < e.entries.len()
            || micros(&proof.final_total)? < 0
        {
            return bad();
        }
        if (h.operation == "genesis" && (!h.parents.is_empty() || h.revision != 1)
            || h.operation == "adopt_local" && (h.parents.is_empty() || h.revision < 2))
            || e.selected_event_id.is_some()
            || e.target_entry_id.is_some()
            || !e.rebased_from.is_empty()
            || e.entries.len() > 256
            || micros(e.base_total.as_deref().ok_or("invalid_progress_payload")?)? < 0
        {
            return bad();
        }
    } else {
        if !matches!(
            h.operation.as_str(),
            "append" | "select" | "rebase" | "correct" | "tombstone" | "migrate"
        ) || h.parents.is_empty()
            || h.revision < 2
            || e.base_total.is_some()
            || e.migration.is_some()
            || !e
                .selected_event_id
                .as_ref()
                .is_some_and(|s| metadata::uuid(s) && h.parents.contains(s))
        {
            return bad();
        }
        if h.operation == "migrate"
            && (h.parents.len() != 1
                || e.entries.is_empty()
                || e.entries.len() > 256
                || e.target_entry_id.is_some()
                || !e.rebased_from.is_empty())
        {
            return bad();
        }
        if h.operation == "append"
            && (h.parents.len() != 1
                || e.entries.len() != 1
                || e.target_entry_id.is_some()
                || !e.rebased_from.is_empty())
        {
            return bad();
        }
        if h.operation == "select"
            && (!e.entries.is_empty() || !e.rebased_from.is_empty() || e.target_entry_id.is_some())
        {
            return bad();
        }
        if matches!(h.operation.as_str(), "correct" | "tombstone") {
            if !e.target_entry_id.as_deref().is_some_and(id) {
                return bad();
            }
        } else if e.target_entry_id.is_some() {
            return bad();
        }
        if matches!(h.operation.as_str(), "rebase" | "correct" | "tombstone")
            && e.entries.len() != e.rebased_from.len()
        {
            return bad();
        }
    }
    Ok(())
}
pub fn encode(e: &Event) -> Result<Vec<u8>> {
    validate(e)?;
    let body = document_codec::canonical(
        &serde_json::to_value(e).map_err(|_| "invalid_progress_payload")?,
    )
    .into_bytes();
    if body.len() + 20 > MAX_FRAME_BYTES {
        return Err("progress_resource_limit".into());
    }
    let mut frame = b"WORTA-C1".to_vec();
    frame.extend([1, 11, 1, 0]);
    frame.extend((body.len() as u32).to_be_bytes());
    frame.extend((body.len() as u32).to_be_bytes());
    frame.extend(body);
    Ok(frame)
}
pub fn decode(f: &[u8]) -> Result<Event> {
    let normalized;
    let f = if f.len() >= 20 && f[11] != 0 && [11].contains(&f[9]) && [1].contains(&f[10]) {
        normalized = crate::frame_compression::normalize_authenticated_frame(
            f,
            &[11],
            &[1],
            MAX_FRAME_BYTES - 20,
        )
        .map_err(str::to_string)?;
        normalized.as_slice()
    } else {
        f
    };

    if f.len() > MAX_FRAME_BYTES {
        return Err("progress_resource_limit".into());
    }
    if f.len() < 20 || &f[..8] != b"WORTA-C1" || f[8..12] != [1, 11, 1, 0] {
        return Err("progress_codec_unsupported".into());
    }
    if f[12..16] != f[16..20]
        || u32::from_be_bytes(f[12..16].try_into().unwrap()) as usize != f.len() - 20
    {
        return bad();
    }
    // serde's default recursion bound is deliberately retained: this flat codec needs <10.
    let v: Value = serde_json::from_slice(&f[20..]).map_err(|_| "invalid_progress_payload")?;
    if document_codec::canonical(&v).as_bytes() != &f[20..] {
        return bad();
    }
    let e: Event = serde_json::from_value(v).map_err(|_| "invalid_progress_payload")?;
    validate(&e)?;
    if encode(&e)? != f {
        return bad();
    }
    Ok(e)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn progress_codec_vectors_and_reader_isolation() {
        let f: Value = serde_json::from_str(include_str!(
            "../../src/cloud/__fixtures__/progressCodecV1.json"
        ))
        .unwrap();
        for v in f["examples"].as_array().unwrap() {
            let e: Event = serde_json::from_value(v["event"].clone()).unwrap();
            let frame = encode(&e).unwrap();
            let hex: String = frame.iter().map(|n| format!("{n:02x}")).collect();
            assert_eq!(hex, v["frame_hex"]);
            assert_eq!(encode(&decode(&frame).unwrap()).unwrap(), frame);
            for i in [8, 9, 10, 11, 12, 16] {
                let mut wrong = frame.clone();
                wrong[i] ^= 1;
                assert!(decode(&wrong).is_err())
            }
        }
    }
    #[test]
    fn progress_amounts_preserve_fixed_precision_and_bounds() {
        assert!(!writing_time("2026-10-03T00:00:00."));
        assert!(writing_time("2026-10-03T00:00:00.1"));
        for s in [
            "NaN",
            "1",
            "01.000000",
            "-0.000000",
            "1.0000001",
            "1000000000001.000000",
        ] {
            assert!(micros(s).is_err())
        }
        for n in [-MAX_AMOUNT, -1, 0, 1, MAX_AMOUNT] {
            assert_eq!(micros(&decimal(n)).unwrap(), n)
        }
    }
}
