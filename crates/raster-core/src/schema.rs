//! Schema types (requires std feature).

use std::string::String;
use std::vec::Vec;
use serde::{Deserialize, Serialize};
use crate::tile::TileId;

/// A sequence schema describes the control flow and tile ordering.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SequenceSchema {
    pub name: String,
    pub version: String,
    pub tiles: Vec<TileId>,
    pub control_flow: ControlFlow,
}

/// Control flow description for a sequence.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ControlFlow {
    Linear { steps: Vec<TileId> },
    Conditional { branches: Vec<Branch> },
    Loop { body: Vec<TileId>, max_iterations: Option<u64> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Branch {
    pub condition: String,
    pub tiles: Vec<TileId>,
}
