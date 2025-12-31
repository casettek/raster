use serde::{Deserialize, Serialize};
use crate::tile::TileId;

/// A complete execution trace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trace {
    pub run_id: String,
    pub timestamp: u64,
    pub events: Vec<TraceEvent>,
}

/// A single event in an execution trace.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum TraceEvent {
    TileStart {
        tile_id: TileId,
        timestamp: u64,
        depth: u32,
    },
    TileEnd {
        tile_id: TileId,
        timestamp: u64,
        duration_ns: u64,
        cycles: Option<u64>,
    },
    SequenceStart {
        name: String,
        timestamp: u64,
    },
    SequenceEnd {
        name: String,
        timestamp: u64,
        duration_ns: u64,
    },
}
