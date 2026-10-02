# Issue: `replay-draft-schema-unbound` — a replayed tile adopts the host's draft schema instead of proving its own

Status: open 2026-09-28. **Latent soundness gap**: nothing consumes the draft root at a site's
close today, so no wrong result is accepted *because of* this alone — but every draft check the
guest makes is relative to a schema the host picks. Direction picked, not implemented:
[`incremental-draft-materialization`](../proposals/incremental-draft-materialization.md) §What must
come from the CFS (decision D1).

Related:
- [`incremental-draft-witness`](../proposals/incremental-draft-witness.md) (implemented) — the
  frontier-based witness whose `pre_state.schema` the guest applies ops against.
- [`carried-state-channel`](../proposals/carried-state-channel.md) — lists draft roots as
  *"host-supplied, unchecked"* at a window open. This is the same class one level down: the
  *schema* those roots are computed under is host-supplied at a chain's first link.

## What happens

The tile knows its draft's schema statically, and the replay throws that knowledge away:

- `Draft::new` sets `replay_state.schema_hash = S::schema_hash()` (`raster/src/input.rs:477`).
- `restore_draft_from_replay_handle` (`:628`) then **overwrites** it with the host-supplied handle's
  value: `draft.replay_state.schema_hash = handle.schema_hash;` (`:636`).
- The journal's `DraftReplayTransition.schema_hash` is taken from that field
  (`draft_replay_transition`, `:653`).

So the replay-proven journal proves the ops, but not which schema they belong to. In the transition
guest (`checks/drafts.rs`):

- the witness's `pre_state.schema` must hash to the journal's `schema_hash` (`:53`) — two
  host-chosen values, consistent by construction at a chain's first link;
- the ops are applied against that `pre_state.schema` (`:80`), which decides field modes
  (set-once vs append-only) and the root function;
- continuity against a tracked entry (`:69`) is `if let Some(..)`, so the first link has nothing to
  match.

The tile's own recording is not the problem: `record_replay_push` (`:578`) checks modes against the
real `S`. What is unbound is the schema the *guest* uses to interpret those ops and to compute the
root chain.

## Reproduce

From the code above, by reading; not probed. The shortest probe: a replay handle whose
`schema_hash` differs from `S::schema_hash()` is accepted by the replay entry point (no assertion
compares them), and the resulting journal's `schema_hash` is the handle's.

## Why it matters

Any check that relies on the draft root inherits the schema's looseness. Today none does at the
point it matters — a site's close compares nothing against the draft chain (the `if let Some(..)`
above; `verify_draft_transition` returns early for a site close). The planned close check,
`draft.root == output_commitment` at `RecurEnd`, would compare a root computed under a
host-chosen schema. So this must be fixed no later than that check lands, and it is the smallest
piece of the fix.

## What it is not

- Not the missing close check itself, which `incremental-draft-materialization` adds (§The draft
  root rides in the site's recur-progress frame). This issue is what that check would rest on.
- Not `draft_id`: the journal also carries a host-chosen `draft_id`, which the same proposal
  removes (decision D2); the schema, unlike the id, is load-bearing.

## Directions

Picked, in `incremental-draft-materialization` (D1): the replay asserts
`handle.schema_hash == S::schema_hash()` instead of overwriting it, so each iteration's schema hash
is replay-proven; and the CFS declares each recur site's output schema hash and empty root
(`RecurTileItem.output`, computed by `raster-compiler::schema_walk`), which the guest checks the
journal's hash against. The replay assertion alone is a standalone fix.
