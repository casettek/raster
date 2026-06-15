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
    auth_ref_result_trace, auth_ref_trace, finalize, into_auth_ref, into_auth_value,
    materialize_auth_result, materialize_auth_return, new_draft, resolve_external_value,
    resolve_internal_ok_value, resolve_internal_value, resolve_typed_external_value,
    run_recur_list, run_recur_list_state, run_recur_list_with_state, select_source, selector_path,
    typed_external, typed_internal, typed_internal_with_resolver, typed_selector_path, Anchor,
    AuthRef, AuthRefTrace, AuthValue, Draft, DraftAppendField, DraftSetField, ExternalRef,
    ExternalSelection, ExternalValue, InternalRef, InternalValue, IntoAuthRef, IntoAuthValue,
    IntoRecurControl, ListProofDirection, ListProofSibling, Op, RecurControl, RecurInput,
    RecurOutput, RecurState, Schema, SchemaField, SchemaFieldMode, SchemaNode, SelectSource,
    Selectable, SelectedPayload, SelectionProof, SelectionProofStep, SelectorPath, SelectorSegment,
    TypedExternalBinding, TypedInternalBinding, TypedSelectorPath,
};

#[cfg(feature = "std")]
pub use input::{encode_raster_value, store_internal_value, write_raster_files};

pub use raster_macros::{select, sequence, tile, Selectable};

/// User-facing execution result contract for fallible tiles and sequences.
pub mod exec {
    pub type Result<T> = core::result::Result<T, crate::alloc::string::String>;
}

/// Internal runtime/protocol surface.
///
/// Raster-internal execution failures remain available here so hosts and
/// executor layers can distinguish infrastructure/runtime failures from
/// user-defined terminal outcomes.
pub mod runtime {
    pub use crate::core::error::{Error, Result};
}

// Runtime helpers are only available with std feature.
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

    #[doc(hidden)]
    pub struct SequenceScopeGuard;

    impl SequenceScopeGuard {
        pub fn enter(sequence_id: &str) -> Self {
            #[cfg(feature = "std")]
            {
                raster_runtime::enter_sequence_scope(sequence_id);
            }

            #[cfg(not(feature = "std"))]
            {
                let _ = sequence_id;
            }

            Self
        }
    }

    impl Drop for SequenceScopeGuard {
        fn drop(&mut self) {
            #[cfg(feature = "std")]
            {
                raster_runtime::exit_sequence_scope();
            }
        }
    }

    #[cfg(feature = "std")]
    pub fn bind_infallible_call<T>(result: T) -> crate::AuthRef<T>
    where
        T: serde::Serialize + serde::de::DeserializeOwned + 'static,
    {
        let reference = crate::store_internal_value(&result).unwrap_or_else(|error| {
            panic!("Failed to store tile output in internal storage: {}", error)
        });
        crate::into_auth_ref::<T, _>(crate::typed_internal::<T>(reference))
    }

    #[cfg(not(feature = "std"))]
    pub fn bind_infallible_call<T>(_: T) -> crate::AuthRef<T> {
        panic!("Sequence call bindings require the `std` feature")
    }

    #[cfg(feature = "std")]
    pub fn bind_fallible_call<T>(
        result: core::result::Result<T, crate::alloc::string::String>,
    ) -> core::result::Result<crate::AuthRef<T>, crate::alloc::string::String>
    where
        T: serde::Serialize + serde::de::DeserializeOwned + 'static,
    {
        let reference = crate::store_internal_value(&result).unwrap_or_else(|error| {
            panic!("Failed to store tile output in internal storage: {}", error)
        });
        match result {
            Ok(_) => Ok(crate::into_auth_ref::<T, _>(
                crate::typed_internal_with_resolver::<T>(
                    reference,
                    crate::resolve_internal_ok_value::<T>,
                ),
            )),
            Err(error) => Err(error),
        }
    }

    #[cfg(not(feature = "std"))]
    pub fn bind_fallible_call<T>(
        _: core::result::Result<T, crate::alloc::string::String>,
    ) -> core::result::Result<crate::AuthRef<T>, crate::alloc::string::String> {
        panic!("Sequence call bindings require the `std` feature")
    }

    pub trait TileCallBinding<Return> {
        type Output;

        fn bind(result: Return) -> Self::Output;
    }

    pub fn bind_tile_call<Marker, Return>(
        result: Return,
    ) -> <Marker as TileCallBinding<Return>>::Output
    where
        Marker: TileCallBinding<Return>,
    {
        Marker::bind(result)
    }

    pub trait TryTileCallBinding<Return> {
        type Output;

        fn bind(result: Return) -> Self::Output;
    }

    pub fn bind_tile_try_call<Marker, Return>(
        result: Return,
    ) -> <Marker as TryTileCallBinding<Return>>::Output
    where
        Marker: TryTileCallBinding<Return>,
    {
        Marker::bind(result)
    }
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
/// let checked = call!(maybe_echo_name, result)?;
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

/// Creates a typed external-input reference for explicit call-site bindings.
#[macro_export]
macro_rules! external {
    ($ty:ty, $name:literal) => {
        $crate::typed_external::<$ty>($name)
    };
    ($ty:ty, $name:expr) => {
        $crate::typed_external::<$ty>($name)
    };
}

/// Creates a typed internal-store reference for explicit call-site bindings.
#[macro_export]
macro_rules! internal {
    ($ty:ty, $reference:expr) => {
        $crate::typed_internal::<$ty>($reference)
    };
}

#[macro_export]
macro_rules! new {
    ($ty:ty) => {
        $crate::new_draft::<$ty>()
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
/// let checked = call_seq!(verify_sequence, result)?;
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

/// Canonical recursive list-call primitive for invoking a recur tile inside a sequence.
///
/// `call_recur!` is only valid inside `#[sequence]` functions, where the sequence macro
/// rewrites it into a hidden driver that iterates a selectable list source, threads a
/// `Draft<_>` through each item, and finalizes the draft once the run ends.
#[macro_export]
macro_rules! call_recur {
    ($($tt:tt)*) => {
        compile_error!("call_recur! can only be used inside #[sequence] functions")
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
///
/// Importing `raster::prelude::*` makes bare `Result<T>` refer to Raster's
/// terminal execution result type. Raster runtime failures remain available
/// separately under `raster::runtime`.
pub mod prelude {
    pub use crate::core::{
        input::ExternalRef,
        tile::{TileId, TileIdStatic, TileMetadata, TileMetadataStatic},
    };

    // These modules require std
    #[cfg(feature = "std")]
    pub use crate::core::{
        manifest::Manifest,
        schema::{ControlFlow, SequenceSchema},
        trace::{FnCallRecord, FnInput, FnInputArg, FnOutput, TileExecRecord},
    };

    pub use crate::exec::Result;
    pub use crate::{
        call, call_recur, call_seq, debug, external, finalize, internal, into_auth_ref,
        materialize_auth_result, materialize_auth_return, new, select, sequence, tile, Anchor,
        AuthRef, AuthValue, Draft, ExternalSelection, ExternalValue, InternalRef, InternalValue,
        IntoAuthRef, IntoAuthValue, ListProofDirection, ListProofSibling, Op, RecurControl,
        RecurInput, RecurOutput, RecurState, Schema, SchemaField, SchemaFieldMode, SchemaNode,
        SelectSource, Selectable, SelectedPayload, SelectionProof, SelectionProofStep,
        SelectorPath, SelectorSegment, TypedExternalBinding, TypedInternalBinding,
        TypedSelectorPath,
    };

    // TODO: Re-enable once Executor/Tracer types are implemented
    // #[cfg(feature = "std")]
    // pub use crate::{Executor, Tracer, FileTracer, NoOpTracer};

    #[cfg(feature = "std")]
    pub use crate::{
        resolve_external_value, resolve_internal_value, resolve_typed_external_value,
        store_internal_value,
    };
}
