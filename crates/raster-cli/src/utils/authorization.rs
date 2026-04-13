use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::Path;

use raster_core::trace::{ExternalInput, StepRecord};
use raster_core::transition::AuthorizationInput;
use raster_core::{Error, Result};

pub fn read_json_source(raw_input: Option<&str>, label: &str) -> Result<Vec<u8>> {
    let Some(raw_input) = raw_input else {
        return Ok(Vec::new());
    };

    if Path::new(raw_input).is_file() {
        fs::read(raw_input).map_err(|e| {
            Error::Other(format!(
                "Failed to read {} '{}': {}",
                label, raw_input, e
            ))
        })
    } else {
        Ok(raw_input.as_bytes().to_vec())
    }
}

pub fn collect_payload_witnesses(
    recorded_step_io: &HashMap<StepRecord, (Option<Vec<u8>>, Option<Vec<u8>>, ExternalInput)>,
) -> BTreeMap<String, Vec<u8>> {
    let mut payload_witnesses = BTreeMap::new();

    for (_step, (_input, _output, external_input)) in recorded_step_io {
        for meta in external_input.values() {
            if let Some(previous) =
                payload_witnesses.insert(meta.name.clone(), meta.payload_bytes.clone())
            {
                assert_eq!(
                    previous,
                    meta.payload_bytes,
                    "Conflicting payload bytes recorded for external input '{}'",
                    meta.name
                );
            }
        }
    }

    payload_witnesses
}

pub fn authorization_input(
    input_manifest: Option<&str>,
    payload_witnesses: BTreeMap<String, Vec<u8>>,
) -> Result<AuthorizationInput> {
    let manifest_bytes = if payload_witnesses.is_empty() {
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

    Ok(AuthorizationInput {
        manifest_bytes,
        payload_witnesses,
    })
}
