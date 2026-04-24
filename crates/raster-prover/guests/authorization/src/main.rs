use raster_core::authorization::{AuthorizationJournal, ManifestedInputs};
use raster_core::input::{InputManifestDocument, InputManifestEntry};
use risc0_zkvm::guest::env;
use risc0_zkvm::sha::{Impl as Risc0Sha256, Sha256 as _};
use std::collections::BTreeMap;

fn sha256_bytes(bytes: &[u8]) -> Vec<u8> {
    Risc0Sha256::hash_bytes(bytes).as_bytes().to_vec()
}

fn sha256_hex(bytes: &[u8]) -> Vec<u8> {
    let digest = sha256_bytes(bytes);
    let mut out = Vec::with_capacity(digest.len() * 2);
    for byte in digest {
        let hi = (byte >> 4) & 0x0f;
        let lo = byte & 0x0f;
        out.push(if hi < 10 { b'0' + hi } else { b'a' + (hi - 10) });
        out.push(if lo < 10 { b'0' + lo } else { b'a' + (lo - 10) });
    }
    out
}

fn normalize_hash_string(commitment: &str) -> Vec<u8> {
    commitment.trim().to_ascii_lowercase().into_bytes()
}

fn parse_manifest_commitment(entry: InputManifestEntry) -> Vec<u8> {
    normalize_hash_string(
        entry
            .as_sha256_commitment()
            .expect("Expected manifest entry to use {\"type\":\"sha256\",\"commitment\":\"...\"}"),
    )
}

fn parse_external_input_commitments(manifest_bytes: &[u8]) -> BTreeMap<String, Vec<u8>> {
    if manifest_bytes.is_empty() {
        return BTreeMap::new();
    }

    let document: InputManifestDocument = serde_json::from_slice(manifest_bytes)
        .expect("Failed to parse authorization manifest as JSON");

    document
        .into_iter()
        .map(|(name, value)| (name, parse_manifest_commitment(value)))
        .collect()
}

fn build_authorization_journal(input: &ManifestedInputs) -> AuthorizationJournal {
    let external_input_commitments = parse_external_input_commitments(&input.manifest_bytes);

    let external_inputs_commitments = input
        .external_inputs_bytes
        .iter()
        .map(|(name, bytes)| {
            let external_input_commitment =
                external_input_commitments.get(name).unwrap_or_else(|| {
                    panic!(
                    "External input '{}' is present in execution but missing from public manifest",
                    name
                )
                });
            let actual_commitment = sha256_hex(bytes);

            assert_eq!(
                &actual_commitment, external_input_commitment,
                "External input '{}' payload does not match the public manifest commitment",
                name
            );

            (name.clone(), external_input_commitment.clone())
        })
        .collect();

    AuthorizationJournal {
        external_inputs_commitments,
        manifest_commitment: sha256_bytes(&input.manifest_bytes),
    }
}

fn main() {
    let input: ManifestedInputs = env::read();
    let journal = build_authorization_journal(&input);
    env::commit(&journal);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_external_input_commitments_from_json_source() {
        let input = ManifestedInputs {
            manifest_bytes: br#"{
                "personal_data": {
                    "type": "sha256",
                    "commitment": "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
                }
            }"#
            .to_vec(),
            external_inputs_bytes: [("personal_data".to_string(), b"abc".to_vec())]
                .into_iter()
                .collect(),
        };

        let journal = build_authorization_journal(&input);

        assert_eq!(
            journal
                .external_inputs_commitments
                .get("personal_data"),
            Some(&b"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".to_vec())
        );
    }

    #[test]
    fn manifest_commitment_is_deterministic_for_same_source_bytes() {
        let payload = b"\n".to_vec();
        let payload_commitment = String::from_utf8(sha256_hex(&payload)).unwrap();
        let input = ManifestedInputs {
            manifest_bytes: format!(
                r#"{{"personal_data":{{"type":"sha256","commitment":"{}"}}}}"#,
                payload_commitment
            )
            .into_bytes(),
            external_inputs_bytes: [("personal_data".to_string(), payload)]
                .into_iter()
                .collect(),
        };

        let first = build_authorization_journal(&input);
        let second = build_authorization_journal(&input);

        assert_eq!(first.manifest_commitment, second.manifest_commitment);
    }

    #[test]
    fn ignores_manifested_external_inputs_that_are_not_witnessed() {
        let payload = b"abc".to_vec();
        let payload_commitment = String::from_utf8(sha256_hex(&payload)).unwrap();
        let input = ManifestedInputs {
            manifest_bytes: format!(
                r#"{{"personal_data":{{"type":"sha256","commitment":"{}"}},"unused_data":{{"type":"sha256","commitment":"deadbeef"}}}}"#,
                payload_commitment
            )
            .into_bytes(),
            external_inputs_bytes: [("personal_data".to_string(), payload)]
                .into_iter()
                .collect(),
        };

        let journal = build_authorization_journal(&input);

        assert!(journal
            .external_inputs_commitments
            .contains_key("personal_data"));
        assert!(!journal
            .external_inputs_commitments
            .contains_key("unused_data"));
    }
}
