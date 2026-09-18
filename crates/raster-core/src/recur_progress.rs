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
use crate::draft::RecurStateTransition;
use crate::input::Hash32;

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
            Self::SiteNotInnermost => {
                write!(f, "recur site closed while a nested site is still live")
            }
        }
    }
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
        });
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
    /// `output` is the iteration's own recorded output. When the site returns
    /// its carried state, that output *is* the state the iteration produced, in
    /// the same postcard encoding — so this is where `state_out` gets pinned.
    ///
    /// It has to happen here rather than at the site's close. A site's recorded
    /// output is the *raster-encoded* stored object, not postcard bytes, so
    /// comparing the chain against it would be comparing two encodings. The
    /// iteration's output is the one place the two forms coincide.
    ///
    /// This closes the chain's last open end: every other `state_out` is pinned
    /// by the next iteration's bound `state_in`, and the final one is pinned
    /// here, by the bytes the iteration actually emitted.
    pub fn fold_sequence_iteration_state(
        &mut self,
        coordinates: &CfsCoordinates,
        state: Option<&RecurStateTransition>,
        output: Option<&[u8]>,
    ) -> Result<(), RecurProgressViolation> {
        let frame = self.0.last_mut().ok_or(RecurProgressViolation::NoActiveSite)?;
        if !coordinates_have_prefix(coordinates, &frame.site) {
            return Err(RecurProgressViolation::SiteMismatch);
        }
        if frame.state_is_output {
            if let Some(transition) = state {
                let bytes = output.ok_or(RecurProgressViolation::TerminalStateUnwitnessed)?;
                let actual = state_commitment(bytes);
                if actual != transition.state_out {
                    return Err(RecurProgressViolation::TerminalStateMismatch {
                        expected: transition.state_out,
                        actual,
                    });
                }
            }
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
    pub fn close_site(
        &mut self,
        site: &CfsCoordinates,
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

/// `H(b"recur-carried-state" ‖ bytes)` — the commitment over one carried-state
/// value's serialized form.
///
/// **One implementation, three callers**: the tile wrapper (which stamps the
/// pair into the replay journal and the host record), the recorder (which folds
/// the host copy) and the guest (which folds the replay-proven copy). A second
/// copy of this hash would be a divergence no test of either side alone could
/// catch — the same reason `chunking` lives in `raster-core` rather than being
/// written twice.
pub fn state_commitment(bytes: &[u8]) -> Hash32 {
    let mut hasher = Sha256::new();
    hasher.update(b"recur-carried-state");
    hasher.update(bytes);
    hasher.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn site() -> CfsCoordinates {
        CfsCoordinates(vec![2])
    }

    fn iteration(index: u64) -> CfsCoordinates {
        CfsCoordinates(vec![2, index as CfsCoordinate])
    }

    fn tile_stack(source_len: u64, chunk: u64) -> RecurProgressStack {
        let mut stack = RecurProgressStack::new();
        stack.push_site(site(), RecurSiteKind::Tile, chunk, source_len, false);
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
        stack.close_site(&site())
    }

    #[test]
    fn a_complete_unchunked_sweep_is_accepted() {
        assert!(sweep_unchunked(3, 3, RecurControlKind::Continue).is_ok());
    }

    /// PROOF OF CONCEPT — soundness. See
    /// `docs/issues/selection-unbound-from-execution.md` §3.
    ///
    /// The completeness rules decide whether a recur sweep covered its source.
    /// They are handed the journal's self-reported numbers and nothing else:
    /// [`RecurProgressStack::advance_tile_iteration`] takes an iteration index,
    /// a declared iteration count, a consumed-element count and a control
    /// flag — **no range, no selection, no bytes**. The selection proof that
    /// says *where in the source this iteration actually read* is verified
    /// separately, in `checks::store`, and the two are never joined.
    ///
    /// So a sweep that reads the first chunk five times presents the rules with
    /// exactly the facts a complete sweep presents. This builds the issue's
    /// worked example — `L = 10`, `chunk = 2` — and shows the rules close it
    /// clean.
    ///
    /// Invert when rule 8 lands: the cross-check must reject a sweep whose
    /// ranges do not tile `[0, L)`.
    #[test]
    fn poc_a_sweep_that_rereads_the_first_chunk_passes_every_completeness_rule() {
        const SOURCE_LEN: u64 = 10;
        const CHUNK: u64 = 2;
        let declared_iterations = SOURCE_LEN.div_ceil(CHUNK); // 5

        // What each iteration *claims* to have consumed. Identical for the
        // honest sweep and the fabricated one, because `consumed_elements` says
        // how much, never from where.
        let mut stack = tile_stack(SOURCE_LEN, CHUNK);
        for index in 0..declared_iterations {
            stack
                .advance_tile_iteration(
                    &iteration(index),
                    index,
                    declared_iterations,
                    CHUNK,
                    RecurControlKind::Continue,
                    None,
                )
                .expect("rule 4 accepts a full chunk while elements remain");
        }

        // Rules 5-7: the sweep is complete and closes clean.
        let frame = stack.close_site(&site()).expect("a complete sweep closes");
        assert_eq!(frame.consumed_total(), SOURCE_LEN);
        assert_eq!(frame.next_iteration_index, declared_iterations);

        // And here is the hole, stated as an equality rather than a story.
        //
        // The honest sweep reads [0,2) [2,4) [4,6) [6,8) [8,10); the fabricated
        // one reads [0,2) five times. Those are different executions over a
        // different number of the source's elements — but the facts the rules
        // receive are byte-identical, because the range is not among them.
        let honest_ranges: Vec<(u64, u64)> =
            (0..declared_iterations).map(|i| (i * CHUNK, i * CHUNK + CHUNK)).collect();
        let fabricated_ranges: Vec<(u64, u64)> =
            (0..declared_iterations).map(|_| (0, CHUNK)).collect();
        assert_ne!(
            honest_ranges, fabricated_ranges,
            "the two sweeps must really differ, or this proves nothing",
        );

        let rule_inputs = |_ranges: &[(u64, u64)]| -> Vec<(u64, u64, u64)> {
            // Everything `advance_tile_iteration` is told, per iteration. The
            // range argument is deliberately unused: there is nowhere to put it.
            (0..declared_iterations)
                .map(|index| (index, declared_iterations, CHUNK))
                .collect()
        };
        assert_eq!(
            rule_inputs(&honest_ranges),
            rule_inputs(&fabricated_ranges),
            "no completeness rule can distinguish the two sweeps",
        );
    }

    /// The same blindness from the other side: an *element* sweep that rereads
    /// element 0 every iteration also closes clean.
    ///
    /// Included because chunking is not the cause — `consumed_elements` carries
    /// no position at any chunk size, so `chunk = 1` is exposed identically.
    #[test]
    fn poc_an_element_sweep_that_rereads_one_element_passes_every_completeness_rule() {
        assert!(sweep_unchunked(4, 4, RecurControlKind::Continue).is_ok());
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
        assert!(stack.close_site(&site()).is_ok());
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
        let frame = stack.close_site(&site()).expect("Break may stop early");
        assert_eq!(frame.consumed_total(), 4);
    }

    #[test]
    fn a_recur_sequence_must_run_exactly_the_source_length() {
        let mut stack = RecurProgressStack::new();
        stack.push_site(site(), RecurSiteKind::Sequence, 1, 3, false);
        for index in 0..3 {
            stack
                .advance_sequence_iteration(&iteration(index), index)
                .unwrap();
        }
        assert!(stack.close_site(&site()).is_ok());
    }

    #[test]
    fn a_short_recur_sequence_is_rejected() {
        let mut stack = RecurProgressStack::new();
        stack.push_site(site(), RecurSiteKind::Sequence, 1, 3, false);
        stack
            .advance_sequence_iteration(&iteration(0), 0)
            .unwrap();
        assert_eq!(
            stack.close_site(&site()),
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
        stack.push_site(site(), RecurSiteKind::Sequence, 1, 0, false);
        assert!(stack.close_site(&site()).is_ok());

        let mut stack = RecurProgressStack::new();
        stack.push_site(site(), RecurSiteKind::Sequence, 1, 0, false);
        stack
            .advance_sequence_iteration(&iteration(0), 0)
            .unwrap();
        assert_eq!(
            stack.close_site(&site()),
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
                &CfsCoordinates(vec![7, 0]),
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
        let inner = CfsCoordinates(vec![2, 1, 3]);
        let mut stack = RecurProgressStack::new();
        stack.push_site(outer.clone(), RecurSiteKind::Sequence, 1, 2, false);

        stack
            .advance_sequence_iteration(&CfsCoordinates(vec![2, 0]), 0)
            .unwrap();
        stack
            .advance_sequence_iteration(&CfsCoordinates(vec![2, 1]), 1)
            .unwrap();

        // The nested site opens, breaks early, and closes — legally.
        stack.push_site(inner.clone(), RecurSiteKind::Tile, 1, 8, false);
        stack
            .advance_tile_iteration(
                &CfsCoordinates(vec![2, 1, 3, 0]),
                0,
                8,
                1,
                RecurControlKind::Break,
                None,
            )
            .unwrap();
        assert!(stack.close_site(&inner).is_ok());

        // The outer sweep is unaffected: it ran its full length.
        assert!(stack.close_site(&outer).is_ok());
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
}
