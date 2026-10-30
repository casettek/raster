//! Trace replayer for re-executing tiles with proof generation, and for
//! measuring recorded calls in the zkVM executor without proving.

use std::time::{Duration, Instant};

use raster_backend::{
    padded_proof_cycles, Backend, CompilationArtifact, ExecutionMode, SegmentStat,
};
use raster_compiler::tile::TileDiscovery;
use raster_compiler::Project;
use sha2::{Digest, Sha256};

use raster_core::draft::TileReplayJournal;
use raster_core::{Error, Result};

#[derive(Debug, Clone)]
pub struct ReplayResult {
    pub fn_name: String,

    pub receipt: Vec<u8>,

    pub image_id: Vec<u8>,
    pub input: Vec<u8>,
    pub output: Vec<u8>,
    pub replay_journal: TileReplayJournal,
}

/// One recorded call, executed in the zkVM executor without proving.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallMeasure {
    /// Guest user cycles, including the tile wrapper (decode, input hash, output encode).
    pub user_cycles: u64,
    pub segments: Vec<SegmentStat>,
    /// `Σ 2^po2` over `segments`: what proving this call would pay.
    pub padded_cycles: u64,
    /// Host wall time of the executor call, on this machine.
    pub exec_time: Duration,
}

/// A tile compiled (or loaded from the cache) once, for measuring any number of
/// its recorded calls. Never produces or accepts a receipt.
pub struct PreparedTileProfile<'a> {
    backend: &'a dyn Backend,
    artifact: Box<dyn CompilationArtifact>,
}

impl PreparedTileProfile<'_> {
    pub fn image_id(&self) -> String {
        self.artifact.artifact_id()
    }

    /// Execute one recorded call and measure it. The guest must reproduce the
    /// recording: its input commitment must be `sha256(input)` and its output the
    /// recorded output. A call that does not is an error, not a measurement.
    pub fn measure(&self, input: &[u8], expected_output: &[u8]) -> Result<CallMeasure> {
        let started = Instant::now();
        let result =
            self.backend
                .execute_tile(self.artifact.as_ref(), input, ExecutionMode::Estimate)?;
        let exec_time = started.elapsed();
        if result.receipt.is_some() {
            return Err(Error::Other(
                "Measurement unexpectedly generated a receipt".into(),
            ));
        }
        let journal = result
            .journal
            .ok_or_else(|| Error::Other("Executor returned no journal".into()))?;
        let journal: TileReplayJournal = raster_core::postcard::from_bytes(&journal)
            .map_err(|e| Error::Other(format!("Failed to decode replay journal: {e}")))?;
        let expected_commitment: [u8; 32] = Sha256::digest(input).into();
        if journal.input_commitment != expected_commitment {
            return Err(Error::Other("Replay input commitment mismatch".into()));
        }
        if journal.output_bytes != expected_output {
            return Err(Error::Other(format!(
                "Replay output mismatch (recorded {} bytes, guest {} bytes)",
                expected_output.len(),
                journal.output_bytes.len(),
            )));
        }
        let user_cycles = result
            .cycles
            .ok_or_else(|| Error::Other("Executor reported no cycles".into()))?;
        if result.segments.is_empty() {
            return Err(Error::Other("Executor reported no segments".into()));
        }
        Ok(CallMeasure {
            user_cycles,
            padded_cycles: padded_proof_cycles(&result.segments),
            segments: result.segments,
            exec_time,
        })
    }
}

/// Result of replaying a trace item.
///
/// Replays trace items on a backend with proof generation.
///
/// `TraceReplayer` takes trace items (typically from `AuditResult.trace_window`)
/// and re-executes them on a specified backend. This allows:
/// - Generating proofs for previously recorded executions
/// - Verifying that replay produces the same outputs
/// - Debugging execution discrepancies
pub struct Replayer<'a> {
    backend: &'a dyn Backend,
    project: &'a Project,
}

impl<'a> Replayer<'a> {
    /// Create a new replayer with the given backend and project context.
    ///
    /// # Arguments
    /// * `backend` - The backend to use for compilation and execution (e.g., Risc0Backend)
    /// * `project` - The project containing tile definitions for lookup
    pub fn new(backend: &'a dyn Backend, project: &'a Project) -> Self {
        Self { backend, project }
    }

    /// The project this replayer resolves tiles against.
    pub fn project(&self) -> &Project {
        self.project
    }

    /// Compile a tile and return its 32-byte image id, without executing it.
    /// Used to assemble the program's tile registry (see program-identity.md).
    pub fn tile_image_id(&self, tile_id: &str) -> Result<[u8; 32]> {
        let discovery = TileDiscovery::new(self.project);
        let tile = discovery.get(tile_id).ok_or_else(|| {
            Error::InvalidTileId(format!("Tile '{}' not found in project", tile_id))
        })?;
        let content_hash = tile.to_content_hash();
        let artifact = self
            .backend
            .compile_tile(&tile.to_metadata(), content_hash)?;
        let bytes = hex::decode(artifact.artifact_id())
            .map_err(|e| Error::Other(format!("Tile image id is not valid hex: {e}")))?;
        bytes
            .try_into()
            .map_err(|_| Error::Other("Tile image id must be 32 bytes".into()))
    }

    /// Compile or load a tile once, for measuring its recorded calls.
    pub fn prepare_profile(&self, tile_id: &str) -> Result<PreparedTileProfile<'a>> {
        let discovery = TileDiscovery::new(self.project);
        let tile = discovery.get(tile_id).ok_or_else(|| {
            Error::InvalidTileId(format!("Tile '{tile_id}' not found in project"))
        })?;
        let artifact = self
            .backend
            .compile_tile(&tile.to_metadata(), tile.to_content_hash())?;
        Ok(PreparedTileProfile {
            backend: self.backend,
            artifact,
        })
    }

    /// Replay a single trace item.
    ///
    /// This method:
    /// 1. Decodes the input data from base64
    /// 2. Looks up the tile by function name in the project
    /// 3. Compiles the tile using the backend
    /// 4. Executes the tile with the given execution mode
    /// 5. Optionally compares the output with the recorded output
    ///
    /// # Arguments
    /// * `item` - The trace item to replay
    /// * `mode` - Execution mode (Estimate or Prove)
    ///
    /// # Returns
    /// A `ReplayResult` containing the execution result and optional output comparison.
    pub fn replay(
        &self,
        tile_id: &str,
        input_bytes: &[u8],
        mode: ExecutionMode,
    ) -> Result<ReplayResult> {
        let discovery = TileDiscovery::new(self.project);

        let tile = discovery.get(tile_id).ok_or_else(|| {
            Error::InvalidTileId(format!("Tile '{}' not found in project", tile_id))
        })?;

        let content_hash = tile.to_content_hash();
        let artifact = self
            .backend
            .compile_tile(&tile.to_metadata(), content_hash)?;

        let image_id = artifact.artifact_id();
        let exec_result = self
            .backend
            .execute_tile(artifact.as_ref(), input_bytes, mode)?;

        let image_id = hex::decode(image_id).unwrap();
        let receipt_bytes = exec_result.receipt.clone().ok_or_else(|| {
            Error::Other("Replay requires a proof receipt to recover the replay journal".into())
        })?;
        let receipt: risc0_zkvm::Receipt = raster_core::postcard::from_bytes(&receipt_bytes)
            .map_err(|e| Error::Other(format!("Failed to decode replay receipt: {}", e)))?;
        let replay_journal: TileReplayJournal =
            raster_core::postcard::from_bytes(&receipt.journal.bytes)
                .map_err(|e| Error::Other(format!("Failed to decode replay journal: {}", e)))?;
        Ok(ReplayResult {
            fn_name: tile_id.to_string(),
            receipt: receipt_bytes,
            image_id,
            input: input_bytes.to_vec(),
            output: replay_journal.output_bytes.clone(),
            replay_journal,
        })
    }
}

#[cfg(test)]
mod measure_tests {
    use super::*;
    use raster_backend::{ArtifactStore, ResourceEstimate, TileExecutionResult};
    use raster_core::tile::TileMetadata;
    use std::any::Any;

    const INPUT: &[u8] = b"recorded input";
    const OUTPUT: &[u8] = b"recorded output";

    struct Artifact;
    impl CompilationArtifact for Artifact {
        fn id(&self) -> &str {
            "tile"
        }
        fn artifact_id(&self) -> String {
            "ab".repeat(32)
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
        fn as_any_mut(&mut self) -> &mut dyn Any {
            self
        }
    }

    /// Executes by returning a canned result; everything else is off limits
    /// to a prepared measurement.
    struct BackendStub {
        result: TileExecutionResult,
        abort: bool,
    }
    impl Backend for BackendStub {
        fn name(&self) -> &'static str {
            "stub"
        }
        fn compile_tile(
            &self,
            _: &TileMetadata,
            _: Option<String>,
        ) -> Result<Box<dyn CompilationArtifact>> {
            panic!("a prepared measurement must never compile")
        }
        fn execute_tile(
            &self,
            _: &dyn CompilationArtifact,
            input: &[u8],
            mode: ExecutionMode,
        ) -> Result<TileExecutionResult> {
            assert_eq!(input, INPUT);
            assert_eq!(mode, ExecutionMode::Estimate);
            if self.abort {
                Err(Error::Other("guest aborted".into()))
            } else {
                Ok(self.result.clone())
            }
        }
        fn artifact_store(&self) -> &dyn ArtifactStore {
            panic!("a prepared measurement has its artifact")
        }
        fn estimate_resources(&self, _: &TileMetadata) -> Result<ResourceEstimate> {
            panic!("a measurement executes, it does not estimate")
        }
        fn verify_receipt(&self, _: &dyn CompilationArtifact, _: &[u8]) -> Result<bool> {
            panic!("no proof is made")
        }
    }

    fn journal() -> TileReplayJournal {
        TileReplayJournal {
            input_commitment: Sha256::digest(INPUT).into(),
            output_bytes: OUTPUT.to_vec(),
            output_root: None,
            input_roots: Vec::new(),
            draft_transition: None,
            recur: None,
        }
    }

    fn stub() -> BackendStub {
        let mut result = TileExecutionResult::estimate(OUTPUT.to_vec(), 1_500_000);
        result.journal = Some(raster_core::postcard::to_allocvec(&journal()).unwrap());
        result.segments = vec![
            SegmentStat {
                po2: 20,
                cycles: 1_000_000,
            },
            SegmentStat {
                po2: 19,
                cycles: 500_000,
            },
        ];
        BackendStub {
            result,
            abort: false,
        }
    }

    fn measure(backend: &BackendStub, output: &[u8]) -> Result<CallMeasure> {
        PreparedTileProfile {
            backend,
            artifact: Box::new(Artifact),
        }
        .measure(INPUT, output)
    }

    fn error(backend: &BackendStub) -> String {
        measure(backend, OUTPUT).unwrap_err().to_string()
    }

    #[test]
    fn measures_cycles_segments_and_padding_without_recompiling() {
        let backend = stub();
        let first = measure(&backend, OUTPUT).unwrap();
        assert_eq!(first.user_cycles, 1_500_000);
        assert_eq!(first.segments.len(), 2);
        assert_eq!(first.padded_cycles, (1 << 20) + (1 << 19));
        // Not the single-segment estimate the result also carries.
        assert_ne!(Some(first.padded_cycles), backend.result.proof_cycles);
        // A second call reuses the artifact; the stub panics on any compile.
        assert_eq!(measure(&backend, OUTPUT).unwrap().user_cycles, 1_500_000);
    }

    #[test]
    fn rejects_a_guest_that_does_not_reproduce_the_recording() {
        let backend = stub();
        assert!(measure(&backend, b"different output")
            .unwrap_err()
            .to_string()
            .contains("output mismatch"));

        let mut backend = stub();
        let mut wrong = journal();
        wrong.input_commitment = [0; 32];
        backend.result.journal = Some(raster_core::postcard::to_allocvec(&wrong).unwrap());
        assert!(error(&backend).contains("input commitment mismatch"));
    }

    #[test]
    fn rejects_incomplete_or_proved_results_and_guest_failures() {
        let mut backend = stub();
        backend.result.journal = None;
        assert!(error(&backend).contains("no journal"));

        let mut backend = stub();
        backend.result.journal = Some(vec![0]);
        assert!(error(&backend).contains("decode"));

        let mut backend = stub();
        backend.result.cycles = None;
        assert!(error(&backend).contains("no cycles"));

        let mut backend = stub();
        backend.result.segments.clear();
        assert!(error(&backend).contains("no segments"));

        let mut backend = stub();
        backend.result.receipt = Some(vec![1]);
        assert!(error(&backend).contains("receipt"));

        let mut backend = stub();
        backend.abort = true;
        assert!(error(&backend).contains("guest aborted"));
    }
}
