use raster_core::authorization::{AuthorizationJournal, ManifestedInputs};
use raster_core::input::{InputManifestDocument, InputManifestEntry};
use risc0_zkvm::guest::env;
use risc0_zkvm::sha::{Impl as Risc0Sha256, Sha256 as _};
use std::collections::BTreeMap;

fn sha256_bytes(bytes: &[u8]) -> Vec<u8> {
    Risc0Sha256::hash_bytes(bytes).as_bytes().to_vec()
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
    // The journal authorizes every entry the public manifest declares. The
    // transition guest cross-checks actual execution against these
    // commitments (`checks::entrypoint`: the recorded entry-argument
    // combined root must be recomputable from exactly the names the CFS
    // declares), so unconsumed manifest entries are inert.
    AuthorizationJournal {
        external_inputs_commitments: parse_external_input_commitments(&input.manifest_bytes),
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
                    "encoding": "raster",
                    "commitment": "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
                }
            }"#
            .to_vec(),
        };

        let journal = build_authorization_journal(&input);

        assert_eq!(
            journal.external_inputs_commitments.get("personal_data"),
            Some(&b"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".to_vec())
        );
    }

    #[test]
    fn manifest_commitment_is_deterministic_for_same_source_bytes() {
        let payload_commitment = "01ba4719c80b6fe911b091a7c05124b64eeece964e09c058ef8f9805daca546b";
        let input = ManifestedInputs {
            manifest_bytes: format!(
                r#"{{"personal_data":{{"type":"sha256","commitment":"{}"}}}}"#,
                payload_commitment
            )
            .into_bytes(),
        };

        let first = build_authorization_journal(&input);
        let second = build_authorization_journal(&input);

        assert_eq!(first.manifest_commitment, second.manifest_commitment);
    }

    #[test]
    fn authorizes_every_manifest_entry() {
        let payload_commitment = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let input = ManifestedInputs {
            manifest_bytes: format!(
                r#"{{"personal_data":{{"type":"sha256","commitment":"{}"}},"other_data":{{"type":"sha256","commitment":"deadbeef"}}}}"#,
                payload_commitment
            )
            .into_bytes(),
        };

        let journal = build_authorization_journal(&input);

        assert!(journal
            .external_inputs_commitments
            .contains_key("personal_data"));
        assert!(journal
            .external_inputs_commitments
            .contains_key("other_data"));
    }
}
