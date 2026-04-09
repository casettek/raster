//! Procedural macros for the Raster toolchain.
//!
//! This crate provides:
//! - `#[tile]` - Marks a function as a tile and registers it in the global registry
//! - `#[sequence]` - Declares tile ordering and control flow. When the function is named `main`,
//!   it is the program entry point and gets init, `--input` parsing, and finish automatically.

use proc_macro::TokenStream;
use quote::{format_ident, quote, ToTokens};
use syn::{parse_macro_input, Attribute, FnArg, ItemFn, Pat, ReturnType, Type};

#[derive(Clone)]
struct ParamInfo {
    ident: syn::Ident,
    ty: Type,
    external_name: Option<String>,
}

fn extract_inputs(input: &ItemFn) -> Vec<ParamInfo> {
    input
        .sig
        .inputs
        .iter()
        .filter_map(|arg| match arg {
            FnArg::Typed(pat_type) => match &*pat_type.pat {
                Pat::Ident(pat_ident) => Some(ParamInfo {
                    ident: pat_ident.ident.clone(),
                    ty: (*pat_type.ty).clone(),
                    external_name: parse_external_attr(&pat_type.attrs),
                }),
                _ => None,
            },
            FnArg::Receiver(_) => None,
        })
        .collect()
}

fn parse_external_attr(attrs: &[Attribute]) -> Option<String> {
    attrs.iter().find_map(|attr| {
        if !attr.path().is_ident("external") {
            return None;
        }

        let mut external_name = None;
        let _ = attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("name") {
                let value = meta.value()?;
                let lit: syn::LitStr = value.parse()?;
                external_name = Some(lit.value());
            }
            Ok(())
        });

        external_name
    })
}

fn filter_external_attrs(attrs: &[Attribute]) -> Vec<Attribute> {
    attrs
        .iter()
        .filter(|attr| !attr.path().is_ident("external"))
        .cloned()
        .collect()
}

fn external_param_ty(ty: &Type) -> Type {
    syn::parse2(quote! { ::raster::External<#ty> }).expect("external wrapper type should parse")
}

fn extract_external_payload_ty(ty: &Type) -> Option<Type> {
    let Type::Path(type_path) = ty else {
        return None;
    };
    let segment = type_path.path.segments.last()?;
    if segment.ident != "External" {
        return None;
    }
    let syn::PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    let first = args.args.first()?;
    let syn::GenericArgument::Type(inner_ty) = first else {
        return None;
    };
    Some(inner_ty.clone())
}

fn exposed_external_param_ty(ty: &Type) -> Type {
    extract_external_payload_ty(ty)
        .map_or_else(|| external_param_ty(ty), |inner| external_param_ty(&inner))
}

fn resolved_external_param_ty(ty: &Type) -> Type {
    extract_external_payload_ty(ty).unwrap_or_else(|| ty.clone())
}

fn rewrite_exposed_external_inputs(sig: &mut syn::Signature, params: &[ParamInfo]) {
    for arg in sig.inputs.iter_mut() {
        if let FnArg::Typed(pat_type) = arg {
            pat_type.attrs = filter_external_attrs(&pat_type.attrs);
        }
    }

    for param in params {
        if param.external_name.is_some() {
            for arg in sig.inputs.iter_mut() {
                if let FnArg::Typed(pat_type) = arg {
                    if let Pat::Ident(pat_ident) = &*pat_type.pat {
                        if pat_ident.ident == param.ident {
                            pat_type.ty = Box::new(exposed_external_param_ty(&param.ty));
                        }
                    }
                }
            }
        }
    }
}

fn rewrite_resolved_external_inputs(sig: &mut syn::Signature, params: &[ParamInfo]) {
    for arg in sig.inputs.iter_mut() {
        if let FnArg::Typed(pat_type) = arg {
            pat_type.attrs = filter_external_attrs(&pat_type.attrs);
        }
    }

    for param in params {
        if param.external_name.is_some() {
            for arg in sig.inputs.iter_mut() {
                if let FnArg::Typed(pat_type) = arg {
                    if let Pat::Ident(pat_ident) = &*pat_type.pat {
                        if pat_ident.ident == param.ident {
                            pat_type.ty = Box::new(resolved_external_param_ty(&param.ty));
                        }
                    }
                }
            }
        }
    }
}

fn external_hash_ident(param: &ParamInfo) -> syn::Ident {
    format_ident!("__raster_external_hash_{}", param.ident)
}

fn gen_external_resolution(input: &ItemFn) -> proc_macro2::TokenStream {
    let params = extract_inputs(input);
    let is_result = returns_result(input);
    let resolutions: Vec<_> = params
        .iter()
        .filter_map(|param| {
            let external_name = param.external_name.as_ref()?;
            let name = &param.ident;
            let resolved_ty = resolved_external_param_ty(&param.ty);
            let hash_ident = external_hash_ident(param);
            let resolve = if is_result {
                quote! {
                    ::raster::resolve_external_value::<#resolved_ty>(#name, #external_name)?
                }
            } else {
                quote! {
                    ::raster::resolve_external_value::<#resolved_ty>(#name, #external_name)
                        .unwrap_or_else(|e| panic!("Failed to resolve external input '{}': {}", #external_name, e))
                }
            };
            Some(quote! {
                let __raster_external_value = #resolve;
                let #hash_ident = __raster_external_value.data_hash.clone();
                let #name: #resolved_ty = __raster_external_value.into_inner();
            })
        })
        .collect();

    quote! {
        #(#resolutions)*
    }
}

fn gen_wrapper_external_unpack(input: &ItemFn) -> proc_macro2::TokenStream {
    let unpack_steps: Vec<_> = extract_inputs(input)
        .into_iter()
        .filter_map(|param| {
            let external_name = param.external_name?;
            let name = param.ident;
            let ty = resolved_external_param_ty(&param.ty);
            let _ = external_name;
            Some(quote! {
                let #name: #ty = #name.into_inner();
            })
        })
        .collect();

    quote! {
        #(#unpack_steps)*
    }
}

/// Check if the function returns a Result type.
fn returns_result(input: &ItemFn) -> bool {
    matches!(&input.sig.output, ReturnType::Type(_, ty) if {
        let ty_str = ty.to_token_stream().to_string();
        ty_str.starts_with("Result") || ty_str.contains(":: Result")
    })
}

/// Generate only the input deserialization code.
///
/// Returns a TokenStream that deserializes input bytes into the appropriate variables.
fn gen_inputs_deserialization(input: &ItemFn) -> proc_macro2::TokenStream {
    let params = extract_inputs(input);
    if params.is_empty() {
        // No arguments - no deserialization needed
        quote! {}
    } else if params.len() == 1 {
        // Single argument - deserialize directly
        let param = &params[0];
        let decode_ty = if let Some(_external_name) = &param.external_name {
            let ty = resolved_external_param_ty(&param.ty);
            quote! {
                ::raster::core::external::ExternalValue<#ty>
            }
        } else {
            let ty = &param.ty;
            quote! { #ty }
        };
        let name = &param.ident;
        quote! {
            let #name: #decode_ty = ::raster::core::postcard::from_bytes(input)
                .map_err(|e| ::raster::core::Error::Serialization(::raster::alloc::format!("Failed to deserialize input: {}", e)))?;
        }
    } else {
        // Multiple arguments - deserialize as tuple
        let decode_types: Vec<_> = params
            .iter()
            .map(|param| {
                if param.external_name.is_some() {
                    let ty = resolved_external_param_ty(&param.ty);
                    quote! { ::raster::core::external::ExternalValue<#ty> }
                } else {
                    let ty = &param.ty;
                    quote! { #ty }
                }
            })
            .collect();
        let names: Vec<_> = params.iter().map(|param| &param.ident).collect();
        let tuple_type = quote! { (#(#decode_types),*) };
        quote! {
            let (#(#names),*): #tuple_type = ::raster::core::postcard::from_bytes(input)
                .map_err(|e| ::raster::core::Error::Serialization(::raster::alloc::format!("Failed to deserialize input: {}", e)))?;
        }
    }
}

/// Add output serialization to the code pipeline.
///
/// Wraps the provided code with serialization of `result` to `output`.
fn gen_output_serialization(_input: &ItemFn) -> proc_macro2::TokenStream {
    quote! {
        let output = ::raster::core::postcard::to_allocvec(&result)
            .map_err(|e| ::raster::core::Error::Serialization(::raster::alloc::format!("Failed to serialize output: {}", e)))?;
    }
}

/// Generate input serialization code for tracing in the original function.
///
/// This serializes the typed input parameters to bytes for the trace emission.
fn gen_input_serialization(input: &ItemFn) -> proc_macro2::TokenStream {
    let params = extract_inputs(input);
    let has_external_inputs = params.iter().any(|param| param.external_name.is_some());

    let input_arg_defs: Vec<_> = params
        .iter()
        .map(|param| {
            let name = &param.ident;
            let name_str = name.to_string();
            let ty = if param.external_name.is_some() {
                resolved_external_param_ty(&param.ty)
            } else {
                param.ty.clone()
            };
            let ty_str = ty.to_token_stream().to_string();
            quote! {
                ::raster::core::trace::FnInputArg {
                    name: ::raster::alloc::string::String::from(#name_str),
                    ty: ::raster::alloc::string::String::from(#ty_str),
                }
            }
        })
        .collect();

    let external_meta_entries: Vec<_> = params
        .iter()
        .filter_map(|param| {
            let external_name = param.external_name.as_ref()?;
            let name_str = param.ident.to_string();
            let hash_ident = external_hash_ident(param);
            Some(quote! {
                (
                    ::raster::alloc::string::String::from(#name_str),
                    ::raster::core::trace::ExternalBindingMeta {
                        name: ::raster::alloc::string::String::from(#external_name),
                        data_commitment: #hash_ident
                            .clone()
                            .map(|value| value.into_bytes())
                            .unwrap_or_default(),
                    }
                )
            })
        })
        .collect();

    let input_bytes = if params.is_empty() {
        quote! {
            let __raster_input_bytes: ::raster::alloc::vec::Vec<u8> = ::raster::alloc::vec::Vec::new();
        }
    } else if params.len() == 1 {
        let param = &params[0];
        let name = &param.ident;
        if let Some(external_name) = &param.external_name {
            let hash_ident = external_hash_ident(param);
            quote! {
                let __raster_external_payload = ::raster::core::external::ExternalValue::new(
                    #external_name,
                    #hash_ident.clone(),
                    &#name,
                );
                let __raster_input_bytes = ::raster::core::postcard::to_allocvec(&__raster_external_payload)
                    .unwrap_or_default();
            }
        } else {
            quote! {
                let __raster_input_bytes = ::raster::core::postcard::to_allocvec(&#name)
                    .unwrap_or_default();
            }
        }
    } else {
        let payloads: Vec<_> = params
            .iter()
            .map(|param| {
                let name = &param.ident;
                if let Some(external_name) = &param.external_name {
                    let hash_ident = external_hash_ident(param);
                    quote! {
                        ::raster::core::external::ExternalValue::new(
                            #external_name,
                            #hash_ident.clone(),
                            &#name,
                        )
                    }
                } else {
                    quote! { &#name }
                }
            })
            .collect();
        quote! {
            let __raster_input_bytes = ::raster::core::postcard::to_allocvec(&(#(#payloads),*))
                .unwrap_or_default();
        }
    };

    quote! {
        let __raster_input_args: ::raster::alloc::vec::Vec<::raster::core::trace::FnInputArg> = ::raster::alloc::vec![
            #(#input_arg_defs),*
        ];

        #input_bytes

        let __raster_input = ::core::option::Option::Some(
            ::raster::core::trace::FnInput {
                data: __raster_input_bytes,
                args: __raster_input_args,
                external: if #has_external_inputs {
                    [#(#external_meta_entries),*]
                        .into_iter()
                        .collect::<::raster::alloc::collections::BTreeMap<
                            ::raster::alloc::string::String,
                            ::raster::core::trace::ExternalBindingMeta,
                        >>()
                } else {
                    ::raster::alloc::collections::BTreeMap::new()
                },
            }
        );
    }
}

/// Generate only the function call code.
///
/// Returns a TokenStream that calls the function and stores the result.
fn gen_function_call(target_fn: &syn::Ident, input: &ItemFn) -> proc_macro2::TokenStream {
    let input_names: Vec<_> = extract_inputs(input)
        .into_iter()
        .map(|param| param.ident)
        .collect();
    let is_result = returns_result(input);

    if input_names.is_empty() {
        if is_result {
            quote! { let result = #target_fn()?; }
        } else {
            quote! { let result = #target_fn(); }
        }
    } else {
        if is_result {
            quote! { let result = #target_fn(#(#input_names),*)?; }
        } else {
            quote! { let result = #target_fn(#(#input_names),*); }
        }
    }
}

/// Parses tile attributes from the macro invocation.
///
/// Uses named argument `kind` for tile type: `#[tile(kind = iter)]` or `#[tile(kind = recur)]`.
struct TileAttrs {
    /// Tile type: "iter" (default) or "recur".
    tile_type: String,
    estimated_cycles: Option<u64>,
    max_memory: Option<u64>,
    description: Option<String>,
}

impl TileAttrs {
    fn parse(attr: TokenStream) -> Self {
        let mut attrs = TileAttrs {
            tile_type: "iter".to_string(), // default to iter
            estimated_cycles: None,
            max_memory: None,
            description: None,
        };

        if attr.is_empty() {
            return attrs; // Default to "iter" if no arguments
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
                    "kind" => match value {
                        "iter" | "recur" => attrs.tile_type = value.to_string(),
                        _ => panic!("Unknown tile kind '{}'. Valid kinds: iter, recur", value),
                    },
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
/// 1. Injects tracing code into the original function (for std targets)
/// 2. Generates an ABI wrapper that handles bincode serialization/deserialization
/// 3. Registers the tile in the global `TILE_REGISTRY` distributed slice
///
/// # Attributes
/// - `kind = iter` - Standard iterative tile (default if not specified)
/// - `kind = recur` - Recursive tile for stateful computations
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
/// #[tile(kind = iter, estimated_cycles = 1000, description = "Greets a user")]
/// fn greet(name: String) -> String {
///     format!("Hello, {}!", name)
/// }
///
/// #[tile(kind = recur)]
/// fn iterate(state: State) -> State {
///     // recursive computation
/// }
/// ```
#[proc_macro_attribute]
pub fn tile(attr: TokenStream, item: TokenStream) -> TokenStream {
    let input_fn = parse_macro_input!(item as ItemFn);
    let fn_name_str = input_fn.sig.ident.to_string();
    let params = extract_inputs(&input_fn);

    let fn_name = &input_fn.sig.ident;
    let fn_vis = &input_fn.vis;
    let fn_attrs = &input_fn.attrs;
    let fn_body = &input_fn.block;

    let function_wrapper_name = format_ident!("__raster_tile_entry_{}", fn_name_str);
    let implementation_name = format_ident!("__raster_tile_impl_{}", fn_name_str);

    let attrs = TileAttrs::parse(attr);

    // Generate optional metadata fields
    let _estimated_cycles_expr = match attrs.estimated_cycles {
        Some(cycles) => quote! { ::core::option::Option::Some(#cycles) },
        None => quote! { ::core::option::Option::None },
    };

    let _max_memory_expr = match attrs.max_memory {
        Some(memory) => quote! { ::core::option::Option::Some(#memory) },
        None => quote! { ::core::option::Option::None },
    };

    // Generate deserialization and function call
    let inputs_deserialization = gen_inputs_deserialization(&input_fn);
    let function_call = gen_function_call(&implementation_name, &input_fn);
    let output_serialization = gen_output_serialization(&input_fn);
    let external_resolution = gen_external_resolution(&input_fn);
    let wrapper_external_unpack = gen_wrapper_external_unpack(&input_fn);

    let mut exposed_sig = input_fn.sig.clone();
    rewrite_exposed_external_inputs(&mut exposed_sig, &params);

    let mut implementation_sig = input_fn.sig.clone();
    implementation_sig.ident = implementation_name.clone();
    rewrite_resolved_external_inputs(&mut implementation_sig, &params);

    // For recursive tiles, also generate a macro with the same name that allows `tile_name!(args)` syntax
    let _recursive_macro = if attrs.tile_type == "recur" {
        let macro_name = format_ident!("{}", fn_name);
        quote! {
            /// Macro wrapper for recursive tile invocation.
            /// Use `tile_name!(args)` to invoke this recursive tile.
            /// For native execution, this simply calls the underlying function.
            /// The `!` syntax signals to the CFS compiler that this tile should
            /// be executed recursively until its first output returns true.
            #[macro_export]
            macro_rules! #macro_name {
                ($($args:expr),* $(,)?) => {
                    #fn_name($($args),*)
                };
            }
        }
    } else {
        quote! {}
    };

    // Generate output type expression
    let output_type_expr = match &input_fn.sig.output {
        ReturnType::Default => quote! { "()" },
        ReturnType::Type(_, ty) => {
            let ty_str = ty.to_token_stream().to_string();
            quote! { #ty_str }
        }
    };

    // Generate input serialization code for tracing
    let input_serialization = gen_input_serialization(&input_fn);

    let implementation_function = quote! {
        #implementation_sig #fn_body
    };

    let original_function = quote! {
        #(#fn_attrs)*
        #fn_vis #exposed_sig {
            #external_resolution

            // On std + non-riscv32: wrap body in closure for tracing
            #[cfg(all(feature = "std", not(target_arch = "riscv32")))]
            {
                // Serialize inputs for tracing
                #input_serialization

                #function_call

                // Serialize output and emit TraceEvent::TileExec
                let __raster_output_bytes = ::raster::core::postcard::to_allocvec(&result)
                    .unwrap_or_default();

                let __raster_output = ::core::option::Option::Some(
                    ::raster::core::trace::FnOutput {
                        data: __raster_output_bytes,
                        ty: ::alloc::string::String::from(#output_type_expr),
                    }
                );

                let __raster_record = ::raster::core::trace::FnCallRecord {
                    fn_name: ::alloc::string::String::from(#fn_name_str),
                    input: __raster_input,
                    output: __raster_output,
                };
                ::raster::publish_trace_event(::raster::core::trace::TraceEvent::TileExec(
                    __raster_record,
                ));

                return result;
            }

            // On riscv32 or no-std: just execute original body directly
            #[cfg(not(all(feature = "std", not(target_arch = "riscv32"))))]
            {
                #function_call
                result
            }
        }
    };

    TokenStream::from(quote! {
        #implementation_function

        // Original function with tracing injected
        #original_function

        // Generate the ABI wrapper function (available on all platforms, no_std compatible)
        pub fn #function_wrapper_name(input: &[u8]) -> ::raster::core::Result<::raster::alloc::vec::Vec<u8>> {
            #inputs_deserialization
            #wrapper_external_unpack

            #function_call

            #output_serialization

            Ok(output)
        }
    })
}

/// Parses optional sequence attributes from the macro invocation.
struct SequenceAttrs {
    description: Option<String>,
}

impl SequenceAttrs {
    fn parse(attr: TokenStream) -> Self {
        let mut attrs = SequenceAttrs { description: None };

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

/// Generates the sequence-wrapped body: either tracing (SequenceStart/body/SequenceEnd) or plain body depending on cfg.
fn gen_sequence_wrapped_body(fn_name_str: &str, item_fn: &ItemFn) -> proc_macro2::TokenStream {
    let body = &item_fn.block;
    let external_resolution = gen_external_resolution(item_fn);
    let input_serialization = gen_input_serialization(&item_fn);

    let output_type_expr = match &item_fn.sig.output {
        ReturnType::Default => quote! { "()" },
        ReturnType::Type(_, ty) => {
            let ty_str = ty.to_token_stream().to_string();
            quote! { #ty_str }
        }
    };

    quote! {
        #external_resolution

        #[cfg(all(feature = "std", not(target_arch = "riscv32")))]
        {
            #input_serialization

            let mut __raster_record = ::raster::core::trace::FnCallRecord {
                fn_name: ::raster::alloc::string::String::from(#fn_name_str),
                input: __raster_input,
                output: ::core::option::Option::None,
            };
            ::raster::publish_trace_event(::raster::core::trace::TraceEvent::SequenceStart(
                __raster_record.clone(),
            ));
            let __raster_result = (|| #body)();
            let __raster_output_bytes = ::raster::core::postcard::to_allocvec(&__raster_result)
                .unwrap_or_default();
            __raster_record.output = ::core::option::Option::Some(::raster::core::trace::FnOutput::new(
                __raster_output_bytes,
                ::raster::alloc::string::String::from(#output_type_expr),
            ));
            ::raster::publish_trace_event(::raster::core::trace::TraceEvent::SequenceEnd(
                __raster_record,
            ));
            __raster_result
        }

        #[cfg(not(all(feature = "std", not(target_arch = "riscv32"))))]
        #body
    }
}

/// Declares a sequence of tiles with linear control flow.
///
/// The `#[sequence]` macro parses the function body to extract tile calls
/// in the order they appear. The function remains callable for native execution,
/// and the sequence is registered for use with `cargo raster preview`.
///
/// When the function is named **`main`**, it is the program entry point: the macro expands to
/// `fn main() { init(); input_parsing; sequence_wrapped_body; finish(); }`. The `--input` CLI
/// argument is parsed into the original function's parameters.
///
/// # Attributes
/// - `description = "..."` - Human-readable description of the sequence
///
/// # Example (entry point)
/// ```ignore
/// #[raster::sequence]
/// fn main(name: String) {
///     let result = greet_sequence(name);
///     println!("{}", result);
/// }
/// ```
///
/// # Example (nested sequence)
/// ```ignore
/// #[sequence]
/// fn greet_sequence(name: String) -> String {
///     exclaim(greet(name))
/// }
/// ```
#[proc_macro_attribute]
pub fn sequence(attr: TokenStream, item: TokenStream) -> TokenStream {
    let item_fn = parse_macro_input!(item as ItemFn);
    let fn_name_str = item_fn.sig.ident.to_string();
    let params = extract_inputs(&item_fn);

    let fn_vis = &item_fn.vis;
    let fn_attrs = &item_fn.attrs;
    let _attrs = SequenceAttrs::parse(attr);
    let mut sequence_sig = item_fn.sig.clone();
    rewrite_exposed_external_inputs(&mut sequence_sig, &params);

    let expanded = if item_fn.sig.ident == "main" {
        // Entry point: replace with fn main() { init(); input_parsing; wrapped_body; finish(); }
        let input_parsing: Vec<_> = params
            .iter()
            .map(|param| {
                let name = &param.ident;
                let ty = &param.ty;
                let field_name = name.to_string();
                if let Some(external_name) = &param.external_name {
                    let ty = exposed_external_param_ty(ty);
                    quote! {
                        let #name: #ty = ::raster::external(#external_name);
                    }
                } else {
                    quote! {
                        let #name: #ty = ::raster::parse_main_input_value(::core::option::Option::Some(#field_name))
                            .expect("Failed to parse --input argument. Use inline JSON or a JSON file path.");
                    }
                }
            })
            .collect();
        let body = gen_sequence_wrapped_body("main", &item_fn);
        quote! {
            #(#fn_attrs)*
            fn main() {
                ::raster::init();

                #(#input_parsing)*

                #body

                ::raster::finish();
            }
        }
    } else {
        // Normal sequence: keep signature, wrap body
        let body = gen_sequence_wrapped_body(&fn_name_str, &item_fn);
        quote! {
            #(#fn_attrs)*
            #fn_vis #sequence_sig {
                #body
            }
        }
    };

    TokenStream::from(expanded)
}
