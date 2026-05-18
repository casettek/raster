//! Procedural macros for the Raster toolchain.
//!
//! This crate provides:
//! - `#[tile]` - Marks a function as a tile and registers it in the global registry
//! - `#[sequence]` - Declares tile ordering and control flow. When the function is named `main`,
//!   it is the program entry point and gets init and finish automatically.

use proc_macro::TokenStream;
use quote::{format_ident, quote, ToTokens};
use syn::{
    parse::{Parse, ParseStream},
    parse_macro_input, Attribute, Expr, ExprField, ExprIndex, FnArg, ItemFn, LitInt, Pat,
    ReturnType, Token, Type,
};

#[derive(Clone)]
struct ParamInfo {
    ident: syn::Ident,
    ty: Type,
}

fn extract_params(input: &ItemFn) -> Vec<ParamInfo> {
    input
        .sig
        .inputs
        .iter()
        .filter_map(|arg| match arg {
            FnArg::Typed(pat_type) => match &*pat_type.pat {
                Pat::Ident(pat_ident) => Some(ParamInfo {
                    ident: pat_ident.ident.clone(),
                    ty: (*pat_type.ty).clone(),
                }),
                _ => None,
            },
            FnArg::Receiver(_) => None,
        })
        .collect()
}

fn parse_schema_tag(attrs: &[Attribute]) -> Option<u32> {
    attrs.iter().find_map(|attr| {
        if !attr.path().is_ident("schema") {
            return None;
        }

        let mut tag = None;
        let _ = attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("tag") {
                let value = meta.value()?;
                let lit: syn::LitInt = value.parse()?;
                tag = lit.base10_parse().ok();
            }
            Ok(())
        });

        tag
    })
}

fn rewrite_into_resolved_args(sig: &mut syn::Signature) {
    for arg in sig.inputs.iter_mut() {
        if let FnArg::Typed(pat_type) = arg {
            let ty = &pat_type.ty;
            pat_type.ty = syn::parse_quote!(impl ::raster::IntoResolvedArg<#ty>);
        }
    }
}

fn external_info_ident(param: &ParamInfo) -> syn::Ident {
    format_ident!("__raster_external_info_{}", param.ident)
}

fn gen_arg_resolution(input: &ItemFn) -> proc_macro2::TokenStream {
    let params = extract_params(input);
    let is_result = returns_result(input);
    let resolutions: Vec<_> = params
        .iter()
        .map(|param| {
            let name = &param.ident;
            let resolved_ty = &param.ty;
            let external_info_ident = external_info_ident(param);
            let resolve = if is_result {
                quote! {
                    ::raster::into_resolved_arg::<#resolved_ty, _>(#name)?
                }
            } else {
                quote! {
                    ::raster::into_resolved_arg::<#resolved_ty, _>(#name)
                        .unwrap_or_else(|e| panic!("Failed to resolve call argument '{}': {}", stringify!(#name), e))
                }
            };
            quote! {
                let __raster_resolved_arg = #resolve;
                let #external_info_ident = __raster_resolved_arg.as_external().cloned();
                let #name: #resolved_ty = __raster_resolved_arg.into_inner();
            }
        })
        .collect();

    quote! {
        #(#resolutions)*
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
    let params = extract_params(input);
    if params.is_empty() {
        // No arguments - no deserialization needed
        quote! {}
    } else if params.len() == 1 {
        // Single argument - deserialize directly
        let param = &params[0];
        let decode_ty = &param.ty;
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
                let ty = &param.ty;
                quote! { #ty }
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
    let params = extract_params(input);

    let input_arg_defs: Vec<_> = params
        .iter()
        .map(|param| {
            let name = &param.ident;
            let name_str = name.to_string();
            let ty = &param.ty;
            let ty_str = ty.to_token_stream().to_string();
            quote! {
                ::raster::core::trace::FnInputArg {
                    name: ::raster::alloc::string::String::from(#name_str),
                    ty: ::raster::alloc::string::String::from(#ty_str),
                }
            }
        })
        .collect();

    let external_binding_entries: Vec<_> = params
        .iter()
        .map(|param| {
            let name_str = param.ident.to_string();
            let external_info_ident = external_info_ident(param);
            quote! {
                if let ::core::option::Option::Some(__raster_external_info) = #external_info_ident.clone() {
                    __raster_external.insert(
                        ::raster::alloc::string::String::from(#name_str),
                        ::raster::core::trace::ExternalData {
                            name: __raster_external_info.name,
                            commitment: __raster_external_info
                                .commitment
                                .map(|value| value.into_bytes())
                                .unwrap_or_default(),
                            selector: __raster_external_info.selector,
                            selected: __raster_external_info.selected,
                        }
                    );
                }
            }
        })
        .collect();

    let input_bytes = if params.is_empty() {
        quote! {
            let __raster_input_bytes: ::raster::alloc::vec::Vec<u8> = ::raster::alloc::vec::Vec::new();
        }
    } else if params.len() == 1 {
        let param = &params[0];
        let name = &param.ident;
        quote! {
            let __raster_input_bytes = ::raster::core::postcard::to_allocvec(&#name)
                .unwrap_or_default();
        }
    } else {
        let payloads: Vec<_> = params
            .iter()
            .map(|param| {
                let name = &param.ident;
                quote! { &#name }
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

        let mut __raster_external = ::raster::alloc::collections::BTreeMap::new();
        #(#external_binding_entries)*

        let __raster_input = ::core::option::Option::Some(
            ::raster::core::trace::FnInput {
                data: __raster_input_bytes,
                args: __raster_input_args,
                external: __raster_external,
            }
        );
    }
}

/// Generate only the function call code.
///
/// Returns a TokenStream that calls the function and stores the result.
fn gen_function_call(target_fn: &syn::Ident, input: &ItemFn) -> proc_macro2::TokenStream {
    let param_names: Vec<syn::Ident> = extract_params(input)
        .into_iter()
        .map(|param| param.ident)
        .collect();
    let is_result = returns_result(input);

    if param_names.is_empty() {
        if is_result {
            quote! { let result = #target_fn()?; }
        } else {
            quote! { let result = #target_fn(); }
        }
    } else {
        if is_result {
            quote! { let result = #target_fn(#(#param_names),*)?; }
        } else {
            quote! { let result = #target_fn(#(#param_names),*); }
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
    let arg_resolution = gen_arg_resolution(&input_fn);

    let mut exposed_sig = input_fn.sig.clone();
    rewrite_into_resolved_args(&mut exposed_sig);

    let mut implementation_sig = input_fn.sig.clone();
    implementation_sig.ident = implementation_name.clone();

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
            #arg_resolution

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
    let arg_resolution = gen_arg_resolution(item_fn);
    let input_serialization = gen_input_serialization(&item_fn);

    let output_type_expr = match &item_fn.sig.output {
        ReturnType::Default => quote! { "()" },
        ReturnType::Type(_, ty) => {
            let ty_str = ty.to_token_stream().to_string();
            quote! { #ty_str }
        }
    };

    quote! {
        #arg_resolution

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
/// `fn main() { init(); sequence_wrapped_body; finish(); }`.
///
/// # Attributes
/// - `description = "..."` - Human-readable description of the sequence
///
/// # Example (entry point)
/// ```ignore
/// #[raster::sequence]
/// fn main() {
///     let name = raster::select!(String, raster::external!(String, "name"));
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
    let params = extract_params(&item_fn);

    let fn_vis = &item_fn.vis;
    let fn_attrs = &item_fn.attrs;
    let _attrs = SequenceAttrs::parse(attr);

    let mut sequence_sig = item_fn.sig.clone();
    rewrite_into_resolved_args(&mut sequence_sig);

    let expanded = if item_fn.sig.ident == "main" {
        if !params.is_empty() {
            panic!(
                "`#[sequence] fn main` must not declare parameters. Bind committed inputs explicitly inside the body with external!(...)."
            );
        }

        // Entry point: replace with fn main() { init(); wrapped_body; finish(); }
        let body = gen_sequence_wrapped_body("main", &item_fn);
        quote! {
            #(#fn_attrs)*
            fn main() {
                ::raster::init();

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

fn split_selector_expr(expr: Expr) -> (Expr, Vec<proc_macro2::TokenStream>) {
    match expr {
        Expr::Field(ExprField { base, member, .. }) => {
            let (base_expr, mut segments) = split_selector_expr(*base);
            let segment = match member {
                syn::Member::Named(ident) => {
                    let name = ident.to_string();
                    quote! { ::raster::SelectorSegment::Field(::raster::alloc::string::String::from(#name)) }
                }
                syn::Member::Unnamed(index) => {
                    let value = index.index;
                    quote! { ::raster::SelectorSegment::Index(#value as u64) }
                }
            };
            segments.push(segment);
            (base_expr, segments)
        }
        Expr::Index(ExprIndex { expr, index, .. }) => {
            let (base_expr, mut segments) = split_selector_expr(*expr);
            let Expr::Lit(expr_lit) = *index else {
                panic!("select! only supports integer literal indexes");
            };
            let syn::Lit::Int(LitInt { .. }) = &expr_lit.lit else {
                panic!("select! only supports integer literal indexes");
            };
            let value = expr_lit.lit.to_token_stream();
            segments.push(quote! { ::raster::SelectorSegment::Index((#value) as u64) });
            (base_expr, segments)
        }
        other => (other, Vec::new()),
    }
}

struct SelectInput {
    selected_ty: Type,
    expr: Expr,
}

impl Parse for SelectInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        Ok(Self {
            selected_ty: input.parse()?,
            expr: {
                input.parse::<Token![,]>()?;
                input.parse()?
            },
        })
    }
}

#[proc_macro]
pub fn select(item: TokenStream) -> TokenStream {
    let SelectInput { selected_ty, expr } = parse_macro_input!(item as SelectInput);
    let (base_expr, segments) = split_selector_expr(expr);

    TokenStream::from(quote! {
        ::raster::select_source(
            #base_expr,
            ::raster::typed_selector_path::<_, #selected_ty>(
                ::raster::SelectorPath::new(::raster::alloc::vec![#(#segments),*]),
            ),
        )
    })
}

#[proc_macro_derive(Selectable, attributes(schema))]
pub fn derive_selectable(item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as syn::DeriveInput);
    let ident = &input.ident;
    let generics = input.generics.clone();
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    let fields = match &input.data {
        syn::Data::Struct(data) => match &data.fields {
            syn::Fields::Named(fields) => fields.named.iter().collect::<Vec<_>>(),
            _ => panic!("Selectable can only be derived for structs with named fields"),
        },
        _ => panic!("Selectable can only be derived for structs"),
    };

    let schema_fields: Vec<_> = fields
        .iter()
        .map(|field| {
            let field_ident = field.ident.as_ref().expect("named field");
            let field_ty = &field.ty;
            let field_name = field_ident.to_string();
            let label = parse_schema_tag(&field.attrs)
                .map(|tag| tag.to_string())
                .unwrap_or_else(|| field_name.clone());
            quote! {
                ::raster::core::input::SchemaField::new(
                    #field_name,
                    #label,
                    <#field_ty as ::raster::core::input::Selectable>::schema(),
                )
            }
        })
        .collect();

    TokenStream::from(quote! {
        impl #impl_generics ::raster::core::input::Selectable for #ident #ty_generics #where_clause {
            fn schema() -> ::raster::core::input::SchemaNode {
                ::raster::core::input::SchemaNode::Struct {
                    type_name: ::raster::alloc::string::String::from(stringify!(#ident)),
                    fields: ::raster::alloc::vec![#(#schema_fields),*],
                }
            }
        }
    })
}

#[proc_macro_derive(Merklized)]
pub fn derive_merklized(item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as syn::DeriveInput);
    let ident = &input.ident;
    let generics = input.generics.clone();
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    TokenStream::from(quote! {
        impl #impl_generics ::raster::core::input::Merklized for #ident #ty_generics #where_clause {}
    })
}
