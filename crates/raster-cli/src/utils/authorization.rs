use std::fs;
use std::path::Path;

use raster_core::authorization::ManifestedInputs;
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

pub fn build_manifested_inputs(input_manifest: Option<&str>) -> Result<ManifestedInputs> {
    Ok(ManifestedInputs {
        manifest_bytes: read_json_source(input_manifest, "authorization manifest")?,
    })
}
