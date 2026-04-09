//! Control Flow Schema (CFS) types.
//!
//! This module defines the data structures for representing the control flow
//! and data flow of a Raster application. The CFS captures:
//! - All tiles and their input/output arities
//! - All sequences and their item composition
//! - Data flow bindings between tiles, sequences, and external inputs

use serde::{Deserialize, Serialize};
use std::ops::{Deref, DerefMut};
use std::string::{String, ToString};
use std::vec::Vec;

pub type CfsCoordinate = u32;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
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

                        return Some(Vec::from([next_coordinates]));
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
                                return Some(Vec::from([next_coordinates]));
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
                        next_coordinates_options.push(next_coordinates);
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
                            next_coordinates_options.push(next_coordinates);
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

    fn get_sequence(&self, coords: &CfsCoordinates) -> (&SequenceDef, Option<CfsCoordinate>) {
        let mut current_sequence = self
            .cfs
            .sequences
            .get(self.entrypoint_coordinate as usize)
            .expect("Wrong cfs entrypoint coordinates");

        let mut sequence_item_coord: Option<CfsCoordinate> = None;

        for &coord in coords.iter() {
            let child_item = current_sequence
                .items
                .get(coord as usize)
                .expect("Could not resolve sequence coordinates");

            match child_item {
                SequenceChildItem::Sequence(sequence_item) => {
                    current_sequence = self
                        .cfs
                        .sequences
                        .iter()
                        .find(|sequence| *sequence.id == *sequence_item.id)
                        .expect("Wrong cfs coordinates");
                }
                SequenceChildItem::Tile(_tile_item) => {
                    sequence_item_coord = Some(coord);
                }
            }
        }

        (current_sequence, sequence_item_coord)
    }

    pub fn try_get_item(&self, coordinates: &CfsCoordinates) -> Option<&SequenceChildItem> {
        let mut current_sequence = self.cfs.sequences.get(self.entrypoint_coordinate as usize);
        let mut current_child_item: Option<&SequenceChildItem> = None;

        for &coord in coordinates.iter() {
            let sequence = current_sequence?;
            let child = sequence.items.get(coord as usize)?;
            current_child_item = Some(child);

            match child {
                SequenceChildItem::Sequence(item) => {
                    current_sequence = self.cfs.sequences.iter().find(|seq| seq.id == item.id);
                }
                _ => {
                    current_sequence = None;
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SequenceDef {
    pub id: SequenceId,
    pub input_sources: Vec<InputBinding>,
    pub items: Vec<SequenceChildItem>,
}

impl SequenceDef {
    /// Create a new sequence definition.
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            input_sources: Vec::new(),
            items: Vec::new(),
        }
    }

    pub fn sequences(&self) -> Vec<SequenceItem> {
        self.items
            .iter()
            .filter_map(|item| match item {
                SequenceChildItem::Tile(_) => None,
                SequenceChildItem::Sequence(sequence) => Some(sequence.clone()),
            })
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SequenceChildItem {
    Sequence(SequenceItem),
    Tile(TileItem),
}

impl SequenceChildItem {
    pub fn inputs(&self) -> &[InputBinding] {
        match self {
            SequenceChildItem::Sequence(sequence_item) => &sequence_item.sources,
            SequenceChildItem::Tile(tile_item) => &tile_item.sources,
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
pub struct InputBinding {
    pub source: InputSource,
}

impl InputBinding {
    /// Create a binding from an input source.
    pub fn new(source: InputSource) -> Self {
        Self { source }
    }

    /// Create an external input binding.
    pub fn external() -> Self {
        Self::new(InputSource::External)
    }

    /// Create a sequence input binding.
    pub fn seq_input(input_index: usize) -> Self {
        Self::new(InputSource::SeqInput { input_index })
    }

    /// Create an item output binding.
    pub fn item_output(item_index: usize, output_index: usize) -> Self {
        Self::new(InputSource::ItemOutput {
            item_index,
            output_index,
        })
    }
}

/// Source of an input value in the data flow schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum InputSource {
    /// Input comes from outside the sequence (runtime-provided).
    External,

    /// Input comes from one of the sequence's declared inputs.
    SeqInput {
        /// Index of the sequence input (0-based).
        input_index: usize,
    },

    /// Input comes from a previous item's output.
    ItemOutput {
        /// Index of the item in the sequence (0-based).
        item_index: usize,
        /// Index of the output from that item (0-based).
        output_index: usize,
    },
}
