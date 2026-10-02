//! Checks that recorded step commitments (input, input source, output)
//! match the provided witnesses, and that tile steps carry a verified
//! replay proof whose output matches the recorded output witness.

use risc0_zkvm::guest::env;

use std::collections::BTreeMap;

use raster_core::authorization::AuthorizationJournal;
use raster_core::draft::TileReplayJournal;
use raster_core::input::{payload_structural_root, SelectionWitness};
use raster_core::trace::{FnInput, FnInputValue, StepRecord};

use crate::merkle_tree::sha256_bytes;

pub fn input_source_commitment(input: &FnInput) -> Vec<u8> {
    sha256_bytes(&input.source_witness_bytes())
}

pub fn verify_io_witness(
    step_record: &StepRecord,
    input_witness: Option<&Vec<u8>>,
    output_witness: Option<&Vec<u8>>,
) {
    let commitment_for = |bytes: Option<&Vec<u8>>| -> Vec<u8> {
        bytes.map(|bytes| sha256_bytes(bytes)).unwrap_or_default()
    };

    if let Some(input_commitment) = step_record.input_commitment() {
        assert_eq!(
            input_commitment,
            &commitment_for(input_witness),
            "Step input commitment does not match recorded input bytes",
        );
    }
    if let Some(output_commitment) = step_record.output_commitment() {
        if step_record.is_execution_step() {
            return;
        }
        assert_eq!(
            output_commitment,
            &commitment_for(output_witness),
            "Step output commitment does not match recorded output bytes",
        );
    }
}

pub fn verify_authorization_journal(
    authorization_journal: &AuthorizationJournal,
    authorization_image_id: &[u8],
) -> bool {
    let image_id_digest = risc0_zkvm::sha::Digest::try_from(authorization_image_id)
        .expect("authorization image id must be 32 bytes");

    let journal_bytes = risc0_zkvm::serde::to_vec(authorization_journal)
        .expect("Failed to serialize authorization journal");

    env::verify(image_id_digest, &journal_bytes).is_ok()
}

pub fn verify_step_record(
    step_record: &StepRecord,
    expected_image_id: Option<&[u8; 32]>,
    replay_journal: Option<&raster_core::draft::TileReplayJournal>,

    input_witness_bytes: Option<&Vec<u8>>,
    output_witness_bytes: Option<&Vec<u8>>,
    input_source_witness: Option<&FnInput>,
    storage_selection_witnesses: &BTreeMap<String, SelectionWitness>,
) {
    verify_io_witness(step_record, input_witness_bytes, output_witness_bytes);
    if let Some(expected_input_source_commitment) = step_record.input_source_commitment() {
        let input_source_witness =
            input_source_witness.expect("Step input source witness is missing");
        assert_eq!(
            expected_input_source_commitment,
            &input_source_commitment(input_source_witness),
            "Step input source witness does not match recorded source commitment",
        );
    } else {
        // `ProgramStart`, `ProgramEnd` and `SequenceEnd` all report `None`
        // here. None of them binds a step input, so a witness supplied for one
        // is unbound by construction — nothing in the record commits to it.
        assert!(
            input_source_witness.is_none(),
            "Step {:?} declares no input source commitment, so it must not carry an input \
             source witness",
            step_record.coordinates,
        );
    }

    if step_record.requires_replay_proof() {
        // The expected image id comes from the program's committed tile
        // registry (resolved by the caller from the step's tile id), never
        // from a host-supplied field — this is what binds the replayed binary
        // to the tile the step claims to run. See program-identity.md.
        let expected_image_id =
            expected_image_id.expect("tile step must resolve a registry image id");
        let replay_journal =
            replay_journal.expect("tile execution should provide a replay journal witness");
        let replay_image_id_digest =
            risc0_zkvm::sha::Digest::try_from(expected_image_id.as_slice())
                .expect("image_id must be 32 bytes");
        let replay_journal_bytes = postcard::to_allocvec(replay_journal)
            .expect("Failed to encode replay journal for receipt verification");
        env::verify(replay_image_id_digest, &replay_journal_bytes)
            .expect("Failed to verify trace replay image id");
        let output_bytes = output_witness_bytes.map(Vec::as_slice).unwrap_or(&[]);
        assert_eq!(
            replay_journal.output_bytes.as_slice(),
            output_bytes,
            "Replay journal output bytes do not match recorded tile output witness",
        );
        // Bind the replay to the *recorded* input: the tile guest committed
        // `sha256(its input)`, which must equal the hash of the recorded input
        // witness. Without this the proof shows only that `output` is some
        // output of the binary, not that `binary(recorded input) = output`.
        let input_bytes = input_witness_bytes.map(Vec::as_slice).unwrap_or(&[]);
        assert_eq!(
            replay_journal.input_commitment.as_slice(),
            sha256_bytes(input_bytes).as_slice(),
            "Replay journal input commitment does not match recorded tile input witness",
        );
        verify_output_root(step_record, replay_journal);
        verify_input_roots(
            step_record,
            replay_journal,
            input_source_witness,
            storage_selection_witnesses,
        );
    }
}

/// The tile's write is the object its replay returned
/// (`tile-io-structural-roots` step 2). `output_root` is the raster root of the
/// returned value, computed by the replay, so it is `Some` exactly when the
/// tile publishes an output; the recorded commitment must then be it. A tile
/// that publishes none — a recur iteration returning its site's draft or state
/// (D3) — must record an empty commitment, and `checks::store` requires a step
/// with an empty commitment to write nothing: together, a tile writes iff its
/// replay produced an output, and writes exactly that.
pub(crate) fn verify_output_root(step_record: &StepRecord, replay_journal: &TileReplayJournal) {
    let recorded = step_record
        .output_commitment()
        .expect("a tile step records an output commitment");
    match replay_journal.output_root {
        Some(root) => assert_eq!(
            recorded.as_slice(),
            root.as_slice(),
            "Tile step's output commitment is not the root of the value its replay returned",
        ),
        None => assert!(
            recorded.is_empty(),
            "Tile step whose replay returned no output must record no output commitment",
        ),
    }
}

/// Every storage-bound argument is the value the tile ran on
/// (`tile-io-structural-roots` step 2). Its selection witness folds to the
/// stored object from the selected payload's root (`checks::store`); the
/// replay committed the root of each decoded argument; this requires the two
/// to be the same root, so the bytes the selection proves and the value the
/// binary executed are the same value.
pub(crate) fn verify_input_roots(
    step_record: &StepRecord,
    replay_journal: &TileReplayJournal,
    input_source_witness: Option<&FnInput>,
    storage_selection_witnesses: &BTreeMap<String, SelectionWitness>,
) {
    let Some(input_source_witness) = input_source_witness else {
        assert!(
            replay_journal.input_roots.is_empty(),
            "Tile step with no recorded input must replay no arguments",
        );
        return;
    };
    let values = input_source_witness.values();
    assert_eq!(
        replay_journal.input_roots.len(),
        values.len(),
        "Tile step {:?} replayed {} arguments but records {}",
        step_record.coordinates,
        replay_journal.input_roots.len(),
        values.len(),
    );
    for (index, value) in values.iter().enumerate() {
        if !matches!(value, FnInputValue::StorageBinding) {
            continue;
        }
        let name = input_source_witness
            .args()
            .get(index)
            .map(|arg| arg.name.as_str())
            .expect("a recorded value has an argument");
        let binding = input_source_witness.storage().get(name).unwrap_or_else(|| {
            panic!(
                "Tile step {:?} is missing the storage binding of argument '{}'",
                step_record.coordinates, name
            )
        });
        if binding.selection.selected_len == 0 {
            continue;
        }
        let witness = storage_selection_witnesses.get(name).unwrap_or_else(|| {
            panic!(
                "Tile step {:?} is missing the selection witness of argument '{}'",
                step_record.coordinates, name
            )
        });
        let selected_root = match witness.selected_root {
            Some(root) => Some(root),
            None => payload_structural_root(&witness.bytes),
        };
        let replayed = replay_journal.input_roots[index].unwrap_or_else(|| {
            panic!(
                "Tile step {:?} reads argument '{}' from storage, but its replay committed no root for it",
                step_record.coordinates, name
            )
        });
        // A fallible call's result is stored whole, `Ok(v)`, and the sequence
        // that consumes it with `?` hands the next tile `v`: the selection
        // proves the `Result`, the tile decoded the value inside it. `v`'s
        // payload is a slice of the proven bytes, so its root is proven too.
        let ok_inner_root = witness
            .selected_root
            .is_none()
            .then(|| ok_payload(&witness.bytes))
            .flatten()
            .and_then(payload_structural_root);
        assert!(
            selected_root == Some(replayed) || ok_inner_root == Some(replayed),
            "Tile step {:?}'s argument '{}' is not the value its selection proves",
            step_record.coordinates,
            name,
        );
    }
}

/// The value inside an `Ok(..)` payload — an enum newtype (`0x06 ‖ variant
/// length ‖ variant ‖ value length ‖ value`) whose variant is `Ok` — or
/// `None` for any other payload.
fn ok_payload(bytes: &[u8]) -> Option<&[u8]> {
    let read_u64 = |at: usize| -> Option<usize> {
        usize::try_from(u64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?)).ok()
    };
    if *bytes.first()? != 0x06 {
        return None;
    }
    let variant_len = read_u64(1)?;
    let variant = bytes.get(9..9 + variant_len)?;
    if variant != b"Ok" {
        return None;
    }
    let value_len = read_u64(9 + variant_len)?;
    let start = 9 + variant_len + 8;
    let value = bytes.get(start..start.checked_add(value_len)?)?;
    (start + value_len == bytes.len()).then_some(value)
}
