//! RISC0 guest types and host utilities for iterative trace verification.
//!
//! This module provides:
//! - Shared types for guest input/output (TransitionInput, TransitionOutput)
//! - Host-side utilities for preparing inputs and verifying outputs
//! - The compiled transition guest ELF (when built)
//!
//! The types in this module are designed to be serialization-compatible with
//! the types used in the RISC0 guest program.

use serde::{Deserialize, Serialize};

use raster_core::trace::TraceItem;
use raster_core::fingerprint::FingerprintAccumulator;

use crate::trace::SerializableFrontier;

// Include the generated methods (ELF) from the build script.
// This may not exist if the build failed or RISC0_SKIP_BUILD was set.
#[cfg(not(feature = "skip-guest-build"))]
include!(concat!(env!("OUT_DIR"), "/methods.rs"));

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitionInput {
    pub trace_item: TraceItem,

    pub replay_image_id: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Transition {
    pub frontier: SerializableFrontier,
    pub fingerprint_acc: FingerprintAccumulator,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitTransitionState {
    pub init_frontier: SerializableFrontier,
    pub ref_fingerprint: FingerprintAccumulator,
    // TODO: Init Transition should verify proof of inclusion of reference fingerprint
    // pub ref_fingerprint_inclusion_proof: Vec<u8>,
    // TODO: Init Transition should contain reference to CFS
    // pub cfs: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TransitionState {
    Init(InitTransitionState),
    Next(Transition),
    Finished,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitionJournal {
    pub init_state: InitTransitionState,
    pub current_state: TransitionState,

    pub self_image_id: Vec<u8>,
}

