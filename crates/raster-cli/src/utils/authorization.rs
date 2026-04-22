use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::Path;

use raster_core::authorization::ManifestedInputs;
use raster_core::trace::{ExternalInput, StepRecord};
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

pub fn read_external_inputs(
    recorded_step_io: &HashMap<StepRecord, (Option<Vec<u8>>, Option<Vec<u8>>, ExternalInput)>,
) -> BTreeMap<String, Vec<u8>> {
    let mut external_inputs_bytes = BTreeMap::new();

    for (_step, (_input, _output, external_inputs)) in recorded_step_io {
        for binding in external_inputs.values() {
            if let Some(previous) =
                external_inputs_bytes.insert(binding.name.clone(), binding.bytes.clone())
            {
                assert_eq!(
                    previous, binding.bytes,
                    "Conflicting payload bytes recorded for external input '{}'",
                    binding.name
                );
            }
        }
    }

    external_inputs_bytes
}

pub fn build_manifested_inputs(
    input_manifest: Option<&str>,
    external_inputs_bytes: BTreeMap<String, Vec<u8>>,
) -> Result<ManifestedInputs> {
    let manifest_bytes = if external_inputs_bytes.is_empty() {
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
        external_inputs_bytes,
    })
}
