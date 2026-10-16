//! One-pass raster encoding: a value's payload, root **and index nodes**
//! together.
//!
//! The payload and root come from `raster_core::tree::assemble_subtree`, the
//! one encoder, so they are byte-identical to `subtree_payload_and_root` by
//! construction. What this adds is the index, built in the same post-order
//! walk: each node's children are already encoded when it is assembled, so
//! their offsets follow from their payload lengths and nothing is hashed or
//! encoded twice. (The builder this replaces re-encoded every subtree once per
//! ancestor.)
//!
//! Offsets are parent-relative (`rindex04`, see `RasterNode::offset`), which
//! is also what lets [`crate::draft_buffer`] encode a draft's elements one at a
//! time and seal them without revisiting any.

use raster_core::input::Hash32;
use raster_core::tree::{assemble_subtree, subtree_children, TreeValue, LIST_HANDLE_HEADER_LEN};
use raster_core::Result as CoreResult;

use crate::input::finalize_raster_kind;
use crate::raster_index::RasterNode;

/// An encoded subtree: its payload, raster root, and the id of its node in the
/// arena it was encoded into.
pub(crate) struct EncodedSubtree {
    pub payload: Vec<u8>,
    pub root: Hash32,
    pub node: u64,
}

/// Where a node's own region starts inside its payload: past the 49-byte
/// header for a list handle, whose index node stands for the inline list.
pub(crate) fn node_shift(value: &TreeValue) -> u64 {
    match value {
        TreeValue::ListHandle(_) => LIST_HANDLE_HEADER_LEN,
        _ => 0,
    }
}

/// Encode `value`, appending its nodes to `nodes`.
///
/// The returned node's `offset` is its shift inside its own payload (`0`, or
/// the handle header); a parent adds the payload's position inside its own
/// region via [`place_child`]. For a root that is already the final value.
pub(crate) fn encode_indexed(
    value: &TreeValue,
    nodes: &mut Vec<RasterNode>,
) -> CoreResult<EncodedSubtree> {
    struct Frame<'a> {
        value: &'a TreeValue,
        children: Vec<&'a TreeValue>,
        next: usize,
        done: Vec<EncodedSubtree>,
    }

    let mut stack = vec![Frame {
        value,
        children: subtree_children(value),
        next: 0,
        done: Vec::new(),
    }];

    loop {
        let frame = stack.last_mut().expect("the stack is never empty here");
        if frame.next < frame.children.len() {
            let child = frame.children[frame.next];
            frame.next += 1;
            stack.push(Frame {
                value: child,
                children: subtree_children(child),
                next: 0,
                done: Vec::new(),
            });
            continue;
        }

        let frame = stack.pop().expect("just observed");
        let encoded = assemble_node(frame.value, frame.done, nodes)?;
        match stack.last_mut() {
            Some(parent) => parent.done.push(encoded),
            None => return Ok(encoded),
        }
    }
}

/// Assemble one node from its encoded children: payload and root by the core
/// encoder, then the node itself, then each child's offset inside it.
fn assemble_node(
    value: &TreeValue,
    children: Vec<EncodedSubtree>,
    nodes: &mut Vec<RasterNode>,
) -> CoreResult<EncodedSubtree> {
    // A map's payload lists its entries sorted by key payload (stable), which
    // is `assemble_subtree`'s order; take it here, before the payloads move.
    let map_order: Vec<usize> = match value {
        TreeValue::Map(_) => {
            let mut order: Vec<usize> = (0..children.len() / 2).collect();
            order.sort_by(|left, right| {
                children[2 * left].payload.cmp(&children[2 * right].payload)
            });
            order
        }
        _ => Vec::new(),
    };
    let child_nodes: Vec<u64> = children.iter().map(|child| child.node).collect();
    let child_roots: Vec<Hash32> = children.iter().map(|child| child.root).collect();
    let child_lens: Vec<u64> = children
        .iter()
        .map(|child| child.payload.len() as u64)
        .collect();
    let (payload, root) = assemble_subtree(
        value,
        children
            .into_iter()
            .map(|child| (child.payload, child.root))
            .collect(),
    )?;

    let shift = node_shift(value);
    let layout = child_layout(value, &child_lens, &child_nodes, &child_roots, &map_order);
    for (child, payload_start) in &layout.placed {
        place_child(nodes, *child, *payload_start, shift);
    }

    let id = nodes.len() as u64;
    nodes.push(RasterNode {
        offset: shift,
        len: payload.len() as u64 - shift,
        root_hash: root,
        kind: finalize_raster_kind(value, &layout.kind_ids, &layout.kind_hashes)?,
    });
    Ok(EncodedSubtree {
        payload,
        root,
        node: id,
    })
}

/// Turn a child's own shift into its offset relative to the parent node:
/// `payload_start` is where the child's payload begins inside the parent's
/// payload, and the parent node itself starts `parent_shift` into it.
pub(crate) fn place_child(nodes: &mut [RasterNode], child: u64, payload_start: u64, parent_shift: u64) {
    let node = &mut nodes[child as usize];
    node.offset = payload_start + node.offset - parent_shift;
}

struct ChildLayout {
    /// `(child node, payload start inside the parent payload)`.
    placed: Vec<(u64, u64)>,
    /// Child ids and roots in the order `finalize_raster_kind` expects — for a
    /// map, the payload's sorted entry order.
    kind_ids: Vec<u64>,
    kind_hashes: Vec<Hash32>,
}

/// Where each child's payload sits inside the parent payload — the layout
/// `assemble_subtree` writes, read back from the child lengths.
fn child_layout(
    value: &TreeValue,
    lens: &[u64],
    ids: &[u64],
    roots: &[Hash32],
    map_order: &[usize],
) -> ChildLayout {
    let mut placed = Vec::with_capacity(ids.len());
    let sequential = |mut cursor: u64, placed: &mut Vec<(u64, u64)>| {
        for (id, len) in ids.iter().zip(lens) {
            placed.push((*id, cursor + 8));
            cursor += 8 + len;
        }
    };
    match value {
        TreeValue::Struct(fields) => {
            let mut cursor = 1 + 8;
            for ((name, _), (id, len)) in fields.iter().zip(ids.iter().zip(lens)) {
                cursor += 8 + name.len() as u64;
                placed.push((*id, cursor + 8));
                cursor += 8 + len;
            }
        }
        TreeValue::EnumStruct(variant, _) | TreeValue::EnumTuple(variant, _) => {
            sequential(1 + 8 + variant.len() as u64 + 8, &mut placed)
        }
        TreeValue::List(_) => sequential(1 + 8, &mut placed),
        TreeValue::ListHandle(_) => sequential(LIST_HANDLE_HEADER_LEN + 1 + 8, &mut placed),
        TreeValue::EnumNewtype(variant, _) => {
            placed.push((ids[0], 1 + 8 + variant.len() as u64 + 8));
        }
        TreeValue::Map(_) => {
            let mut cursor = 1 + 8u64;
            for entry in map_order {
                let (key, value) = (2 * entry, 2 * entry + 1);
                placed.push((ids[key], cursor + 8));
                cursor += 8 + lens[key];
                placed.push((ids[value], cursor + 8));
                cursor += 8 + lens[value];
            }
            return ChildLayout {
                placed,
                kind_ids: map_order
                    .iter()
                    .flat_map(|entry| [ids[2 * entry], ids[2 * entry + 1]])
                    .collect(),
                kind_hashes: map_order
                    .iter()
                    .flat_map(|entry| [roots[2 * entry], roots[2 * entry + 1]])
                    .collect(),
            };
        }
        _ => {}
    }
    ChildLayout {
        placed,
        kind_ids: ids.to_vec(),
        kind_hashes: roots.to_vec(),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::BTreeMap;

    use raster_core::collections::List;
    use raster_core::input::{SelectorPath, SelectorSegment};
    use serde::Serialize;

    use crate::input::{encode_raster_value, encode_raster_value_reference, tree_value_from_raster_location};
    use crate::raster_index::{RasterIndex, RasterNodeKind};

    /// Every selector path the reference index can answer: each field, each
    /// list element, a few ranges per list — recursively.
    pub(crate) fn paths(index: &RasterIndex) -> Vec<SelectorPath> {
        let mut out = vec![SelectorPath::default()];
        let mut stack = vec![(index.root_node, Vec::<SelectorSegment>::new())];
        while let Some((node, prefix)) = stack.pop() {
            match &index.get_node(node).unwrap().kind {
                RasterNodeKind::Struct { fields } => {
                    for field in fields {
                        let mut path = prefix.clone();
                        path.push(SelectorSegment::Field(field.name.clone()));
                        out.push(SelectorPath::new(path.clone()));
                        stack.push((field.child, path));
                    }
                }
                RasterNodeKind::List { len, elements, .. } => {
                    for (i, child) in elements.iter().enumerate() {
                        let mut path = prefix.clone();
                        path.push(SelectorSegment::Index(i as u64));
                        out.push(SelectorPath::new(path.clone()));
                        stack.push((*child, path));
                    }
                    for (start, end) in [(0, *len), (0, 1), (len / 2, *len)] {
                        if start < end && end <= *len {
                            let mut path = prefix.clone();
                            path.push(SelectorSegment::Range { start, end });
                            out.push(SelectorPath::new(path));
                        }
                    }
                }
                _ => {}
            }
        }
        out
    }

    /// The one-pass encoder answers every selection exactly as the builder it
    /// replaced: same payload, root, located bytes, proof and decoded value.
    fn assert_matches_reference<T: Serialize>(value: &T) {
        let (payload, index_bytes, _) = encode_raster_value(value).unwrap();
        let index = RasterIndex::from_bytes(&index_bytes).unwrap();
        let (reference_payload, reference) = encode_raster_value_reference(value).unwrap();
        assert_same_object(&payload, &index, &reference_payload, &reference);
    }

    /// Two encodings of one object answer every selection identically. Node
    /// ids may differ; positions, lengths, roots, proofs and values may not.
    pub(crate) fn assert_same_object(
        payload: &[u8],
        index: &RasterIndex,
        reference_payload: &[u8],
        reference: &RasterIndex,
    ) {
        assert_eq!(payload, reference_payload, "payload");
        assert_eq!(index.root_commitment, reference.root_commitment, "root");
        for path in paths(&reference) {
            let new = index.select(&path).unwrap();
            let old = reference.select(&path).unwrap();
            assert_eq!(
                (new.offset, new.len, new.root_hash, &new.steps, new.range),
                (old.offset, old.len, old.root_hash, &old.steps, old.range),
                "selection {:?}",
                path
            );
            let new_value =
                tree_value_from_raster_location(index, &payload.to_vec(), &index.locate(&path).unwrap())
                    .unwrap();
            let old_value = tree_value_from_raster_location(
                reference,
                &reference_payload.to_vec(),
                &reference.locate(&path).unwrap(),
            )
            .unwrap();
            assert_eq!(new_value, old_value, "value at {:?}", path);
        }
    }

    #[derive(Serialize)]
    struct Line {
        text: String,
        n: u32,
    }

    #[derive(Serialize)]
    enum Shape {
        Unit,
        New(u64),
        Tuple(u8, String),
        Named { a: i32, b: List<u16> },
    }

    #[derive(Serialize)]
    struct Everything {
        title: String,
        lines: List<String>,
        rows: List<Line>,
        nested: List<List<u8>>,
        plain: Vec<u64>,
        empty: List<u32>,
        maybe: Option<String>,
        none: Option<u8>,
        shapes: Vec<Shape>,
        map: BTreeMap<String, u32>,
        unit: (),
        tuple: (u8, String),
    }

    #[test]
    fn the_one_pass_encoder_matches_the_reference_builder() {
        assert_matches_reference(&42u64);
        assert_matches_reference(&String::from("leaf"));
        assert_matches_reference(&List::from(vec![String::from("a"), String::from("bb")]));
        assert_matches_reference(&vec![vec![1u8, 2], vec![], vec![3]]);
        let map: BTreeMap<String, u32> = [("zz", 1), ("a", 2), ("mm", 3)]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        assert_matches_reference(&map);
        assert_matches_reference(&Everything {
            title: "t".into(),
            lines: List::from((0..7).map(|i| format!("line {i}")).collect::<Vec<_>>()),
            rows: List::from(
                (0..5)
                    .map(|n| Line {
                        text: "x".repeat(n as usize),
                        n,
                    })
                    .collect::<Vec<_>>(),
            ),
            nested: List::from(vec![List::from(vec![1u8, 2, 3]), List::from(vec![])]),
            plain: vec![9, 8, 7],
            empty: List::from(vec![]),
            maybe: Some("yes".into()),
            none: None,
            shapes: vec![
                Shape::Unit,
                Shape::New(5),
                Shape::Tuple(1, "t".into()),
                Shape::Named {
                    a: -3,
                    b: List::from(vec![4, 5]),
                },
            ],
            map,
            unit: (),
            tuple: (7, "seven".into()),
        });
    }
}
