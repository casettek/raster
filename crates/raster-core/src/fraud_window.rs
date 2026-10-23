//! What the four parts of a fraud window's proof share: the record chain they
//! all fold, and the column chains for values one part produces and another
//! consumes. See `docs/proposals/transition-split.md`.
//!
//! Every part's segment commits a [`PartSegmentJournal`]: where it starts and
//! ends in the window, and its record chain, state and columns before and
//! after. [`check_window`] is the FRAUD WINDOW aggregator's whole logic, minus
//! the receipt verification its guest does first: it stitches each part's
//! segments, compares the chains parts share, and returns the
//! [`FraudWindowJournal`].

use std::vec::Vec;

use serde::{Deserialize, Serialize};

use crate::authorization::AuthorizationJournal;
use crate::cfs::CfsCoordinates;
use crate::hash::{Digest, Sha256};
use crate::input::Hash32;
use crate::program::ImageId;
use crate::recur_progress::RecurProgressStack;
use crate::storage_check::StorageState;
use crate::transition::{EntrypointAuthorization, InitTransition, OutputAuthorization};

/// The four parts a fraud window's proof splits into, by data source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum PartKind {
    /// `commit.bin`: the trace tree, fingerprint, window binding and verdict.
    Trace,
    /// The program: where steps sit and where arguments come from.
    ControlFlow,
    /// The store witnesses: reads, writes and roots.
    Storage,
    /// The tile replays: io, recur, drafts and entrypoint.
    Transition,
}

impl PartKind {
    pub const ALL: [PartKind; 4] = [
        PartKind::Trace,
        PartKind::ControlFlow,
        PartKind::Storage,
        PartKind::Transition,
    ];

    /// Whether the part reads the program, and so must commit the
    /// commitment of the program it read.
    pub fn reads_program(self) -> bool {
        matches!(self, PartKind::ControlFlow | PartKind::Transition)
    }

    /// The columns this part folds, as producer or consumer, in
    /// [`Column::ALL`] order: exactly what each of its segments commits.
    pub fn columns(self) -> Vec<Column> {
        Column::ALL
            .into_iter()
            .filter(|column| column.producer() == self || column.consumers().contains(&self))
            .collect()
    }
}

/// A value one part produces and another consumes, one per window step.
///
/// The complete list, from every cross-family read in the semantic checks
/// (`transition-split` §Shared values). A value that crosses parts and isn't
/// here would be read from the host with nothing checking it, so the consumer
/// side only ever sees these through `Consumed` (in the TRANSITION part's
/// guest), which folds each value into its chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Column {
    /// The store before the step: recur's returned-state read and
    /// `ProgramEnd`'s output read are checked against it.
    StoreBefore,
    /// `selection_witnesses_commitment` of the step's selection witnesses:
    /// recur and io read the witnesses the storage check folded.
    Selections,
}

impl Column {
    pub const ALL: [Column; 2] = [Column::StoreBefore, Column::Selections];

    /// The part whose checks make the value true.
    pub fn producer(self) -> PartKind {
        match self {
            Column::StoreBefore | Column::Selections => PartKind::Storage,
        }
    }

    /// The parts that read it.
    pub fn consumers(self) -> &'static [PartKind] {
        match self {
            Column::StoreBefore | Column::Selections => &[PartKind::Transition],
        }
    }

    fn domain(self) -> &'static [u8] {
        match self {
            Column::StoreBefore => b"raster/fraud-window/column/store-before/v1",
            Column::Selections => b"raster/fraud-window/column/selections/v1",
        }
    }
}

const RECORDS_DOMAIN: &[u8] = b"raster/fraud-window/records/v1";
const WINDOW_DOMAIN: &[u8] = b"raster/fraud-window/window/v1";
const STORAGE_STATE_DOMAIN: &[u8] = b"raster/fraud-window/storage-state/v1";
const REPLAY_CLAIMS_DOMAIN: &[u8] = b"raster/fraud-window/replay-claims/v1";
const TRACE_STATE_DOMAIN: &[u8] = b"raster/fraud-window/state/trace/v1";
const CONTROL_FLOW_STATE_DOMAIN: &[u8] = b"raster/fraud-window/state/control-flow/v1";
const TRANSITION_STATE_DOMAIN: &[u8] = b"raster/fraud-window/state/transition/v1";
const AUTHORIZATION_DOMAIN: &[u8] = b"raster/fraud-window/authorization/v1";

/// `sha256(domain ‖ postcard(init))`: the identity every part's chains start
/// from, so chains from different windows can't be mixed.
pub fn window_id(init: &InitTransition) -> Hash32 {
    let bytes = postcard::to_allocvec(init).expect("an initial transition always encodes");
    hash(&[WINDOW_DOMAIN, &bytes])
}

/// The digest a `StoreBefore` column folds for one step.
pub fn storage_state_digest(state: &StorageState) -> Hash32 {
    let bytes = postcard::to_allocvec(state).expect("a storage state always encodes");
    hash(&[STORAGE_STATE_DOMAIN, &bytes])
}

/// The trace part's state between steps: its frontier, which position and
/// root bind (the root is a hash chain over the frontier at that position).
pub fn trace_state_digest(position: u64, root: &[u8]) -> Hash32 {
    hash(&[TRACE_STATE_DOMAIN, &position.to_le_bytes(), root])
}

/// The control-flow part's state between steps: the trace frontier (as
/// position and root) and the coordinates the next step may take.
pub fn control_flow_state_digest(
    position: u64,
    root: &[u8],
    expected: Option<&Vec<CfsCoordinates>>,
) -> Hash32 {
    let expected = postcard::to_allocvec(&expected).expect("coordinates always encode");
    hash(&[CONTROL_FLOW_STATE_DOMAIN, &position.to_le_bytes(), root, &expected])
}

/// The transition part's state between steps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransitionPartState {
    pub recur_progress: RecurProgressStack,
    pub entrypoint_authorization: EntrypointAuthorization,
    pub output_authorization: OutputAuthorization,
}

impl TransitionPartState {
    pub fn digest(&self) -> Hash32 {
        let bytes = postcard::to_allocvec(self).expect("a transition state always encodes");
        hash(&[TRANSITION_STATE_DOMAIN, &bytes])
    }
}

/// What the transition part commits for the authorization journal it read,
/// and FRAUD WINDOW recomputes from the one it verified.
pub fn authorization_digest(journal: &AuthorizationJournal) -> Hash32 {
    let bytes = postcard::to_allocvec(journal).expect("an authorization journal always encodes");
    hash(&[AUTHORIZATION_DOMAIN, &bytes])
}

fn hash(parts: &[&[u8]]) -> Hash32 {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part);
    }
    hasher.finalize().into()
}

/// What a [`Chain`] folds, and so its domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChainKind {
    Records,
    Column(Column),
    /// One [`ReplayClaim`] per tile step, folded by the transition part and
    /// recomputed by FRAUD WINDOW from the claims it verified.
    ReplayClaims,
}

impl ChainKind {
    fn domain(self) -> &'static [u8] {
        match self {
            ChainKind::Records => RECORDS_DOMAIN,
            ChainKind::Column(column) => column.domain(),
            ChainKind::ReplayClaims => REPLAY_CLAIMS_DOMAIN,
        }
    }
}

/// A running hash over one value per window step: `c' = H(domain ‖ c ‖ v)`,
/// starting from `H(domain ‖ window_id)`. Order-sensitive, and different for
/// every kind and window. A segment of any length can commit the chain before
/// and after it, and consecutive segments stitch by equality.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chain {
    kind: ChainKind,
    value: Hash32,
}

impl Chain {
    pub fn new(kind: ChainKind, window: &Hash32) -> Self {
        Self {
            kind,
            value: hash(&[kind.domain(), window]),
        }
    }

    /// The record chain: every part folds `hash_trace_item(record)` of each
    /// step it ran.
    pub fn records(window: &Hash32) -> Self {
        Self::new(ChainKind::Records, window)
    }

    pub fn column(column: Column, window: &Hash32) -> Self {
        Self::new(ChainKind::Column(column), window)
    }

    pub fn replay_claims(window: &Hash32) -> Self {
        Self::new(ChainKind::ReplayClaims, window)
    }

    /// A chain resumed from a value a previous segment ended on.
    pub fn resume(kind: ChainKind, value: Hash32) -> Self {
        Self { kind, value }
    }

    pub fn fold(&mut self, item: &[u8]) {
        self.value = hash(&[self.kind.domain(), &self.value, item]);
    }

    pub fn value(&self) -> Hash32 {
        self.value
    }
}

/// The chains the storage and TRANSITION parts both fold over a window: the
/// record chain, and one chain per column (the storage part producing, the
/// TRANSITION part consuming). Each segment commits them before and after.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowChains {
    pub records: Chain,
    pub store_before: Chain,
    pub selections: Chain,
}

impl WindowChains {
    pub fn new(window: &Hash32) -> Self {
        Self {
            records: Chain::records(window),
            store_before: Chain::column(Column::StoreBefore, window),
            selections: Chain::column(Column::Selections, window),
        }
    }

    /// The chains folded over a run of steps from the window's start, from
    /// each step's record hash, store before and selection commitment: what
    /// either side of the columns should end on.
    pub fn fold_all<'a>(
        window: &Hash32,
        steps: impl IntoIterator<Item = (&'a [u8], &'a StorageState, &'a Hash32)>,
    ) -> Self {
        let mut chains = Self::new(window);
        for (record_hash, before, selections) in steps {
            chains.records.fold(record_hash);
            chains.store_before.fold(&storage_state_digest(before));
            chains.selections.fold(selections);
        }
        chains
    }
}

/// A chain's value, or a part's state digest, before and after one segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub before: Hash32,
    pub after: Hash32,
}

/// One tile step's replay, as the transition part read it: the registry image
/// id of the tile and `sha256` of the replay journal bytes, which is the
/// journal digest in the replay receipt's claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayClaim {
    pub image_id: ImageId,
    pub journal_digest: Hash32,
}

impl ReplayClaim {
    pub fn fold_into(&self, chain: &mut Chain) {
        let mut item = [0u8; 64];
        item[..32].copy_from_slice(&self.image_id);
        item[32..].copy_from_slice(&self.journal_digest);
        chain.fold(&item);
    }
}

/// What every segment of every part commits (raw postcard bytes,
/// [`Self::encode`], `env::commit_slice`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartSegmentJournal {
    pub part: PartKind,
    /// [`window_id`] of the window's `InitTransition`, which the segment read.
    pub window_id: Hash32,
    /// The commitment of the program the segment read, for parts that read it.
    #[serde(with = "serde_bytes")]
    pub program_commitment: Option<Vec<u8>>,
    /// The trace position of the segment's first step.
    pub first_position: u64,
    pub step_count: u64,
    /// The record chain: `hash_trace_item` of every step the segment ran.
    pub records: Span,
    /// The part's own state between steps, as its digest.
    pub state: Span,
    /// One chain per column the part folds ([`PartKind::columns`]).
    pub columns: Vec<(Column, Span)>,
    /// What only this part can say ([`PartOutput`]).
    pub out: Option<PartOutput>,
}

impl PartSegmentJournal {
    pub fn encode(&self) -> Vec<u8> {
        postcard::to_allocvec(self).expect("a segment journal always encodes")
    }

    pub fn decode(bytes: &[u8]) -> postcard::Result<Self> {
        postcard::from_bytes(bytes)
    }

    pub fn index_of(&self, trace_position: u64) -> Option<u64> {
        let index = trace_position.checked_sub(self.first_position)?;
        (index < self.step_count).then_some(index)
    }

    /// The trace position after the segment's last step.
    pub fn end_position(&self) -> u64 {
        self.first_position + self.step_count
    }

    pub fn column(&self, column: Column) -> Option<&Span> {
        self.columns
            .iter()
            .find_map(|(c, span)| (*c == column).then_some(span))
    }

    pub fn trace_output(&self) -> Option<&TraceOutput> {
        match &self.out {
            Some(PartOutput::Trace(out)) => Some(out),
            _ => None,
        }
    }

    pub fn transition_output(&self) -> Option<&TransitionOutput> {
        match &self.out {
            Some(PartOutput::Transition(out)) => Some(out),
            _ => None,
        }
    }
}

/// What only one part can say, committed on each of its segments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PartOutput {
    Trace(TraceOutput),
    Transition(TransitionOutput),
}

/// The window's binding to the disputed commitment. Every trace segment binds
/// the window afresh, and refuses unless the verdict holds at each of its
/// steps: matches before the window's last index, divergence at it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceOutput {
    /// `TraceCommitmentHeader::digest()` of the refuted commitment.
    #[serde(with = "serde_bytes")]
    pub refuted_trace_commitment: Vec<u8>,
    pub window_is_terminal: bool,
    #[serde(with = "serde_bytes")]
    pub final_committed_root: Option<Vec<u8>>,
}

/// What the transition part read but can't verify itself: the replay claims
/// and the authorization journal, which FRAUD WINDOW verifies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransitionOutput {
    /// The authorization state after the segment (also in its state digest).
    pub entrypoint_authorization: EntrypointAuthorization,
    pub output_authorization: OutputAuthorization,
    /// The replay-claims chain before and after the segment.
    pub replay_claims: Span,
    /// [`authorization_digest`] of the authorization journal the segment's
    /// checks read.
    pub authorization: Hash32,
}

/// The journal a FRAUD WINDOW receipt commits: the fraud proof's public
/// facts. A receipt exists only for a window whose last step diverged from the
/// commitment, so the receipt itself is the verdict; it names no image id but
/// its own, with every part's compiled in.
#[derive(Clone, Serialize, Deserialize)]
pub struct FraudWindowJournal {
    /// The window: its initial trace frontier and store, and its fingerprint
    /// slice. Every segment of every part was bound to [`window_id`] of it.
    pub init_state: InitTransition,
    /// `sha256(domain || program.bin)` — the identity of the program the
    /// window executes, as the control-flow and TRANSITION parts read it. See
    /// `docs/proposals/program-identity.md`.
    #[serde(with = "serde_bytes")]
    pub program_commitment: Vec<u8>,
    /// The manifest the authorization journal authorizes the window's
    /// external inputs against.
    #[serde(with = "serde_bytes")]
    pub input_manifest_commitment: Vec<u8>,
    /// `TraceCommitmentHeader::digest()` of the `commit.bin` this window
    /// refutes. The trace part derives it after asserting the window's
    /// fingerprint is that commitment's slice at the offset fixed by the
    /// window's initial frontier. A downstream verifier binds it to a stage
    /// checkpoint by hash equality alone (`docs/proposals/chain-fraud-proof.md`).
    #[serde(with = "serde_bytes")]
    pub refuted_trace_commitment: Vec<u8>,
    /// Whether the window ends exactly where the refuted commitment ends.
    ///
    /// Why it matters, and why it is separate from
    /// [`Self::output_authorization`]: a `ProgramEnd` step forces the next
    /// expected coordinates empty, so nothing may follow it *within* a
    /// window — a window containing one ends with it. Add this flag and the
    /// `ProgramEnd` is the *commitment's* last item too. A consumer that
    /// requires `Established { .. } && window_is_terminal` therefore learns
    /// the trace's actual result, not the output of some `ProgramEnd` sitting
    /// mid-commitment. See `docs/proposals/chain-io-commitment.md`.
    pub window_is_terminal: bool,
    /// The committed trace root revealed for the window's **final** index,
    /// when that index falls inside the commitment's revealed tail.
    ///
    /// It is what lets a tail divergence be *proven* and not merely detected:
    /// the packed fingerprint keeps `bits_per_item` bits of each root, so when
    /// every entry from the divergence to the end collides there is nothing
    /// left in the fingerprint to diverge on, and at `window_size >= 128`
    /// (`bits_per_item == 1`) that is a coin flip rather than a rarity.
    #[serde(with = "serde_bytes")]
    pub final_committed_root: Option<Vec<u8>>,
    /// Whether `main`'s entry-argument binding is tied to the authorization
    /// journal. See [`EntrypointAuthorization`].
    pub entrypoint_authorization: EntrypointAuthorization,
    /// Whether the program's output is tied to committed storage —
    /// `NotRequired` when `main` returns unit, `Pending` unless the window
    /// verified the `ProgramEnd` step, `Established` after. See
    /// [`OutputAuthorization`].
    pub output_authorization: OutputAuthorization,
}

impl FraudWindowJournal {
    /// The bytes FRAUD WINDOW commits (`env::commit_slice`).
    pub fn encode(&self) -> Vec<u8> {
        postcard::to_allocvec(self).expect("a fraud-window journal always encodes")
    }

    pub fn decode(bytes: &[u8]) -> postcard::Result<Self> {
        postcard::from_bytes(bytes)
    }
}

/// FRAUD WINDOW's host input: the window, the program, and what it verifies.
/// Read as one frame after the segment journals.
#[derive(Clone, Serialize, Deserialize)]
pub struct FraudWindowParams {
    pub init: InitTransition,
    #[serde(with = "serde_bytes")]
    pub program_commitment: Vec<u8>,
    /// Every tile step's replay, in window order.
    pub replay_claims: Vec<ReplayClaim>,
    pub authorization_journal: AuthorizationJournal,
}

/// Why [`check_window`] refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    WrongWindow { part: PartKind, first_position: u64 },
    WrongProgram { part: PartKind, first_position: u64 },
    WrongColumns { part: PartKind, first_position: u64 },
    WrongOutput { part: PartKind, first_position: u64 },
    EmptySegment { part: PartKind, first_position: u64 },
    /// A part's segments don't cover the window exactly once, in order.
    Coverage { part: PartKind, expected: u64, found: u64 },
    /// A part's segments don't continue each other.
    Stitch { part: PartKind, what: &'static str, at: u64 },
    /// A part's first segment doesn't start a chain from the window's seed.
    Seed { part: PartKind, what: &'static str },
    /// The parts ran on different records.
    Records,
    /// A consumer read different values than the producer produced.
    ColumnMismatch(Column),
    TraceBinding,
    Authorization,
    ReplayClaims,
}

impl core::fmt::Display for Refusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{self:?}")
    }
}

/// The FRAUD WINDOW aggregator's checks, after its guest has verified every
/// segment's receipt against the part's compiled-in image id, every replay
/// claim, and the authorization journal. `segments` may come in any order.
///
/// For each part: its segments are for this window and program, commit the
/// part's columns and outputs, cover the window from its first step to its
/// last exactly once, and continue each other (records, state, columns and,
/// for the transition part, the replay claims); the first starts every chain
/// from the window's seed. Across parts: every part's record chain ends equal,
/// every consumer's column chain ends equal to its producer's, the replay
/// claims fold to the transition part's chain, and the authorization journal
/// is the one it read. Each part's opening segment checked its own initial
/// state against `InitTransition`, so no part-specific state is known here.
pub fn check_window(
    params: &FraudWindowParams,
    segments: &[PartSegmentJournal],
) -> Result<FraudWindowJournal, Refusal> {
    let window = window_id(&params.init);
    let start = params.init.init_frontier.position;
    let end = start + params.init.fingerprint.len() as u64;

    let mut final_records = Vec::new();
    let mut final_columns: Vec<(PartKind, Column, Hash32)> = Vec::new();
    let mut trace_output: Option<&TraceOutput> = None;
    let mut transition_output: Option<(&TransitionOutput, Hash32)> = None;

    for part in PartKind::ALL {
        let mut own: Vec<&PartSegmentJournal> =
            segments.iter().filter(|s| s.part == part).collect();
        own.sort_by_key(|s| s.first_position);
        let columns = part.columns();

        let mut position = start;
        let mut previous: Option<&PartSegmentJournal> = None;
        for segment in &own {
            let at = segment.first_position;
            if segment.window_id != window {
                return Err(Refusal::WrongWindow { part, first_position: at });
            }
            let program_ok = match &segment.program_commitment {
                Some(commitment) => part.reads_program() && *commitment == params.program_commitment,
                None => !part.reads_program(),
            };
            if !program_ok {
                return Err(Refusal::WrongProgram { part, first_position: at });
            }
            let committed: Vec<Column> = segment.columns.iter().map(|(c, _)| *c).collect();
            if committed != columns {
                return Err(Refusal::WrongColumns { part, first_position: at });
            }
            let out_ok = match part {
                PartKind::Trace => segment.trace_output().is_some(),
                PartKind::Transition => segment.transition_output().is_some(),
                PartKind::ControlFlow | PartKind::Storage => segment.out.is_none(),
            };
            if !out_ok {
                return Err(Refusal::WrongOutput { part, first_position: at });
            }
            if segment.step_count == 0 {
                return Err(Refusal::EmptySegment { part, first_position: at });
            }
            if at != position {
                return Err(Refusal::Coverage { part, expected: position, found: at });
            }
            position = segment.end_position();

            match previous {
                None => {
                    if segment.records.before != Chain::records(&window).value() {
                        return Err(Refusal::Seed { part, what: "records" });
                    }
                    for (column, span) in &segment.columns {
                        if span.before != Chain::column(*column, &window).value() {
                            return Err(Refusal::Seed { part, what: "column" });
                        }
                    }
                    if let Some(out) = segment.transition_output() {
                        if out.replay_claims.before != Chain::replay_claims(&window).value() {
                            return Err(Refusal::Seed { part, what: "replay claims" });
                        }
                    }
                }
                Some(previous) => {
                    if previous.records.after != segment.records.before {
                        return Err(Refusal::Stitch { part, what: "records", at });
                    }
                    if previous.state.after != segment.state.before {
                        return Err(Refusal::Stitch { part, what: "state", at });
                    }
                    for ((_, before), (_, after)) in previous.columns.iter().zip(&segment.columns) {
                        if before.after != after.before {
                            return Err(Refusal::Stitch { part, what: "column", at });
                        }
                    }
                    if let (Some(prev), Some(next)) =
                        (previous.transition_output(), segment.transition_output())
                    {
                        if prev.replay_claims.after != next.replay_claims.before {
                            return Err(Refusal::Stitch { part, what: "replay claims", at });
                        }
                        if prev.authorization != next.authorization {
                            return Err(Refusal::Stitch { part, what: "authorization", at });
                        }
                    }
                    if let (Some(prev), Some(next)) =
                        (previous.trace_output(), segment.trace_output())
                    {
                        if prev != next {
                            return Err(Refusal::TraceBinding);
                        }
                    }
                }
            }
            previous = Some(segment);
        }
        if position != end {
            return Err(Refusal::Coverage { part, expected: end, found: position });
        }
        let last = previous.expect("a window covers at least one step");
        final_records.push(last.records.after);
        for (column, span) in &last.columns {
            final_columns.push((part, *column, span.after));
        }
        match part {
            PartKind::Trace => trace_output = last.trace_output(),
            PartKind::Transition => {
                let out = last.transition_output().expect("checked above");
                transition_output = Some((out, out.replay_claims.after));
            }
            _ => {}
        }
    }

    if final_records.windows(2).any(|pair| pair[0] != pair[1]) {
        return Err(Refusal::Records);
    }
    for column in Column::ALL {
        let produced = final_columns
            .iter()
            .find(|(part, c, _)| *c == column && *part == column.producer())
            .map(|(_, _, value)| value);
        for consumer in column.consumers() {
            let consumed = final_columns
                .iter()
                .find(|(part, c, _)| *c == column && part == consumer)
                .map(|(_, _, value)| value);
            if produced.is_none() || consumed != produced {
                return Err(Refusal::ColumnMismatch(column));
            }
        }
    }

    let (transition, replay_claims) = transition_output.expect("the window has a transition part");
    let mut chain = Chain::replay_claims(&window);
    for claim in &params.replay_claims {
        claim.fold_into(&mut chain);
    }
    if chain.value() != replay_claims {
        return Err(Refusal::ReplayClaims);
    }
    if authorization_digest(&params.authorization_journal) != transition.authorization {
        return Err(Refusal::Authorization);
    }
    let trace = trace_output.expect("the window has a trace part");

    Ok(FraudWindowJournal {
        init_state: params.init.clone(),
        program_commitment: params.program_commitment.clone(),
        input_manifest_commitment: params.authorization_journal.input_manifest_commitment.clone(),
        refuted_trace_commitment: trace.refuted_trace_commitment.clone(),
        window_is_terminal: trace.window_is_terminal,
        final_committed_root: trace.final_committed_root.clone(),
        entrypoint_authorization: transition.entrypoint_authorization,
        output_authorization: transition.output_authorization.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec;
    use std::vec::Vec;

    fn state(seed: u8) -> StorageState {
        StorageState {
            position: u64::from(seed),
            root: vec![seed; 32],
            index_root: vec![seed ^ 0xff; 32],
        }
    }

    #[test]
    fn a_chain_depends_on_order_window_and_domain() {
        let a = [1u8; 32];
        let b = [2u8; 32];
        let window = [7u8; 32];
        let mut ab = Chain::records(&window);
        ab.fold(&a);
        ab.fold(&b);
        let mut ba = Chain::records(&window);
        ba.fold(&b);
        ba.fold(&a);
        assert_ne!(ab.value(), ba.value(), "order");

        let mut other_window = Chain::records(&[8u8; 32]);
        other_window.fold(&a);
        other_window.fold(&b);
        assert_ne!(ab.value(), other_window.value(), "window");

        let mut column = Chain::column(Column::Selections, &window);
        column.fold(&a);
        column.fold(&b);
        assert_ne!(ab.value(), column.value(), "domain");
    }

    /// Folding a window in one go or in two runs, carrying the chain, gives the
    /// same chain: what lets segments of any length stitch.
    #[test]
    fn a_chain_folded_in_pieces_equals_one_fold() {
        let window = [3u8; 32];
        let records: Vec<[u8; 32]> = (0..5u8).map(|i| [i; 32]).collect();
        let states: Vec<StorageState> = (0..5u8).map(state).collect();
        let selections: Vec<Hash32> = (0..5u8).map(|i| [i ^ 0x55; 32]).collect();
        let steps = || {
            records
                .iter()
                .zip(&states)
                .zip(&selections)
                .map(|((r, s), c)| (r.as_slice(), s, c))
        };
        let whole = WindowChains::fold_all(&window, steps());

        let mut pieces = WindowChains::new(&window);
        for (record, before, selection) in steps() {
            pieces.records.fold(record);
            pieces.store_before.fold(&storage_state_digest(before));
            pieces.selections.fold(selection);
        }
        assert_eq!(whole, pieces);
    }

    mod check_window {
        use super::super::*;
        use crate::fingerprint::{BitPacker, Fingerprint};
        use crate::transition::SerializableFrontier;
        use std::collections::BTreeMap;
        use std::vec;
        use std::vec::Vec;

        const WINDOW_START: u64 = 10;
        const WINDOW_LEN: u64 = 6;
        const PROGRAM: [u8; 32] = [0xab; 32];
        const TILE_STEPS: [u64; 2] = [11, 14];

        fn init() -> InitTransition {
            let frontier = |position| SerializableFrontier {
                position,
                leaf: vec![1; 32],
                ommers: Vec::new(),
            };
            InitTransition {
                init_frontier: frontier(WINDOW_START),
                init_storage_frontier: frontier(3),
                init_storage_root: vec![2; 32],
                init_storage_index_root: vec![3; 32],
                fingerprint: Fingerprint {
                    bits_packer: BitPacker::new(8),
                    bits: vec![0],
                    len: WINDOW_LEN as usize,
                },
            }
        }

        fn authorization() -> AuthorizationJournal {
            AuthorizationJournal {
                external_inputs_commitments: BTreeMap::new(),
                input_manifest_commitment: vec![9; 32],
            }
        }

        fn record(position: u64) -> [u8; 32] {
            [position as u8; 32]
        }

        fn claim(position: u64) -> ReplayClaim {
            ReplayClaim {
                image_id: [0x70 | position as u8; 32],
                journal_digest: [position as u8 ^ 0x5a; 32],
            }
        }

        fn state(part: PartKind, position: u64) -> Hash32 {
            hash(&[b"test-state", &[part as u8], &position.to_le_bytes()])
        }

        fn column_value(column: Column, position: u64) -> Hash32 {
            hash(&[b"test-column", &[column as u8], &position.to_le_bytes()])
        }

        fn trace_output() -> TraceOutput {
            TraceOutput {
                refuted_trace_commitment: vec![4; 32],
                window_is_terminal: false,
                final_committed_root: None,
            }
        }

        /// One part's honest segments, `len` steps each but the last.
        fn honest(part: PartKind, len: u64) -> Vec<PartSegmentJournal> {
            let window = window_id(&init());
            let mut records = Chain::records(&window);
            let mut columns: Vec<(Column, Chain)> = part
                .columns()
                .into_iter()
                .map(|column| (column, Chain::column(column, &window)))
                .collect();
            let mut replays = Chain::replay_claims(&window);
            let end = WINDOW_START + WINDOW_LEN;
            let mut out = Vec::new();
            let mut first = WINDOW_START;
            while first < end {
                let step_count = len.min(end - first);
                let records_before = records.value();
                let columns_before: Vec<Hash32> = columns.iter().map(|(_, c)| c.value()).collect();
                let replays_before = replays.value();
                for position in first..first + step_count {
                    records.fold(&record(position));
                    for (column, chain) in &mut columns {
                        chain.fold(&column_value(*column, position));
                    }
                    if TILE_STEPS.contains(&position) {
                        claim(position).fold_into(&mut replays);
                    }
                }
                let output = match part {
                    PartKind::Trace => Some(PartOutput::Trace(trace_output())),
                    PartKind::Transition => Some(PartOutput::Transition(TransitionOutput {
                        entrypoint_authorization: EntrypointAuthorization::Established,
                        output_authorization: OutputAuthorization::Pending,
                        replay_claims: Span { before: replays_before, after: replays.value() },
                        authorization: authorization_digest(&authorization()),
                    })),
                    _ => None,
                };
                out.push(PartSegmentJournal {
                    part,
                    window_id: window,
                    program_commitment: part.reads_program().then(|| PROGRAM.to_vec()),
                    first_position: first,
                    step_count,
                    records: Span { before: records_before, after: records.value() },
                    state: Span {
                        before: state(part, first),
                        after: state(part, first + step_count),
                    },
                    columns: columns
                        .iter()
                        .zip(columns_before)
                        .map(|((column, chain), before)| {
                            (*column, Span { before, after: chain.value() })
                        })
                        .collect(),
                    out: output,
                });
                first += step_count;
            }
            out
        }

        fn params() -> FraudWindowParams {
            FraudWindowParams {
                init: init(),
                program_commitment: PROGRAM.to_vec(),
                replay_claims: TILE_STEPS.iter().map(|p| claim(*p)).collect(),
                authorization_journal: authorization(),
            }
        }

        /// Mixed segment lengths: trace whole-window, control flow per step,
        /// storage per 4, transition per 2.
        fn window() -> Vec<PartSegmentJournal> {
            [
                honest(PartKind::Trace, WINDOW_LEN),
                honest(PartKind::ControlFlow, 1),
                honest(PartKind::Storage, 4),
                honest(PartKind::Transition, 2),
            ]
            .concat()
        }

        fn refuse(segments: &[PartSegmentJournal]) -> Refusal {
            match check_window(&params(), segments) {
                Ok(_) => panic!("the window was accepted"),
                Err(refusal) => refusal,
            }
        }

        fn at(segments: &mut [PartSegmentJournal], part: PartKind, first: u64) -> &mut PartSegmentJournal {
            segments
                .iter_mut()
                .find(|s| s.part == part && s.first_position == first)
                .expect("a segment there")
        }

        #[test]
        fn an_honest_window_is_accepted_in_any_order_and_any_lengths() {
            let journal = check_window(&params(), &window()).unwrap();
            assert_eq!(journal.program_commitment, PROGRAM.to_vec());
            assert_eq!(journal.input_manifest_commitment, vec![9; 32]);
            assert_eq!(journal.refuted_trace_commitment, vec![4; 32]);
            assert_eq!(journal.entrypoint_authorization, EntrypointAuthorization::Established);

            let mut reversed = window();
            reversed.reverse();
            assert!(check_window(&params(), &reversed).is_ok());
            for len in [1, 2, 3, WINDOW_LEN] {
                let segments: Vec<_> = PartKind::ALL.iter().flat_map(|p| honest(*p, len)).collect();
                assert!(check_window(&params(), &segments).is_ok(), "length {len}");
            }
        }

        /// A consumer handed a different value than the producer made, at any
        /// one step, ends with a different chain.
        #[test]
        fn a_tampered_column_value_is_refused_for_every_column() {
            for column in Column::ALL {
                for consumer in column.consumers() {
                    let mut segments = window();
                    let others: Vec<_> =
                        segments.iter().filter(|s| s.part != *consumer).cloned().collect();
                    // Re-fold the consumer with one value changed at step 12.
                    let window = window_id(&init());
                    let mut chain = Chain::column(column, &window);
                    let mut tampered = Vec::new();
                    for mut segment in segments.drain(..).filter(|s| s.part == *consumer) {
                        let before = chain.value();
                        for position in segment.first_position..segment.end_position() {
                            let mut value = column_value(column, position);
                            if position == 12 {
                                value[0] ^= 1;
                            }
                            chain.fold(&value);
                        }
                        let span = segment
                            .columns
                            .iter_mut()
                            .find(|(c, _)| *c == column)
                            .map(|(_, span)| span)
                            .unwrap();
                        *span = Span { before, after: chain.value() };
                        tampered.push(segment);
                    }
                    let all = [others, tampered].concat();
                    assert_eq!(refuse(&all), Refusal::ColumnMismatch(column));
                }
            }
        }

        #[test]
        fn a_part_that_ran_other_records_is_refused() {
            let mut segments: Vec<_> =
                window().into_iter().filter(|s| s.part != PartKind::Storage).collect();
            let mut storage = honest(PartKind::Storage, WINDOW_LEN);
            storage[0].records.after[0] ^= 1;
            segments.extend(storage);
            assert_eq!(refuse(&segments), Refusal::Records);
        }

        #[test]
        fn a_gap_is_refused() {
            let segments: Vec<_> = window()
                .into_iter()
                .filter(|s| !(s.part == PartKind::ControlFlow && s.first_position == 12))
                .collect();
            assert_eq!(
                refuse(&segments),
                Refusal::Coverage { part: PartKind::ControlFlow, expected: 12, found: 13 },
            );
        }

        #[test]
        fn an_overlap_or_a_repeat_is_refused() {
            let mut segments = window();
            let repeat = at(&mut segments, PartKind::Transition, 12).clone();
            segments.push(repeat);
            assert!(matches!(refuse(&segments), Refusal::Coverage { part: PartKind::Transition, .. }));

            let mut segments = window();
            at(&mut segments, PartKind::Storage, 14).first_position = 13;
            assert!(matches!(refuse(&segments), Refusal::Coverage { part: PartKind::Storage, .. }));
        }

        #[test]
        fn a_part_that_stops_short_or_runs_past_the_window_is_refused() {
            let segments: Vec<_> = window()
                .into_iter()
                .filter(|s| !(s.part == PartKind::Storage && s.first_position == 14))
                .collect();
            assert_eq!(
                refuse(&segments),
                Refusal::Coverage { part: PartKind::Storage, expected: 16, found: 14 },
            );

            let mut segments = window();
            at(&mut segments, PartKind::Storage, 14).step_count = 3;
            assert_eq!(
                refuse(&segments),
                Refusal::Coverage { part: PartKind::Storage, expected: 16, found: 17 },
            );
        }

        #[test]
        fn segments_that_do_not_continue_each_other_are_refused() {
            let mut segments = window();
            at(&mut segments, PartKind::Storage, 14).state.before[0] ^= 1;
            assert_eq!(
                refuse(&segments),
                Refusal::Stitch { part: PartKind::Storage, what: "state", at: 14 },
            );

            let mut segments = window();
            at(&mut segments, PartKind::ControlFlow, 13).records.before[0] ^= 1;
            assert_eq!(
                refuse(&segments),
                Refusal::Stitch { part: PartKind::ControlFlow, what: "records", at: 13 },
            );

            let mut segments = window();
            at(&mut segments, PartKind::Transition, 14).columns[1].1.before[0] ^= 1;
            assert_eq!(
                refuse(&segments),
                Refusal::Stitch { part: PartKind::Transition, what: "column", at: 14 },
            );

            let mut segments = window();
            let Some(PartOutput::Transition(out)) = &mut at(&mut segments, PartKind::Transition, 12).out
            else {
                unreachable!()
            };
            out.replay_claims.before[0] ^= 1;
            assert_eq!(
                refuse(&segments),
                Refusal::Stitch { part: PartKind::Transition, what: "replay claims", at: 12 },
            );
        }

        /// Every TRANSITION segment's entrypoint checks read one authorization
        /// journal: a segment that read another doesn't stitch.
        #[test]
        fn transition_segments_that_read_other_authorization_journals_are_refused() {
            let mut segments = window();
            let Some(PartOutput::Transition(out)) =
                &mut at(&mut segments, PartKind::Transition, 14).out
            else {
                unreachable!()
            };
            out.authorization[0] ^= 1;
            assert_eq!(
                refuse(&segments),
                Refusal::Stitch { part: PartKind::Transition, what: "authorization", at: 14 },
            );
        }

        /// Two segments swapped in place: each is honest, but neither continues
        /// the one before it.
        #[test]
        fn swapped_segments_are_refused() {
            let mut segments = window();
            let a = at(&mut segments, PartKind::Transition, 10).clone();
            let b = at(&mut segments, PartKind::Transition, 12).clone();
            let (mut a2, mut b2) = (b.clone(), a.clone());
            a2.first_position = 10;
            b2.first_position = 12;
            *at(&mut segments, PartKind::Transition, 10) = a2;
            *at(&mut segments, PartKind::Transition, 12) = b2;
            assert!(matches!(refuse(&segments), Refusal::Seed { .. } | Refusal::Stitch { .. }));
        }

        #[test]
        fn a_chain_not_started_from_the_window_seed_is_refused() {
            let mut segments = window();
            at(&mut segments, PartKind::Storage, 10).columns[0].1.before[0] ^= 1;
            assert_eq!(refuse(&segments), Refusal::Seed { part: PartKind::Storage, what: "column" });
        }

        #[test]
        fn a_segment_of_another_window_or_program_is_refused() {
            let mut segments = window();
            at(&mut segments, PartKind::Trace, 10).window_id[0] ^= 1;
            assert!(matches!(refuse(&segments), Refusal::WrongWindow { part: PartKind::Trace, .. }));

            let mut segments = window();
            at(&mut segments, PartKind::ControlFlow, 11).program_commitment = Some(vec![0; 32]);
            assert!(matches!(refuse(&segments), Refusal::WrongProgram { .. }));

            let mut segments = window();
            at(&mut segments, PartKind::Transition, 10).program_commitment = None;
            assert!(matches!(refuse(&segments), Refusal::WrongProgram { .. }));
        }

        #[test]
        fn a_segment_missing_its_columns_or_output_is_refused() {
            let mut segments = window();
            at(&mut segments, PartKind::Storage, 10).columns.pop();
            assert!(matches!(refuse(&segments), Refusal::WrongColumns { .. }));

            let mut segments = window();
            at(&mut segments, PartKind::Trace, 10).out = None;
            assert!(matches!(refuse(&segments), Refusal::WrongOutput { .. }));
        }

        #[test]
        fn a_missing_part_is_refused() {
            let segments: Vec<_> =
                window().into_iter().filter(|s| s.part != PartKind::Trace).collect();
            assert!(matches!(refuse(&segments), Refusal::Coverage { part: PartKind::Trace, .. }));
        }

        #[test]
        fn replay_claims_other_than_the_ones_read_are_refused() {
            let mut p = params();
            p.replay_claims[1].journal_digest[0] ^= 1;
            assert_eq!(check_window(&p, &window()).err(), Some(Refusal::ReplayClaims));

            let mut p = params();
            p.replay_claims.pop();
            assert_eq!(check_window(&p, &window()).err(), Some(Refusal::ReplayClaims));
        }

        #[test]
        fn an_authorization_journal_other_than_the_one_read_is_refused() {
            let mut p = params();
            p.authorization_journal.input_manifest_commitment = vec![8; 32];
            assert_eq!(check_window(&p, &window()).err(), Some(Refusal::Authorization));
        }
    }

    #[test]
    fn every_column_has_one_producer_and_a_consumer() {
        for column in Column::ALL {
            assert!(!column.consumers().is_empty());
            assert!(!column.consumers().contains(&column.producer()));
        }
    }
}
