use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::marker::PhantomData;
use core::{hash::Hash, hash::Hasher};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

#[cfg(not(feature = "std"))]
use alloc::format;
pub use raster_core::collections::{
    bytes_field_key, page_index_for_field, page_index_for_region, page_range_for_field,
    page_range_for_region, Block, Bytes, BytesFieldPageSize, BytesPage, List, Materializable,
    PageSized,
};
use raster_core::draft::{draft_value_from_serialize, DraftOp};
use raster_core::draft::{replay_handle_for_schema, DraftReplayHandle, DraftReplayTransition};
pub use raster_core::input::{
    verify_selection_proof, AuthValue, ExternalEncoding, IndexWidth, ListProofDirection,
    ListProofSibling, Op, Schema, SchemaField, SchemaFieldMode, SchemaNode, Selectable,
    SelectedPayload, SelectionCommitment, SelectionPayloadKind, SelectionProof,
    SelectionProofStep, SelectionWitness, SelectorPath, SelectorSegment, StorageRef, StorageValue,
};
use raster_core::trace::{FnInputValue, StorageData as TraceStorageData};

#[derive(Debug)]
pub struct TypedStorageBinding<Root> {
    reference: StorageRef,
    resolve: fn(StorageRef) -> raster_core::Result<StorageValue<Root>>,
    marker: PhantomData<fn() -> Root>,
}

#[derive(Debug, PartialEq, Eq, Hash)]
pub struct TypedSelectorPath<Root, Selected> {
    path: SelectorPath,
    marker: PhantomData<fn() -> (Root, Selected)>,
}

pub type Anchor = [u8; 32];

/// Live draft handle backed by thread-local runtime state.
///
/// Serialized forms are trace-only markers and cannot be deserialized back into
/// a reusable draft handle.
pub struct Draft<S: Schema> {
    anchor: Anchor,
    current_root: [u8; 32],
    #[cfg(not(feature = "std"))]
    replay_state: ReplayDraftState,
    _schema: PhantomData<fn() -> S>,
}

#[cfg(not(feature = "std"))]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ReplayDraftFieldValue {
    Set,
    Append,
}

#[cfg(not(feature = "std"))]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ReplayDraftState {
    schema_hash: [u8; 32],
    ops: Vec<DraftOp>,
    fields: Vec<(String, ReplayDraftFieldValue)>,
}

#[derive(Debug, Serialize)]
struct DraftTraceMarker {
    kind: &'static str,
    schema: &'static str,
    reusable: bool,
}

#[derive(Debug)]
pub struct DraftSetField<'a, S: Schema, Value> {
    draft: &'a mut Draft<S>,
    field: &'static str,
    marker: PhantomData<fn() -> Value>,
}

#[derive(Debug)]
pub struct DraftAppendField<'a, S: Schema, Value> {
    draft: &'a mut Draft<S>,
    field: &'static str,
    marker: PhantomData<fn() -> Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RecurControl<T> {
    Continue(T),
    Break(T),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RecurInput<T> {
    value: T,
    index: u64,
    len: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RecurState<T> {
    inner: T,
}

pub type RecurOutput<S> = Draft<S>;

/// Opaque recursive-sequence view of the current item.
///
/// Recursive sequences are orchestration-only: they may pass this handle to
/// normal tiles, but they cannot inspect item values or iteration position.
pub struct RecurSequenceInput<T> {
    item: AuthRef<T>,
    index: u64,
    len: u64,
}

/// Opaque recursive-sequence view of threaded inline state.
///
/// State transitions must happen in normal tiles, not directly in sequence
/// bodies, so this type intentionally exposes no mutation accessors.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RecurSequenceState<T> {
    inner: T,
}

/// Opaque recursive-sequence view of threaded draft output.
///
/// Draft mutations must happen in normal tiles, not directly in sequence bodies.
#[derive(Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RecurSequenceOutput<S: Schema> {
    inner: RecurOutput<S>,
}

#[derive(Debug, Serialize)]
struct RecurSequenceInputTraceMarker {
    kind: &'static str,
    index: u64,
    len: u64,
    item: FnInputValue,
}

pub trait IntoRecurControl<T> {
    fn into_recur_control(self) -> RecurControl<T>;
}

impl<T> IntoRecurControl<T> for RecurControl<T> {
    fn into_recur_control(self) -> RecurControl<T> {
        self
    }
}

impl<T> IntoRecurControl<T> for T {
    fn into_recur_control(self) -> RecurControl<T> {
        RecurControl::Continue(self)
    }
}

impl<T> RecurInput<T> {
    pub fn new(value: T, index: u64, len: u64) -> Self {
        Self { value, index, len }
    }

    pub fn value(&self) -> &T {
        &self.value
    }

    pub fn into_value(self) -> T {
        self.value
    }

    pub fn index(&self) -> u64 {
        self.index
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_first(&self) -> bool {
        self.index == 0
    }

    pub fn is_last(&self) -> bool {
        self.index + 1 == self.len
    }
}

impl<T> RecurState<T> {
    pub fn new(inner: T) -> Self {
        Self { inner }
    }

    pub fn get(&self) -> &T {
        &self.inner
    }

    pub fn get_mut(&mut self) -> &mut T {
        &mut self.inner
    }

    pub fn into_inner(self) -> T {
        self.inner
    }
}

impl<T> RecurSequenceInput<T> {
    #[doc(hidden)]
    pub fn __raster_from_auth_ref(item: AuthRef<T>, index: u64, len: u64) -> Self {
        Self { item, index, len }
    }

    #[doc(hidden)]
    pub fn __raster_as_auth_ref(&self) -> &AuthRef<T> {
        &self.item
    }

    /// Implementation of [`into_ref!`](crate::into_ref) — call the macro, not
    /// this.
    ///
    /// Hidden deliberately. The CFS flow resolver reads the sequence body as
    /// source and attributes provenance by recognizing the grammar's *macros*
    /// by name; a bare method call is a form it cannot attribute, so a local
    /// bound to one resolves to `InputSource::Inline` — a step argument the
    /// schema does not pin to any upstream binding. Making the macro the only
    /// public spelling keeps the surface and the analysis in agreement by
    /// construction, rather than by convention.
    ///
    /// Materializes nothing: the reference resolves only when a step reads it.
    #[doc(hidden)]
    pub fn __raster_into_ref(self) -> AuthRef<T> {
        self.item
    }
}

impl<T> RecurSequenceInput<T>
where
    T: Serialize + DeserializeOwned,
{
    #[doc(hidden)]
    pub fn __raster_auth_trace(&self) -> raster_core::Result<AuthRefTrace> {
        let item_trace = auth_ref_trace(&self.item)?;
        let marker = RecurSequenceInputTraceMarker {
            kind: "raster::RecurSequenceInput",
            index: self.index,
            len: self.len,
            item: item_trace.value.clone(),
        };
        Ok(AuthRefTrace {
            value: FnInputValue::Inline(
                raster_core::postcard::to_allocvec(&marker).unwrap_or_default(),
            ),
            storage: item_trace.storage,
            index_bindings: item_trace.index_bindings,
        })
    }
}

impl<T> RecurSequenceState<T> {
    #[doc(hidden)]
    pub fn __raster_from_recur_state(inner: RecurState<T>) -> Self {
        Self {
            inner: inner.into_inner(),
        }
    }

    #[doc(hidden)]
    pub fn __raster_into_recur_state(self) -> RecurState<T> {
        RecurState::new(self.inner)
    }
}

impl<S> RecurSequenceOutput<S>
where
    S: Schema,
{
    #[doc(hidden)]
    pub fn __raster_from_recur_output(inner: RecurOutput<S>) -> Self {
        Self { inner }
    }

    #[doc(hidden)]
    pub fn __raster_into_recur_output(self) -> RecurOutput<S> {
        self.inner
    }

    #[doc(hidden)]
    pub fn __raster_serialize_replay_handle(&self) -> Vec<u8> {
        serialize_draft_replay_handle(&self.inner)
    }
}

impl<T> Serialize for RecurSequenceInput<T>
where
    T: Serialize,
{
    fn serialize<Ser>(&self, serializer: Ser) -> Result<Ser::Ok, Ser::Error>
    where
        Ser: serde::Serializer,
    {
        let item = match &self.item {
            AuthRef::Inline(value) => {
                FnInputValue::Inline(raster_core::postcard::to_allocvec(value).unwrap_or_default())
            }
            AuthRef::Storage(_) => FnInputValue::StorageBinding,
        };
        RecurSequenceInputTraceMarker {
            kind: "raster::RecurSequenceInput",
            index: self.index,
            len: self.len,
            item,
        }
        .serialize(serializer)
    }
}

fn draft_trace_marker<S: Schema>() -> DraftTraceMarker {
    DraftTraceMarker {
        kind: "raster::Draft",
        schema: core::any::type_name::<S>(),
        reusable: false,
    }
}

impl<S> Serialize for Draft<S>
where
    S: Schema,
{
    fn serialize<Ser>(&self, serializer: Ser) -> Result<Ser::Ok, Ser::Error>
    where
        Ser: serde::Serializer,
    {
        let _ = self;
        draft_trace_marker::<S>().serialize(serializer)
    }
}

impl<S> core::fmt::Debug for Draft<S>
where
    S: Schema,
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Draft")
            .field("anchor", &self.anchor)
            .field("current_root", &self.current_root)
            .finish()
    }
}

impl<S> PartialEq for Draft<S>
where
    S: Schema,
{
    fn eq(&self, other: &Self) -> bool {
        self.anchor == other.anchor && self.current_root == other.current_root
    }
}

impl<S> Eq for Draft<S> where S: Schema {}

impl<S> Hash for Draft<S>
where
    S: Schema,
{
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.anchor.hash(state);
        self.current_root.hash(state);
    }
}

impl<'de, S> Deserialize<'de> for Draft<S>
where
    S: Schema,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let _ = serde::de::IgnoredAny::deserialize(deserializer)?;
        Err(serde::de::Error::custom(
            "Serialized Draft values are trace-only and cannot be deserialized into a live draft",
        ))
    }
}

impl<T> From<T> for RecurState<T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

impl<T> From<AuthRef<T>> for RecurState<T>
where
    T: DeserializeOwned + Serialize,
{
    fn from(value: AuthRef<T>) -> Self {
        Self::new(
            into_auth_value::<T, _>(value)
                .unwrap_or_else(|error| {
                    panic!(
                        "Failed to materialize recursive state from tile output: {}",
                        error
                    )
                })
                .into_inner(),
        )
    }
}

impl<T> core::ops::Deref for RecurState<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.get()
    }
}

impl<T> core::ops::DerefMut for RecurState<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.get_mut()
    }
}

impl<Root> TypedStorageBinding<Root>
where
    Root: DeserializeOwned + Serialize,
{
    pub fn new(reference: StorageRef) -> Self {
        Self {
            reference,
            resolve: resolve_storage_value::<Root>,
            marker: PhantomData,
        }
    }

    #[doc(hidden)]
    pub fn with_resolver(
        reference: StorageRef,
        resolve: fn(StorageRef) -> raster_core::Result<StorageValue<Root>>,
    ) -> Self {
        Self {
            reference,
            resolve,
            marker: PhantomData,
        }
    }

    pub fn reference(&self) -> &StorageRef {
        &self.reference
    }
}

impl<Root, Selected> TypedSelectorPath<Root, Selected> {
    pub fn new(path: SelectorPath) -> Self {
        Self {
            path,
            marker: PhantomData,
        }
    }

    pub fn path(&self) -> &SelectorPath {
        &self.path
    }

    pub fn into_path(self) -> SelectorPath {
        self.path
    }
}

impl<S: Schema> Draft<S> {
    pub fn new(anchor: Anchor, current_root: [u8; 32]) -> Self {
        Self {
            anchor,
            current_root,
            #[cfg(not(feature = "std"))]
            replay_state: ReplayDraftState {
                schema_hash: S::schema_hash(),
                ops: Vec::new(),
                fields: Vec::new(),
            },
            _schema: PhantomData,
        }
    }

    pub fn anchor(&self) -> &Anchor {
        &self.anchor
    }

    pub fn current_root(&self) -> &[u8; 32] {
        &self.current_root
    }

    #[doc(hidden)]
    pub fn set_current_root(&mut self, current_root: [u8; 32]) {
        self.current_root = current_root;
    }

    #[cfg(not(feature = "std"))]
    fn replay_state(&self) -> &ReplayDraftState {
        &self.replay_state
    }

    #[cfg(not(feature = "std"))]
    fn replay_state_mut(&mut self) -> &mut ReplayDraftState {
        &mut self.replay_state
    }

    #[doc(hidden)]
    pub fn set_field<Value>(&mut self, field: &'static str) -> DraftSetField<'_, S, Value> {
        DraftSetField {
            draft: self,
            field,
            marker: PhantomData,
        }
    }

    #[doc(hidden)]
    pub fn append_field<Value>(&mut self, field: &'static str) -> DraftAppendField<'_, S, Value> {
        DraftAppendField {
            draft: self,
            field,
            marker: PhantomData,
        }
    }
}

#[cfg(not(feature = "std"))]
fn schema_struct_fields(schema: &SchemaNode) -> raster_core::Result<&[SchemaField]> {
    match schema {
        SchemaNode::Struct { fields, .. } => Ok(fields.as_slice()),
        _ => Err(raster_core::Error::Other(
            "Drafts currently support only struct schemas at the root".into(),
        )),
    }
}

#[cfg(not(feature = "std"))]
fn locate_schema_field<S: Schema>(field: &str) -> raster_core::Result<SchemaField> {
    let schema = S::schema();
    schema_struct_fields(&schema)?
        .iter()
        .find(|schema_field| schema_field.name == field)
        .cloned()
        .ok_or_else(|| raster_core::Error::Other(format!("Unknown draft field '{}'", field)))
}

#[cfg(not(feature = "std"))]
fn record_replay_set<S: Schema, Value: Serialize>(
    draft: &mut Draft<S>,
    field: &'static str,
    value: &Value,
) -> raster_core::Result<()> {
    let schema_field = locate_schema_field::<S>(field)?;
    if schema_field.mode != SchemaFieldMode::SetOnce {
        return Err(raster_core::Error::Other(format!(
            "Draft field '{}' does not support set; use push",
            field
        )));
    }
    let replay_state = draft.replay_state_mut();
    if replay_state.fields.iter().any(|(name, _)| name == field) {
        return Err(raster_core::Error::Other(format!(
            "Draft field '{}' can only be written once",
            field
        )));
    }
    replay_state
        .fields
        .push((field.into(), ReplayDraftFieldValue::Set));
    replay_state.ops.push(DraftOp::Set {
        field: field.into(),
        value: draft_value_from_serialize(value)?,
    });
    Ok(())
}

#[cfg(not(feature = "std"))]
fn record_replay_push<S: Schema, Value: Serialize>(
    draft: &mut Draft<S>,
    field: &'static str,
    value: &Value,
) -> raster_core::Result<()> {
    let schema_field = locate_schema_field::<S>(field)?;
    if schema_field.mode != SchemaFieldMode::AppendOnlyVec {
        return Err(raster_core::Error::Other(format!(
            "Draft field '{}' does not support push; use set",
            field
        )));
    }
    let replay_state = draft.replay_state_mut();
    match replay_state
        .fields
        .iter_mut()
        .find(|(name, _)| name == field)
    {
        Some((_, ReplayDraftFieldValue::Set)) => {
            return Err(raster_core::Error::Other(format!(
                "Draft field '{}' is not appendable",
                field
            )))
        }
        Some((_, ReplayDraftFieldValue::Append)) => {}
        None => replay_state
            .fields
            .push((field.into(), ReplayDraftFieldValue::Append)),
    }
    replay_state.ops.push(DraftOp::Push {
        field: field.into(),
        value: draft_value_from_serialize(value)?,
    });
    Ok(())
}

pub fn draft_replay_handle<S>(draft: &Draft<S>) -> DraftReplayHandle
where
    S: Schema,
{
    replay_handle_for_schema::<S>(*draft.anchor(), *draft.current_root())
}

pub fn serialize_draft_replay_handle<S>(draft: &Draft<S>) -> Vec<u8>
where
    S: Schema,
{
    raster_core::postcard::to_allocvec(&draft_replay_handle(draft)).unwrap_or_default()
}

pub fn restore_draft_from_replay_handle<S>(handle: DraftReplayHandle) -> Draft<S>
where
    S: Schema,
{
    let draft = Draft::new(handle.draft_id, handle.root_before);
    #[cfg(not(feature = "std"))]
    {
        let mut draft = draft;
        draft.replay_state.schema_hash = handle.schema_hash;
        return draft;
    }
    #[cfg(feature = "std")]
    {
        draft
    }
}

pub fn draft_replay_transition<S>(draft: &Draft<S>) -> Option<DraftReplayTransition>
where
    S: Schema,
{
    #[cfg(not(feature = "std"))]
    {
        return Some(DraftReplayTransition {
            draft_id: *draft.anchor(),
            schema_hash: draft.replay_state().schema_hash,
            root_before: *draft.current_root(),
            ops: draft.replay_state().ops.clone(),
        });
    }

    #[cfg(feature = "std")]
    {
        let _ = draft;
        None
    }
}

#[cfg(feature = "std")]
#[doc(hidden)]
pub fn begin_draft_transition_capture<S>(
    draft: &Draft<S>,
) -> Option<raster_runtime::DraftCaptureSnapshot>
where
    S: Schema,
{
    // The witness exists to let a guest replay one draft step. An
    // unauthenticated run produces no trace for it to live in, so returning
    // `None` here disables it end to end: the tile macro threads this through
    // `Option::and_then`, so `finish_draft_transition_capture` is skipped with
    // no codegen change. See `docs/proposals/unauthenticated-execution.md` §7.
    if !crate::auth_mode().is_authenticated() {
        return None;
    }

    Some(
        raster_runtime::begin_draft_step_capture::<S>(draft.anchor(), draft.current_root())
            .unwrap_or_else(|error| {
                panic!(
                    "Failed to start draft transition capture '{}': {}",
                    core::any::type_name::<S>(),
                    error
                )
            }),
    )
}

#[cfg(feature = "std")]
#[doc(hidden)]
pub fn finish_draft_transition_capture<S>(
    snapshot: raster_runtime::DraftCaptureSnapshot,
    draft: &Draft<S>,
) -> Option<raster_core::draft::DraftTransitionWitness>
where
    S: Schema,
{
    Some(
        raster_runtime::finish_draft_step_capture::<S>(snapshot, draft.current_root())
            .unwrap_or_else(|error| {
                panic!(
                    "Failed to finish draft transition capture '{}': {}",
                    core::any::type_name::<S>(),
                    error
                )
            }),
    )
}

impl<'a, S, Value> DraftSetField<'a, S, Value>
where
    S: Schema,
    Value: Serialize,
{
    pub fn set(self, value: Value) {
        #[cfg(feature = "std")]
        {
            let expected_root = *self.draft.current_root();
            let next_root = raster_runtime::apply_draft_set::<S, Value>(
                self.draft.anchor(),
                &expected_root,
                self.field,
                &value,
            )
            .unwrap_or_else(|error| {
                panic!("Failed to set draft field '{}': {}", self.field, error)
            });
            self.draft.set_current_root(next_root);
        }

        #[cfg(not(feature = "std"))]
        {
            record_replay_set::<S, Value>(self.draft, self.field, &value).unwrap_or_else(|error| {
                panic!("Failed to set draft field '{}': {}", self.field, error)
            });
        }
    }
}

impl<'a, S, Value> DraftAppendField<'a, S, Value>
where
    S: Schema,
    Value: Serialize,
{
    pub fn push(self, value: Value) {
        #[cfg(feature = "std")]
        {
            let expected_root = *self.draft.current_root();
            let next_root = raster_runtime::apply_draft_push::<S, Value>(
                self.draft.anchor(),
                &expected_root,
                self.field,
                &value,
            )
            .unwrap_or_else(|error| {
                panic!("Failed to push draft field '{}': {}", self.field, error)
            });
            self.draft.set_current_root(next_root);
        }

        #[cfg(not(feature = "std"))]
        {
            record_replay_push::<S, Value>(self.draft, self.field, &value).unwrap_or_else(
                |error| panic!("Failed to push draft field '{}': {}", self.field, error),
            );
        }
    }
}

pub fn typed_storage<Root>(reference: StorageRef) -> TypedStorageBinding<Root>
where
    Root: DeserializeOwned + Serialize,
{
    TypedStorageBinding::new(reference)
}

#[doc(hidden)]
pub fn typed_storage_with_resolver<Root>(
    reference: StorageRef,
    resolve: fn(StorageRef) -> raster_core::Result<StorageValue<Root>>,
) -> TypedStorageBinding<Root>
where
    Root: DeserializeOwned + Serialize,
{
    TypedStorageBinding::with_resolver(reference, resolve)
}

/// Binds one of `main`'s declared entry arguments as a storage-backed
/// `AuthRef` rooted at the combined entry object: the argument's name is the
/// binding's selector prefix, so nested `select!`s compose onto it and every
/// read — whole value or field — reaches storage as a single indexed select.
/// Nothing materializes at the binding boundary itself.
#[doc(hidden)]
pub fn entry_argument_auth_ref<T>(reference: StorageRef, name: &str) -> AuthRef<T>
where
    T: DeserializeOwned + Serialize + 'static,
{
    let selector = SelectorPath::new(Vec::from([SelectorSegment::Field(String::from(name))]));
    let resolve_selector = selector.clone();
    AuthRef::Storage(DeferredAuthStorage {
        reference,
        selector,
        // An entry argument's path is a single field name — no index to bind.
        index_bindings: Vec::new(),
        resolve: Rc::new(move |reference| select_stored_value::<T>(&reference, &resolve_selector)),
        marker: PhantomData,
    })
}

pub fn typed_selector_path<Root, Selected>(
    path: SelectorPath,
) -> TypedSelectorPath<Root, Selected> {
    TypedSelectorPath::new(path)
}

type StorageResolveFn<Current> =
    Rc<dyn Fn(StorageRef) -> raster_core::Result<StorageValue<Current>>>;

/// A storage binding that must travel alongside another one: the authorized
/// value that supplied a [`SelectorSegment::BoundIndex`] in its path.
///
/// Named by content (see [`index_binding_name`]) rather than by the consuming
/// parameter, because a `select!` does not know which tile argument the value it
/// produces will eventually be passed as.
pub type IndexBinding = (String, TraceStorageData);

#[doc(hidden)]
pub struct DeferredAuthStorage<Current> {
    reference: StorageRef,
    /// Selector path from `reference`'s stored root down to this value.
    /// Each `select` extends it, so a chain of selects composes into one
    /// full path that storage can serve as a single indexed read — the
    /// value this binding was selected out of never has to materialize.
    selector: SelectorPath,
    /// Bindings this one's selector cites by name through a `BoundIndex`
    /// segment. They are resolved when the `select!` runs (the index value has
    /// to be known to write the path at all) and carried here so that whatever
    /// step eventually reads this value also records them — a `BoundIndex`
    /// whose source is missing from the step's storage map is rejected by the
    /// verifier, so this is what keeps an honest recording verifiable.
    index_bindings: Vec<IndexBinding>,
    resolve: StorageResolveFn<Current>,
    marker: PhantomData<fn() -> Current>,
}

pub enum AuthRef<Current> {
    Inline(Current),
    Storage(DeferredAuthStorage<Current>),
}

impl<Current> AuthRef<Current> {
    pub fn reference(&self) -> &StorageRef {
        match self {
            Self::Storage(binding) => &binding.reference,
            Self::Inline(_) => {
                panic!("AuthRef::reference() is only available for storage bindings")
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AuthRefTrace {
    pub value: FnInputValue,
    pub storage: Option<TraceStorageData>,
    /// Extra storage bindings this argument's path cites through a `BoundIndex`
    /// segment. They belong in the step's `FnInput.storage` map but have no
    /// entry in `values`/`args` — the tile does not take the index as a
    /// parameter. This is the one place `storage` and `values` stop being
    /// parallel; see `docs/proposals/dynamic-index-selection.md` §2.
    #[serde(default)]
    pub index_bindings: Vec<IndexBinding>,
}

impl<Root> Clone for TypedStorageBinding<Root> {
    fn clone(&self) -> Self {
        Self {
            reference: self.reference.clone(),
            resolve: self.resolve,
            marker: PhantomData,
        }
    }
}

impl<Root> PartialEq for TypedStorageBinding<Root> {
    fn eq(&self, other: &Self) -> bool {
        self.reference == other.reference
    }
}

impl<Root> Eq for TypedStorageBinding<Root> {}

impl<Root> core::hash::Hash for TypedStorageBinding<Root> {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        self.reference.hash(state);
    }
}

impl<Root, Selected> Clone for TypedSelectorPath<Root, Selected> {
    fn clone(&self) -> Self {
        Self {
            path: self.path.clone(),
            marker: PhantomData,
        }
    }
}

impl<Current> Clone for DeferredAuthStorage<Current> {
    fn clone(&self) -> Self {
        Self {
            reference: self.reference.clone(),
            selector: self.selector.clone(),
            index_bindings: self.index_bindings.clone(),
            resolve: self.resolve.clone(),
            marker: PhantomData,
        }
    }
}

impl<Current> core::fmt::Debug for DeferredAuthStorage<Current> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("DeferredAuthStorage")
            .field("reference", &self.reference)
            .finish()
    }
}

impl<Current> Clone for AuthRef<Current>
where
    Current: Clone,
{
    fn clone(&self) -> Self {
        match self {
            Self::Inline(value) => Self::Inline(value.clone()),
            Self::Storage(binding) => Self::Storage(binding.clone()),
        }
    }
}

fn summarize_coordinates(coordinates: &raster_core::cfs::CfsCoordinates) -> String {
    if coordinates.is_empty() {
        return "<root>".into();
    }

    let mut summary = String::new();
    for (index, coordinate) in coordinates.iter().enumerate() {
        if index > 0 {
            summary.push('/');
        }
        summary.push_str(&alloc::format!("{}", coordinate));
    }

    summary
}

impl<Current> core::fmt::Debug for AuthRef<Current>
where
    Current: core::fmt::Debug,
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Inline(value) => f
                .debug_struct("AuthRef")
                .field("storage", &"inline")
                .field("value", value)
                .finish(),
            Self::Storage(binding) => match (binding.resolve.as_ref())(binding.reference.clone()) {
                Ok(resolved) => f
                    .debug_struct("AuthRef")
                    .field("storage", &"storage")
                    .field(
                        "coordinates",
                        &summarize_coordinates(&resolved.reference.coordinates),
                    )
                    .field("commitment_len", &resolved.reference.commitment.len())
                    .field("stored_bytes_len", &resolved.bytes.len())
                    .field("value", &resolved.value)
                    .finish(),
                Err(error) => f
                    .debug_struct("AuthRef")
                    .field("storage", &"storage")
                    .field(
                        "coordinates",
                        &summarize_coordinates(&binding.reference.coordinates),
                    )
                    .field("commitment_len", &binding.reference.commitment.len())
                    .field("materialization_error", &alloc::format!("{}", error))
                    .finish(),
            },
        }
    }
}

pub trait IntoAuthRef<Current> {
    fn into_auth_ref(self) -> AuthRef<Current>;
}

/// Bound on a `call_recur!` source. The note names `.pages` so `select!(Bytes, …)`
/// as a sweep input is a compile error with a useful diagnostic.
#[cfg_attr(
    not(doc),
    diagnostic::on_unimplemented(
        message = "call_recur! input must be a `List<T>`",
        label = "not a page list",
        note = "to sweep a `Bytes` region, select its pages: `select!(List<BytesPage>, region.pages)`"
    )
)]
pub trait RecurListSource<E>: IntoAuthRef<List<E>> {}

impl<T, E> RecurListSource<E> for T where T: IntoAuthRef<List<E>> {}

impl<T: PageSized> PageSized for AuthRef<T> {
    const PAGE_SIZE: u64 = T::PAGE_SIZE;
}

impl<T: BytesFieldPageSize<FIELD>, const FIELD: u64> BytesFieldPageSize<FIELD> for AuthRef<T> {
    const PAGE_SIZE: u64 = T::PAGE_SIZE;
}

pub trait SelectSource {
    type Root;
    type Current;
    type Selected<Selected>;

    fn select<Selected>(
        self,
        selector: TypedSelectorPath<Self::Current, Selected>,
    ) -> Self::Selected<Selected>
    where
        Selected: DeserializeOwned + Serialize;
}

impl<Root> SelectSource for TypedStorageBinding<Root>
where
    Root: DeserializeOwned + Serialize + Selectable + 'static,
{
    type Root = Root;
    type Current = Root;
    type Selected<Selected> = AuthRef<Selected>;

    fn select<Selected>(
        self,
        selector: TypedSelectorPath<Self::Current, Selected>,
    ) -> Self::Selected<Selected>
    where
        Selected: DeserializeOwned + Serialize,
    {
        let reference = self.reference.clone();
        let selector = selector.into_path();
        let resolve = self.resolve;
        let resolve_selector = selector.clone();
        AuthRef::Storage(DeferredAuthStorage {
            reference,
            selector,
            // A `TypedStorageBinding` is a root; any bound index in the path
            // just handed to us is attached by `attach_index_bindings`.
            index_bindings: Vec::new(),
            resolve: Rc::new(move |reference| {
                let current = resolve(reference.clone())?;
                select_storage_value::<Root, Selected>(&current, &resolve_selector)
            }),
            marker: PhantomData,
        })
    }
}

impl<Current> SelectSource for AuthRef<Current>
where
    Current: DeserializeOwned + Serialize + Selectable + 'static,
{
    type Root = Current;
    type Current = Current;
    type Selected<Selected> = AuthRef<Selected>;

    fn select<Selected>(
        self,
        selector: TypedSelectorPath<Self::Current, Selected>,
    ) -> Self::Selected<Selected>
    where
        Selected: DeserializeOwned + Serialize,
    {
        match self {
            AuthRef::Inline(_) => {
                panic!(
                    "select! on inline sequence values is not supported; use committed storage bindings instead"
                )
            }
            AuthRef::Storage(binding) => {
                let relative_selector = selector.into_path();
                let mut full_selector = binding.selector.clone();
                full_selector
                    .segments
                    .extend(relative_selector.segments.iter().cloned());
                let reference = binding.reference.clone();
                let resolve_current = binding.resolve.clone();
                let resolve_selector = full_selector.clone();
                AuthRef::Storage(DeferredAuthStorage {
                    reference: reference.clone(),
                    selector: full_selector,
                    // Selecting *through* a value inherits its citations: the
                    // composed path still contains the parent's `BoundIndex`
                    // segments, so their sources must still reach the step.
                    index_bindings: binding.index_bindings.clone(),
                    resolve: Rc::new(move |reference| {
                        // The composed path is anchored to the stored root
                        // `reference` names, so storage can serve the whole
                        // select chain as one indexed read; only when the
                        // root isn't path-addressable (e.g. behind a custom
                        // resolver) does this fall back to materializing the
                        // parent and selecting in memory.
                        if let Ok(selected) =
                            select_stored_value::<Selected>(&reference, &resolve_selector)
                        {
                            return Ok(selected);
                        }
                        let current = (resolve_current.as_ref())(reference.clone())?;
                        select_storage_value::<Current, Selected>(&current, &relative_selector)
                    }),
                    marker: PhantomData,
                })
            }
        }
    }
}

pub fn select_source<Source, Selected>(
    source: Source,
    selector: TypedSelectorPath<Source::Current, Selected>,
) -> Source::Selected<Selected>
where
    Source: SelectSource,
    Selected: DeserializeOwned + Serialize,
{
    source.select(selector)
}

/// A `select!` base, seen by the unauthenticated arm.
///
/// The mirror of [`SelectSource`], which the two source forms — a sequence
/// binding and an explicit `storage!` reference — both implement. Both must
/// implement this too, or `select!` would stop compiling for one of them the
/// moment the unauthenticated arm exists, regardless of which arm ever runs.
pub trait InlineSelectSource {
    type Current;

    fn select_inline_with<Selected, F>(&self, select: F) -> AuthRef<Selected>
    where
        F: FnOnce(&Self::Current) -> Selected;

    /// Whether this base names data in storage rather than carrying it.
    ///
    /// `select!` dispatches on this, not on the mode: a storage-backed base
    /// keeps composing a selector even with authentication off, so an external
    /// input is read by indexed lookup at the tile boundary instead of being
    /// materialized whole at every step. That is what keeps a large region
    /// lazy — the property `lazy-list-recur` exists to protect — and what lets
    /// `call_recur!` keep its raster-indexed source in either mode.
    fn is_storage_backed(&self) -> bool;
}

/// Free-function form of [`InlineSelectSource::is_storage_backed`], for the
/// `select!` expansion.
pub fn is_storage_backed<Source: InlineSelectSource>(source: &Source) -> bool {
    source.is_storage_backed()
}

impl<Current> InlineSelectSource for AuthRef<Current> {
    type Current = Current;

    fn is_storage_backed(&self) -> bool {
        matches!(self, AuthRef::Storage(_))
    }

    fn select_inline_with<Selected, F>(&self, select: F) -> AuthRef<Selected>
    where
        F: FnOnce(&Current) -> Selected,
    {
        match self {
            AuthRef::Inline(value) => AuthRef::Inline(select(value)),
            // Unreachable through `select!`, which now sends a storage-backed
            // base down the selector arm instead (see `is_storage_backed`).
            // Kept correct rather than a panic because the trait is public and
            // a direct caller can still land here; resolving is the honest
            // answer, at the cost of materializing the parent.
            AuthRef::Storage(binding) => {
                let resolved = (binding.resolve.as_ref())(binding.reference.clone())
                    .unwrap_or_else(|error| {
                        panic!("Failed to resolve external input for select!: {}", error)
                    });
                AuthRef::Inline(select(&resolved.value))
            }
        }
    }
}

impl<Root> InlineSelectSource for TypedStorageBinding<Root>
where
    Root: DeserializeOwned + Serialize,
{
    type Current = Root;

    /// Always. `storage!(T, reference)` names a storage coordinate outright.
    fn is_storage_backed(&self) -> bool {
        true
    }

    fn select_inline_with<Selected, F>(&self, select: F) -> AuthRef<Selected>
    where
        F: FnOnce(&Root) -> Selected,
    {
        // As above: `select!` routes this to the selector arm, so this is a
        // direct-caller path only.
        let resolved = (self.resolve)(self.reference.clone()).unwrap_or_else(|error| {
            panic!("Failed to resolve storage! binding for select!: {}", error)
        });
        AuthRef::Inline(select(&resolved.value))
    }
}

/// Apply a `select!` path as a plain Rust field access.
///
/// The unauthenticated counterpart of [`select_source`]. `select!` knows the
/// path structurally at expansion time, so the accessor arrives here already
/// lowered to real field and index expressions — nothing walks a
/// [`SelectorPath`] at runtime, and a sequence binding is never serialized. See
/// `docs/proposals/unauthenticated-execution.md` §5.1.
pub fn select_inline<Source, Selected, F>(source: &Source, select: F) -> AuthRef<Selected>
where
    Source: InlineSelectSource,
    F: FnOnce(&Source::Current) -> Selected,
{
    source.select_inline_with(select)
}

/// A value that may supply a `select!` index.
///
/// Implemented only for `AuthRef<T>` with `T` an unsigned integer, which is what
/// makes "the index must be authorized, and must be an unsigned integer" a
/// compile error rather than a runtime one. There is deliberately no blanket
/// impl for plain integers: a computed or literal-valued index has no lineage,
/// and an index without lineage is the prover-chosen index the whole mechanism
/// exists to rule out. See `docs/proposals/dynamic-index-selection.md`.
pub trait IndexSource {
    /// The committed width of the supplying value. Load-bearing: leaf bytes are
    /// fixed-width, so the verifier needs this to re-derive them.
    const WIDTH: IndexWidth;

    /// Materialize the index and the storage binding that authorizes it.
    ///
    /// Returns the index value, the binding to record, and any citations the
    /// index's *own* path carried (a nested dynamic index).
    ///
    /// Takes `&self` so one authorized index can locate several values —
    /// `rows[i]` and `cells[i]` in the same body. Consuming the reference would
    /// force a `.clone()`, which the `select!` grammar rejects as a computed
    /// index, making the shared-index case (the whole reason citations are
    /// content-named and deduplicated) unwritable.
    fn resolve_index(&self) -> raster_core::Result<(u64, TraceStorageData, Vec<IndexBinding>)>;

    /// The index value alone, with no binding to record.
    ///
    /// The unauthenticated counterpart of [`Self::resolve_index`]. There is no
    /// lineage to preserve in that mode, so the rule this trait exists to
    /// enforce — an index must be an authorized value — protects nothing and is
    /// suspended. See `docs/proposals/unauthenticated-execution.md` §5.3.
    fn inline_index(&self) -> u64;
}

/// Read a `select!` index without authorizing it. Only reachable from the
/// unauthenticated arm of `select!`.
pub fn inline_index<I: IndexSource>(index: &I) -> u64 {
    index.inline_index()
}

macro_rules! impl_index_source {
    ($($ty:ty => $width:ident),* $(,)?) => {$(
        impl IndexSource for AuthRef<$ty> {
            const WIDTH: IndexWidth = IndexWidth::$width;

            fn inline_index(&self) -> u64 {
                match self {
                    AuthRef::Inline(value) => u64::from(*value),
                    AuthRef::Storage(binding) => {
                        let resolved = (binding.resolve.as_ref())(binding.reference.clone())
                            .unwrap_or_else(|error| {
                                panic!("Failed to resolve select! index: {}", error)
                            });
                        u64::from(resolved.value)
                    }
                }
            }

            fn resolve_index(
                &self,
            ) -> raster_core::Result<(u64, TraceStorageData, Vec<IndexBinding>)> {
                match self {
                    // An inline value reached a sequence body without passing
                    // through storage, so nothing commits to it. Rejecting here
                    // is the runtime half of the type rule above.
                    AuthRef::Inline(_) => Err(raster_core::Error::Other(
                        "a select! index must be an authorized storage binding, \
                         not an inline sequence value"
                            .into(),
                    )),
                    AuthRef::Storage(binding) => {
                        let resolved = (binding.resolve.as_ref())(binding.reference.clone())?;
                        let data = TraceStorageData {
                            coordinates: resolved.reference.coordinates.clone(),
                            commitment: resolved.reference.commitment.clone(),
                            selector: resolved.selector.clone(),
                            selection: resolved.selection.clone(),
                        };
                        Ok((
                            u64::from(resolved.value),
                            data,
                            binding.index_bindings.clone(),
                        ))
                    }
                }
            }
        }
    )*};
}

impl_index_source! {
    u8 => U8,
    u16 => U16,
    u32 => U32,
    u64 => U64,
}

/// Content-derived name for an index binding.
///
/// A `select!` cannot know which tile parameter its result will be passed as, so
/// the name cannot be derived from the consumer. Hashing the binding itself
/// gives two properties that matter:
///
/// * **collision-freedom with real parameters** — the `@` prefix is not a legal
///   Rust identifier, so an index binding can never shadow an argument's entry
///   in the step's storage map (which `resolved_source_at` looks up by
///   parameter name);
/// * **the same index used twice is one binding** — two selects citing the same
///   authorized value produce identical `StorageData`, hence the same name, hence
///   a single map entry. That is what makes "the same index" *mean* the same
///   index rather than two independently forgeable ones.
pub fn index_binding_name(data: &TraceStorageData) -> String {
    let bytes = raster_core::postcard::to_allocvec(data).unwrap_or_default();
    let digest = raster_core::input::selection_payload_hash(&bytes);
    let mut name = String::from("@idx/");
    for byte in digest.iter().take(8) {
        name.push_str(&alloc::format!("{:02x}", byte));
    }
    name
}

/// Resolve a `select!` index expression into its selector segment, recording the
/// binding that authorizes it into `sink`.
///
/// Called from `select!`'s expansion once per dynamic index. Resolution is eager
/// because the path cannot be written without the index value — unlike the rest
/// of a selection, which stays deferred. Panics on failure, matching how
/// `select!` already treats an unresolvable path: a sequence body has no way to
/// handle it, and continuing with a wrong index must not be possible.
///
/// Unauthenticated, the segment degrades to a plain [`SelectorSegment::Index`]
/// and nothing is recorded. `BoundIndex` names a sibling storage binding so a
/// verifier can establish that the index was one this execution already
/// authorized; this mode writes no trace, so that binding has no reader and no
/// verifier will ever run. This is the same suspension
/// `docs/proposals/unauthenticated-execution.md` §5.3 makes for a plain integer
/// index — reached here rather than in the accessor arm because a
/// storage-backed base keeps selector lowering (§5.4) even when the index
/// feeding it is a tile-produced inline value, which is the combination §5.3
/// did not cover. A guest answers `Authenticated` unconditionally, so a
/// replayed step cannot take this arm.
#[doc(hidden)]
pub fn push_bound_index<I>(sink: &mut Vec<IndexBinding>, index: &I) -> SelectorSegment
where
    I: IndexSource,
{
    if !crate::auth_mode().is_authenticated() {
        return SelectorSegment::Index(index.inline_index());
    }

    let (value, data, inherited) = index
        .resolve_index()
        .unwrap_or_else(|error| panic!("Failed to resolve select! index: {}", error));

    let name = index_binding_name(&data);
    // The index's own citations must also reach the step.
    for binding in inherited {
        if !sink.iter().any(|(existing, _)| *existing == binding.0) {
            sink.push(binding);
        }
    }
    if !sink.iter().any(|(existing, _)| *existing == name) {
        sink.push((name.clone(), data));
    }

    SelectorSegment::BoundIndex {
        index: value,
        source: name,
        width: I::WIDTH,
    }
}

/// Attach the index bindings collected by a `select!` to the reference it
/// produced, so every step that later reads it also records them.
#[doc(hidden)]
pub fn attach_index_bindings<T>(value: AuthRef<T>, bindings: Vec<IndexBinding>) -> AuthRef<T> {
    if bindings.is_empty() {
        return value;
    }
    match value {
        // Unreachable through `select!` (selecting on an inline value already
        // panics), but there is no correct way to attach a citation to a value
        // that has no path, so say so rather than drop it silently.
        AuthRef::Inline(_) => {
            panic!("a select! with a dynamic index requires a storage-backed source")
        }
        AuthRef::Storage(mut binding) => {
            for entry in bindings {
                if !binding
                    .index_bindings
                    .iter()
                    .any(|(existing, _)| *existing == entry.0)
                {
                    binding.index_bindings.push(entry);
                }
            }
            AuthRef::Storage(binding)
        }
    }
}

pub fn selector_path(segments: Vec<SelectorSegment>) -> SelectorPath {
    SelectorPath::new(segments)
}

#[doc(hidden)]
pub fn serialize_draft_trace<S>(draft: &Draft<S>) -> Vec<u8>
where
    S: Schema,
{
    let _ = draft;
    raster_core::postcard::to_allocvec(&draft_trace_marker::<S>()).unwrap_or_default()
}

/// Produce an owned copy of a value through its postcard encoding.
///
/// Used by the inline [`ListCursor`] to hand out an item without requiring
/// `T: Clone` on every recur element type — that bound would have to propagate
/// through the nine `run_recur_*` signatures and out to `call_recur!`, breaking
/// programs whose element types are not `Clone`. It is not extra cost relative
/// to the storage path, which deserializes a fresh value per item anyway; it is
/// only a cost relative to a clone, and it is confined to a source that came
/// from a tile in an unauthenticated run.
fn reencode_value<T>(value: &T) -> raster_core::Result<T>
where
    T: Serialize + DeserializeOwned,
{
    let bytes = raster_core::postcard::to_allocvec(value).map_err(|error| {
        raster_core::Error::Serialization(alloc::format!(
            "Failed to encode inline recur item: {}",
            error
        ))
    })?;
    raster_core::postcard::from_bytes(&bytes).map_err(|error| {
        raster_core::Error::Serialization(alloc::format!(
            "Failed to decode inline recur item: {}",
            error
        ))
    })
}

/// An authenticated cursor over a stored list, opened without reading it.
///
/// **Invariant: every recur source is raster-indexed, and no recur ever
/// materializes its source whole.** There is no second backend and no
/// materializing fallback, so there is no path that reintroduces `O(list)` —
/// see `docs/proposals/lazy-list-recur.md` §3.
///
/// `len` is authenticated at [`ListCursor::open`] by the `0x0A` metadata
/// selection, not read off the index: a nested list node's `len` is
/// index-trusted, and trusting it is the forged-`len = 0` sweep. Items and
/// ranges are then reached by their own selection proofs against the same
/// committed root.
///
/// The parsed index is *not* held here — `RasterIndex` is private to
/// `raster-runtime`, and the retention that matters (not re-parsing per item)
/// belongs to the storage layer that owns it, which is where every
/// `select_item` call lands anyway.
#[doc(hidden)]
pub enum ListCursor<T> {
    Storage {
        reference: StorageRef,
        /// Path from `reference`'s stored root down to the list itself. Item and
        /// range selectors extend it.
        selector: SelectorPath,
        len: u64,
        /// Citations carried by the source list's own selector, inherited by every
        /// item path. Without them a recur over a list reached through a bound
        /// index emits per-item paths whose cited source never reaches the step,
        /// and `verify_bound_index_bindings` rejects the selection outright.
        index_bindings: Vec<IndexBinding>,
        marker: PhantomData<fn() -> T>,
    },
    /// A source that is already in memory: a tile output in an unauthenticated
    /// run. The invariant above is about never *materializing* a stored list;
    /// here the elements already exist, so there is nothing left to keep lazy.
    /// Only reachable with authentication off — an inline source in an
    /// authenticated run is still rejected by [`ListCursor::open`].
    Inline(List<T>),
}

impl<T> ListCursor<T>
where
    T: DeserializeOwned + Serialize + Selectable + 'static,
{
    /// Open a cursor over a recur source, authenticating its length.
    ///
    /// Refuses anything that is not a raster-indexed storage list. That is a
    /// real restriction on existing programs and the right one: postcard is
    /// sequential and not self-indexing, so `rows[i]` cannot be located without
    /// decoding everything before it, and keeping a fallback would preserve
    /// exactly the `O(list)` peak this exists to remove — on the input class
    /// most likely to be large, since it came from a file.
    #[cfg(feature = "std")]
    pub fn open(source: &AuthRef<List<T>>) -> raster_core::Result<Self> {
        let binding = match source {
            AuthRef::Storage(binding) => binding,
            // A tile-produced list in an unauthenticated run. The rule above
            // exists to stop a *stored* list being pulled in whole; this one is
            // already in memory, so refusing it would forbid the case for no
            // benefit. Authenticated runs keep refusing it — there, an inline
            // source means a list with no lineage.
            AuthRef::Inline(list) => {
                if crate::auth_mode().is_authenticated() {
                    return Err(raster_core::Error::Other(
                        "call_recur! requires a raster-indexed List source; \
                         re-encode this input with encoding = \"raster\""
                            .into(),
                    ));
                }
                return Ok(Self::Inline(reencode_value(list)?));
            }
        };

        let metadata =
            raster_runtime::stored_list_metadata(&binding.reference, &binding.selector)?;

        Ok(Self::Storage {
            reference: binding.reference.clone(),
            selector: binding.selector.clone(),
            len: metadata.len,
            index_bindings: binding.index_bindings.clone(),
            marker: PhantomData,
        })
    }

    /// The authenticated element count. This is the `L` every completeness
    /// rule is stated against.
    pub fn len(&self) -> u64 {
        match self {
            Self::Storage { len, .. } => *len,
            Self::Inline(list) => list.len() as u64,
        }
    }

    /// One element, as its own authenticated reference.
    ///
    /// Errors propagate. The previous per-item path fell back to resolving the
    /// whole parent when an indexed read failed, which treated a malformed
    /// index and an out-of-range selector identically to "this source has no
    /// index" — turning a corruption signal into a multi-gigabyte allocation.
    /// With no materializing backend there is nowhere to fall back *to*, so
    /// every failure here is a failure.
    pub fn select_item(&self, index: u64) -> raster_core::Result<AuthRef<T>> {
        if let Self::Inline(list) = self {
            let item = list.as_slice().get(index as usize).ok_or_else(|| {
                raster_core::Error::Other(alloc::format!(
                    "recur item {} is out of range for an inline source of {}",
                    index,
                    list.len()
                ))
            })?;
            return Ok(AuthRef::Inline(reencode_value(item)?));
        }
        self.select_at(SelectorSegment::Index(index))
    }

    /// A contiguous chunk, as one authenticated reference.
    ///
    /// The proof is a `ListRange` over the real `List<T>` — there is no
    /// synthetic `List<Block<T>>` anywhere in the tree to prove membership in,
    /// which is why chunking has to be a driver-level range selection rather
    /// than a type-changing adapter (§6).
    pub fn select_range(&self, start: u64, end: u64) -> raster_core::Result<AuthRef<Block<T>>> {
        if let Self::Inline(list) = self {
            let slice = list
                .as_slice()
                .get(start as usize..end as usize)
                .ok_or_else(|| {
                    raster_core::Error::Other(alloc::format!(
                        "recur range {}..{} is out of bounds for an inline source of {}",
                        start,
                        end,
                        list.len()
                    ))
                })?;
            let items = slice
                .iter()
                .map(reencode_value)
                .collect::<raster_core::Result<Vec<T>>>()?;
            return Ok(AuthRef::Inline(Block::__from_selection(items)));
        }
        self.select_at(SelectorSegment::Range { start, end })
    }

    fn select_at<Selected>(
        &self,
        segment: SelectorSegment,
    ) -> raster_core::Result<AuthRef<Selected>>
    where
        Selected: DeserializeOwned + Serialize + 'static,
    {
        let Self::Storage {
            reference,
            selector,
            index_bindings,
            ..
        } = self
        else {
            // Both public callers handle the inline variant before reaching
            // here; this arm exists so the enum match is total.
            return Err(raster_core::Error::Other(
                "select_at is only defined for a storage-backed recur source".into(),
            ));
        };

        let mut item_selector = selector.clone();
        item_selector.segments.push(segment);
        let resolve_selector = item_selector.clone();

        Ok(AuthRef::Storage(DeferredAuthStorage {
            reference: reference.clone(),
            selector: item_selector,
            // A recur item's index is the loop counter, whose provenance is
            // structural (the CFS pins the driver) — it emits a plain `Index`
            // and cites nothing. Any citation already on the list binding is
            // inherited.
            index_bindings: index_bindings.clone(),
            resolve: Rc::new(move |reference| {
                select_stored_value::<Selected>(&reference, &resolve_selector)
            }),
            marker: PhantomData,
        }))
    }
}

/// Materialize one authorized recur item into the value the tile receives,
/// keeping the binding that authorizes it.
///
/// Materialization has **two** outputs. `into_auth_value(...).into_inner()`
/// keeps the value and discards the selector, the selection commitment and any
/// inherited citations — before the iteration's trace can record them. That is
/// the whole of the bug `build_recur_input` embodied, and the shape here is the
/// established one, not a new invention: `IndexSource::resolve_index` and
/// `program_output_binding` both already return "the value, plus what
/// authorizes it".
///
/// The second output is handed to the tile wrapper through a host-side stash
/// rather than through `RecurInput`, so the replay ABI and every recur tile's
/// image id stay exactly where they are. See
/// [`raster_runtime::stash_recur_item_binding`].
#[doc(hidden)]
#[cfg(feature = "std")]
pub fn materialize_recur_item<T>(
    item: AuthRef<T>,
    index: u64,
    len: u64,
) -> raster_core::Result<RecurInput<T>>
where
    T: DeserializeOwned + Serialize,
{
    let (auth_value, index_bindings) = into_auth_value_with_bindings::<T, _>(item)?;

    if let Some(stored) = auth_value.as_storage() {
        raster_runtime::stash_recur_item_binding(
            TraceStorageData {
                coordinates: stored.reference.coordinates.clone(),
                commitment: stored.reference.commitment.clone(),
                selector: stored.selector.clone(),
                selection: stored.selection.clone(),
            },
            index_bindings,
        );
    }

    Ok(RecurInput::new(auth_value.into_inner(), index, len))
}

/// Select and materialize iteration `index`, recording what authorized it.
///
/// Both failures are panics rather than fallbacks. There is no materializing
/// backend to fall back *to*, so a malformed index and an out-of-range
/// selector are what they look like — failures — instead of a signal to
/// resolve the whole list.
#[cfg(feature = "std")]
fn recur_iteration_input<T>(cursor: &ListCursor<T>, index: u64, len: u64) -> RecurInput<T>
where
    T: DeserializeOwned + Serialize + Selectable + 'static,
{
    let item = cursor
        .select_item(index)
        .unwrap_or_else(|error| panic!("Failed to select recur item {}: {}", index, error));
    materialize_recur_item(item, index, len)
        .unwrap_or_else(|error| panic!("Failed to materialize recur item {}: {}", index, error))
}

impl<T> IntoAuthRef<T> for T
where
    T: Serialize,
{
    fn into_auth_ref(self) -> AuthRef<T> {
        AuthRef::Inline(self)
    }
}

impl<Root> IntoAuthRef<Root> for TypedStorageBinding<Root>
where
    Root: DeserializeOwned + Serialize + 'static,
{
    fn into_auth_ref(self) -> AuthRef<Root> {
        let reference = self.reference;
        let resolve = self.resolve;
        AuthRef::Storage(DeferredAuthStorage {
            reference,
            selector: SelectorPath::default(),
            // An empty path cites nothing.
            index_bindings: Vec::new(),
            resolve: Rc::new(move |reference| (resolve)(reference)),
            marker: PhantomData,
        })
    }
}

impl<Current> IntoAuthRef<Current> for AuthRef<Current> {
    fn into_auth_ref(self) -> AuthRef<Current> {
        self
    }
}

pub fn into_auth_ref<T, A>(arg: A) -> AuthRef<T>
where
    A: IntoAuthRef<T>,
{
    arg.into_auth_ref()
}

pub trait IntoAuthValue<T> {
    fn into_auth_value(self) -> raster_core::Result<AuthValue<T>>;

    /// Materialize, and also surrender the storage bindings this argument's path
    /// cites through a `BoundIndex` segment.
    ///
    /// Materializing a reference resolves it to a `StorageValue`, which carries
    /// no citations — so without this, passing a dynamically-indexed value into a
    /// tile would record the value while dropping the binding that authorizes
    /// its index, and the verifier would reject the step for a missing source.
    /// Defaulted to "no citations", which is correct for every argument form
    /// except [`AuthRef`]: only a reference can have been built by a `select!`.
    fn into_auth_value_with_bindings(self) -> raster_core::Result<(AuthValue<T>, Vec<IndexBinding>)>
    where
        Self: Sized,
    {
        Ok((self.into_auth_value()?, Vec::new()))
    }
}

/// The bounded tile-argument boundary: every plain tile argument is materialized
/// through this trait, whose target `T` must be [`Materializable`]. `IntoAuthValue`
/// remains the untyped mechanism (recur internals, state threading); a tile
/// boundary only ever uses `IntoMaterialized`, so an unbounded collection — or an
/// inline `vec![..]` literal — cannot cross it.
pub trait IntoMaterialized<T: Materializable>: IntoAuthValue<T> {}

impl<T, A> IntoMaterialized<T> for A
where
    T: Materializable,
    A: IntoAuthValue<T>,
{
}

pub trait IntoDraft<S: Schema> {
    fn into_draft(self) -> Draft<S>;
}

impl<S> IntoDraft<S> for Draft<S>
where
    S: Schema,
{
    fn into_draft(self) -> Draft<S> {
        self
    }
}

impl<S> IntoDraft<S> for RecurSequenceOutput<S>
where
    S: Schema,
{
    fn into_draft(self) -> Draft<S> {
        self.inner
    }
}

pub fn into_draft<S, D>(draft: D) -> Draft<S>
where
    S: Schema,
    D: IntoDraft<S>,
{
    draft.into_draft()
}

impl<T> IntoAuthValue<T> for T
where
    T: Serialize,
{
    fn into_auth_value(self) -> raster_core::Result<AuthValue<T>> {
        Ok(AuthValue::inline(self))
    }
}

impl<Root> IntoAuthValue<Root> for TypedStorageBinding<Root>
where
    Root: DeserializeOwned + Serialize,
{
    fn into_auth_value(self) -> raster_core::Result<AuthValue<Root>> {
        let value = (self.resolve)(self.reference)?;
        Ok(AuthValue::storage(value))
    }
}

impl<Current> IntoAuthValue<Current> for AuthRef<Current>
where
    Current: Serialize,
{
    fn into_auth_value(self) -> raster_core::Result<AuthValue<Current>> {
        match self {
            AuthRef::Inline(value) => Ok(AuthValue::inline(value)),
            AuthRef::Storage(binding) => {
                let value = (binding.resolve.as_ref())(binding.reference)?;
                Ok(AuthValue::storage(value))
            }
        }
    }

    fn into_auth_value_with_bindings(
        self,
    ) -> raster_core::Result<(AuthValue<Current>, Vec<IndexBinding>)> {
        match self {
            AuthRef::Inline(value) => Ok((AuthValue::inline(value), Vec::new())),
            AuthRef::Storage(binding) => {
                let bindings = binding.index_bindings.clone();
                let value = (binding.resolve.as_ref())(binding.reference)?;
                Ok((AuthValue::storage(value), bindings))
            }
        }
    }
}

impl<T> IntoAuthRef<T> for RecurSequenceInput<T> {
    fn into_auth_ref(self) -> AuthRef<T> {
        self.item
    }
}

impl<T> IntoAuthValue<T> for RecurSequenceInput<T>
where
    T: Serialize,
{
    fn into_auth_value(self) -> raster_core::Result<AuthValue<T>> {
        self.item.into_auth_value()
    }

    /// Forwards to the item: a recur-sequence item is an `AuthRef`, and if the
    /// list it iterates was itself reached through a bound index, the item's
    /// path carries that citation.
    fn into_auth_value_with_bindings(
        self,
    ) -> raster_core::Result<(AuthValue<T>, Vec<IndexBinding>)> {
        self.item.into_auth_value_with_bindings()
    }
}

impl<T> IntoAuthValue<T> for RecurSequenceState<T>
where
    T: Serialize,
{
    fn into_auth_value(self) -> raster_core::Result<AuthValue<T>> {
        Ok(AuthValue::inline(self.inner))
    }
}

impl<T> From<T> for RecurSequenceState<T> {
    fn from(value: T) -> Self {
        Self { inner: value }
    }
}

impl<T> From<AuthRef<T>> for RecurSequenceState<T>
where
    T: DeserializeOwned + Serialize,
{
    fn from(value: AuthRef<T>) -> Self {
        Self {
            inner: into_auth_value::<T, _>(value)
                .unwrap_or_else(|error| {
                    panic!(
                        "Failed to materialize recursive sequence state from tile output: {}",
                        error
                    )
                })
                .into_inner(),
        }
    }
}

impl<S> From<Draft<S>> for RecurSequenceOutput<S>
where
    S: Schema,
{
    fn from(value: Draft<S>) -> Self {
        Self { inner: value }
    }
}

pub fn into_auth_value<T, A>(arg: A) -> raster_core::Result<AuthValue<T>>
where
    A: IntoAuthValue<T>,
{
    arg.into_auth_value()
}

/// [`into_auth_value`], also yielding the argument's index citations.
///
/// This is what tile-argument materialization calls, so a dynamically-indexed
/// value and the binding authorizing its index reach the step's storage map
/// together.
pub fn into_auth_value_with_bindings<T, A>(
    arg: A,
) -> raster_core::Result<(AuthValue<T>, Vec<IndexBinding>)>
where
    A: IntoAuthValue<T>,
{
    arg.into_auth_value_with_bindings()
}

/// Selections at or above this size are worth remembering across repeats.
///
/// The threshold is what keeps the memo self-limiting: an entry is only ever
/// created by a resolution that already moved at least this many bytes, and it
/// costs ~200. So the memo can never grow to more than a fraction of a percent
/// of the work already done, without any eviction policy to get wrong.
#[cfg(feature = "std")]
const TRACE_MEMO_MIN_SELECTED_LEN: u64 = 64 * 1024;

#[cfg(feature = "std")]
std::thread_local! {
    /// Memoizes the storage half of [`auth_ref_trace`], keyed by the
    /// `(coordinates, commitment, selector)` triple that determines it.
    ///
    /// Sound because the key names immutable content: storage is append-only,
    /// and `verify_reference` holds an object at fixed coordinates to a fixed
    /// commitment, so the same key can only ever describe the same bytes and
    /// therefore the same `SelectionCommitment`.
    static THREAD_AUTH_REF_TRACE_MEMO: core::cell::RefCell<
        alloc::collections::BTreeMap<Vec<u8>, TraceStorageData>,
    > = core::cell::RefCell::new(alloc::collections::BTreeMap::new());
}

/// The storage half of an `AuthRef`'s trace record.
///
/// Split out of [`auth_ref_trace`] so that the resolution it needs — which is
/// eager, and on a whole-collection selector reads and decodes the entire
/// collection — happens **once per distinct binding** instead of once per
/// call. That difference is the whole point: a `#[sequence(kind = recur)]`
/// re-enters its wrapper for every item, and traces every parameter each time,
/// so an un-memoized big argument is re-decoded on every iteration even when
/// the body never touches it.
#[cfg(feature = "std")]
fn auth_ref_trace_storage<T>(
    binding: &DeferredAuthStorage<T>,
) -> raster_core::Result<TraceStorageData> {
    let key = raster_core::postcard::to_allocvec(&(
        &binding.reference.coordinates,
        &binding.reference.commitment,
        &binding.selector,
    ))
    .unwrap_or_default();

    if !key.is_empty() {
        if let Some(hit) =
            THREAD_AUTH_REF_TRACE_MEMO.with(|memo| memo.borrow().get(&key).cloned())
        {
            return Ok(hit);
        }
    }

    let resolved = (binding.resolve.as_ref())(binding.reference.clone())?;
    let storage = TraceStorageData {
        coordinates: resolved.reference.coordinates,
        commitment: resolved.reference.commitment,
        selector: resolved.selector,
        selection: resolved.selection,
    };

    if !key.is_empty() && storage.selection.selected_len >= TRACE_MEMO_MIN_SELECTED_LEN {
        THREAD_AUTH_REF_TRACE_MEMO.with(|memo| {
            memo.borrow_mut().insert(key, storage.clone());
        });
    }

    Ok(storage)
}

#[cfg(not(feature = "std"))]
fn auth_ref_trace_storage<T>(
    binding: &DeferredAuthStorage<T>,
) -> raster_core::Result<TraceStorageData> {
    let resolved = (binding.resolve.as_ref())(binding.reference.clone())?;
    Ok(TraceStorageData {
        coordinates: resolved.reference.coordinates,
        commitment: resolved.reference.commitment,
        selector: resolved.selector,
        selection: resolved.selection,
    })
}

pub fn auth_ref_trace<T>(arg: &AuthRef<T>) -> raster_core::Result<AuthRefTrace>
where
    T: Serialize + DeserializeOwned,
{
    match arg {
        AuthRef::Inline(value) => Ok(AuthRefTrace {
            value: FnInputValue::Inline(
                raster_core::postcard::to_allocvec(value).unwrap_or_default(),
            ),
            storage: None,
            index_bindings: Vec::new(),
        }),
        AuthRef::Storage(binding) => Ok(AuthRefTrace {
            value: FnInputValue::StorageBinding,
            storage: Some(auth_ref_trace_storage(binding)?),
            index_bindings: binding.index_bindings.clone(),
        }),
    }
}

/// Trace a recur source without resolving it.
///
/// [`auth_ref_trace`] exists to obtain a `SelectionCommitment`, and for a
/// whole-list selector the payload it commits to is the entire list — so it
/// resolves the binding, throws the value away (`resolved.value` is never
/// read), and keeps only the commitment. On a recur source that is the
/// earliest and largest of the three eager paths: it runs *before any runner*,
/// so nothing downstream can be lazy.
///
/// This replaces it with the `0x0A` metadata selection of `lazy-list-recur.md`
/// §1: the same coordinates, commitment, selector and citations, over a
/// payload of 41 bytes (9 when empty) instead of the list. The bound it
/// commits to is *more* authenticated than before, not less — the root is
/// recomputed from `(len, elements_root)`, where previously a nested list
/// node's `len` was only index-trusted.
///
/// Inline sources are refused here rather than traced: a recur source must be
/// a selectable storage list, and `ListCursor::open` says so with the message
/// authors are meant to act on.
#[cfg(feature = "std")]
pub fn recur_source_trace<T>(arg: &AuthRef<List<T>>) -> raster_core::Result<AuthRefTrace>
where
    T: Serialize + DeserializeOwned,
{
    match arg {
        AuthRef::Inline(_) => Err(raster_core::Error::Other(
            "call_recur! requires a raster-indexed List source; \
             re-encode this input with encoding = \"raster\""
                .into(),
        )),
        AuthRef::Storage(binding) => {
            let metadata =
                raster_runtime::stored_list_metadata(&binding.reference, &binding.selector)?;
            Ok(AuthRefTrace {
                value: FnInputValue::StorageBinding,
                storage: Some(TraceStorageData {
                    coordinates: binding.reference.coordinates.clone(),
                    commitment: binding.reference.commitment.clone(),
                    selector: binding.selector.clone(),
                    selection: metadata.selected.commitment,
                }),
                index_bindings: binding.index_bindings.clone(),
            })
        }
    }
}

pub fn auth_ref_result_trace<T>(
    result: &core::result::Result<AuthRef<T>, String>,
) -> raster_core::Result<core::result::Result<AuthRefTrace, String>>
where
    T: Serialize + DeserializeOwned,
{
    match result {
        Ok(value) => Ok(Ok(auth_ref_trace(value)?)),
        Err(error) => Ok(Err(error.clone())),
    }
}

/// Resolve `main`'s returned `AuthRef` to its committed storage binding and
/// the materialized value in a single storage read — used by
/// [`end_program_output`] to build both the `ProgramEnd` output binding and
/// the output artifact.
///
/// Rejects an inline (non-storage) result: a program's output must be a
/// committed value with provable lineage (a tile or `select!` result), never
/// an arbitrary in-body literal.
fn program_output_binding<T>(result: &AuthRef<T>) -> raster_core::Result<(TraceStorageData, T)>
where
    T: Serialize + DeserializeOwned,
{
    match result {
        AuthRef::Inline(_) => Err(raster_core::Error::Other(
            "program output must be a stored value (a tile or select! result), \
             not an inline literal"
                .into(),
        )),
        AuthRef::Storage(binding) => {
            let resolved = (binding.resolve.as_ref())(binding.reference.clone())?;
            let storage = TraceStorageData {
                coordinates: resolved.reference.coordinates.clone(),
                commitment: resolved.reference.commitment.clone(),
                selector: resolved.selector.clone(),
                selection: resolved.selection.clone(),
            };
            Ok((storage, resolved.value))
        }
    }
}

/// Emit the program's terminal `ProgramEnd` event for a `main` that returns
/// unit — it produces no output artifact and binds no storage.
#[cfg(feature = "std")]
pub fn end_program_unit() {
    raster_runtime::publish_trace_event(raster_core::trace::TraceEvent::ProgramEnd(
        raster_core::trace::ProgramEndEvent { output: None },
    ));
}

/// Emit the program's terminal `ProgramEnd` event for a `main` that returns a
/// value, and export that value as the program's output artifact (see
/// `raster_runtime::write_program_output_artifact`). The output must be a
/// storage-backed `AuthRef`; an inline literal is rejected.
#[cfg(feature = "std")]
pub fn end_program_output<T>(result: &AuthRef<T>)
where
    T: Serialize + DeserializeOwned,
{
    // "The output must be storage-backed" is a statement about lineage, and an
    // unauthenticated run has none to offer — `main` returns an inline value
    // here by construction. The artifact is still written: its hash commits to
    // the output *bytes*, which is honest in either mode, and a cheap stage
    // that still produces one is what the chain work needs (§6.1). There is no
    // `ProgramEnd` event because there is no trace to carry it.
    if !crate::auth_mode().is_authenticated() {
        let write = |value: &T| {
            raster_runtime::write_program_output_artifact(value)
                .unwrap_or_else(|error| panic!("Failed to write program output artifact: {}", error))
        };
        match result {
            AuthRef::Inline(value) => write(value),
            AuthRef::Storage(binding) => {
                let resolved = (binding.resolve.as_ref())(binding.reference.clone())
                    .unwrap_or_else(|error| panic!("Failed to resolve program output: {}", error));
                write(&resolved.value)
            }
        };
        return;
    }

    let (storage, value) = program_output_binding(result)
        .unwrap_or_else(|error| panic!("Failed to resolve program output: {}", error));
    raster_runtime::write_program_output_artifact(&value)
        .unwrap_or_else(|error| panic!("Failed to write program output artifact: {}", error));
    raster_runtime::publish_trace_event(raster_core::trace::TraceEvent::ProgramEnd(
        raster_core::trace::ProgramEndEvent {
            output: Some(storage),
        },
    ));
}

#[cfg(feature = "std")]
pub fn raster_trace_payload<T: Serialize>(
    value: &T,
) -> raster_core::Result<raster_core::trace::RasterPayload> {
    let (bytes, index_bytes, commitment) = raster_runtime::encode_raster_value(value)?;
    let mut root_hash = Vec::with_capacity(commitment.len() / 2);
    let chars: Vec<char> = commitment.chars().collect();
    for pair in chars.chunks(2) {
        if pair.len() != 2 {
            return Err(raster_core::Error::Serialization(
                "Malformed raster commitment hex".into(),
            ));
        }
        let hi = pair[0].to_digit(16).ok_or_else(|| {
            raster_core::Error::Serialization("Malformed raster commitment hex".into())
        })?;
        let lo = pair[1].to_digit(16).ok_or_else(|| {
            raster_core::Error::Serialization("Malformed raster commitment hex".into())
        })?;
        root_hash.push(((hi << 4) | lo) as u8);
    }

    let root_hash = root_hash.try_into().map_err(|_| {
        raster_core::Error::Serialization("Malformed raster commitment hash length".into())
    })?;

    Ok(raster_core::trace::RasterPayload {
        bytes,
        index_bytes,
        root_hash,
    })
}

#[cfg(not(feature = "std"))]
pub fn raster_trace_payload<T: Serialize>(
    _value: &T,
) -> raster_core::Result<raster_core::trace::RasterPayload> {
    Err(raster_core::Error::Other(
        "Raster trace payload generation requires the `std` feature".into(),
    ))
}

pub fn select_storage_value<Root, T>(
    value: &StorageValue<Root>,
    selector: &SelectorPath,
) -> raster_core::Result<StorageValue<T>>
where
    Root: DeserializeOwned + Serialize + Selectable,
    T: DeserializeOwned + Serialize,
{
    #[cfg(feature = "std")]
    {
        return raster_runtime::select_storage_value::<Root, T>(value, selector);
    }

    #[cfg(not(feature = "std"))]
    {
        let _ = value;
        let _ = selector;
        Err(raster_core::Error::Other(format!(
            "Storage selection refinement requires the `std` feature"
        )))
    }
}

pub fn select_stored_value<T>(
    reference: &StorageRef,
    selector: &SelectorPath,
) -> raster_core::Result<StorageValue<T>>
where
    T: DeserializeOwned + Serialize,
{
    #[cfg(feature = "std")]
    {
        return raster_runtime::select_stored_value::<T>(reference, selector);
    }

    #[cfg(not(feature = "std"))]
    {
        let _ = reference;
        let _ = selector;
        Err(raster_core::Error::Other(alloc::format!(
            "Storage raster selection requires the `std` feature"
        )))
    }
}

pub fn resolve_storage_value<T: DeserializeOwned + Serialize>(
    reference: StorageRef,
) -> raster_core::Result<raster_core::input::StorageValue<T>> {
    #[cfg(feature = "std")]
    {
        return raster_runtime::resolve_storage_value(&reference);
    }

    #[cfg(not(feature = "std"))]
    {
        let _ = reference;
        Err(raster_core::Error::Other(alloc::format!(
            "Storage input resolution requires the `std` feature"
        )))
    }
}

pub fn resolve_storage_ok_value<T: DeserializeOwned + Serialize>(
    reference: StorageRef,
) -> raster_core::Result<raster_core::input::StorageValue<T>> {
    #[cfg(feature = "std")]
    {
        return raster_runtime::resolve_storage_ok_value(&reference);
    }

    #[cfg(not(feature = "std"))]
    {
        let _ = reference;
        Err(raster_core::Error::Other(alloc::format!(
            "Result-backed storage input resolution requires the `std` feature"
        )))
    }
}

/// Implementation of [`clone!`](crate::clone) — call the macro, not this.
///
/// A single choke point for duplicating a sequence binding. Bindings are
/// *references*, so this copies a handle, never data.
///
/// Taking `&T` rather than `self` keeps the macro's argument borrowed, which is
/// what lets `clone!(x)` appear in an argument list without moving `x`. The
/// `Clone` bound is what keeps linear handles linear: `Draft<S>` is deliberately
/// not `Clone`, so `clone!(draft)` does not compile — the same rule the
/// `draft_handle_cannot_clone` UI test pins.
#[doc(hidden)]
pub fn __raster_clone<T>(value: &T) -> T
where
    T: Clone,
{
    value.clone()
}

pub fn new_draft<S>() -> Draft<S>
where
    S: Schema,
{
    #[cfg(feature = "std")]
    {
        let (anchor, current_root) = raster_runtime::create_draft::<S>().unwrap_or_else(|error| {
            panic!(
                "Failed to create draft '{}': {}",
                core::any::type_name::<S>(),
                error
            )
        });
        return Draft::new(anchor, current_root);
    }

    #[cfg(not(feature = "std"))]
    {
        panic!("Draft creation requires the `std` feature")
    }
}

pub fn finalize<S>(draft: Draft<S>) -> AuthRef<S>
where
    S: Schema + DeserializeOwned + Serialize + 'static,
{
    #[cfg(feature = "std")]
    {
        if !crate::auth_mode().is_authenticated() {
            let value =
                raster_runtime::finalize_draft_value::<S>(draft.anchor(), draft.current_root(), true)
                    .unwrap_or_else(|error| {
                        panic!(
                            "Failed to finalize draft '{}': {}",
                            core::any::type_name::<S>(),
                            error
                        )
                    });
            return AuthRef::Inline(value);
        }
        let reference = raster_runtime::finalize_draft::<S>(draft.anchor(), draft.current_root())
            .unwrap_or_else(|error| {
                panic!(
                    "Failed to finalize draft '{}': {}",
                    core::any::type_name::<S>(),
                    error
                )
            });
        return into_auth_ref::<S, _>(typed_storage::<S>(reference));
    }

    #[cfg(not(feature = "std"))]
    {
        let _ = draft;
        panic!("Draft finalization requires the `std` feature")
    }
}

fn finalize_recur_output<S>(draft: Draft<S>, allow_partial: bool) -> AuthRef<S>
where
    S: Schema + DeserializeOwned + Serialize + 'static,
{
    #[cfg(feature = "std")]
    {
        // `allow_partial` is the empty-recur path: an output draft no iteration
        // ever touched must still materialize if the schema permits it.
        let require_complete = !allow_partial;
        if !crate::auth_mode().is_authenticated() {
            let value = raster_runtime::finalize_draft_value::<S>(
                draft.anchor(),
                draft.current_root(),
                require_complete,
            )
            .unwrap_or_else(|error| {
                panic!(
                    "Failed to finalize draft '{}': {}",
                    core::any::type_name::<S>(),
                    error
                )
            });
            return AuthRef::Inline(value);
        }
        let reference = if allow_partial {
            raster_runtime::finalize_empty_draft::<S>(draft.anchor(), draft.current_root())
        } else {
            raster_runtime::finalize_draft::<S>(draft.anchor(), draft.current_root())
        }
        .unwrap_or_else(|error| {
            panic!(
                "Failed to finalize draft '{}': {}",
                core::any::type_name::<S>(),
                error
            )
        });
        return into_auth_ref::<S, _>(typed_storage::<S>(reference));
    }

    #[cfg(not(feature = "std"))]
    {
        let _ = draft;
        let _ = allow_partial;
        panic!("Draft finalization requires the `std` feature")
    }
}

#[doc(hidden)]
pub fn run_recur_list<T, S, Step, Output>(
    source: AuthRef<List<T>>,
    output: Draft<S>,
    mut step: Step,
) -> AuthRef<S>
where
    T: DeserializeOwned + Serialize + Selectable + 'static,
    S: Schema + DeserializeOwned + Serialize + 'static,
    Step: FnMut(RecurInput<T>, RecurOutput<S>) -> Output,
    Output: IntoRecurControl<RecurOutput<S>>,
{
    #[cfg(feature = "std")]
    {
        let cursor = ListCursor::open(&source)
            .unwrap_or_else(|error| panic!("Failed to open recursive list source: {}", error));
        let len = cursor.len();
        if len == 0 {
            return finalize_recur_output(output, true);
        }
        let mut output = output;

        for index in 0..len {
            let input = recur_iteration_input(&cursor, index, len);

            match step(input, output).into_recur_control() {
                RecurControl::Continue(next) => {
                    output = next;
                }
                RecurControl::Break(done) => {
                    output = done;
                    break;
                }
            }
        }

        return finalize_recur_output(output, false);
    }

    #[cfg(not(feature = "std"))]
    {
        let _ = source;
        let _ = output;
        let _ = step;
        panic!("Recursive list execution requires the `std` feature")
    }
}

/// Iteration count for a chunked sweep: `⌈len / chunk⌉`.
///
/// `chunk` is a CFS literal, so it is already part of program identity; `max(1)`
/// only guards a nonsensical `chunk = 0` from producing a division trap.
#[cfg(feature = "std")]
fn chunk_iteration_count(len: u64, chunk: u64) -> u64 {
    len.div_ceil(chunk.max(1))
}

/// Select and materialize chunk `index` of a chunked sweep.
///
/// The range is `[i·C, min((i+1)·C, L))`, so only the final chunk is ever
/// short — which is exactly the shape the completeness rules hold every
/// iteration to (§5 rule 4).
#[cfg(feature = "std")]
fn recur_chunk_input<T>(
    cursor: &ListCursor<T>,
    index: u64,
    iterations: u64,
    chunk: u64,
    len: u64,
) -> RecurInput<Block<T>>
where
    T: DeserializeOwned + Serialize + Selectable + 'static,
{
    let chunk = chunk.max(1);
    let start = index * chunk;
    let end = core::cmp::min(start + chunk, len);
    let block = cursor
        .select_range(start, end)
        .unwrap_or_else(|error| panic!("Failed to select recur chunk {}..{}: {}", start, end, error));
    materialize_recur_item(block, index, iterations)
        .unwrap_or_else(|error| panic!("Failed to materialize recur chunk {}: {}", index, error))
}

/// `call_recur! { ..., chunk = N }`, output-only.
///
/// The source stays `AuthRef<List<T>>` in both modes — no synthetic
/// `List<Block<T>>` is ever constructed, because none exists in the storage
/// tree to prove membership in. `RecurInput.len` remains the **iteration**
/// count, so `is_first`/`is_last` keep their current meanings inside the step.
#[doc(hidden)]
pub fn run_recur_chunked_list<T, S, Step, Output>(
    source: AuthRef<List<T>>,
    chunk: u64,
    output: Draft<S>,
    mut step: Step,
) -> AuthRef<S>
where
    T: DeserializeOwned + Serialize + Selectable + 'static,
    S: Schema + DeserializeOwned + Serialize + 'static,
    Step: FnMut(RecurInput<Block<T>>, RecurOutput<S>) -> Output,
    Output: IntoRecurControl<RecurOutput<S>>,
{
    #[cfg(feature = "std")]
    {
        let cursor = ListCursor::open(&source)
            .unwrap_or_else(|error| panic!("Failed to open recursive list source: {}", error));
        let len = cursor.len();
        let iterations = chunk_iteration_count(len, chunk);
        if iterations == 0 {
            return finalize_recur_output(output, true);
        }
        let mut output = output;

        for index in 0..iterations {
            let input = recur_chunk_input(&cursor, index, iterations, chunk, len);

            match step(input, output).into_recur_control() {
                RecurControl::Continue(next) => output = next,
                RecurControl::Break(done) => {
                    output = done;
                    break;
                }
            }
        }

        return finalize_recur_output(output, false);
    }

    #[cfg(not(feature = "std"))]
    {
        let _ = (source, chunk, output, step);
        panic!("Recursive list execution requires the `std` feature")
    }
}

/// `call_recur! { ..., chunk = N }`, state-only.
#[doc(hidden)]
pub fn run_recur_chunked_list_state<T, State, Step, Output>(
    source: AuthRef<List<T>>,
    chunk: u64,
    state: RecurState<State>,
    mut step: Step,
) -> AuthRef<State>
where
    T: DeserializeOwned + Serialize + Selectable + 'static,
    State: DeserializeOwned + Serialize + 'static,
    Step: FnMut(RecurInput<Block<T>>, RecurState<State>) -> Output,
    Output: IntoRecurControl<RecurState<State>>,
{
    #[cfg(feature = "std")]
    {
        let cursor = ListCursor::open(&source)
            .unwrap_or_else(|error| panic!("Failed to open recursive list source: {}", error));
        let len = cursor.len();
        let iterations = chunk_iteration_count(len, chunk);
        let mut state = state;

        for index in 0..iterations {
            let input = recur_chunk_input(&cursor, index, iterations, chunk, len);

            match step(input, state).into_recur_control() {
                RecurControl::Continue(next_state) => state = next_state,
                RecurControl::Break(done_state) => {
                    state = done_state;
                    break;
                }
            }
        }

        return crate::__private::bind_infallible_call(state.into_inner());
    }

    #[cfg(not(feature = "std"))]
    {
        let _ = (source, chunk, state, step);
        panic!("Recursive list execution requires the `std` feature")
    }
}

/// `call_recur! { ..., chunk = N }`, state + output.
#[doc(hidden)]
pub fn run_recur_chunked_list_with_state<T, State, S, Step, Output>(
    source: AuthRef<List<T>>,
    chunk: u64,
    state: RecurState<State>,
    output: Draft<S>,
    mut step: Step,
) -> AuthRef<S>
where
    T: DeserializeOwned + Serialize + Selectable + 'static,
    State: DeserializeOwned + Serialize + 'static,
    S: Schema + DeserializeOwned + Serialize + 'static,
    Step: FnMut(RecurInput<Block<T>>, RecurState<State>, RecurOutput<S>) -> Output,
    Output: IntoRecurControl<(RecurState<State>, RecurOutput<S>)>,
{
    #[cfg(feature = "std")]
    {
        let cursor = ListCursor::open(&source)
            .unwrap_or_else(|error| panic!("Failed to open recursive list source: {}", error));
        let len = cursor.len();
        let iterations = chunk_iteration_count(len, chunk);
        if iterations == 0 {
            let _ = state;
            return finalize_recur_output(output, true);
        }
        let mut state = state;
        let mut output = output;

        for index in 0..iterations {
            let input = recur_chunk_input(&cursor, index, iterations, chunk, len);

            match step(input, state, output).into_recur_control() {
                RecurControl::Continue((next_state, next_output)) => {
                    state = next_state;
                    output = next_output;
                }
                RecurControl::Break((done_state, done_output)) => {
                    state = done_state;
                    output = done_output;
                    break;
                }
            }
        }

        let _ = state;
        return finalize_recur_output(output, false);
    }

    #[cfg(not(feature = "std"))]
    {
        let _ = (source, chunk, state, output, step);
        panic!("Recursive list execution requires the `std` feature")
    }
}

#[doc(hidden)]
pub fn run_recur_list_state<T, State, Step, Output>(
    source: AuthRef<List<T>>,
    state: RecurState<State>,
    mut step: Step,
) -> AuthRef<State>
where
    T: DeserializeOwned + Serialize + Selectable + 'static,
    State: DeserializeOwned + Serialize + 'static,
    Step: FnMut(RecurInput<T>, RecurState<State>) -> Output,
    Output: IntoRecurControl<RecurState<State>>,
{
    #[cfg(feature = "std")]
    {
        let cursor = ListCursor::open(&source)
            .unwrap_or_else(|error| panic!("Failed to open recursive list source: {}", error));
        let len = cursor.len();
        let mut state = state;

        for index in 0..len {
            let input = recur_iteration_input(&cursor, index, len);

            match step(input, state).into_recur_control() {
                RecurControl::Continue(next_state) => {
                    state = next_state;
                }
                RecurControl::Break(done_state) => {
                    state = done_state;
                    break;
                }
            }
        }

        return crate::__private::bind_infallible_call(state.into_inner());
    }

    #[cfg(not(feature = "std"))]
    {
        let _ = source;
        let _ = state;
        let _ = step;
        panic!("Recursive list execution requires the `std` feature")
    }
}

#[doc(hidden)]
pub fn run_recur_list_with_state<T, State, S, Step, Output>(
    source: AuthRef<List<T>>,
    state: RecurState<State>,
    output: Draft<S>,
    mut step: Step,
) -> AuthRef<S>
where
    T: DeserializeOwned + Serialize + Selectable + 'static,
    State: DeserializeOwned + Serialize + 'static,
    S: Schema + DeserializeOwned + Serialize + 'static,
    Step: FnMut(RecurInput<T>, RecurState<State>, RecurOutput<S>) -> Output,
    Output: IntoRecurControl<(RecurState<State>, RecurOutput<S>)>,
{
    #[cfg(feature = "std")]
    {
        let cursor = ListCursor::open(&source)
            .unwrap_or_else(|error| panic!("Failed to open recursive list source: {}", error));
        let len = cursor.len();
        if len == 0 {
            let _ = state;
            return finalize_recur_output(output, true);
        }
        let mut state = state;
        let mut output = output;

        for index in 0..len {
            let input = recur_iteration_input(&cursor, index, len);

            match step(input, state, output).into_recur_control() {
                RecurControl::Continue((next_state, next_output)) => {
                    state = next_state;
                    output = next_output;
                }
                RecurControl::Break((done_state, done_output)) => {
                    state = done_state;
                    output = done_output;
                    break;
                }
            }
        }

        let _ = state;
        return finalize_recur_output(output, false);
    }

    #[cfg(not(feature = "std"))]
    {
        let _ = source;
        let _ = state;
        let _ = output;
        let _ = step;
        panic!("Recursive list execution requires the `std` feature")
    }
}

#[doc(hidden)]
pub fn run_recur_sequence_list<T, S, Step, Output>(
    source: AuthRef<List<T>>,
    output: Draft<S>,
    mut step: Step,
) -> AuthRef<S>
where
    T: DeserializeOwned + Serialize + Selectable + 'static,
    S: Schema + DeserializeOwned + Serialize + 'static,
    Step: FnMut(RecurSequenceInput<T>, RecurSequenceOutput<S>) -> Output,
    Output: Into<RecurSequenceOutput<S>>,
{
    #[cfg(feature = "std")]
    {
        let cursor = ListCursor::open(&source).unwrap_or_else(|error| {
            panic!("Failed to open recursive sequence list source: {}", error)
        });
        let len = cursor.len();
        if len == 0 {
            return finalize_recur_output(output, true);
        }

        let mut output = output;
        for index in 0..len {
            let item = cursor.select_item(index).unwrap_or_else(|error| {
                panic!("Failed to select recursive sequence list item: {}", error)
            });
            let input = RecurSequenceInput::__raster_from_auth_ref(item, index, len);
            let sequence_output = RecurSequenceOutput::__raster_from_recur_output(output);
            output = step(input, sequence_output)
                .into()
                .__raster_into_recur_output();
        }

        return finalize_recur_output(output, false);
    }

    #[cfg(not(feature = "std"))]
    {
        let _ = source;
        let _ = output;
        let _ = step;
        panic!("Recursive sequence list execution requires the `std` feature")
    }
}

#[doc(hidden)]
pub fn run_recur_sequence_list_state<T, State, Step, Output>(
    source: AuthRef<List<T>>,
    state: RecurState<State>,
    mut step: Step,
) -> AuthRef<State>
where
    T: DeserializeOwned + Serialize + Selectable + 'static,
    State: DeserializeOwned + Serialize + 'static,
    Step: FnMut(RecurSequenceInput<T>, RecurSequenceState<State>) -> Output,
    Output: Into<RecurSequenceState<State>>,
{
    #[cfg(feature = "std")]
    {
        let cursor = ListCursor::open(&source).unwrap_or_else(|error| {
            panic!("Failed to open recursive sequence list source: {}", error)
        });
        let len = cursor.len();
        let mut state = state;

        for index in 0..len {
            let item = cursor.select_item(index).unwrap_or_else(|error| {
                panic!("Failed to select recursive sequence list item: {}", error)
            });
            let input = RecurSequenceInput::__raster_from_auth_ref(item, index, len);
            let sequence_state = RecurSequenceState::__raster_from_recur_state(state);
            state = step(input, sequence_state)
                .into()
                .__raster_into_recur_state();
        }

        return crate::__private::bind_infallible_call(state.into_inner());
    }

    #[cfg(not(feature = "std"))]
    {
        let _ = source;
        let _ = state;
        let _ = step;
        panic!("Recursive sequence list execution requires the `std` feature")
    }
}

#[doc(hidden)]
pub fn run_recur_sequence_list_with_state<T, State, S, Step, Output>(
    source: AuthRef<List<T>>,
    state: RecurState<State>,
    output: Draft<S>,
    mut step: Step,
) -> AuthRef<S>
where
    T: DeserializeOwned + Serialize + Selectable + 'static,
    State: DeserializeOwned + Serialize + 'static,
    S: Schema + DeserializeOwned + Serialize + 'static,
    Step: FnMut(RecurSequenceInput<T>, RecurSequenceState<State>, RecurSequenceOutput<S>) -> Output,
    Output: Into<(RecurSequenceState<State>, RecurSequenceOutput<S>)>,
{
    #[cfg(feature = "std")]
    {
        let cursor = ListCursor::open(&source).unwrap_or_else(|error| {
            panic!("Failed to open recursive sequence list source: {}", error)
        });
        let len = cursor.len();
        if len == 0 {
            let _ = state;
            return finalize_recur_output(output, true);
        }

        let mut state = state;
        let mut output = output;
        for index in 0..len {
            let item = cursor.select_item(index).unwrap_or_else(|error| {
                panic!("Failed to select recursive sequence list item: {}", error)
            });
            let input = RecurSequenceInput::__raster_from_auth_ref(item, index, len);
            let sequence_state = RecurSequenceState::__raster_from_recur_state(state);
            let sequence_output = RecurSequenceOutput::__raster_from_recur_output(output);
            let (next_state, next_output) = step(input, sequence_state, sequence_output).into();
            state = next_state.__raster_into_recur_state();
            output = next_output.__raster_into_recur_output();
        }

        let _ = state;
        return finalize_recur_output(output, false);
    }

    #[cfg(not(feature = "std"))]
    {
        let _ = source;
        let _ = state;
        let _ = output;
        let _ = step;
        panic!("Recursive sequence list execution requires the `std` feature")
    }
}

#[cfg(feature = "std")]
pub fn store_value<T: Serialize>(value: &T) -> raster_core::Result<StorageRef> {
    raster_runtime::store_value(value)
}

pub fn materialize_auth_return<T, A>(value: A) -> T
where
    T: DeserializeOwned + Serialize,
    A: IntoAuthValue<T>,
{
    into_auth_value::<T, _>(value)
        .unwrap_or_else(|error| panic!("Failed to materialize Raster auth return: {}", error))
        .into_inner()
}

pub fn materialize_auth_result<T, A>(
    value: core::result::Result<A, String>,
) -> core::result::Result<T, String>
where
    T: DeserializeOwned + Serialize,
    A: IntoAuthValue<T>,
{
    value.map(|arg| {
        into_auth_value::<T, A>(arg)
            .unwrap_or_else(|error| panic!("Failed to materialize Raster auth result: {}", error))
            .into_inner()
    })
}

#[cfg(feature = "std")]
pub fn encode_raster_value<T: Serialize>(
    value: &T,
) -> raster_core::Result<(Vec<u8>, Vec<u8>, String)> {
    raster_runtime::encode_raster_value(value)
}

#[cfg(feature = "std")]
pub fn write_raster_files<T: Serialize>(
    value: &T,
    data_path: &std::path::Path,
    index_path: &std::path::Path,
) -> raster_core::Result<String> {
    raster_runtime::write_raster_files(value, data_path, index_path)
}

#[cfg(feature = "std")]
pub fn postcard_structural_commitment<T: Serialize>(value: &T) -> raster_core::Result<String> {
    raster_runtime::postcard_structural_commitment(value)
}
