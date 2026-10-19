use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use raster_core::input::{SelectorPath, SelectorSegment};
use raster_runtime::{
    encode_raster_value, enter_sequence_scope, entry_argument_spec, exit_sequence_scope,
    install_file_source_resolver, select_stored_value, start_program,
};

struct FixtureDir(PathBuf);

impl FixtureDir {
    fn new(label: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "raster-file-resolver-{label}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn stage(&self, name: &str, value: u64) -> (PathBuf, PathBuf) {
        let dir = self.0.join(name);
        fs::create_dir_all(&dir).unwrap();
        let (data, index, commitment) = encode_raster_value(&value).unwrap();
        fs::write(dir.join("payload.raster"), data).unwrap();
        fs::write(dir.join("payload.ridx"), index).unwrap();
        let input = dir.join("input.json");
        let manifest = dir.join("input_manifest.json");
        fs::write(
            &input,
            r#"{"payload":{"path":"payload.raster","index_path":"payload.ridx","load_preference":"read"}}"#,
        )
        .unwrap();
        fs::write(
            &manifest,
            serde_json::to_vec(&serde_json::json!({
                "payload": {
                    "type": "sha256",
                    "encoding": "raster",
                    "commitment": commitment
                }
            }))
            .unwrap(),
        )
        .unwrap();
        (input, manifest)
    }
}

impl Drop for FixtureDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn read_installed_payload() -> u64 {
    enter_sequence_scope("main");
    let result = (|| -> raster_core::Result<u64> {
        let binding = start_program(&[entry_argument_spec::<u64>("payload")])?;
        let selector = SelectorPath::new(vec![SelectorSegment::Field("payload".into())]);
        Ok(select_stored_value::<u64>(&binding.reference, &selector)?.value)
    })();
    exit_sequence_scope();
    result.unwrap()
}

#[test]
fn explicit_paths_switch_inputs_between_program_runs() {
    let fixture = FixtureDir::new("switch");
    let first = fixture.stage("first", 123);
    let second = fixture.stage("second", 456);

    // Reuse the same argument and relative filenames in different stage
    // directories, including returning to the first stage after a reset.
    for ((input, manifest), expected) in [(&first, 123), (&second, 456), (&first, 123)] {
        install_file_source_resolver(input, manifest).unwrap();
        assert_eq!(read_installed_payload(), expected);
    }
}

#[test]
fn invalid_documents_preserve_the_previous_input_context() {
    let fixture = FixtureDir::new("invalid");
    let (input, manifest) = fixture.stage("valid", 789);
    let invalid = fixture.0.join("invalid.json");
    fs::write(&invalid, "not JSON").unwrap();
    install_file_source_resolver(&input, &manifest).unwrap();
    assert_eq!(read_installed_payload(), 789);

    assert!(install_file_source_resolver(&invalid, &manifest).is_err());
    assert_eq!(read_installed_payload(), 789);
    assert!(install_file_source_resolver(&input, &invalid).is_err());
    assert_eq!(read_installed_payload(), 789);
}
