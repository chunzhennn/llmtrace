//! Offline CDC prototype for P0 admission measurement only, never archive I/O.
use std::collections::BTreeMap;
use std::time::Instant;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub(super) fn measure(body: &[u8]) -> anyhow::Result<Value> {
    let mut corpus = Vec::new();
    for turn in 0..24 {
        let mut request = body.to_vec();
        request.extend_from_slice(format!("\nsynthetic-turn-{turn}\r\n").as_bytes());
        corpus.push(request);
    }
    let raw_bytes: usize = corpus.iter().map(Vec::len).sum();
    let mut report = json!({"raw_bytes":raw_bytes,"bodies":corpus.len(),"scope":"exact raw request bytes, including invalid-JSON suffix; no provider normalization",
        "cdc_scope":"offline prototype only; metadata estimates are not a deployed manifest/index/pack format", "zstd":{},"cdc":{}});
    for level in [1, 3] {
        let start = Instant::now();
        let compressed = corpus
            .iter()
            .map(|body| zstd::stream::encode_all(body.as_slice(), level))
            .collect::<Result<Vec<_>, _>>()?;
        let encode_ms = start.elapsed().as_secs_f64() * 1000.0;
        let start = Instant::now();
        for (encoded, original) in compressed.iter().zip(&corpus) {
            anyhow::ensure!(
                zstd::stream::decode_all(encoded.as_slice())? == *original,
                "zstd byte restoration mismatch"
            );
        }
        report["zstd"][level.to_string()] = json!({"compressed_bytes":compressed.iter().map(Vec::len).sum::<usize>(),"encode_ms":encode_ms,"restore_ms":start.elapsed().as_secs_f64()*1000.0});
    }
    for average in [4096_usize, 16_384, 65_536] {
        let start = Instant::now();
        let mut dictionary = BTreeMap::<[u8; 32], Vec<u8>>::new();
        let mut manifests = Vec::new();
        let mut references = 0;
        for body in &corpus {
            let mut chunks = Vec::new();
            let mut begin = 0;
            let mut rolling = 0_u64;
            for (index, byte) in body.iter().enumerate() {
                rolling = rolling
                    .rotate_left(1)
                    .wrapping_add((u64::from(*byte) + 1).wrapping_mul(0x517cc1b727220a95));
                let length = index + 1 - begin;
                if length >= average / 4
                    && (rolling & (average as u64 - 1) == 0 || length >= average * 4)
                {
                    chunks.push(&body[begin..index + 1]);
                    begin = index + 1;
                    rolling = 0;
                }
            }
            if begin < body.len() {
                chunks.push(&body[begin..]);
            }
            let mut manifest = Vec::new();
            for chunk in chunks {
                let hash: [u8; 32] = Sha256::digest(chunk).into();
                if let std::collections::btree_map::Entry::Vacant(entry) = dictionary.entry(hash) {
                    entry.insert(zstd::stream::encode_all(chunk, 1)?);
                }
                manifest.push(hash);
                references += 1;
            }
            manifests.push(manifest);
        }
        let encode_ms = start.elapsed().as_secs_f64() * 1000.0;
        let compressed_bytes: usize = dictionary.values().map(Vec::len).sum();
        let metadata_estimate = dictionary.len() * 64 + references * 40 + manifests.len() * 64;
        let start = Instant::now();
        for (manifest, original) in manifests.iter().zip(&corpus) {
            let mut restored = Vec::new();
            for hash in manifest {
                let compressed = dictionary
                    .get(hash)
                    .ok_or_else(|| anyhow::anyhow!("CDC benchmark lost a chunk"))?;
                restored.extend(zstd::stream::decode_all(compressed.as_slice())?);
            }
            anyhow::ensure!(restored == *original, "CDC byte restoration mismatch");
        }
        report["cdc"][average.to_string()] = json!({"unique_chunks":dictionary.len(),"references":references,"compressed_bytes":compressed_bytes,
            "metadata_estimate_bytes":metadata_estimate,"net_estimate_bytes":compressed_bytes+metadata_estimate,"encode_ms":encode_ms,"restore_ms":start.elapsed().as_secs_f64()*1000.0});
    }
    Ok(report)
}
