use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::Path;

use raster_core::authorization::ManifestedInputs;
use raster_core::draft::DraftTransitionWitness;
use raster_core::trace::{ExternalInput, FnInput, StepRecord};
use raster_core::transition::InternalStoreWitness;
use raster_core::{Error, Result};

pub fn read_json_source(raw_input: Option<&str>, label: &str) -> Result<Vec<u8>> {
    let Some(raw_input) = raw_input else {
        return Ok(Vec::new());
    };

    if Path::new(raw_input).is_file() {
        fs::read(raw_input)
            .map_err(|e| Error::Other(format!("Failed to read {} '{}': {}", label, raw_input, e)))
    } else {
        Ok(raw_input.as_bytes().to_vec())
    }
}

pub fn collect_external_input_commitments(
    recorded_step_io: &HashMap<
        StepRecord,
        (
            Option<Vec<u8>>,
            Option<Vec<u8>>,
            Option<FnInput>,
            Option<FnInput>,
            ExternalInput,
            Option<InternalStoreWitness>,
            Option<DraftTransitionWitness>,
        ),
    >,
) -> BTreeMap<String, Vec<u8>> {
    let mut commitments_by_name = BTreeMap::new();
    for (
        _step_record,
        (
            _recorded_input,
            _recorded_output,
            _input_source_witness,
            _sequence_scope_witness,
            external_input,
            _internal_store,
            _draft_transition,
        ),
    ) in recorded_step_io
    {
        for external_data in external_input.values() {
            match commitments_by_name.get(&external_data.name) {
                None => {
                    commitments_by_name
                        .insert(external_data.name.clone(), external_data.commitment.clone());
                }
                Some(existing_commitment) => {
                    assert_eq!(
                        existing_commitment, &external_data.commitment,
                        "Conflicting commitments recorded for external input '{}'",
                        external_data.name
                    );
                }
            }
        }
    }

    commitments_by_name
}

pub fn build_manifested_inputs(
    input_manifest: Option<&str>,
    external_inputs_commitments: BTreeMap<String, Vec<u8>>,
) -> Result<ManifestedInputs> {
    let manifest_bytes = if external_inputs_commitments.is_empty() {
        read_json_source(input_manifest, "authorization manifest")?
    } else {
        let Some(input_manifest) = input_manifest else {
            return Err(Error::Other(
                "Missing --input-manifest. External inputs now require a separate public manifest file."
                    .into(),
            ));
        };
        read_json_source(Some(input_manifest), "authorization manifest")?
    };

    Ok(ManifestedInputs {
        manifest_bytes,
        external_inputs_commitments,
    })
}
