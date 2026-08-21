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

pub type CfsCoordinate = u32;

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
        let entrypoint_coordinate: u32 = cfs
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

    pub fn is_next_coordinates(&mut self, next_coordinates: &CfsCoordinates) -> bool {
        if let Some(next_coordinates_options) = self.try_get_next_coordinates(&self.coordinates()) {
            return next_coordinates_options.contains(next_coordinates);
        }

        false
    }

    pub fn try_get_next_coordinates(
        &self,
        coordinates: &CfsCoordinates,
    ) -> Option<Vec<CfsCoordinates>> {
        if let Some((site_coordinates, iteration_index)) =
            self.try_get_recur_iteration_coordinates(coordinates)
        {
            // Only a recur *tile* iteration is a leaf: it is one `Exec` record
            // at `[s][i]`, so its successors really are "the next iteration, or
            // the site". A recur *sequence* iteration is a scope whose own
            // steps live at `[s][i][j]`, so it must fall through to the descend
            // path below — otherwise the first step inside an iteration is not
            // an accepted successor and `get_next_expected_coordinates` rejects
            // it. Covered by `recur_sequence_iteration_offers_its_first_inner_step`.
            if matches!(
                self.try_get_item(&site_coordinates),
                Some(SequenceChildItem::RecurTile(_))
            ) {
                let mut next_iteration_coordinates = site_coordinates.clone();
                next_iteration_coordinates.push(iteration_index + 1);

                return Some(Vec::from([next_iteration_coordinates, site_coordinates]));
            }
        }

        let mut current_coordinates = coordinates.clone();
        loop {
            let (current_sequence, current_item_coordinate) =
                self.get_sequence(&current_coordinates);
            let sequence_last_coordinate = current_sequence.items.len() - 1;

            // if the item is not the sequence itself we can try find next item within that
            // sequence (last coordinate, is a sequence coordinate)
            match current_item_coordinate {
                Some(current_item_coordinate) => {
                    let next_item_coordinate = current_item_coordinate + 1;

                    if current_sequence
                        .items
                        .get(next_item_coordinate as usize)
                        .is_some()
                    {
                        let mut next_coordinates = current_coordinates.clone();

                        match next_coordinates.last_mut() {
                            Some(coordinate) => {
                                *coordinate = next_item_coordinate;
                            }
                            // TODO: is this actually possible?
                            None => {
                                return None;
                            }
                        }

                        return Some(self.expand_recur_entry_coordinates(next_coordinates));
                    } else if current_item_coordinate as usize == sequence_last_coordinate {
                        let (current_sequence_coordinate, parent_sequence_coordinates) =
                            current_coordinates.split_last().expect("Empty coordinates");
                        let parent_sequence_coordinates =
                            CfsCoordinates(parent_sequence_coordinates.to_vec());

                        let (parent_sequence, _) = self.get_sequence(&parent_sequence_coordinates);

                        if let Some(_next_item) = parent_sequence
                            .items
                            .get((*current_sequence_coordinate + 1) as usize)
                        {
                            let mut next_coordinates = parent_sequence_coordinates.clone();
                            next_coordinates.push(*current_sequence_coordinate + 1);

                            if self.try_get_item(&next_coordinates).is_some() {
                                return Some(self.expand_recur_entry_coordinates(next_coordinates));
                            }

                            return None;
                        }

                        if parent_sequence_coordinates.is_empty() {
                            return None;
                        }
                        current_coordinates = parent_sequence_coordinates;
                    } else {
                        return None;
                    }
                }
                None => {
                    let mut next_coordinates_options: Vec<CfsCoordinates> = Vec::new();

                    next_coordinates_options.push(current_coordinates.clone());

                    let mut next_coordinates = current_coordinates.clone();

                    next_coordinates.push(0);
                    if self.try_get_item(&next_coordinates).is_some() {
                        next_coordinates_options
                            .extend(self.expand_recur_entry_coordinates(next_coordinates));
                    }

                    let Some((current_sequence_coordinate, parent_sequence_coordinates)) =
                        current_coordinates.split_last()
                    else {
                        // Entrypoint start
                        next_coordinates_options.push(current_coordinates.clone());

                        return Some(next_coordinates_options);
                    };

                    let parent_sequence_coordinates =
                        CfsCoordinates(parent_sequence_coordinates.to_vec());
                    let (parent_sequence, _) = self.get_sequence(&parent_sequence_coordinates);

                    if let Some(_next_item) = parent_sequence
                        .items
                        .get((*current_sequence_coordinate + 1) as usize)
                    {
                        let mut next_coordinates = parent_sequence_coordinates.clone();
                        next_coordinates.push(*current_sequence_coordinate + 1);

                        if self.try_get_item(&next_coordinates).is_some() {
                            next_coordinates_options
                                .extend(self.expand_recur_entry_coordinates(next_coordinates));
                        }
                    } else {
                        let mut next_coordinates = current_coordinates.clone();
                        next_coordinates.pop();
                        next_coordinates_options.push(next_coordinates);
                    }

                    return Some(next_coordinates_options);
                }
            }
        }
    }

    fn sequence_by_id(&self, id: &str) -> &SequenceDef {
        self.cfs
            .sequences
            .iter()
            .find(|sequence| sequence.id == id)
            .expect("Wrong cfs coordinates")
    }

    fn get_sequence(&self, coords: &CfsCoordinates) -> (&SequenceDef, Option<CfsCoordinate>) {
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
                .items
                .get(coord as usize)
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
            return self.try_get_item(&site_coordinates);
        }

        let mut current_sequence = self.cfs.sequences.get(self.entrypoint_coordinate as usize);
        let mut current_child_item: Option<&SequenceChildItem> = None;

        let mut depth = 0usize;
        while depth < coordinates.len() {
            let coord = coordinates[depth];
            let sequence = current_sequence?;
            let child = sequence.items.get(coord as usize)?;
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

                id == child_id && index >= parent_current_index as usize
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
            child_coord
                .try_into()
                .expect("Sequence coordinate out ouf bound u8"),
        );

        current_coords
    }

    pub fn try_get_recur_iteration_coordinates(
        &self,
        coordinates: &CfsCoordinates,
    ) -> Option<(CfsCoordinates, CfsCoordinate)> {
        let (&iteration_index, site_prefix) = coordinates.split_last()?;
        let site_coordinates = CfsCoordinates(site_prefix.to_vec());
        // Both recur kinds address their iterations the same way — `site ++ [i]`
        // — so both decompose here. Accepting only `RecurTile` made a
        // recur-sequence site look like an ordinary item to every caller, which
        // is what kept its iterations out of reach of the completeness rules.
        // `expand_recur_entry_coordinates` just below has always matched both.
        matches!(
            self.try_get_item(&site_coordinates),
            Some(SequenceChildItem::RecurTile(_) | SequenceChildItem::RecurSequence(_))
        )
        .then_some((site_coordinates, iteration_index))
    }

    fn expand_recur_entry_coordinates(&self, coordinates: CfsCoordinates) -> Vec<CfsCoordinates> {
        if matches!(
            self.try_get_item(&coordinates),
            Some(SequenceChildItem::RecurTile(_) | SequenceChildItem::RecurSequence(_))
        ) {
            let mut iteration_coordinates = coordinates.clone();
            iteration_coordinates.push(0);
            Vec::from([coordinates, iteration_coordinates])
        } else {
            Vec::from([coordinates])
        }
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
        }
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecurTileItem {
    pub id: TileId,
    pub sources: Vec<InputBinding>,
    /// Static chunk size from `call_recur! { ..., chunk = N }`: each iteration
    /// consumes a contiguous group of N source elements (the final group may be
    /// shorter). `None` means per-element iteration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunk: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecurSequenceItem {
    pub id: SequenceId,
    pub sources: Vec<InputBinding>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
                    }),
                    SequenceChildItem::Tile(TileItem {
                        id: "after".to_string(),
                        sources: vec![],
                    }),
                ],
                entry_arguments: vec![],
                produces_output: false,
            }],
        })
    }

    #[test]
    fn recur_site_entry_offers_site_and_first_iteration_coordinates() {
        let cursor = recur_cursor();
        let next = cursor
            .try_get_next_coordinates(&CfsCoordinates(vec![0]))
            .expect("next coordinates should exist");

        assert_eq!(
            next,
            vec![CfsCoordinates(vec![1]), CfsCoordinates(vec![1, 0])]
        );
    }

    #[test]
    fn recur_iteration_advances_or_returns_to_site() {
        let cursor = recur_cursor();
        let next = cursor
            .try_get_next_coordinates(&CfsCoordinates(vec![1, 0]))
            .expect("next coordinates should exist");

        assert_eq!(
            next,
            vec![CfsCoordinates(vec![1, 1]), CfsCoordinates(vec![1])]
        );
        assert_eq!(
            cursor
                .try_get_item(&CfsCoordinates(vec![1, 4]))
                .map(|item| matches!(item, SequenceChildItem::RecurTile(_))),
            Some(true)
        );
    }



    /// A recur site coordinate is a *scope*, so its successor set covers both
    /// halves of the site: the `End` back at `[1]`, iteration 0 at `[1][0]`
    /// (and that iteration's first inner step), and the next sibling `[2]`.
    ///
    /// This used to assert `[2]` alone, from when `[s]` was a leaf item with a
    /// single trailing event. With a `Start`/`End` pair both at `[s]`, the
    /// successor genuinely depends on which half the record is — and since the
    /// relation is keyed on the coordinate, the set must be the union.
    ///
    /// That is not a new weakening: a nested `Sequence` coordinate already
    /// yields exactly this shape (`{[s], [s][0], [s+1]}`), for the same reason.
    /// The ordering check bounds the *shape*; the count is bounded by the recur
    /// progress rules, where `close_site` has popped the frame so a stray
    /// iteration after the site fails with `NoActiveSite`.
    #[test]
    fn recur_site_coordinate_offers_both_halves_and_the_next_sibling() {
        let cursor = recur_cursor();
        let next = cursor
            .try_get_next_coordinates(&CfsCoordinates(vec![1]))
            .expect("next coordinates should exist");

        assert!(next.contains(&CfsCoordinates(vec![1])), "the End half: {:?}", next);
        assert!(next.contains(&CfsCoordinates(vec![1, 0])), "iteration 0: {:?}", next);
        assert!(next.contains(&CfsCoordinates(vec![2])), "next sibling: {:?}", next);
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
            tiles: vec![TileDef::iter("inner", 0, 0), TileDef::iter("after", 0, 0)],
            sequences: vec![
                SequenceDef {
                    id: "main".to_string(),
                    input_sources: vec![],
                    items: vec![
                        SequenceChildItem::RecurSequence(RecurSequenceItem {
                            id: "body".to_string(),
                            sources: vec![],
                        }),
                        SequenceChildItem::Tile(TileItem {
                            id: "after".to_string(),
                            sources: vec![],
                        }),
                    ],
                    entry_arguments: vec![],
                    produces_output: false,
                },
                SequenceDef {
                    id: "body".to_string(),
                    input_sources: vec![],
                    items: vec![SequenceChildItem::Tile(TileItem {
                        id: "inner".to_string(),
                        sources: vec![],
                    })],
                    entry_arguments: vec![],
                    produces_output: false,
                },
            ],
        })
    }

    #[test]
    fn recur_sequence_iteration_offers_its_first_inner_step() {
        let cursor = recur_sequence_cursor();
        let next = cursor
            .try_get_next_coordinates(&CfsCoordinates(vec![0, 0]))
            .expect("next coordinates should exist");

        assert!(
            next.contains(&CfsCoordinates(vec![0, 0, 0])),
            "iteration [0][0] must be able to be followed by its own first step \
             [0][0][0], got {:?}",
            next,
        );
    }
}
