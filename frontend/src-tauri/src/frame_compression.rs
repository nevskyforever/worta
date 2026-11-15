//! C18.7.01 RFC1950/RFC1951 substrate. Input must already be AEAD-authenticated.
//! No convenience decoder, auto-concatenation, dictionary or writer activation.
use flate2::{Compression, Decompress, FlushDecompress, Status};
use std::io::Write;
pub const MAX_BYTES: usize = 8 * 1024 * 1024 - 20;
pub const MAX_INPUT: usize = 1024 * 1024;
pub const RATIO: usize = 512;
pub const CHUNK: usize = 16384;
pub const CODES: [&str; 6] = [
    "compression_unsupported",
    "compression_invalid_stream",
    "compression_input_limit",
    "compression_output_limit",
    "compression_resource_limit",
    "compression_length_mismatch",
];
pub fn decompress_bounded(
    algorithm: u8,
    input: &[u8],
    declared: usize,
    entity_limit: usize,
) -> Result<Vec<u8>, &'static str> {
    if algorithm > 1 {
        return Err(CODES[0]);
    }
    if input.len() > if algorithm == 0 { MAX_BYTES } else { MAX_INPUT } {
        return Err(CODES[2]);
    }
    if declared > entity_limit.min(MAX_BYTES) {
        return Err(CODES[3]);
    }
    if algorithm == 0 {
        return if input.len() == declared {
            Ok(input.to_vec())
        } else {
            Err(CODES[5])
        };
    }
    if declared == 0 || declared > input.len().saturating_mul(RATIO) {
        return Err(CODES[4]);
    }
    if input.len() < 6
        || input[0] & 15 != 8
        || input[0] >> 4 > 7
        || (u16::from(input[0]) * 256 + u16::from(input[1])) % 31 != 0
        || input[1] & 32 != 0
    {
        return Err(CODES[1]);
    }
    let mut decoder = Decompress::new(true);
    let mut output = vec![0; declared + 1];
    loop {
        let start_in = decoder.total_in() as usize;
        let start_out = decoder.total_out() as usize;
        let end = (start_out + CHUNK).min(output.len());
        let status = decoder
            .decompress(
                &input[start_in..],
                &mut output[start_out..end],
                FlushDecompress::None,
            )
            .map_err(|_| CODES[1])?;
        if decoder.total_out() as usize > declared {
            return Err(CODES[5]);
        }
        if status == Status::StreamEnd {
            if decoder.total_in() as usize != input.len() {
                return Err(CODES[1]);
            }
            if decoder.total_out() as usize != declared {
                return Err(CODES[5]);
            }
            output.truncate(declared);
            return Ok(output);
        }
        if decoder.total_in() as usize == start_in && decoder.total_out() as usize == start_out {
            return Err(CODES[1]);
        }
    }
}
pub fn compress_for_frame(bytes: &[u8], codec: u8) -> Result<(u8, Vec<u8>), &'static str> {
    if bytes.len() > MAX_BYTES {
        return Err(CODES[3]);
    }
    if !(1..=13).contains(&codec) || bytes.len() < 1024 {
        return Ok((0, bytes.to_vec()));
    }
    let mut writer = flate2::write::ZlibEncoder::new(Vec::new(), Compression::new(6));
    writer.write_all(bytes).map_err(|_| CODES[1])?;
    let payload = writer.finish().map_err(|_| CODES[1])?;
    let saving = bytes.len().saturating_sub(payload.len());
    if payload.len() > MAX_INPUT
        || saving < 128
        || saving * 100 < bytes.len() * 10
        || bytes.len() > payload.len().saturating_mul(RATIO)
    {
        return Ok((0, bytes.to_vec()));
    }
    Ok((1, payload))
}
pub fn normalize_authenticated_frame(
    frame: &[u8],
    codecs: &[u8],
    versions: &[u8],
    limit: usize,
) -> Result<Vec<u8>, &'static str> {
    if frame.len() < 20
        || &frame[..8] != b"WORTA-C1"
        || frame[8] != 1
        || !codecs.contains(&frame[9])
        || !versions.contains(&frame[10])
    {
        return Err(CODES[1]);
    }
    let declared = u32::from_be_bytes(frame[12..16].try_into().unwrap()) as usize;
    if u32::from_be_bytes(frame[16..20].try_into().unwrap()) as usize != frame.len() - 20 {
        return Err(CODES[1]);
    }
    let mut payload = decompress_bounded(frame[11], &frame[20..], declared, limit)?;
    let mut out = Vec::with_capacity(20 + declared);
    out.extend_from_slice(&frame[..20]);
    out[11] = 0;
    out[16..20].copy_from_slice(&(declared as u32).to_be_bytes());
    out.extend_from_slice(&payload);
    payload.fill(0);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use sha2::{Digest, Sha256};
    fn fixture() -> Value {
        serde_json::from_str(include_str!(
            "../../src/cloud/__fixtures__/frameCompressionV1.json"
        ))
        .unwrap()
    }
    fn hex(raw: &str) -> Vec<u8> {
        (0..raw.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&raw[i..i + 2], 16).unwrap())
            .collect()
    }
    fn original(v: &Value) -> Vec<u8> {
        format!(
            "{}{}{}",
            v["prefix"].as_str().unwrap(),
            v["unit"]
                .as_str()
                .unwrap()
                .repeat(v["repeat"].as_u64().unwrap() as usize),
            v["suffix"].as_str().unwrap()
        )
        .into_bytes()
    }
    fn stream(bytes: &[u8]) -> Vec<u8> {
        let mut z = flate2::write::ZlibEncoder::new(vec![], Compression::new(6));
        z.write_all(bytes).unwrap();
        z.finish().unwrap()
    }
    #[test]
    fn shared_cross_runtime_vectors_and_native_policy() {
        for v in fixture()["vectors"].as_array().unwrap() {
            let bytes = original(v);
            assert_eq!(
                format!("{:x}", Sha256::digest(&bytes)),
                v["sha256"].as_str().unwrap()
            );
            for key in ["ts_hex", "rust_hex"] {
                assert_eq!(
                    decompress_bounded(1, &hex(v[key].as_str().unwrap()), bytes.len(), MAX_BYTES)
                        .unwrap(),
                    bytes
                );
            }
            let (algorithm, payload) =
                compress_for_frame(&bytes, v["codec"].as_u64().unwrap() as u8).unwrap();
            assert_eq!(algorithm as u64, v["policy"].as_u64().unwrap());
            assert_eq!(
                decompress_bounded(algorithm, &payload, bytes.len(), MAX_BYTES).unwrap(),
                bytes
            );
            for key in ["frame_hex", "rust_frame_hex"] {
                let frame = hex(v[key].as_str().unwrap());
                let id = frame[9];
                let view =
                    normalize_authenticated_frame(&frame, &[id], &[frame[10]], MAX_BYTES).unwrap();
                assert_eq!(&view[20..], &bytes);
                if id == 1 {
                    assert!(crate::project_metadata_sync::unframe_metadata_event(&frame).is_ok());
                } else {
                    assert!(crate::document_codec::decode(&frame).is_ok());
                }
            }
        }
    }
    #[test]
    fn hostile_streams_fail_closed() {
        let bytes = "Живая рукопись ".repeat(100).into_bytes();
        let compressed = stream(&bytes);
        assert_eq!(
            decompress_bounded(2, &[], 0, MAX_BYTES).unwrap_err(),
            CODES[0]
        );
        assert_eq!(
            decompress_bounded(1, &vec![0; MAX_BYTES + 1], 1, MAX_BYTES).unwrap_err(),
            CODES[2]
        );
        assert_eq!(
            decompress_bounded(1, &[0; 6], MAX_BYTES + 1, MAX_BYTES).unwrap_err(),
            CODES[3]
        );
        assert_eq!(
            decompress_bounded(1, &[0; 6], 100000, MAX_BYTES).unwrap_err(),
            CODES[4]
        );
        let bomb = stream(&vec![0; 100000]);
        assert_eq!(
            decompress_bounded(1, &bomb, 100000, MAX_BYTES).unwrap_err(),
            CODES[4]
        );
        assert_eq!(
            decompress_bounded(1, &bomb, 100, MAX_BYTES).unwrap_err(),
            CODES[5]
        );
        for n in [bytes.len() - 1, bytes.len() + 1] {
            assert_eq!(
                decompress_bounded(1, &compressed, n, MAX_BYTES).unwrap_err(),
                CODES[5]
            );
        }
        for i in 0..compressed.len() {
            assert!(decompress_bounded(1, &compressed[..i], bytes.len(), MAX_BYTES).is_err());
        }
        let mut trailing = compressed.clone();
        trailing.push(0);
        let mut concat = compressed.clone();
        concat.extend_from_slice(&compressed);
        let mut corrupt = compressed.clone();
        *corrupt.last_mut().unwrap() ^= 1;
        for bad in [
            trailing,
            concat,
            corrupt,
            vec![0x88, 0x1c, 0, 0, 0, 0],
            vec![0x78, 0x20, 0, 0, 0, 0],
            vec![0; 6],
        ] {
            assert_eq!(
                decompress_bounded(1, &bad, bytes.len(), MAX_BYTES).unwrap_err(),
                CODES[1]
            );
        }
    }
    #[test]
    fn invalid_canonical_and_deep_document_after_compression() {
        for body in [
            "{\"version\":1,\"unknown\":true}".to_string(),
            format!("{}0{}", "[".repeat(170), "]".repeat(170)),
        ] {
            let compressed = stream(body.as_bytes());
            let mut frame = b"WORTA-C1".to_vec();
            frame.extend([1, 10, 1, 1]);
            frame.extend((body.len() as u32).to_be_bytes());
            frame.extend((compressed.len() as u32).to_be_bytes());
            frame.extend(compressed);
            assert!(crate::document_codec::decode(&frame).is_err());
        }
    }
    #[test]
    fn every_codec_reader_accepts_id1_without_changing_id0_writers() {
        let metadata: Value = serde_json::from_str(include_str!(
            "../../src/cloud/__fixtures__/projectMetadataV2.json"
        ))
        .unwrap();
        let stage: Value = serde_json::from_str(include_str!(
            "../../src/cloud/__fixtures__/stageCodecV1.json"
        ))
        .unwrap();
        let catalog: Value = serde_json::from_str(include_str!(
            "../../src/cloud/__fixtures__/accountCatalogV1.json"
        ))
        .unwrap();
        let mut cases = vec![
            (1, metadata[0]["event"].clone()),
            (2, stage["event"].clone()),
        ];
        let mut order = stage["event"].clone();
        order["header"]["entity_type"] = Value::from("stage_order");
        order["header"]["entity_id"] = Value::from("stage_order");
        order["stage"] = Value::Null;
        order["stage_ids"] = serde_json::json!([]);
        order["stage_heads"] = serde_json::json!({});
        cases.push((3, order));
        for (id, kind) in [
            (4, "folder"),
            (5, "folder_order"),
            (6, "folder_membership"),
            (7, "project_order"),
        ] {
            let mut e = catalog.clone();
            e["header"]["entity_type"] = Value::from(kind);
            if id == 5 || id == 7 {
                e["header"]["entity_id"] = Value::from(kind);
                e["payload"] = serde_json::json!({"ids":[]});
            }
            if id == 6 {
                e["header"]["entity_id"] = Value::from("P1");
                e["payload"] = serde_json::json!({"folder_id":"F1"});
                e["dependencies"] = serde_json::json!({"folders":{"F1":[stage["event"]["header"]["event_id"]]},"projects":{"P1":{"bootstrap_id":stage["event"]["header"]["bootstrap_id"],"metadata_event_id":stage["event"]["header"]["metadata_event_id"]}},"memberships":{}});
            }
            cases.push((id, e));
        }
        for (id, raw) in [
            (
                8,
                include_str!("../../src/cloud/__fixtures__/contentNoteCodecV1.json"),
            ),
            (
                9,
                include_str!("../../src/cloud/__fixtures__/mapCodecV1.json"),
            ),
            (
                10,
                include_str!("../../src/cloud/__fixtures__/documentCodecV1.json"),
            ),
            (
                11,
                include_str!("../../src/cloud/__fixtures__/progressCodecV1.json"),
            ),
        ] {
            let f: Value = serde_json::from_str(raw).unwrap();
            cases.push((id, f["examples"][0]["event"].clone()));
        }
        let games: Value = serde_json::from_str(include_str!(
            "../../src/cloud/__fixtures__/gameCodecV1.json"
        ))
        .unwrap();
        for (id, scope) in [(12, "project"), (13, "account")] {
            let e = games["examples"]
                .as_array()
                .unwrap()
                .iter()
                .find(|e| e["event"]["header"]["scope"] == scope)
                .unwrap();
            cases.push((id, e["event"].clone()));
        }
        assert_eq!(
            cases.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            (1..=13).collect::<Vec<_>>()
        );
        for (id, e) in cases {
            let body = if id == 9 {
                let v: Value = serde_json::from_str(include_str!(
                    "../../src/cloud/__fixtures__/mapCodecV1.json"
                ))
                .unwrap();
                v["examples"][0]["canonical_json"]
                    .as_str()
                    .unwrap()
                    .as_bytes()
                    .to_vec()
            } else {
                serde_json::to_vec(&e).unwrap()
            };
            let payload = stream(&body);
            let mut f = b"WORTA-C1".to_vec();
            f.extend([1, id, e["version"].as_u64().unwrap() as u8, 1]);
            f.extend((body.len() as u32).to_be_bytes());
            f.extend((payload.len() as u32).to_be_bytes());
            f.extend(payload);
            match id {
                1 => {
                    crate::project_metadata_sync::unframe_metadata_event(&f).unwrap();
                }
                2 | 3 => {
                    let e = crate::stage_sync::unframe(&f).unwrap();
                    assert_eq!(crate::stage_sync::frame(&e).unwrap()[11], 0);
                }
                4..=7 => {
                    let e = crate::account_catalog::unframe(&f).unwrap();
                    assert_eq!(crate::account_catalog::frame(&e).unwrap()[11], 0);
                }
                8 => {
                    crate::content_note_sync::decode(&f).unwrap();
                }
                9 => {
                    let e = crate::map_codec::decode(&f).unwrap();
                    assert_eq!(crate::map_codec::encode(&e).unwrap()[11], 0);
                }
                10 => {
                    let e = crate::document_codec::decode(&f).unwrap();
                    assert_eq!(crate::document_codec::encode(&e).unwrap()[11], 0);
                }
                11 => {
                    let e = crate::progress_codec::decode(&f).unwrap();
                    assert_eq!(crate::progress_codec::encode(&e).unwrap()[11], 0);
                }
                12 | 13 => {
                    let e = crate::game_codec::unframe(&f, id == 12).unwrap();
                    assert_eq!(crate::game_codec::frame(&e).unwrap()[11], 0);
                }
                _ => unreachable!(),
            }
        }
    }
}

/// Durable canonical identity uses the historical ID0 view; the original
/// nonce/ciphertext remains immutable replay and ACK evidence. Owning codecs
/// still perform their exact canonical/entity-limit checks after this transform.
pub fn canonical_view(frame: &[u8]) -> Result<std::borrow::Cow<'_, [u8]>, &'static str> {
    if frame.len() >= 20 && frame[11] != 0 {
        normalize_authenticated_frame(
            frame,
            &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13],
            &[1, 2],
            match frame[9] {
                1..=3 => 1024 * 1024,
                4..=7 => 4 * 1024 * 1024,
                12 | 13 => 1024 * 1024 - 20,
                _ => MAX_BYTES,
            },
        )
        .map(std::borrow::Cow::Owned)
    } else {
        Ok(std::borrow::Cow::Borrowed(frame))
    }
}

#[cfg(test)]
mod canonical_view_bounds_tests {
    #[test]
    fn owning_entity_ceiling_is_checked_before_normalization_allocation() {
        for (codec, limit) in [
            (1, 1024 * 1024),
            (2, 1024 * 1024),
            (3, 1024 * 1024),
            (4, 4 * 1024 * 1024),
            (12, 1024 * 1024 - 20),
            (13, 1024 * 1024 - 20),
        ] {
            let mut frame = b"WORTA-C1".to_vec();
            frame.extend([1, codec, 1, 1]);
            frame.extend(((limit + 1) as u32).to_be_bytes());
            frame.extend(6u32.to_be_bytes());
            frame.extend([0x78, 0x9c, 0, 0, 0, 0]);
            assert_eq!(
                super::canonical_view(&frame).unwrap_err(),
                "compression_output_limit"
            );
        }
    }
}
