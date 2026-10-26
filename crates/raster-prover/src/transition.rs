//! What the host hands the window's guests: each step's recorded witnesses
//! ([`StepIo`]), and the frame encoding every guest input is written in.

use std::collections::BTreeMap;

use raster_core::draft::DraftTransitionWitness;
use raster_core::input::SelectionWitness;
use raster_core::trace::FnInput;
use raster_core::transition::{StorageReadWitness, StorageWitness};

/// Everything the host recorded about one executed step, to be handed to the
/// guest as that step's witnesses.
#[derive(Debug, Clone, Default)]
pub struct StepIo {
    pub input_witness: Option<Vec<u8>>,
    pub output_witness: Option<Vec<u8>>,
    pub input_source_witness: Option<FnInput>,
    pub sequence_scope_witness: Option<FnInput>,
    pub storage_selection_witnesses: BTreeMap<String, SelectionWitness>,
    pub storage_witness: Option<StorageWitness>,
    pub draft_transition_witness: Option<DraftTransitionWitness>,
    /// For a `ProgramEnd` step: the read proof that the program output object
    /// is present in the current storage state, and the selection proof
    /// narrowing it to the returned value. `None` for every other step.
    pub program_output_read_witness: Option<StorageReadWitness>,
    pub program_output_selection_witness: Option<SelectionWitness>,
    /// For a recur-sequence iteration's `SequenceEnd` that carries state: the
    /// read proof of the object it returned as its state (D5b).
    pub returned_state_read_witness: Option<StorageReadWitness>,
}

/// Write one guest input as a frame: a `u32` byte length, then the
/// bytes. The guest reads it back with two bulk `read_slice` calls (its
/// `frame` module). Not `builder.write`: risc0's serde makes the guest fetch one
/// word per syscall, one per `u8` of a `Vec<u8>`, and on risc0 3.x every
/// syscall traps through the v1compat kernel. That was 68% of a step's cycles.
pub(crate) fn write_frame(builder: &mut risc0_zkvm::ExecutorEnvBuilder<'_>, bytes: &[u8]) {
    let len = u32::try_from(bytes.len()).expect("guest input frame exceeds u32::MAX bytes");
    builder.write_slice(&[len]);
    builder.write_slice(bytes);
}

/// [`write_frame`] of `value`'s postcard encoding.
pub(crate) fn write_postcard<T: serde::Serialize>(
    builder: &mut risc0_zkvm::ExecutorEnvBuilder<'_>,
    value: &T,
) {
    let bytes = postcard::to_allocvec(value).expect("guest input must encode with postcard");
    write_frame(builder, &bytes);
}
