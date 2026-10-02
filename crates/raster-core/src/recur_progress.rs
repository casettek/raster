//! Cross-step recur loop state that a fraud-proof window can *verify* rather
//! than inherit.
//!
//! The transition guest proves one step per execution, so anything holding
//! across steps travels in `Transition` — pinned by the previous journal's
//! receipt, which is sound for every `Next` step. A window's **first** step has
//! no previous journal, so its starting state comes straight off the host.
//!
//! That is fine for fields that are re-derived or compared against something.
//! It is not fine for a map that is only ever read. Give
//! `lazy-list-recur.md` §5's completeness rules the obvious carrier — a per-site
//! map seeded from `InitTransition` — and a fresh window opening at a recur site
//! can simply claim nine completed iterations, and the terminal rules pass over
//! iterations nobody verified. The prover picks where windows open, so that is
//! not a corner case; it is the default way to defeat the rules.
//!
//! The fix is not to trust the seed. Every step records the commitment of the
//! stack **after** it, and the guest checks
//!
//! ```text
//! advance(carried, this step's facts).commitment() == step.recur_progress_commitment
//! ```
//!
//! A wrong seed advances to a different stack, hashes to a different value, and
//! fails against the recorded one. Only the true predecessor state survives, up
//! to hash collision. Recording only the *after* state is what makes this work
//! with one 32-byte field: the seed is validated by reproducing the step's own
//! recorded commitment, not by matching a predecessor record the window does not
//! contain.
//!
//! See `docs/proposals/recur-progress-commitment.md`.

use alloc::vec::Vec;
use core::fmt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::cfs::{CfsCoordinate, CfsCoordinates};
use crate::draft::RecurControlKind;
use crate::draft::{DraftRoot, RecurStateTransition};
use crate::input::{Hash32, SelectionProofStep, SelectionWitness, SelectorSegment};
use crate::trace::StorageData;

/// Which rule set a site's iterations are held to.
///
/// The two kinds are genuinely different mechanisms, not a subset relation: a
/// recur *tile*'s facts are replay-proven in its journal, while a recur
/// *sequence* has no journal at all — its iterations are read from trace
/// structure, and it cannot terminate early. Mixing them up is what would let
/// one loop's progress be attached to another's iterations.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum RecurSiteKind {
    Tile,
    Sequence,
}

/// One live recur site's progress.
///
/// Every field is under the commitment, blocks a specific forgery, and — the
/// property that makes the mechanism implementable at all — is reachable by the
/// **recorder** as well as the guest. The last column is the parity check every
/// future field has to pass:
///
/// | field | what it stops | producer sees it via |
/// | --- | --- | --- |
/// | `site`, `kind` | attaching one loop's progress to another's iterations | CFS + step coordinates |
/// | `chunk` | re-declaring `C` mid-loop, which would make rule 4 partly prover-chosen | CFS literal |
/// | `source_len` | switching `L` mid-loop | the site `Start` event's metadata selection |
/// | `next_iteration_index` | rules 1 and 2 — first index is 0, indices are contiguous | `RecurExecutionState` |
/// | `last_control` | rule 6 — a `Break` is invisible to the iteration after it | the control bit on the iteration event |
/// | `state_commitment` | substituting the carried state between iterations | the state transition on the iteration event |
/// | `state_is_output` | dropping the *final* iteration's state, which has no successor to pin it | CFS literal |
///
/// **There is deliberately no `consumed_total`.** Rule 4 *defines* it —
/// `consumed_elements == min(C, L − covered_before)` — so once that rule is
/// enforced the honest running total is fully determined by `(chunk,
/// source_len, next_iteration_index)`, all three producer-visible.
/// [`RecurProgressFrame::consumed_total`] derives it.
///
/// Carrying it explicitly is what made revision 1 of
/// `recur-progress-commitment.md` unimplementable: it is the running sum of a
/// **replay-journal** field, and the recorder never sees a journal, so no
/// honest producer could compute the commitment the guest demanded. The
/// journal's `consumed_elements` is therefore *checked against* the derived
/// value, never *folded into* the commitment — which is also what
/// `lazy-list-recur` §5 means by "the journal field is a binding, not an
/// authority".
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct RecurProgressFrame {
    pub site: CfsCoordinates,
    pub kind: RecurSiteKind,
    /// `C` — the CFS literal, 1 when unchunked.
    pub chunk: u64,
    /// `L` — the authenticated source length, learned at the site `Start` event
    /// from `lazy-list-recur.md` §1's metadata.
    pub source_len: u64,
    pub next_iteration_index: u64,
    pub last_control: RecurControlKind,
    /// Commitment of the carried state as it stood after the last iteration —
    /// `None` before iteration 0, and for a site that carries no state.
    ///
    /// Recording only the *after* value is what lets one field chain a whole
    /// sweep and still seed a window: the seed is validated by reproducing the
    /// window's own first step's commitment, not by matching a predecessor
    /// record the window does not contain.
    #[serde(default)]
    pub state_commitment: Option<Hash32>,
    /// Whether this site's own output is its carried state, from the CFS.
    ///
    /// Every carried state but the last is pinned by the next iteration's
    /// bound `state_in`. The last one has no successor, so [`Self::close_site`]
    /// compares it against what the site actually returned — which is only a
    /// meaningful comparison for this shape.
    #[serde(default)]
    pub state_is_output: bool,
    /// Which list the site sweeps — [`source_identity`] of the site `Start`'s
    /// `"input"` binding, whose object the CFS input check has already bound.
    ///
    /// Held in the frame for the same reason `L` is: an iteration is checked
    /// against it (rule 8, [`RecurProgressStack::check_iteration_item`]) and a
    /// fraud window may open after the `Start` that established it. Without
    /// it, nothing ties an iteration's item to the source at all — recur
    /// iterations skip the CFS input check.
    #[serde(default)]
    pub source: Hash32,
    /// The object this site builds, as it stands after the last step that
    /// changed it — `None` exactly when the CFS says the site owns no object
    /// (a state-only site).
    ///
    /// Opened at the site's `Start` from the CFS (`RecurOutputDecl`): the empty
    /// root of `S` for a creating site, the base's commitment for a deriving
    /// one. Every step carrying a draft transition advances it
    /// ([`RecurProgressStack::advance_draft`]); the close compares it with the
    /// object the site wrote ([`RecurProgressStack::close_site`]). A window
    /// opening mid-site inherits it through the seeded stack, which the first
    /// step's recorded commitment validates. See
    /// `incremental-draft-materialization.md` §The draft root rides in the
    /// site's recur-progress frame.
    #[serde(default)]
    pub draft: Option<SiteDraft>,
}

/// A site's object under construction: its schema, and its root after the last
/// op applied to it.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct SiteDraft {
    pub schema_hash: [u8; 32],
    pub root: DraftRoot,
    /// The site derives from a base (`output = base`), from the CFS: it may
    /// only push. Set-once fields cannot enforce that on their own — a base
    /// field never written stores `Unit`, whose root is the absent-field root,
    /// so a witness could present it as absent and a `Set` would apply
    /// (`incremental-draft-materialization` §Continuation on the draft
    /// buffer, item 3).
    #[serde(default)]
    pub derived: bool,
}

/// One step's draft transition, as both the guest (replay journal + witness)
/// and the recorder (native witness) derive it: `root_after` is what
/// `apply_draft_ops` reaches from `root_before`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DraftStep {
    pub schema_hash: [u8; 32],
    pub root_before: DraftRoot,
    pub root_after: DraftRoot,
    /// Whether the transition writes a set-once field.
    pub sets: bool,
}

impl DraftStep {
    /// The step a native draft witness records, for the **recorder**: the same
    /// `apply_draft_ops` the guest runs over the replay journal's ops, here
    /// over the native capture's. `None` when the tile carried no draft.
    pub fn from_native_witness(
        witness: Option<&crate::draft::DraftTransitionWitness>,
    ) -> crate::Result<Option<Self>> {
        let Some(witness) = witness else {
            return Ok(None);
        };
        let Some(native) = witness.native_transition.as_ref() else {
            return Ok(None);
        };
        let (_, root_after) = crate::draft::apply_draft_ops(&witness.pre_state, &native.ops)?;
        Ok(Some(Self {
            schema_hash: native.schema_hash,
            root_before: native.root_before,
            root_after,
            sets: native
                .ops
                .iter()
                .any(|op| matches!(op, crate::draft::DraftOp::Set { .. })),
        }))
    }
}

impl RecurProgressFrame {
    /// Chain one iteration's carried-state transition onto the frame.
    ///
    /// Iteration 0 *adopts*; every later iteration must *match*. There is no
    /// `if let Some` here on purpose — a live chain whose next iteration omits
    /// its transition is a violation, not a skip.
    ///
    /// `is_first` is passed rather than inferred from `next_iteration_index`,
    /// because the two kinds fold at different moments: a tile folds before its
    /// counter moves, a sequence after. Inferring it made a stateless site's
    /// second iteration look like a first one, which is exactly the case that
    /// must be rejected.
    fn fold_carried_state(
        &mut self,
        state: Option<&RecurStateTransition>,
        is_first: bool,
    ) -> Result<(), RecurProgressViolation> {
        match (self.state_commitment, state) {
            (None, Some(_)) if !is_first => Err(RecurProgressViolation::CarriedStateUnexpected),
            (None, None) => Ok(()),
            (None, Some(transition)) => {
                self.state_commitment = Some(transition.state_out);
                Ok(())
            }
            (Some(_), None) => Err(RecurProgressViolation::CarriedStateOmitted),
            (Some(expected), Some(transition)) => {
                if transition.state_in != expected {
                    return Err(RecurProgressViolation::CarriedStateMismatch {
                        expected,
                        actual: transition.state_in,
                    });
                }
                self.state_commitment = Some(transition.state_out);
                Ok(())
            }
        }
    }

    /// Elements covered so far, derived rather than carried.
    ///
    /// Exact while rule 4 holds, which `advance_tile_iteration` enforces on
    /// every iteration: each consumes `min(C, L − covered_before)`, so `k`
    /// iterations cover `min(k · C, L)`. A recur sequence has no chunking, so
    /// `C == 1` and this is just the iteration count.
    pub fn consumed_total(&self) -> u64 {
        core::cmp::min(
            self.next_iteration_index.saturating_mul(self.chunk.max(1)),
            self.source_len,
        )
    }
}

/// Live recur sites, innermost last.
///
/// Nesting is strictly LIFO — the recorder models the active tile site as a
/// single `Option` and refuses an ordinary tile while iterations are live — so
/// this is a stack, not a map.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct RecurProgressStack(Vec<RecurProgressFrame>);

/// A violation of the recur progress discipline.
///
/// Same shape as [`crate::chunking::ChunkViolation`]: `raster-core` returns a
/// typed error and the guest panics at the call site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecurProgressViolation {
    /// An iteration arrived with no live site to attribute it to.
    NoActiveSite,
    /// The innermost frame's site is not a prefix of the step's coordinates.
    SiteMismatch,
    /// Rules 1 and 2: the first index must be 0 and indices must be contiguous.
    NonContiguousIteration { expected: u64, actual: u64 },
    /// Rule 3: the tile's view of the loop must equal `⌈L / C⌉`.
    DeclaredIterationsMismatch { expected: u64, actual: u64 },
    /// Rule 4: `consumed_elements == min(C, L − consumed_total)`.
    UnexpectedConsumption { expected: u64, actual: u64 },
    /// Rule 6: a `Break` must be terminal.
    IterationAfterBreak,
    /// Rule 5: a terminal `Continue` requires the prefix to be complete.
    IncompleteSweep { source_len: u64, consumed_total: u64 },
    /// Rule 7 / S4: zero iterations are valid iff `L == 0`.
    EmptySweepOverNonEmptySource { source_len: u64 },
    /// S4: a recur sequence's observed iteration count must equal `L`.
    SequenceIterationCountMismatch { expected: u64, actual: u64 },
    /// A site closed that is not the innermost live one.
    SiteNotInnermost,
    /// A live carried-state chain's next iteration supplied no transition.
    /// Deliberately an error rather than a skip: absence is the cheapest attack
    /// on a continuity check, which is what `checks/drafts.rs`'s permissive
    /// `if let Some(..)` still allows for drafts.
    CarriedStateOmitted,
    /// A transition arrived for a site whose chain never started.
    CarriedStateUnexpected,
    /// Iteration *N*'s incoming state is not iteration *N−1*'s outgoing state.
    CarriedStateMismatch { expected: Hash32, actual: Hash32 },
    /// A site returning its carried state closed with no recorded output to
    /// hold the chain's last value against.
    TerminalStateUnwitnessed,
    /// The value a site returned is not the carried state its sweep produced.
    TerminalStateMismatch { expected: Hash32, actual: Hash32 },
    /// Rule 8: the iteration's item is not selected out of the site's source —
    /// another object, or another path inside it.
    ItemNotFromSource,
    /// Rule 8: the item's selection does not end in the step the site's mode
    /// requires — a `Range` for a chunked site, a literal `Index` otherwise.
    ItemSelectionShape,
    /// Rule 8: the item does not start where the sweep has reached.
    ItemOutOfPlace { expected: u64, actual: u64 },
    /// Rule 8: the item's proof folds against a list whose length is not `L`.
    ItemSourceLenMismatch { expected: u64, actual: u64 },
    /// Rule 8: the item holds a different number of elements than the
    /// iteration reports consuming, or than its selector claims.
    ItemWidthMismatch { expected: u64, actual: u64 },
    /// A step carried a draft transition with no site object to apply it to —
    /// outside every site, or inside a state-only one.
    DraftWithoutSiteObject,
    /// A recur tile iteration of a site that owns an object carried no draft
    /// transition: every iteration's return is the site's draft.
    DraftTransitionOmitted,
    /// The transition names another schema than the site's object.
    DraftSchemaMismatch { expected: [u8; 32], actual: [u8; 32] },
    /// The transition starts from another root than the site's object has.
    DraftRootMismatch { expected: DraftRoot, actual: DraftRoot },
    /// The object a site wrote at its close is not the one its steps built.
    DraftOutputMismatch { expected: DraftRoot, actual: [u8; 32] },
    /// A site closed without writing its object.
    SiteOutputMissing,
    /// A deriving site's transition writes a set-once field: derivation is
    /// push-only.
    DerivedSiteSets,
}

impl fmt::Display for RecurProgressViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoActiveSite => write!(f, "recur iteration has no active recur site"),
            Self::CarriedStateOmitted => write!(
                f,
                "recur iteration omitted the carried-state transition its site is chaining"
            ),
            Self::CarriedStateUnexpected => write!(
                f,
                "recur iteration supplied a carried-state transition for a site that carries none"
            ),
            Self::CarriedStateMismatch { expected, actual } => write!(
                f,
                "recur carried state does not continue the previous iteration: expected {:?}, got {:?}",
                expected, actual
            ),
            Self::TerminalStateUnwitnessed => write!(
                f,
                "recur site returns its carried state but recorded no output to hold it against"
            ),
            Self::TerminalStateMismatch { expected, actual } => write!(
                f,
                "recur site output is not the carried state its sweep produced: expected {:?}, got {:?}",
                expected, actual
            ),
            Self::SiteMismatch => write!(
                f,
                "recur progress frame site is not a prefix of the step coordinates"
            ),
            Self::NonContiguousIteration { expected, actual } => write!(
                f,
                "recur iteration index {} is not the expected next index {}",
                actual, expected
            ),
            Self::DeclaredIterationsMismatch { expected, actual } => write!(
                f,
                "recur iteration declares {} iterations but the source implies {}",
                actual, expected
            ),
            Self::UnexpectedConsumption { expected, actual } => write!(
                f,
                "recur iteration consumed {} elements but the declared shape requires {}",
                actual, expected
            ),
            Self::IterationAfterBreak => {
                write!(f, "recur iteration follows a terminating Break")
            }
            Self::IncompleteSweep {
                source_len,
                consumed_total,
            } => write!(
                f,
                "recur sweep ended with Continue after covering {} of {} elements",
                consumed_total, source_len
            ),
            Self::EmptySweepOverNonEmptySource { source_len } => write!(
                f,
                "recur sweep ran zero iterations over a source of {} elements",
                source_len
            ),
            Self::SequenceIterationCountMismatch { expected, actual } => write!(
                f,
                "recur sequence ran {} iterations over a source of {} elements",
                actual, expected
            ),
            Self::ItemNotFromSource => write!(
                f,
                "recur iteration's item is not selected out of its site's source list"
            ),
            Self::ItemSelectionShape => write!(
                f,
                "recur iteration's item selection does not end in the range or index its site's mode requires"
            ),
            Self::ItemOutOfPlace { expected, actual } => write!(
                f,
                "recur iteration's item starts at element {} but the sweep has reached {}",
                actual, expected
            ),
            Self::ItemSourceLenMismatch { expected, actual } => write!(
                f,
                "recur iteration's item is proven against a list of {} elements but the source has {}",
                actual, expected
            ),
            Self::ItemWidthMismatch { expected, actual } => write!(
                f,
                "recur iteration's item holds {} elements but {} were expected",
                actual, expected
            ),
            Self::SiteNotInnermost => {
                write!(f, "recur site closed while a nested site is still live")
            }
            Self::DraftWithoutSiteObject => write!(
                f,
                "step carries a draft transition but no live recur site owns an object"
            ),
            Self::DraftTransitionOmitted => write!(
                f,
                "recur iteration of a site that owns an object carried no draft transition"
            ),
            Self::DraftSchemaMismatch { expected, actual } => write!(
                f,
                "draft transition names schema {:?}, but the site's object is {:?}",
                actual, expected
            ),
            Self::DraftRootMismatch { expected, actual } => write!(
                f,
                "draft transition starts from root {:?}, but the site's object is at {:?}",
                actual, expected
            ),
            Self::DraftOutputMismatch { expected, actual } => write!(
                f,
                "recur site wrote object {:?}, but its steps built {:?}",
                actual, expected
            ),
            Self::SiteOutputMissing => write!(f, "recur site closed without writing its object"),
            Self::DerivedSiteSets => write!(
                f,
                "a recur site deriving from a base wrote a set-once field; derivation only pushes"
            ),
        }
    }
}

/// The identity a recur site's frame records for the list it sweeps: the
/// object and the selector path of the site `Start`'s `"input"` binding.
///
/// Uses `selection.path` — the path the selection proof is pinned to — never
/// `selector`, which nothing verifies. Shared by the recorder, which opens the
/// frame, and the guest, which re-opens it, so the two cannot spell it
/// differently.
pub fn source_identity(binding: &StorageData) -> Hash32 {
    source_identity_parts(
        &binding.coordinates,
        &binding.commitment,
        &binding.selection.path.segments,
    )
}

fn source_identity_parts(
    coordinates: &CfsCoordinates,
    commitment: &[u8],
    path: &[SelectorSegment],
) -> Hash32 {
    let encoded = postcard::to_allocvec(&(coordinates, commitment, path))
        .expect("coordinates, a commitment and a selector path always encode");
    let mut hasher = Sha256::new();
    hasher.update(b"recur-source");
    hasher.update(&encoded);
    hasher.finalize().into()
}

/// Element count of a `0x02` list payload, `None` for any other kind.
fn list_payload_len(bytes: &[u8]) -> Option<u64> {
    if *bytes.first()? != 0x02 {
        return None;
    }
    Some(u64::from_le_bytes(bytes.get(1..9)?.try_into().ok()?))
}

/// `⌈len / chunk⌉`, the iteration count a sweep of `len` elements implies.
fn iteration_count(source_len: u64, chunk: u64) -> u64 {
    source_len.div_ceil(chunk.max(1))
}

impl RecurProgressStack {
    pub fn new() -> Self {
        Self(Vec::new())
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn depth(&self) -> usize {
        self.0.len()
    }

    pub fn innermost(&self) -> Option<&RecurProgressFrame> {
        self.0.last()
    }

    /// `H(b"recur-progress" ‖ postcard(stack))`.
    ///
    /// The empty stack has a canonical value, so *"no loop in flight"* is a
    /// positive statement in the trace rather than an absent field. That is
    /// what lets **any** step seed a window, including an ordinary tile inside
    /// a recur-sequence iteration.
    pub fn commitment(&self) -> Hash32 {
        let mut hasher = Sha256::new();
        hasher.update(b"recur-progress");
        hasher.update(postcard::to_allocvec(self).unwrap_or_default());
        hasher.finalize().into()
    }



    /// Open a site. `source_len` comes from the authenticated `0x0A` metadata
    /// at the site step — **not** from the first item's proof.
    ///
    /// The direction matters: `lazy-list-recur.md` rule 7 (zero iterations
    /// valid iff `L == 0`) is the forged-`len = 0` sweep, and an empty sweep
    /// has no iteration 0 and therefore no item proof to learn `L` from.
    /// Metadata at the site step is the only source that exists there.
    pub fn push_site(
        &mut self,
        site: CfsCoordinates,
        kind: RecurSiteKind,
        chunk: u64,
        source_len: u64,
        state_is_output: bool,
        source: Hash32,
    ) {
        self.0.push(RecurProgressFrame {
            site,
            kind,
            chunk: chunk.max(1),
            source_len,
            next_iteration_index: 0,
            // A site that has run no iterations has not broken out of
            // anything; the first iteration is always legal.
            last_control: RecurControlKind::Continue,
            // Adopted from iteration 0's own transition rather than read off
            // the site's recorded seed. `InputSource::Inline` is a unit variant
            // — the CFS holds no literal bytes — so a recorded seed is a
            // prover-chosen value compared against a prover-chosen value. What
            // this chain guarantees is that the fold is consistent with the
            // seed the prover recorded, not with the seed the program wrote;
            // pinning the latter needs `InlineLiteral { commitment }` and is
            // deliberately out of scope. See `loop-carried-state.md` §4.
            state_commitment: None,
            state_is_output,
            source,
            draft: None,
        });
    }

    /// Open the innermost site's object, at its `Start`: the root the CFS
    /// declares for a creating site, or the base's commitment for a deriving
    /// one. Called right after [`Self::push_site`].
    pub fn open_draft(&mut self, draft: SiteDraft) {
        if let Some(frame) = self.0.last_mut() {
            frame.draft = Some(draft);
        }
    }

    /// Open the innermost site's carried state from a **stored** seed, at its
    /// `Start` (`incremental-draft-materialization` D5b): the seed's object
    /// commitment, which `Start`'s storage read authenticates. Iteration 0 must
    /// then continue it. A site seeded inline has no such fact and adopts
    /// iteration 0's `state_in`, as before.
    pub fn seed_state(&mut self, commitment: Hash32) {
        if let Some(frame) = self.0.last_mut() {
            frame.state_commitment = Some(commitment);
        }
    }

    /// Advance the innermost site's object by one step's draft transition.
    ///
    /// One rule for both families (`incremental-draft-materialization` §Recur
    /// sequences, D5a): any tile step whose replay carries a draft transition
    /// advances the **innermost** live site — a recur tile's iteration, or a
    /// recur sequence's body tile. `is_tile_iteration` adds the "iff" for a
    /// recur tile: each of its iterations returns the site's draft, so a site
    /// that owns an object requires one. A sequence body tile that does not
    /// touch the draft carries none.
    pub fn advance_draft(
        &mut self,
        coordinates: &CfsCoordinates,
        step: Option<&DraftStep>,
        is_tile_iteration: bool,
    ) -> Result<(), RecurProgressViolation> {
        let Some(frame) = self.0.last_mut() else {
            return match step {
                Some(_) => Err(RecurProgressViolation::DraftWithoutSiteObject),
                None => Ok(()),
            };
        };
        if !coordinates_have_prefix(coordinates, &frame.site) {
            return Err(RecurProgressViolation::SiteMismatch);
        }
        match (frame.draft.as_mut(), step) {
            (None, None) => Ok(()),
            (None, Some(_)) => Err(RecurProgressViolation::DraftWithoutSiteObject),
            (Some(_), None) if is_tile_iteration => {
                Err(RecurProgressViolation::DraftTransitionOmitted)
            }
            (Some(_), None) => Ok(()),
            (Some(draft), Some(step)) => {
                if step.schema_hash != draft.schema_hash {
                    return Err(RecurProgressViolation::DraftSchemaMismatch {
                        expected: draft.schema_hash,
                        actual: step.schema_hash,
                    });
                }
                if step.root_before != draft.root {
                    return Err(RecurProgressViolation::DraftRootMismatch {
                        expected: draft.root,
                        actual: step.root_before,
                    });
                }
                if draft.derived && step.sets {
                    return Err(RecurProgressViolation::DerivedSiteSets);
                }
                draft.root = step.root_after;
                Ok(())
            }
        }
    }

    /// Rule 8: the iteration's item is the next slice of the site's source.
    ///
    /// Rules 1–4 pin *how many* elements each iteration consumes, from the
    /// replay journal alone; nothing there says *which*. The item's selection
    /// proof does, by an independent route, and this requires the two to
    /// agree (`lazy-list-recur.md` §6):
    ///
    /// | fact | frame / journal | item selection |
    /// | --- | --- | --- |
    /// | which list | `source` (site `Start`) | object + path before the last segment |
    /// | where it sat | `consumed_total` | `ListRange.start` / `List.index` |
    /// | source length | `L` | `ListRange.len` / `List.len` |
    /// | how much | `consumed_elements` | payload element count |
    ///
    /// Both families: a recur tile's item (`chunked` from the CFS, `consumed`
    /// from its journal) and a recur sequence's (never chunked, one element).
    ///
    /// Call it **before** [`Self::advance_tile_iteration`] /
    /// [`Self::advance_sequence_iteration`], which move
    /// `consumed_total` past this iteration. `item` must already be verified
    /// against `witness` (`checks::store`); the steps read here are the ones
    /// that verification pinned to `item.selection.path`. A `Range` segment is
    /// pinned to its proof step by `start` alone, so its width is taken from
    /// the payload and the segment's `end` is held to it.
    pub fn check_iteration_item(
        &self,
        coordinates: &CfsCoordinates,
        item: &StorageData,
        witness: &SelectionWitness,
        chunked: bool,
        consumed_elements: u64,
    ) -> Result<(), RecurProgressViolation> {
        let frame = self.0.last().ok_or(RecurProgressViolation::NoActiveSite)?;
        if !coordinates_have_prefix(coordinates, &frame.site) {
            return Err(RecurProgressViolation::SiteMismatch);
        }

        let segments = &item.selection.path.segments;
        let (last, prefix) = segments
            .split_last()
            .ok_or(RecurProgressViolation::ItemSelectionShape)?;
        if source_identity_parts(&item.coordinates, &item.commitment, prefix) != frame.source {
            return Err(RecurProgressViolation::ItemNotFromSource);
        }

        let expected_start = frame.consumed_total();
        let (start, proven_len) = match (chunked, last, witness.proof.steps.last()) {
            (
                true,
                SelectorSegment::Range { start, end },
                Some(SelectionProofStep::ListRange {
                    start: proven_start,
                    len,
                    ..
                }),
            ) => {
                let width = list_payload_len(&witness.bytes)
                    .ok_or(RecurProgressViolation::ItemSelectionShape)?;
                if width != consumed_elements {
                    return Err(RecurProgressViolation::ItemWidthMismatch {
                        expected: consumed_elements,
                        actual: width,
                    });
                }
                if end.checked_sub(*start) != Some(width) {
                    return Err(RecurProgressViolation::ItemWidthMismatch {
                        expected: width,
                        actual: end.saturating_sub(*start),
                    });
                }
                if proven_start != start {
                    return Err(RecurProgressViolation::ItemSelectionShape);
                }
                (*start, *len)
            }
            (
                false,
                SelectorSegment::Index(index),
                Some(SelectionProofStep::List {
                    index: proven_index,
                    len,
                    ..
                }),
            ) => {
                if proven_index != index {
                    return Err(RecurProgressViolation::ItemSelectionShape);
                }
                (*index, *len)
            }
            _ => return Err(RecurProgressViolation::ItemSelectionShape),
        };

        if start != expected_start {
            return Err(RecurProgressViolation::ItemOutOfPlace {
                expected: expected_start,
                actual: start,
            });
        }
        if proven_len != frame.source_len {
            return Err(RecurProgressViolation::ItemSourceLenMismatch {
                expected: frame.source_len,
                actual: proven_len,
            });
        }
        Ok(())
    }

    /// Advance the innermost frame by one recur-**tile** iteration.
    ///
    /// Applies rules 1–4 and 6. Rules 5 and 7 are terminal and belong to
    /// [`Self::close_site`], because they constrain how many iterations exist
    /// rather than the shape of any one of them.
    pub fn advance_tile_iteration(
        &mut self,
        coordinates: &CfsCoordinates,
        iteration_index: u64,
        declared_iterations: u64,
        consumed_elements: u64,
        control: RecurControlKind,
        state: Option<&RecurStateTransition>,
    ) -> Result<(), RecurProgressViolation> {
        let frame = self.0.last_mut().ok_or(RecurProgressViolation::NoActiveSite)?;
        if !coordinates_have_prefix(coordinates, &frame.site) {
            return Err(RecurProgressViolation::SiteMismatch);
        }

        // Rule 6, read forward: a `Break` is invisible to the iteration after
        // it, so the *previous* iteration's control is the only thing that can
        // reject this one.
        if frame.last_control == RecurControlKind::Break {
            return Err(RecurProgressViolation::IterationAfterBreak);
        }

        // Rules 1 and 2 together: the first index is 0 because the frame starts
        // at 0, and indices are contiguous because each advance moves by one.
        if iteration_index != frame.next_iteration_index {
            return Err(RecurProgressViolation::NonContiguousIteration {
                expected: frame.next_iteration_index,
                actual: iteration_index,
            });
        }

        // Rule 3: this is where the tile's view of the loop is tied to the
        // authenticated source length.
        let expected_iterations = iteration_count(frame.source_len, frame.chunk);
        if declared_iterations != expected_iterations {
            return Err(RecurProgressViolation::DeclaredIterationsMismatch {
                expected: expected_iterations,
                actual: declared_iterations,
            });
        }

        // Rule 4. One equation replaces a rule plus two exemptions: it forces
        // progress (a non-exhausted source implies at least one element), keeps
        // the running total from passing `L` by construction, and makes a chunk
        // short *exactly* when it is the final source chunk — so `4,1,4,1` at
        // `C = 4, L = 10` is rejected at iteration 1 rather than needing a
        // separate ordering rule.
        //
        // It is unconditional, including on a terminating `Break`. Exempting
        // the terminal iteration would let a prover pick both the chunk size
        // and the stopping point: with `L = 100, C = 4`, one iteration
        // consuming 1 element and returning `Break` would otherwise satisfy
        // every rule while the program declared `chunk = 4`. How much *this*
        // iteration sees is the program's decision; whether there is a *next*
        // one is the tile's.
        let remaining = frame.source_len.saturating_sub(frame.consumed_total());
        let expected_consumed = core::cmp::min(frame.chunk, remaining);
        if consumed_elements != expected_consumed {
            return Err(RecurProgressViolation::UnexpectedConsumption {
                expected: expected_consumed,
                actual: consumed_elements,
            });
        }

        frame.fold_carried_state(state, frame.next_iteration_index == 0)?;
        frame.next_iteration_index += 1;
        frame.last_control = control;
        Ok(())
    }

    /// Advance the innermost frame by one recur-**sequence** iteration.
    ///
    /// Rules S3 only: a sequence's iterations carry no journal, so there is no
    /// consumption or control to check. Each iteration consumes exactly one
    /// element (a recur sequence has no chunking).
    pub fn advance_sequence_iteration(
        &mut self,
        coordinates: &CfsCoordinates,
        iteration_index: u64,
    ) -> Result<(), RecurProgressViolation> {
        let frame = self.0.last_mut().ok_or(RecurProgressViolation::NoActiveSite)?;
        if !coordinates_have_prefix(coordinates, &frame.site) {
            return Err(RecurProgressViolation::SiteMismatch);
        }
        if iteration_index != frame.next_iteration_index {
            return Err(RecurProgressViolation::NonContiguousIteration {
                expected: frame.next_iteration_index,
                actual: iteration_index,
            });
        }
        frame.next_iteration_index += 1;
        Ok(())
    }

    /// Chain a **sequence** iteration's carried state, at its `End`.
    ///
    /// Split from [`Self::advance_sequence_iteration`] because the two facts
    /// arrive at different events: the iteration is counted when it opens, but
    /// what it *produced* is only known when it closes. A recur tile has both at
    /// once and folds them together.
    ///
    /// Under D5b both commitments are object commitments: `state_in` is the
    /// binding the iteration read its state through, `state_out` the binding
    /// its body returned — which the caller has already checked is the object
    /// the CFS's `returns` names inside this iteration, read from storage. So
    /// the chain is the chain of objects the bodies wrote, and the last one is
    /// held to the site's stored result by [`Self::close_site`]: the same
    /// function commits both, which the postcard commitment this replaces
    /// could not offer.
    pub fn fold_sequence_iteration_state(
        &mut self,
        coordinates: &CfsCoordinates,
        state: Option<&RecurStateTransition>,
    ) -> Result<(), RecurProgressViolation> {
        let frame = self.0.last_mut().ok_or(RecurProgressViolation::NoActiveSite)?;
        if !coordinates_have_prefix(coordinates, &frame.site) {
            return Err(RecurProgressViolation::SiteMismatch);
        }
        frame.fold_carried_state(state, frame.next_iteration_index == 1)
    }

    /// Close the innermost site, applying the terminal rules.
    ///
    /// For a **tile** site: rule 5 (a terminal `Continue` requires a complete
    /// prefix), rule 6 (a `Break` permits an incomplete one) and rule 7 (zero
    /// iterations iff `L == 0`).
    ///
    /// For a **sequence** site: S4 alone — the observed iteration count must
    /// equal `L`. There is no prefix/terminal split because a recur sequence
    /// has no early exit to excuse a short sweep, and `count == L` covers
    /// `L == 0` in both directions, so S4 needs no empty-source special case.
    ///
    /// Then the object it wrote, `output_commitment` (empty when it wrote
    /// none): every site writes exactly one object at `[s]`. A site that owns
    /// a draft wrote the object its steps built; a site returning its carried
    /// state wrote that state (D5b) — both commitments are raster roots, the
    /// function storage commits objects with.
    pub fn close_site(
        &mut self,
        site: &CfsCoordinates,
        output_commitment: &[u8],
    ) -> Result<RecurProgressFrame, RecurProgressViolation> {
        let frame = self.0.last().ok_or(RecurProgressViolation::NoActiveSite)?;
        if &frame.site != site {
            return Err(RecurProgressViolation::SiteNotInnermost);
        }
        let frame = self.0.pop().expect("frame was just observed");

        match frame.kind {
            RecurSiteKind::Sequence => {
                if frame.next_iteration_index != frame.source_len {
                    return Err(RecurProgressViolation::SequenceIterationCountMismatch {
                        expected: frame.source_len,
                        actual: frame.next_iteration_index,
                    });
                }
            }
            RecurSiteKind::Tile => {
                // Rule 7: the forged-`len = 0` sweep, and the case the whole
                // mechanism exists for.
                if frame.next_iteration_index == 0 {
                    if frame.source_len != 0 {
                        return Err(RecurProgressViolation::EmptySweepOverNonEmptySource {
                            source_len: frame.source_len,
                        });
                    }
                } else if frame.last_control == RecurControlKind::Continue
                    && frame.consumed_total() != frame.source_len
                {
                    // Rule 5 pins where the prefix *ends*, which rule 4 does
                    // not: rule 4 fixes the size of every iteration that
                    // exists and says nothing about how many exist. With
                    // `C = 4, L = 10`, two iterations consuming `4, 4` and
                    // ending in `Continue` satisfy rules 2, 3 and 4 completely
                    // and stop at 8. Dropping the tail by running fewer
                    // correctly-shaped iterations is what this sees, and it is
                    // the only rule that does.
                    return Err(RecurProgressViolation::IncompleteSweep {
                        source_len: frame.source_len,
                        consumed_total: frame.consumed_total(),
                    });
                }
                // A terminal `Break` permits an incomplete prefix. Splitting
                // the invariant (coverage is always a contiguous prefix) from
                // the terminal condition (complete on `Continue`, free on
                // `Break`) is what makes an early exit expressible without
                // also excusing a truncated sweep.
            }
        }

        if output_commitment.is_empty() {
            return Err(RecurProgressViolation::SiteOutputMissing);
        }
        if let Some(draft) = frame.draft {
            if draft.root.as_slice() != output_commitment {
                return Err(RecurProgressViolation::DraftOutputMismatch {
                    expected: draft.root,
                    actual: output_commitment.try_into().unwrap_or([0u8; 32]),
                });
            }
        }
        if frame.state_is_output {
            // `None` only for a zero-iteration site seeded inline: nothing
            // committed the state, so there is nothing to hold the result to —
            // the inline seed is unpinned, as `push_site` records.
            if let Some(expected) = frame.state_commitment {
                if expected.as_slice() != output_commitment {
                    return Err(RecurProgressViolation::TerminalStateMismatch {
                        expected,
                        actual: output_commitment.try_into().unwrap_or([0u8; 32]),
                    });
                }
            }
        }

        Ok(frame)
    }
}

/// Whether `coordinates` sits at or below `prefix`.
fn coordinates_have_prefix(coordinates: &CfsCoordinates, prefix: &CfsCoordinates) -> bool {
    coordinates.len() >= prefix.len()
        && coordinates
            .iter()
            .zip(prefix.iter())
            .all(|(left, right)| left == right)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cfs::FIRST_COORDINATE;
    use crate::input::{SelectionCommitment, SelectionProof, SelectorPath};
    use alloc::vec;

    /// A site's close always writes its object; frames without a draft or a
    /// returned state accept any commitment.
    const WROTE: &[u8] = &[1u8; 32];

    fn site() -> CfsCoordinates {
        CfsCoordinates(vec![2])
    }

    /// The coordinate of 0-based iteration `index` of `site()`: coordinates are
    /// 1-based, the progress rules count iterations from 0.
    fn iteration(index: u64) -> CfsCoordinates {
        CfsCoordinates(vec![2, index as CfsCoordinate + FIRST_COORDINATE])
    }

    fn tile_stack(source_len: u64, chunk: u64) -> RecurProgressStack {
        let mut stack = RecurProgressStack::new();
        stack.push_site(site(), RecurSiteKind::Tile, chunk, source_len, false, [0u8; 32]);
        stack
    }

    /// Drive a whole unchunked sweep, returning the terminal result.
    fn sweep_unchunked(
        source_len: u64,
        iterations: u64,
        terminal: RecurControlKind,
    ) -> Result<RecurProgressFrame, RecurProgressViolation> {
        let mut stack = tile_stack(source_len, 1);
        for index in 0..iterations {
            let control = if index + 1 == iterations {
                terminal
            } else {
                RecurControlKind::Continue
            };
            stack.advance_tile_iteration(&iteration(index), index, source_len, 1, control, None)?;
        }
        stack.close_site(&site(), WROTE)
    }

    #[test]
    fn a_complete_unchunked_sweep_is_accepted() {
        assert!(sweep_unchunked(3, 3, RecurControlKind::Continue).is_ok());
    }

    // -----------------------------------------------------------------------
    // Rule 8 — the item is the next slice of the site's source
    // -----------------------------------------------------------------------

    /// The site `Start`'s source binding: `lines` inside the object at `[1]`.
    fn source_binding() -> StorageData {
        StorageData {
            coordinates: CfsCoordinates(vec![1]),
            commitment: vec![7; 32],
            selector: Default::default(),
            selection: SelectionCommitment {
                path: SelectorPath::new(vec![SelectorSegment::Field("lines".into())]),
                source_root_hash: [7; 32],
                ..Default::default()
            },
        }
    }

    fn swept_stack(source_len: u64, chunk: u64) -> RecurProgressStack {
        let mut stack = RecurProgressStack::new();
        stack.push_site(
            site(),
            RecurSiteKind::Tile,
            chunk,
            source_len,
            false,
            source_identity(&source_binding()),
        );
        stack
    }

    /// A `0x02` list payload of `width` one-byte leaves.
    fn list_payload(width: u64) -> Vec<u8> {
        let mut bytes = vec![0x02];
        bytes.extend_from_slice(&width.to_le_bytes());
        for element in 0..width {
            bytes.extend_from_slice(&10u64.to_le_bytes());
            bytes.push(0x00);
            bytes.extend_from_slice(&1u64.to_le_bytes());
            bytes.push(element as u8);
        }
        bytes
    }

    /// An iteration item selecting `last` out of `binding`'s list, proven
    /// against a list of `proven_len` elements and carrying `width` of them.
    /// Only the facts rule 8 reads are filled in; the fold itself is
    /// `checks::store`'s and is not re-run here.
    fn item_from(
        binding: &StorageData,
        last: SelectorSegment,
        proven_len: u64,
        width: u64,
    ) -> (StorageData, SelectionWitness) {
        let step = match &last {
            SelectorSegment::Range { start, .. } => SelectionProofStep::ListRange {
                start: *start,
                len: proven_len,
                siblings: Vec::new(),
            },
            SelectorSegment::Index(index) => SelectionProofStep::List {
                index: *index,
                len: proven_len,
                siblings: Vec::new(),
            },
            _ => unreachable!("items end in a range or an index"),
        };
        let mut segments = binding.selection.path.segments.clone();
        segments.push(last);
        let path = SelectorPath::new(segments);
        let item = StorageData {
            coordinates: binding.coordinates.clone(),
            commitment: binding.commitment.clone(),
            selector: path.clone(),
            selection: SelectionCommitment {
                path: path.clone(),
                ..binding.selection.clone()
            },
        };
        let witness = SelectionWitness {
            bytes: list_payload(width),
            proof: SelectionProof {
                path,
                root_hash: [7; 32],
                steps: vec![step],
            },
            selected_root: None,
        };
        (item, witness)
    }

    fn slice_item(last: SelectorSegment, proven_len: u64, width: u64) -> (StorageData, SelectionWitness) {
        item_from(&source_binding(), last, proven_len, width)
    }

    fn range(start: u64, end: u64) -> SelectorSegment {
        SelectorSegment::Range { start, end }
    }

    /// Drive a chunked sweep whose journal reports the honest counts while
    /// iteration `i` reads `ranges[i]`, applying rule 8 then rules 1–4.
    fn chunked_sweep(
        source_len: u64,
        chunk: u64,
        ranges: &[(u64, u64)],
    ) -> Result<RecurProgressFrame, RecurProgressViolation> {
        let mut stack = swept_stack(source_len, chunk);
        let declared = iteration_count(source_len, chunk);
        for (index, (start, end)) in ranges.iter().enumerate() {
            let index = index as u64;
            let consumed = core::cmp::min(chunk, source_len - index * chunk);
            let (item, witness) = slice_item(range(*start, *end), source_len, end - start);
            stack.check_iteration_item(&iteration(index), &item, &witness, true, consumed)?;
            stack.advance_tile_iteration(
                &iteration(index),
                index,
                declared,
                consumed,
                RecurControlKind::Continue,
                None,
            )?;
        }
        stack.close_site(&site(), WROTE)
    }

    #[test]
    fn an_honest_chunked_sweep_satisfies_rule_8() {
        // `L = 9, C = 2`: four full chunks and a short final one.
        let frame = chunked_sweep(9, 2, &[(0, 2), (2, 4), (4, 6), (6, 8), (8, 9)])
            .expect("the honest ranges tile [0, 9)");
        assert_eq!(frame.consumed_total(), 9);
    }

    /// Was `poc_a_sweep_that_rereads_the_first_chunk_passes_every_completeness_rule`
    /// (`selection-unbound-from-execution.md` §3): the issue's worked example,
    /// `L = 10, C = 2`, reading `[0, 2)` five times while the journal reports
    /// the honest counts. Rules 1–7 accepted it; rule 8 stops it at the first
    /// repeat.
    #[test]
    fn a_sweep_that_rereads_the_first_chunk_is_rejected() {
        assert_eq!(
            chunked_sweep(10, 2, &[(0, 2); 5]),
            Err(RecurProgressViolation::ItemOutOfPlace {
                expected: 2,
                actual: 0,
            }),
        );
    }

    /// Was `poc_an_element_sweep_that_rereads_one_element_passes_every_completeness_rule`:
    /// chunking was never the cause, so the unchunked form is pinned the same
    /// way, through the proof's `List.index`.
    #[test]
    fn an_element_sweep_that_rereads_one_element_is_rejected() {
        let mut stack = swept_stack(4, 1);
        for index in 0..2 {
            let (item, witness) = slice_item(SelectorSegment::Index(0), 4, 1);
            let result = stack.check_iteration_item(&iteration(index), &item, &witness, false, 1);
            if index == 0 {
                result.expect("element 0 is where the sweep starts");
            } else {
                assert_eq!(
                    result,
                    Err(RecurProgressViolation::ItemOutOfPlace {
                        expected: 1,
                        actual: 0,
                    }),
                );
            }
            stack
                .advance_tile_iteration(&iteration(index), index, 4, 1, RecurControlKind::Continue, None)
                .expect("rules 1-4 cannot see the position");
        }
    }

    #[test]
    fn an_honest_element_sweep_satisfies_rule_8() {
        let mut stack = swept_stack(3, 1);
        for index in 0..3 {
            let (item, witness) = slice_item(SelectorSegment::Index(index), 3, 1);
            stack
                .check_iteration_item(&iteration(index), &item, &witness, false, 1)
                .expect("element i at iteration i");
            stack
                .advance_tile_iteration(&iteration(index), index, 3, 1, RecurControlKind::Continue, None)
                .expect("honest advance");
        }
        assert!(stack.close_site(&site(), WROTE).is_ok());
    }

    /// Before rule 8 nothing tied an iteration's item to the site's source at
    /// all: recur iterations skip the CFS input check, and `checks::store`
    /// proves only that the item is *some* stored object's slice.
    #[test]
    fn an_item_from_another_list_is_rejected() {
        let stack = swept_stack(10, 2);

        let mut other_object = source_binding();
        other_object.coordinates = CfsCoordinates(vec![3]);
        let (item, witness) = item_from(&other_object, range(0, 2), 10, 2);
        assert_eq!(
            stack.check_iteration_item(&iteration(0), &item, &witness, true, 2),
            Err(RecurProgressViolation::ItemNotFromSource),
        );

        let mut other_field = source_binding();
        other_field.selection.path = SelectorPath::new(vec![SelectorSegment::Field("other".into())]);
        let (item, witness) = item_from(&other_field, range(0, 2), 10, 2);
        assert_eq!(
            stack.check_iteration_item(&iteration(0), &item, &witness, true, 2),
            Err(RecurProgressViolation::ItemNotFromSource),
        );
    }

    #[test]
    fn an_item_proven_against_a_list_of_another_length_is_rejected() {
        let stack = swept_stack(10, 2);
        let (item, witness) = slice_item(range(0, 2), 12, 2);
        assert_eq!(
            stack.check_iteration_item(&iteration(0), &item, &witness, true, 2),
            Err(RecurProgressViolation::ItemSourceLenMismatch {
                expected: 10,
                actual: 12,
            }),
        );
    }

    /// Width, from both sides: the payload must hold what the journal reports
    /// consuming, and the selector's `end` — which the proof does not pin —
    /// must agree with the payload.
    #[test]
    fn an_item_whose_width_disagrees_is_rejected() {
        let stack = swept_stack(10, 2);

        let (item, witness) = slice_item(range(0, 1), 10, 1);
        assert_eq!(
            stack.check_iteration_item(&iteration(0), &item, &witness, true, 2),
            Err(RecurProgressViolation::ItemWidthMismatch {
                expected: 2,
                actual: 1,
            }),
        );

        let (item, mut witness) = slice_item(range(0, 5), 10, 2);
        witness.bytes = list_payload(2);
        assert_eq!(
            stack.check_iteration_item(&iteration(0), &item, &witness, true, 2),
            Err(RecurProgressViolation::ItemWidthMismatch {
                expected: 2,
                actual: 5,
            }),
        );
    }

    /// The selection's last step must be the one the site's mode implies.
    #[test]
    fn an_item_selected_the_wrong_way_is_rejected() {
        let chunked = swept_stack(10, 2);
        let (item, witness) = slice_item(SelectorSegment::Index(0), 10, 1);
        assert_eq!(
            chunked.check_iteration_item(&iteration(0), &item, &witness, true, 2),
            Err(RecurProgressViolation::ItemSelectionShape),
        );

        let unchunked = swept_stack(10, 1);
        let (item, witness) = slice_item(range(0, 1), 10, 1);
        assert_eq!(
            unchunked.check_iteration_item(&iteration(0), &item, &witness, false, 1),
            Err(RecurProgressViolation::ItemSelectionShape),
        );
    }

    #[test]
    fn an_empty_source_with_zero_iterations_is_accepted() {
        assert!(sweep_unchunked(0, 0, RecurControlKind::Continue).is_ok());
    }

    /// Rule 7 — the forged `len = 0` sweep, inverted: a real source with no
    /// iterations at all.
    #[test]
    fn zero_iterations_over_a_non_empty_source_is_rejected() {
        assert_eq!(
            sweep_unchunked(5, 0, RecurControlKind::Continue),
            Err(RecurProgressViolation::EmptySweepOverNonEmptySource { source_len: 5 }),
        );
    }

    /// Rule 5 — correctly-shaped iterations, just too few of them.
    #[test]
    fn a_terminal_continue_after_too_few_iterations_is_rejected() {
        assert_eq!(
            sweep_unchunked(5, 3, RecurControlKind::Continue),
            Err(RecurProgressViolation::IncompleteSweep {
                source_len: 5,
                consumed_total: 3,
            }),
        );
    }

    /// Rule 6 — the same coverage, ended by a `Break`, is legal. This and the
    /// test above are the prefix/terminal split, and they must be read as a
    /// pair: an earlier draft's "coverage is `[0, L)`" rule made the accepting
    /// half impossible.
    #[test]
    fn a_terminal_break_with_the_same_coverage_is_accepted() {
        assert!(sweep_unchunked(5, 3, RecurControlKind::Break).is_ok());
    }

    #[test]
    fn an_iteration_after_a_break_is_rejected() {
        let mut stack = tile_stack(5, 1);
        stack
            .advance_tile_iteration(&iteration(0), 0, 5, 1, RecurControlKind::Break, None)
            .unwrap();
        assert_eq!(
            stack.advance_tile_iteration(&iteration(1), 1, 5, 1, RecurControlKind::Continue, None),
            Err(RecurProgressViolation::IterationAfterBreak),
        );
    }

    #[test]
    fn a_non_zero_first_index_is_rejected() {
        let mut stack = tile_stack(5, 1);
        assert_eq!(
            stack.advance_tile_iteration(&iteration(1), 1, 5, 1, RecurControlKind::Continue, None),
            Err(RecurProgressViolation::NonContiguousIteration {
                expected: 0,
                actual: 1,
            }),
        );
    }

    #[test]
    fn a_gap_in_iteration_indices_is_rejected() {
        let mut stack = tile_stack(5, 1);
        stack
            .advance_tile_iteration(&iteration(0), 0, 5, 1, RecurControlKind::Continue, None)
            .unwrap();
        assert_eq!(
            stack.advance_tile_iteration(&iteration(2), 2, 5, 1, RecurControlKind::Continue, None),
            Err(RecurProgressViolation::NonContiguousIteration {
                expected: 1,
                actual: 2,
            }),
        );
    }

    /// Rule 3 — `declared_iterations` must equal `⌈L / C⌉`.
    #[test]
    fn a_declared_iteration_count_that_disagrees_with_the_source_is_rejected() {
        let mut stack = tile_stack(10, 4);
        assert_eq!(
            stack.advance_tile_iteration(&iteration(0), 0, 2, 4, RecurControlKind::Continue, None),
            Err(RecurProgressViolation::DeclaredIterationsMismatch {
                expected: 3,
                actual: 2,
            }),
        );
    }

    /// Rule 4 — a short chunk is legal only as the *final* source chunk.
    /// `4,4,2` at `C = 4, L = 10` is the accepted shape.
    #[test]
    fn a_short_final_chunk_is_accepted() {
        let mut stack = tile_stack(10, 4);
        for (index, consumed) in [(0u64, 4u64), (1, 4), (2, 2)] {
            stack
                .advance_tile_iteration(
                    &iteration(index),
                    index,
                    3,
                    consumed,
                    RecurControlKind::Continue,
                    None,
                )
                .unwrap_or_else(|e| panic!("iteration {} should be accepted: {}", index, e));
        }
        assert!(stack.close_site(&site(), WROTE).is_ok());
    }

    /// The `4,1,4,1` shape, rejected at iteration 1 — the case the old
    /// ordering rule needed a remembered previous length to catch.
    #[test]
    fn a_short_non_final_chunk_is_rejected_where_it_happens() {
        let mut stack = tile_stack(10, 4);
        stack
            .advance_tile_iteration(&iteration(0), 0, 3, 4, RecurControlKind::Continue, None)
            .unwrap();
        assert_eq!(
            stack.advance_tile_iteration(&iteration(1), 1, 3, 1, RecurControlKind::Continue, None),
            Err(RecurProgressViolation::UnexpectedConsumption {
                expected: 4,
                actual: 1,
            }),
        );
    }

    /// The undersized `Break`: `L = 100`, `C = 4`, one iteration consuming a
    /// single element and stopping. Every other rule passes — the declared
    /// count is right, the sweep is non-empty, `Break` is terminal, and a range
    /// selection of one element would be honest — so rule 4's unconditional
    /// equation is the only thing that rejects it.
    #[test]
    fn an_undersized_terminating_chunk_is_rejected() {
        let mut stack = tile_stack(100, 4);
        assert_eq!(
            stack.advance_tile_iteration(&iteration(0), 0, 25, 1, RecurControlKind::Break, None),
            Err(RecurProgressViolation::UnexpectedConsumption {
                expected: 4,
                actual: 1,
            }),
        );
    }

    /// A `Break` on a *full* chunk mid-source is legal — the pair to the test
    /// above, showing rule 4 constrains the size while rule 6 constrains only
    /// what follows.
    #[test]
    fn a_full_chunk_break_mid_source_is_accepted() {
        let mut stack = tile_stack(100, 4);
        stack
            .advance_tile_iteration(&iteration(0), 0, 25, 4, RecurControlKind::Break, None)
            .unwrap();
        let frame = stack.close_site(&site(), WROTE).expect("Break may stop early");
        assert_eq!(frame.consumed_total(), 4);
    }

    #[test]
    fn a_recur_sequence_must_run_exactly_the_source_length() {
        let mut stack = RecurProgressStack::new();
        stack.push_site(site(), RecurSiteKind::Sequence, 1, 3, false, [0u8; 32]);
        for index in 0..3 {
            stack
                .advance_sequence_iteration(&iteration(index), index)
                .unwrap();
        }
        assert!(stack.close_site(&site(), WROTE).is_ok());
    }

    #[test]
    fn a_short_recur_sequence_is_rejected() {
        let mut stack = RecurProgressStack::new();
        stack.push_site(site(), RecurSiteKind::Sequence, 1, 3, false, [0u8; 32]);
        stack
            .advance_sequence_iteration(&iteration(0), 0)
            .unwrap();
        assert_eq!(
            stack.close_site(&site(), WROTE),
            Err(RecurProgressViolation::SequenceIterationCountMismatch {
                expected: 3,
                actual: 1,
            }),
        );
    }

    /// S4 covers `L == 0` in both directions with no special case.
    #[test]
    fn a_recur_sequence_over_an_empty_source_runs_no_iterations() {
        let mut stack = RecurProgressStack::new();
        stack.push_site(site(), RecurSiteKind::Sequence, 1, 0, false, [0u8; 32]);
        assert!(stack.close_site(&site(), WROTE).is_ok());

        let mut stack = RecurProgressStack::new();
        stack.push_site(site(), RecurSiteKind::Sequence, 1, 0, false, [0u8; 32]);
        stack
            .advance_sequence_iteration(&iteration(0), 0)
            .unwrap();
        assert_eq!(
            stack.close_site(&site(), WROTE),
            Err(RecurProgressViolation::SequenceIterationCountMismatch {
                expected: 0,
                actual: 1,
            }),
        );
    }

    #[test]
    fn an_iteration_outside_the_frame_site_is_rejected() {
        let mut stack = tile_stack(5, 1);
        assert_eq!(
            stack.advance_tile_iteration(
                &CfsCoordinates(vec![7, 1]),
                0,
                5,
                1,
                RecurControlKind::Continue, None),
            Err(RecurProgressViolation::SiteMismatch),
        );
    }

    #[test]
    fn an_iteration_with_no_live_site_is_rejected() {
        let mut stack = RecurProgressStack::new();
        assert_eq!(
            stack.advance_tile_iteration(&iteration(0), 0, 1, 1, RecurControlKind::Continue, None),
            Err(RecurProgressViolation::NoActiveSite),
        );
    }

    /// Nesting: a `call_recur!` inside a recur-sequence iteration pushes and
    /// pops a second frame, and the inner `Break` is attributed to the inner
    /// site — the outer sweep still has to run to `L`.
    #[test]
    fn a_nested_break_does_not_terminate_the_outer_sweep() {
        let outer = CfsCoordinates(vec![2]);
        let inner = CfsCoordinates(vec![2, 2, 3]);
        let mut stack = RecurProgressStack::new();
        stack.push_site(outer.clone(), RecurSiteKind::Sequence, 1, 2, false, [0u8; 32]);

        stack
            .advance_sequence_iteration(&CfsCoordinates(vec![2, 1]), 0)
            .unwrap();
        stack
            .advance_sequence_iteration(&CfsCoordinates(vec![2, 2]), 1)
            .unwrap();

        // The nested site opens, breaks early, and closes — legally.
        stack.push_site(inner.clone(), RecurSiteKind::Tile, 1, 8, false, [0u8; 32]);
        stack
            .advance_tile_iteration(
                &CfsCoordinates(vec![2, 2, 3, 1]),
                0,
                8,
                1,
                RecurControlKind::Break,
                None,
            )
            .unwrap();
        assert!(stack.close_site(&inner, WROTE).is_ok());

        // The outer sweep is unaffected: it ran its full length.
        assert!(stack.close_site(&outer, WROTE).is_ok());
    }

    /// Every field is load-bearing: mutating any of them changes the
    /// commitment, which is what makes a forged seed fail to reproduce the
    /// recorded value.
    #[test]
    fn every_frame_field_changes_the_commitment() {
        let base = tile_stack(10, 2);
        let baseline = base.commitment();

        let mut variants = vec![];
        for mutate in [
            |f: &mut RecurProgressFrame| f.site = CfsCoordinates(vec![9]),
            |f: &mut RecurProgressFrame| f.kind = RecurSiteKind::Sequence,
            |f: &mut RecurProgressFrame| f.chunk = 3,
            |f: &mut RecurProgressFrame| f.source_len = 11,
            |f: &mut RecurProgressFrame| f.next_iteration_index = 1,
            |f: &mut RecurProgressFrame| f.last_control = RecurControlKind::Break,
        ] {
            let mut stack = base.clone();
            mutate(&mut stack.0[0]);
            variants.push(stack.commitment());
        }

        for (index, variant) in variants.iter().enumerate() {
            assert_ne!(*variant, baseline, "field {} did not affect the commitment", index);
        }
    }

    /// "No loop in flight" is a positive statement, not an absent field — that
    /// is what lets an ordinary tile seed a window.
    #[test]
    fn the_empty_stack_has_a_canonical_commitment() {
        assert_eq!(
            RecurProgressStack::new().commitment(),
            RecurProgressStack::default().commitment()
        );
        assert_ne!(
            RecurProgressStack::new().commitment(),
            tile_stack(1, 1).commitment()
        );
    }

    // ---- The site's object (`incremental-draft-materialization` batch C) ----

    fn draft_site(len: u64) -> RecurProgressStack {
        let mut stack = RecurProgressStack::new();
        stack.push_site(site(), RecurSiteKind::Tile, 1, len, false, [0u8; 32]);
        stack.open_draft(SiteDraft {
            schema_hash: [7u8; 32],
            root: [1u8; 32],
            derived: false,
        });
        stack
    }

    fn at_iteration(index: CfsCoordinate) -> CfsCoordinates {
        CfsCoordinates(vec![2, index])
    }

    fn draft_step(before: u8, after: u8) -> DraftStep {
        DraftStep {
            schema_hash: [7u8; 32],
            root_before: [before; 32],
            root_after: [after; 32],
            sets: false,
        }
    }

    #[test]
    fn a_draft_chains_through_iterations_and_the_close_checks_the_object() {
        // `L = 0` keeps the sweep rules out of the way: this is the draft
        // entry alone, which advances independently of the iteration count.
        let mut stack = draft_site(0);
        stack
            .advance_draft(&at_iteration(1), Some(&draft_step(1, 2)), true)
            .expect("iteration 0 continues the opened root");
        stack
            .advance_draft(&at_iteration(2), Some(&draft_step(2, 3)), true)
            .expect("iteration 1 continues iteration 0");
        assert_eq!(
            stack.clone().close_site(&site(), &[9u8; 32]).unwrap_err(),
            RecurProgressViolation::DraftOutputMismatch {
                expected: [3u8; 32],
                actual: [9u8; 32],
            },
        );
        assert!(stack.close_site(&site(), &[3u8; 32]).is_ok());
    }

    #[test]
    fn an_object_owning_tile_iteration_must_carry_a_transition() {
        let mut stack = draft_site(1);
        assert_eq!(
            stack.advance_draft(&at_iteration(1), None, true),
            Err(RecurProgressViolation::DraftTransitionOmitted),
        );
        // A recur sequence's body tile that leaves the draft alone carries none.
        assert!(stack.advance_draft(&CfsCoordinates(vec![2, 1, 1]), None, false).is_ok());
    }

    #[test]
    fn a_transition_with_no_site_object_is_rejected() {
        let mut outside = RecurProgressStack::new();
        assert_eq!(
            outside.advance_draft(&CfsCoordinates(vec![3]), Some(&draft_step(1, 2)), false),
            Err(RecurProgressViolation::DraftWithoutSiteObject),
        );
        let mut state_only = RecurProgressStack::new();
        state_only.push_site(site(), RecurSiteKind::Tile, 1, 1, true, [0u8; 32]);
        assert_eq!(
            state_only.advance_draft(&at_iteration(1), Some(&draft_step(1, 2)), true),
            Err(RecurProgressViolation::DraftWithoutSiteObject),
        );
    }

    #[test]
    fn a_transition_for_another_schema_is_rejected() {
        let mut stack = draft_site(1);
        let mut step = draft_step(1, 2);
        step.schema_hash = [8u8; 32];
        assert_eq!(
            stack.advance_draft(&at_iteration(1), Some(&step), true),
            Err(RecurProgressViolation::DraftSchemaMismatch {
                expected: [7u8; 32],
                actual: [8u8; 32],
            }),
        );
    }

    #[test]
    fn a_zero_iteration_creating_site_stores_its_opening_root() {
        let stack = draft_site(0);
        assert!(stack.clone().close_site(&site(), &[1u8; 32]).is_ok());
        assert!(stack.clone().close_site(&site(), &[2u8; 32]).is_err());
    }

    /// A stored seed opens the chain (D5b): iteration 0 can no longer adopt a
    /// state of its choosing.
    #[test]
    fn a_stored_seed_pins_iteration_zero() {
        let mut stack = RecurProgressStack::new();
        stack.push_site(site(), RecurSiteKind::Tile, 1, 1, true, [0u8; 32]);
        stack.seed_state([5u8; 32]);
        assert_eq!(
            stack.clone().advance_tile_iteration(
                &at_iteration(1),
                0,
                1,
                1,
                RecurControlKind::Continue,
                Some(&RecurStateTransition {
                    state_in: [6u8; 32],
                    state_out: [7u8; 32],
                }),
            ),
            Err(RecurProgressViolation::CarriedStateMismatch {
                expected: [5u8; 32],
                actual: [6u8; 32],
            }),
        );
        stack
            .advance_tile_iteration(
                &at_iteration(1),
                0,
                1,
                1,
                RecurControlKind::Continue,
                Some(&RecurStateTransition {
                    state_in: [5u8; 32],
                    state_out: [7u8; 32],
                }),
            )
            .expect("continues the seed");
        assert!(stack.close_site(&site(), &[7u8; 32]).is_ok());
    }

    /// Derivation is push-only even where set-once cannot tell: a `Set` in a
    /// deriving site's transition is refused, a creating site's is not.
    #[test]
    fn a_deriving_site_refuses_a_set() {
        let mut stack = draft_site(1);
        if let Some(frame) = stack.0.last_mut() {
            frame.draft.as_mut().unwrap().derived = true;
        }
        let mut step = draft_step(1, 2);
        step.sets = true;
        assert_eq!(
            stack.clone().advance_draft(&at_iteration(1), Some(&step), true),
            Err(RecurProgressViolation::DerivedSiteSets),
        );
        step.sets = false;
        assert!(stack.advance_draft(&at_iteration(1), Some(&step), true).is_ok());
    }

    // ---- Window-open attacks on the frame's draft entry ----

    /// A window opening mid-sweep is validated by reproducing its first
    /// step's recorded commitment from the seed. A seed whose draft root is
    /// not the honest one, or which omits the entry, commits differently, so
    /// it cannot reproduce it.
    #[test]
    fn a_seed_forging_or_omitting_the_draft_entry_commits_differently() {
        let honest = draft_site(3);
        let mut forged = draft_site(3);
        forged.0.last_mut().unwrap().draft.as_mut().unwrap().root = [9u8; 32];
        let mut omitted = draft_site(3);
        omitted.0.last_mut().unwrap().draft = None;
        assert_ne!(honest.commitment(), forged.commitment());
        assert_ne!(honest.commitment(), omitted.commitment());
    }

    /// A window opening at the close with a seed whose root is one the sweep
    /// legitimately reached — but not the one it ended on — cannot close on
    /// the object the sweep wrote (the spliced chain of
    /// `carried-state-channel` §Verification).
    #[test]
    fn a_spliced_seed_cannot_close_on_the_written_object() {
        let mut honest = draft_site(0);
        honest
            .advance_draft(&at_iteration(1), Some(&draft_step(1, 2)), true)
            .unwrap();
        let mut spliced = honest.clone();
        honest
            .advance_draft(&at_iteration(2), Some(&draft_step(2, 3)), true)
            .unwrap();
        // `spliced` stands at root 2, a root the sweep produced, while the
        // sweep ended at 3 and wrote 3.
        assert_eq!(
            spliced.close_site(&site(), &[3u8; 32]).unwrap_err(),
            RecurProgressViolation::DraftOutputMismatch {
                expected: [2u8; 32],
                actual: [3u8; 32],
            },
        );
        assert!(honest.close_site(&site(), &[3u8; 32]).is_ok());
    }

    /// `{6}` proven, `{7}` stored: a state-only recur **tile** site's stored
    /// result must be the chain's final state (D5b) — the sequence case is in
    /// the transition guest's tests.
    #[test]
    fn a_state_only_tile_site_must_store_its_final_state() {
        let six = [6u8; 32];
        let seven = [7u8; 32];
        let mut stack = RecurProgressStack::new();
        stack.push_site(site(), RecurSiteKind::Tile, 1, 1, true, [0u8; 32]);
        stack
            .advance_tile_iteration(
                &iteration(0),
                0,
                1,
                1,
                RecurControlKind::Continue,
                Some(&RecurStateTransition {
                    state_in: [1u8; 32],
                    state_out: six,
                }),
            )
            .unwrap();
        assert_eq!(
            stack.clone().close_site(&site(), &seven).unwrap_err(),
            RecurProgressViolation::TerminalStateMismatch {
                expected: six,
                actual: seven,
            },
        );
        assert!(stack.close_site(&site(), &six).is_ok());
    }
}
