//! Structural validation for plaintext already authenticated by TypeScript.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const MAX_NOTE_SYNC_PLAINTEXT_BYTES: usize = 8 * 1024 * 1024;
const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NoteSyncHeader {
    pub event_id: String,
    pub parent_event_id: Option<String>,
    pub project_id: String,
    pub entity_id: String,
    pub entity_type: String,
    pub operation: String,
    pub revision: i64,
    pub updated_at: String,
    pub deleted_at: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct NoteSyncRoute {
    pub id: String,
    pub project_id: String,
    pub stage_id: Option<String>,
    pub source_type: String,
    pub source_map_id: Option<String>,
    pub source_node_id: Option<String>,
    pub content_format: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChecklistItem {
    pub id: String,
    pub text: String,
    pub checked: bool,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct NoteSyncRecord {
    #[serde(flatten)]
    pub route: NoteSyncRoute,
    pub title: String,
    pub content: String,
    pub checklist: Vec<ChecklistItem>,
    pub color: String,
    pub pinned: bool,
    pub archived: bool,
    pub sort_order: i64,
    pub tags: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
    pub metadata: Map<String, Value>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct NoteSyncTombstone {
    #[serde(flatten)]
    pub route: NoteSyncRoute,
    pub deleted_at: String,
}
#[derive(Clone, Debug)]
pub(crate) enum NoteSyncPlaintext {
    Create {
        header: NoteSyncHeader,
        note: NoteSyncRecord,
    },
    Update {
        header: NoteSyncHeader,
        note: NoteSyncRecord,
    },
    Delete {
        header: NoteSyncHeader,
        note: NoteSyncTombstone,
    },
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Eligibility {
    Eligible,
    DependencyNotSynced,
    UnsupportedContentFormat,
}

fn keys(object: &Map<String, Value>, expected: &[&str]) -> bool {
    object.len() == expected.len() && expected.iter().all(|key| object.contains_key(*key))
}
fn string(object: &Map<String, Value>, key: &str) -> Result<String, String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| "invalid_note_payload".into())
}
fn canonical_uuid(value: &str) -> bool {
    let b = value.as_bytes();
    b.len() == 36
        && [8, 13, 18, 23].iter().all(|&i| b[i] == b'-')
        && b.iter().enumerate().all(|(i, &c)| {
            if [8, 13, 18, 23].contains(&i) {
                true
            } else {
                c.is_ascii_digit() || (b'a'..=b'f').contains(&c)
            }
        })
}
fn timestamp(value: &str) -> bool {
    let b = value.as_bytes();
    if !(b.len() == 27
        && b[4] == b'-'
        && b[7] == b'-'
        && b[10] == b'T'
        && b[13] == b':'
        && b[16] == b':'
        && b[19] == b'.'
        && b[26] == b'Z'
        && b.iter()
            .enumerate()
            .all(|(i, &c)| matches!(i, 4 | 7 | 10 | 13 | 16 | 19 | 26) || c.is_ascii_digit()))
    {
        return false;
    };
    let n = |a: usize, z: usize| value[a..z].parse::<u32>().unwrap();
    let (y, m, d, h, mi, s) = (n(0, 4), n(5, 7), n(8, 10), n(11, 13), n(14, 16), n(17, 19));
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let days = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => 0,
    };
    d > 0 && d <= days && h < 24 && mi < 60 && s < 60
}
fn route(value: &Value) -> Result<NoteSyncRoute, String> {
    let o = value.as_object().ok_or("invalid_note_payload")?;
    let base = [
        "id",
        "project_id",
        "stage_id",
        "source_type",
        "source_map_id",
        "source_node_id",
        "content_format",
    ];
    if !base.iter().all(|k| o.contains_key(*k)) {
        return Err("invalid_note_payload".into());
    };
    let r: NoteSyncRoute =
        serde_json::from_value(value.clone()).map_err(|_| "invalid_note_payload")?;
    if r.id.is_empty()
        || r.project_id.is_empty()
        || !matches!(r.source_type.as_str(), "project" | "mindmap")
        || !matches!(r.content_format.as_str(), "html" | "plain")
    {
        Err("invalid_note_payload".into())
    } else {
        Ok(r)
    }
}
pub(crate) fn decode_note_sync_plaintext(bytes: &[u8]) -> Result<NoteSyncPlaintext, String> {
    if bytes.len() > MAX_NOTE_SYNC_PLAINTEXT_BYTES {
        return Err("payload_too_large".into());
    };
    let root: Value = serde_json::from_slice(bytes).map_err(|_| "invalid_note_payload")?;
    let o = root.as_object().ok_or("invalid_note_payload")?;
    if !keys(o, &["version", "header", "mutation", "note"])
        || o.get("version") != Some(&Value::from(1))
    {
        return Err("invalid_note_payload".into());
    };
    let ho = o["header"].as_object().ok_or("invalid_note_payload")?;
    if !keys(
        ho,
        &[
            "event_id",
            "parent_event_id",
            "project_id",
            "entity_id",
            "entity_type",
            "operation",
            "revision",
            "updated_at",
            "deleted_at",
        ],
    ) {
        return Err("invalid_note_payload".into());
    };
    let h: NoteSyncHeader =
        serde_json::from_value(o["header"].clone()).map_err(|_| "invalid_note_payload")?;
    if !canonical_uuid(&h.event_id)
        || h.parent_event_id
            .as_deref()
            .is_some_and(|v| !canonical_uuid(v))
        || h.project_id.is_empty()
        || h.entity_id.is_empty()
        || h.entity_type != "note"
        || !matches!(h.operation.as_str(), "upsert" | "delete")
        || !(1..=MAX_SAFE_INTEGER).contains(&h.revision)
        || !timestamp(&h.updated_at)
        || h.deleted_at.as_deref().is_some_and(|v| !timestamp(v))
        || ((h.revision == 1) != (h.parent_event_id.is_none()))
        || h.parent_event_id.as_deref() == Some(&h.event_id)
    {
        return Err("invalid_note_payload".into());
    };
    let mutation = string(o, "mutation")?;
    if h.operation == "delete" {
        if mutation != "delete"
            || h.deleted_at.is_none()
            || !keys(
                o["note"].as_object().ok_or("invalid_note_payload")?,
                &[
                    "id",
                    "project_id",
                    "stage_id",
                    "source_type",
                    "source_map_id",
                    "source_node_id",
                    "content_format",
                    "deleted_at",
                ],
            )
        {
            return Err("invalid_note_payload".into());
        };
        let n: NoteSyncTombstone =
            serde_json::from_value(o["note"].clone()).map_err(|_| "invalid_note_payload")?;
        let r = route(&o["note"])?;
        if n.deleted_at != h.deleted_at.clone().unwrap()
            || r.id != h.entity_id
            || r.project_id != h.project_id
            || !timestamp(&n.deleted_at)
        {
            return Err("invalid_note_payload".into());
        };
        Ok(NoteSyncPlaintext::Delete { header: h, note: n })
    } else {
        if h.deleted_at.is_some()
            || mutation != if h.revision == 1 { "create" } else { "update" }
            || !keys(
                o["note"].as_object().ok_or("invalid_note_payload")?,
                &[
                    "id",
                    "project_id",
                    "stage_id",
                    "source_type",
                    "source_map_id",
                    "source_node_id",
                    "content_format",
                    "title",
                    "content",
                    "checklist",
                    "color",
                    "pinned",
                    "archived",
                    "sort_order",
                    "tags",
                    "created_at",
                    "updated_at",
                    "metadata",
                ],
            )
        {
            return Err("invalid_note_payload".into());
        };
        let n: NoteSyncRecord =
            serde_json::from_value(o["note"].clone()).map_err(|_| "invalid_note_payload")?;
        let r = route(&o["note"])?;
        if !n.checklist.iter().all(|i| !i.id.is_empty())
            || !(-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(&n.sort_order)
            || r.id != h.entity_id
            || r.project_id != h.project_id
            || n.updated_at != h.updated_at
            || !timestamp(&n.created_at)
            || !timestamp(&n.updated_at)
        {
            return Err("invalid_note_payload".into());
        };
        if mutation == "create" {
            Ok(NoteSyncPlaintext::Create { header: h, note: n })
        } else {
            Ok(NoteSyncPlaintext::Update { header: h, note: n })
        }
    }
}

#[allow(dead_code)]
#[derive(Clone, Debug)]
pub(crate) struct NoteSyncResolutionV2Header {
    pub event_id: String,
    pub parent_event_id: String,
    pub additional_parent_event_ids: Vec<String>,
    pub project_id: String,
    pub entity_id: String,
    pub revision: i64,
    pub updated_at: String,
}
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub(crate) enum NoteSyncResolutionV2Strategy {
    ChooseVersion {
        selected_event_id: String,
    },
    ManualMerge,
    KeepBoth {
        selected_event_id: String,
        retained_event_id: String,
        retained_note: NoteSyncRecord,
    },
    Delete,
}
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub(crate) enum NoteSyncResolutionV2Result {
    Upsert(NoteSyncRecord),
    Delete(NoteSyncTombstone),
}
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub(crate) struct NoteSyncResolutionV2 {
    pub header: NoteSyncResolutionV2Header,
    pub conflict_group_id: String,
    pub conflict_generation: i64,
    pub resolved_event_ids: Vec<String>,
    pub strategy: NoteSyncResolutionV2Strategy,
    pub result: NoteSyncResolutionV2Result,
}

#[allow(dead_code)]
fn sorted_ids(value: &Value, minimum: usize, maximum: usize) -> Result<Vec<String>, String> {
    let values = value
        .as_array()
        .filter(|v| v.len() >= minimum && v.len() <= maximum)
        .ok_or("invalid_note_payload")?;
    let ids: Vec<String> = values
        .iter()
        .map(|v| {
            v.as_str()
                .filter(|id| canonical_uuid(id))
                .map(str::to_owned)
                .ok_or_else(|| "invalid_note_payload".into())
        })
        .collect::<Result<_, String>>()?;
    if ids.windows(2).any(|pair| pair[0] >= pair[1]) {
        Err("invalid_note_payload".into())
    } else {
        Ok(ids)
    }
}
#[allow(dead_code)]
fn resolution_record(value: &Value) -> Result<NoteSyncRecord, String> {
    let o = value.as_object().ok_or("invalid_note_payload")?;
    if !keys(
        o,
        &[
            "id",
            "project_id",
            "stage_id",
            "source_type",
            "source_map_id",
            "source_node_id",
            "content_format",
            "title",
            "content",
            "checklist",
            "color",
            "pinned",
            "archived",
            "sort_order",
            "tags",
            "created_at",
            "updated_at",
            "metadata",
        ],
    ) {
        return Err("invalid_note_payload".into());
    }
    let n: NoteSyncRecord =
        serde_json::from_value(value.clone()).map_err(|_| "invalid_note_payload")?;
    let r = route(value)?;
    if !n.checklist.iter().all(|i| !i.id.is_empty())
        || !(-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(&n.sort_order)
        || r.id != n.route.id
        || r.project_id != n.route.project_id
        || !timestamp(&n.created_at)
        || !timestamp(&n.updated_at)
    {
        return Err("invalid_note_payload".into());
    }
    Ok(n)
}
#[allow(dead_code)]
fn resolution_tombstone(value: &Value) -> Result<NoteSyncTombstone, String> {
    let o = value.as_object().ok_or("invalid_note_payload")?;
    if !keys(
        o,
        &[
            "id",
            "project_id",
            "stage_id",
            "source_type",
            "source_map_id",
            "source_node_id",
            "content_format",
            "deleted_at",
        ],
    ) {
        return Err("invalid_note_payload".into());
    }
    let n: NoteSyncTombstone =
        serde_json::from_value(value.clone()).map_err(|_| "invalid_note_payload")?;
    let r = route(value)?;
    if r.id != n.route.id || r.project_id != n.route.project_id || !timestamp(&n.deleted_at) {
        Err("invalid_note_payload".into())
    } else {
        Ok(n)
    }
}

/// Decodes only the frozen v2 resolution plaintext.  It is intentionally not
/// called by the v1 remote apply path; causal-history proof belongs to a later
/// SQLite transaction, not this structural boundary.
#[allow(dead_code)]
pub(crate) fn decode_note_sync_resolution_v2(bytes: &[u8]) -> Result<NoteSyncResolutionV2, String> {
    if bytes.len() > MAX_NOTE_SYNC_PLAINTEXT_BYTES {
        return Err("payload_too_large".into());
    }
    let root: Value = serde_json::from_slice(bytes).map_err(|_| "invalid_note_payload")?;
    let o = root.as_object().ok_or("invalid_note_payload")?;
    if !keys(
        o,
        &["version", "header", "mutation", "resolution", "result"],
    ) || o.get("version") != Some(&Value::from(2))
        || o.get("mutation") != Some(&Value::from("resolution"))
    {
        return Err("invalid_note_payload".into());
    }
    let h = o["header"].as_object().ok_or("invalid_note_payload")?;
    if !keys(
        h,
        &[
            "event_id",
            "parent_event_id",
            "additional_parent_event_ids",
            "project_id",
            "entity_id",
            "entity_type",
            "operation",
            "revision",
            "updated_at",
        ],
    ) {
        return Err("invalid_note_payload".into());
    }
    let event_id = string(h, "event_id")?;
    let parent_event_id = string(h, "parent_event_id")?;
    let project_id = string(h, "project_id")?;
    let entity_id = string(h, "entity_id")?;
    let revision = h
        .get("revision")
        .and_then(Value::as_i64)
        .filter(|v| (2..=MAX_SAFE_INTEGER).contains(v))
        .ok_or("invalid_note_payload")?;
    let updated_at = string(h, "updated_at")?;
    if !canonical_uuid(&event_id)
        || !canonical_uuid(&parent_event_id)
        || h.get("entity_type") != Some(&Value::from("note"))
        || h.get("operation") != Some(&Value::from("resolution"))
        || !timestamp(&updated_at)
    {
        return Err("invalid_note_payload".into());
    }
    let additional = sorted_ids(
        h.get("additional_parent_event_ids")
            .ok_or("invalid_note_payload")?,
        1,
        63,
    )?;
    let mut parents = vec![parent_event_id.clone()];
    parents.extend(additional.iter().cloned());
    let r = o["resolution"].as_object().ok_or("invalid_note_payload")?;
    let strategy_name = string(r, "strategy")?;
    let group = string(r, "conflict_group_id")?;
    let generation = r
        .get("conflict_generation")
        .and_then(Value::as_i64)
        .filter(|v| (1..=MAX_SAFE_INTEGER).contains(v))
        .ok_or("invalid_note_payload")?;
    if !canonical_uuid(&group) {
        return Err("invalid_note_payload".into());
    };
    let resolved = sorted_ids(
        r.get("resolved_event_ids").ok_or("invalid_note_payload")?,
        2,
        64,
    )?;
    if parents != resolved
        || parent_event_id != resolved[0]
        || resolved.iter().any(|id| id == &event_id)
    {
        return Err("invalid_note_payload".into());
    }
    let result = o["result"].as_object().ok_or("invalid_note_payload")?;
    if !keys(result, &["operation", "note"]) {
        return Err("invalid_note_payload".into());
    };
    let operation = string(result, "operation")?;
    let decoded_result = match operation.as_str() {
        "upsert" => NoteSyncResolutionV2Result::Upsert(resolution_record(&result["note"])?),
        "delete" => NoteSyncResolutionV2Result::Delete(resolution_tombstone(&result["note"])?),
        _ => return Err("invalid_note_payload".into()),
    };
    let (result_id, result_project) = match &decoded_result {
        NoteSyncResolutionV2Result::Upsert(n) => (n.route.id.as_str(), n.route.project_id.as_str()),
        NoteSyncResolutionV2Result::Delete(n) => (n.route.id.as_str(), n.route.project_id.as_str()),
    };
    if result_id != entity_id || result_project != project_id {
        return Err("invalid_note_payload".into());
    }
    let strategy = match strategy_name.as_str() {
        "choose_version" => {
            if !keys(
                r,
                &[
                    "conflict_group_id",
                    "conflict_generation",
                    "resolved_event_ids",
                    "strategy",
                    "selected_event_id",
                ],
            ) {
                return Err("invalid_note_payload".into());
            };
            let selected = string(r, "selected_event_id")?;
            if !canonical_uuid(&selected) || !resolved.contains(&selected) {
                return Err("invalid_note_payload".into());
            };
            NoteSyncResolutionV2Strategy::ChooseVersion {
                selected_event_id: selected,
            }
        }
        "manual_merge" => {
            if !keys(
                r,
                &[
                    "conflict_group_id",
                    "conflict_generation",
                    "resolved_event_ids",
                    "strategy",
                ],
            ) || !matches!(decoded_result, NoteSyncResolutionV2Result::Upsert(_))
            {
                return Err("invalid_note_payload".into());
            };
            NoteSyncResolutionV2Strategy::ManualMerge
        }
        "delete" => {
            if !keys(
                r,
                &[
                    "conflict_group_id",
                    "conflict_generation",
                    "resolved_event_ids",
                    "strategy",
                ],
            ) || !matches!(decoded_result, NoteSyncResolutionV2Result::Delete(_))
            {
                return Err("invalid_note_payload".into());
            };
            NoteSyncResolutionV2Strategy::Delete
        }
        "keep_both" => {
            if !keys(
                r,
                &[
                    "conflict_group_id",
                    "conflict_generation",
                    "resolved_event_ids",
                    "strategy",
                    "selected_event_id",
                    "retained_event_id",
                    "retained_note",
                ],
            ) || resolved.len() != 2
                || !matches!(decoded_result, NoteSyncResolutionV2Result::Upsert(_))
            {
                return Err("invalid_note_payload".into());
            };
            let selected = string(r, "selected_event_id")?;
            let retained = string(r, "retained_event_id")?;
            let note = resolution_record(&r["retained_note"])?;
            if !canonical_uuid(&selected)
                || !canonical_uuid(&retained)
                || selected == retained
                || !resolved.contains(&selected)
                || !resolved.contains(&retained)
                || note.route.id == entity_id
                || note.route.project_id != project_id
            {
                return Err("invalid_note_payload".into());
            };
            NoteSyncResolutionV2Strategy::KeepBoth {
                selected_event_id: selected,
                retained_event_id: retained,
                retained_note: note,
            }
        }
        _ => return Err("invalid_note_payload".into()),
    };
    Ok(NoteSyncResolutionV2 {
        header: NoteSyncResolutionV2Header {
            event_id,
            parent_event_id,
            additional_parent_event_ids: additional,
            project_id,
            entity_id,
            revision,
            updated_at,
        },
        conflict_group_id: group,
        conflict_generation: generation,
        resolved_event_ids: resolved,
        strategy,
        result: decoded_result,
    })
}

#[cfg(test)]
mod golden_fixture_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn golden_create() {
        let fixture: Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../src/cloud/__fixtures__/noteSyncPlaintextV1.json"
        )))
        .unwrap();
        let bytes = fixture["canonical_json"].as_str().unwrap().as_bytes();
        match decode_note_sync_plaintext(bytes).unwrap() {
            NoteSyncPlaintext::Create { header, note } => {
                assert_eq!(header.revision, 1);
                assert!(header.parent_event_id.is_none());
                assert_eq!(header.project_id, "p");
                assert_eq!(header.entity_id, "n");
                assert_eq!(note.sort_order, 0);
                assert_eq!(eligibility(&note.route), Eligibility::Eligible);
            }
            _ => panic!("golden fixture must be a create"),
        }
    }

    #[test]
    fn golden_update() {
        let fixture: Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../src/cloud/__fixtures__/noteSyncPlaintextV1.json"
        )))
        .unwrap();
        match decode_note_sync_plaintext(
            fixture["update"]["canonical_json"]
                .as_str()
                .unwrap()
                .as_bytes(),
        )
        .unwrap()
        {
            NoteSyncPlaintext::Update { header, note } => {
                assert_eq!(header.revision, 2);
                assert_eq!(
                    header.parent_event_id.as_deref(),
                    Some("123e4567-e89b-42d3-a456-426614174000")
                );
                assert_eq!(header.project_id, "p");
                assert_eq!(header.entity_id, "n");
                assert_eq!(header.updated_at, "2026-01-02T00:00:00.000000Z");
                assert_eq!(eligibility(&note.route), Eligibility::Eligible);
            }
            _ => panic!("golden fixture must be an update"),
        }
    }

    #[test]
    fn resolution_v2_shared_golden_examples_decode() {
        let fixture: Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../src/cloud/__fixtures__/noteSyncPlaintextV2Resolution.json"
        )))
        .unwrap();
        for example in fixture["examples"].as_array().unwrap() {
            let bytes = example["canonical_json"].as_str().unwrap().as_bytes();
            let decoded = decode_note_sync_resolution_v2(bytes).unwrap();
            assert_eq!(
                decoded.header.parent_event_id,
                decoded.resolved_event_ids[0]
            );
            assert_eq!(
                decoded.header.revision,
                match example["name"].as_str().unwrap() {
                    "edit_edit_choose_version" => 4,
                    "delete_edit_choose_edit" => 6,
                    "unequal_depth_manual_merge" => 7,
                    _ => panic!("unknown fixture"),
                }
            );
            match (example["name"].as_str().unwrap(), decoded.strategy) {
                (
                    "edit_edit_choose_version" | "delete_edit_choose_edit",
                    NoteSyncResolutionV2Strategy::ChooseVersion { .. },
                ) => {}
                ("unequal_depth_manual_merge", NoteSyncResolutionV2Strategy::ManualMerge) => {}
                _ => panic!("unexpected v2 strategy"),
            }
        }
    }

    #[test]
    fn resolution_v2_rejects_bad_structure_and_v1_rejects_v2() {
        let fixture: Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../src/cloud/__fixtures__/noteSyncPlaintextV2Resolution.json"
        )))
        .unwrap();
        let base = fixture["examples"][0]["plaintext"].clone();
        let mut cases = Vec::new();
        let mut extra = base.clone();
        extra["extra"] = json!(true);
        cases.push(extra);
        let mut missing_nested = base.clone();
        missing_nested["result"]["note"]
            .as_object_mut()
            .unwrap()
            .remove("title");
        cases.push(missing_nested);
        let mut extra_nested = base.clone();
        extra_nested["result"]["note"]["extra"] = json!(true);
        cases.push(extra_nested);
        let mut duplicate = base.clone();
        duplicate["header"]["additional_parent_event_ids"] =
            json!([base["header"]["parent_event_id"].clone()]);
        cases.push(duplicate);
        let mut unsorted = base.clone();
        unsorted["resolution"]["resolved_event_ids"] = json!([
            base["resolution"]["resolved_event_ids"][1].clone(),
            base["resolution"]["resolved_event_ids"][0].clone()
        ]);
        cases.push(unsorted);
        let mut self_ref = base.clone();
        self_ref["header"]["event_id"] = base["header"]["parent_event_id"].clone();
        cases.push(self_ref);
        let mut upper_uuid = base.clone();
        upper_uuid["header"]["event_id"] = json!("123E4567-e89b-42d3-a456-426614174100");
        cases.push(upper_uuid);
        let mut bad_timestamp = base.clone();
        bad_timestamp["header"]["updated_at"] = json!("2026-09-25T10:00:00Z");
        cases.push(bad_timestamp);
        let mut bad_variant = base.clone();
        bad_variant["resolution"] = json!({"conflict_group_id":base["resolution"]["conflict_group_id"].clone(),"conflict_generation":1,"resolved_event_ids":base["resolution"]["resolved_event_ids"].clone(),"strategy":"manual_merge"});
        bad_variant["result"] = json!({"operation":"delete","note":{"id":"note-resolution","project_id":"project-resolution","stage_id":null,"source_type":"project","source_map_id":null,"source_node_id":null,"content_format":"html","deleted_at":"2026-09-25T10:00:00.000000Z"}});
        cases.push(bad_variant);
        let mut bad_keep = base.clone();
        bad_keep["resolution"] = json!({"conflict_group_id":base["resolution"]["conflict_group_id"].clone(),"conflict_generation":1,"resolved_event_ids":base["resolution"]["resolved_event_ids"].clone(),"strategy":"keep_both","selected_event_id":base["resolution"]["resolved_event_ids"][0].clone(),"retained_event_id":base["resolution"]["resolved_event_ids"][1].clone(),"retained_note":base["result"]["note"].clone()});
        cases.push(bad_keep);
        for value in cases {
            assert!(decode_note_sync_resolution_v2(
                serde_json::to_string(&value).unwrap().as_bytes()
            )
            .is_err());
        }
        assert!(decode_note_sync_plaintext(
            fixture["examples"][0]["canonical_json"]
                .as_str()
                .unwrap()
                .as_bytes()
        )
        .is_err());
    }
}
pub(crate) fn eligibility(route: &NoteSyncRoute) -> Eligibility {
    if route.stage_id.is_some()
        || route.source_type != "project"
        || route.source_map_id.is_some()
        || route.source_node_id.is_some()
    {
        Eligibility::DependencyNotSynced
    } else if route.content_format != "html" {
        Eligibility::UnsupportedContentFormat
    } else {
        Eligibility::Eligible
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn valid() -> String {
        r#"{"version":1,"header":{"event_id":"123e4567-e89b-42d3-a456-426614174000","parent_event_id":null,"project_id":"p","entity_id":"n","entity_type":"note","operation":"upsert","revision":1,"updated_at":"2026-01-01T00:00:00.000000Z","deleted_at":null},"mutation":"create","note":{"id":"n","project_id":"p","stage_id":null,"source_type":"project","source_map_id":null,"source_node_id":null,"content_format":"html","title":"Привет","content":"x","checklist":[{"id":"c","text":"x","checked":false}],"color":"default","pinned":false,"archived":false,"sort_order":0,"tags":[],"created_at":"2026-01-01T00:00:00.000000Z","updated_at":"2026-01-01T00:00:00.000000Z","metadata":{"nested":{"x":true}}}}"#.into()
    }
    #[test]
    fn valid_create_and_eligibility() {
        let p = decode_note_sync_plaintext(valid().as_bytes()).unwrap();
        if let NoteSyncPlaintext::Create { note, .. } = p {
            assert_eq!(eligibility(&note.route), Eligibility::Eligible)
        } else {
            panic!()
        }
    }
    #[test]
    fn exact_record_keys_and_calendar_are_required() {
        let bad = valid().replace(
            "\"metadata\":{\"nested\":{\"x\":true}}",
            "\"metadata\":{},\"extra\":1",
        );
        assert!(decode_note_sync_plaintext(bad.as_bytes()).is_err());
        let bad = valid().replace("2026-01-01", "2026-02-30");
        assert!(decode_note_sync_plaintext(bad.as_bytes()).is_err())
    }
    #[test]
    fn size_is_limited() {
        assert_eq!(
            decode_note_sync_plaintext(&vec![b' '; MAX_NOTE_SYNC_PLAINTEXT_BYTES + 1]).unwrap_err(),
            "payload_too_large"
        )
    }
    #[test]
    fn safe_sort_orders_and_eligibility_variants() {
        for order in ["0", "-1", "9007199254740991", "-9007199254740991"] {
            assert!(decode_note_sync_plaintext(
                valid()
                    .replace("\"sort_order\":0", &format!("\"sort_order\":{order}"))
                    .as_bytes()
            )
            .is_ok())
        }
        let plain = valid().replace(
            "\"content_format\":\"html\"",
            "\"content_format\":\"plain\"",
        );
        let Value::Object(root) = serde_json::from_str::<Value>(&plain).unwrap() else {
            panic!()
        };
        let n = match decode_note_sync_plaintext(serde_json::to_string(&root).unwrap().as_bytes())
            .unwrap()
        {
            NoteSyncPlaintext::Create { note, .. } => note,
            _ => panic!(),
        };
        assert_eq!(eligibility(&n.route), Eligibility::UnsupportedContentFormat);
        let route = NoteSyncRoute {
            id: "n".into(),
            project_id: "p".into(),
            stage_id: Some("s".into()),
            source_type: "project".into(),
            source_map_id: None,
            source_node_id: None,
            content_format: "html".into(),
        };
        assert_eq!(eligibility(&route), Eligibility::DependencyNotSynced)
    }
    #[test]
    fn semantic_and_exact_key_rejections() {
        for bad in [
            valid().replace("\"id\":\"n\"", "\"id\":\"other\""),
            valid().replace("123e4567", "123E4567"),
            valid().replace("\"revision\":1", "\"revision\":0"),
            valid().replace("\"stage_id\":null,", ""),
            valid().replace("\"checked\":false", "\"checked\":false,\"x\":1"),
            valid().replace("\"metadata\":{\"nested\":{\"x\":true}}", "\"metadata\":[]"),
            valid().replace("2026-01-01", "2026-02-30"),
        ] {
            assert!(decode_note_sync_plaintext(bad.as_bytes()).is_err())
        }
    }
}
