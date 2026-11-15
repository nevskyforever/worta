//! Non-CI timing evidence: cargo run --release --example c18_compression_benchmark -- <corpus.json> <output.json>
#[allow(dead_code)]
#[path = "../src/frame_compression.rs"]
mod frame_compression;
use serde_json::{json, Value};
use std::{env, fs, io::Write, time::Instant};
fn main() {
    let args: Vec<String> = env::args().collect();
    let corpus: Value = serde_json::from_slice(&fs::read(&args[1]).unwrap()).unwrap();
    let mut rows = vec![];
    for c in corpus.as_array().unwrap() {
        let input = c["canonical"].as_str().unwrap().as_bytes();
        let start = Instant::now();
        let mut z = flate2::write::ZlibEncoder::new(vec![], flate2::Compression::new(6));
        z.write_all(input).unwrap();
        let compressed = z.finish().unwrap();
        let compress = start.elapsed().as_secs_f64() * 1000.;
        let start = Instant::now();
        let opened = frame_compression::decompress_bounded(
            1,
            &compressed,
            input.len(),
            frame_compression::MAX_BYTES,
        )
        .unwrap();
        assert_eq!(opened, input);
        rows.push(json!({"name":c["name"],"input":input.len(),"output":compressed.len(),"compress_ms":compress,"decompress_ms":start.elapsed().as_secs_f64()*1000.,"rust_hex":compressed.iter().map(|b|format!("{b:02x}")).collect::<String>()}));
    }
    fs::write(&args[2], serde_json::to_vec(&rows).unwrap()).unwrap();
}
