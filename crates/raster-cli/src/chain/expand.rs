//! Turning an authored `ChainManifest` into the flat `ChainSpec` everything
//! else works in.
//!
//! Expansion is a **pure, total function of the manifest and its counts** — it
//! reads no files and runs no stages. That is what lets the three consumers
//! which must never execute anything (`chain audit`, `chain run --stage`,
//! `detect_output_fraud`) reconstruct the same stage list the run loop
//! produced, from the counts recorded in the chain commitment.
//!
//! Substitution is **textual**, and names are resolved afterwards against the
//! flat list by the ordinary `validate_spec` rules. Nothing about a name is
//! special because it came from a template — which is what lets one block bind
//! an interior stage of another, as `prefill_range_l{l}` binds
//! `prefill_prepare_aux_l{l}`.
//!
//! See `docs/proposals/chain-repeat.md` §2 and §4.

use super::*;
use raster_core::input::{scalar_leaf_root, IndexWidth};

/// One index in scope during substitution.
///
/// `start` is carried beside `value` because `{i-1}` underflows at
/// `value == start`, and that edge — not zero — is where a block's entry
/// binding takes over.
#[derive(Debug, Clone, Copy)]
struct Index<'a> {
    name: &'a str,
    value: u32,
    start: u32,
}

/// One `{...}` occurrence in a template.
struct Placeholder<'a> {
    name: &'a str,
    /// Spelled `{name-1}` rather than `{name}`.
    previous: bool,
    span: core::ops::Range<usize>,
}

/// A template rendered against the indexes in scope.
enum Rendered {
    Text(String),
    /// The template contained `{i-1}` and `i` is at its start value, so the
    /// name it would produce does not exist. The binding's `first` applies.
    Underflow,
}

/// A manifest's stage list, plus how each repeat block's count was arrived at.
pub(super) struct Expansion {
    pub spec: ChainSpec,
    pub repeats: Vec<RepeatResolution>,
}

/// Counts that have been checked against a `ChainCommitment`.
///
/// A newtype rather than a bare map so "verified before use" is a property of
/// the type instead of a comment: `expand` cannot be called without one, and
/// `verify_shape` is the only thing that makes one. Expansion is then ordering,
/// not trust.
#[derive(Debug)]
pub(super) struct VerifiedCounts(BTreeMap<String, u32>);

impl VerifiedCounts {
    /// The counts exactly as the commitment records them, **unchecked**.
    ///
    /// For reconstructing the chain a claimer *asserts*, which is what a fraud
    /// prover needs: it has to expand the stage list the claimer says they ran
    /// in order to exhibit what is wrong with it. `verify_shape` would refuse
    /// that chain, correctly, and refusing is the wrong move when the whole
    /// task is to produce a receipt for the refusal.
    ///
    /// Never a substitute for `verify_shape`. The name of the type is about who
    /// decided the counts, not about whether they are true — and here it is the
    /// party under examination.
    pub(super) fn as_claimed(chain: &ChainCommitment) -> Self {
        Self(
            chain
                .shape
                .repeats
                .iter()
                .map(|repeat| (repeat.name.clone(), repeat.resolved_count))
                .collect(),
        )
    }
}

/// Where a block's trip count comes from on this pass.
enum CountPlan<'a> {
    /// Derive it from the manifest — the run loop's path.
    Resolve,
    /// Take it from a commitment whose shape has been verified — every
    /// consumer that must not execute anything.
    Verified(&'a VerifiedCounts),
}

/// Expand a manifest, resolving each repeat block's count from the manifest.
///
/// Fails if any block's count comes from a stage — those are only knowable once
/// that stage has run, which is `resolve_partial`'s business.
pub(super) fn resolve(manifest: &ChainManifest) -> Result<Expansion> {
    expand_with(manifest, CountPlan::Resolve)
}

/// A count carried over from a previous invocation, via the run directory's
/// `chain-shape` sidecar.
///
/// `source_stage` travels with the count because a count means nothing without
/// the stage it came out of: reuse has to establish that the producer this
/// manifest names today is the one that produced it. A bare number would make
/// that unaskable.
#[derive(Debug, Clone, Copy)]
pub(super) struct RecordedCount {
    pub width: IndexWidth,
    pub count: u32,
    /// Index into the stage list expanded so far, as `producer_index` derives it.
    pub source_stage: u32,
}

/// A block whose count is not yet knowable, and the stage that will supply it.
pub(super) struct PendingCount {
    pub block: String,
    /// The producing stage, by name and by index into the stages expanded so
    /// far — it precedes the block, so it is already in the list.
    pub from: String,
    pub source_stage: u32,
    pub max: u32,
}

/// As much of a manifest as the counts resolved so far allow.
pub(super) struct PartialExpansion {
    pub spec: ChainSpec,
    pub repeats: Vec<RepeatResolution>,
    /// `None` once every block's count is known — the expansion is complete.
    pub pending: Option<PendingCount>,
}

/// Expand up to the first repeat block whose count is not yet known.
///
/// `known` accumulates as the run loop reads counts out of stages that have
/// finished. Because expansion is total and does no I/O, calling this again
/// with one more count reproduces every stage it produced before — which is
/// what lets the run loop re-expand from scratch each round rather than
/// splicing, and assert that the part already executed did not move.
///
/// A count in `known` is keyed by block name and nothing else, so every other
/// thing the manifest says about that block is re-derived here and checked
/// against it — see `check_recorded_count`.
pub(super) fn resolve_partial(
    manifest: &ChainManifest,
    known: &BTreeMap<String, RecordedCount>,
) -> Result<PartialExpansion> {
    let mut stages: Vec<StageSpec> = Vec::new();
    let mut repeats: Vec<RepeatResolution> = Vec::new();
    let mut exports: BTreeMap<String, String> = BTreeMap::new();

    for item in &manifest.items {
        match item {
            ChainItem::Stage(stage) => stages.push(stage.clone()),
            ChainItem::Repeat(block) => {
                let (count, resolution) = match (&block.count, known.get(&block.name)) {
                    (CountSource::Literal(n), _) => (*n, literal_resolution(block, *n)),
                    (CountSource::Stage { from, max }, Some(recorded)) => {
                        // The producer's index is re-derived rather than carried,
                        // so this stays a pure function of its two arguments.
                        let source_stage = producer_index(&stages, block)?;
                        check_recorded_count(block, from, recorded, source_stage, *max)?;
                        (
                            recorded.count,
                            RepeatResolution {
                                name: block.name.clone(),
                                source_stage: Some(source_stage),
                                source_commitment: Vec::new(),
                                selector: String::new(),
                                // The width the producing stage actually returned.
                                // Not an assumption: `7u32` and `7u64` commit to
                                // different roots, so a guessed width would fail
                                // verification against an honest chain.
                                width: recorded.width,
                                max: *max,
                                resolved_count: recorded.count,
                            },
                        )
                    }
                    (CountSource::Stage { from, max }, None) => {
                        let pending = PendingCount {
                            block: block.name.clone(),
                            from: from.clone(),
                            source_stage: producer_index(&stages, block)?,
                            max: *max,
                        };
                        resolve_exports(&mut stages, &exports);
                        return Ok(PartialExpansion {
                            spec: ChainSpec {
                                stages,
                                inputs: manifest.inputs.clone(),
                            },
                            repeats,
                            pending: Some(pending),
                        });
                    }
                    (CountSource::Input { input, .. }, _) => {
                        return Err(Error::Other(format!(
                            "repeat block '{}': a count read from chain input '{input}' is not \
                             supported yet",
                            block.name
                        )))
                    }
                };
                expand_repeat(block, count, &mut stages, &mut exports)?;
                repeats.push(resolution);
            }
        }
    }

    resolve_exports(&mut stages, &exports);
    Ok(PartialExpansion {
        spec: ChainSpec {
            stages,
            inputs: manifest.inputs.clone(),
        },
        repeats,
        pending: None,
    })
}

/// Where the stage supplying a block's count sits in the expansion so far.
///
/// This is the structural rule: the producer must already exist, which means it
/// precedes the block. Without it a chain could declare a count that depends on
/// the stages the count creates.
fn producer_index(expanded: &[StageSpec], block: &RepeatSpec) -> Result<u32> {
    let CountSource::Stage { from, .. } = &block.count else {
        unreachable!("only called for a stage-sourced count")
    };
    expanded
        .iter()
        .position(|stage| &stage.name == from)
        .map(|index| index as u32)
        .ok_or_else(|| {
            Error::Other(format!(
                "repeat block '{}': its count comes from stage '{from}', which does not run \
                 before it. A count may only be produced by a stage the block does not create",
                block.name
            ))
        })
}

/// Recheck a count that was resolved by some earlier invocation.
///
/// A recorded count is host-local state a run *inherits*, so both of the things
/// the manifest says about it are re-derived here rather than taken from the
/// record: `max` is the manifest's bound, and the producer is the stage
/// expansion has just located. `read_trip_count` applies the same `max` at the
/// moment a count is first read out of a stage; this is that check on the other
/// way in, so a count cannot enter the graph over the bound by having been
/// resolved somewhere else.
///
/// `read_chain_shape` has already discarded a sidecar resolved from a different
/// manifest, so in practice these fire on a sidecar whose body was edited or
/// corrupted under a digest that still matches — the digest covers the manifest,
/// not the record. Refused rather than re-resolved: this function may not do
/// I/O, so it cannot go and ask the producing stage who is right.
fn check_recorded_count(
    block: &RepeatSpec,
    from: &str,
    recorded: &RecordedCount,
    source_stage: u32,
    max: u32,
) -> Result<()> {
    if recorded.count > max {
        return Err(Error::Other(format!(
            "repeat block '{}': the recorded shape asks for {} iteration(s), over the \
             manifest's max of {max}",
            block.name, recorded.count,
        )));
    }
    if recorded.source_stage != source_stage {
        return Err(Error::Other(format!(
            "repeat block '{}': its count comes from stage '{from}', which this manifest \
             places at position {source_stage}, but the recorded shape's count came from \
             position {}. That record describes a different chain",
            block.name, recorded.source_stage,
        )));
    }
    Ok(())
}

fn literal_resolution(block: &RepeatSpec, count: u32) -> RepeatResolution {
    RepeatResolution {
        name: block.name.clone(),
        source_stage: None,
        source_commitment: Vec::new(),
        selector: String::new(),
        width: IndexWidth::U32,
        max: 0,
        resolved_count: count,
    }
}

/// Expand a manifest against counts already checked against a commitment.
pub(super) fn expand(manifest: &ChainManifest, counts: &VerifiedCounts) -> Result<ChainSpec> {
    Ok(expand_with(manifest, CountPlan::Verified(counts))?.spec)
}

/// `sha256` over the canonical encoding of the unexpanded manifest.
///
/// Over the decoded manifest, not the file: `Raster.toml` and `chain.json`
/// describe the same chains, so a digest over bytes would make a verifier
/// holding the other spelling unable to check anything, and would pin an
/// encoding rather than the thing expansion is a function of.
pub(super) fn spec_digest(manifest: &ChainManifest) -> Vec<u8> {
    let bytes = postcard::to_allocvec(manifest).expect("a chain manifest is serializable");
    Sha256::digest(bytes).to_vec()
}

/// Re-derive every repeat block's count and check it against what the
/// commitment records — `docs/proposals/chain-repeat.md` §6, steps 1 and 2.
///
/// Reads the manifest, the commitment, and nothing else. No prover, no trace,
/// no re-execution.
pub(super) fn verify_shape(
    manifest: &ChainManifest,
    chain: &ChainCommitment,
) -> Result<VerifiedCounts> {
    let digest = spec_digest(manifest);
    if digest != chain.shape.spec_digest {
        return Err(Error::Other(format!(
            "shape fraud — the commitment was built from a different chain manifest
               manifest here: {}
  commitment names: {}",
            short_hex(&digest),
            short_hex(&chain.shape.spec_digest),
        )));
    }

    let blocks: Vec<&RepeatSpec> = manifest
        .items
        .iter()
        .filter_map(|item| match item {
            ChainItem::Repeat(block) => Some(block),
            ChainItem::Stage(_) => None,
        })
        .collect();

    // The digest above already pins the manifest, so a count mismatch here is a
    // malformed commitment rather than a substituted manifest. Checked anyway:
    // the whole point of the shape record is that the length is derived, and a
    // derived length that disagrees with itself should say so.
    if blocks.len() != chain.shape.repeats.len() {
        return Err(Error::Other(format!(
            "shape fraud — the manifest declares {} repeat block(s) but the commitment records {}",
            blocks.len(),
            chain.shape.repeats.len(),
        )));
    }

    // The manifest is walked and expanded here rather than only in `expand`,
    // because a stage-sourced count's producer is identified *by position* in
    // the expanded list, and that position is only knowable once the blocks
    // before it have expanded. Each block's count is verified against the
    // producer this walk derives, then the block expands with it — so a count
    // can never be checked against a stage the count itself brought into being.
    let mut counts = BTreeMap::new();
    let mut stages: Vec<StageSpec> = Vec::new();
    let mut exports: BTreeMap<String, String> = BTreeMap::new();
    let mut ordinal = 0usize;

    for item in &manifest.items {
        match item {
            ChainItem::Stage(stage) => stages.push(stage.clone()),
            ChainItem::Repeat(block) => {
                let recorded = &chain.shape.repeats[ordinal];
                if block.name != recorded.name {
                    return Err(Error::Other(format!(
                        "shape fraud — repeat block {ordinal} is '{}' in the manifest and '{}' in \
                         the commitment",
                        block.name, recorded.name,
                    )));
                }
                verify_count(block, recorded, chain, &stages)?;
                expand_repeat(block, recorded.resolved_count, &mut stages, &mut exports)?;
                counts.insert(recorded.name.clone(), recorded.resolved_count);
                ordinal += 1;
            }
        }
    }
    debug_assert_eq!(ordinal, blocks.len());

    Ok(VerifiedCounts(counts))
}

/// One block's count, re-derived from whatever authorized value it names.
///
/// `expanded` is the stage list up to but not including this block — the same
/// prefix the run loop had when it recorded the resolution, and what the
/// producing stage's index is derived against.
fn verify_count(
    block: &RepeatSpec,
    recorded: &RepeatResolution,
    chain: &ChainCommitment,
    expanded: &[StageSpec],
) -> Result<()> {
    let fault = |detail: String| {
        Error::Other(format!(
            "shape fraud — repeat block '{}': {detail}",
            block.name
        ))
    };

    match &block.count {
        CountSource::Literal(n) => {
            if recorded.resolved_count != *n {
                return Err(fault(format!(
                    "the manifest says {n} iterations, the commitment records {}",
                    recorded.resolved_count
                )));
            }
            if recorded.source_stage.is_some() {
                return Err(fault(
                    "a literal count names a producing stage, which it cannot have".to_string(),
                ));
            }
            Ok(())
        }
        CountSource::Stage { from, max } => {
            let recorded_index = recorded.source_stage.ok_or_else(|| {
                fault(format!("the count comes from stage '{from}', but none is recorded"))
            })?;
            // Which stage produced the count is settled by the manifest, not by
            // the record being checked: re-derive the index the same way the run
            // loop did. A recorded index is only ever compared against it, never
            // followed — otherwise a chain could point at any checkpoint whose
            // output happens to encode the count it wants, including one the
            // block itself created, and the count would be authenticated by a
            // stage the manifest never named.
            let index = producer_index(expanded, block)?;
            if recorded_index != index {
                return Err(fault(format!(
                    "its count comes from stage '{from}' at index {index}, but the commitment \
                     names stage {recorded_index} as the producer"
                )));
            }
            let index = index as usize;
            let producer = chain.stages.get(index).ok_or_else(|| {
                fault(format!("names producing stage {index}, past the end of the chain"))
            })?;
            // Position pins which checkpoint is read; this pins that the
            // checkpoint at that position is the stage the manifest named. The
            // two disagree only if `chain.stages` does not correspond to the
            // expansion at all, which `audit` reports stage-for-stage — but the
            // count is verified here, before that comparison runs.
            if producer.name != *from {
                return Err(fault(format!(
                    "its count comes from stage '{from}', but the commitment's stage {index} is \
                     '{}'",
                    producer.name
                )));
            }

            // The bound comes from the manifest, which `spec_digest` pins —
            // never from the record being checked. A recorded `max` that
            // disagrees is a fault rather than something to quietly ignore, so
            // the field cannot become a place to put a convenient number.
            if recorded.max != *max {
                return Err(fault(format!(
                    "the manifest bounds this count at {max}, the commitment records {}",
                    recorded.max
                )));
            }
            if recorded.resolved_count > *max {
                return Err(fault(format!(
                    "resolved to {} iterations, over the manifest's max of {max}",
                    recorded.resolved_count
                )));
            }

            // The producing stage's output *is* the count, so the check is one
            // hash: re-encode what the commitment claims and compare against
            // what that stage committed. Nothing is parsed, so a malformed
            // `output.bin` has no path in here.
            let expected =
                scalar_leaf_root(recorded.width, u64::from(recorded.resolved_count))
                    .ok_or_else(|| {
                        fault(format!(
                            "count {} does not fit its recorded width {:?}",
                            recorded.resolved_count, recorded.width
                        ))
                    })?;
            if expected.as_slice() != producer.output_structural_commitment.as_slice() {
                return Err(fault(format!(
                    "the commitment claims {} iterations, but stage {index} ('{}') committed a \
                     different value",
                    recorded.resolved_count, producer.name,
                )));
            }
            Ok(())
        }
        CountSource::Input { input, .. } => Err(fault(format!(
            "a count read from chain input '{input}' is not supported yet"
        ))),
    }
}

/// Expand a manifest into its flat stage list.
fn expand_with(manifest: &ChainManifest, plan: CountPlan<'_>) -> Result<Expansion> {
    let mut stages: Vec<StageSpec> = Vec::with_capacity(manifest.items.len());
    let mut repeats: Vec<RepeatResolution> = Vec::new();
    // `<block>.<export>` -> the stage name it resolves to.
    let mut exports: BTreeMap<String, String> = BTreeMap::new();

    for item in &manifest.items {
        match item {
            ChainItem::Stage(stage) => stages.push(stage.clone()),
            ChainItem::Repeat(block) => {
                let resolution = count_of(block, &plan)?;
                expand_repeat(block, resolution.resolved_count, &mut stages, &mut exports)?;
                repeats.push(resolution);
            }
        }
    }

    resolve_exports(&mut stages, &exports);
    Ok(Expansion {
        spec: ChainSpec {
            stages,
            inputs: manifest.inputs.clone(),
        },
        repeats,
    })
}

/// The trip count of one block, and the record of where it came from.
fn count_of(block: &RepeatSpec, plan: &CountPlan<'_>) -> Result<RepeatResolution> {
    let mut resolution = RepeatResolution {
        name: block.name.clone(),
        source_stage: None,
        source_commitment: Vec::new(),
        selector: String::new(),
        width: IndexWidth::U32,
        max: 0,
        resolved_count: 0,
    };

    match plan {
        CountPlan::Verified(counts) => {
            resolution.resolved_count = *counts.0.get(&block.name).ok_or_else(|| {
                Error::Other(format!(
                    "repeat block '{}' has no verified count — `verify_shape` must run first",
                    block.name
                ))
            })?;
        }
        CountPlan::Resolve => match &block.count {
            CountSource::Literal(n) => resolution.resolved_count = *n,
            CountSource::Input { input, .. } => {
                return Err(Error::Other(format!(
                    "repeat block '{}': a count read from chain input '{input}' is not supported yet",
                    block.name
                )))
            }
            CountSource::Stage { from, .. } => {
                return Err(Error::Other(format!(
                    "repeat block '{}': a count produced by stage '{from}' is not supported yet",
                    block.name
                )))
            }
        },
    }

    if let CountSource::Stage { max, .. } | CountSource::Input { max, .. } = &block.count {
        resolution.max = *max;
    }
    Ok(resolution)
}

/// Expand one `[[chain.repeat]]` block in place.
///
/// Order is fixed and load-bearing: outer index ascending, then
/// `[[chain.repeat.stage]]` declaration order, then inner index ascending.
/// `InputBindingSource::Chained` records a producer positionally, so a change
/// here is a change to every checkpoint the block produces.
fn expand_repeat(
    block: &RepeatSpec,
    count: u32,
    out: &mut Vec<StageSpec>,
    exports: &mut BTreeMap<String, String>,
) -> Result<()> {
    // Every index range is checked before any of it is materialized, so the
    // additions below cannot overflow. Checked here rather than at parse time
    // because `count` is not always authored — a stage-sourced one arrives from
    // a run or from a commitment, and `expand_repeat` is where the two meet.
    //
    // Inner ranges are checked even when the block is empty, for the reason
    // `render_binding` checks `first` before rendering: the manifest is wrong
    // regardless of which count happens to be zero.
    check_index_range(&format!("repeat block '{}'", block.name), block.start, count)?;
    for template in &block.stages {
        if let Some(inner_count) = template.count {
            check_index_range(
                &format!("repeat block '{}': stage '{}'", block.name, template.name),
                template.start,
                inner_count,
            )?;
        }
    }

    for step in 0..count {
        let outer = Index {
            name: &block.index,
            value: block.start + step,
            start: block.start,
        };

        for template in &block.stages {
            match (&template.index, template.count) {
                (Some(name), Some(inner_count)) => {
                    let mut scope = [outer, outer];
                    for inner_step in 0..inner_count {
                        scope[1] = Index {
                            name,
                            value: template.start + inner_step,
                            start: template.start,
                        };
                        out.push(render_stage(template, &block.name, &scope)?);
                    }
                }
                (None, None) => out.push(render_stage(template, &block.name, &[outer])?),
                // An inner fan is "this many, named by this index"; half of that
                // is a manifest that reads as if it fans out and does not.
                (index, _) => {
                    return Err(Error::Other(format!(
                        "repeat block '{}': stage '{}' declares {} without {} — an inner fan needs both",
                        block.name,
                        template.name,
                        if index.is_some() { "index" } else { "count" },
                        if index.is_some() { "count" } else { "index" },
                    )))
                }
            }
        }
    }

    for (name, decl) in &block.exports {
        let target = if count == 0 {
            // The block contributed no stages, so the export falls back to
            // whatever fed it before the block — per export, because a block
            // emits several stages per iteration and each may fall back
            // somewhere different.
            decl.entry.clone()
        } else {
            let last = Index {
                name: &block.index,
                // Grouped this way on purpose: `start + count` is the index one
                // past the last, and `check_index_range` above bounds the last
                // one — not that.
                value: block.start + (count - 1),
                start: block.start,
            };
            let what = format!("repeat block '{}' export '{name}'", block.name);
            render_required(&decl.stage, &[last], &what)?
        };
        exports.insert(format!("{}.{name}", block.name), target);
    }

    Ok(())
}

/// Whether `start .. start + count` fits a `u32`.
///
/// Refused rather than saturated or widened: an index range that does not fit
/// is unauthorable, and clamping it would make two different manifests expand
/// to the same stage names — the argument `max` makes against clamping a count
/// (`docs/proposals/chain-repeat.md` §3), applied to the range the count
/// produces. Without this the sum wraps in release builds, which is the same
/// collision arriving silently.
fn check_index_range(what: &str, start: u32, count: u32) -> Result<()> {
    // An empty range materializes no index, so it cannot overflow.
    if count == 0 || start.checked_add(count - 1).is_some() {
        return Ok(());
    }
    Err(Error::Other(format!(
        "{what}: its indexes run from {start} for {count} iteration(s), past the end of a u32. \
         `start` plus `count` must be at most {}",
        u32::MAX
    )))
}

/// Render one templated stage against the indexes in scope.
fn render_stage(template: &RepeatStageSpec, block: &str, scope: &[Index<'_>]) -> Result<StageSpec> {
    let what = format!("repeat block '{block}': stage '{}'", template.name);
    let name = render_required(&template.name, scope, &what)?;
    let project = render_required(&template.project, scope, &what)?;

    let mut inputs = BTreeMap::new();
    for (param, binding) in &template.inputs {
        inputs.insert(
            param.clone(),
            render_binding(binding, scope, &format!("{what}, parameter '{param}'"))?,
        );
    }

    Ok(StageSpec {
        name,
        project,
        inputs,
    })
}

/// Render one binding, applying `first` where `{i-1}` underflows.
fn render_binding(
    binding: &RepeatBinding,
    scope: &[Index<'_>],
    what: &str,
) -> Result<InputBinding> {
    match binding {
        RepeatBinding::From { from, first } => {
            // Checked before rendering, so the error fires on every iteration of
            // a mis-authored block rather than only the first — the manifest is
            // wrong regardless of which index happens to underflow.
            if template_reads_previous(from)? && first.is_none() {
                return Err(Error::Other(format!(
                    "{what}: '{from}' reads the previous iteration but declares no `first`. \
                     Every `{{i-1}}` binding needs the stage it resolves to at the block's \
                     entry edge; there is no default"
                )));
            }
            let resolved = match render(from, scope)? {
                Rendered::Text(name) => name,
                Rendered::Underflow => {
                    let first = first.as_ref().expect("checked above");
                    render_required(first, scope, &format!("{what}: `first`"))?
                }
            };
            Ok(InputBinding::From(resolved))
        }
        RepeatBinding::Input { input } => Ok(InputBinding::Input(render_required(
            input, scope, what,
        )?)),
        RepeatBinding::External { external } => Ok(InputBinding::External(ExternalRef {
            path: render_required(&external.path, scope, what)?,
            index_path: match &external.index_path {
                Some(p) => Some(render_required(p, scope, what)?),
                None => None,
            },
            commitment: external.commitment.clone(),
        })),
    }
}

/// Rewrite `{ from = "<block>.<export>" }` references to the stage they name.
///
/// Runs after every block has expanded, so a block may export to a stage that
/// precedes it as well as one that follows. An unknown dotted name is left
/// alone: `validate_spec` reports it as an unknown producer, which is the
/// message the author wants either way.
fn resolve_exports(stages: &mut [StageSpec], exports: &BTreeMap<String, String>) {
    if exports.is_empty() {
        return;
    }
    for stage in stages {
        for binding in stage.inputs.values_mut() {
            if let InputBinding::From(producer) = binding {
                if let Some(target) = exports.get(producer.as_str()) {
                    *producer = target.clone();
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Templates
// ---------------------------------------------------------------------------

/// Locate every `{...}` in a template.
fn placeholders(template: &str) -> Result<Vec<Placeholder<'_>>> {
    let mut found = Vec::new();
    let bytes = template.as_bytes();
    let mut at = 0;

    while let Some(open) = template[at..].find('{').map(|i| at + i) {
        let close = template[open..].find('}').map(|i| open + i).ok_or_else(|| {
            Error::Other(format!("template '{template}' has an unclosed '{{'"))
        })?;
        let body = &template[open + 1..close];
        let (name, previous) = match body.strip_suffix("-1") {
            Some(name) => (name, true),
            None => (body, false),
        };
        if name.is_empty() {
            return Err(Error::Other(format!(
                "template '{template}' has an empty '{{}}' placeholder"
            )));
        }
        found.push(Placeholder {
            name,
            previous,
            span: open..close + 1,
        });
        at = close + 1;
        debug_assert!(at <= bytes.len());
    }

    Ok(found)
}

/// Whether a template reads the previous iteration — i.e. contains `{i-1}`.
///
/// Also enforces the one-`{i-1}`-per-template rule. Two would need two
/// fallbacks and `first` has one slot; rather than growing `first` into a map
/// for a case nothing needs, the form is refused.
fn template_reads_previous(template: &str) -> Result<bool> {
    let previous = placeholders(template)?
        .into_iter()
        .filter(|p| p.previous)
        .count();
    if previous > 1 {
        return Err(Error::Other(format!(
            "template '{template}' reads the previous iteration of more than one index; \
             `first` names a single entry edge, so at most one `{{i-1}}` is allowed"
        )));
    }
    Ok(previous == 1)
}

/// Substitute the indexes in scope, reporting underflow rather than resolving it.
fn render(template: &str, scope: &[Index<'_>]) -> Result<Rendered> {
    let found = placeholders(template)?;
    if found.is_empty() {
        return Ok(Rendered::Text(template.to_string()));
    }

    let mut out = String::with_capacity(template.len());
    let mut copied = 0;
    for placeholder in found {
        let index = scope
            .iter()
            .find(|i| i.name == placeholder.name)
            .ok_or_else(|| {
                Error::Other(format!(
                    "template '{template}' uses index '{}', which is not bound here — \
                     indexes in scope: {}",
                    placeholder.name,
                    if scope.is_empty() {
                        "none".to_string()
                    } else {
                        scope
                            .iter()
                            .map(|i| i.name)
                            .collect::<Vec<_>>()
                            .join(", ")
                    },
                ))
            })?;

        let value = if placeholder.previous {
            if index.value == index.start {
                return Ok(Rendered::Underflow);
            }
            index.value - 1
        } else {
            index.value
        };

        out.push_str(&template[copied..placeholder.span.start]);
        out.push_str(&value.to_string());
        copied = placeholder.span.end;
    }
    out.push_str(&template[copied..]);

    Ok(Rendered::Text(out))
}

/// Render a template that must produce a name — `first`, an export target, a
/// stage name. `{i-1}` underflowing here has nothing to fall back to.
fn render_required(template: &str, scope: &[Index<'_>], what: &str) -> Result<String> {
    match render(template, scope)? {
        Rendered::Text(text) => Ok(text),
        Rendered::Underflow => Err(Error::Other(format!(
            "{what}: '{template}' reads the previous iteration at the block's first \
             iteration, where there is none"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::super::*;
    use super::*;

    fn manifest(text: &str) -> Result<ChainManifest> {
        let doc: RasterTomlDoc = toml::from_str(text).unwrap();
        let table = doc.chain.expect("[chain] table");
        Ok(ChainManifest {
            inputs: flatten_input_decls(table.inputs)?,
            items: merge_chain_items(table.stages, table.repeats),
        })
    }

    fn spec(text: &str) -> Result<ChainSpec> {
        Ok(resolve(&manifest(text)?)?.spec)
    }

    fn names(spec: &ChainSpec) -> Vec<&str> {
        spec.stages.iter().map(|s| s.name.as_str()).collect()
    }

    /// Every stage reduced to what it actually feeds its program: a name, a
    /// project, and per parameter either the producer it chains from or the
    /// resolved `(path, index_path, commitment)` of an external.
    ///
    /// Comparing this rather than `StageSpec` itself is the point — a named
    /// input and an inline external are different `InputBinding`s that resolve
    /// identically, and "identically" is the claim under test.
    fn resolved(spec: &ChainSpec) -> Vec<(String, String, Vec<(String, String)>)> {
        let base = Path::new("/base");
        spec.stages
            .iter()
            .map(|stage| {
                let inputs = stage
                    .inputs
                    .iter()
                    .map(|(param, binding)| {
                        let described = match binding {
                            InputBinding::From(producer) => format!("from:{producer}"),
                            InputBinding::External(ext) => {
                                let (p, i, c, _) = resolve_external(base, ext);
                                format!("ext:{}|{}|{c}", p.display(), i.display())
                            }
                            InputBinding::Input(name) => {
                                let ext = &spec.inputs[name];
                                let (p, i, c, _) = resolve_external(base, ext);
                                format!("ext:{}|{}|{c}", p.display(), i.display())
                            }
                        };
                        (param.clone(), described)
                    })
                    .collect();
                (stage.name.clone(), stage.project.clone(), inputs)
            })
            .collect()
    }

    /// The acceptance shape, hand-written: `raster-inference`'s 35 uniform
    /// `prefill_prepare_aux` stages, each with its own layer file and its own
    /// commitment. Synthetic hashes — the shape is what is under test.
    fn hand_written_aux(layers: u32) -> String {
        let mut out = String::from(
            "[chain]\n\n[[chain.stage]]\nname = \"input_embedding\"\nproject = \"input-embedding\"\n",
        );
        for l in 0..layers {
            out.push_str(&format!(
                r#"
[[chain.stage]]
name = "prefill_prepare_aux_l{l}"
project = "prefill-prepare-aux"
inputs.embedded = {{ from = "input_embedding" }}
inputs.layer = {{ external = {{ path = "prefill-prepare-aux/layer{l}.rastered", index_path = "prefill-prepare-aux/layer{l}.rindex", commitment = "c{l}" }} }}
"#
            ));
        }
        out
    }

    /// The same chain as one repeat block plus one indexed input.
    fn repeat_aux(layers: u32) -> String {
        let commitments = (0..layers)
            .map(|l| format!("\"c{l}\""))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            r#"
[chain]

[chain.input.aux_layer]
index       = "l"
path        = "prefill-prepare-aux/layer{{l}}.rastered"
index_path  = "prefill-prepare-aux/layer{{l}}.rindex"
commitments = [{commitments}]

[[chain.stage]]
name = "input_embedding"
project = "input-embedding"

[[chain.repeat]]
name  = "prefill_aux"
index = "l"
count = {layers}

  [[chain.repeat.stage]]
  name    = "prefill_prepare_aux_l{{l}}"
  project = "prefill-prepare-aux"
  inputs.embedded = {{ from = "input_embedding" }}
  inputs.layer    = {{ input = "aux_layer_{{l}}" }}
"#
        )
    }

    #[test]
    fn a_repeat_block_reproduces_the_hand_written_stages_exactly() {
        // The gate this whole feature turns on: if `[[chain.repeat]]` cannot
        // reproduce the 35 stages `raster-inference` writes out today, the
        // syntax is wrong — and that is worth discovering here, before the
        // chain-commitment format moves.
        let expanded = spec(&repeat_aux(35)).unwrap();
        let control = spec(&hand_written_aux(35)).unwrap();

        assert_eq!(expanded.stages.len(), 36, "one embedding stage plus 35 layers");
        assert_eq!(resolved(&expanded), resolved(&control));
        assert!(validate_spec(&expanded).is_ok());
    }

    #[test]
    fn indexes_are_zero_based_and_honour_start() {
        let s = spec(
            r#"
[chain]
[[chain.repeat]]
name  = "b"
index = "l"
count = 3
  [[chain.repeat.stage]]
  name    = "s_l{l}"
  project = "p"
"#,
        )
        .unwrap();
        assert_eq!(names(&s), ["s_l0", "s_l1", "s_l2"]);

        // `start` is how a segment that begins partway is said — layers 15..34
        // are a different wiring regime from 0..14.
        let s = spec(
            r#"
[chain]
[[chain.repeat]]
name  = "b"
index = "l"
start = 15
count = 3
  [[chain.repeat.stage]]
  name    = "s_l{l}"
  project = "p"
"#,
        )
        .unwrap();
        assert_eq!(names(&s), ["s_l15", "s_l16", "s_l17"]);
    }

    #[test]
    fn first_supplies_the_entry_edge_and_only_there() {
        let s = spec(
            r#"
[chain]
[[chain.stage]]
name = "seed"
project = "p"

[[chain.repeat]]
name  = "b"
index = "t"
count = 3
  [[chain.repeat.stage]]
  name    = "s{t}"
  project = "p"
  inputs.prev = { from = "s{t-1}", first = "seed" }
"#,
        )
        .unwrap();
        let prev: Vec<&str> = s.stages[1..]
            .iter()
            .map(|stage| match &stage.inputs["prev"] {
                InputBinding::From(p) => p.as_str(),
                _ => panic!("expected a chained binding"),
            })
            .collect();
        assert_eq!(prev, ["seed", "s0", "s1"]);
        assert!(validate_spec(&s).is_ok());
    }

    #[test]
    fn first_is_required_wherever_a_template_reads_the_previous_iteration() {
        // Including when `start > 0`, where `{t-1}` would *happen* to name a
        // real earlier stage. Reasoning "it doesn't underflow here" is exactly
        // how an entry edge becomes implicit and then wrong.
        for start in [0, 15] {
            let err = spec(&format!(
                r#"
[chain]
[[chain.repeat]]
name  = "b"
index = "t"
start = {start}
count = 2
  [[chain.repeat.stage]]
  name    = "s{{t}}"
  project = "p"
  inputs.prev = {{ from = "s{{t-1}}" }}
"#
            ))
            .unwrap_err()
            .to_string();
            assert!(err.contains("declares no `first`"), "start={start}: {err}");
        }
    }

    #[test]
    fn at_most_one_previous_index_per_template() {
        let err = spec(
            r#"
[chain]
[[chain.repeat]]
name  = "b"
index = "t"
count = 2
  [[chain.repeat.stage]]
  name    = "s{t}_l{l}"
  index   = "l"
  count   = 2
  project = "p"
  inputs.prev = { from = "s{t-1}_l{l-1}", first = "seed" }
"#,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("more than one index"), "{err}");
    }

    #[test]
    fn an_unbound_index_names_what_is_in_scope() {
        let err = spec(
            r#"
[chain]
[[chain.repeat]]
name  = "b"
index = "t"
count = 1
  [[chain.repeat.stage]]
  name    = "s{t}_l{l}"
  project = "p"
"#,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("index 'l', which is not bound"), "{err}");
        assert!(err.contains("indexes in scope: t"), "{err}");
    }

    #[test]
    fn expansion_order_is_outer_then_declaration_then_inner() {
        // Fixed because `InputBindingSource::Chained` is positional: reordering
        // here silently rewrites every checkpoint the block produces.
        let s = spec(
            r#"
[chain]
[[chain.repeat]]
name  = "b"
index = "t"
count = 2
  [[chain.repeat.stage]]
  name    = "a{t}"
  project = "p"
  [[chain.repeat.stage]]
  name    = "f{t}_l{l}"
  index   = "l"
  count   = 2
  project = "p"
"#,
        )
        .unwrap();
        assert_eq!(
            names(&s),
            ["a0", "f0_l0", "f0_l1", "a1", "f1_l0", "f1_l1"]
        );
    }

    #[test]
    fn an_export_names_the_final_iteration_and_falls_back_when_empty() {
        let chain = |count: u32| {
            format!(
                r#"
[chain]
[[chain.stage]]
name = "prompt"
project = "p"

[[chain.repeat]]
name  = "decode"
index = "t"
count = {count}

  [chain.repeat.exports.transcript]
  stage = "select{{t}}"
  entry = "prompt"

  [[chain.repeat.stage]]
  name    = "select{{t}}"
  project = "p"

[[chain.stage]]
name = "detokenize"
project = "p"
inputs.transcript = {{ from = "decode.transcript" }}
"#
            )
        };

        let s = spec(&chain(3)).unwrap();
        assert!(matches!(
            &s.stages.last().unwrap().inputs["transcript"],
            InputBinding::From(p) if p == "select2"
        ));
        assert!(validate_spec(&s).is_ok());

        // A zero-token generation request still has to detokenize the prompt,
        // so `count = 0` is a real case and the export resolves to `entry`.
        let s = spec(&chain(0)).unwrap();
        assert_eq!(names(&s), ["prompt", "detokenize"]);
        assert!(matches!(
            &s.stages.last().unwrap().inputs["transcript"],
            InputBinding::From(p) if p == "prompt"
        ));
        assert!(validate_spec(&s).is_ok());
    }

    #[test]
    fn one_block_may_bind_an_interior_stage_of_another() {
        // The rule that matters for the real manifest: `prefill_range_l{l}`
        // binds `prefill_prepare_aux_l{l}`, an interior name of a *different*
        // static block, indexed by the consumer's own index. Substitution is
        // textual and names resolve afterwards, so this is ordinary.
        let s = spec(
            r#"
[chain]
[[chain.stage]]
name = "embed"
project = "p"

[[chain.repeat]]
name  = "aux"
index = "l"
count = 3
  [[chain.repeat.stage]]
  name    = "aux_l{l}"
  project = "p"
  inputs.embedded = { from = "embed" }

[[chain.repeat]]
name  = "range"
index = "l"
count = 3
  [[chain.repeat.stage]]
  name    = "range_l{l}"
  project = "p"
  inputs.ple  = { from = "aux_l{l}" }
  inputs.prev = { from = "range_l{l-1}", first = "embed" }
"#,
        )
        .unwrap();
        assert!(validate_spec(&s).is_ok());
        let ple: Vec<&str> = s.stages[4..]
            .iter()
            .map(|stage| match &stage.inputs["ple"] {
                InputBinding::From(p) => p.as_str(),
                _ => panic!(),
            })
            .collect();
        assert_eq!(ple, ["aux_l0", "aux_l1", "aux_l2"]);
    }

    #[test]
    fn an_inner_fan_needs_both_index_and_count() {
        let err = spec(
            r#"
[chain]
[[chain.repeat]]
name  = "b"
index = "t"
count = 1
  [[chain.repeat.stage]]
  name    = "s{t}"
  index   = "l"
  project = "p"
"#,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("declares index without count"), "{err}");
    }

    #[test]
    fn an_index_range_past_the_end_of_a_u32_is_refused() {
        // Not a hypothetical about a hostile manifest so much as the module's
        // own claim: expansion is a *total* function. Unchecked, this addition
        // panics in a debug build and wraps in a release one, and the wrap is
        // worse — `start = u32::MAX` would silently produce an `s_l0` beside a
        // real one.
        let err = spec(
            r#"
[chain]
[[chain.repeat]]
name  = "b"
index = "l"
start = 4294967295
count = 2
  [[chain.repeat.stage]]
  name    = "s_l{l}"
  project = "p"
"#,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("past the end of a u32"), "{err}");
        assert!(err.contains("repeat block 'b'"), "{err}");

        // The inner fan has its own `start` and its own range.
        let err = spec(
            r#"
[chain]
[[chain.repeat]]
name  = "b"
index = "t"
count = 1
  [[chain.repeat.stage]]
  name    = "s{t}_l{l}"
  index   = "l"
  start   = 4294967290
  count   = 10
  project = "p"
"#,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("past the end of a u32"), "{err}");
        assert!(err.contains("stage 's{t}_l{l}'"), "{err}");
    }

    #[test]
    fn a_range_ending_exactly_at_u32_max_still_expands() {
        // The bound is the last index, not the one past it — a range that ends
        // on `u32::MAX` is authorable, and the export target renders against
        // that final iteration.
        let s = spec(
            r#"
[chain]
[[chain.stage]]
name = "seed"
project = "p"

[[chain.repeat]]
name  = "b"
index = "l"
start = 4294967294
count = 2

  [chain.repeat.exports.last]
  stage = "s_l{l}"
  entry = "seed"

  [[chain.repeat.stage]]
  name    = "s_l{l}"
  project = "p"

[[chain.stage]]
name = "sink"
project = "p"
inputs.tail = { from = "b.last" }
"#,
        )
        .unwrap();
        assert_eq!(
            names(&s),
            ["seed", "s_l4294967294", "s_l4294967295", "sink"]
        );
        assert!(matches!(
            &s.stages.last().unwrap().inputs["tail"],
            InputBinding::From(p) if p == "s_l4294967295"
        ));
        assert!(validate_spec(&s).is_ok());
    }

    /// A chain whose one block takes its count from a stage, so `resolve_partial`
    /// either inherits a recorded count or reports the block as pending.
    const RECORDED: &str = r#"
[chain]
[[chain.stage]]
name = "planner"
project = "planner"

[[chain.repeat]]
name  = "steps"
index = "t"
count = { from = "planner", max = 8 }
  [[chain.repeat.stage]]
  name    = "step{t}"
  project = "step"
"#;

    fn recorded(count: u32, source_stage: u32) -> BTreeMap<String, RecordedCount> {
        [(
            "steps".to_string(),
            RecordedCount {
                width: IndexWidth::U64,
                count,
                source_stage,
            },
        )]
        .into_iter()
        .collect()
    }

    #[test]
    fn a_recorded_count_expands_the_block_it_was_resolved_for() {
        let partial = resolve_partial(&manifest(RECORDED).unwrap(), &recorded(3, 0)).unwrap();
        assert!(partial.pending.is_none());
        assert_eq!(names(&partial.spec), ["planner", "step0", "step1", "step2"]);
        // Recorded, not re-derived: the width came from the producing stage and
        // there is nothing in the manifest to recover it from.
        assert_eq!(partial.repeats[0].width, IndexWidth::U64);
        assert_eq!(partial.repeats[0].source_stage, Some(0));
    }

    #[test]
    fn a_recorded_count_over_the_manifests_max_is_refused() {
        // `read_trip_count` bounds a count on the way *out* of the producing
        // stage. This is the same bound on the way in from a previous run, so a
        // count cannot enter the graph over the manifest's max by having been
        // resolved somewhere else — the manifest in hand decides, always.
        let err = resolve_partial(&manifest(RECORDED).unwrap(), &recorded(9, 0))
            .err()
            .expect("a count over max must not expand")
            .to_string();
        assert!(err.contains("over the manifest's max of 8"), "{err}");
        assert!(err.contains("repeat block 'steps'"), "{err}");
    }

    #[test]
    fn a_recorded_count_from_another_producer_is_refused() {
        // A count is only meaningful together with the stage that produced it.
        // The manifest puts `planner` at position 0; a record claiming the count
        // came from somewhere else describes a chain this is not.
        let err = resolve_partial(&manifest(RECORDED).unwrap(), &recorded(3, 1))
            .err()
            .expect("a count from another producer must not expand")
            .to_string();
        assert!(err.contains("describes a different chain"), "{err}");
        assert!(err.contains("position 0"), "{err}");
    }

    #[test]
    fn no_recorded_count_leaves_the_block_pending() {
        // The other half of the pair: with nothing inherited the block is
        // reported against the stage that would supply it, rather than expanded
        // to a guess.
        let partial = resolve_partial(&manifest(RECORDED).unwrap(), &BTreeMap::new()).unwrap();
        let pending = partial.pending.expect("the count is not knowable yet");
        assert_eq!(pending.block, "steps");
        assert_eq!(pending.from, "planner");
        assert_eq!(pending.source_stage, 0);
        assert_eq!(names(&partial.spec), ["planner"]);
    }

    fn checkpoint(name: &str, structural: Vec<u8>) -> StageCheckpoint {
        StageCheckpoint {
            name: name.into(),
            program_commitment: vec![1],
            input_manifest_commitment: vec![2],
            input_bindings: BTreeMap::new(),
            output_payload_commitment: vec![3],
            output_structural_commitment: structural,
        }
    }

    /// A chain whose repeat block takes its count from an earlier stage.
    fn dynamic_manifest() -> ChainManifest {
        manifest(
            r#"
[chain]
[[chain.stage]]
name = "planner"
project = "p"

[[chain.repeat]]
name  = "decode"
index = "t"
count = { from = "planner", max = 128 }
  [[chain.repeat.stage]]
  name    = "step{t}"
  project = "p"
"#,
        )
        .unwrap()
    }

    /// A commitment for that chain claiming `count` iterations, where the
    /// planner committed `committed`.
    fn dynamic_commitment(manifest: &ChainManifest, count: u32, committed: u32) -> ChainCommitment {
        let root = scalar_leaf_root(IndexWidth::U32, u64::from(committed)).unwrap();
        let mut stages = vec![checkpoint("planner", root.to_vec())];
        for t in 0..count {
            stages.push(checkpoint(&format!("step{t}"), vec![t as u8]));
        }
        ChainCommitment {
            stages,
            shape: ChainShape {
                spec_digest: spec_digest(manifest),
                repeats: vec![RepeatResolution {
                    name: "decode".into(),
                    source_stage: Some(0),
                    source_commitment: Vec::new(),
                    selector: String::new(),
                    width: IndexWidth::U32,
                    max: 128,
                    resolved_count: count,
                }],
            },
        }
    }

    #[test]
    fn verify_shape_accepts_a_count_the_producing_stage_committed() {
        let manifest = dynamic_manifest();
        let chain = dynamic_commitment(&manifest, 3, 3);

        let counts = verify_shape(&manifest, &chain).unwrap();
        let spec = expand(&manifest, &counts).unwrap();
        assert_eq!(names(&spec), ["planner", "step0", "step1", "step2"]);
    }

    #[test]
    fn verify_shape_rejects_a_count_the_producing_stage_did_not_commit() {
        // The case the whole shape record exists for: a chain truncated to 3
        // iterations when the planner said 7. The count is re-derived from an
        // authenticated artifact — the planner's own committed output — so
        // claiming 3 requires the planner to have committed 3, which is then an
        // ordinary claim about that stage's execution.
        let manifest = dynamic_manifest();
        let chain = dynamic_commitment(&manifest, 3, 7);

        let err = verify_shape(&manifest, &chain).unwrap_err().to_string();
        assert!(err.contains("shape fraud"), "{err}");
        assert!(err.contains("claims 3 iterations"), "{err}");
    }

    /// The same chain, plus a second stage before the block that the block's
    /// count does *not* come from.
    fn sidecar_manifest() -> ChainManifest {
        manifest(
            r#"
[chain]
[[chain.stage]]
name = "planner"
project = "p"

[[chain.stage]]
name = "sidecar"
project = "p"

[[chain.repeat]]
name  = "decode"
index = "t"
count = { from = "planner", max = 128 }
  [[chain.repeat.stage]]
  name    = "step{t}"
  project = "p"
"#,
        )
        .unwrap()
    }

    fn count_root(n: u32) -> Vec<u8> {
        scalar_leaf_root(IndexWidth::U32, u64::from(n)).unwrap().to_vec()
    }

    fn decode_resolution(source_stage: u32, count: u32) -> RepeatResolution {
        RepeatResolution {
            name: "decode".into(),
            source_stage: Some(source_stage),
            source_commitment: Vec::new(),
            selector: String::new(),
            width: IndexWidth::U32,
            max: 128,
            resolved_count: count,
        }
    }

    #[test]
    fn verify_shape_rejects_a_count_authenticated_by_a_stage_the_manifest_did_not_name() {
        // `source_stage` is a field of the record being checked, so following it
        // would let any checkpoint whose output happens to encode the wanted
        // count stand in for the declared producer: here `planner` committed 3
        // and `sidecar` committed 5, and the chain expands to 5 by pointing at
        // `sidecar`. The index is re-derived from the manifest instead.
        let manifest = sidecar_manifest();
        let mut stages = vec![
            checkpoint("planner", count_root(3)),
            checkpoint("sidecar", count_root(5)),
        ];
        for t in 0..5u32 {
            stages.push(checkpoint(&format!("step{t}"), vec![t as u8]));
        }
        let chain = ChainCommitment {
            stages,
            shape: ChainShape {
                spec_digest: spec_digest(&manifest),
                repeats: vec![decode_resolution(1, 5)],
            },
        };

        let err = verify_shape(&manifest, &chain).unwrap_err().to_string();
        assert!(err.contains("shape fraud"), "{err}");
        assert!(err.contains("names stage 1 as the producer"), "{err}");
    }

    #[test]
    fn a_block_may_not_authenticate_its_own_count() {
        // The circularity `producer_index` exists to prevent, arriving through
        // the commitment rather than the manifest: `step0` only exists because
        // the count says 3, so it cannot be what establishes that the count is 3.
        let manifest = dynamic_manifest();
        let chain = ChainCommitment {
            stages: vec![
                checkpoint("planner", count_root(1)),
                checkpoint("step0", count_root(3)),
                checkpoint("step1", vec![1]),
                checkpoint("step2", vec![2]),
            ],
            shape: ChainShape {
                spec_digest: spec_digest(&manifest),
                repeats: vec![decode_resolution(1, 3)],
            },
        };

        let err = verify_shape(&manifest, &chain).unwrap_err().to_string();
        assert!(err.contains("names stage 1 as the producer"), "{err}");
    }

    #[test]
    fn verify_shape_rejects_a_producer_whose_checkpoint_is_another_stage() {
        // Right index, wrong stage at it: the commitment drops `planner` and
        // puts the stage that committed 5 in its place. Caught here rather than
        // left to `audit`'s stage-for-stage comparison, because this is the
        // check that decides the count is authentic.
        let manifest = sidecar_manifest();
        let mut stages = vec![
            checkpoint("sidecar", count_root(5)),
            checkpoint("planner", count_root(3)),
        ];
        for t in 0..5u32 {
            stages.push(checkpoint(&format!("step{t}"), vec![t as u8]));
        }
        let chain = ChainCommitment {
            stages,
            shape: ChainShape {
                spec_digest: spec_digest(&manifest),
                repeats: vec![decode_resolution(0, 5)],
            },
        };

        let err = verify_shape(&manifest, &chain).unwrap_err().to_string();
        assert!(err.contains("the commitment's stage 0 is 'sidecar'"), "{err}");
    }

    #[test]
    fn verify_shape_rejects_a_commitment_built_from_another_manifest() {
        let manifest = dynamic_manifest();
        let mut chain = dynamic_commitment(&manifest, 3, 3);
        chain.shape.spec_digest = vec![0xde, 0xad];

        let err = verify_shape(&manifest, &chain).unwrap_err().to_string();
        assert!(err.contains("different chain manifest"), "{err}");
    }

    #[test]
    fn verify_shape_rejects_a_count_over_the_manifests_max() {
        // `max` bounds the work from the manifest alone, before any value is
        // read, so a hostile count cannot ask for 10^9 stages.
        let manifest = dynamic_manifest();
        let chain = dynamic_commitment(&manifest, 200, 200);

        let err = verify_shape(&manifest, &chain).unwrap_err().to_string();
        assert!(err.contains("over the manifest's max of 128"), "{err}");
    }

    #[test]
    fn verify_shape_takes_the_bound_from_the_manifest_not_the_record() {
        // Otherwise `max` would be a number the party being checked gets to
        // choose, which is the opposite of what it is for.
        let manifest = dynamic_manifest();
        let mut chain = dynamic_commitment(&manifest, 3, 3);
        chain.shape.repeats[0].max = 1_000_000;

        let err = verify_shape(&manifest, &chain).unwrap_err().to_string();
        assert!(err.contains("bounds this count at 128"), "{err}");
    }

    #[test]
    fn verify_shape_rejects_a_width_the_count_does_not_fit() {
        // `IndexWidth::encode` refuses rather than truncating, and that refusal
        // carries up: a recorded count of 300 at width U8 must not be allowed to
        // match a stage that committed 44.
        let manifest = manifest(
            r#"
[chain]
[[chain.stage]]
name = "planner"
project = "p"

[[chain.repeat]]
name  = "decode"
index = "t"
count = { from = "planner", max = 1000 }
  [[chain.repeat.stage]]
  name    = "step{t}"
  project = "p"
"#,
        )
        .unwrap();
        let chain = ChainCommitment {
            stages: vec![checkpoint("planner", vec![0])],
            shape: ChainShape {
                spec_digest: spec_digest(&manifest),
                repeats: vec![RepeatResolution {
                    name: "decode".into(),
                    source_stage: Some(0),
                    source_commitment: Vec::new(),
                    selector: String::new(),
                    width: IndexWidth::U8,
                    max: 1000,
                    resolved_count: 300,
                }],
            },
        };

        let err = verify_shape(&manifest, &chain).unwrap_err().to_string();
        assert!(err.contains("does not fit its recorded width"), "{err}");
    }

    #[test]
    fn a_literal_count_needs_no_stage_and_may_not_claim_one() {
        let manifest = manifest(
            r#"
[chain]
[[chain.repeat]]
name  = "b"
index = "l"
count = 2
  [[chain.repeat.stage]]
  name    = "s{l}"
  project = "p"
"#,
        )
        .unwrap();
        let mut chain = ChainCommitment {
            stages: vec![checkpoint("s0", vec![0]), checkpoint("s1", vec![1])],
            shape: ChainShape {
                spec_digest: spec_digest(&manifest),
                repeats: vec![RepeatResolution {
                    name: "b".into(),
                    source_stage: None,
                    source_commitment: Vec::new(),
                    selector: String::new(),
                    width: IndexWidth::U32,
                    max: 0,
                    resolved_count: 2,
                }],
            },
        };
        assert!(verify_shape(&manifest, &chain).is_ok());

        chain.shape.repeats[0].resolved_count = 3;
        let err = verify_shape(&manifest, &chain).unwrap_err().to_string();
        assert!(err.contains("manifest says 2 iterations"), "{err}");
    }

    #[test]
    fn a_non_literal_count_is_named_as_unsupported_for_now() {
        let err = spec(
            r#"
[chain]
[[chain.repeat]]
name  = "decode"
index = "t"
count = { from = "planner", max = 128 }
  [[chain.repeat.stage]]
  name    = "s{t}"
  project = "p"
"#,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("produced by stage 'planner'"), "{err}");
    }
}
