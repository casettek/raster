//! Bounded native timing and exact-invocation capture. No execution history.
use crate::TraceRecorder;
use raster_core::{
    cfs::CfsCoordinates,
    trace::{ExecTarget, StepKind, StepRecord},
    Error, Result,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};
pub const CONFIG_ENV: &str = "RASTER_SELECTIVE_PROFILE_CONFIG";
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    pub timing: bool,
    pub top_tiles: usize,
    pub calls_per_tile: usize,
    pub max_bytes: u64,
    pub directory: PathBuf,
    pub selection: Option<Manifest>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Call {
    pub tile_id: String,
    pub exec_index: u64,
    pub coordinates: CfsCoordinates,
    pub native_duration_ns: u64,
    #[serde(default)]
    pub input_bytes: u64,
    #[serde(default)]
    pub output_bytes: u64,
    #[serde(default)]
    pub input_sha256: String,
    #[serde(default)]
    pub output_sha256: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub kind: String,
    pub complete: bool,
    pub step_count: u64,
    pub top_tiles: usize,
    pub calls_per_tile: usize,
    pub calls_recorded: BTreeMap<String, u64>,
    pub calls: Vec<Call>,
    pub payload_bytes: u64,
    #[serde(default)]
    pub binding: serde_json::Value,
}
impl Manifest {
    pub fn validate(&self) -> Result<()> {
        let fail =
            || Error::Other("invalid selective manifest coverage or invocation identity".into());
        if self.version != 1
            || !self.complete
            || self.top_tiles == 0
            || self.calls_per_tile == 0
            || self.step_count == 0
        {
            return Err(fail());
        }
        let mut seen = std::collections::BTreeSet::new();
        let mut selected = BTreeMap::<&str, usize>::new();
        let mut bytes = 0u64;
        for call in &self.calls {
            if call.exec_index == 0
                || call.exec_index > self.step_count
                || !seen.insert(call.exec_index)
                || self.calls_recorded.get(&call.tile_id).copied().unwrap_or(0) == 0
            {
                return Err(fail());
            }
            *selected.entry(&call.tile_id).or_default() += 1;
            bytes = bytes
                .checked_add(call.input_bytes)
                .and_then(|n| n.checked_add(call.output_bytes))
                .ok_or_else(fail)?;
        }
        if selected.len() > self.top_tiles
            || selected
                .iter()
                .any(|(id, n)| *n > self.calls_per_tile || *n as u64 > self.calls_recorded[*id])
            || bytes != self.payload_bytes
        {
            return Err(fail());
        }
        Ok(())
    }
}
#[derive(Default)]
struct State {
    config: Option<Config>,
    counts: BTreeMap<String, u64>,
    top: BTreeMap<String, Vec<Call>>,
    pending: Option<Call>,
    captured: Vec<Call>,
    steps: u64,
    bytes: u64,
}
static STATE: OnceLock<Mutex<State>> = OnceLock::new();
fn state() -> &'static Mutex<State> {
    STATE.get_or_init(|| Mutex::new(State::default()))
}
pub fn enabled() -> bool {
    STATE
        .get()
        .is_some_and(|s| s.lock().unwrap().config.is_some())
}
pub fn timing() -> bool {
    STATE
        .get()
        .is_some_and(|s| s.lock().unwrap().config.as_ref().is_some_and(|c| c.timing))
}
pub fn initialize() -> Result<bool> {
    let Some(path) = std::env::var_os(CONFIG_ENV) else {
        return Ok(false);
    };
    let config: Config =
        serde_json::from_slice(&std::fs::read(path)?).map_err(|e| Error::Other(e.to_string()))?;
    if config.top_tiles == 0 || config.calls_per_tile == 0 || config.max_bytes == 0 {
        return Err(Error::Other("invalid selective profile limits".into()));
    }
    if config.timing && !cfg!(feature = "profiling") {
        return Err(Error::Other(
            "native timing requires the profiling Cargo feature".into(),
        ));
    }
    std::fs::create_dir_all(config.directory.join("samples"))?;
    *state().lock().unwrap() = State {
        config: Some(config),
        ..Default::default()
    };
    Ok(true)
}
fn rank(a: &Call, b: &Call) -> std::cmp::Ordering {
    b.native_duration_ns
        .cmp(&a.native_duration_ns)
        .then(a.exec_index.cmp(&b.exec_index))
}
impl State {
    fn selected_calls(&self) -> Vec<Call> {
        let mut tiles: Vec<_> = self.top.values().cloned().collect();
        tiles.sort_by(|a, b| rank(&a[0], &b[0]));
        tiles
            .into_iter()
            .take(self.config.as_ref().unwrap().top_tiles)
            .flatten()
            .collect()
    }
    fn adjust_output_store(&mut self, ns: u64) {
        if let Some(c) = self.pending.as_mut() {
            c.native_duration_ns = c.native_duration_ns.saturating_add(ns);
        }
    }
    fn flush(&mut self) {
        if let Some(call) = self.pending.take() {
            let k = self.config.as_ref().unwrap().calls_per_tile;
            let calls = self.top.entry(call.tile_id.clone()).or_default();
            calls.push(call);
            calls.sort_by(rank);
            calls.truncate(k);
        }
    }
}
pub fn record_native(tile: &str, coords: &CfsCoordinates, ns: u64) {
    let mut s = state().lock().unwrap();
    let c = s
        .pending
        .as_mut()
        .expect("timing has no matching execution step");
    assert!(
        c.tile_id == tile && &c.coordinates == coords,
        "native timing invocation mismatch"
    );
    c.native_duration_ns = ns;
}
pub fn add_output_store(ns: u64) {
    state().lock().unwrap().adjust_output_store(ns);
}
pub fn on_step(step: &StepRecord, recorder: &TraceRecorder) -> Result<()> {
    let mut s = state().lock().unwrap();
    let config = s.config.clone().expect("selective config");
    if step.exec_index != s.steps + 1 {
        return Err(Error::Other("noncontiguous profiling index".into()));
    }
    s.steps = step.exec_index;
    let StepKind::Exec(exec) = &step.kind else {
        return Ok(());
    };
    let ExecTarget::Tile(tile) = &exec.target;
    *s.counts.entry(tile.clone()).or_default() += 1;
    if config.timing {
        s.flush();
        s.pending = Some(Call {
            tile_id: tile.clone(),
            exec_index: step.exec_index,
            coordinates: step.coordinates.clone(),
            native_duration_ns: 0,
            input_bytes: 0,
            output_bytes: 0,
            input_sha256: String::new(),
            output_sha256: String::new(),
        });
    } else if let Some(wanted) = config
        .selection
        .as_ref()
        .and_then(|m| m.calls.iter().find(|c| c.exec_index == step.exec_index))
    {
        if wanted.tile_id != *tile || wanted.coordinates != step.coordinates {
            return Err(Error::Other("selected invocation mapping mismatch".into()));
        }
        if s.captured.iter().any(|c| c.exec_index == step.exec_index) {
            return Err(Error::Other("duplicate selected invocation".into()));
        }
        let witness = recorder
            .step_witness_at(&step.coordinates)
            .ok_or_else(|| Error::Other("missing live selected witness".into()))?;
        let input = witness.input_data().unwrap_or_default();
        let output = witness.output_data().unwrap_or_default();
        let required = check_budget(
            s.bytes,
            input.len() as u64,
            output.len() as u64,
            config.max_bytes,
            fs2::available_space(&config.directory)?,
        )
        .map_err(|e| Error::Other(format!("tile {tile}, execution {}: {e}", step.exec_index)))?;
        let mut call = wanted.clone();
        call.input_bytes = input.len() as u64;
        call.output_bytes = output.len() as u64;
        call.input_sha256 = hash(&input);
        call.output_sha256 = hash(&output);
        std::fs::write(
            payload_path(&config.directory, call.exec_index, "input"),
            input,
        )?;
        std::fs::write(
            payload_path(&config.directory, call.exec_index, "output"),
            output,
        )?;
        s.bytes = required;
        s.captured.push(call);
    }
    Ok(())
}
fn check_budget(used: u64, input: u64, output: u64, limit: u64, free: u64) -> Result<u64> {
    let added = input
        .checked_add(output)
        .ok_or_else(|| Error::Other("sample byte overflow".into()))?;
    let required = used
        .checked_add(added)
        .ok_or_else(|| Error::Other("sample byte overflow".into()))?;
    if required > limit {
        return Err(Error::Other(format!(
            "sample budget exceeded: required {required}, limit {limit}"
        )));
    }
    let reserve = added
        .checked_add(2 * 1024 * 1024 * 1024)
        .ok_or_else(|| Error::Other("sample byte overflow".into()))?;
    if free < reserve {
        return Err(Error::Other(
            "insufficient sample disk space; 2 GiB reserve required".into(),
        ));
    }
    Ok(required)
}
pub fn payload_path(root: &Path, index: u64, side: &str) -> PathBuf {
    root.join("samples").join(format!("{index}-{side}.bin"))
}
pub fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub fn finish() -> Result<()> {
    let mut s = state().lock().unwrap();
    s.flush();
    let c = s.config.clone().unwrap();
    let calls = if c.timing {
        s.selected_calls()
    } else {
        let wanted = c
            .selection
            .as_ref()
            .ok_or_else(|| Error::Other("capture selection missing".into()))?;
        if s.steps != wanted.step_count
            || s.counts != wanted.calls_recorded
            || s.captured.len() != wanted.calls.len()
        {
            return Err(Error::Other(
                "capture coverage differs from selected run".into(),
            ));
        }
        s.captured.clone()
    };
    let manifest = Manifest {
        version: 1,
        kind: if c.timing {
            "native-selection"
        } else {
            "selected-capture"
        }
        .into(),
        complete: true,
        step_count: s.steps,
        top_tiles: c.top_tiles,
        calls_per_tile: c.calls_per_tile,
        calls_recorded: s.counts.clone(),
        calls,
        payload_bytes: s.bytes,
        binding: serde_json::Value::Null,
    };
    manifest.validate()?;
    std::fs::write(
        c.directory.join("profile-samples.pending.json"),
        serde_json::to_vec_pretty(&manifest).map_err(|e| Error::Other(e.to_string()))?,
    )?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn call(index: u64, ns: u64) -> Call {
        Call {
            tile_id: "recur".into(),
            exec_index: index,
            coordinates: Default::default(),
            native_duration_ns: ns,
            input_bytes: 0,
            output_bytes: 0,
            input_sha256: String::new(),
            output_sha256: String::new(),
        }
    }
    #[test]
    fn bounded_top_calls_and_ties() {
        let mut s = State {
            config: Some(Config {
                timing: true,
                top_tiles: 3,
                calls_per_tile: 1,
                max_bytes: 100,
                directory: ".".into(),
                selection: None,
            }),
            ..Default::default()
        };
        for i in 1..10001 {
            s.pending = Some(call(i, 10));
            s.flush();
        }
        assert_eq!(s.top["recur"].len(), 1);
        assert_eq!(s.top["recur"][0].exec_index, 1);
        s.pending = Some(call(10001, 11));
        s.flush();
        assert_eq!(s.top["recur"][0].exec_index, 10001);
    }
    #[test]
    fn budget_and_free_space_are_enforced() {
        assert_eq!(check_budget(1, 4, 5, 10, u64::MAX).unwrap(), 10);
        assert!(check_budget(1, 4, 5, 9, u64::MAX).is_err());
        assert!(check_budget(0, 1, 1, 100, 2 * 1024 * 1024 * 1024).is_err());
        assert!(check_budget(u64::MAX, 1, 0, u64::MAX, u64::MAX).is_err());
    }

    #[test]
    fn final_output_store_adjustment_changes_ranking_and_k_is_bounded() {
        let mut s = State {
            config: Some(Config {
                timing: true,
                top_tiles: 3,
                calls_per_tile: 2,
                max_bytes: 100,
                directory: ".".into(),
                selection: None,
            }),
            ..Default::default()
        };
        s.pending = Some(call(1, 10));
        s.flush();
        s.pending = Some(call(2, 8));
        s.adjust_output_store(5);
        s.flush();
        for i in 3..10003 {
            s.pending = Some(call(i, 10));
            s.flush();
        }
        assert_eq!(
            s.top["recur"]
                .iter()
                .map(|c| c.exec_index)
                .collect::<Vec<_>>(),
            vec![2, 1]
        );
    }
    #[test]
    fn invalid_capture_coverage_is_rejected() {
        let mut m = Manifest {
            version: 1,
            kind: "selected-capture".into(),
            complete: true,
            step_count: 100,
            top_tiles: 3,
            calls_per_tile: 1,
            calls_recorded: BTreeMap::from([("recur".into(), 100)]),
            calls: vec![call(1, 10)],
            payload_bytes: 0,
            binding: serde_json::Value::Null,
        };
        m.validate().unwrap();
        m.calls.push(call(1, 9));
        assert!(m.validate().is_err());
        m.calls.pop();
        m.calls[0].exec_index = 101;
        assert!(m.validate().is_err());
        m.calls[0].exec_index = 1;
        m.complete = false;
        assert!(m.validate().is_err());
    }
    #[test]
    fn n_tiles_are_ranked_by_their_single_slowest_call() {
        let mut s = State {
            config: Some(Config {
                timing: true,
                top_tiles: 3,
                calls_per_tile: 1,
                max_bytes: 100,
                directory: ".".into(),
                selection: None,
            }),
            ..Default::default()
        };
        for i in 1..=8 {
            let mut c = call(i, 10 + i);
            c.tile_id = format!("tile{}", i % 4);
            s.pending = Some(c);
            s.flush();
        }
        let selected = s.selected_calls();
        assert_eq!(selected.len(), 3);
        assert_eq!(
            selected.iter().map(|c| c.exec_index).collect::<Vec<_>>(),
            vec![8, 7, 6]
        );
        s.config.as_mut().unwrap().top_tiles = 2;
        s.config.as_mut().unwrap().calls_per_tile = 2;
        for i in 9..=12 {
            let mut c = call(i, 1);
            c.tile_id = format!("tile{}", i % 4);
            s.pending = Some(c);
            s.flush();
        }
        assert_eq!(
            s.selected_calls()
                .iter()
                .map(|c| c.exec_index)
                .collect::<Vec<_>>(),
            vec![8, 12, 7, 11]
        );
    }
}
