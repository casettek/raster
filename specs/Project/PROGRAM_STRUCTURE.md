# Program Structure (Canonical Spec)

This document specifies the canonical structure and invariants of **Raster programs**. The Raster Core implementation **MUST** adhere to these definitions so that execution is **traceable** and **fraud-provable**.

---

## 0. Scope and Goals

### 0.1 Scope

This spec defines:

- The **structural units** of Raster programs: **Tiles**, **Sequences**, and **Programs**.
- The **execution invariants** that make a run **deterministic** and **traceable**.
- The minimum **static representation** of a program’s allowed executions, called the **Control Flow Schema (CFS)**, sufficient to mechanically deduce “what must run next” from the schema and committed prior values.

This spec does **not** define:

- Any particular surface syntax or authoring language (beyond what is required to disambiguate the program’s semantics).
- Any particular trace commitment scheme or fraud-proof protocol/proof-system implementation; it only specifies the required *commitment material* and transcript *ordering* needed for interoperable verification.

### 0.2 Verification objective (fraud detection)

This document does not specify a dispute protocol or proof system; it specifies the **evidence** a verifier consumes and the **conditions** under which an execution trace MUST be rejected as invalid.

The verifier is assumed to be given:

- A **static schema** (CFS) describing what is allowed to execute and how data flows.
- An **execution trace**: the committed per-step tile execution inputs/outputs for a run of `main`.

An execution is invalid if it:

- Executes the wrong code (wrong artifact identity),
- Executes out of order,
- Uses inputs that are not derivable from the schema + prior committed outputs + external inputs,
- Skips required steps or forges termination.

---

## 1. Fundamental Unit: Tile

### 1.1 Definition

A **Tile** is the atomic unit of computation in Raster.

A Tile is a deterministic, side-effect-free function with an explicit byte-level interface:

- **ABI**: a stable entrypoint that maps canonical-encoded `input` bytes to canonical-encoded output bytes (or an error), i.e. `input: bytes -> Result<bytes>`.
- **Encoding**: Tile inputs and outputs are serialized with a **canonical encoding** declared by the program (see §4.2 and §5.1). The canonical encoding identifier MUST be recorded in the CFS.
- **Semantic model**: a function \(F: B_{in} \to B_{out}\) over bytes, where \(B_{in}\) and \(B_{out}\) are the canonical encodings of the tile’s logical inputs and outputs.

Tiles are authored to run in two execution environments:

- **Native execution**: fast local execution for development, testing, and non-verifiable runs.
- **Verifiable execution (zkVM)**: execution inside a zkVM-based backend that produces verifiable artifacts (e.g., receipts).

Raster requires that each Tile be compilable for (and runnable on) a chosen zkVM backend, and that native execution matches the zkVM semantics at the byte-level interface (same `input` bytes \(\rightarrow\) same `output` bytes).

### 1.2 Tile purity and determinism requirements

A Tile execution **MUST** be deterministic:

- For a given input byte string \(B_{in}\), the output byte string \(B_{out}\) is uniquely determined.

A Tile **MUST NOT** depend on ambient state not represented in its explicit input:

- filesystem, network, wall-clock time, randomness,
- host environment variables, process-global mutable state,
- any external oracle not committed as an explicit input.

If a backend provides additional ambient capabilities, programs that use them are **non-portable** and **MUST NOT** claim fraud-provable behavior under this spec.

### 1.3 Isolation and backend compatibility

Tiles **MUST** be isolatable as standalone backend execution units.

Because Raster requires a zkVM-based backend, Tile code **MUST** be compatible with the supported zkVM guest environment:

- Tile crates SHOULD be `no_std` compatible, and
- any required allocation MUST be explicit and deterministic.

Backends MAY impose stricter constraints, but a Tile that cannot run on the required zkVM backend is not a valid Raster Tile.

### 1.4 Tile I/O arity and encoding rules

Tiles have a fixed input arity and output arity that MUST be declared in the CFS:

- **Inputs**: the number of logical arguments accepted by the tile function.
- **Outputs**: the number of logical values returned by the tile function (0 for unit/`()`, 1 for a single value, \(n\) for an \(n\)-tuple).

Canonical encoding rules:

- If a tile has **0 inputs**, `input` MUST be the canonical encoding of `()`.
- If a tile has **1 input**, `input` MUST be the canonical encoding of that single value.
- If a tile has **k > 1 inputs**, `input` MUST be the canonical encoding of a tuple of arity \(k\), in argument order.
- Output encoding follows the same rule: a single value is encoded directly; multiple outputs are encoded as a tuple.

### 1.5 Tile identity

Each tile has two identities:

- A **TileId**: a stable logical identifier referenced from the CFS (e.g., a function name).
- A **backend artifact identity**: a backend-defined identifier that binds the TileId to a specific compiled artifact (e.g., a zkVM image/method id derived from the tile guest artifact).

Fraud-proving requires:

- The schema MUST specify, for each tile and each backend, the artifact identity that is permitted for execution, or a rule to derive it unambiguously.
- The trace MUST commit to the artifact identity used for each tile execution step (see §5).

### 1.6 Tile execution modes

Every tile MUST be declared as one of:

- **Iterative tile**: executes exactly once per invocation.
- **Recursive tile**: denotes a step-function tile that is executed repeatedly until explicit termination (see §1.7).

The tile mode MUST be recorded in the CFS.

### 1.7 Recursive tile semantics

A recursive tile denotes a **step function** that is executed repeatedly until it terminates.

#### 1.7.1 Step function contract

For a recursive tile with input tuple type \(S\) (the “state”), execution of one iteration MUST follow:

\[
step(S_i) \to (done_i, S_{i+1})
\]

where:

- The **first output** MUST be a boolean `done`.
- The **remaining outputs** MUST form the next iteration’s input tuple (the next state).

Therefore, a recursive tile MUST satisfy the arity invariant:

- **output_count = input_count + 1**

#### 1.7.2 Termination

Recursive execution terminates at the first iteration \(t\) where `done_t == true`.

Rules:

- Termination MUST be determined solely by the tile’s explicit inputs and outputs.
- Infinite recursion is invalid behavior under this spec.
- Runners MAY impose additional bounds (e.g., max iterations) but MUST treat bound-exhaustion as a verifiable failure, not as a successful termination.

#### 1.7.3 Driver and observability

Repetition MUST be driven by Raster orchestration (compiler/runtime/backend), not by ambient recursion:

- The tile MUST NOT rely on Rust call stack growth to represent recursion.
- The host MUST represent recursion as repeated invocations of the same tile artifact, each with explicit input bytes derived from the previous iteration’s outputs.

Each iteration is a distinct **tile execution step**:

- The trace MUST commit to every iteration’s input bytes and output bytes, including the terminating iteration (see §5).

#### 1.7.4 Return value of a recursive invocation

The value returned from invoking a recursive tile is the **full output tuple** of the terminating iteration (including `done=true` and the terminal state).

---

## 2. Fundamental Unit: Sequence

### 2.1 Definition

A **Sequence** is the unit of composition and orchestration.

A Sequence is an ordered list of invocation steps (Tiles or other Sequences) together with explicit data-flow bindings between:

- sequence parameters,
- prior step outputs,
- and (only for the program entry sequence) external inputs.

### 2.2 Scope closure and explicit state flow

A Sequence forms a **closed scope**:

- A Sequence’s only allowed inputs are its declared parameters, and (for the entry sequence only) external inputs.
- Intermediate state MUST be carried only via explicit bindings from prior outputs to later inputs.

A Sequence **MUST NOT** depend on ambient state (filesystem, network, time, randomness) except via explicit inputs.

### 2.3 Allowed operations

To remain traceable/fraud-provable, a Sequence body (its semantics) MUST be representable purely as:

- calls to Tiles, and
- calls to other Sequences.

Any additional host-language computation that affects control flow or dataflow MUST be lowered into Tiles and expressed via explicit calls and bindings.

### 2.4 Invocation semantics

An invocation step:

- identifies a callee by TileId or SequenceId in the CFS, and
- derives its input tuple solely from explicit input bindings.

If the callee is a Tile, the tile’s declared execution mode determines whether this step executes once (iterative) or expands to repeated executions per §1.7 (recursive).

---

## 3. Fundamental Unit: Program

### 3.1 Definition and entry point

A Raster **Program** consists of:

- a set of Tile definitions,
- a set of Sequence definitions,
- and exactly one entry Sequence named **`main`**.

Execution of a program means executing `main`.

### 3.2 External inputs restriction

Only the entry sequence `main` MAY accept external inputs.

All inputs to non-entry sequences and all tile invocations MUST be derived via explicit dataflow:

- from `main`’s external inputs,
- through sequence parameters,
- and through prior step outputs.

This restriction is required so that the full execution is deducible from the schema and the committed external inputs.

---

## 4. Static Representation: Control Flow Schema (CFS)

### 4.1 Purpose

The **Control Flow Schema (CFS)** is the canonical static representation of a Raster program’s *allowed executions*.

Concretely, the CFS exists so that a runner and verifier can mechanically enforce correct execution order and dataflow. Given:

- the CFS,
- the program’s committed external inputs (for `main`),
- and the committed outputs of prior tile execution steps,

the next *single correct* execution step (callee identity + required artifact identity + exact input bytes) MUST be derivable without relying on any host-language interpretation or additional hidden state.

This is the core property needed for traceability and fraud-provability: any execution trace that deviates from the CFS-implied “what must run next” rule is invalid.

### 4.2 Required schema content (minimum)

The CFS MUST contain:

- **Schema metadata**
  - schema version,
  - project/program identifier,
  - canonical encoding identifier (exactly one per program; used for all tile ABI inputs/outputs and externally provided inputs).
- **Tile definitions**
  - TileId,
  - execution mode (iterative/recursive),
  - input/output arity,
  - backend artifact identity (or a derivation rule) sufficient for verification.
- **Sequence definitions**
  - SequenceId,
  - ordered list of invocation steps,
  - for each step:
    - callee kind (tile or sequence),
    - callee id,
    - a full set of input bindings (one per callee input).
- **Entry point**
  - identification of the entry sequence (`main`).

### 4.3 Input binding model

Bindings MUST be explicit and index-addressable so that verifiers can compute required inputs mechanically.

Each binding MUST reference exactly one of:

- **External input**: permitted only within the entry sequence.
- **Sequence parameter**: by 0-based index.
- **Prior step output**: by `(step_index, output_index)` with `step_index` strictly less than the current step index.

Bindings MUST NOT reference:

- future outputs,
- values computed by host-language expressions not represented as tile outputs.

### 4.4 Recursion in the schema

For a step whose callee is a recursive tile, the verifier MUST interpret the step as a loop where:

- iteration 0 input comes from the step’s input bindings,
- iteration \(i+1\) input is derived from iteration \(i\)’s outputs (excluding the leading `done` boolean),
- termination is witnessed by `done=true`.

### 4.5 Artifact identity in the schema

To be fraud-provable, the schema MUST bind TileId to a specific artifact identity.

Because Raster requires a zkVM backend, this MUST include:

- a method/image id derived from the tile guest artifact,
- and the verifier MUST check that any provided receipt/proof is verified against this id.

---

## 5. Traceability and Fraud-Proving Requirements

### 5.1 Execution trace (commitment material)

Because the execution trace commits to byte strings, the program’s canonical encoding MUST be deterministic:

- Equal logical values MUST serialize to equal bytes.
- No serialization step may incorporate non-determinism (e.g., map iteration order) unless explicitly canonically defined.

For each tile execution step, the runner MUST commit to:

- **TileId**
- **artifact identity** (backend-specific)
- **input bytes** \(B_{in}\)
- **output bytes** \(B_{out}\)

For a recursive invocation, the runner MUST commit to the above for **each iteration**, in order.

These commitments MUST be domain-separated and ordered so that a verifier can reproduce the transcript hash deterministically from the same material.

### 5.2 “What must run next” deduction rule

Given:

- the CFS,
- the entry external inputs,
- the already-committed outputs of prior steps,

a verifier MUST be able to determine:

- the next sequence step index,
- the callee id and required artifact identity,
- the exact input bytes required for that step (by applying the input bindings to known prior values),
- and for recursion, the required next iteration input (derived from the prior iteration output).

Any trace that deviates from this uniquely determined next-step rule MUST be rejected.

### 5.3 Errors and invalid behavior

If a tile fails (returns an error), the runner MUST treat the run as failed in a way that is verifiable:

- Either by committing an explicit error output in a canonically defined format, or
- by aborting with an explicit failure record that can be checked by the verifier.

Silent aborts that lose determinism or prevent verification MUST NOT be used for fraud-provable runs.

