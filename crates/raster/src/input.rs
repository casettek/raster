use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::marker::PhantomData;
use core::{hash::Hash, hash::Hasher};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

#[cfg(not(feature = "std"))]
use alloc::format;
#[cfg(not(feature = "std"))]
use raster_core::draft::{draft_value_from_serialize, DraftOp};
use raster_core::draft::{replay_handle_for_schema, DraftReplayHandle, DraftReplayTransition};
pub use raster_core::input::{
    verify_selection_proof, AuthValue, ExternalEncoding, ExternalRef, ExternalSelection,
    ExternalValue, InternalRef, InternalValue, ListProofDirection, ListProofSibling, Op, Schema,
    SchemaField, SchemaFieldMode, SchemaNode, Selectable, SelectedPayload, SelectionCommitment,
    SelectionProof, SelectionProofStep, SelectionWitness, SelectorPath, SelectorSegment,
};
use raster_core::trace::{
    ExternalData as TraceExternalData, FnInputValue, InternalData as TraceInternalData,
};

#[derive(Debug, PartialEq, Eq, Hash)]
pub struct TypedExternalBinding<Root> {
    name: String,
    marker: PhantomData<fn() -> Root>,
}

#[derive(Debug)]
pub struct TypedInternalBinding<Root> {
    reference: InternalRef,
    resolve: fn(InternalRef) -> raster_core::Result<InternalValue<Root>>,
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
            external: item_trace.external,
            internal: item_trace.internal,
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
            AuthRef::External(_) => FnInputValue::ExternalBinding,
            AuthRef::Internal(_) => FnInputValue::InternalBinding,
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

impl<Root> TypedExternalBinding<Root> {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.into(),
            marker: PhantomData,
        }
    }

    pub fn into_selection(self) -> ExternalSelection {
        ExternalSelection::new(self.name)
    }
}

impl<Root> TypedInternalBinding<Root>
where
    Root: DeserializeOwned + Serialize,
{
    pub fn new(reference: InternalRef) -> Self {
        Self {
            reference,
            resolve: resolve_internal_value::<Root>,
            marker: PhantomData,
        }
    }

    #[doc(hidden)]
    pub fn with_resolver(
        reference: InternalRef,
        resolve: fn(InternalRef) -> raster_core::Result<InternalValue<Root>>,
    ) -> Self {
        Self {
            reference,
            resolve,
            marker: PhantomData,
        }
    }

    pub fn reference(&self) -> &InternalRef {
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

pub fn typed_external<Root>(name: &str) -> TypedExternalBinding<Root> {
    TypedExternalBinding::new(name)
}

pub fn typed_internal<Root>(reference: InternalRef) -> TypedInternalBinding<Root>
where
    Root: DeserializeOwned + Serialize,
{
    TypedInternalBinding::new(reference)
}

#[doc(hidden)]
pub fn typed_internal_with_resolver<Root>(
    reference: InternalRef,
    resolve: fn(InternalRef) -> raster_core::Result<InternalValue<Root>>,
) -> TypedInternalBinding<Root>
where
    Root: DeserializeOwned + Serialize,
{
    TypedInternalBinding::with_resolver(reference, resolve)
}

pub fn typed_selector_path<Root, Selected>(
    path: SelectorPath,
) -> TypedSelectorPath<Root, Selected> {
    TypedSelectorPath::new(path)
}

type ExternalResolveFn<Current> =
    Rc<dyn Fn(ExternalSelection) -> raster_core::Result<ExternalValue<Current>>>;
type InternalResolveFn<Current> =
    Rc<dyn Fn(InternalRef) -> raster_core::Result<InternalValue<Current>>>;

#[doc(hidden)]
pub struct DeferredAuthExternal<Current> {
    name: String,
    selector: SelectorPath,
    resolve: ExternalResolveFn<Current>,
}

#[doc(hidden)]
pub struct DeferredAuthInternal<Current> {
    reference: InternalRef,
    resolve: InternalResolveFn<Current>,
    marker: PhantomData<fn() -> Current>,
}

pub enum AuthRef<Current> {
    Inline(Current),
    External(DeferredAuthExternal<Current>),
    Internal(DeferredAuthInternal<Current>),
}

impl<Current> AuthRef<Current> {
    pub fn reference(&self) -> &InternalRef {
        match self {
            Self::Internal(binding) => &binding.reference,
            Self::Inline(_) | Self::External(_) => {
                panic!("AuthRef::reference() is only available for internal bindings")
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AuthRefTrace {
    pub value: FnInputValue,
    pub external: Option<TraceExternalData>,
    pub internal: Option<TraceInternalData>,
}

impl<Root> Clone for TypedExternalBinding<Root> {
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            marker: PhantomData,
        }
    }
}

impl<Root> Clone for TypedInternalBinding<Root> {
    fn clone(&self) -> Self {
        Self {
            reference: self.reference.clone(),
            resolve: self.resolve,
            marker: PhantomData,
        }
    }
}

impl<Root> PartialEq for TypedInternalBinding<Root> {
    fn eq(&self, other: &Self) -> bool {
        self.reference == other.reference
    }
}

impl<Root> Eq for TypedInternalBinding<Root> {}

impl<Root> core::hash::Hash for TypedInternalBinding<Root> {
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

impl<Current> Clone for DeferredAuthExternal<Current> {
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            selector: self.selector.clone(),
            resolve: self.resolve.clone(),
        }
    }
}

impl<Current> core::fmt::Debug for DeferredAuthExternal<Current> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("DeferredAuthExternal")
            .field("name", &self.name)
            .field("selector", &self.selector)
            .finish()
    }
}

impl<Current> Clone for DeferredAuthInternal<Current> {
    fn clone(&self) -> Self {
        Self {
            reference: self.reference.clone(),
            resolve: self.resolve.clone(),
            marker: PhantomData,
        }
    }
}

impl<Current> core::fmt::Debug for DeferredAuthInternal<Current> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("DeferredAuthInternal")
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
            Self::External(binding) => Self::External(binding.clone()),
            Self::Internal(binding) => Self::Internal(binding.clone()),
        }
    }
}

fn summarize_selector_path(selector: &SelectorPath) -> String {
    if selector.is_empty() {
        return "<root>".into();
    }

    let mut summary = String::new();
    for segment in &selector.segments {
        match segment {
            SelectorSegment::Field(name) => {
                if !summary.is_empty() {
                    summary.push('.');
                }
                summary.push_str(name);
            }
            SelectorSegment::Index(index) => {
                summary.push('[');
                summary.push_str(&alloc::format!("{}", index));
                summary.push(']');
            }
            SelectorSegment::Range { start, end } => {
                summary.push('[');
                summary.push_str(&alloc::format!("{}..{}", start, end));
                summary.push(']');
            }
        }
    }

    summary
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
            Self::External(binding) => {
                let selector = summarize_selector_path(&binding.selector);
                match (binding.resolve.as_ref())(ExternalSelection::with_selector(
                    binding.name.clone(),
                    binding.selector.clone(),
                )) {
                    Ok(resolved) => f
                        .debug_struct("AuthRef")
                        .field("storage", &"external")
                        .field("name", &resolved.name)
                        .field("selector", &summarize_selector_path(&resolved.selector))
                        .field("commitment_present", &resolved.commitment.is_some())
                        .field(
                            "selection_root_len",
                            &resolved.selected.commitment.source_root_hash.len(),
                        )
                        .field("selected_bytes_len", &resolved.selected.bytes.len())
                        .field("value", &resolved.value)
                        .finish(),
                    Err(error) => f
                        .debug_struct("AuthRef")
                        .field("storage", &"external")
                        .field("name", &binding.name)
                        .field("selector", &selector)
                        .field("materialization_error", &alloc::format!("{}", error))
                        .finish(),
                }
            }
            Self::Internal(binding) => {
                match (binding.resolve.as_ref())(binding.reference.clone()) {
                    Ok(resolved) => f
                        .debug_struct("AuthRef")
                        .field("storage", &"internal")
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
                        .field("storage", &"internal")
                        .field(
                            "coordinates",
                            &summarize_coordinates(&binding.reference.coordinates),
                        )
                        .field("commitment_len", &binding.reference.commitment.len())
                        .field("materialization_error", &alloc::format!("{}", error))
                        .finish(),
                }
            }
        }
    }
}

pub trait IntoAuthRef<Current> {
    fn into_auth_ref(self) -> AuthRef<Current>;
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

impl<Root> SelectSource for TypedExternalBinding<Root>
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
        let name = self.name;
        let selector = selector.into_path();
        AuthRef::External(DeferredAuthExternal {
            name: name.clone(),
            selector: selector.clone(),
            resolve: Rc::new(move |reference| {
                resolve_typed_external_value::<Root, Selected>(reference)
            }),
        })
    }
}

impl<Root> SelectSource for TypedInternalBinding<Root>
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
        AuthRef::Internal(DeferredAuthInternal {
            reference,
            resolve: Rc::new(move |reference| {
                let current = resolve(reference.clone())?;
                select_internal_value::<Root, Selected>(&current, &selector)
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
                    "select! on inline sequence values is not supported; use committed external or internal bindings instead"
                )
            }
            AuthRef::External(binding) => {
                let relative_selector = selector.into_path();
                let full_selector =
                    compose_selector_paths(binding.selector.clone(), relative_selector.clone());
                let current_name = binding.name.clone();
                let current_selector = binding.selector.clone();
                let resolve_current = binding.resolve.clone();
                AuthRef::External(DeferredAuthExternal {
                    name: current_name.clone(),
                    selector: full_selector.clone(),
                    resolve: Rc::new(move |_| {
                        if let Ok(selected) =
                            resolve_external_value::<Selected>(ExternalSelection::with_selector(
                                current_name.clone(),
                                full_selector.clone(),
                            ))
                        {
                            return Ok(selected);
                        }
                        let current = resolve_current(ExternalSelection::with_selector(
                            current_name.clone(),
                            current_selector.clone(),
                        ))?;
                        select_external_value::<Current, Selected>(
                            &current,
                            &relative_selector,
                            &full_selector,
                        )
                    }),
                })
            }
            AuthRef::Internal(binding) => {
                let relative_selector = selector.into_path();
                let reference = binding.reference.clone();
                let resolve_current = binding.resolve.clone();
                AuthRef::Internal(DeferredAuthInternal {
                    reference: reference.clone(),
                    resolve: Rc::new(move |reference| {
                        if let Ok(selected) =
                            select_stored_internal_value::<Selected>(&reference, &relative_selector)
                        {
                            return Ok(selected);
                        }
                        let current = (resolve_current.as_ref())(reference.clone())?;
                        select_internal_value::<Current, Selected>(&current, &relative_selector)
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

fn compose_selector_paths(prefix: SelectorPath, suffix: SelectorPath) -> SelectorPath {
    let mut segments = prefix.segments;
    segments.extend(suffix.segments);
    SelectorPath::new(segments)
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

/// A recursive-sequence list source resolved exactly once.
///
/// The parent list value is cached behind an `Rc` so that counting the list
/// and selecting each item are O(1) on the source: per-item `AuthRef`s select
/// out of this cached value rather than re-resolving the whole parent list.
#[doc(hidden)]
enum ResolvedRecurList<T> {
    External {
        name: String,
        selector: SelectorPath,
        value: Rc<ExternalValue<Vec<T>>>,
    },
    Internal {
        reference: InternalRef,
        value: Rc<InternalValue<Vec<T>>>,
    },
}

#[doc(hidden)]
fn resolve_recur_list_source<T>(
    source: &AuthRef<Vec<T>>,
) -> raster_core::Result<ResolvedRecurList<T>>
where
    T: DeserializeOwned + Serialize,
{
    match source {
        AuthRef::Inline(_) => Err(raster_core::Error::Other(
            "call_recur! requires a selectable external or internal list source".into(),
        )),
        AuthRef::External(binding) => {
            let value = (binding.resolve.as_ref())(ExternalSelection::with_selector(
                binding.name.clone(),
                binding.selector.clone(),
            ))?;
            Ok(ResolvedRecurList::External {
                name: binding.name.clone(),
                selector: binding.selector.clone(),
                value: Rc::new(value),
            })
        }
        AuthRef::Internal(binding) => {
            let value = (binding.resolve.as_ref())(binding.reference.clone())?;
            Ok(ResolvedRecurList::Internal {
                reference: binding.reference.clone(),
                value: Rc::new(value),
            })
        }
    }
}

impl<T> ResolvedRecurList<T> {
    fn len(&self) -> u64 {
        match self {
            ResolvedRecurList::External { value, .. } => value.value.len() as u64,
            ResolvedRecurList::Internal { value, .. } => value.value.len() as u64,
        }
    }

    fn select_item(&self, index: u64) -> raster_core::Result<AuthRef<T>>
    where
        T: DeserializeOwned + Serialize + Selectable + 'static,
    {
        let relative_selector = selector_path(Vec::from([SelectorSegment::Index(index)]));

        match self {
            ResolvedRecurList::External {
                name,
                selector,
                value,
            } => {
                let full_selector =
                    compose_selector_paths(selector.clone(), relative_selector.clone());
                let parent = value.clone();
                let external_name = name.clone();

                Ok(AuthRef::External(DeferredAuthExternal {
                    name: external_name.clone(),
                    selector: full_selector.clone(),
                    resolve: Rc::new(move |_| {
                        if let Ok(selected) =
                            resolve_external_value::<T>(ExternalSelection::with_selector(
                                external_name.clone(),
                                full_selector.clone(),
                            ))
                        {
                            return Ok(selected);
                        }
                        select_external_value::<Vec<T>, T>(
                            parent.as_ref(),
                            &relative_selector,
                            &full_selector,
                        )
                    }),
                }))
            }
            ResolvedRecurList::Internal { reference, value } => {
                let parent = value.clone();
                let item_selector = relative_selector.clone();

                Ok(AuthRef::Internal(DeferredAuthInternal {
                    reference: reference.clone(),
                    resolve: Rc::new(move |_| {
                        if let Ok(selected) =
                            select_stored_internal_value::<T>(&parent.reference, &item_selector)
                        {
                            return Ok(selected);
                        }
                        select_internal_value::<Vec<T>, T>(parent.as_ref(), &relative_selector)
                    }),
                    marker: PhantomData,
                }))
            }
        }
    }
}

#[doc(hidden)]
pub fn build_recur_input<T>(
    item: AuthRef<T>,
    index: u64,
    len: u64,
) -> raster_core::Result<RecurInput<T>>
where
    T: DeserializeOwned + Serialize,
{
    let value = into_auth_value::<T, _>(item)?.into_inner();
    Ok(RecurInput::new(value, index, len))
}

fn resolve_recur_list<T>(source: &AuthRef<Vec<T>>) -> raster_core::Result<Vec<T>>
where
    T: DeserializeOwned + Serialize,
{
    match source {
        AuthRef::Inline(_) => Err(raster_core::Error::Other(
            "call_recur! requires a selectable external or internal list source".into(),
        )),
        AuthRef::External(binding) => {
            let current = (binding.resolve.as_ref())(ExternalSelection::with_selector(
                binding.name.clone(),
                binding.selector.clone(),
            ))?;
            Ok(current.value)
        }
        AuthRef::Internal(binding) => {
            let current = (binding.resolve.as_ref())(binding.reference.clone())?;
            Ok(current.value)
        }
    }
}

fn group_into_chunks<T>(items: Vec<T>, chunk: usize) -> Vec<Vec<T>> {
    let chunk = chunk.max(1);
    let mut chunks = Vec::with_capacity(items.len().div_ceil(chunk));
    let mut current = Vec::with_capacity(chunk);
    for item in items {
        current.push(item);
        if current.len() == chunk {
            chunks.push(core::mem::replace(&mut current, Vec::with_capacity(chunk)));
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

/// Adapt a flat list source `AuthRef<Vec<T>>` into a chunked source
/// `AuthRef<Vec<Vec<T>>>` for `call_recur! { ..., chunk = N }`.
///
/// The underlying source binding (name/selector/commitment for external,
/// reference for internal) is preserved unchanged, so the trace still records a
/// single authenticated binding for the whole collection. Only the resolved
/// value is regrouped into contiguous chunks of `chunk` items (the final chunk
/// may be shorter), turning per-element iteration into per-chunk iteration.
#[doc(hidden)]
pub fn chunk_auth_ref<T>(source: AuthRef<Vec<T>>, chunk: usize) -> AuthRef<Vec<Vec<T>>>
where
    T: DeserializeOwned + Serialize + 'static,
{
    match source {
        AuthRef::Inline(items) => AuthRef::Inline(group_into_chunks(items, chunk)),
        AuthRef::External(binding) => {
            let name = binding.name.clone();
            let selector = binding.selector.clone();
            let inner = binding.resolve.clone();
            AuthRef::External(DeferredAuthExternal {
                name: binding.name,
                selector: binding.selector,
                resolve: Rc::new(move |_| {
                    let resolved = (inner.as_ref())(ExternalSelection::with_selector(
                        name.clone(),
                        selector.clone(),
                    ))?;
                    Ok(ExternalValue::new(
                        resolved.name,
                        resolved.selector,
                        resolved.commitment,
                        resolved.selected,
                        group_into_chunks(resolved.value, chunk),
                    ))
                }),
            })
        }
        AuthRef::Internal(binding) => {
            let reference = binding.reference.clone();
            let inner = binding.resolve.clone();
            AuthRef::Internal(DeferredAuthInternal {
                reference: binding.reference,
                resolve: Rc::new(move |_| {
                    let resolved = (inner.as_ref())(reference.clone())?;
                    Ok(InternalValue::new_with_selection(
                        resolved.reference,
                        resolved.bytes,
                        resolved.selector,
                        resolved.selection,
                        group_into_chunks(resolved.value, chunk),
                    ))
                }),
                marker: PhantomData,
            })
        }
    }
}

impl<T> IntoAuthRef<T> for T
where
    T: Serialize,
{
    fn into_auth_ref(self) -> AuthRef<T> {
        AuthRef::Inline(self)
    }
}

impl<Root> IntoAuthRef<Root> for TypedExternalBinding<Root>
where
    Root: DeserializeOwned + Serialize + 'static,
{
    fn into_auth_ref(self) -> AuthRef<Root> {
        let name = self.name;
        AuthRef::External(DeferredAuthExternal {
            name: name.clone(),
            selector: SelectorPath::default(),
            resolve: Rc::new(move |reference| resolve_external_value::<Root>(reference)),
        })
    }
}

impl<Root> IntoAuthRef<Root> for TypedInternalBinding<Root>
where
    Root: DeserializeOwned + Serialize + 'static,
{
    fn into_auth_ref(self) -> AuthRef<Root> {
        let reference = self.reference;
        let resolve = self.resolve;
        AuthRef::Internal(DeferredAuthInternal {
            reference,
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

impl<Root> IntoAuthValue<Root> for TypedExternalBinding<Root>
where
    Root: DeserializeOwned + Serialize,
{
    fn into_auth_value(self) -> raster_core::Result<AuthValue<Root>> {
        let value = resolve_external_value::<Root>(self.into_selection())?;
        Ok(AuthValue::external(value))
    }
}

impl<Root> IntoAuthValue<Root> for TypedInternalBinding<Root>
where
    Root: DeserializeOwned + Serialize,
{
    fn into_auth_value(self) -> raster_core::Result<AuthValue<Root>> {
        let value = (self.resolve)(self.reference)?;
        Ok(AuthValue::internal(value))
    }
}

impl<Current> IntoAuthValue<Current> for AuthRef<Current>
where
    Current: Serialize,
{
    fn into_auth_value(self) -> raster_core::Result<AuthValue<Current>> {
        match self {
            AuthRef::Inline(value) => Ok(AuthValue::inline(value)),
            AuthRef::External(binding) => {
                let value = (binding.resolve.as_ref())(ExternalSelection::with_selector(
                    binding.name,
                    binding.selector,
                ))?;
                Ok(AuthValue::external(value))
            }
            AuthRef::Internal(binding) => {
                let value = (binding.resolve.as_ref())(binding.reference)?;
                Ok(AuthValue::internal(value))
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

pub fn auth_ref_trace<T>(arg: &AuthRef<T>) -> raster_core::Result<AuthRefTrace>
where
    T: Serialize + DeserializeOwned,
{
    match arg {
        AuthRef::Inline(value) => Ok(AuthRefTrace {
            value: FnInputValue::Inline(
                raster_core::postcard::to_allocvec(value).unwrap_or_default(),
            ),
            external: None,
            internal: None,
        }),
        AuthRef::External(binding) => {
            if let Some(external) = trace_raster_external_binding(&binding.name, &binding.selector)?
            {
                return Ok(AuthRefTrace {
                    value: FnInputValue::ExternalBinding,
                    external: Some(external),
                    internal: None,
                });
            }

            let resolved = (binding.resolve.as_ref())(ExternalSelection::with_selector(
                binding.name.clone(),
                binding.selector.clone(),
            ))?;
            Ok(AuthRefTrace {
                value: FnInputValue::ExternalBinding,
                external: Some(TraceExternalData {
                    name: resolved.name,
                    commitment: resolved
                        .commitment
                        .map(|value| value.into_bytes())
                        .unwrap_or_default(),
                    tree_root: resolved.selected.commitment.source_root_hash.to_vec(),
                    selector: resolved.selector,
                    selection: resolved.selected.commitment,
                }),
                internal: None,
            })
        }
        AuthRef::Internal(binding) => {
            let resolved = (binding.resolve.as_ref())(binding.reference.clone())?;
            Ok(AuthRefTrace {
                value: FnInputValue::InternalBinding,
                external: None,
                internal: Some(TraceInternalData {
                    coordinates: resolved.reference.coordinates,
                    commitment: resolved.reference.commitment,
                    selector: resolved.selector,
                    selection: resolved.selection,
                }),
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

pub fn select_external_value<Root, T>(
    value: &ExternalValue<Root>,
    selector: &SelectorPath,
    full_selector: &SelectorPath,
) -> raster_core::Result<ExternalValue<T>>
where
    Root: DeserializeOwned + Serialize + Selectable,
    T: DeserializeOwned + Serialize,
{
    #[cfg(feature = "std")]
    {
        return raster_runtime::select_external_arg::<Root, T>(value, selector, full_selector);
    }

    #[cfg(not(feature = "std"))]
    {
        let _ = value;
        let _ = selector;
        let _ = full_selector;
        Err(raster_core::Error::Other(format!(
            "External selection refinement requires the `std` feature"
        )))
    }
}

pub fn select_internal_value<Root, T>(
    value: &InternalValue<Root>,
    selector: &SelectorPath,
) -> raster_core::Result<InternalValue<T>>
where
    Root: DeserializeOwned + Serialize + Selectable,
    T: DeserializeOwned + Serialize,
{
    #[cfg(feature = "std")]
    {
        return raster_runtime::select_internal_value::<Root, T>(value, selector);
    }

    #[cfg(not(feature = "std"))]
    {
        let _ = value;
        let _ = selector;
        Err(raster_core::Error::Other(format!(
            "Internal selection refinement requires the `std` feature"
        )))
    }
}

pub fn select_stored_internal_value<T>(
    reference: &InternalRef,
    selector: &SelectorPath,
) -> raster_core::Result<InternalValue<T>>
where
    T: DeserializeOwned + Serialize,
{
    #[cfg(feature = "std")]
    {
        return raster_runtime::select_stored_internal_value::<T>(reference, selector);
    }

    #[cfg(not(feature = "std"))]
    {
        let _ = reference;
        let _ = selector;
        Err(raster_core::Error::Other(alloc::format!(
            "Internal raster selection requires the `std` feature"
        )))
    }
}

pub fn resolve_external_value<T: DeserializeOwned + Serialize>(
    reference: ExternalSelection,
) -> raster_core::Result<raster_core::input::ExternalValue<T>> {
    #[cfg(feature = "std")]
    {
        return raster_runtime::resolve_external_value(reference);
    }

    #[cfg(not(feature = "std"))]
    {
        let _ = reference;
        Err(raster_core::Error::Other(alloc::format!(
            "External input resolution requires the `std` feature"
        )))
    }
}

pub fn resolve_typed_external_value<Root, T>(
    reference: ExternalSelection,
) -> raster_core::Result<raster_core::input::ExternalValue<T>>
where
    Root: DeserializeOwned + Serialize + Selectable,
    T: DeserializeOwned + Serialize,
{
    #[cfg(feature = "std")]
    {
        return raster_runtime::resolve_typed_external_value::<Root, T>(reference);
    }

    #[cfg(not(feature = "std"))]
    {
        let _ = reference;
        Err(raster_core::Error::Other(alloc::format!(
            "Typed external resolution requires the `std` feature"
        )))
    }
}

pub fn trace_raster_external_binding(
    name: &str,
    selector: &SelectorPath,
) -> raster_core::Result<Option<TraceExternalData>> {
    #[cfg(feature = "std")]
    {
        return raster_runtime::trace_raster_external_binding(name, selector);
    }

    #[cfg(not(feature = "std"))]
    {
        let _ = name;
        let _ = selector;
        Ok(None)
    }
}

pub fn resolve_internal_value<T: DeserializeOwned + Serialize>(
    reference: InternalRef,
) -> raster_core::Result<raster_core::input::InternalValue<T>> {
    #[cfg(feature = "std")]
    {
        return raster_runtime::resolve_internal_value(&reference);
    }

    #[cfg(not(feature = "std"))]
    {
        let _ = reference;
        Err(raster_core::Error::Other(alloc::format!(
            "Internal input resolution requires the `std` feature"
        )))
    }
}

pub fn resolve_internal_ok_value<T: DeserializeOwned + Serialize>(
    reference: InternalRef,
) -> raster_core::Result<raster_core::input::InternalValue<T>> {
    #[cfg(feature = "std")]
    {
        return raster_runtime::resolve_internal_ok_value(&reference);
    }

    #[cfg(not(feature = "std"))]
    {
        let _ = reference;
        Err(raster_core::Error::Other(alloc::format!(
            "Result-backed internal input resolution requires the `std` feature"
        )))
    }
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
        let reference = raster_runtime::finalize_draft::<S>(draft.anchor(), draft.current_root())
            .unwrap_or_else(|error| {
                panic!(
                    "Failed to finalize draft '{}': {}",
                    core::any::type_name::<S>(),
                    error
                )
            });
        return into_auth_ref::<S, _>(typed_internal::<S>(reference));
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
        return into_auth_ref::<S, _>(typed_internal::<S>(reference));
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
    source: AuthRef<Vec<T>>,
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
        let items = resolve_recur_list(&source)
            .unwrap_or_else(|error| panic!("Failed to resolve recursive list source: {}", error));
        let len = items.len() as u64;
        if len == 0 {
            return finalize_recur_output(output, true);
        }
        let mut output = output;

        for (index, value) in items.into_iter().enumerate() {
            let input = RecurInput::new(value, index as u64, len);

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

#[doc(hidden)]
pub fn run_recur_list_state<T, State, Step, Output>(
    source: AuthRef<Vec<T>>,
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
        let items = resolve_recur_list(&source)
            .unwrap_or_else(|error| panic!("Failed to resolve recursive list source: {}", error));
        let len = items.len() as u64;
        let mut state = state;

        for (index, value) in items.into_iter().enumerate() {
            let input = RecurInput::new(value, index as u64, len);

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
    source: AuthRef<Vec<T>>,
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
        let items = resolve_recur_list(&source)
            .unwrap_or_else(|error| panic!("Failed to resolve recursive list source: {}", error));
        let len = items.len() as u64;
        if len == 0 {
            let _ = state;
            return finalize_recur_output(output, true);
        }
        let mut state = state;
        let mut output = output;

        for (index, value) in items.into_iter().enumerate() {
            let input = RecurInput::new(value, index as u64, len);

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
    source: AuthRef<Vec<T>>,
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
        let resolved = resolve_recur_list_source(&source).unwrap_or_else(|error| {
            panic!(
                "Failed to resolve recursive sequence list source: {}",
                error
            )
        });
        let len = resolved.len();
        if len == 0 {
            return finalize_recur_output(output, true);
        }

        let mut output = output;
        for index in 0..len {
            let item = resolved.select_item(index).unwrap_or_else(|error| {
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
    source: AuthRef<Vec<T>>,
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
        let resolved = resolve_recur_list_source(&source).unwrap_or_else(|error| {
            panic!(
                "Failed to resolve recursive sequence list source: {}",
                error
            )
        });
        let len = resolved.len();
        let mut state = state;

        for index in 0..len {
            let item = resolved.select_item(index).unwrap_or_else(|error| {
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
    source: AuthRef<Vec<T>>,
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
        let resolved = resolve_recur_list_source(&source).unwrap_or_else(|error| {
            panic!(
                "Failed to resolve recursive sequence list source: {}",
                error
            )
        });
        let len = resolved.len();
        if len == 0 {
            let _ = state;
            return finalize_recur_output(output, true);
        }

        let mut state = state;
        let mut output = output;
        for index in 0..len {
            let item = resolved.select_item(index).unwrap_or_else(|error| {
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
pub fn store_internal_value<T: Serialize>(value: &T) -> raster_core::Result<InternalRef> {
    raster_runtime::store_internal_value(value)
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
