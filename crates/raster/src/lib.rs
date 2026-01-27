//! Raster: A Rust-based developer toolchain for tile-based execution.
//!
//! This is the main entry point for user applications. It re-exports the core
//! functionality from other Raster crates.
//!
//! This crate is `no_std` compatible when the `std` feature is disabled.

#![no_std]

extern crate alloc;

#[cfg(feature = "std")]
extern crate std;

pub use raster_core as core;
pub use raster_macros::{tile, sequence, main};

// Runtime is only available with std feature
#[cfg(feature = "std")]
pub use raster_runtime::{Executor, Tracer, FileTracer, NoOpTracer};

// Tile execution helper for native backend subprocess communication
#[cfg(feature = "std")]
mod exec_helper;

#[cfg(feature = "std")]
pub use exec_helper::{try_execute_tile_from_args, parse_main_input};

/// Emit a trace marker to stdout for tile execution tracing.
///
/// This function is called by the generated ABI wrapper from the `#[tile]` macro
/// to record input/output data for each tile execution. The trace is emitted as
/// a single line in JSON format for easy parsing.
///
/// # Format
///
/// ```text
/// RASTER_TRACE:{"tile":"<tile_id>","desc":<desc_or_null>,"input":"<base64>","output":"<base64>"}
/// ```
///
/// # Arguments
///
/// * `tile_id` - The name/identifier of the tile
/// * `description` - Optional human-readable description of the tile
/// * `input` - The raw input bytes (postcard-encoded)
/// * `output` - The raw output bytes (postcard-encoded)
#[cfg(all(feature = "std", not(target_arch = "riscv32")))]
pub fn emit_trace(tile_id: &str, description: Option<&str>, input: &[u8], output: &[u8]) {
    use base64::Engine;
    let input_b64 = base64::engine::general_purpose::STANDARD.encode(input);
    let output_b64 = base64::engine::general_purpose::STANDARD.encode(output);

    // Use simple JSON format for easy parsing
    // Escape the tile_id and description to handle special characters
    let desc_json = match description {
        Some(d) => {
            // Simple JSON string escaping
            let escaped = d
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\n', "\\n")
                .replace('\r', "\\r")
                .replace('\t', "\\t");
            alloc::format!("\"{}\"", escaped)
        }
        None => alloc::string::String::from("null"),
    };

    let tile_escaped = tile_id
        .replace('\\', "\\\\")
        .replace('"', "\\\"");

    std::println!(
        "RASTER_TRACE:{{\"tile\":\"{}\",\"desc\":{},\"input\":\"{}\",\"output\":\"{}\"}}",
        tile_escaped,
        desc_json,
        input_b64,
        output_b64
    );
}

/// Prelude module for convenient imports.
pub mod prelude {
    pub use crate::core::{
        tile::{TileId, TileMetadata, TileIdStatic, TileMetadataStatic},
        Result,
    };
    
    // These modules require std
    #[cfg(feature = "std")]
    pub use crate::core::{
        schema::{SequenceSchema, ControlFlow},
        manifest::Manifest,
        trace::{Trace, TraceEvent},
    };
    
    // Registry is only available with std and on platforms that support linkme
    #[cfg(all(feature = "std", not(target_arch = "riscv32")))]
    pub use crate::core::registry::{
        TileRegistration, iter_tiles, find_tile, find_tile_by_str, tile_count,
        SequenceRegistration, SequenceMetadataStatic, iter_sequences, find_sequence, sequence_count,
    };
    
    pub use crate::{tile, sequence};
    
    #[cfg(feature = "std")]
    pub use crate::{Executor, Tracer, FileTracer, NoOpTracer};
    
    // Tile execution helper for native backend
    #[cfg(all(feature = "std", not(target_arch = "riscv32")))]
    pub use crate::try_execute_tile_from_args;
}
