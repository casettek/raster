//! Control Flow Schema (CFS) types.
//!
//! This module defines the data structures for representing the control flow
//! and data flow of a Raster application. The CFS captures:
//! - All tiles and their input/output arities
//! - All sequences and their item composition
//! - Data flow bindings between tiles, sequences, and external inputs

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ops::{Deref, DerefMut};
use serde::{Deserialize, Serialize};

use crate::input::SelectorSegment;

/// One step of a CFS position.
///
/// Signed so a scope's **closing** step can occupy a coordinate distinct from
/// its opening one. Before that, a `SequenceStart` and its matching
/// `SequenceEnd` shared a single coordinate, and anything dispatching on
/// position alone could not tell them apart: the witness store served the
/// Start's input to the End, and `try_get_next_coordinates` answered with the
/// Start's successors after a close, rejecting the next iteration of an honest
/// recur sweep.
pub type CfsCoordinate = i32;

/// The first valid coordinate. Positions are **1-based**, so `0` is never a
/// position: it is free to mean "unset", and a stray `vec![0]` fails to resolve
/// instead of silently naming the first item.
///
/// 1-basing is what makes the bracket symmetric — open `i` closes at `-i`, with
/// no offset to carry. At 0-based it could not be, since `-0 == 0` would fold
/// the first item's close back onto its open.
pub const FIRST_COORDINATE: CfsCoordinate = 1;

/// Reserved namespace for synthetic (draft) coordinates, which are not CFS
/// item positions at all. Was `u32::MAX`; `i32::MIN` keeps it out of reach of
/// both real positions and closing markers, whose range is `[-i32::MAX, -1]`.
pub const DRAFT_NAMESPACE: CfsCoordinate = i32::MIN;

/// The coordinate a scope's closing step occupies: the negation of its open.
///
/// `[3, 1]` opens and `[3, -1]` closes. Exact inverse of
/// [`opening_coordinate`], with nothing to offset — see [`FIRST_COORDINATE`].
pub const fn closing_coordinate(open_index: CfsCoordinate) -> CfsCoordinate {
    debug_assert!(open_index >= FIRST_COORDINATE);
    -open_index
}

/// The opening coordinate a closing marker refers to, or `None` when this is an
/// ordinary position. [`DRAFT_NAMESPACE`] is excluded explicitly: it is
/// negative but is not a close marker, and negating `i32::MIN` would overflow.
pub const fn opening_coordinate(coordinate: CfsCoordinate) -> Option<CfsCoordinate> {
    if coordinate < 0 && coordinate != DRAFT_NAMESPACE {
        Some(-coordinate)
    } else {
        None
    }
}

/// Whether this coordinate marks a scope's close rather than a position.
pub const fn is_closing_coordinate(coordinate: CfsCoordinate) -> bool {
    opening_coordinate(coordinate).is_some()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CfsCoordinates(pub Vec<CfsCoordinate>);

impl CfsCoordinates {
    pub fn new() -> Self {
        Self(Vec::new())
    }

    pub fn try_parent(&self) -> Option<(CfsCoordinates, CfsCoordinate)> {
        let (&current_child_index, parent_coords) = self.split_last()?;

        Some((CfsCoordinates(parent_coords.to_vec()), current_child_index))
    }

    /// This position with a trailing close marker resolved back to the scope it
    /// closes.
    ///
    /// Only the last element can ever be a close marker — a scope closes at its
    /// own level, and every step inside it is recorded while it is still open.
    /// Anything asking *where in the CFS* wants this; only code distinguishing a
    /// scope's boundaries from one another wants the raw coordinates.
    pub fn opened(&self) -> CfsCoordinates {
        match self.split_last() {
            Some((&last, prefix)) => match opening_coordinate(last) {
                Some(open_index) => {
                    let mut opened = prefix.to_vec();
                    opened.push(open_index);
                    CfsCoordinates(opened)
                }
                None => self.clone(),
            },
            None => self.clone(),
        }
    }

    /// Whether this position is a scope's closing step.
    pub fn is_closing(&self) -> bool {
        self.last().copied().is_some_and(is_closing_coordinate)
    }
}

impl Default for CfsCoordinates {
    fn default() -> Self {
        Self::new()
    }
}

impl Deref for CfsCoordinates {
    type Target = Vec<CfsCoordinate>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for CfsCoordinates {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

#[derive(Debug, Clone)]
pub struct CfsCursor {
    cfs: ControlFlowSchema,
    entrypoint_coordinate: CfsCoordinate,

    coordinates: CfsCoordinates,
}

impl CfsCursor {
    pub fn new(cfs: ControlFlowSchema) -> Self {
        let entrypoint_coordinate: CfsCoordinate = cfs
            .sequences
            .iter()
            .position(|s| s.id == "main")
            .expect("Missing main entrypoint")
            .try_into()
            .expect("Sequence definitions out of bounds");

        Self {
            cfs,
            entrypoint_coordinate,

            coordinates: CfsCoordinates::new(),
        }
    }

    pub fn coordinates(&self) -> CfsCoordinates {
        self.coordinates.clone()
    }

    pub fn set_coordinates(&mut self, coordinates: CfsCoordinates) {
        self.coordinates = coordinates;
    }

    /// The declared entry-argument names, in canonical order, if `main`
    /// declares any (`SequenceDef::entry_arguments`). `None` means `main`
    /// declares no external arguments at all — there is nothing to authorize
    /// at entry, and the program's `ProgramStart` step binds nothing.
    pub fn main_entrypoint_names(&self) -> Option<&[String]> {
        let main = self
            .cfs
            .sequences
            .get(self.entrypoint_coordinate as usize)?;
        if main.entry_arguments.is_empty() {
            None
        } else {
            Some(main.entry_arguments.as_slice())
        }
    }

    /// Whether `main` declares a program output (`SequenceDef::produces_output`).
    /// When `true`, the program's `ProgramEnd` step must bind a storage-backed
    /// output; when `false`, it binds nothing (a unit program).
    pub fn main_produces_output(&self) -> bool {
        self.cfs
            .sequences
            .get(self.entrypoint_coordinate as usize)
            .map(|main| main.produces_output)
            .unwrap_or(false)
    }

    /// What `main` returns (`SequenceDef::returns`), if the CFS could resolve
    /// it.
    pub fn main_returns(&self) -> Option<&SequenceReturn> {
        self.cfs
            .sequences
            .get(self.entrypoint_coordinate as usize)
            .and_then(|main| main.returns.as_ref())
    }

    pub fn is_next_coordinates(&mut self, next_coordinates: &CfsCoordinates) -> bool {
        if let Some(next_coordinates_options) = self.try_get_next_coordinates(&self.coordinates()) {
            return next_coordinates_options.contains(next_coordinates);
        }

        false
    }

    /// The coordinates the step after `coordinates` may have.
    ///
    /// Every scope — `main`, a nested sequence, a recur site, a recur-sequence
    /// iteration — opens at `…[k]` and closes at `…[-k]` (`main` at `[]` both
    /// ways: `ProgramStart` and `ProgramEnd`). Each set is exact, so the only
    /// way out of a scope is through its close, and a scope with a body
    /// cannot close before running it:
    ///
    /// | step at | successors |
    /// | --- | --- |
    /// | a scope's open | its first child, or its close if it has none |
    /// | a recur site's open `[s]` | iteration `[s][1]`, or the close `[-s]` (zero iterations) |
    /// | a recur tile iteration `[s][i]` | `[s][i+1]`, or `[-s]` |
    /// | a recur-sequence iteration's close `[s][-i]` | `[s][i+1]`, or `[-s]` |
    /// | any other close, or a tile | the next sibling, or the enclosing scope's close |
    ///
    /// Before closes had their own coordinates, a scope's open and close shared
    /// one, and this function answered with the union of both halves'
    /// successors: after a sequence's `Start`, its next sibling (skipping the
    /// `End`); after its last step, its first step again (restarting the
    /// body); after `ProgramStart`, `ProgramEnd` (skipping the program). See
    /// `docs/proposals/incremental-draft-materialization.md` §What the move to
    /// `[-s]` forces.
    pub fn try_get_next_coordinates(
        &self,
        coordinates: &CfsCoordinates,
    ) -> Option<Vec<CfsCoordinates>> {
        if coordinates.is_closing() {
            let opened = coordinates.opened();
            // An iteration's close: the next iteration, or the site's close.
            if let Some((site, iteration)) = self.try_get_recur_iteration_coordinates(&opened) {
                return Some(self.next_iteration_or_close(&site, iteration));
            }
            return self.after_item(&opened);
        }

        if coordinates.is_empty() {
            return Some(self.scope_entry(coordinates));
        }

        if let Some((site, iteration)) = self.try_get_recur_iteration_coordinates(coordinates) {
            return match self.try_get_item_exact(&site)? {
                // A recur tile iteration is one `Exec` record: a leaf.
                SequenceChildItem::RecurTile(_) => {
                    Some(self.next_iteration_or_close(&site, iteration))
                }
                // A recur-sequence iteration opens a body frame.
                _ => Some(self.scope_entry(coordinates)),
            };
        }

        match self.try_get_item_exact(coordinates)? {
            SequenceChildItem::Tile(_) => self.after_item(coordinates),
            SequenceChildItem::Sequence(_) => Some(self.scope_entry(coordinates)),
            // A recur site: its first iteration, or straight to its close.
            SequenceChildItem::RecurTile(_) | SequenceChildItem::RecurSequence(_) => {
                let mut first_iteration = coordinates.clone();
                first_iteration.push(FIRST_COORDINATE);
                Some(Vec::from([
                    first_iteration,
                    self.closing_coordinates_of(coordinates)?,
                ]))
            }
        }
    }

    /// A scope just opened: its first child, or its close when the body is
    /// empty.
    fn scope_entry(&self, scope: &CfsCoordinates) -> Vec<CfsCoordinates> {
        let has_body = self
            .frame_sequence(scope)
            .is_some_and(|sequence| sequence.item_at(FIRST_COORDINATE).is_some());
        if has_body {
            let mut first_child = scope.clone();
            first_child.push(FIRST_COORDINATE);
            Vec::from([first_child])
        } else {
            Vec::from([self.close_of_scope(scope)])
        }
    }

    /// After the item at `item` finished: the next sibling, or the enclosing
    /// scope's close.
    fn after_item(&self, item: &CfsCoordinates) -> Option<Vec<CfsCoordinates>> {
        let (&position, frame) = item.split_last()?;
        let frame = CfsCoordinates(frame.to_vec());
        let sequence = self.frame_sequence(&frame)?;
        if sequence.item_at(position + 1).is_some() {
            let mut next = frame;
            next.push(position + 1);
            Some(Vec::from([next]))
        } else {
            Some(Vec::from([self.close_of_scope(&frame)]))
        }
    }

    fn next_iteration_or_close(
        &self,
        site: &CfsCoordinates,
        iteration: CfsCoordinate,
    ) -> Vec<CfsCoordinates> {
        let mut next = site.clone();
        next.push(iteration + 1);
        Vec::from([next, self.close_of_scope(site)])
    }

    /// Where a scope closes: `[]` for `main` (its `ProgramEnd`), `…[-k]` for
    /// every other scope.
    fn close_of_scope(&self, scope: &CfsCoordinates) -> CfsCoordinates {
        self.closing_coordinates_of(scope)
            .unwrap_or_else(|| scope.clone())
    }

    /// The sequence whose items a frame's children index: `main` at `[]`, a
    /// nested sequence's definition at its call, a recur sequence's body at one
    /// of its iterations. `None` for a coordinate that opens no frame.
    fn frame_sequence(&self, frame: &CfsCoordinates) -> Option<&SequenceDef> {
        if frame.is_empty() {
            return self.cfs.sequences.get(self.entrypoint_coordinate as usize);
        }
        if let Some((site, _)) = self.try_get_recur_iteration_coordinates(frame) {
            return match self.try_get_item_exact(&site)? {
                SequenceChildItem::RecurSequence(item) => {
                    self.cfs.sequences.iter().find(|sequence| sequence.id == item.id)
                }
                _ => None,
            };
        }
        match self.try_get_item_exact(frame)? {
            SequenceChildItem::Sequence(item) => {
                self.cfs.sequences.iter().find(|sequence| sequence.id == item.id)
            }
            _ => None,
        }
    }

    /// Follow a binding to the step that actually wrote its value.
    ///
    /// `frame` is the coordinates of the sequence the binding is written in —
    /// `[]` for `main`, a call's own coordinates for a nested sequence, `[s, i]`
    /// for an iteration of a recur sequence. `path` is the selector already
    /// known to apply on top of the binding (e.g. `SequenceReturn::path`).
    ///
    /// A sequence writes many objects under its coordinate but returns one, and
    /// a sequence body has no control flow, so *which* one is fixed by the CFS
    /// (`SequenceDef::returns`, recorded relative to the sequence — an item
    /// index, never a coordinate — because one definition is called from many
    /// places). The walk descends one call at a time:
    ///
    /// * `PriorItemOutput(k)` names item `F ++ [k + 1]`. A tile, recur tile or
    ///   recur sequence wrote its value exactly there; a nested sequence hands
    ///   on its own `returns`, prepending its path.
    /// * `SequenceScope(i)` is the caller's `i`-th argument at this frame's call
    ///   site. Argument bindings record no path yet, so from here on only a
    ///   *suffix* of the path is known (`path_complete = false`). A parameter of
    ///   a recur sequence body is the site source's element for that iteration,
    ///   which this walk does not follow (`RecurBodyParameter`).
    /// * `EntryArgument` is the entry object at `[]`.
    pub fn resolve_value(
        &self,
        frame: &CfsCoordinates,
        source: &InputBinding,
        path: &[SelectorSegment],
    ) -> Result<ResolvedValue, ResolveError> {
        // The CFS has no recursive sequence calls, so every walk ends; the
        // bound only turns a malformed schema into a refusal instead of a hang.
        const MAX_HOPS: usize = 256;

        let mut frame = frame.clone();
        let mut source = source.clone();
        let mut path = path.to_vec();
        let mut path_complete = true;
        for _ in 0..MAX_HOPS {
            match source {
                InputBinding::PriorItemOutput {
                    intra_sequence_item_index,
                } => {
                    let position = CfsCoordinate::try_from(intra_sequence_item_index)
                        .ok()
                        .and_then(|index| index.checked_add(FIRST_COORDINATE))
                        .ok_or(ResolveError::NotStorage)?;
                    let mut item_coordinates = frame.clone();
                    item_coordinates.push(position);
                    match self.try_get_item(&item_coordinates) {
                        Some(
                            SequenceChildItem::Tile(_)
                            | SequenceChildItem::RecurTile(_)
                            | SequenceChildItem::RecurSequence(_),
                        ) => {
                            return Ok(ResolvedValue {
                                coordinates: item_coordinates,
                                path,
                                path_complete,
                            })
                        }
                        Some(SequenceChildItem::Sequence(item)) => {
                            let returns = self
                                .cfs
                                .sequences
                                .iter()
                                .find(|sequence| sequence.id == item.id)
                                .and_then(|sequence| sequence.returns.as_ref())
                                .ok_or_else(|| ResolveError::UnboundReturn(item.id.clone()))?;
                            let mut composed = returns.path.clone();
                            composed.extend(path);
                            path = composed;
                            source = returns.source.clone();
                            frame = item_coordinates;
                        }
                        None => return Err(ResolveError::NotStorage),
                    }
                }
                InputBinding::SequenceScope { input_index } => {
                    if self.try_get_recur_iteration_coordinates(&frame).is_some() {
                        return Err(ResolveError::RecurBodyParameter);
                    }
                    let call = self.try_get_item(&frame).ok_or(ResolveError::NotStorage)?;
                    let argument = call
                        .inputs()
                        .get(input_index)
                        .cloned()
                        .ok_or(ResolveError::NotStorage)?;
                    let (caller_frame, _) = frame.try_parent().ok_or(ResolveError::NotStorage)?;
                    path_complete = false;
                    source = argument;
                    frame = caller_frame;
                }
                InputBinding::EntryArgument => {
                    return Ok(ResolvedValue {
                        coordinates: CfsCoordinates::new(),
                        path,
                        path_complete,
                    })
                }
                InputBinding::Direct(_) | InputBinding::Indexed { .. } => {
                    return Err(ResolveError::NotStorage)
                }
            }
        }
        Err(ResolveError::TooDeep)
    }

    fn sequence_by_id(&self, id: &str) -> &SequenceDef {
        self.cfs
            .sequences
            .iter()
            .find(|sequence| sequence.id == id)
            .expect("Wrong cfs coordinates")
    }

    fn get_sequence(&self, coords: &CfsCoordinates) -> (&SequenceDef, Option<CfsCoordinate>) {
        // A close marker names the same CFS position its open does; only the
        // trace distinguishes them. Resolve it before walking, or `items.get`
        // would index with a negative coordinate.
        let coords = &coords.opened();
        let mut current_sequence = self
            .cfs
            .sequences
            .get(self.entrypoint_coordinate as usize)
            .expect("Wrong cfs entrypoint coordinates");

        let mut sequence_item_coord: Option<CfsCoordinate> = None;
        let mut depth = 0usize;

        while depth < coords.len() {
            let coord = coords[depth];
            let child_item = current_sequence
                .item_at(coord)
                .expect("Could not resolve sequence coordinates");

            match child_item {
                SequenceChildItem::Sequence(sequence_item) => {
                    current_sequence = self.sequence_by_id(&sequence_item.id);
                    depth += 1;
                }
                SequenceChildItem::Tile(_tile_item) => {
                    if depth + 1 != coords.len() {
                        panic!("Tile coordinates cannot have nested child coordinates");
                    }
                    sequence_item_coord = Some(coord);
                    depth += 1;
                }
                SequenceChildItem::RecurTile(_recur_item) => {
                    if depth + 2 < coords.len() {
                        panic!("RecurTile iteration coordinates can only extend by one index");
                    }
                    // Same split as `RecurSequence`: bare `[s]` is the site
                    // scope, `[s][i]` is one iteration.
                    sequence_item_coord = if depth + 1 == coords.len() {
                        None
                    } else {
                        Some(coord)
                    };
                    depth = coords.len();
                }
                SequenceChildItem::RecurSequence(recur_sequence_item) => {
                    if depth + 1 == coords.len() {
                        // A bare site coordinate is a *scope*, like the nested
                        // `Sequence` arm above: the site brackets its iterations
                        // with a Start/End pair at `[s]`, so its successors are
                        // "descend to iteration 0", "the End back at `[s]`", and
                        // "the following item" — exactly the set the `None` arm
                        // of `try_get_next_coordinates` builds. Returning `Some`
                        // made `[s]` a leaf whose only successor was the next
                        // item, which is what made a site `Start` unorderable.
                        sequence_item_coord = None;
                        depth += 1;
                    } else {
                        if depth + 2 > coords.len() {
                            panic!("RecurSequence coordinates require an iteration index");
                        }
                        current_sequence = self.sequence_by_id(&recur_sequence_item.id);
                        sequence_item_coord = None;
                        depth += 2;
                    }
                }
            }
        }

        (current_sequence, sequence_item_coord)
    }

    pub fn try_get_item(&self, coordinates: &CfsCoordinates) -> Option<&SequenceChildItem> {
        if let Some((site_coordinates, _)) = self.try_get_recur_iteration_coordinates(coordinates) {
            return self.try_get_item_exact(&site_coordinates);
        }
        self.try_get_item_exact(coordinates)
    }

    /// Resolve an item without interpreting the final coordinate as a recur
    /// iteration. Keeping this separate prevents a nested coordinate such as
    /// `[outer_site, iteration, inner_item]` from being misclassified as an
    /// iteration of `[outer_site, iteration]`.
    fn try_get_item_exact(&self, coordinates: &CfsCoordinates) -> Option<&SequenceChildItem> {
        // As in `get_sequence`: a close names the same item its open does.
        let coordinates = &coordinates.opened();
        let mut current_sequence = self.cfs.sequences.get(self.entrypoint_coordinate as usize);
        let mut current_child_item: Option<&SequenceChildItem> = None;

        let mut depth = 0usize;
        while depth < coordinates.len() {
            let coord = coordinates[depth];
            let sequence = current_sequence?;
            let child = sequence.item_at(coord)?;
            current_child_item = Some(child);

            match child {
                SequenceChildItem::Sequence(item) => {
                    current_sequence = self.cfs.sequences.iter().find(|seq| seq.id == item.id);
                    depth += 1;
                }
                SequenceChildItem::RecurSequence(item) => {
                    if depth + 1 == coordinates.len() {
                        current_sequence = None;
                        depth += 1;
                    } else {
                        current_sequence = self.cfs.sequences.iter().find(|seq| seq.id == item.id);
                        current_child_item = None;
                        depth += 2;
                    }
                }
                _ => {
                    current_sequence = None;
                    depth += 1;
                }
            }
        }

        current_child_item
    }

    pub fn get_child_coordinates(
        &self,
        parent_coords: &CfsCoordinates,
        parent_current_index: CfsCoordinate,

        child_id: SequenceChildId,
    ) -> CfsCoordinates {
        if parent_coords.is_empty() && child_id == SequenceChildId::Sequence("main".to_string()) {
            return parent_coords.clone();
        }

        let (parent_sequence, _sequence_item_coord) = self.get_sequence(parent_coords);

        let child_coord = parent_sequence
            .items
            .iter()
            .enumerate()
            .position(|(index, item)| {
                let id = match item {
                    SequenceChildItem::Sequence(sequence_item) => {
                        SequenceChildId::Sequence(sequence_item.id.clone())
                    }
                    SequenceChildItem::Tile(tile_item) => {
                        SequenceChildId::Tile(tile_item.id.clone())
                    }
                    SequenceChildItem::RecurTile(recur_item) => {
                        SequenceChildId::RecurTile(recur_item.id.clone())
                    }
                    SequenceChildItem::RecurSequence(recur_sequence_item) => {
                        SequenceChildId::RecurSequence(recur_sequence_item.id.clone())
                    }
                };

                // `index` is a 0-based Vec position; `parent_current_index` is a
                // 1-based coordinate.
                id == child_id
                    && index >= (parent_current_index - FIRST_COORDINATE).max(0) as usize
            })
            .unwrap_or_else(|| {
                panic!(
                    "Wrong coordinates for sequence child '{:?}[index: {}]': [{} [{:?}] {:?}]",
                    child_id,
                    parent_current_index,
                    parent_sequence.id,
                    parent_coords,
                    parent_sequence
                        .items
                        .iter()
                        .cloned()
                        .map(|item| match item {
                            SequenceChildItem::Sequence(item) => item.id,
                            SequenceChildItem::Tile(item) => item.id,
                            SequenceChildItem::RecurTile(item) => item.id,
                            SequenceChildItem::RecurSequence(item) => item.id,
                        })
                        .collect::<Vec<_>>()
                )
            });

        let mut current_coords = parent_coords.clone();
        current_coords.push(
            CfsCoordinate::try_from(child_coord).expect("Sequence coordinate out of bounds")
                + FIRST_COORDINATE,
        );

        current_coords
    }

    pub fn try_get_recur_iteration_coordinates(
        &self,
        coordinates: &CfsCoordinates,
    ) -> Option<(CfsCoordinates, CfsCoordinate)> {
        // Report the *opening* index for a close, so callers asking "which
        // iteration is this step in" get the same answer at both boundaries.
        let coordinates = &coordinates.opened();
        let (&iteration_index, site_prefix) = coordinates.split_last()?;
        let site_coordinates = CfsCoordinates(site_prefix.to_vec());
        // Both recur kinds address their iterations the same way — `site ++ [i]`
        // — so both decompose here. Accepting only `RecurTile` made a
        // recur-sequence site look like an ordinary item to every caller, which
        // is what kept its iterations out of reach of the completeness rules.
        // `expand_recur_entry_coordinates` just below has always matched both.
        matches!(
            self.try_get_item_exact(&site_coordinates),
            Some(SequenceChildItem::RecurTile(_) | SequenceChildItem::RecurSequence(_))
        )
        .then_some((site_coordinates, iteration_index))
    }

    /// The coordinate at which the scope opened at `coordinates` closes:
    /// `…[-k]` for a nested sequence, a recur site (tile or sequence) and a
    /// recur-sequence iteration.
    ///
    /// `None` for anything that opens no scope — a tile, a recur *tile*
    /// iteration (one `Exec` record, with no boundary steps) — and for `[]`,
    /// whose close is `ProgramEnd` at `[]` itself.
    pub fn closing_coordinates_of(&self, coordinates: &CfsCoordinates) -> Option<CfsCoordinates> {
        let (&last, prefix) = coordinates.split_last()?;
        if is_closing_coordinate(last) {
            return None;
        }
        let opens_scope = match self.try_get_recur_iteration_coordinates(coordinates) {
            Some((site, _)) => matches!(
                self.try_get_item_exact(&site),
                Some(SequenceChildItem::RecurSequence(_))
            ),
            None => matches!(
                self.try_get_item_exact(coordinates),
                Some(
                    SequenceChildItem::Sequence(_)
                        | SequenceChildItem::RecurTile(_)
                        | SequenceChildItem::RecurSequence(_)
                )
            ),
        };
        opens_scope.then(|| {
            let mut closing = prefix.to_vec();
            closing.push(closing_coordinate(last));
            CfsCoordinates(closing)
        })
    }
}

/// The root control flow schema structure for a Raster project.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlFlowSchema {
    /// Schema version for forward compatibility.
    pub version: String,
    /// Project name (from Cargo.toml).
    pub project: String,
    /// Serialization encoding used (e.g., "postcard").
    pub encoding: String,
    /// All tiles defined in the project.
    pub tiles: Vec<TileDef>,
    /// All sequences defined in the project.
    pub sequences: Vec<SequenceDef>,
}

impl ControlFlowSchema {
    /// Create a new CFS with the given project name.
    pub fn new(project: impl Into<String>) -> Self {
        Self {
            version: "1.0".to_string(),
            project: project.into(),
            encoding: "postcard".to_string(),
            tiles: Vec::new(),
            sequences: Vec::new(),
        }
    }
}

/// Definition of a tile in the CFS.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TileDef {
    /// Unique identifier for the tile (function name).
    pub id: String,
    /// Tile type (e.g., "iter" for iterator-style tiles).
    #[serde(rename = "type")]
    pub tile_type: String,
    /// Number of input arguments.
    pub inputs: usize,
    /// Number of output values.
    pub outputs: usize,
}

impl TileDef {
    /// Create a new tile definition with the specified type.
    pub fn new(
        id: impl Into<String>,
        tile_type: impl Into<String>,
        inputs: usize,
        outputs: usize,
    ) -> Self {
        Self {
            id: id.into(),
            tile_type: tile_type.into(),
            inputs,
            outputs,
        }
    }

    /// Create a new tile definition with the default "iter" type.
    pub fn iter(id: impl Into<String>, inputs: usize, outputs: usize) -> Self {
        Self::new(id, "iter", inputs, outputs)
    }
}

pub type SequenceId = String;
pub type TileId = String;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum SequenceChildId {
    Sequence(SequenceId),
    Tile(TileId),
    RecurTile(TileId),
    RecurSequence(SequenceId),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SequenceDef {
    pub id: SequenceId,
    pub input_sources: Vec<InputBinding>,
    pub items: Vec<SequenceChildItem>,
    /// `main`'s declared entry-argument names, in declaration order. Bound
    /// once by the program's `ProgramStart` step into a single authorized
    /// storage object at coordinates `[]`. Empty for every sequence other
    /// than `main`, and for a `main` that declares no external arguments.
    #[serde(default)]
    pub entry_arguments: Vec<String>,
    /// Whether `main` returns a program output (a non-unit value). When set,
    /// the program's `ProgramEnd` step binds and authorizes that output.
    /// Always `false` for sequences other than `main`.
    #[serde(default)]
    pub produces_output: bool,
    /// The value `main` returns: which item's output (or which entry
    /// argument) the program's output is, and which part of it.
    ///
    /// Without it the guest can check that a `ProgramEnd` names *some* stored
    /// value, not the one `main` returns — any intermediate object, or any
    /// field of the right one, would verify as the program's output. `None`
    /// for a unit `main`, for every other sequence (their returns are not bound
    /// yet), and for a `main` whose returned expression the CFS cannot bind (a
    /// finalized draft, a computed value); the guest then refuses the
    /// `ProgramEnd` rather than accept an unchecked output. See
    /// `docs/issues/program-output-unbound.md`.
    #[serde(default)]
    pub returns: Option<SequenceReturn>,
}

/// Where a binding's value was written, from [`CfsCursor::resolve_value`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedValue {
    /// The coordinates of the step that wrote the object.
    pub coordinates: CfsCoordinates,
    /// The selector into that object — all of it when `path_complete`, else
    /// only its trailing segments.
    pub path: Vec<SelectorSegment>,
    /// `false` once the walk crossed a sequence parameter, whose argument
    /// binding records no path.
    pub path_complete: bool,
}

/// Why [`CfsCursor::resolve_value`] could not follow a binding to a written
/// object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    /// A sequence on the chain has no `returns` — its returned expression is
    /// not a binding the CFS can follow (a finalized draft, a computed value).
    UnboundReturn(SequenceId),
    /// The chain reached a parameter of a recur sequence body.
    RecurBodyParameter,
    /// The chain reached an inline value, a data-sourced index, or a
    /// position the CFS does not describe.
    NotStorage,
    /// The walk exceeded its hop bound.
    TooDeep,
}

/// What a sequence returns: where the value comes from, and the path into it.
///
/// `source` is resolved like a step argument. `path` is the selector the
/// returned value is read through, composed the way the runtime composes it:
/// every `select!` along the returned binding's alias chain appends its
/// segments, and an entry argument's path starts with the argument's name,
/// because all of `main`'s arguments live in one entry object at `[]`. Only
/// static segments (`Field`, `Index`, `Range`) occur — a data-sourced index
/// would need its citation carried alongside, so such a return is not bound.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SequenceReturn {
    pub source: InputBinding,
    pub path: Vec<SelectorSegment>,
}

impl SequenceDef {
    /// Create a new sequence definition.
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            input_sources: Vec::new(),
            items: Vec::new(),
            entry_arguments: Vec::new(),
            produces_output: false,
            returns: None,
        }
    }

    /// The item at a **1-based** coordinate.
    ///
    /// `None` for `0`, for a negative coordinate, and past the end — so an
    /// unset or closing coordinate resolves to nothing rather than silently
    /// naming the first item. See [`FIRST_COORDINATE`].
    pub fn item_at(&self, coordinate: CfsCoordinate) -> Option<&SequenceChildItem> {
        let index = usize::try_from(coordinate.checked_sub(FIRST_COORDINATE)?).ok()?;
        self.items.get(index)
    }

    /// The coordinate of this sequence's last item, or `None` when it has none.
    pub fn last_coordinate(&self) -> Option<CfsCoordinate> {
        (!self.items.is_empty()).then(|| self.items.len() as CfsCoordinate)
    }

    pub fn sequences(&self) -> Vec<SequenceItem> {
        self.items
            .iter()
            .filter_map(|item| match item {
                SequenceChildItem::Tile(_) => None,
                SequenceChildItem::RecurTile(_) => None,
                SequenceChildItem::RecurSequence(_) => None,
                SequenceChildItem::Sequence(sequence) => Some(sequence.clone()),
            })
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SequenceChildItem {
    Sequence(SequenceItem),
    Tile(TileItem),
    RecurTile(RecurTileItem),
    RecurSequence(RecurSequenceItem),
}

impl SequenceChildItem {
    pub fn inputs(&self) -> &[InputBinding] {
        match self {
            SequenceChildItem::Sequence(sequence_item) => &sequence_item.sources,
            SequenceChildItem::Tile(tile_item) => &tile_item.sources,
            SequenceChildItem::RecurTile(recur_item) => &recur_item.sources,
            SequenceChildItem::RecurSequence(recur_sequence_item) => &recur_sequence_item.sources,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SequenceItem {
    pub id: SequenceId,
    pub sources: Vec<InputBinding>,
}

impl From<SequenceDef> for SequenceItem {
    fn from(def: SequenceDef) -> Self {
        Self {
            id: def.id,
            sources: def.input_sources,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TileItem {
    pub id: TileId,
    pub sources: Vec<InputBinding>,
}

// Every field below is written unconditionally. `skip_serializing_if` would be
// harmless in the CFS's JSON form, but this struct is also postcard-encoded as
// part of `ProgramDefinition::canonical_bytes` — the transition guest's
// verification frame — and postcard is positional and non-self-describing. A
// skipped field is simply absent from the stream while `Deserialize` still
// reads one at that offset, so every later field shifts and the frame fails to
// decode. `#[serde(default)]` stays: it costs nothing here and keeps older
// JSON readable.

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecurTileItem {
    pub id: TileId,
    pub sources: Vec<InputBinding>,
    /// Static chunk size from `call_recur! { ..., chunk = N }`: each iteration
    /// consumes a contiguous group of N source elements (the final group may be
    /// shorter). `None` means per-element iteration.
    #[serde(default)]
    pub chunk: Option<u64>,
    /// Whether this recur deliberately returns its output draft without
    /// finalizing it. False is the historical/default behavior.
    #[serde(default)]
    pub leaves_output_open: bool,
    /// Whether the site's own output *is* its carried state — `state` with no
    /// `output`.
    ///
    /// This is what pins a sweep's **final** carried state. Every earlier one is
    /// pinned by the next iteration's bound `state_in`; the last has no
    /// successor, so the only thing it can be compared against is the value the
    /// site returned. That comparison is only meaningful for this shape: a
    /// state+output site discards its state and returns the draft.
    #[serde(default)]
    pub state_is_output: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecurSequenceItem {
    pub id: SequenceId,
    pub sources: Vec<InputBinding>,
    /// See [`RecurTileItem::state_is_output`]. A recur sequence needs it more
    /// than a recur tile does: a tile's last iteration is replay-proven, so its
    /// final state is anchored regardless, while a sequence's is not.
    #[serde(default)]
    pub state_is_output: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum InputBinding {
    Direct(InputSource),
    SequenceScope {
        input_index: usize,
    },
    PriorItemOutput {
        intra_sequence_item_index: usize,
    },
    /// One of `main`'s entry arguments, reached from the single authorized
    /// entry object at the sequence root (coordinates `[]`) that the
    /// `ProgramStart` step bound. `main` has no caller, so its arguments are
    /// not `SequenceScope`; and the entry object sits at the sequence root
    /// itself, which no `PriorItemOutput` index can name.
    EntryArgument,
    /// A value whose selector reaches it through one or more **data-sourced**
    /// list indexes (`select!(Row, rows[token_id])`).
    ///
    /// Composite rather than a peer of the variants above, because index
    /// provenance is orthogonal to value provenance: the value still comes from
    /// an entry argument, a prior item, or the caller's scope, and `value`
    /// records which. `indexes` records where each index came from, in selector
    /// order, so nesting (`a.rows[i].cells[j]`) is expressible.
    ///
    /// This is what makes "reads the element named by binding X" and "reads
    /// element 7" different *programs*: the schema is hashed into program
    /// identity, so the two cannot be swapped for one another behind a fixed
    /// identity. Verification of the index values themselves is separate and
    /// lives in [`crate::trace::verify_bound_index_bindings`]. See
    /// `docs/proposals/dynamic-index-selection.md` §5.
    ///
    /// Declared last so the postcard variant indices of the four above — which
    /// existing committed schemas encode — do not shift.
    Indexed {
        value: Box<InputBinding>,
        indexes: Vec<InputBinding>,
    },
}

impl InputBinding {
    /// Create a binding from a direct semantic source.
    pub fn new(source: InputSource) -> Self {
        Self::Direct(source)
    }

    /// Create an inline input binding.
    pub fn inline() -> Self {
        Self::new(InputSource::Inline)
    }

    /// Create a direct storage input binding.
    pub fn storage() -> Self {
        Self::new(InputSource::Storage)
    }

    /// Create a sequence-scope binding.
    pub fn seq_input(input_index: usize) -> Self {
        Self::SequenceScope { input_index }
    }

    /// Create a binding sourced from a prior item's committed output.
    pub fn prior_item_output(intra_sequence_item_index: usize) -> Self {
        Self::PriorItemOutput {
            intra_sequence_item_index,
        }
    }

    /// Create a binding to one of `main`'s entry arguments.
    pub fn entry_argument() -> Self {
        Self::EntryArgument
    }

    /// Wrap this binding as one reached through data-sourced list indexes.
    ///
    /// Returns `self` unchanged when `indexes` is empty, so a literal-index
    /// selection keeps emitting exactly the binding it emits today — which is
    /// what makes this phase a no-op for every existing program's identity.
    pub fn indexed_by(self, indexes: Vec<InputBinding>) -> Self {
        if indexes.is_empty() {
            return self;
        }
        Self::Indexed {
            value: Box::new(self),
            indexes,
        }
    }

    /// The underlying value binding, looking through any index wrapper.
    pub fn value_binding(&self) -> &InputBinding {
        match self {
            Self::Indexed { value, .. } => value.value_binding(),
            other => other,
        }
    }

    /// The index bindings this one declares, in selector order.
    pub fn index_bindings(&self) -> &[InputBinding] {
        match self {
            Self::Indexed { indexes, .. } => indexes.as_slice(),
            _ => &[],
        }
    }
}

/// Semantic source of an input value in the data flow schema.
///
/// Note there is no "external" source: data entering a program does so as
/// `main`'s entry arguments, which are loaded once into storage by the
/// program's `ProgramStart` step and reached from there through
/// `InputBinding::EntryArgument`. A source here is only about values with no
/// upstream item to bind to.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum InputSource {
    /// Input is materialized inline in the sequence body.
    Inline,

    /// Input is resolved from storage.
    Storage,
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn recur_cursor() -> CfsCursor {
        CfsCursor::new(ControlFlowSchema {
            version: "1.0".to_string(),
            project: "test".to_string(),
            encoding: "postcard".to_string(),
            tiles: vec![
                TileDef::iter("before", 0, 0),
                TileDef::iter("recur", 0, 0),
                TileDef::iter("after", 0, 0),
            ],
            sequences: vec![SequenceDef {
                id: "main".to_string(),
                input_sources: vec![],
                items: vec![
                    SequenceChildItem::Tile(TileItem {
                        id: "before".to_string(),
                        sources: vec![],
                    }),
                    SequenceChildItem::RecurTile(RecurTileItem {
                        id: "recur".to_string(),
                        sources: vec![],
                        chunk: None,
                        leaves_output_open: false,
                        state_is_output: false,
                    }),
                    SequenceChildItem::Tile(TileItem {
                        id: "after".to_string(),
                        sources: vec![],
                    }),
                ],
                entry_arguments: vec![],
                produces_output: false,
                returns: None,
            }],
        })
    }

    /// A site is entered through its `RecurStart` at `[s]` only — never
    /// straight into an iteration.
    #[test]
    fn a_recur_site_is_entered_through_its_start() {
        let cursor = recur_cursor();
        let next = cursor
            .try_get_next_coordinates(&CfsCoordinates(vec![1]))
            .expect("next coordinates should exist");

        assert_eq!(next, vec![CfsCoordinates(vec![2])]);
    }

    #[test]
    fn a_recur_tile_iteration_advances_or_closes_the_site() {
        let cursor = recur_cursor();
        let next = cursor
            .try_get_next_coordinates(&CfsCoordinates(vec![2, 1]))
            .expect("next coordinates should exist");

        assert_eq!(
            next,
            vec![CfsCoordinates(vec![2, 2]), CfsCoordinates(vec![-2])]
        );
        assert_eq!(
            cursor
                .try_get_item(&CfsCoordinates(vec![2, 5]))
                .map(|item| matches!(item, SequenceChildItem::RecurTile(_))),
            Some(true)
        );
    }



    /// A site's `RecurStart` at `[2]` is followed by its first iteration or,
    /// for an empty source, its close at `[-2]` — never the next sibling, so
    /// a site cannot be left without its `RecurEnd`. The close is followed by
    /// the next sibling only.
    ///
    /// This used to be the union `{[2], [2][1], [3]}`: with `Start` and `End`
    /// sharing `[2]`, the relation keyed on the coordinate could not tell
    /// them apart.
    #[test]
    fn a_recur_site_start_offers_its_first_iteration_or_its_close() {
        let cursor = recur_cursor();
        assert_eq!(
            cursor.try_get_next_coordinates(&CfsCoordinates(vec![2])),
            Some(vec![CfsCoordinates(vec![2, 1]), CfsCoordinates(vec![-2])]),
        );
        assert_eq!(
            cursor.try_get_next_coordinates(&CfsCoordinates(vec![-2])),
            Some(vec![CfsCoordinates(vec![3])]),
        );
    }

    /// A recur *sequence* iteration is a scope with children, so its successor
    /// set must include the first step **inside** it.
    ///
    /// Regression test. `try_get_next_coordinates` early-returns
    /// `{site ++ [i+1], site}` for any coordinate that decomposes as a recur
    /// iteration — a set written for the recur-*tile* shape, where an iteration
    /// is a single leaf `Exec`. A recur sequence's iteration has children, so
    /// that set excludes the only step that can legally follow it.
    fn recur_sequence_cursor() -> CfsCursor {
        CfsCursor::new(ControlFlowSchema {
            version: "1.0".to_string(),
            project: "test".to_string(),
            encoding: "postcard".to_string(),
            tiles: vec![
                TileDef::iter("inner", 0, 0),
                TileDef::iter("chunked", 0, 0),
                TileDef::iter("after", 0, 0),
            ],
            sequences: vec![
                SequenceDef {
                    id: "main".to_string(),
                    input_sources: vec![],
                    items: vec![
                        SequenceChildItem::RecurSequence(RecurSequenceItem {
                            id: "body".to_string(),
                            sources: vec![],
                            state_is_output: false,
                        }),
                        SequenceChildItem::Tile(TileItem {
                            id: "after".to_string(),
                            sources: vec![],
                        }),
                    ],
                    entry_arguments: vec![],
                    produces_output: false,
                    returns: None,
                },
                SequenceDef {
                    id: "body".to_string(),
                    input_sources: vec![],
                    items: vec![
                        SequenceChildItem::Tile(TileItem {
                            id: "inner".to_string(),
                            sources: vec![],
                        }),
                        SequenceChildItem::RecurTile(RecurTileItem {
                            id: "chunked".to_string(),
                            sources: vec![],
                            chunk: Some(64),
                            leaves_output_open: false,
                            state_is_output: false,
                        }),
                    ],
                    entry_arguments: vec![],
                    produces_output: false,
                    returns: None,
                },
            ],
        })
    }

    #[test]
    fn recur_sequence_iteration_offers_its_first_inner_step() {
        let cursor = recur_sequence_cursor();
        let next = cursor
            .try_get_next_coordinates(&CfsCoordinates(vec![1, 1]))
            .expect("next coordinates should exist");

        assert!(
            next.contains(&CfsCoordinates(vec![1, 1, 1])),
            "iteration [1][1] must be able to be followed by its own first step \
             [1][1][1], got {:?}",
            next,
        );
    }

    #[test]
    fn a_recur_sequence_iteration_closes_at_its_own_coordinate() {
        let cursor = recur_sequence_cursor();
        let open = CfsCoordinates(vec![1, 1]);

        let close = cursor
            .closing_coordinates_of(&open)
            .expect("a recur sequence iteration has a distinct close");
        // The bracket is symmetric: open `1` closes at `-1`. That symmetry is
        // what 1-basing buys — at 0-based, `-0 == 0` would fold the first
        // iteration's close back onto its own open.
        assert_eq!(close, CfsCoordinates(vec![1, -1]));
        assert_eq!(close, CfsCoordinates(vec![1, closing_coordinate(1)]));
        assert_ne!(close, open);

        // An iteration with a body runs it before closing: the open offers
        // the first inner step only, and the body's last step offers the close.
        assert_eq!(
            cursor.try_get_next_coordinates(&open),
            Some(vec![CfsCoordinates(vec![1, 1, 1])]),
        );
        let last_inner_close = CfsCoordinates(vec![1, 1, closing_coordinate(2)]);
        assert_eq!(
            cursor.try_get_next_coordinates(&last_inner_close),
            Some(vec![close]),
        );
    }

    #[test]
    fn a_closed_recur_sequence_iteration_is_followed_by_the_next_one() {
        // The transition an honest sweep makes and the guest rejected:
        // iteration 1 ends, iteration 2 begins.
        let cursor = recur_sequence_cursor();
        let close_of_2 = CfsCoordinates(vec![1, closing_coordinate(2)]);

        let next = cursor
            .try_get_next_coordinates(&close_of_2)
            .expect("a closed iteration has successors");

        assert!(
            next.contains(&CfsCoordinates(vec![1, 3])),
            "a closed iteration must be followed by the next one, got {:?}",
            next,
        );
        assert!(
            next.contains(&CfsCoordinates(vec![-1])),
            "or by the site closing at its own coordinate, got {:?}",
            next,
        );
        assert!(
            !next.contains(&CfsCoordinates(vec![1, 2, 1])),
            "a closed iteration must not offer its own body again: {:?}",
            next,
        );
    }

    /// `main = [a, child, d]`, `child = [b, c]` — the shape the D4 probe
    /// measured (`incremental-draft-materialization.md` §What the move to
    /// `[-s]` forces).
    fn nested_sequence_cursor() -> CfsCursor {
        let tile = |id: &str| {
            SequenceChildItem::Tile(TileItem {
                id: id.to_string(),
                sources: vec![],
            })
        };
        let sequence = |id: &str, items| SequenceDef {
            id: id.to_string(),
            input_sources: vec![],
            items,
            entry_arguments: vec![],
            produces_output: false,
            returns: None,
        };
        CfsCursor::new(ControlFlowSchema {
            version: "1.0".to_string(),
            project: "test".to_string(),
            encoding: "postcard".to_string(),
            tiles: ["a", "b", "c", "d"]
                .iter()
                .map(|id| TileDef::iter(*id, 0, 0))
                .collect(),
            sequences: vec![
                sequence(
                    "main",
                    vec![
                        tile("a"),
                        SequenceChildItem::Sequence(SequenceItem {
                            id: "child".to_string(),
                            sources: vec![],
                        }),
                        tile("d"),
                    ],
                ),
                sequence("child", vec![tile("b"), tile("c")]),
            ],
        })
    }

    /// Measured before the move: after the child's `SequenceStart` at `[2]`,
    /// the next sibling `[3]` was accepted (skipping the `End`); after its last
    /// step `[2,2]`, `[2,1]` was (restarting the body). Now every set is exact.
    #[test]
    fn a_nested_sequence_runs_its_body_once_and_closes_at_its_own_coordinate() {
        let cursor = nested_sequence_cursor();
        let next = |coordinates: Vec<CfsCoordinate>| {
            cursor.try_get_next_coordinates(&CfsCoordinates(coordinates))
        };
        assert_eq!(next(vec![1]), Some(vec![CfsCoordinates(vec![2])]));
        assert_eq!(next(vec![2]), Some(vec![CfsCoordinates(vec![2, 1])]));
        assert_eq!(next(vec![2, 1]), Some(vec![CfsCoordinates(vec![2, 2])]));
        assert_eq!(next(vec![2, 2]), Some(vec![CfsCoordinates(vec![-2])]));
        assert_eq!(next(vec![-2]), Some(vec![CfsCoordinates(vec![3])]));
        assert_eq!(
            cursor.closing_coordinates_of(&CfsCoordinates(vec![2])),
            Some(CfsCoordinates(vec![-2])),
        );
        assert_eq!(cursor.closing_coordinates_of(&CfsCoordinates(vec![1])), None);
    }

    /// `main`'s scope opens and closes at `[]`: `ProgramStart` is followed by
    /// its first item, and only its last item is followed by `ProgramEnd`.
    /// The old union also offered `[]` straight after `ProgramStart`.
    #[test]
    fn main_runs_its_body_before_program_end() {
        let cursor = nested_sequence_cursor();
        assert_eq!(
            cursor.try_get_next_coordinates(&CfsCoordinates(vec![])),
            Some(vec![CfsCoordinates(vec![1])]),
        );
        assert_eq!(
            cursor.try_get_next_coordinates(&CfsCoordinates(vec![3])),
            Some(vec![CfsCoordinates(vec![])]),
        );
    }

    #[test]
    fn a_draft_namespace_coordinate_is_not_read_as_a_close() {
        // `DRAFT_NAMESPACE` is negative but is not a close marker, and negating
        // `i32::MIN` would overflow.
        assert!(opening_coordinate(DRAFT_NAMESPACE).is_none());
        assert!(!is_closing_coordinate(DRAFT_NAMESPACE));
        assert!(is_closing_coordinate(closing_coordinate(FIRST_COORDINATE)));
        assert_eq!(opening_coordinate(closing_coordinate(7)), Some(7));
        // `0` is not a position at all, so it is not a close either.
        assert!(!is_closing_coordinate(0));
    }

    #[test]
    fn nested_recur_tile_resolves_as_inner_item_not_outer_iteration() {
        let cursor = recur_sequence_cursor();
        let coordinates = CfsCoordinates(vec![1, 1, 2]);
        let item = cursor
            .try_get_item(&coordinates)
            .expect("nested recur tile should resolve");
        let SequenceChildItem::RecurTile(item) = item else {
            panic!("expected nested recur tile, got {item:?}");
        };
        assert_eq!(item.chunk, Some(64));
    }

    /// `main = [a, outer, outer, choose, rs, noret]`, where `outer` returns
    /// `select!(inner_result.x)`, `inner` returns `select!(c.y)`, `choose`
    /// returns `select!(its parameter.z)`, and `rs` is a recur sequence whose
    /// body calls `choose` on the body's own parameter.
    fn returns_cursor() -> CfsCursor {
        let tile = |id: &str, sources: Vec<InputBinding>| {
            SequenceChildItem::Tile(TileItem { id: id.to_string(), sources })
        };
        let call = |id: &str, sources: Vec<InputBinding>| {
            SequenceChildItem::Sequence(SequenceItem { id: id.to_string(), sources })
        };
        let returning = |index: usize, field: &str| {
            Some(SequenceReturn {
                source: InputBinding::prior_item_output(index),
                path: vec![SelectorSegment::Field(field.to_string())],
            })
        };
        let seq = |id: &str, items: Vec<SequenceChildItem>, returns: Option<SequenceReturn>| {
            SequenceDef {
                id: id.to_string(),
                input_sources: vec![InputBinding::seq_input(0)],
                items,
                entry_arguments: vec![],
                produces_output: false,
                returns,
            }
        };
        CfsCursor::new(ControlFlowSchema {
            version: "1.0".to_string(),
            project: "test".to_string(),
            encoding: "postcard".to_string(),
            tiles: vec![TileDef::iter("a", 0, 1), TileDef::iter("b", 1, 1), TileDef::iter("c", 1, 1)],
            sequences: vec![
                SequenceDef {
                    id: "main".to_string(),
                    input_sources: vec![],
                    items: vec![
                        tile("a", vec![]),
                        call("outer", vec![InputBinding::prior_item_output(0)]),
                        call("outer", vec![InputBinding::prior_item_output(0)]),
                        call("choose", vec![InputBinding::prior_item_output(0)]),
                        SequenceChildItem::RecurSequence(RecurSequenceItem {
                            id: "rs".to_string(),
                            sources: vec![InputBinding::prior_item_output(0)],
                            state_is_output: false,
                        }),
                        call("noret", vec![InputBinding::prior_item_output(0)]),
                    ],
                    entry_arguments: vec![],
                    produces_output: true,
                    returns: None,
                },
                seq(
                    "outer",
                    vec![tile("b", vec![InputBinding::seq_input(0)]), call("inner", vec![InputBinding::prior_item_output(0)])],
                    returning(1, "x"),
                ),
                seq("inner", vec![tile("c", vec![InputBinding::seq_input(0)])], returning(0, "y")),
                seq(
                    "choose",
                    vec![],
                    Some(SequenceReturn {
                        source: InputBinding::seq_input(0),
                        path: vec![SelectorSegment::Field("z".to_string())],
                    }),
                ),
                seq("rs", vec![call("choose", vec![InputBinding::seq_input(0)])], returning(0, "w")),
                seq("noret", vec![tile("b", vec![InputBinding::seq_input(0)])], None),
            ],
        })
    }

    fn fields(names: &[&str]) -> Vec<SelectorSegment> {
        names.iter().map(|name| SelectorSegment::Field(name.to_string())).collect()
    }

    #[test]
    fn a_nested_return_resolves_to_the_step_that_wrote_it() {
        let cursor = returns_cursor();
        let resolved = cursor
            .resolve_value(&CfsCoordinates::new(), &InputBinding::prior_item_output(1), &fields(&["m"]))
            .expect("resolves");
        // main → outer at [2] → inner at [2,2] → tile c at [2,2,1]; each hop
        // prepends its own selection: c.y, then .x, then main's .m.
        assert_eq!(resolved.coordinates, CfsCoordinates(vec![2, 2, 1]));
        assert_eq!(resolved.path, fields(&["y", "x", "m"]));
        assert!(resolved.path_complete);
    }

    /// One definition, two call sites: the relative `returns` resolves against
    /// whichever call it is reached through.
    #[test]
    fn the_same_definition_resolves_per_call_site() {
        let cursor = returns_cursor();
        let second = cursor
            .resolve_value(&CfsCoordinates::new(), &InputBinding::prior_item_output(2), &[])
            .expect("resolves");
        assert_eq!(second.coordinates, CfsCoordinates(vec![3, 2, 1]));
    }

    /// A sequence returning its parameter hands the value back to the caller's
    /// argument — which here is `a`'s output at `[1]`, outside the call at
    /// `[4]`. The argument binding records no path, so only a suffix is known.
    #[test]
    fn a_returned_parameter_resolves_through_the_callers_argument() {
        let cursor = returns_cursor();
        let resolved = cursor
            .resolve_value(&CfsCoordinates::new(), &InputBinding::prior_item_output(3), &[])
            .expect("resolves");
        assert_eq!(resolved.coordinates, CfsCoordinates(vec![1]));
        assert_eq!(resolved.path, fields(&["z"]));
        assert!(!resolved.path_complete);
    }

    #[test]
    fn a_recur_sequence_site_is_its_own_leaf() {
        let cursor = returns_cursor();
        let resolved = cursor
            .resolve_value(&CfsCoordinates::new(), &InputBinding::prior_item_output(4), &[])
            .expect("resolves");
        assert_eq!(resolved.coordinates, CfsCoordinates(vec![5]));
    }

    #[test]
    fn a_recur_body_parameter_is_not_followed() {
        let cursor = returns_cursor();
        // Inside iteration 1 of `rs` at [5], `choose` (item [5,1,1]) returns the
        // body's own parameter.
        assert_eq!(
            cursor.resolve_value(&CfsCoordinates(vec![5, 1]), &InputBinding::prior_item_output(0), &[]),
            Err(ResolveError::RecurBodyParameter)
        );
    }

    #[test]
    fn an_unbound_return_and_an_inline_value_are_refused() {
        let cursor = returns_cursor();
        assert_eq!(
            cursor.resolve_value(&CfsCoordinates::new(), &InputBinding::prior_item_output(5), &[]),
            Err(ResolveError::UnboundReturn("noret".to_string()))
        );
        assert_eq!(
            cursor.resolve_value(&CfsCoordinates::new(), &InputBinding::inline(), &[]),
            Err(ResolveError::NotStorage)
        );
        assert_eq!(
            cursor
                .resolve_value(&CfsCoordinates::new(), &InputBinding::entry_argument(), &fields(&["cfg"]))
                .map(|resolved| (resolved.coordinates, resolved.path)),
            Ok((CfsCoordinates::new(), fields(&["cfg"])))
        );
    }

}
