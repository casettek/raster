//! A tile's `FnInput::data`, encoded by whoever consumes the event rather than
//! by the program (`docs/proposals/instrumentation-pipeline.md`, stage 2,
//! option b).
//!
//! `data` is the postcard encoding of the tile's arguments, and the step's
//! `input_commitment` hashes it, so every traced tile needs it. Encoding a page
//! argument costs far more than cloning it, and the instrumentation thread has
//! time to spare while the program has none. So the generated wrapper clones
//! each argument before the call and sends the clones with the event; the
//! publisher encodes them, on its own thread when it has one.
//!
//! The bytes are the same either way: postcard encodes a tuple as its elements
//! one after another, so the parts concatenated are the tuple the wrapper
//! encoded before.
use raster_core::trace::TraceEvent;
use serde::Serialize;

/// An argument kept for encoding later.
pub trait DeferredEncode: Send {
    fn encode(&self) -> Option<Vec<u8>>;
}

impl<T: Serialize + Send> DeferredEncode for T {
    fn encode(&self) -> Option<Vec<u8>> {
        raster_core::postcard::to_allocvec(self).ok()
    }
}

/// One argument's share of `FnInput::data`.
pub enum InputPart {
    /// Encoded in the wrapper. `None` when encoding failed.
    Encoded(Option<Vec<u8>>),
    Deferred(Box<dyn DeferredEncode>),
}

impl InputPart {
    pub fn now<T: Serialize + ?Sized>(value: &T) -> Self {
        Self::Encoded(raster_core::postcard::to_allocvec(value).ok())
    }

    /// The part's bytes. An encoded part gives its buffer up; a deferred one
    /// keeps its value, so the value can go back to the thread that cloned it.
    fn encode(&mut self) -> Option<Vec<u8>> {
        match self {
            Self::Encoded(bytes) => bytes.take(),
            Self::Deferred(value) => value.encode(),
        }
    }
}

/// The parts of a tile's `FnInput::data`, in parameter order.
pub struct DeferredInput {
    parts: Vec<InputPart>,
}

impl DeferredInput {
    pub fn new(parts: Vec<InputPart>) -> Self {
        Self { parts }
    }

    /// What the wrapper's `postcard::to_allocvec(&(a, b, …)).unwrap_or_default()`
    /// gave: empty if any argument failed to encode. Once only: encoded parts
    /// are moved out.
    pub fn encode(&mut self) -> Vec<u8> {
        let mut parts = self.parts.iter_mut();
        let Some(first) = parts.next() else {
            return Vec::new();
        };
        let Some(mut data) = first.encode() else {
            return Vec::new();
        };
        for part in parts {
            match part.encode() {
                Some(bytes) => data.extend_from_slice(&bytes),
                None => return Vec::new(),
            }
        }
        data
    }

    /// Encode into the event's input.
    pub fn fill(&mut self, event: &mut TraceEvent) {
        if let Some(input) = tile_input(event) {
            input.data = self.encode();
        }
    }
}

fn tile_input(event: &mut TraceEvent) -> Option<&mut raster_core::trace::FnInput> {
    match event {
        TraceEvent::TileExec(record) | TraceEvent::RecurTileIterationExec(record) => {
            record.input.as_mut()
        }
        _ => None,
    }
}

/// Take the encoded `data` back out of a spent event, so it is freed by the
/// thread that encoded it.
pub(crate) fn take_input_data(event: &mut TraceEvent) -> Vec<u8> {
    tile_input(event)
        .map(|input| std::mem::take(&mut input.data))
        .unwrap_or_default()
}

/// An argument of the generated wrapper, choosing how its part of `data` is
/// made: a clone kept for later where the type allows it ([`DeferByClone`]),
/// else encoded on the spot ([`EncodeNow`]). Call as
/// `(&DeferArg(&value)).__raster_input_part()`, with both traits in scope:
/// method lookup tries `DeferByClone` first and falls back to `EncodeNow`
/// one auto-reference later, when the clone's bounds don't hold.
pub struct DeferArg<'a, T: ?Sized>(pub &'a T);

pub trait DeferByClone {
    fn __raster_input_part(&self) -> InputPart;
}

impl<T: Serialize + Clone + Send + 'static> DeferByClone for DeferArg<'_, T> {
    fn __raster_input_part(&self) -> InputPart {
        InputPart::Deferred(Box::new(self.0.clone()))
    }
}

pub trait EncodeNow {
    fn __raster_input_part(&self) -> InputPart;
}

impl<T: Serialize + ?Sized> EncodeNow for &DeferArg<'_, T> {
    fn __raster_input_part(&self) -> InputPart {
        InputPart::now(self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use raster_core::collections::BytesPage;

    #[allow(unused_imports)]
    use super::{DeferByClone as _, EncodeNow as _};

    /// Not `Clone`: must take the encode-now path and still give the same bytes.
    #[derive(Serialize)]
    struct NotClone(u32, String);

    #[test]
    fn parts_concatenate_to_the_tuple_encoding() {
        let page = BytesPage::__from_parts(2, 8, vec![1, 2, 3, 250]);
        let width = 7u32;
        let opaque = NotClone(5, "x".into());
        let parts = vec![
            (&DeferArg(&page)).__raster_input_part(),
            (&DeferArg(&width)).__raster_input_part(),
            (&DeferArg(&opaque)).__raster_input_part(),
        ];
        assert!(matches!(parts[0], InputPart::Deferred(_)));
        assert!(matches!(parts[2], InputPart::Encoded(_)));
        let expected = raster_core::postcard::to_allocvec(&(&page, &width, &opaque)).unwrap();
        assert_eq!(DeferredInput::new(parts).encode(), expected);
    }

    #[test]
    fn single_and_empty_inputs_match_the_wrapper() {
        let page = BytesPage::__from_parts(0, 0, vec![9; 300]);
        let mut single = DeferredInput::new(vec![(&DeferArg(&page)).__raster_input_part()]);
        assert_eq!(
            single.encode(),
            raster_core::postcard::to_allocvec(&page).unwrap()
        );
        assert!(DeferredInput::new(Vec::new()).encode().is_empty());
    }

    #[test]
    fn a_failed_part_empties_the_whole_input() {
        let parts = vec![
            InputPart::now(&1u8),
            InputPart::Encoded(None),
        ];
        assert!(DeferredInput::new(parts).encode().is_empty());
    }
}
