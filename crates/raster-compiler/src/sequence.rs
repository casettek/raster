use crate::{
    tile::{TileDiscovery, TileResult},
    Project,
};
use std::collections::HashSet;
use std::path::Path;

use crate::ast::FunctionAstItem;
use crate::tile::Tile;
#[derive(Debug, Clone)]
pub struct Sequence<'ast> {
    pub function: &'ast FunctionAstItem,
    pub steps: Vec<SequenceStep<'ast>>,
    pub description: Option<String>,
}

impl<'ast> Sequence<'ast> {
    pub fn iter(&'ast self) -> impl Iterator<Item = &'ast SequenceStep<'ast>> {
        self.steps.iter()
    }
}

#[derive(Debug, Clone)]
pub enum SequenceStep<'ast> {
    Tile(&'ast Tile<'ast>), // Reference to tile
    Sequence(String),       // Sequence name (resolved later if needed)
}

#[derive(Debug, Clone)]
pub struct SequenceResult {
    pub output: Vec<u8>,
    pub step_results: Vec<StepResult>,
}

#[derive(Debug, Clone)]
pub enum StepResult {
    Tile(TileResult),
    Sequence(SequenceResult),
}
#[derive(Debug, Clone)]
pub struct SequenceDiscovery<'ast> {
    pub project: &'ast Project,
    pub sequences: Vec<Sequence<'ast>>,
}

impl<'ast> SequenceDiscovery<'ast> {
    pub fn new(project: &'ast Project, tile_discovery: &'ast TileDiscovery<'ast>) -> Self {
        let project_ast = &project.ast;

        let sequence_names: HashSet<String> = project_ast
            .functions
            .iter()
            .filter(|f| {
                f.macros
                    .iter()
                    .any(|m| m.name == "sequence" || m.name == "raster::sequence")
            })
            .map(|f| f.name.clone())
            .collect();

        let sequences = project
            .ast
            .functions
            .iter()
            .filter(|f| sequence_names.contains(&f.name))
            .map(|f| Self::extract_sequence(f, &tile_discovery, &sequence_names))
            .collect();

        Self { sequences, project }
    }

    fn extract_sequence(
        func: &'ast FunctionAstItem,
        tile_discovery: &'ast TileDiscovery<'ast>,
        sequence_names: &HashSet<String>,
    ) -> Sequence<'ast> {
        let description = func
            .macros
            .iter()
            .find(|m| m.name == "sequence" || m.name == "raster::sequence")
            .and_then(|m| m.args.get("description").cloned());

        let steps: Vec<SequenceStep<'ast>> = func
            .call_infos
            .iter()
            .filter_map(|call_info| {
                // Try to find tile by name
                if let Some(tile) = tile_discovery.get(&call_info.callee) {
                    Some(SequenceStep::Tile(tile))
                } else if sequence_names.contains(&call_info.callee) {
                    Some(SequenceStep::Sequence(call_info.callee.clone()))
                } else {
                    None
                }
            })
            .collect();

        Sequence {
            function: func,
            steps,
            description,
        }
    }

    /// Get a sequence by name.
    pub fn get(&self, name: &str) -> Option<&Sequence<'ast>> {
        self.sequences.iter().find(|s| s.function.name == name)
    }
}

impl<'ast> Sequence<'ast> {
    pub fn id(&self) -> &str {
        &self.function.name
    }

    pub fn source_file(&self) -> &Path {
        &self.function.path
    }

    pub fn flatten<'a>(
        &'a self,
        seq_discovery: &'a SequenceDiscovery<'ast>,
    ) -> SequenceStepIter<'a, 'ast> {
        let mut stack = vec![self.steps.iter()];
        let mut visited = HashSet::new();
        visited.insert(self.id().to_string());

        SequenceStepIter {
            discovery: seq_discovery,
            stack,
            visited,
            pending_sequence: None,
        }
    }
}

/// Represents a flattened step yielded by the iterator.
/// When iterating, sequences are yielded before their contents are expanded.
#[derive(Debug, Clone)]
pub enum FlattenedStep<'a, 'ast> {
    Tile(&'a Tile<'ast>),
    Sequence(&'a Sequence<'ast>),
}
/// Iterator that yields flattened steps from a sequence, expanding nested sequences inline.
///
/// Uses a stack-based approach to avoid recursion and allow lazy iteration.
/// When a nested sequence is encountered, it yields the sequence first, then expands its steps.
/// Cycle detection prevents infinite loops when sequences reference each other.
pub struct SequenceStepIter<'a, 'ast> {
    discovery: &'a SequenceDiscovery<'ast>,
    /// Stack of iterators over sequence steps for nested expansion
    stack: Vec<std::slice::Iter<'a, SequenceStep<'ast>>>,
    /// Track visited sequences to detect cycles
    visited: HashSet<String>,
    /// Pending sequence to yield before expanding its steps
    pending_sequence: Option<&'a Sequence<'ast>>,
}

impl<'a, 'ast> Iterator for SequenceStepIter<'a, 'ast> {
    type Item = FlattenedStep<'a, 'ast>;

    fn next(&mut self) -> Option<Self::Item> {
        // If we have a pending sequence to yield, do that first
        if let Some(seq) = self.pending_sequence.take() {
            return Some(FlattenedStep::Sequence(seq));
        }

        while let Some(current_iter) = self.stack.last_mut() {
            match current_iter.next() {
                Some(SequenceStep::Tile(tile)) => return Some(FlattenedStep::Tile(tile)),
                Some(SequenceStep::Sequence(name)) => {
                    // Check for cycles - skip if already visited
                    if self.visited.contains(name) {
                        continue;
                    }
                    if let Some(nested_seq) = self.discovery.get(name) {
                        self.visited.insert(name.clone());
                        self.stack.push(nested_seq.steps.iter());
                        // Yield the sequence first, its steps will be expanded on subsequent calls
                        return Some(FlattenedStep::Sequence(nested_seq));
                    }
                }
                None => {
                    // Pop exhausted iterator from stack
                    self.stack.pop();
                }
            }
        }
        None
    }
}
