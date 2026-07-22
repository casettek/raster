//! Starting a program and binding `main`'s declared entry arguments.
//!
//! This is the one place data enters a program: each declared parameter is
//! tied to the commitment the public manifest declares for it, and the whole
//! set is committed as a single struct-of-commitments object at the sequence
//! root coordinate `[]` of `main` (see `ReferencedObject`). Everything
//! downstream reaches those arguments through storage, so this module is what
//! the transition guest's `checks::entrypoint` verifies against the
//! authorization journal. `start_program` runs once, as the program's first
//! traced step, and is always emitted — even when `main` declares no entry
//! arguments, in which case it binds nothing and touches no storage.

use std::sync::Arc;

use raster_core::input::{ExternalEncoding, SchemaNode, StorageRef};
use raster_core::{Error, Result};
use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::backing::ReferencedSourceKind;
use crate::input::{tree_value_from_serialize, TreeValue};
use crate::source::{FileInputSourceResolver, SourceResolver};
use crate::storage::{
    decode_hex_bytes, AuthorizedSource, AuthorizedSourceLoad, THREAD_SEQUENCE_CONTEXT,
    THREAD_STORAGE,
};

/// Opaque, per-argument spec for `start_program` — carries whatever
/// a Postcard-encoded source would need to be deserialized (a monomorphized
/// `to_tree`/`schema` pair, derived from the argument's Rust type), without
/// exposing `TreeValue` or any other crate-internal type across the crate
/// boundary. Built via [`entry_argument_spec`]; which encoding actually
/// applies is a manifest fact, decided inside `start_program`, not
/// something the caller commits to ahead of time.
pub struct EntryArgumentSpec {
    name: &'static str,
    to_tree: fn(&[u8]) -> Result<TreeValue>,
    schema: fn() -> SchemaNode,
}

pub(crate) fn postcard_bytes_to_tree<T: DeserializeOwned + Serialize>(
    bytes: &[u8],
) -> Result<TreeValue> {
    let value: T = raster_core::postcard::from_bytes(bytes).map_err(|e| {
        Error::Serialization(format!(
            "Failed to deserialize entry argument from postcard bytes: {}",
            e
        ))
    })?;
    tree_value_from_serialize(&value)
}

/// Build the spec for one declared `main` argument of type `T`. Macro
/// codegen calls this once per declared parameter, in declaration order,
/// before passing the resulting slice to `start_program`.
pub fn entry_argument_spec<T>(name: &'static str) -> EntryArgumentSpec
where
    T: DeserializeOwned + Serialize + raster_core::input::Selectable,
{
    EntryArgumentSpec {
        name,
        to_tree: postcard_bytes_to_tree::<T>,
        schema: T::schema,
    }
}

/// The result of [`start_program`]: the coordinate-`[]` reference (for
/// building each argument's storage-backed `AuthRef`), plus enough
/// per-argument metadata for the caller to publish a matching
/// `TraceEvent::ProgramStart`. When `main` declares no entry arguments,
/// `arguments` is empty and `reference` is a placeholder at `[]` that is
/// never dereferenced (no `AuthRef` is built).
pub struct EntryArgumentsBinding {
    pub reference: StorageRef,
    pub arguments: Vec<raster_core::trace::EntrypointArgumentBinding>,
}

/// Starts the program: loads `main`'s declared arguments into a single
/// authorized storage object at the sequence root coordinate `[]`, as the
/// program's first traced step. Must be called before any other write in
/// `main`'s scope, since `[]` is the sequence root and every later write
/// takes a child coordinate. Reads each argument's `(encoding, commitment)`
/// straight from the manifest — no file bytes are touched — computes the
/// combined struct-of-commitments root, and relies on the store's
/// `SourceResolver` so later `select!` calls into these arguments resolve
/// lazily, one source at a time.
///
/// With no declared arguments this binds nothing and touches no storage; it
/// still returns a placeholder binding so the caller uniformly publishes a
/// `ProgramStart` event.
pub fn start_program(args: &[EntryArgumentSpec]) -> Result<EntryArgumentsBinding> {
    let coordinates = THREAD_SEQUENCE_CONTEXT
        .with(|context| context.borrow().sequence_root_coordinates())?;

    if args.is_empty() {
        return Ok(EntryArgumentsBinding {
            reference: StorageRef::new(coordinates, Vec::new()),
            arguments: Vec::new(),
        });
    }

    // The resolver is installed once, by `init` (see
    // `install_default_source_resolver`) — this is a consumer of the
    // runtime's input context, not a second place that decides what it is.
    let resolver = THREAD_STORAGE
        .with(|storage| storage.borrow().source_resolver())
        .ok_or_else(|| {
            Error::Other(
                "Program declares main entry arguments but no --input/--input-manifest was provided"
                    .into(),
            )
        })?;

    let mut sources = Vec::with_capacity(args.len());
    let mut bindings = Vec::with_capacity(args.len());
    for spec in args {
        let (encoding, commitment_hex) = resolver.manifest_commitment_metadata(spec.name)?;
        let kind = match encoding {
            ExternalEncoding::Raster => ReferencedSourceKind::Raster,
            ExternalEncoding::Postcard => ReferencedSourceKind::Postcard {
                to_tree: spec.to_tree,
                schema: spec.schema,
            },
        };
        let commitment = decode_hex_bytes(&commitment_hex)?;
        bindings.push(raster_core::trace::EntrypointArgumentBinding {
            name: spec.name.to_string(),
            encoding,
            commitment: commitment.clone(),
        });
        sources.push(AuthorizedSource {
            name: spec.name.to_string(),
            encoding,
            commitment,
            kind,
        });
    }

    let load = AuthorizedSourceLoad { sources };

    THREAD_STORAGE.with(|storage| {
        let write = storage
            .borrow_mut()
            .load_authorized_sources(load, coordinates.clone());
        Ok(EntryArgumentsBinding {
            reference: StorageRef::new(coordinates, write.entry.object_commitment),
            arguments: bindings,
        })
    })
}

/// Wire the process's external input context into the runtime, from the
/// `--input` / `--input-manifest` arguments it was started with.
///
/// Called once from `init`. This is the only place production code decides
/// where entry-argument bytes come from — `start_program` and the trace
/// recorder consume that decision rather than each re-deriving it from
/// `std::env::args`. A program run without those arguments installs nothing,
/// which is only an error if it goes on to declare entry arguments.
pub fn install_default_source_resolver() -> Result<()> {
    let Some(manager) = FileInputSourceResolver::from_cli_args()? else {
        return Ok(());
    };
    let resolver: Arc<dyn SourceResolver> = Arc::new(manager);
    THREAD_STORAGE.with(|storage| storage.borrow_mut().set_source_resolver(resolver));
    Ok(())
}
