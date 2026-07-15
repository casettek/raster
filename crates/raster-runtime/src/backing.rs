//! What backs the bytes committed at a storage coordinate.
//!
//! An object at a coordinate is either `Owned` — bytes this run computed,
//! held in memory — or `Referenced`: no bytes at all, just a
//! struct-of-commitments over `main`'s entry arguments, resolved lazily from
//! disk the first time something selects into it. Keeping the two behind one
//! enum is what lets every reader treat "a value at a coordinate" uniformly
//! while only one of them can touch the filesystem.

use std::vec::Vec;

use raster_core::cfs::CfsCoordinates;
use raster_core::input::{
    struct_commitments_root, Hash32, SchemaNode, SelectedPayload, SelectionCommitment,
    SelectionProof, SelectionProofStep, SelectionWitness, SelectorPath, SelectorSegment,
};
use raster_core::trace::RasterPayload;
use raster_core::{Error, Result};
use serde::de::DeserializeOwned;

use crate::input::{
    hex_string, prove_selection, selected_payload_from_proven,
    selected_payload_from_raster_location, selection_witness_from_raster_selection,
    subtree_payload_and_root, tree_value_from_raster_location, typed_value_from_tree, TreeValue,
};
use crate::raster_index::RasterIndex;
use crate::source::{ResolvedSourceData, SourceResolver};

/// What backs the bytes committed at a storage coordinate.
#[derive(Debug, Clone)]
pub(crate) enum ObjectBacking {
    /// Bytes were computed this run (tile output, finalized draft, recur
    /// iteration, ...) and live in memory — reads are served directly.
    Owned(OwnedObject),
    /// This coordinate holds no bytes at all, only a struct-of-commitments
    /// over `main`'s declared entry arguments. A selection must name which
    /// argument it wants before anything can be resolved into it.
    Referenced(ReferencedObject),
}

#[derive(Debug, Clone)]
pub(crate) struct OwnedObject {
    pub bytes: Vec<u8>,
    pub raster: Option<RasterPayload>,
}

#[derive(Debug, Clone)]
pub(crate) struct ReferencedObject {
    pub sources: Vec<ReferencedSource>,
}

#[derive(Debug, Clone)]
pub(crate) struct ReferencedSource {
    pub name: String,
    pub commitment: Vec<u8>,
    pub kind: ReferencedSourceKind,
}

#[derive(Clone)]
pub(crate) enum ReferencedSourceKind {
    /// Self-describing on disk (an `.rindex` carries the schema), so no
    /// type-specific hook is needed to select into it.
    Raster,
    /// Postcard bytes carry no schema of their own; the macro-generated
    /// bind site supplies these two monomorphized, zero-capture function
    /// pointers so this stays a plain `Clone` enum rather than a trait
    /// object.
    Postcard {
        to_tree: fn(&[u8]) -> Result<TreeValue>,
        schema: fn() -> SchemaNode,
    },
}

impl std::fmt::Debug for ReferencedSourceKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Raster => f.write_str("Raster"),
            Self::Postcard { .. } => f.write_str("Postcard"),
        }
    }
}

impl ReferencedObject {
    /// The struct-of-commitments root over all declared sources, in
    /// declaration order. Uses the shared `TreeValue::Struct` convention, so
    /// the combined entry object is an ordinary struct node as far as
    /// selection is concerned — selecting one argument out of it composes as
    /// one ordinary proof step rather than a special case — and the guest's
    /// `checks::entrypoint::combined_root` recomputes the identical bytes by
    /// calling the same function.
    pub fn combined_root(&self) -> Vec<u8> {
        struct_commitments_root(
            self.sources
                .iter()
                .map(|source| (source.name.as_str(), source.commitment.as_slice())),
        )
        .to_vec()
    }

    fn find_source(&self, selector: &SelectorPath) -> Result<(&ReferencedSource, SelectorPath)> {
        let Some((head, rest)) = selector.segments.split_first() else {
            return Err(Error::Other(
                "Referenced object requires a field selector naming a declared entry argument"
                    .into(),
            ));
        };
        let SelectorSegment::Field(name) = head else {
            return Err(Error::Other(
                "Referenced object selector must start with a named field".into(),
            ));
        };
        let source = self
            .sources
            .iter()
            .find(|source| &source.name == name)
            .ok_or_else(|| Error::Other(format!("Unknown entry argument '{}'", name)))?;
        Ok((source, SelectorPath::new(rest.to_vec())))
    }

    fn verify_source_commitment(
        source: &ReferencedSource,
        resolved: &ResolvedSourceData,
    ) -> Result<()> {
        if !resolved
            .commitment()
            .eq_ignore_ascii_case(&hex_string(&source.commitment))
        {
            return Err(Error::Other(format!(
                "Entry argument '{}' resolved to a different commitment than authorized at bind time",
                source.name
            )));
        }
        Ok(())
    }

    /// The outer struct proof step over the named source: its position among
    /// the declared arguments, every argument's name, and the other
    /// arguments' already-public commitments as siblings (ascending position
    /// order, `field_index` skipped) — the exact shape
    /// `SelectionProofStep::Struct` expects.
    fn struct_step(&self, name: &str) -> Result<SelectionProofStep> {
        let index = self
            .sources
            .iter()
            .position(|source| source.name == name)
            .ok_or_else(|| Error::Other(format!("Unknown entry argument '{}'", name)))?;
        let siblings = self
            .sources
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != index)
            .map(|(_, source)| -> Result<Hash32> {
                source.commitment.as_slice().try_into().map_err(|_| {
                    Error::Other(format!(
                        "Entry argument '{}' commitment is not 32 bytes",
                        source.name
                    ))
                })
            })
            .collect::<Result<Vec<Hash32>>>()?;
        Ok(SelectionProofStep::Struct {
            field_index: index as u64,
            field_names: self
                .sources
                .iter()
                .map(|source| source.name.clone())
                .collect(),
            siblings,
        })
    }

    /// Resolve `selector` (must start with `Field(name)`) against the named
    /// source, returning the selected tree value and its selection payload
    /// anchored to the combined root.
    pub fn select(
        &self,
        selector: &SelectorPath,
        resolver: &dyn SourceResolver,
    ) -> Result<(TreeValue, SelectedPayload)> {
        let (source, remaining) = self.find_source(selector)?;
        let resolved = resolver.resolve(&source.name)?;
        Self::verify_source_commitment(source, &resolved)?;

        let (tree, mut selected) = self.select_from_resolved(source, &remaining, &resolved)?;

        selected.commitment.path = full_selector_path(&source.name, &remaining);
        selected.commitment.source_root_hash = self
            .combined_root()
            .try_into()
            .map_err(|_| Error::Other("Combined root is not 32 bytes".into()))?;
        Ok((tree, selected))
    }

    fn select_from_resolved(
        &self,
        source: &ReferencedSource,
        remaining: &SelectorPath,
        resolved: &ResolvedSourceData,
    ) -> Result<(TreeValue, SelectedPayload)> {
        match (&source.kind, resolved) {
            (ReferencedSourceKind::Raster, ResolvedSourceData::Raster { .. }) => {
                let index = resolved
                    .raster_index()
                    .ok_or_else(|| Error::Other("Expected raster index metadata".into()))?;
                let data = resolved
                    .raster_bytes()
                    .ok_or_else(|| Error::Other("Expected raster data bytes".into()))?;
                let location = index.locate(remaining)?;
                let tree = tree_value_from_raster_location(index, data, &location)?;
                let selected = selected_payload_from_raster_location(data, remaining, location)?;
                Ok((tree, selected))
            }
            (
                ReferencedSourceKind::Postcard { to_tree, schema },
                ResolvedSourceData::Postcard { .. },
            ) => {
                let tree = to_tree(resolved.bytes())?;
                let (_, root_hash) = subtree_payload_and_root(&tree)?;
                if root_hash.to_vec() != source.commitment {
                    return Err(Error::Other(format!(
                        "Entry argument '{}' failed structural integrity check",
                        source.name
                    )));
                }
                let proven = prove_selection(&schema(), &tree, &remaining.segments)?;
                let selected_tree = proven.selected_value.clone();
                let selected = selected_payload_from_proven(remaining, proven);
                Ok((selected_tree, selected))
            }
            _ => Err(Error::Other(format!(
                "Entry argument '{}' encoding does not match its declared kind",
                source.name
            ))),
        }
    }

    /// Same resolution as `select`, but produces a full `SelectionWitness`
    /// (with the recombination proof steps) for guest verification —
    /// prepending the outer struct step over the other sources' already-
    /// public commitments to whatever inner steps locate the value inside
    /// the named source.
    pub fn selection_witness(
        &self,
        selector: &SelectorPath,
        resolver: &dyn SourceResolver,
    ) -> Result<SelectionWitness> {
        let (source, remaining) = self.find_source(selector)?;
        let resolved = resolver.resolve(&source.name)?;
        Self::verify_source_commitment(source, &resolved)?;

        let inner = match (&source.kind, &resolved) {
            (ReferencedSourceKind::Raster, ResolvedSourceData::Raster { .. }) => {
                let index = resolved
                    .raster_index()
                    .ok_or_else(|| Error::Other("Expected raster index metadata".into()))?;
                let data = resolved
                    .raster_bytes()
                    .ok_or_else(|| Error::Other("Expected raster data bytes".into()))?;
                let selection = index.select(&remaining)?;
                selection_witness_from_raster_selection(data, &remaining, selection)?
            }
            (
                ReferencedSourceKind::Postcard { to_tree, schema },
                ResolvedSourceData::Postcard { .. },
            ) => {
                let tree = to_tree(resolved.bytes())?;
                let (_, root_hash) = subtree_payload_and_root(&tree)?;
                if root_hash.to_vec() != source.commitment {
                    return Err(Error::Other(format!(
                        "Entry argument '{}' failed structural integrity check",
                        source.name
                    )));
                }
                let proven = prove_selection(&schema(), &tree, &remaining.segments)?;
                SelectionWitness {
                    bytes: proven.selected_bytes.clone(),
                    proof: SelectionProof {
                        path: remaining.clone(),
                        root_hash: proven.root_hash,
                        steps: proven.steps.clone(),
                    },
                }
            }
            _ => {
                return Err(Error::Other(format!(
                    "Entry argument '{}' encoding does not match its declared kind",
                    source.name
                )))
            }
        };

        let mut steps = inner.proof.steps;
        // `verify_selection_proof` walks `steps` via `.rev()`, from the leaf
        // outward — so the outermost step (combining this source's own root
        // with its siblings into the combined root) must be the *first*
        // element, not appended after the source's own (more inner) steps.
        steps.insert(0, self.struct_step(&source.name)?);
        Ok(SelectionWitness {
            bytes: inner.bytes,
            proof: SelectionProof {
                path: full_selector_path(&source.name, &remaining),
                root_hash: self
                    .combined_root()
                    .try_into()
                    .map_err(|_| Error::Other("Combined root is not 32 bytes".into()))?,
                steps,
            },
        })
    }
}

fn full_selector_path(name: &str, remaining: &SelectorPath) -> SelectorPath {
    let mut segments = Vec::with_capacity(remaining.segments.len() + 1);
    segments.push(SelectorSegment::Field(name.to_string()));
    segments.extend(remaining.segments.iter().cloned());
    SelectorPath::new(segments)
}

impl OwnedObject {
    /// Whole-value resolve (no selector): the raster case walks the same
    /// path a selector-based read would with an empty selector; the
    /// non-raster case deserializes directly (there is no selection tree
    /// to prove against, hence the placeholder commitment — matches the
    /// pre-refactor behavior exactly).
    pub(crate) fn resolve_whole<T: DeserializeOwned>(
        &self,
        coordinates: &CfsCoordinates,
    ) -> Result<(Vec<u8>, SelectionCommitment, T)> {
        if let Some(raster) = self.raster.as_ref() {
            let index = RasterIndex::from_bytes(&raster.index_bytes)?;
            let location = index.root_location()?;
            let tree = tree_value_from_raster_location(&index, &raster.bytes, &location)?;
            let value = typed_value_from_tree(&tree)?;
            Ok((
                raster.bytes.clone(),
                SelectionCommitment {
                    path: SelectorPath::default(),
                    source_root_hash: raster.root_hash,
                    selected_hash: raster_core::input::selection_payload_hash(&raster.bytes),
                    selected_len: raster.bytes.len() as u64,
                },
                value,
            ))
        } else {
            let value = raster_core::postcard::from_bytes(&self.bytes).map_err(|e| {
                Error::Serialization(format!(
                    "Failed to deserialize storage object at coordinates {:?}: {}",
                    coordinates, e
                ))
            })?;
            Ok((
                self.bytes.clone(),
                SelectionCommitment {
                    path: SelectorPath::default(),
                    source_root_hash: [0; 32],
                    selected_hash: [0; 32],
                    selected_len: 0,
                },
                value,
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry_arguments::postcard_bytes_to_tree;
    use crate::input::tree_value_from_serialize;
    use raster_core::input::{ExternalEncoding, SchemaField, Selectable};
    use serde::{Deserialize, Serialize};
    use std::collections::BTreeMap;
    use std::sync::Arc;

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
    struct EntryA {
        value: u64,
    }

    impl Selectable for EntryA {
        fn schema() -> SchemaNode {
            SchemaNode::Struct {
                type_name: "EntryA".into(),
                fields: vec![SchemaField::new(
                    "value",
                    "value",
                    SchemaNode::Leaf {
                        type_name: "u64".into(),
                    },
                )],
            }
        }
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
    struct EntryB {
        name: String,
    }

    impl Selectable for EntryB {
        fn schema() -> SchemaNode {
            SchemaNode::Struct {
                type_name: "EntryB".into(),
                fields: vec![SchemaField::new(
                    "name",
                    "name",
                    SchemaNode::Leaf {
                        type_name: "String".into(),
                    },
                )],
            }
        }
    }

    struct FakeResolver {
        files: BTreeMap<String, (Vec<u8>, String)>,
    }

    impl SourceResolver for FakeResolver {
        fn manifest_commitment_metadata(&self, name: &str) -> Result<(ExternalEncoding, String)> {
            let (_, commitment) = self
                .files
                .get(name)
                .cloned()
                .ok_or_else(|| Error::Other(format!("no fixture for '{}'", name)))?;
            Ok((ExternalEncoding::Postcard, commitment))
        }

        fn resolve(&self, name: &str) -> Result<ResolvedSourceData> {
            let (bytes, commitment) = self
                .files
                .get(name)
                .cloned()
                .ok_or_else(|| Error::Other(format!("no fixture for '{}'", name)))?;
            Ok(ResolvedSourceData::Postcard {
                commitment,
                file: crate::source::SourceFile::Read(Arc::from(bytes.into_boxed_slice())),
            })
        }
    }

    fn referenced_object_fixture() -> (ReferencedObject, FakeResolver) {
        let a = EntryA { value: 42 };
        let b = EntryB {
            name: "hello".into(),
        };
        let a_bytes = raster_core::postcard::to_allocvec(&a).unwrap();
        let b_bytes = raster_core::postcard::to_allocvec(&b).unwrap();
        let (_, a_root) =
            subtree_payload_and_root(&tree_value_from_serialize(&a).unwrap()).unwrap();
        let (_, b_root) =
            subtree_payload_and_root(&tree_value_from_serialize(&b).unwrap()).unwrap();

        let sources = vec![
            ReferencedSource {
                name: "entry_a".into(),
                commitment: a_root.to_vec(),
                kind: ReferencedSourceKind::Postcard {
                    to_tree: postcard_bytes_to_tree::<EntryA>,
                    schema: EntryA::schema,
                },
            },
            ReferencedSource {
                name: "entry_b".into(),
                commitment: b_root.to_vec(),
                kind: ReferencedSourceKind::Postcard {
                    to_tree: postcard_bytes_to_tree::<EntryB>,
                    schema: EntryB::schema,
                },
            },
        ];
        let referenced = ReferencedObject { sources };

        let mut files = BTreeMap::new();
        files.insert("entry_a".to_string(), (a_bytes, hex_string(&a_root)));
        files.insert("entry_b".to_string(), (b_bytes, hex_string(&b_root)));
        (referenced, FakeResolver { files })
    }

    #[test]
    fn referenced_object_selects_named_source_field() {
        let (referenced, resolver) = referenced_object_fixture();
        let selector = SelectorPath::new(vec![
            SelectorSegment::Field("entry_a".into()),
            SelectorSegment::Field("value".into()),
        ]);

        let (tree, selected) = referenced.select(&selector, &resolver).unwrap();

        assert_eq!(typed_value_from_tree::<u64>(&tree).unwrap(), 42);
        assert_eq!(
            selected.commitment.source_root_hash.to_vec(),
            referenced.combined_root()
        );
    }

    #[test]
    fn referenced_object_selection_witness_verifies_against_combined_root() {
        let (referenced, resolver) = referenced_object_fixture();
        let selector = SelectorPath::new(vec![
            SelectorSegment::Field("entry_b".into()),
            SelectorSegment::Field("name".into()),
        ]);

        let (_, selected) = referenced.select(&selector, &resolver).unwrap();
        let witness = referenced.selection_witness(&selector, &resolver).unwrap();

        assert!(raster_core::input::verify_selection_witness(
            &selected.commitment,
            &witness
        ));
    }

    #[test]
    fn referenced_object_rejects_unknown_argument_name() {
        let (referenced, resolver) = referenced_object_fixture();
        let selector = SelectorPath::new(vec![SelectorSegment::Field("missing".into())]);

        let err = referenced.select(&selector, &resolver).unwrap_err();

        assert!(err.to_string().contains("Unknown entry argument"));
    }

    #[test]
    fn referenced_object_rejects_tampered_source_bytes() {
        let (referenced, mut resolver) = referenced_object_fixture();
        resolver.files.get_mut("entry_a").unwrap().0 =
            raster_core::postcard::to_allocvec(&EntryA { value: 999 }).unwrap();
        let selector = SelectorPath::new(vec![
            SelectorSegment::Field("entry_a".into()),
            SelectorSegment::Field("value".into()),
        ]);

        let err = referenced.select(&selector, &resolver).unwrap_err();

        assert!(err
            .to_string()
            .contains("failed structural integrity check"));
    }

    #[test]
    fn combined_root_uses_the_shared_struct_commitment_convention() {
        let (referenced, _resolver) = referenced_object_fixture();

        // Deliberately not a hand-rolled hash: the whole point of sharing
        // `struct_commitments_root` is that the guest recomputes this by
        // calling the same function, so a local re-implementation here would
        // only test that two copies agree, not that there is one.
        let expected = struct_commitments_root(
            referenced
                .sources
                .iter()
                .map(|source| (source.name.as_str(), source.commitment.as_slice())),
        )
        .to_vec();

        assert_eq!(referenced.combined_root(), expected);
    }

    #[test]
    fn combined_root_distinguishes_entry_arguments_by_name() {
        let (referenced, _resolver) = referenced_object_fixture();
        let renamed = ReferencedObject {
            sources: referenced
                .sources
                .iter()
                .enumerate()
                .map(|(index, source)| ReferencedSource {
                    name: format!("renamed_{}", index),
                    commitment: source.commitment.clone(),
                    kind: source.kind.clone(),
                })
                .collect(),
        };

        assert_ne!(referenced.combined_root(), renamed.combined_root());
    }
}
