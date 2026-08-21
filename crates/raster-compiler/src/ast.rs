use cargo_toml::Manifest;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use syn::{
    parse::{Parse, ParseStream},
    parse_file,
    visit::Visit,
    Attribute, Expr, ExprLit, ExprMacro, FnArg, Lit, Local, Meta, Pat, StmtMacro, Token,
};
use walkdir::WalkDir;

use raster_core::Result;

#[derive(Debug, Clone)]
pub struct ProjectAst {
    pub name: String,
    pub root_path: PathBuf,
    pub functions: Vec<FunctionAstItem>,
    /// Named structs in `src/`, used to fill `InterfaceDecl.schema_hash`.
    pub structs: Vec<StructAstItem>,
}

#[derive(Debug, Clone)]
pub struct StructAstItem {
    pub name: String,
    pub fields: Vec<StructFieldAst>,
}

#[derive(Debug, Clone)]
pub struct StructFieldAst {
    pub name: String,
    pub ty: String,
}

/// Indicates which canonical Raster call primitive was used.
///
/// Only canonical call primitives are recognized by the compiler. Bare function calls
/// in sequence bodies are not extracted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallKind {
    /// Invoked via `call!(tile_fn, args...)` — caller declares this is a tile step.
    Tile,
    /// Invoked via `call_seq!(seq_fn, args...)` — caller declares this is a sequence call.
    Sequence,
    /// Invoked via `call_recur!` — caller declares this is a recursive tile step.
    RecursiveTile,
    /// Invoked via `call_recur_seq!(sequence_fn, args...)` — caller declares this is a recursive sequence step.
    RecursiveSequence,
}

/// Where a call argument's value comes from, as far as syntax can tell.
///
/// The parser's job is only to find the *name* an argument's value flows
/// from; deciding what that name binds to (a sequence parameter, a prior
/// item's output, or a local) needs the resolver's tables, not the AST.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallArgumentKind {
    /// The value flows from `root`: a bare identifier, or any expression
    /// rooted at one — `x.field[0]`, `x.clone()`, `select!(T, x.field)`.
    Rooted { root: String },
    /// A literal, or an expression with no identifier at its root: the value
    /// is materialized in the sequence body itself.
    Inline,
}

/// Captures detailed information about a function call within a function body.
#[derive(Debug, Clone)]
pub struct CallInfo {
    /// The name of the function being called
    pub callee: String,
    /// The arguments passed to the function (as string representations)
    pub arguments: Vec<String>,
    /// Lightweight classification of each argument expression.
    pub argument_kinds: Vec<CallArgumentKind>,
    /// The variable name this call result is bound to, if any (e.g., `let x = foo()` -> Some("x"))
    pub result_binding: Option<String>,
    /// Which canonical call primitive produced this call.
    pub call_kind: CallKind,
    /// Static chunk size from `call_recur! { ..., chunk = N }`, if declared.
    pub chunk: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct FunctionAstItem {
    pub name: String,
    pub path: PathBuf,
    /// Detailed information about each function call in this function's body
    pub call_infos: Vec<CallInfo>,
    pub macros: Vec<MacroAstItem>,
    /// Parameter names of the function
    pub input_names: Vec<String>,
    /// Parameter types of the function
    pub inputs: Vec<String>,
    pub output: Option<String>,
    pub signature: String,
    /// `let name = select!(T, root.field)` locals, as `(name, root)`. A
    /// selection is a view of its source, so uses of `name` must bind to
    /// whatever `root` binds to.
    pub selection_aliases: Vec<(String, String)>,
    /// `let name = select!(T, rows[idx])` locals, as `(name, [idx, ..])` — the
    /// names supplying that selection's data-sourced indexes, in selector
    /// order. Empty for every literal-index selection, which is why programs
    /// that do not use the feature are unaffected.
    pub selection_index_sources: Vec<(String, Vec<String>)>,
}

#[derive(Debug, Clone)]
pub struct MacroAstItem {
    pub name: String,
    pub args: HashMap<String, String>,
}
// TODO: Project AST Explorer should contain function with full path resulution (like mod's)
impl ProjectAst {
    pub fn new(project_root: &Path) -> Result<Self> {
        let cargo_toml_path = project_root.join("Cargo.toml");

        let cargo_manifest = Manifest::from_path(&cargo_toml_path).unwrap();

        let package = cargo_manifest.package.as_ref().unwrap();

        let files_paths = Self::find_all_rs_files(project_root);

        let mut functions = Vec::new();
        let mut structs = Vec::new();
        for path in &files_paths {
            let content = std::fs::read_to_string(path).unwrap();
            let ast = parse_file(&content).unwrap();
            functions.extend(Self::parse_functions(&ast, path.to_path_buf()));
            structs.extend(crate::schema_walk::collect_structs(&ast.items));
        }

        Ok(Self {
            name: package.name.clone(),
            root_path: project_root.to_path_buf(),
            functions,
            structs,
        })
    }

    fn find_all_rs_files(project_root: &Path) -> Vec<PathBuf> {
        let src_dir = project_root.join("src");

        WalkDir::new(&src_dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "rs"))
            .filter(|e| e.path().file_name().unwrap_or_default() != "mod.rs")
            .map(|e| e.path().to_path_buf())
            .collect()
    }

    fn parse_functions(ast: &syn::File, path: PathBuf) -> Vec<FunctionAstItem> {
        let mut functions = Vec::new();

        for item in &ast.items {
            if let syn::Item::Fn(func) = item {
                let name = func.sig.ident.to_string();

                let macros: Vec<MacroAstItem> = func
                    .attrs
                    .iter()
                    .filter(|attr| !attr.path().is_ident("doc")) // Skip doc attributes
                    .map(Self::parse_macro)
                    .collect();

                let (input_names, inputs): (Vec<String>, Vec<String>) = func
                    .sig
                    .inputs
                    .iter()
                    .filter_map(|arg| {
                        if let FnArg::Typed(pat_type) = arg {
                            let ty = &pat_type.ty;
                            let name = Self::extract_param_name(&pat_type.pat);
                            Some((name, quote::quote!(#ty).to_string()))
                        } else {
                            None
                        }
                    })
                    .unzip();

                let output = if let syn::ReturnType::Type(_, ty) = &func.sig.output {
                    Some(quote::quote!(#ty).to_string())
                } else {
                    None
                };

                let sig = &func.sig;
                let signature = quote::quote!(#sig).to_string();

                let mut visitor = CallVisitor::new();
                visitor.visit_item_fn(func);
                let call_infos = visitor.get_call_infos();
                let selection_aliases = visitor.get_selection_aliases();
                let selection_index_sources = visitor.get_selection_index_sources();
                let function_info = FunctionAstItem {
                    name,
                    path: path.clone(),
                    call_infos,
                    macros,
                    input_names,
                    inputs,
                    output,
                    signature,
                    selection_aliases,
                    selection_index_sources,
                };
                functions.push(function_info);
            }
        }
        functions
    }

    fn parse_macro(attr: &Attribute) -> MacroAstItem {
        let name = attr
            .path()
            .segments
            .iter()
            .map(|seg| seg.ident.to_string())
            .collect::<Vec<_>>()
            .join("::");

        let mut args = HashMap::new();

        if let Meta::List(meta_list) = &attr.meta {
            let _ = meta_list.parse_nested_meta(|meta| {
                let key = meta
                    .path
                    .get_ident()
                    .map(|id| id.to_string())
                    .unwrap_or_default();

                let value: Expr = meta.value()?.parse()?;

                // Convert everything to String
                let value_str = match &value {
                    Expr::Lit(ExprLit {
                        lit: Lit::Str(s), ..
                    }) => s.value(),
                    Expr::Lit(ExprLit {
                        lit: Lit::Int(i), ..
                    }) => i.to_string(),
                    Expr::Lit(ExprLit {
                        lit: Lit::Bool(b), ..
                    }) => b.value.to_string(),
                    Expr::Lit(ExprLit {
                        lit: Lit::Float(f), ..
                    }) => f.to_string(),
                    _ => quote::quote!(#value).to_string(),
                };

                args.insert(key, value_str);
                Ok(())
            });
        }

        MacroAstItem { name, args }
    }

    /// Extracts the parameter name from a pattern
    fn extract_param_name(pat: &Pat) -> String {
        match pat {
            Pat::Ident(pat_ident) => pat_ident.ident.to_string(),
            Pat::Type(pat_type) => Self::extract_param_name(&pat_type.pat),
            Pat::Wild(_) => "_".to_string(),
            _ => quote::quote!(#pat).to_string(),
        }
    }
}

/// The body of a `select!(Type, expr)` — only `expr` matters here, since it
/// is what the selection reads from.
struct SelectionMacroArgs {
    expr: Expr,
}

impl Parse for SelectionMacroArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let _selected_ty: syn::Type = input.parse()?;
        input.parse::<Token![,]>()?;
        Ok(Self {
            expr: input.parse()?,
        })
    }
}

pub struct CallVisitor {
    call_infos: Vec<CallInfo>,
    /// Tracks the current let binding name when visiting let statements
    current_binding: Option<String>,
    /// `let name = select!(T, root.field)` — maps `name` to `root`.
    ///
    /// A selection's result carries its source's provenance, so a local
    /// bound to one is not a fresh value: it must resolve to whatever its
    /// root resolves to. Without this the resolver would see only an
    /// unknown identifier and fall back to treating committed data as if it
    /// were materialized in the body.
    selection_aliases: Vec<(String, String)>,
    /// Per selection local, the names supplying its data-sourced indexes.
    selection_index_sources: Vec<(String, Vec<String>)>,
}

impl CallVisitor {
    fn new() -> Self {
        Self {
            call_infos: Vec::new(),
            current_binding: None,
            selection_aliases: Vec::new(),
            selection_index_sources: Vec::new(),
        }
    }

    fn get_call_infos(&self) -> Vec<CallInfo> {
        self.call_infos.clone()
    }

    fn get_selection_aliases(&self) -> Vec<(String, String)> {
        self.selection_aliases.clone()
    }

    fn get_selection_index_sources(&self) -> Vec<(String, Vec<String>)> {
        self.selection_index_sources.clone()
    }

    /// Extracts the binding name from a pattern (e.g., `x` from `let x = ...`)
    fn extract_binding_name(pat: &Pat) -> Option<String> {
        match pat {
            Pat::Ident(pat_ident) => Some(pat_ident.ident.to_string()),
            Pat::Type(pat_type) => Self::extract_binding_name(&pat_type.pat),
            _ => None,
        }
    }

    /// Converts an expression to its string representation for argument capture
    fn expr_to_string(expr: &Expr) -> String {
        quote::quote!(#expr).to_string()
    }

    /// Parse a canonical call macro token stream into (callee, arguments).
    ///
    /// The macro syntax is `call!(callee_fn, arg1, arg2, ...)`. The token stream
    /// is a comma-separated list; the first token is the callee identifier.
    fn parse_call_macro_args(
        mac: &syn::Macro,
    ) -> Option<(String, Vec<String>, Vec<CallArgumentKind>, Option<u64>)> {
        if matches!(Self::macro_call_kind(mac), Some(CallKind::RecursiveTile)) {
            return Self::parse_recur_call_macro_args(mac);
        }
        if matches!(
            Self::macro_call_kind(mac),
            Some(CallKind::RecursiveSequence)
        ) {
            return Self::parse_recur_sequence_call_macro_args(mac)
                .map(|(callee, args, kinds)| (callee, args, kinds, None));
        }

        // Parse the macro tokens as a punctuated sequence of expressions.
        let args: syn::punctuated::Punctuated<Expr, syn::Token![,]> = mac
            .parse_body_with(syn::punctuated::Punctuated::parse_terminated)
            .ok()?;

        let mut iter = args.iter();

        // First argument must be a plain identifier (the callee function name).
        let callee_expr = iter.next()?;
        let callee = match callee_expr {
            Expr::Path(path) => path.path.get_ident()?.to_string(),
            _ => return None,
        };

        // Remaining arguments are the call arguments.
        let rest: Vec<&Expr> = iter.collect();
        let arguments: Vec<String> = rest.iter().map(|expr| Self::expr_to_string(expr)).collect();
        let argument_kinds: Vec<CallArgumentKind> = rest
            .iter()
            .map(|expr| Self::classify_argument(expr))
            .collect();

        Some((callee, arguments, argument_kinds, None))
    }

    fn parse_recur_call_macro_args(
        mac: &syn::Macro,
    ) -> Option<(String, Vec<String>, Vec<CallArgumentKind>, Option<u64>)> {
        struct RecurCallInput {
            tile: syn::Ident,
            input: Expr,
            // Chunk size is a call-site control parameter, not a tile input:
            // it groups iterations but binds no data source, so it is pinned
            // into the CFS item and excluded from the extracted arguments.
            chunk: Option<Expr>,
            state: Option<Expr>,
            output: Option<Expr>,
            args: syn::punctuated::Punctuated<Expr, Token![,]>,
        }

        fn parse_named_key(input: ParseStream, expected: &str) -> syn::Result<()> {
            let ident: syn::Ident = input.parse()?;
            if ident != expected {
                return Err(syn::Error::new(
                    ident.span(),
                    format!("expected `{}` key", expected),
                ));
            }
            input.parse::<Token![=]>()?;
            Ok(())
        }

        fn parse_optional_named_expr(
            input: ParseStream,
            expected: &str,
        ) -> syn::Result<Option<Expr>> {
            if !input.peek(syn::Ident) {
                return Ok(None);
            }
            let fork = input.fork();
            let ident: syn::Ident = fork.parse()?;
            if ident != expected {
                return Ok(None);
            }
            parse_named_key(input, expected)?;
            let expr: Expr = input.parse()?;
            input.parse::<Token![,]>()?;
            Ok(Some(expr))
        }

        impl Parse for RecurCallInput {
            fn parse(input: ParseStream) -> syn::Result<Self> {
                parse_named_key(input, "tile")?;
                let tile: syn::Ident = input.parse()?;
                input.parse::<Token![,]>()?;

                parse_named_key(input, "input")?;
                let input_expr: Expr = input.parse()?;
                input.parse::<Token![,]>()?;

                let chunk = parse_optional_named_expr(input, "chunk")?;
                let state = parse_optional_named_expr(input, "state")?;
                let output = parse_optional_named_expr(input, "output")?;

                parse_named_key(input, "args")?;
                let content;
                syn::parenthesized!(content in input);
                let args = syn::punctuated::Punctuated::parse_terminated(&content)?;
                let _ = input.parse::<Option<Token![,]>>()?;

                Ok(Self {
                    tile,
                    input: input_expr,
                    chunk,
                    state,
                    output,
                    args,
                })
            }
        }

        let parsed = syn::parse2::<RecurCallInput>(mac.tokens.clone()).ok()?;
        // The chunk size lands in the CFS, so it must be statically known:
        // only integer literals are accepted.
        let chunk = parsed.chunk.map(|expr| match &expr {
            Expr::Lit(expr_lit) => match &expr_lit.lit {
                syn::Lit::Int(int_lit) => int_lit
                    .base10_parse::<u64>()
                    .expect("call_recur! chunk literal does not fit in u64"),
                _ => panic!(
                    "call_recur! `chunk = ...` must be an integer literal so it can be pinned in the CFS"
                ),
            },
            _ => panic!(
                "call_recur! `chunk = ...` must be an integer literal so it can be pinned in the CFS"
            ),
        });
        let mut arguments = vec![Self::expr_to_string(&parsed.input)];
        let mut argument_kinds = vec![Self::classify_argument(&parsed.input)];

        if let Some(state) = parsed.state {
            arguments.push(Self::expr_to_string(&state));
            argument_kinds.push(Self::classify_argument(&state));
        }

        if let Some(output) = parsed.output {
            arguments.push(Self::expr_to_string(&output));
            argument_kinds.push(Self::classify_argument(&output));
        }

        for expr in parsed.args {
            arguments.push(Self::expr_to_string(&expr));
            argument_kinds.push(Self::classify_argument(&expr));
        }

        Some((parsed.tile.to_string(), arguments, argument_kinds, chunk))
    }

    fn parse_recur_sequence_call_macro_args(
        mac: &syn::Macro,
    ) -> Option<(String, Vec<String>, Vec<CallArgumentKind>)> {
        struct RecurSequenceCallInput {
            sequence: syn::Ident,
            input: Expr,
            state: Option<Expr>,
            output: Option<Expr>,
            args: syn::punctuated::Punctuated<Expr, Token![,]>,
        }

        impl Parse for RecurSequenceCallInput {
            fn parse(input: ParseStream) -> syn::Result<Self> {
                CallVisitor::parse_named_recur_key(input, "sequence")?;
                let sequence: syn::Ident = input.parse()?;
                input.parse::<Token![,]>()?;

                CallVisitor::parse_named_recur_key(input, "input")?;
                let input_expr: Expr = input.parse()?;
                input.parse::<Token![,]>()?;

                let state = if input.peek(syn::Ident) {
                    let fork = input.fork();
                    let ident: syn::Ident = fork.parse()?;
                    if ident == "state" {
                        CallVisitor::parse_named_recur_key(input, "state")?;
                        let state_expr: Expr = input.parse()?;
                        input.parse::<Token![,]>()?;
                        Some(state_expr)
                    } else {
                        None
                    }
                } else {
                    None
                };

                let output = if input.peek(syn::Ident) {
                    let fork = input.fork();
                    let ident: syn::Ident = fork.parse()?;
                    if ident == "output" {
                        CallVisitor::parse_named_recur_key(input, "output")?;
                        let output_expr: Expr = input.parse()?;
                        input.parse::<Token![,]>()?;
                        Some(output_expr)
                    } else {
                        None
                    }
                } else {
                    None
                };

                CallVisitor::parse_named_recur_key(input, "args")?;
                let content;
                syn::parenthesized!(content in input);
                let args = syn::punctuated::Punctuated::parse_terminated(&content)?;
                let _ = input.parse::<Option<Token![,]>>()?;

                Ok(Self {
                    sequence,
                    input: input_expr,
                    state,
                    output,
                    args,
                })
            }
        }

        let parsed = syn::parse2::<RecurSequenceCallInput>(mac.tokens.clone()).ok()?;
        let mut arguments = vec![Self::expr_to_string(&parsed.input)];
        let mut argument_kinds = vec![Self::classify_argument(&parsed.input)];

        if let Some(state) = parsed.state {
            arguments.push(Self::expr_to_string(&state));
            argument_kinds.push(Self::classify_argument(&state));
        }

        if let Some(output) = parsed.output {
            arguments.push(Self::expr_to_string(&output));
            argument_kinds.push(Self::classify_argument(&output));
        }

        for expr in parsed.args {
            arguments.push(Self::expr_to_string(&expr));
            argument_kinds.push(Self::classify_argument(&expr));
        }

        Some((parsed.sequence.to_string(), arguments, argument_kinds))
    }

    fn parse_named_recur_key(input: ParseStream, expected: &str) -> syn::Result<()> {
        let ident: syn::Ident = input.parse()?;
        if ident != expected {
            return Err(syn::Error::new(
                ident.span(),
                format!("expected `{}` key", expected),
            ));
        }
        input.parse::<Token![=]>()?;
        Ok(())
    }

    fn classify_argument(expr: &Expr) -> CallArgumentKind {
        match Self::expr_root_ident(expr) {
            Some(root) => CallArgumentKind::Rooted { root },
            None => CallArgumentKind::Inline,
        }
    }

    /// The identifier whose value an expression ultimately derives from, if
    /// any: `x` for all of `x`, `x.f[0]`, `x.clone()`, `&x`, `x?` and
    /// `select!(T, x.f)`.
    ///
    /// Narrowing an argument — selecting a field out of it, cloning it,
    /// indexing it — does not change where it came from, and the CFS binds
    /// provenance, not shape. Anything not rooted at a name (a literal, a
    /// constructor call, arithmetic) is materialized in the body itself and
    /// has no upstream to bind to.
    pub(crate) fn expr_root_ident(expr: &Expr) -> Option<String> {
        match expr {
            Expr::Path(path) => path.path.get_ident().map(|ident| ident.to_string()),
            Expr::Field(field) => Self::expr_root_ident(&field.base),
            Expr::Index(index) => Self::expr_root_ident(&index.expr),
            Expr::MethodCall(call) => Self::expr_root_ident(&call.receiver),
            Expr::Paren(paren) => Self::expr_root_ident(&paren.expr),
            Expr::Reference(reference) => Self::expr_root_ident(&reference.expr),
            Expr::Try(try_expr) => Self::expr_root_ident(&try_expr.expr),
            Expr::Macro(expr_macro) if Self::is_selection_macro(&expr_macro.mac) => {
                Self::selection_macro_root(&expr_macro.mac)
            }
            Expr::Macro(expr_macro) if Self::is_reference_macro(&expr_macro.mac) => {
                Self::reference_macro_root(&expr_macro.mac)
            }
            _ => None,
        }
    }

    /// The root identifier of a `select!(Type, expr)`'s selector expression —
    /// what the selection reads *from*.
    fn selection_macro_root(mac: &syn::Macro) -> Option<String> {
        let args = mac.parse_body_with(SelectionMacroArgs::parse).ok()?;
        Self::expr_root_ident(&args.expr)
    }

    /// The root identifier of a **reference macro** — `into_ref!(handle)` or
    /// `clone!(binding)`.
    ///
    /// Both take a single expression and hand back a reference to the same
    /// data: `into_ref!` unwraps a recur-item handle, `clone!` duplicates a
    /// binding. Neither narrows, computes, or produces a step, so the result
    /// carries exactly the provenance of its argument — which is why they
    /// resolve like selections rather than like calls.
    fn reference_macro_root(mac: &syn::Macro) -> Option<String> {
        let expr = mac.parse_body_with(Expr::parse).ok()?;
        Self::expr_root_ident(&expr)
    }

    /// The names supplying a `select!`'s **data-sourced** indexes, outermost
    /// segment first.
    ///
    /// `rows[token_id]` yields `["token_id"]`; `a.rows[i].cells[j]` yields
    /// `["i", "j"]`; `rows[7]` and `rows[a..b]` yield nothing. The order matches
    /// the selector's segment order, which is what lets the resolver line these
    /// up with the `BoundIndex` segments the macro emits.
    ///
    /// Only a bare path counts, mirroring what `select!` itself accepts: the
    /// macro rejects a computed index outright, so anything else here is not a
    /// dynamic index the CFS needs to record.
    fn selection_macro_index_roots(mac: &syn::Macro) -> Vec<String> {
        let Ok(args) = mac.parse_body_with(SelectionMacroArgs::parse) else {
            return Vec::new();
        };
        let mut roots = Vec::new();
        Self::collect_index_roots(&args.expr, &mut roots);
        roots
    }

    /// Walk a selector expression outside-in, collecting bare-path indexes.
    ///
    /// Recurses into the base *before* recording this level's index so the
    /// collected order matches selector order (the base is the outer path).
    fn collect_index_roots(expr: &Expr, roots: &mut Vec<String>) {
        match expr {
            Expr::Field(field) => Self::collect_index_roots(&field.base, roots),
            Expr::Paren(paren) => Self::collect_index_roots(&paren.expr, roots),
            Expr::MethodCall(call) => Self::collect_index_roots(&call.receiver, roots),
            Expr::Reference(reference) => Self::collect_index_roots(&reference.expr, roots),
            Expr::Try(try_expr) => Self::collect_index_roots(&try_expr.expr, roots),
            Expr::Index(index) => {
                Self::collect_index_roots(&index.expr, roots);
                if let Expr::Path(path) = index.index.as_ref() {
                    if let Some(ident) = path.path.get_ident() {
                        roots.push(ident.to_string());
                    }
                }
            }
            _ => {}
        }
    }

    fn is_selection_macro(mac: &syn::Macro) -> bool {
        Self::macro_path_is(mac, "select")
    }

    /// A **reference macro**: `into_ref!` (recur-item unwrap) or `clone!`
    /// (binding duplication).
    ///
    /// Grouped with `select!` rather than with the call primitives because
    /// neither produces a step: like a selection, each re-points at data that
    /// already exists, so its result carries its argument's provenance.
    fn is_reference_macro(mac: &syn::Macro) -> bool {
        Self::macro_path_is(mac, "into_ref") || Self::macro_path_is(mac, "clone")
    }

    /// Whether a macro path is `name!` or `raster::name!`.
    fn macro_path_is(mac: &syn::Macro, name: &str) -> bool {
        let segments: Vec<String> = mac
            .path
            .segments
            .iter()
            .map(|s| s.ident.to_string())
            .collect();

        match segments.as_slice() {
            [only] => only == name,
            [prefix, last] => prefix == "raster" && last == name,
            _ => false,
        }
    }

    /// Check if a macro path matches one of the canonical call primitive names.
    ///
    /// Matches canonical call primitives and `raster::*` qualified variants.
    fn macro_call_kind(mac: &syn::Macro) -> Option<CallKind> {
        let segments: Vec<String> = mac
            .path
            .segments
            .iter()
            .map(|s| s.ident.to_string())
            .collect();

        match segments.as_slice() {
            [name] if name == "call" => Some(CallKind::Tile),
            [prefix, name] if prefix == "raster" && name == "call" => Some(CallKind::Tile),
            [name] if name == "call_seq" => Some(CallKind::Sequence),
            [prefix, name] if prefix == "raster" && name == "call_seq" => Some(CallKind::Sequence),
            [name] if name == "call_recur" => Some(CallKind::RecursiveTile),
            [prefix, name] if prefix == "raster" && name == "call_recur" => {
                Some(CallKind::RecursiveTile)
            }
            [name] if name == "call_recur_seq" => Some(CallKind::RecursiveSequence),
            [prefix, name] if prefix == "raster" && name == "call_recur_seq" => {
                Some(CallKind::RecursiveSequence)
            }
            _ => None,
        }
    }
}

impl<'ast> Visit<'ast> for CallVisitor {
    fn visit_local(&mut self, node: &'ast Local) {
        let binding_name = Self::extract_binding_name(&node.pat);

        // `let name = select!(T, root.field)` and `let name = into_ref!(root)`
        // both bind a *view* of `root`, not a new value — record it so uses of
        // `name` resolve to `root`'s binding. Missing this is not a cosmetic
        // gap: an unattributed local resolves to `InputSource::Inline`, and an
        // `Inline` argument is one the schema does not pin to any upstream
        // binding, so a claimed trace could substitute bytes for it and still
        // verify. Call results are handled separately, through
        // `current_binding` / `result_binding`.
        if let (Some(name), Some(init)) = (binding_name.as_ref(), node.init.as_ref()) {
            if let Expr::Macro(expr_macro) = init.expr.as_ref() {
                if Self::is_selection_macro(&expr_macro.mac) {
                    if let Some(root) = Self::selection_macro_root(&expr_macro.mac) {
                        self.selection_aliases.push((name.clone(), root));
                    }
                    let index_roots = Self::selection_macro_index_roots(&expr_macro.mac);
                    if !index_roots.is_empty() {
                        self.selection_index_sources
                            .push((name.clone(), index_roots));
                    }
                } else if Self::is_reference_macro(&expr_macro.mac) {
                    if let Some(root) = Self::reference_macro_root(&expr_macro.mac) {
                        self.selection_aliases.push((name.clone(), root));
                    }
                }
            // A bare `binding.clone()` is the pre-DSL spelling of `clone!`. It
            // is still legal Rust and still correct in an *argument* position
            // (`classify_argument` roots it through the receiver), so analyzing
            // it here keeps the two positions from disagreeing while `clone!`
            // becomes the written form. Allowlisted by name rather than
            // accepting any method: a wrong alias claims provenance that does
            // not hold, which is worse than claiming none.
            } else if let Expr::MethodCall(call) = init.expr.as_ref() {
                if call.method == "clone" {
                    if let Some(root) = Self::expr_root_ident(&call.receiver) {
                        self.selection_aliases.push((name.clone(), root));
                    }
                }
            }
        }

        // Set the current binding context before visiting the initializer
        self.current_binding = binding_name;

        // Visit the initializer expression (this will trigger visit_expr_call /
        // visit_expr_macro if there's a call or macro invocation)
        if let Some(init) = &node.init {
            self.visit_expr(&init.expr);
        }

        // Clear the binding context after processing
        self.current_binding = None;
    }

    fn visit_expr_macro(&mut self, node: &'ast ExprMacro) {
        if let Some(call_kind) = Self::macro_call_kind(&node.mac) {
            if let Some((callee, arguments, argument_kinds, chunk)) =
                Self::parse_call_macro_args(&node.mac)
            {
                let result_binding = self.current_binding.take();
                self.call_infos.push(CallInfo {
                    callee,
                    arguments,
                    argument_kinds,
                    result_binding,
                    call_kind,
                    chunk,
                });
                // Do not recurse into the macro body — arguments are already captured above.
                return;
            }
        }
        // For unrecognized macros, continue default visitation.
        syn::visit::visit_expr_macro(self, node);
    }

    fn visit_stmt_macro(&mut self, node: &'ast StmtMacro) {
        // Bare macro statements (e.g. `call_seq!(foo, x);` without a `let` binding) are
        // parsed as `Stmt::Macro(StmtMacro)` by syn, not as `Stmt::Expr(Expr::Macro)`.
        // The default Visit path for StmtMacro calls `visit_macro` (the raw Macro struct),
        // which does NOT trigger `visit_expr_macro`. We handle them here so that statement-
        // position `call!` and `call_seq!` invocations are captured without a binding.
        if let Some(call_kind) = Self::macro_call_kind(&node.mac) {
            if let Some((callee, arguments, argument_kinds, chunk)) =
                Self::parse_call_macro_args(&node.mac)
            {
                // current_binding is None here — bare statements have no let binding.
                self.call_infos.push(CallInfo {
                    callee,
                    arguments,
                    argument_kinds,
                    result_binding: None,
                    call_kind,
                    chunk,
                });
                return;
            }
        }
        syn::visit::visit_stmt_macro(self, node);
    }

    // Note: visit_expr_call is intentionally NOT overridden. Bare function calls
    // (e.g. `greet(name)`) are not extracted — only canonical `call!` and `call_seq!`
    // macro invocations are recognized as step boundaries in sequences.
}

#[cfg(test)]
mod tests {
    use super::*;
    use syn::visit::Visit;

    fn parse_calls(code: &str) -> Vec<CallInfo> {
        let file: syn::File = syn::parse_str(code).expect("Failed to parse test code");
        let mut visitor = CallVisitor::new();
        visitor.visit_file(&file);
        visitor.get_call_infos()
    }

    fn rooted(root: &str) -> CallArgumentKind {
        CallArgumentKind::Rooted {
            root: root.to_string(),
        }
    }

    #[test]
    fn test_bare_call_not_extracted() {
        // Bare function calls (without call!/call_seq!) must NOT be extracted.
        let calls = parse_calls("fn seq() { let x = greet(name); }");
        assert_eq!(calls.len(), 0);
    }

    #[test]
    fn test_call_macro_extraction() {
        let calls = parse_calls("fn seq() { let greeting = call!(greet, name); }");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].callee, "greet");
        assert_eq!(calls[0].call_kind, CallKind::Tile);
        assert_eq!(calls[0].result_binding.as_deref(), Some("greeting"));
        assert_eq!(calls[0].arguments, vec!["name"]);
        assert_eq!(calls[0].argument_kinds, vec![rooted("name")]);
    }

    #[test]
    fn test_call_seq_macro_extraction() {
        let calls = parse_calls("fn seq() { let result = call_seq!(wish_sequence, greeting); }");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].callee, "wish_sequence");
        assert_eq!(calls[0].call_kind, CallKind::Sequence);
        assert_eq!(calls[0].result_binding.as_deref(), Some("result"));
        assert_eq!(calls[0].arguments, vec!["greeting"]);
        assert_eq!(calls[0].argument_kinds, vec![rooted("greeting")]);
    }

    #[test]
    fn test_call_recur_macro_extraction() {
        let calls = parse_calls(
            "fn seq() { let result = call_recur!(tile = build, input = items, output = new!(Doc), args = (needle,)); }",
        );
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].callee, "build");
        assert_eq!(calls[0].call_kind, CallKind::RecursiveTile);
        assert_eq!(calls[0].result_binding.as_deref(), Some("result"));
        assert_eq!(calls[0].arguments.len(), 3);
    }

    #[test]
    fn test_call_recur_state_only_macro_extraction() {
        let calls = parse_calls(
            "fn seq() { let result = call_recur!(tile = reduce, input = items, state = Stats { max_len: 0 }, args = (needle,)); }",
        );
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].callee, "reduce");
        assert_eq!(calls[0].call_kind, CallKind::RecursiveTile);
        assert_eq!(calls[0].result_binding.as_deref(), Some("result"));
        assert_eq!(calls[0].arguments.len(), 3);
    }

    #[test]
    fn test_call_recur_chunked_macro_extraction() {
        // `chunk = N` is a control parameter: the call must still be
        // discovered, chunk must not appear among the extracted arguments,
        // and the literal value must be captured for CFS pinning.
        let calls = parse_calls(
            "fn seq() { let result = call_recur!(tile = collect, input = items, chunk = 2, output = new!(Doc), args = (title,)); }",
        );
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].callee, "collect");
        assert_eq!(calls[0].call_kind, CallKind::RecursiveTile);
        assert_eq!(calls[0].result_binding.as_deref(), Some("result"));
        assert_eq!(calls[0].arguments, vec!["items", "new ! (Doc)", "title"]);
        assert_eq!(calls[0].chunk, Some(2));
    }

    #[test]
    fn test_call_recur_without_chunk_has_no_chunk() {
        let calls = parse_calls(
            "fn seq() { let result = call_recur!(tile = build, input = items, output = new!(Doc), args = (needle,)); }",
        );
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].chunk, None);
    }

    #[test]
    #[should_panic(expected = "must be an integer literal")]
    fn test_call_recur_rejects_non_literal_chunk() {
        parse_calls(
            "fn seq() { let result = call_recur!(tile = collect, input = items, chunk = size, output = new!(Doc), args = (title,)); }",
        );
    }

    #[test]
    fn test_call_macro_no_args() {
        let calls = parse_calls("fn seq() { let r = call!(no_arg_tile); }");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].callee, "no_arg_tile");
        assert_eq!(calls[0].call_kind, CallKind::Tile);
        assert_eq!(calls[0].arguments.len(), 0);
    }

    #[test]
    fn test_call_macro_multiple_args() {
        let calls = parse_calls("fn seq() { let r = call!(tile_fn, a, b, c); }");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].callee, "tile_fn");
        assert_eq!(calls[0].call_kind, CallKind::Tile);
        assert_eq!(calls[0].arguments.len(), 3);
        assert_eq!(
            calls[0].argument_kinds,
            vec![rooted("a"), rooted("b"), rooted("c")]
        );
    }

    #[test]
    fn test_argument_classification_roots_selections_and_literals() {
        let calls = parse_calls(
            r#"fn seq() { let r = call_seq!(next, select!(String, source.name), "x"); }"#,
        );
        assert_eq!(calls.len(), 1);
        // A selection is a view of `source`, so it carries `source`'s
        // provenance; a literal has none.
        assert_eq!(
            calls[0].argument_kinds,
            vec![rooted("source"), CallArgumentKind::Inline]
        );
    }

    #[test]
    fn test_argument_classification_sees_through_narrowing_expressions() {
        let calls = parse_calls(
            r#"fn seq() { let r = call!(t, data.clone(), other.rows[0].name, &third, select!(u64, nested.clone().id)); }"#,
        );
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].argument_kinds,
            vec![
                rooted("data"),
                rooted("other"),
                rooted("third"),
                rooted("nested"),
            ]
        );
    }

    #[test]
    fn test_argument_classification_treats_constructed_values_as_inline() {
        let calls = parse_calls(
            r#"fn seq() { let r = call!(t, "lit".to_string(), Doc { id: 1 }, 2 + 2); }"#,
        );
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].argument_kinds,
            vec![
                CallArgumentKind::Inline,
                CallArgumentKind::Inline,
                CallArgumentKind::Inline,
            ]
        );
    }

    #[test]
    fn test_selection_aliases_record_let_bound_selections() {
        let file: syn::File = syn::parse_str(
            r#"fn seq() {
                let name = select!(String, personal_data.clone().name);
                let seed = select!(u64, seed);
                let plain = compute();
            }"#,
        )
        .expect("Failed to parse test code");
        let mut visitor = CallVisitor::new();
        visitor.visit_file(&file);

        assert_eq!(
            visitor.get_selection_aliases(),
            vec![
                ("name".to_string(), "personal_data".to_string()),
                ("seed".to_string(), "seed".to_string()),
            ]
        );
    }

    /// The reference macros — `into_ref!` and `clone!` — hand back a reference
    /// to the same data, so the local must alias to their argument. Without
    /// this the local is unattributed and resolves to `InputSource::Inline`: an
    /// argument the schema pins to nothing.
    #[test]
    fn test_reference_macros_alias_to_their_argument() {
        let file: syn::File = syn::parse_str(
            r#"fn seq() {
                let activation = into_ref!(input);
                let qualified = raster::into_ref!(input);
                let params_again = clone!(params);
                let token_id = select!(u32, activation.clone().token_id);
            }"#,
        )
        .expect("Failed to parse test code");
        let mut visitor = CallVisitor::new();
        visitor.visit_file(&file);

        assert_eq!(
            visitor.get_selection_aliases(),
            vec![
                ("activation".to_string(), "input".to_string()),
                ("qualified".to_string(), "input".to_string()),
                ("params_again".to_string(), "params".to_string()),
                ("token_id".to_string(), "activation".to_string()),
            ]
        );
    }

    /// The pre-DSL spelling. `clone!` is the form to write, but a bare
    /// `.clone()` is legal Rust and is already rooted correctly in argument
    /// position — so a `let` bound to one must agree, or the same expression
    /// means two different things depending on where it appears.
    ///
    /// Only `clone` is allowlisted: an arbitrary method may not preserve
    /// provenance, and a wrong alias is worse than no alias.
    #[test]
    fn test_bare_clone_method_aliases_but_other_methods_do_not() {
        let file: syn::File = syn::parse_str(
            r#"fn seq() {
                let copy = params.clone();
                let nested = layer.params.clone();
                let opaque = handle.into_inner();
            }"#,
        )
        .expect("Failed to parse test code");
        let mut visitor = CallVisitor::new();
        visitor.visit_file(&file);

        assert_eq!(
            visitor.get_selection_aliases(),
            vec![
                ("copy".to_string(), "params".to_string()),
                ("nested".to_string(), "layer".to_string()),
            ],
            "`into_inner` is not a provenance-preserving form and must not alias"
        );
    }

    /// The index roots of a `select!` are what the CFS records as citations, so
    /// this is the AST half of `Indexed`. Nothing covered it before, which is
    /// why an unattributed index could reach the schema unnoticed.
    #[test]
    fn test_selection_index_sources_record_dynamic_indexes() {
        let file: syn::File = syn::parse_str(
            r#"fn seq() {
                let row = select!(Row, rows[token_id]);
                let cell = select!(Cell, grid[i].cells[j]);
                let fixed = select!(Row, rows[7]);
                let window = select!(Block<Row>, rows[0..4]);
            }"#,
        )
        .expect("Failed to parse test code");
        let mut visitor = CallVisitor::new();
        visitor.visit_file(&file);

        assert_eq!(
            visitor.get_selection_index_sources(),
            vec![
                ("row".to_string(), vec!["token_id".to_string()]),
                ("cell".to_string(), vec!["i".to_string(), "j".to_string()]),
            ],
            "literal and range indexes cite nothing; data-sourced ones cite in selector order"
        );
    }

    #[test]
    fn test_mixed_calls_in_sequence() {
        let calls = parse_calls(
            r#"
            fn seq(name: String) -> String {
                let greeting = call!(greet, name);
                let e1 = call!(exclaim, greeting);
                let result = call_seq!(wish_sequence, e1);
                result
            }
            "#,
        );
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0].callee, "greet");
        assert_eq!(calls[0].call_kind, CallKind::Tile);
        assert_eq!(calls[1].callee, "exclaim");
        assert_eq!(calls[1].call_kind, CallKind::Tile);
        assert_eq!(calls[2].callee, "wish_sequence");
        assert_eq!(calls[2].call_kind, CallKind::Sequence);
    }

    #[test]
    fn test_statement_level_call_seq_no_binding() {
        // call_seq! as a bare statement (no `let`) must still be captured.
        let calls = parse_calls(
            r#"
            fn main() {
                call_seq!(greet_sequence, x);
            }
            "#,
        );
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].callee, "greet_sequence");
        assert_eq!(calls[0].call_kind, CallKind::Sequence);
        assert_eq!(calls[0].result_binding, None);
    }

    #[test]
    fn test_statement_level_call_seq_extraction() {
        // call_seq! used as a statement expression (no `let` binding) must still be captured.
        let calls = parse_calls(
            r#"
            fn main(name: String) {
                call_seq!(greet_sequence, "Rust".to_string());
                let name_2 = call_seq!(placeholder_sequence, name);
                let _result = call_seq!(greet_sequence, name_2);
            }
            "#,
        );
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0].callee, "greet_sequence");
        assert_eq!(calls[0].call_kind, CallKind::Sequence);
        assert_eq!(calls[0].result_binding, None);
        assert_eq!(calls[1].callee, "placeholder_sequence");
        assert_eq!(calls[1].call_kind, CallKind::Sequence);
        assert_eq!(calls[1].result_binding.as_deref(), Some("name_2"));
        assert_eq!(calls[2].callee, "greet_sequence");
        assert_eq!(calls[2].call_kind, CallKind::Sequence);
        assert_eq!(calls[2].result_binding.as_deref(), Some("_result"));
    }

    #[test]
    fn test_qualified_call_macro_extraction() {
        let calls = parse_calls("fn seq() { let x = raster::call!(greet, name); }");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].callee, "greet");
        assert_eq!(calls[0].call_kind, CallKind::Tile);
        assert_eq!(calls[0].result_binding.as_deref(), Some("x"));
    }

    #[test]
    fn test_qualified_call_seq_macro_extraction() {
        let calls = parse_calls("fn seq() { let x = raster::call_seq!(wish_seq, name); }");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].callee, "wish_seq");
        assert_eq!(calls[0].call_kind, CallKind::Sequence);
        assert_eq!(calls[0].result_binding.as_deref(), Some("x"));
    }
}
