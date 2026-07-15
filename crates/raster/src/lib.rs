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
pub use raster_core::draft;

pub mod input;
pub use input::{
    auth_ref_result_trace, auth_ref_trace, draft_replay_handle, draft_replay_transition, finalize,
    into_auth_ref, into_auth_value, into_draft, materialize_auth_result, materialize_auth_return,
    new_draft, raster_trace_payload, resolve_internal_ok_value, resolve_internal_value,
    restore_draft_from_replay_handle, run_recur_list, run_recur_list_state,
    run_recur_list_with_state, run_recur_sequence_list, run_recur_sequence_list_state,
    run_recur_sequence_list_with_state, select_source, select_stored_internal_value,
    selector_path, serialize_draft_replay_handle, serialize_draft_trace, typed_internal,
    typed_internal_with_resolver, typed_selector_path, Anchor, AuthRef, AuthRefTrace, AuthValue,
    Draft, DraftAppendField, DraftSetField, InternalRef, InternalValue, IntoAuthRef, IntoAuthValue,
    IntoDraft, IntoRecurControl, ListProofDirection, ListProofSibling, Op, RecurControl,
    RecurInput, RecurOutput, RecurSequenceInput, RecurSequenceOutput, RecurSequenceState,
    RecurState, Schema, SchemaField, SchemaFieldMode, SchemaNode, SelectSource, Selectable,
    SelectedPayload, SelectionCommitment, SelectionProof, SelectionProofStep, SelectionWitness,
    SelectorPath, SelectorSegment, TypedInternalBinding, TypedSelectorPath,
};

#[cfg(feature = "std")]
pub use input::{
    begin_draft_transition_capture, encode_raster_value, finish_draft_transition_capture,
    postcard_structural_commitment, store_internal_value, write_raster_files,
};

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
pub use raster_runtime::{
    bind_entry_arguments, entry_argument_spec, finish, init, init_with, publish_trace_event,
    EntryArgumentsBinding, EntryArgumentSpec,
};

#[cfg(feature = "std")]
pub mod utils;

#[doc(hidden)]
pub mod __private {
    #[cfg(feature = "std")]
    pub fn emit_output(args: core::fmt::Arguments<'_>) {
        std::println!("[output] {}", args);
    }

    #[cfg(not(feature = "std"))]
    pub fn emit_output(_: core::fmt::Arguments<'_>) {}

    #[cfg(feature = "std")]
    pub type ProfileInstant = std::time::Instant;

    #[cfg(feature = "std")]
    pub fn profile_now() -> ProfileInstant {
        std::time::Instant::now()
    }

    #[cfg(not(feature = "std"))]
    #[derive(Clone, Copy)]
    pub struct ProfileInstant;

    #[cfg(not(feature = "std"))]
    impl ProfileInstant {
        pub fn elapsed(&self) -> core::time::Duration {
            core::time::Duration::from_secs(0)
        }
    }

    #[cfg(not(feature = "std"))]
    pub fn profile_now() -> ProfileInstant {
        ProfileInstant
    }

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

    #[doc(hidden)]
    pub struct TileExecutionScopeGuard {
        #[cfg(feature = "std")]
        inner: Option<raster_runtime::TileExecutionScopeGuard>,
    }

    impl TileExecutionScopeGuard {
        pub fn enter() -> Self {
            #[cfg(feature = "std")]
            {
                return Self {
                    inner: Some(
                        raster_runtime::TileExecutionScopeGuard::enter().unwrap_or_else(|error| {
                            panic!("Failed to enter tile execution scope: {}", error)
                        }),
                    ),
                };
            }

            #[cfg(not(feature = "std"))]
            {
                Self {}
            }
        }

        pub fn coordinates(&self) -> &crate::core::cfs::CfsCoordinates {
            #[cfg(feature = "std")]
            {
                return self
                    .inner
                    .as_ref()
                    .expect("Tile execution scope guard is missing runtime state")
                    .coordinates();
            }

            #[cfg(not(feature = "std"))]
            {
                panic!("Tile execution coordinates require the `std` feature")
            }
        }
    }

    impl Drop for TileExecutionScopeGuard {
        fn drop(&mut self) {
            #[cfg(feature = "std")]
            {
                self.inner.take();
            }
        }
    }

    #[cfg(feature = "std")]
    pub fn begin_sequence_profile(sequence_id: &str) {
        #[cfg(feature = "profiling")]
        raster_runtime::begin_sequence_profile(sequence_id);
        #[cfg(not(feature = "profiling"))]
        let _ = sequence_id;
    }

    #[cfg(not(feature = "std"))]
    pub fn begin_sequence_profile(_: &str) {}

    #[cfg(feature = "std")]
    pub fn finish_sequence_profile(
        sequence_id: &str,
        total_duration_ns: u64,
        scope_enter_ns: u64,
        input_trace_ns: u64,
        start_event_publish_ns: u64,
        output_trace_ns: u64,
        end_event_publish_ns: u64,
    ) {
        #[cfg(feature = "profiling")]
        raster_runtime::finish_sequence_profile(
            sequence_id,
            total_duration_ns,
            raster_runtime::SequenceProfileSelfBreakdown {
                body_self_ns: 0,
                scope_enter_ns,
                synthetic_coordinate_alloc_ns: 0,
                input_trace_ns,
                start_event_publish_ns,
                output_trace_ns,
                end_event_publish_ns,
                other_wrapper_ns: 0,
            },
        );
        #[cfg(not(feature = "profiling"))]
        let _ = (
            sequence_id,
            total_duration_ns,
            scope_enter_ns,
            input_trace_ns,
            start_event_publish_ns,
            output_trace_ns,
            end_event_publish_ns,
        );
    }

    #[cfg(not(feature = "std"))]
    pub fn finish_sequence_profile(_: &str, _: u64, _: u64, _: u64, _: u64, _: u64, _: u64) {}

    #[cfg(feature = "std")]
    pub fn record_tile_profile(
        tile_id: &str,
        coordinates: crate::core::cfs::CfsCoordinates,
        total_duration_ns: u64,
        user_duration_ns: u64,
        external_input_resolve_ns: u64,
        internal_input_resolve_ns: u64,
        trace_serialize_ns: u64,
        draft_capture_ns: u64,
        scope_enter_ns: u64,
        output_record_build_ns: u64,
        trace_event_publish_ns: u64,
        output_coordinate_publish_ns: u64,
    ) {
        #[cfg(feature = "profiling")]
        raster_runtime::record_tile_profile(
            tile_id,
            coordinates,
            total_duration_ns,
            user_duration_ns,
            raster_runtime::TileProfileOverheadBreakdown {
                external_input_resolve_ns,
                internal_input_resolve_ns,
                output_store_ns: 0,
                trace_serialize_ns,
                draft_capture_ns,
                scope_enter_ns,
                output_record_build_ns,
                trace_event_publish_ns,
                output_coordinate_publish_ns,
                other_wrapper_ns: 0,
            },
        );
        #[cfg(not(feature = "profiling"))]
        let _ = (
            tile_id,
            coordinates,
            total_duration_ns,
            user_duration_ns,
            external_input_resolve_ns,
            internal_input_resolve_ns,
            trace_serialize_ns,
            draft_capture_ns,
            scope_enter_ns,
            output_record_build_ns,
            trace_event_publish_ns,
            output_coordinate_publish_ns,
        );
    }

    #[cfg(not(feature = "std"))]
    pub fn record_tile_profile(
        _: &str,
        _: crate::core::cfs::CfsCoordinates,
        _: u64,
        _: u64,
        _: u64,
        _: u64,
        _: u64,
        _: u64,
        _: u64,
        _: u64,
        _: u64,
        _: u64,
    ) {
    }

    #[cfg(feature = "std")]
    pub fn publish_tile_output_coordinates(coordinates: crate::core::cfs::CfsCoordinates) {
        raster_runtime::publish_pending_output_coordinates(coordinates);
    }

    #[cfg(not(feature = "std"))]
    pub fn publish_tile_output_coordinates(_: crate::core::cfs::CfsCoordinates) {
        panic!("Tile output coordinates require the `std` feature")
    }

    #[doc(hidden)]
    pub struct RecurSiteScopeGuard;

    impl RecurSiteScopeGuard {
        pub fn enter() -> Self {
            #[cfg(feature = "std")]
            {
                raster_runtime::enter_recur_site_scope()
                    .unwrap_or_else(|error| panic!("Failed to enter recur site scope: {}", error));
            }

            Self
        }
    }

    impl Drop for RecurSiteScopeGuard {
        fn drop(&mut self) {
            #[cfg(feature = "std")]
            {
                raster_runtime::exit_recur_site_scope();
            }
        }
    }

    #[doc(hidden)]
    pub struct RecurSequenceIterationScopeGuard;

    impl RecurSequenceIterationScopeGuard {
        pub fn enter() -> Self {
            #[cfg(feature = "std")]
            {
                raster_runtime::enter_recur_sequence_iteration_scope().unwrap_or_else(|error| {
                    panic!(
                        "Failed to enter recursive sequence iteration scope: {}",
                        error
                    )
                });
            }

            Self
        }
    }

    impl Drop for RecurSequenceIterationScopeGuard {
        fn drop(&mut self) {
            #[cfg(feature = "std")]
            {
                raster_runtime::exit_recur_sequence_iteration_scope();
            }
        }
    }

    #[doc(hidden)]
    pub struct RecurTraceScopeGuard {
        #[cfg(feature = "std")]
        inner: Option<raster_runtime::RecurTraceScopeGuard>,
    }

    impl RecurTraceScopeGuard {
        pub fn enter() -> Self {
            #[cfg(feature = "std")]
            {
                return Self {
                    inner: Some(raster_runtime::RecurTraceScopeGuard::enter()),
                };
            }

            #[cfg(not(feature = "std"))]
            {
                Self {}
            }
        }
    }

    impl Drop for RecurTraceScopeGuard {
        fn drop(&mut self) {
            #[cfg(feature = "std")]
            {
                self.inner.take();
            }
        }
    }

    #[cfg(feature = "std")]
    pub fn bind_infallible_call<T>(result: T) -> crate::AuthRef<T>
    where
        T: serde::Serialize + serde::de::DeserializeOwned + 'static,
    {
        #[cfg(feature = "profiling")]
        let output_store_start = profile_now();
        let reference =
            raster_runtime::store_execution_output_value(&result).unwrap_or_else(|error| {
                panic!("Failed to store tile output in internal storage: {}", error)
            });
        #[cfg(feature = "profiling")]
        let output_store_duration_ns =
            u64::try_from(output_store_start.elapsed().as_nanos()).unwrap_or(u64::MAX);
        #[cfg(feature = "profiling")]
        raster_runtime::record_tile_output_store_profile(output_store_duration_ns);
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
        #[cfg(feature = "profiling")]
        let output_store_start = profile_now();
        let reference =
            raster_runtime::store_execution_output_value(&result).unwrap_or_else(|error| {
                panic!("Failed to store tile output in internal storage: {}", error)
            });
        #[cfg(feature = "profiling")]
        let output_store_duration_ns =
            u64::try_from(output_store_start.elapsed().as_nanos()).unwrap_or(u64::MAX);
        #[cfg(feature = "profiling")]
        raster_runtime::record_tile_output_store_profile(output_store_duration_ns);
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
///
/// Empty inputs skip the step function entirely. They only finalize successfully when the
/// untouched output schema can still be materialized without any set-once writes.
#[macro_export]
macro_rules! call_recur {
    ($($tt:tt)*) => {
        compile_error!("call_recur! can only be used inside #[sequence] functions")
    };
}

/// Canonical recursive list-call primitive for invoking a recur sequence inside a sequence.
///
/// `call_recur_seq!` is only valid inside `#[sequence]` functions, where the
/// sequence macro rewrites it into a hidden driver that iterates a selectable
/// list source while preserving tiles inside each iteration as replay units.
#[macro_export]
macro_rules! call_recur_seq {
    ($($tt:tt)*) => {
        compile_error!("call_recur_seq! can only be used inside #[sequence] functions")
    };
}

/// Emits a Raster-controlled output line that `cargo raster run` will surface.
///
/// Use this instead of `std::println!` in Raster user code. The CLI captures
/// this output and displays it under the `Output:` section.
#[macro_export]
macro_rules! println {
    ($($arg:tt)*) => {{
        $crate::__private::emit_output(::core::format_args!($($arg)*));
    }};
}

/// Prelude module for convenient imports.
///
/// Importing `raster::prelude::*` makes bare `Result<T>` refer to Raster's
/// terminal execution result type. Raster runtime failures remain available
/// separately under `raster::runtime`.
pub mod prelude {
    pub use crate::core::tile::{TileId, TileIdStatic, TileMetadata, TileMetadataStatic};

    // These modules require std
    #[cfg(feature = "std")]
    pub use crate::core::{
        manifest::Manifest,
        schema::{ControlFlow, SequenceSchema},
        trace::{FnCallRecord, FnInput, FnInputArg, FnOutput, TileExecRecord},
    };

    pub use crate::exec::Result;
    pub use crate::{
        call, call_recur, call_recur_seq, call_seq, finalize, internal, into_auth_ref, into_draft,
        materialize_auth_result, materialize_auth_return, new, select, sequence, tile, Anchor,
        AuthRef, AuthValue, Draft, InternalRef, InternalValue, IntoAuthRef, IntoAuthValue,
        IntoDraft, ListProofDirection, ListProofSibling, Op, RecurControl, RecurInput, RecurOutput,
        RecurSequenceInput, RecurSequenceOutput, RecurSequenceState, RecurState, Schema,
        SchemaField, SchemaFieldMode, SchemaNode, SelectSource, Selectable, SelectedPayload,
        SelectionProof, SelectionProofStep, SelectorPath, SelectorSegment, TypedInternalBinding,
        TypedSelectorPath,
    };

    // TODO: Re-enable once Executor/Tracer types are implemented
    // #[cfg(feature = "std")]
    // pub use crate::{Executor, Tracer, FileTracer, NoOpTracer};

    #[cfg(feature = "std")]
    pub use crate::{resolve_internal_value, store_internal_value};
}
