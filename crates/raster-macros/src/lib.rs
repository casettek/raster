//! Procedural macros for the Raster toolchain.
//!
//! This crate provides:
//! - `#[tile]` - Marks a function as a tile and registers it in the global registry
//! - `#[sequence]` - Declares tile ordering and control flow

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{parse_macro_input, FnArg, ItemFn, Pat, ReturnType, Type};

/// Parses optional tile attributes from the macro invocation.
struct TileAttrs {
    estimated_cycles: Option<u64>,
    max_memory: Option<u64>,
    description: Option<String>,
}

impl TileAttrs {
    fn parse(attr: TokenStream) -> Self {
        let mut attrs = TileAttrs {
            estimated_cycles: None,
            max_memory: None,
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
/// # Attributes
/// - `estimated_cycles = N` - Expected cycle count for resource estimation
/// - `max_memory = N` - Maximum memory usage in bytes
/// - `description = "..."` - Human-readable description
///
/// # Example
/// ```ignore
/// #[tile]
/// fn compute(input: u64) -> u64 {
///     input * 2
/// }
///
/// #[tile(estimated_cycles = 1000, description = "Greets a user")]
/// fn greet(name: String) -> String {
///     format!("Hello, {}!", name)
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
                let #name: #ty = ::raster::core::bincode::deserialize(input)
                    .map_err(|e| ::raster::core::Error::Other(format!("Failed to deserialize input: {}", e)))?;
                let result = #fn_name(#name)?;
            }
        } else {
            quote! {
                let #name: #ty = ::raster::core::bincode::deserialize(input)
                    .map_err(|e| ::raster::core::Error::Other(format!("Failed to deserialize input: {}", e)))?;
                let result = #fn_name(#name);
            }
        }
    } else {
        // Multiple arguments - deserialize as tuple
        let tuple_type = quote! { (#(#input_types),*) };
        if returns_result {
            quote! {
                let (#(#input_names),*): #tuple_type = ::raster::core::bincode::deserialize(input)
                    .map_err(|e| ::raster::core::Error::Other(format!("Failed to deserialize input: {}", e)))?;
                let result = #fn_name(#(#input_names),*)?;
            }
        } else {
            quote! {
                let (#(#input_names),*): #tuple_type = ::raster::core::bincode::deserialize(input)
                    .map_err(|e| ::raster::core::Error::Other(format!("Failed to deserialize input: {}", e)))?;
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

        // Generate the ABI wrapper function
        fn #wrapper_name(input: &[u8]) -> ::raster::core::Result<::std::vec::Vec<u8>> {
            #deserialize_and_call

            ::raster::core::bincode::serialize(&result)
                .map_err(|e| ::raster::core::Error::Other(format!("Failed to serialize output: {}", e)))
        }

        // Register the tile in the distributed slice
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

/// Declares a sequence of tiles with control flow.
///
/// # Example
/// ```ignore
/// #[sequence]
/// fn my_sequence() {
///     tile_a();
///     tile_b();
/// }
/// ```
#[proc_macro_attribute]
pub fn sequence(_attr: TokenStream, item: TokenStream) -> TokenStream {
    // TODO: Implement sequence macro
    // - Parse control flow
    // - Generate schema
    item
}
