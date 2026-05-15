//! Raster: A Rust-based developer toolchain for tile-based execution.
//!
//! This is the main entry point for user applications. It re-exports the core
//! functionality from other Raster crates.
//!
//! This crate is `no_std` compatible when the `std` feature is disabled.

#![no_std]

pub extern crate alloc;

#[cfg(feature = "std")]
extern crate std;

pub use raster_core as core;

pub mod input;
pub use input::{
    external, into_resolved_arg, resolve_external_value, resolve_typed_external_value,
    select_source, selector_path, typed_external, ArgKind, External, ExternalArg, ExternalArgInfo,
    ExternalRef, ExternalSelection, IntoResolvedArg, ListProofDirection, ListProofSibling,
    Merklized, ResolvedArg, SchemaField, SchemaNode, Selectable, SelectedPayload, SelectionProof,
    SelectionProofStep, SelectorPath, SelectorSegment, StructProofSibling, TypedExternal,
};

pub use raster_macros::{select, sequence, tile, Merklized, Selectable};

// Runtime is only available with std feature
#[cfg(feature = "std")]
pub use raster_runtime::{finish, init, init_with, publish_trace_event};

#[cfg(feature = "std")]
pub mod utils;

#[doc(hidden)]
pub mod __private {
    #[cfg(feature = "std")]
    pub fn emit_debug(args: core::fmt::Arguments<'_>) {
        std::println!("[debug] {}", args);
    }

    #[cfg(not(feature = "std"))]
    pub fn emit_debug(_: core::fmt::Arguments<'_>) {}
}

/// Canonical call primitive for invoking a tile inside a sequence.
///
/// `call!` is the explicit "step boundary" — use it instead of bare function calls
/// in sequences to enable reliable compiler call extraction, trace emission, and
/// CFS derivation.
///
/// # Usage
/// ```ignore
/// let greeting = call!(greet, name);
/// let result = call!(exclaim, greeting);
/// ```
///
/// On `std` + non-riscv32 targets, the underlying tile function's `#[tile]` wrapper
/// handles trace emission (`TraceEvent::Tile`). On `no_std` / riscv32 targets,
/// this expands to a plain function call with no overhead.
///
/// Bare function calls in sequences are soft-deprecated. Use `call!` for all
/// tile invocations to ensure the compiler can extract calls reliably.
#[macro_export]
macro_rules! call {
    ($tile:ident $(,)?) => {
        $tile()
    };
    ($tile:ident, $($args:expr),+ $(,)?) => {
        $tile($($args),+)
    };
}

/// Creates an external-input reference for explicit call-site bindings.
#[macro_export]
macro_rules! external {
    ($ty:ty, $name:literal) => {
        $crate::typed_external::<$ty>($name)
    };
    ($ty:ty, $name:expr) => {
        $crate::typed_external::<$ty>($name)
    };
    ($name:literal) => {
        $crate::external($name)
    };
    ($name:expr) => {
        $crate::external($name)
    };
}

/// Canonical call primitive for invoking a sub-sequence inside a sequence.
///
/// `call_seq!` is the explicit "sequence call boundary" — use it instead of bare
/// function calls when invoking another sequence from within a sequence.
///
/// # Usage
/// ```ignore
/// let result = call_seq!(wish_sequence, greeting);
/// ```
///
/// Semantically distinct from `call!`: invoking a sequence means the callee will
/// fire `SequenceStart` at entry and `SequenceEnd` on return (from the `#[sequence]`
/// macro on the callee side). `call_seq!` just invokes the function; the callee's
/// own `#[sequence]` wrapper handles sequence-level trace events.
///
/// On `no_std` / riscv32 targets, this expands to a plain function call.
///
/// Bare function calls to sequences are soft-deprecated. Use `call_seq!` for all
/// sequence invocations.
#[macro_export]
macro_rules! call_seq {
    ($seq:ident $(,)?) => {
        $seq()
    };
    ($seq:ident, $($args:expr),+ $(,)?) => {
        $seq($($args),+)
    };
}

/// Emits a Raster debug line that `cargo raster run --verbose` will surface.
///
/// Use this instead of `println!` in Raster user code, especially in `no_std`
/// tile/sequence crates where the standard print macros are unavailable.
#[macro_export]
macro_rules! debug {
    ($($arg:tt)*) => {{
        $crate::__private::emit_debug(::core::format_args!($($arg)*));
    }};
}

/// Prelude module for convenient imports.
pub mod prelude {
    pub use crate::core::{
        input::{External, ExternalRef},
        tile::{TileId, TileIdStatic, TileMetadata, TileMetadataStatic},
        Result,
    };

    // These modules require std
    #[cfg(feature = "std")]
    pub use crate::core::{
        manifest::Manifest,
        schema::{ControlFlow, SequenceSchema},
        trace::{FnCallRecord, FnInput, FnInputArg, FnOutput, TileExecRecord},
    };

    // Registry is only available with std and on platforms that support linkme
    #[cfg(all(feature = "std", not(target_arch = "riscv32")))]
    pub use crate::core::registry::{
        find_sequence, find_tile, find_tile_by_str, iter_sequences, iter_tiles, sequence_count,
        tile_count, SequenceMetadataStatic, SequenceRegistration, TileRegistration,
    };

    pub use crate::{
        call, call_seq, debug, external, select, sequence, tile, ArgKind, ExternalArg,
        ExternalArgInfo, ExternalSelection, IntoResolvedArg, ListProofDirection, ListProofSibling,
        Merklized, ResolvedArg, SchemaField, SchemaNode, Selectable, SelectedPayload,
        SelectionProof, SelectionProofStep, SelectorPath, SelectorSegment, StructProofSibling,
        TypedExternal,
    };

    // TODO: Re-enable once Executor/Tracer types are implemented
    // #[cfg(feature = "std")]
    // pub use crate::{Executor, Tracer, FileTracer, NoOpTracer};

    #[cfg(feature = "std")]
    pub use crate::{resolve_external_value, resolve_typed_external_value};

    #[cfg(feature = "std")]
    pub use crate::input::parse_program_input_value;
}
