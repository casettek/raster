//! Procedural macros for the Raster toolchain.
//!
//! This crate provides:
//! - `#[tile]` - Marks a function as a tile and registers it in the global registry
//! - `#[sequence]` - Declares tile ordering and control flow

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{parse_macro_input, visit::Visit, Expr, ExprCall, ExprPath, FnArg, ItemFn, Pat, ReturnType, Type};

/// Parses tile attributes from the macro invocation.
/// 
/// The first argument must be the tile type: `iter` or `recur`.
struct TileAttrs {
    /// Tile type (required: "iter" or "recur").
    tile_type: String,
    estimated_cycles: Option<u64>,
    max_memory: Option<u64>,
    description: Option<String>,
}

impl TileAttrs {
    fn parse(attr: TokenStream) -> Self {
        let mut attrs = TileAttrs {
            tile_type: "iter".to_string(), // fallback, but should always be specified
            estimated_cycles: None,
            max_memory: None,
            description: None,
        };

        if attr.is_empty() {
            panic!("tile macro requires a type argument, e.g., #[tile(iter)] or #[tile(recur)]");
        }

        // Parse comma-separated: first is tile type, rest are key=value pairs
        let attr_str = attr.to_string();
        let mut first = true;
        
        for part in attr_str.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }

            if first {
                // First argument is the tile type
                first = false;
                match part {
                    "iter" | "recur" => {
                        attrs.tile_type = part.to_string();
                    }
                    _ => {
                        panic!("Unknown tile type '{}'. Valid types: iter, recur", part);
                    }
                }
                continue;
            }

            if let Some((key, value)) = part.split_once('=') {
                // Key=value pair
                let key = key.trim();
                let value = value.trim().trim_matches('"');
                match key {
                    "estimated_cycles" => {
                        attrs.estimated_cycles = value.parse().ok();
                    }
                    "max_memory" => {
                        attrs.max_memory = value.parse().ok();
                    }
                    "description" => {
                        attrs.description = Some(value.to_string());
                    }
                    _ => {}
                }
            }
        }

        attrs
    }
}

/// Marks a function as a Raster tile.
///
/// This macro:
/// 1. Preserves the original function unchanged
/// 2. Generates an ABI wrapper that handles bincode serialization/deserialization
/// 3. Registers the tile in the global `TILE_REGISTRY` distributed slice
///
/// # Arguments
/// The first argument is required and specifies the tile type:
/// - `iter` - Standard iterative tile (most common)
/// - `recur` - Recursive tile for stateful computations
///
/// # Optional Attributes
/// - `estimated_cycles = N` - Expected cycle count for resource estimation
/// - `max_memory = N` - Maximum memory usage in bytes
/// - `description = "..."` - Human-readable description
///
/// # Example
/// ```ignore
/// #[tile(iter)]
/// fn compute(input: u64) -> u64 {
///     input * 2
/// }
///
/// #[tile(iter, estimated_cycles = 1000, description = "Greets a user")]
/// fn greet(name: String) -> String {
///     format!("Hello, {}!", name)
/// }
///
/// #[tile(recur)]
/// fn iterate(state: State) -> State {
///     // recursive computation
/// }
/// ```
#[proc_macro_attribute]
pub fn tile(attr: TokenStream, item: TokenStream) -> TokenStream {
    let attrs = TileAttrs::parse(attr);
    let input_fn = parse_macro_input!(item as ItemFn);

    let fn_name = &input_fn.sig.ident;
    let fn_name_str = fn_name.to_string();
    let wrapper_name = format_ident!("__raster_tile_entry_{}", fn_name);
    let registration_name = format_ident!("__RASTER_TILE_REGISTRATION_{}", fn_name.to_string().to_uppercase());

    // Extract input types for deserialization
    let input_types: Vec<_> = input_fn
        .sig
        .inputs
        .iter()
        .filter_map(|arg| match arg {
            FnArg::Typed(pat_type) => Some(&*pat_type.ty),
            FnArg::Receiver(_) => None,
        })
        .collect();

    // Extract input pattern names for calling the original function
    let input_names: Vec<_> = input_fn
        .sig
        .inputs
        .iter()
        .filter_map(|arg| match arg {
            FnArg::Typed(pat_type) => match &*pat_type.pat {
                Pat::Ident(pat_ident) => Some(&pat_ident.ident),
                _ => None,
            },
            FnArg::Receiver(_) => None,
        })
        .collect();

    // Check if function returns a Result or a plain value
    let returns_result = match &input_fn.sig.output {
        ReturnType::Default => false,
        ReturnType::Type(_, ty) => {
            if let Type::Path(type_path) = &**ty {
                type_path
                    .path
                    .segments
                    .last()
                    .map(|seg| seg.ident == "Result")
                    .unwrap_or(false)
            } else {
                false
            }
        }
    };

    // Generate the deserialization and call logic based on number of args
    let deserialize_and_call = if input_types.is_empty() {
        // No arguments
        if returns_result {
            quote! {
                let result = #fn_name()?;
            }
        } else {
            quote! {
                let result = #fn_name();
            }
        }
    } else if input_types.len() == 1 {
        // Single argument - deserialize directly
        let ty = input_types[0];
        let name = input_names[0];
        if returns_result {
            quote! {
                let #name: #ty = ::raster::core::postcard::from_bytes(input)
                    .map_err(|e| ::raster::core::Error::Serialization(::alloc::format!("Failed to deserialize input: {}", e)))?;
                let result = #fn_name(#name)?;
            }
        } else {
            quote! {
                let #name: #ty = ::raster::core::postcard::from_bytes(input)
                    .map_err(|e| ::raster::core::Error::Serialization(::alloc::format!("Failed to deserialize input: {}", e)))?;
                let result = #fn_name(#name);
            }
        }
    } else {
        // Multiple arguments - deserialize as tuple
        let tuple_type = quote! { (#(#input_types),*) };
        if returns_result {
            quote! {
                let (#(#input_names),*): #tuple_type = ::raster::core::postcard::from_bytes(input)
                    .map_err(|e| ::raster::core::Error::Serialization(::alloc::format!("Failed to deserialize input: {}", e)))?;
                let result = #fn_name(#(#input_names),*)?;
            }
        } else {
            quote! {
                let (#(#input_names),*): #tuple_type = ::raster::core::postcard::from_bytes(input)
                    .map_err(|e| ::raster::core::Error::Serialization(::alloc::format!("Failed to deserialize input: {}", e)))?;
                let result = #fn_name(#(#input_names),*);
            }
        }
    };

    // Generate optional metadata fields
    let estimated_cycles_expr = match attrs.estimated_cycles {
        Some(cycles) => quote! { ::core::option::Option::Some(#cycles) },
        None => quote! { ::core::option::Option::None },
    };

    let max_memory_expr = match attrs.max_memory {
        Some(memory) => quote! { ::core::option::Option::Some(#memory) },
        None => quote! { ::core::option::Option::None },
    };

    let description_expr = match &attrs.description {
        Some(desc) => {
            let desc_str = desc.as_str();
            quote! { ::core::option::Option::Some(#desc_str) }
        }
        None => quote! { ::core::option::Option::None },
    };

    let expanded = quote! {
        // Keep the original function unchanged
        #input_fn

        // Generate the ABI wrapper function (available on all platforms, no_std compatible)
        pub fn #wrapper_name(input: &[u8]) -> ::raster::core::Result<::alloc::vec::Vec<u8>> {
            #deserialize_and_call

            ::raster::core::postcard::to_allocvec(&result)
                .map_err(|e| ::raster::core::Error::Serialization(::alloc::format!("Failed to serialize output: {}", e)))
        }

        // Register the tile in the distributed slice (only on platforms that support linkme and std)
        #[cfg(all(feature = "std", not(target_arch = "riscv32")))]
        #[::raster::core::linkme::distributed_slice(::raster::core::registry::TILE_REGISTRY)]
        #[linkme(crate = ::raster::core::linkme)]
        static #registration_name: ::raster::core::registry::TileRegistration =
            ::raster::core::registry::TileRegistration::new(
                ::raster::core::tile::TileMetadataStatic::new(
                    #fn_name_str,
                    #fn_name_str,
                    #description_expr,
                    #estimated_cycles_expr,
                    #max_memory_expr,
                ),
                #wrapper_name,
            );
    };

    TokenStream::from(expanded)
}

/// Parses optional sequence attributes from the macro invocation.
struct SequenceAttrs {
    description: Option<String>,
}

impl SequenceAttrs {
    fn parse(attr: TokenStream) -> Self {
        let mut attrs = SequenceAttrs {
            description: None,
        };

        if attr.is_empty() {
            return attrs;
        }

        // Parse comma-separated key=value pairs
        let attr_str = attr.to_string();
        for part in attr_str.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }

            if let Some((key, value)) = part.split_once('=') {
                let key = key.trim();
                let value = value.trim().trim_matches('"');
                if key == "description" {
                    attrs.description = Some(value.to_string());
                }
            }
        }

        attrs
    }
}

/// Visitor that extracts function call names from the AST.
/// It collects function names in the order they appear.
struct TileCallExtractor {
    tile_calls: Vec<String>,
}

impl TileCallExtractor {
    fn new() -> Self {
        Self { tile_calls: Vec::new() }
    }
}

impl<'ast> Visit<'ast> for TileCallExtractor {
    fn visit_expr_call(&mut self, node: &'ast ExprCall) {
        // Check if this is a simple function call (not a method call)
        if let Expr::Path(ExprPath { path, .. }) = &*node.func {
            // Get the function name (last segment of the path)
            if let Some(segment) = path.segments.last() {
                let fn_name = segment.ident.to_string();
                // Skip common non-tile functions
                if !is_excluded_function(&fn_name) {
                    self.tile_calls.push(fn_name);
                }
            }
        }
        // Continue visiting nested expressions
        syn::visit::visit_expr_call(self, node);
    }
}

/// Check if a function name should be excluded from tile extraction.
/// These are common Rust functions that are not tiles.
fn is_excluded_function(name: &str) -> bool {
    matches!(
        name,
        // Common Rust functions that aren't tiles
        "println" | "print" | "eprintln" | "eprint" | "dbg" |
        "format" | "panic" | "assert" | "assert_eq" | "assert_ne" |
        "Some" | "None" | "Ok" | "Err" |
        "Box" | "Vec" | "String" | "to_string" | "to_owned" |
        "clone" | "into" | "from" | "default" |
        // Allocator functions
        "alloc" | "dealloc"
    )
}

/// Declares a sequence of tiles with linear control flow.
///
/// The `#[sequence]` macro parses the function body to extract tile calls
/// in the order they appear. The function remains callable for native execution,
/// and the sequence is registered for use with `cargo raster preview`.
///
/// # Attributes
/// - `description = "..."` - Human-readable description of the sequence
///
/// # Example
/// ```ignore
/// #[sequence]
/// fn main(name: String) -> String {
///     let greeting = greet(name);
///     exclaim(greeting)
/// }
/// ```
///
/// This will register a sequence named "main" with tiles `["greet", "exclaim"]`.
#[proc_macro_attribute]
pub fn sequence(attr: TokenStream, item: TokenStream) -> TokenStream {
    let attrs = SequenceAttrs::parse(attr);
    let input_fn = parse_macro_input!(item as ItemFn);

    let fn_name = &input_fn.sig.ident;
    let fn_name_str = fn_name.to_string();
    let registration_name = format_ident!("__RASTER_SEQUENCE_REGISTRATION_{}", fn_name.to_string().to_uppercase());

    // Extract tile calls from the function body
    let mut extractor = TileCallExtractor::new();
    extractor.visit_item_fn(&input_fn);
    let tile_calls = extractor.tile_calls;

    // Generate the tile list as a static array
    let tile_count = tile_calls.len();
    let tile_strs: Vec<_> = tile_calls.iter().map(|s| s.as_str()).collect();

    // Generate description expression
    let description_expr = match &attrs.description {
        Some(desc) => {
            let desc_str = desc.as_str();
            quote! { ::core::option::Option::Some(#desc_str) }
        }
        None => quote! { ::core::option::Option::None },
    };

    let expanded = quote! {
        // Keep the original function unchanged for native execution
        #input_fn

        // Static array of tile IDs for this sequence
        #[cfg(all(feature = "std", not(target_arch = "riscv32")))]
        static __RASTER_SEQUENCE_TILES: [&'static str; #tile_count] = [#(#tile_strs),*];

        // Register the sequence in the distributed slice (only on platforms that support linkme and std)
        #[cfg(all(feature = "std", not(target_arch = "riscv32")))]
        #[::raster::core::linkme::distributed_slice(::raster::core::registry::SEQUENCE_REGISTRY)]
        #[linkme(crate = ::raster::core::linkme)]
        static #registration_name: ::raster::core::registry::SequenceRegistration =
            ::raster::core::registry::SequenceRegistration::new(
                ::raster::core::registry::SequenceMetadataStatic::new(
                    #fn_name_str,
                    #fn_name_str,
                    #description_expr,
                ),
                &__RASTER_SEQUENCE_TILES,
            );
    };

    TokenStream::from(expanded)
}
