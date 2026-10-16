use cargo_toml::Manifest;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::path::{Path, PathBuf};
use syn::{
    parse::{Parse, ParseStream},
    parse_file,
    visit::Visit,
    Attribute, Expr, ExprLit, ExprMacro, FnArg, Lit, Local, Meta, Pat, Stmt, StmtMacro, Token,
};
use walkdir::WalkDir;

use raster_core::input::SelectorSegment;
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
    ///
    /// `path` is the static selector the expression itself appends to `root`
    /// (`[field]` for `select!(T, x.field)`, empty for `x` or `clone!(x)`);
    /// the resolver prefixes the paths of `root`'s alias chain. `None` for a
    /// `select!` with a data-sourced index, which has no static path.
    Rooted {
        root: String,
        path: Option<Vec<SelectorSegment>>,
    },
    /// A literal, or an expression with no identifier at its root: the value
    /// is materialized in the sequence body itself.
    Inline,
}

/// How a recur site gets its object (`incremental-draft-materialization` §One
/// storage rule).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SiteOutputKind {
    /// `output,` — the site creates a new object.
    Create,
    /// `output = base,` — the site derives from a stored object.
    Derive,
}

/// What a function body returns — its last expression, classified the way a
/// call argument is, so the flow resolver can bind it to a source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReturnExpr {
    /// The body ends with a call primitive (`call!`, `call_seq!`, …) and
    /// returns that call's result: the output of the last step.
    TailCall,
    /// The body returns a value rooted at a name — a binding, `select!(T,
    /// x.f)`, `clone!(x)` — optionally wrapped in `Ok(..)` or `..?`. `path` is
    /// the returned expression's own static selector path (`[f]` for
    /// `select!(T, x.f)`, empty for a bare binding); the resolver prefixes the
    /// paths of `root`'s alias chain.
    Rooted {
        root: String,
        path: Vec<SelectorSegment>,
    },
    /// A returned expression the CFS cannot bind: a literal, a computed
    /// value, a plain function call such as `finalize(draft)`, or a `select!`
    /// with a data-sourced index. Kept as source text for the diagnostic.
    Unbound { expr: String },
    /// `(a, b, …)` — a recur-sequence body's `(state, output)`. Each element
    /// is classified on its own, except that a call inside the tuple is not
    /// bound: it need not be the body's last step, which is what `TailCall`
    /// means.
    Tuple(Vec<ReturnExpr>),
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
    /// A recur site's `output`, when it has one: bare `output` creates the
    /// site's own object, `output = base` derives it from a stored one. This
    /// is control-flow shape, not only a data argument — the CFS declares it
    /// (`RecurOutputDecl`).
    pub output: Option<SiteOutputKind>,
    /// True when the site's own output *is* its carried state — `state` with no
    /// `output`. It is what lets the trace pin a sweep's **final** carried
    /// state: every earlier one is pinned by the next iteration's, and this is
    /// the only shape where the last one has something to be compared against.
    pub state_is_output: bool,
    /// Whether the site carries state at all (`state = …`), with or without
    /// an `output`. Recorded in the CFS so a stored seed is found by position
    /// at `RecurStart` (`incremental-draft-materialization` D5b).
    pub carries_state: bool,
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
    /// `let name = select!(T, root.path)` locals, as `(name, path)` — the
    /// static selector path each selection appends to its root's selector,
    /// in selector order. `clone!`/`into_ref!`/`.clone()` aliases record the
    /// path of what they wrap: empty for `clone!(x)`, `[f]` for
    /// `clone!(select!(T, x.f))`. A selection with a data-sourced index
    /// records none.
    pub selection_paths: Vec<(String, Vec<SelectorSegment>)>,
    /// The body's returned expression, classified. `None` when the body ends
    /// in a statement (a unit return).
    pub return_expr: Option<ReturnExpr>,
    /// Selections the compiler could not lower to the path the `select!`
    /// macro records: a byte offset into a `Bytes<N>` whose page size could
    /// not be read from the source, or a range bound it cannot evaluate.
    /// Building a CFS fails on any of these rather than declaring a path no
    /// run records.
    pub selection_errors: Vec<String>,
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

        // Two passes: every file's structs and function signatures first, so a
        // function body can be read against the whole project's types.
        let files: Vec<(PathBuf, syn::File)> = files_paths
            .iter()
            .map(|path| {
                let content = std::fs::read_to_string(path).unwrap();
                (path.clone(), parse_file(&content).unwrap())
            })
            .collect();
        let structs: Vec<StructAstItem> = files
            .iter()
            .flat_map(|(_, ast)| crate::schema_walk::collect_structs(&ast.items))
            .collect();
        let fn_outputs: HashMap<String, String> = files
            .iter()
            .flat_map(|(_, ast)| ast.items.iter())
            .filter_map(|item| match item {
                syn::Item::Fn(func) => match &func.sig.output {
                    syn::ReturnType::Type(_, ty) => {
                        Some((func.sig.ident.to_string(), quote::quote!(#ty).to_string()))
                    }
                    syn::ReturnType::Default => None,
                },
                _ => None,
            })
            .collect();

        let shared_structs = Rc::new(structs.clone());
        let fn_outputs = Rc::new(fn_outputs);
        let mut functions = Vec::new();
        for (path, ast) in &files {
            functions.extend(Self::parse_functions(
                ast,
                path.clone(),
                &shared_structs,
                &fn_outputs,
            ));
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

    pub(crate) fn parse_functions(
        ast: &syn::File,
        path: PathBuf,
        structs: &Rc<Vec<StructAstItem>>,
        fn_outputs: &Rc<HashMap<String, String>>,
    ) -> Vec<FunctionAstItem> {
        let mut functions = Vec::new();
        let file_constants = CallVisitor::file_constants(ast);

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

                let types = TypeContext::for_function(
                    Rc::clone(structs),
                    Rc::clone(fn_outputs),
                    input_names.iter().cloned().zip(inputs.iter().cloned()),
                );
                // A parameter shadows the file constant it names.
                let mut constants = file_constants.clone();
                for name in &input_names {
                    constants.remove(name);
                }
                let mut visitor = CallVisitor::with_types(types).with_constants(constants);
                visitor.visit_item_fn(func);
                let call_infos = visitor.get_call_infos();
                let selection_aliases = visitor.get_selection_aliases();
                let selection_index_sources = visitor.get_selection_index_sources();
                let selection_paths = visitor.get_selection_paths();
                let return_expr = visitor.classify_return(&func.block);
                let selection_errors = visitor.get_selection_errors();
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
                    selection_paths,
                    return_expr,
                    selection_errors,
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
    selected_ty: syn::Type,
    expr: Expr,
}

impl Parse for SelectionMacroArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let selected_ty: syn::Type = input.parse()?;
        input.parse::<Token![,]>()?;
        Ok(Self {
            selected_ty,
            expr: input.parse()?,
        })
    }
}

/// What the compiler can see of types while it reads one function body: the
/// project's structs and function outputs, and each local's declared type in
/// statement order. Enough to find the `Bytes<N>` a byte-page `select!`
/// indexes, so its declared path can be lowered as the macro lowers it.
#[derive(Debug, Clone, Default)]
pub(crate) struct TypeContext {
    structs: Rc<Vec<StructAstItem>>,
    fn_outputs: Rc<HashMap<String, String>>,
    locals: HashMap<String, String>,
}

impl TypeContext {
    /// A function's context: the project's types, and its own parameters.
    pub(crate) fn for_function(
        structs: Rc<Vec<StructAstItem>>,
        fn_outputs: Rc<HashMap<String, String>>,
        params: impl IntoIterator<Item = (String, String)>,
    ) -> Self {
        Self {
            structs,
            fn_outputs,
            locals: params.into_iter().collect(),
        }
    }
}

/// Separates a shadowing binding's source name from its ordinal (`x#1`). Not
/// an identifier character, so a binding name never collides with a source
/// name.
const BINDING_SUFFIX: char = '#';

/// The source name of a binding name, for messages: `x` for `x#1`.
pub(crate) fn source_name(binding: &str) -> &str {
    binding.split(BINDING_SUFFIX).next().unwrap_or(binding)
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
    /// Per alias local, the static selector path it appends (see
    /// `FunctionAstItem::selection_paths`).
    selection_paths: Vec<(String, Vec<SelectorSegment>)>,
    /// Types in scope, for lowering byte-page selections.
    types: TypeContext,
    /// Selections whose path could not be lowered (see
    /// `FunctionAstItem::selection_errors`).
    selection_errors: RefCell<Vec<String>>,
    /// Integer constants in scope, by name: the file's `const` items, then the
    /// body's `let`s and `const`s in statement order. A `select!` range whose
    /// bounds are locals (`rows[start..end]`) records the values those locals
    /// hold, so the compiler reads them here to declare the same path.
    constants: HashMap<String, u64>,
    /// How many blocks deep the visit is, for scoping `constants`.
    block_depth: usize,
    /// Each source name in scope, to the binding it currently names (see
    /// [`Self::bind_name`]). A name absent here names its first binding.
    scope: HashMap<String, String>,
    /// How many times each source name has been bound so far in the function,
    /// parameters included. Never restored at a block's end, so a binding
    /// name is never reused.
    binding_counts: HashMap<String, usize>,
}

impl CallVisitor {
    /// A visitor with no types in scope, for tests of everything else.
    #[cfg(test)]
    fn new() -> Self {
        Self::with_types(TypeContext::default())
    }

    fn with_types(types: TypeContext) -> Self {
        Self {
            call_infos: Vec::new(),
            current_binding: None,
            selection_aliases: Vec::new(),
            selection_index_sources: Vec::new(),
            selection_paths: Vec::new(),
            types,
            selection_errors: RefCell::new(Vec::new()),
            constants: HashMap::new(),
            block_depth: 0,
            scope: HashMap::new(),
            binding_counts: HashMap::new(),
        }
    }

    /// Seed the integer constants in scope at the start of the body.
    fn with_constants(mut self, constants: HashMap<String, u64>) -> Self {
        self.constants = constants;
        self
    }

    /// Record a selection error once: the same `select!` is lowered from
    /// several places (its `let`, each argument naming it, the return).
    fn push_selection_error(&self, message: String) {
        let mut errors = self.selection_errors.borrow_mut();
        if !errors.contains(&message) {
            errors.push(message);
        }
    }

    fn get_selection_errors(&self) -> Vec<String> {
        self.selection_errors.borrow().clone()
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

    fn get_selection_paths(&self) -> Vec<(String, Vec<SelectorSegment>)> {
        self.selection_paths.clone()
    }

    /// Record `name` as a view of `root` through `view`, a form that re-points
    /// at the same value (`clone!(x)`, `into_ref!(x)`, `x.clone()`). It keeps
    /// the path and index sources of any selection it wraps:
    /// `clone!(select!(T, x.f))` is `x.f`, not the whole of `x`. Like a bare
    /// `select!` alias, a data-sourced index records no static path.
    fn record_view_alias(&mut self, name: &str, root: String, view: &Expr) {
        self.selection_aliases.push((name.to_string(), root));
        if let Some(path) = self.argument_path(view) {
            self.selection_paths.push((name.to_string(), path));
        }
        let index_roots = self.bindings_of(Self::view_index_roots(view));
        if !index_roots.is_empty() {
            self.selection_index_sources
                .push((name.to_string(), index_roots));
        }
    }

    fn bindings_of(&self, names: Vec<String>) -> Vec<String> {
        names.iter().map(|name| self.binding_of(name)).collect()
    }

    /// The binding a source name refers to at this point of the body.
    ///
    /// Everything this visitor records about a body — call arguments, results,
    /// selection aliases and their index sources, the return — names
    /// *bindings*, not source names: the resolver looks them up in flat maps
    /// after the whole body has been read, so `let x = select!(T, a.f); use(x);
    /// let x = select!(T, a.g); use(x);` would otherwise resolve both uses
    /// through the last `x`. Renaming at each use to the binding then in scope
    /// keeps Rust's shadowing and block scoping.
    fn binding_of(&self, name: &str) -> String {
        self.scope.get(name).cloned().unwrap_or_else(|| name.to_string())
    }

    /// A new binding of `name`: its first keeps the source name — so a body
    /// that never shadows records exactly the names it is written with, and a
    /// parameter is always its own name — and each later one is `name#k`,
    /// which no source name can collide with. Not in scope until
    /// [`Self::enter_binding`]: the `let`'s initializer still sees the old one.
    fn bind_name(&mut self, name: &str) -> String {
        let count = self.binding_counts.entry(name.to_string()).or_insert(0);
        let binding = match *count {
            0 => name.to_string(),
            k => format!("{name}{BINDING_SUFFIX}{k}"),
        };
        *count += 1;
        binding
    }

    fn enter_binding(&mut self, name: &str, binding: String) {
        self.scope.insert(name.to_string(), binding);
    }

    /// `x` in `x: T`.
    fn unwrap_type_pat(pat: &Pat) -> &Pat {
        match pat {
            Pat::Type(pat_type) => Self::unwrap_type_pat(&pat_type.pat),
            other => other,
        }
    }

    /// Extracts the binding name from a pattern (e.g., `x` from `let x = ...`)
    fn extract_binding_name(pat: &Pat) -> Option<String> {
        match pat {
            Pat::Ident(pat_ident) => Some(pat_ident.ident.to_string()),
            Pat::Type(pat_type) => Self::extract_binding_name(&pat_type.pat),
            _ => None,
        }
    }

    /// Classify a function body's returned expression: its last statement
    /// when that statement is a value (no trailing `;`), or the operand of a
    /// final `return`.
    pub(crate) fn classify_return(&self, block: &syn::Block) -> Option<ReturnExpr> {
        match block.stmts.last()? {
            Stmt::Expr(Expr::Return(ret), _) => ret.expr.as_deref().map(|expr| self.classify_return_expr(expr)),
            Stmt::Expr(expr, None) => Some(self.classify_return_expr(expr)),
            // A trailing macro invocation with no `;` parses as a statement
            // macro, not an expression.
            Stmt::Macro(stmt) if stmt.semi_token.is_none() => {
                Some(self.classify_return_expr(&Expr::Macro(ExprMacro {
                    attrs: stmt.attrs.clone(),
                    mac: stmt.mac.clone(),
                })))
            }
            _ => None,
        }
    }

    fn classify_return_expr(&self, expr: &Expr) -> ReturnExpr {
        match expr {
            // `Ok(x)` from a fallible `main`, and `x?`, return `x`.
            Expr::Call(call) if call.args.len() == 1 && Self::expr_path_is(&call.func, "Ok") => {
                self.classify_return_expr(&call.args[0])
            }
            Expr::Try(try_expr) => self.classify_return_expr(&try_expr.expr),
            Expr::Paren(paren) => self.classify_return_expr(&paren.expr),
            Expr::Tuple(tuple) => ReturnExpr::Tuple(
                tuple
                    .elems
                    .iter()
                    .map(|elem| match self.classify_return_expr(elem) {
                        ReturnExpr::TailCall => ReturnExpr::Unbound {
                            expr: Self::expr_to_string(elem),
                        },
                        other => other,
                    })
                    .collect(),
            ),
            Expr::Macro(expr_macro) if Self::macro_call_kind(&expr_macro.mac).is_some() => {
                ReturnExpr::TailCall
            }
            // A selection: its root, and the static path it appends. One with
            // a data-sourced index has no static path — the index is cited as
            // a separate binding a program output has nowhere to carry — so it
            // is not bound rather than bound loosely.
            Expr::Macro(expr_macro) if Self::is_selection_macro(&expr_macro.mac) => {
                match (
                    Self::selection_macro_root(&expr_macro.mac),
                    self.selection_macro_path(&expr_macro.mac),
                ) {
                    (Some(root), Some(path)) => ReturnExpr::Rooted {
                        root: self.binding_of(&root),
                        path,
                    },
                    _ => ReturnExpr::Unbound {
                        expr: Self::expr_to_string(expr),
                    },
                }
            }
            // Views re-point at the value they wrap, so they return whatever it
            // is: `clone!(x)` returns `x`, and `clone!(select!(T, x.f))` returns
            // `x.f`, not the whole of `x`.
            Expr::Macro(expr_macro) if Self::is_reference_macro(&expr_macro.mac) => {
                match Self::reference_macro_arg(&expr_macro.mac) {
                    Some(inner) => self.classify_return_expr(&inner),
                    None => ReturnExpr::Unbound {
                        expr: Self::expr_to_string(expr),
                    },
                }
            }
            Expr::MethodCall(call) if call.method == "clone" && call.args.is_empty() => {
                self.classify_return_expr(&call.receiver)
            }
            Expr::Path(path) if path.path.get_ident().is_some() => ReturnExpr::Rooted {
                root: self.binding_of(&path.path.get_ident().expect("checked above").to_string()),
                path: Vec::new(),
            },
            // Anything else — including a raw `x.f` or `x[0]`, which would
            // narrow without saying so — is not bound: returning it with an
            // empty path would claim the whole of `x`.
            _ => ReturnExpr::Unbound {
                expr: Self::expr_to_string(expr),
            },
        }
    }

    /// The static selector path a `select!(T, root.path)` appends, lowered the
    /// way the `select!` macro lowers it (`split_selector_expr` in
    /// `raster-macros`): a named field to `Field`, a tuple field or an integer
    /// literal index to `Index`, a literal `start..end` to `Range`. `None` for
    /// a data-sourced index (a bare binding), which is cited at run time rather
    /// than fixed by the program, and for anything the macro rejects.
    ///
    /// A byte-page selection is lowered as well; see
    /// [`Self::lower_byte_page_path`].
    ///
    /// A range bound may be a constant the body or file binds (`let start = 0;`
    /// … `rows[start..end]`): the macro records the value it holds, and so
    /// does this. When a selection with no data-sourced index still has no
    /// static path — a bound the compiler cannot evaluate — it is recorded in
    /// `selection_errors`: returning `None` would declare a read of the whole
    /// root, which no run records.
    fn selection_macro_path(&self, mac: &syn::Macro) -> Option<Vec<SelectorSegment>> {
        let args = mac.parse_body_with(SelectionMacroArgs::parse).ok()?;
        let mut path = Vec::new();
        if self.collect_static_path(&args.expr, &mut path).is_none() {
            let mut index_roots = Vec::new();
            Self::collect_index_roots(&args.expr, &mut index_roots);
            if index_roots.is_empty() {
                self.push_selection_error(format!(
                    "cannot evaluate the selector path of `{}`: range bounds must be integer \
                     literals, or locals and constants bound to integer constant expressions",
                    Self::expr_to_string(&args.expr)
                ));
            }
            return None;
        }
        Some(self.lower_byte_page_path(&args, path))
    }

    /// Lower a byte-page selection's path the way `select!` does
    /// (`page_insertion_plan` / `emit_index_lit` in `raster-macros`).
    ///
    /// A selection of `BytesPage` or `Block<BytesPage>` that ends in an index
    /// or range, not already under `.pages`, reads the region's `pages` list:
    /// the macro inserts that field before the last segment, and a literal
    /// there is a byte offset (or byte range) that it divides by the region's
    /// page size. So `select!(BytesPage, model.weights[4])` on a `Bytes<4>`
    /// field is `[weights, pages, 1]`, and the CFS must declare the same.
    ///
    /// The page size is the `N` of the `Bytes<N>` the rest of the path
    /// reaches, read from the root binding's declared type. When it cannot be
    /// read, the selection is recorded in `selection_errors` rather than
    /// declared with a path no run records.
    fn lower_byte_page_path(
        &self,
        args: &SelectionMacroArgs,
        mut path: Vec<SelectorSegment>,
    ) -> Vec<SelectorSegment> {
        if !Self::is_byte_page_type(&args.selected_ty) || path.is_empty() {
            return path;
        }
        let last = path.len() - 1;
        let already_pages = last > 0
            && matches!(&path[last - 1], SelectorSegment::Field(name) if name == "pages");
        if already_pages
            || !matches!(
                path[last],
                SelectorSegment::Index(_) | SelectorSegment::Range { .. }
            )
        {
            return path;
        }

        let root = Self::expr_root_ident(&args.expr);
        let page_size = root.as_ref().and_then(|root| {
            let root_ty = self.types.locals.get(root)?;
            crate::schema_walk::bytes_page_size_at(root_ty, &path[..last], &self.types.structs)
        });
        let selection = Self::expr_to_string(&args.expr);
        let Some(page_size) = page_size.filter(|size| *size > 0) else {
            self.push_selection_error(format!(
                "cannot read the page size of the `Bytes<N>` that `{selection}` indexes, so its \
                 byte offset cannot be lowered to a page index; select the page explicitly \
                 with `.pages[k]` or `.pages[a..b]`"
            ));
            return path;
        };
        let aligned = |offset: u64| offset % page_size == 0;
        path[last] = match path[last] {
            SelectorSegment::Index(offset) if aligned(offset) => {
                SelectorSegment::Index(offset / page_size)
            }
            SelectorSegment::Range { start, end } if aligned(start) && aligned(end) => {
                SelectorSegment::Range {
                    start: start / page_size,
                    end: end / page_size,
                }
            }
            _ => {
                // `select!` refuses to compile an unaligned offset.
                self.push_selection_error(format!(
                    "`{selection}` is not aligned to its page size {page_size}"
                ));
                return path;
            }
        };
        path.insert(last, SelectorSegment::Field("pages".to_string()));
        path
    }

    /// `BytesPage` or `Block<BytesPage>`: the targets `select!` lowers byte
    /// offsets for.
    fn is_byte_page_type(ty: &syn::Type) -> bool {
        let syn::Type::Path(type_path) = ty else {
            return false;
        };
        let Some(segment) = type_path.path.segments.last() else {
            return false;
        };
        match segment.ident.to_string().as_str() {
            "BytesPage" => true,
            "Block" => match &segment.arguments {
                syn::PathArguments::AngleBracketed(args) => args.args.iter().any(|arg| {
                    matches!(arg, syn::GenericArgument::Type(inner) if Self::is_byte_page_type(inner))
                }),
                _ => false,
            },
            _ => false,
        }
    }

    /// The declared type of a view expression: a local's, a `select!`'s
    /// target, or what a `clone!` / `into_ref!` / `.clone()` wraps.
    fn view_type(&self, expr: &Expr) -> Option<String> {
        match expr {
            Expr::Path(path) => self.types.locals.get(&path.path.get_ident()?.to_string()).cloned(),
            Expr::Macro(expr_macro) if Self::is_selection_macro(&expr_macro.mac) => {
                let args = expr_macro.mac.parse_body_with(SelectionMacroArgs::parse).ok()?;
                let ty = &args.selected_ty;
                Some(quote::quote!(#ty).to_string())
            }
            Expr::Macro(expr_macro) if Self::is_reference_macro(&expr_macro.mac) => {
                self.view_type(&Self::reference_macro_arg(&expr_macro.mac)?)
            }
            Expr::MethodCall(call) if call.method == "clone" => self.view_type(&call.receiver),
            Expr::Paren(paren) => self.view_type(&paren.expr),
            Expr::Reference(reference) => self.view_type(&reference.expr),
            Expr::Try(try_expr) => self.view_type(&try_expr.expr),
            _ => None,
        }
    }

    fn collect_static_path(&self, expr: &Expr, path: &mut Vec<SelectorSegment>) -> Option<()> {
        match expr {
            Expr::Field(field) => {
                self.collect_static_path(&field.base, path)?;
                path.push(match &field.member {
                    syn::Member::Named(ident) => SelectorSegment::Field(ident.to_string()),
                    syn::Member::Unnamed(index) => SelectorSegment::Index(u64::from(index.index)),
                });
                Some(())
            }
            Expr::Index(index) => {
                self.collect_static_path(&index.expr, path)?;
                match index.index.as_ref() {
                    Expr::Lit(ExprLit {
                        lit: Lit::Int(value),
                        ..
                    }) => path.push(SelectorSegment::Index(value.base10_parse().ok()?)),
                    Expr::Range(range) => {
                        // A literal bound, or one the macro evaluates at run
                        // time (`(start) as u64`) from a constant in scope.
                        let bound = |expr: &Option<Box<Expr>>| self.eval_const(expr.as_deref()?);
                        if !matches!(range.limits, syn::RangeLimits::HalfOpen(_)) {
                            return None;
                        }
                        path.push(SelectorSegment::Range {
                            start: bound(&range.start)?,
                            end: bound(&range.end)?,
                        });
                    }
                    _ => return None,
                }
                Some(())
            }
            // The selection's base — the root binding, possibly `x.clone()` —
            // contributes no segments, as in the macro.
            _ => Some(()),
        }
    }

    /// Evaluate an integer constant expression over the constants in scope:
    /// literals, constant names, parentheses, `as` casts to an unsigned type
    /// the value fits, and checked `+ - * / %`. `None` for anything else,
    /// including arithmetic that would overflow or divide by zero.
    fn eval_const(&self, expr: &Expr) -> Option<u64> {
        match expr {
            Expr::Lit(ExprLit {
                lit: Lit::Int(value),
                ..
            }) => value.base10_parse::<u64>().ok(),
            Expr::Path(path) => self.constants.get(&path.path.get_ident()?.to_string()).copied(),
            Expr::Paren(paren) => self.eval_const(&paren.expr),
            Expr::Group(group) => self.eval_const(&group.expr),
            Expr::Cast(cast) => {
                let value = self.eval_const(&cast.expr)?;
                let syn::Type::Path(ty) = cast.ty.as_ref() else {
                    return None;
                };
                let max = match ty.path.get_ident()?.to_string().as_str() {
                    "u8" => u64::from(u8::MAX),
                    "u16" => u64::from(u16::MAX),
                    "u32" => u64::from(u32::MAX),
                    "u64" | "usize" => u64::MAX,
                    _ => return None,
                };
                (value <= max).then_some(value)
            }
            Expr::Binary(binary) => {
                let left = self.eval_const(&binary.left)?;
                let right = self.eval_const(&binary.right)?;
                match binary.op {
                    syn::BinOp::Add(_) => left.checked_add(right),
                    syn::BinOp::Sub(_) => left.checked_sub(right),
                    syn::BinOp::Mul(_) => left.checked_mul(right),
                    syn::BinOp::Div(_) => left.checked_div(right),
                    syn::BinOp::Rem(_) => left.checked_rem(right),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// The file's integer `const` items: what a body can name as a range
    /// bound before it binds any of its own.
    fn file_constants(ast: &syn::File) -> HashMap<String, u64> {
        let mut visitor = Self::with_types(TypeContext::default());
        visitor.record_consts(ast.items.iter().filter_map(|item| match item {
            syn::Item::Const(item) => Some(item),
            _ => None,
        }));
        visitor.constants
    }

    /// Bring one scope's `const` items into scope together. An item may name
    /// one declared after it, so they are evaluated until none more resolve,
    /// not in source order. Each shadows the outer constant of its name; one
    /// that never resolves (a cycle, an unsupported expression) is unknown.
    fn record_consts<'a>(&mut self, items: impl IntoIterator<Item = &'a syn::ItemConst>) {
        let mut pending: Vec<&syn::ItemConst> = items.into_iter().collect();
        for item in &pending {
            self.constants.remove(&item.ident.to_string());
        }
        loop {
            let before = pending.len();
            pending.retain(|item| match self.eval_const(&item.expr) {
                Some(value) => {
                    self.constants.insert(item.ident.to_string(), value);
                    false
                }
                None => true,
            });
            if pending.len() == before {
                break;
            }
        }
    }

    /// Every name a pattern binds, for forgetting the constants it shadows.
    fn pattern_bindings(pat: &Pat, names: &mut Vec<String>) {
        match pat {
            Pat::Ident(pat_ident) => names.push(pat_ident.ident.to_string()),
            Pat::Type(pat_type) => Self::pattern_bindings(&pat_type.pat, names),
            Pat::Tuple(tuple) => tuple
                .elems
                .iter()
                .for_each(|elem| Self::pattern_bindings(elem, names)),
            Pat::TupleStruct(tuple) => tuple
                .elems
                .iter()
                .for_each(|elem| Self::pattern_bindings(elem, names)),
            Pat::Struct(pat_struct) => pat_struct
                .fields
                .iter()
                .for_each(|field| Self::pattern_bindings(&field.pat, names)),
            Pat::Reference(reference) => Self::pattern_bindings(&reference.pat, names),
            Pat::Slice(slice) => slice
                .elems
                .iter()
                .for_each(|elem| Self::pattern_bindings(elem, names)),
            Pat::Paren(paren) => Self::pattern_bindings(&paren.pat, names),
            Pat::Or(or) => or
                .cases
                .iter()
                .for_each(|case| Self::pattern_bindings(case, names)),
            _ => {}
        }
    }

    /// Whether `expr` is the bare path `name`.
    fn expr_path_is(expr: &Expr, name: &str) -> bool {
        matches!(expr, Expr::Path(path) if path.path.is_ident(name))
    }

    /// Converts an expression to its string representation for argument capture
    fn expr_to_string(expr: &Expr) -> String {
        quote::quote!(#expr).to_string()
    }

    /// Parse a canonical call macro token stream into (callee, arguments).
    ///
    /// The macro syntax is `call!(callee_fn, arg1, arg2, ...)`. The token stream
    /// is a comma-separated list; the first token is the callee identifier.
    fn parse_call_macro_args(&self, mac: &syn::Macro,
    ) -> Option<(String, Vec<String>, Vec<CallArgumentKind>, Option<u64>, Option<SiteOutputKind>, bool)> {
        if matches!(Self::macro_call_kind(mac), Some(CallKind::RecursiveTile)) {
            return self.parse_recur_call_macro_args(mac);
        }
        if matches!(
            Self::macro_call_kind(mac),
            Some(CallKind::RecursiveSequence)
        ) {
            return self.parse_recur_sequence_call_macro_args(mac)
                .map(|(callee, args, kinds, output, carries_state)| {
                    (callee, args, kinds, None, output, carries_state)
                });
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
            .map(|expr| self.classify_argument(expr))
            .collect();

        Some((callee, arguments, argument_kinds, None, None, false))
    }

    fn parse_recur_call_macro_args(&self, mac: &syn::Macro,
    ) -> Option<(String, Vec<String>, Vec<CallArgumentKind>, Option<u64>, Option<SiteOutputKind>, bool)> {
        struct RecurCallInput {
            tile: syn::Ident,
            input: Expr,
            // Chunk size is a call-site control parameter, not a tile input:
            // it groups iterations but binds no data source, so it is pinned
            // into the CFS item and excluded from the extracted arguments.
            chunk: Option<Expr>,
            state: Option<Expr>,
            /// `None`: no output; `Some(None)`: bare `output` (create);
            /// `Some(Some(base))`: `output = base` (derive).
            output: Option<Option<Expr>>,
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
                let output = CallVisitor::parse_site_output(input)?;

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
        // A site whose own output *is* its carried state: `state` with no
        // `output`. That is the shape whose final state the trace can pin, by
        // comparing the site's recorded output against the chain's last value.
        // Whether the site carries state: the CFS records it, so the guest can
        // find a stored seed at `RecurStart` (D5b). `state_is_output` follows
        // from it and `output` at the visitor.
        let carries_state = parsed.state.is_some();
        let mut arguments = vec![Self::expr_to_string(&parsed.input)];
        let mut argument_kinds = vec![self.classify_argument(&parsed.input)];

        if let Some(state) = parsed.state {
            arguments.push(Self::expr_to_string(&state));
            argument_kinds.push(self.classify_argument(&state));
        }

        let output = self.push_site_output(parsed.output, &mut arguments, &mut argument_kinds);

        for expr in parsed.args {
            arguments.push(Self::expr_to_string(&expr));
            argument_kinds.push(self.classify_argument(&expr));
        }

        Some((
            parsed.tile.to_string(),
            arguments,
            argument_kinds,
            chunk,
            output,
            carries_state,
        ))
    }

    fn parse_recur_sequence_call_macro_args(&self, mac: &syn::Macro,
    ) -> Option<(String, Vec<String>, Vec<CallArgumentKind>, Option<SiteOutputKind>, bool)> {
        struct RecurSequenceCallInput {
            sequence: syn::Ident,
            input: Expr,
            state: Option<Expr>,
            output: Option<Option<Expr>>,
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

                let output = CallVisitor::parse_site_output(input)?;

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
        // Whether the site carries state: the CFS records it, so the guest can
        // find a stored seed at `RecurStart` (D5b). `state_is_output` follows
        // from it and `output` at the visitor.
        let carries_state = parsed.state.is_some();
        let mut arguments = vec![Self::expr_to_string(&parsed.input)];
        let mut argument_kinds = vec![self.classify_argument(&parsed.input)];

        if let Some(state) = parsed.state {
            arguments.push(Self::expr_to_string(&state));
            argument_kinds.push(self.classify_argument(&state));
        }

        let output = self.push_site_output(parsed.output, &mut arguments, &mut argument_kinds);

        for expr in parsed.args {
            arguments.push(Self::expr_to_string(&expr));
            argument_kinds.push(self.classify_argument(&expr));
        }

        Some((
            parsed.sequence.to_string(),
            arguments,
            argument_kinds,
            output,
            carries_state,
        ))
    }

    /// Parse an optional `output,` (create) or `output = base,` (derive).
    fn parse_site_output(input: ParseStream) -> syn::Result<Option<Option<Expr>>> {
        if !input.peek(syn::Ident) {
            return Ok(None);
        }
        let fork = input.fork();
        let ident: syn::Ident = fork.parse()?;
        if ident != "output" {
            return Ok(None);
        }
        let _: syn::Ident = input.parse()?;
        if input.parse::<Option<Token![=]>>()?.is_some() {
            let base: Expr = input.parse()?;
            input.parse::<Token![,]>()?;
            return Ok(Some(Some(base)));
        }
        input.parse::<Token![,]>()?;
        Ok(Some(None))
    }

    /// Record a site's `output` as its call argument. A created object has no
    /// expression, so it takes an inline slot — the site still records one
    /// value for it, and the CFS one source.
    fn push_site_output(&self, output: Option<Option<Expr>>,
        arguments: &mut Vec<String>,
        argument_kinds: &mut Vec<CallArgumentKind>,
    ) -> Option<SiteOutputKind> {
        match output? {
            None => {
                arguments.push("output".to_string());
                argument_kinds.push(CallArgumentKind::Inline);
                Some(SiteOutputKind::Create)
            }
            Some(base) => {
                arguments.push(Self::expr_to_string(&base));
                argument_kinds.push(self.classify_argument(&base));
                Some(SiteOutputKind::Derive)
            }
        }
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

    fn classify_argument(&self, expr: &Expr) -> CallArgumentKind {
        match Self::expr_root_ident(expr) {
            Some(root) => CallArgumentKind::Rooted {
                root: self.binding_of(&root),
                path: self.argument_path(expr),
            },
            None => CallArgumentKind::Inline,
        }
    }

    /// The static selector path an argument expression appends to its root —
    /// what the runtime appends to the root's selector when the argument is
    /// bound. Only `select!` narrows: `clone!`, `into_ref!`, `.clone()`, `&x`
    /// and parentheses re-point at the same value, so they pass through to it.
    /// That value may itself be a selection: `clone!(select!(T, x.f))` keeps
    /// the selected reference, so its path is `[f]`, not empty.
    ///
    /// A raw `x.f` or `x[0]` does not compile on a sequence binding (an
    /// `AuthRef` has no `Deref`), so it is read as passing `x` whole rather
    /// than given a path no run could record.
    fn argument_path(&self, expr: &Expr) -> Option<Vec<SelectorSegment>> {
        match expr {
            Expr::Macro(expr_macro) if Self::is_selection_macro(&expr_macro.mac) => {
                self.selection_macro_path(&expr_macro.mac)
            }
            Expr::Macro(expr_macro) if Self::is_reference_macro(&expr_macro.mac) => {
                self.argument_path(&Self::reference_macro_arg(&expr_macro.mac)?)
            }
            Expr::Paren(paren) => self.argument_path(&paren.expr),
            Expr::Reference(reference) => self.argument_path(&reference.expr),
            Expr::Try(try_expr) => self.argument_path(&try_expr.expr),
            Expr::MethodCall(call) => self.argument_path(&call.receiver),
            _ => Some(Vec::new()),
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
        Self::expr_root_ident(&Self::reference_macro_arg(mac)?)
    }

    /// The single expression a reference macro wraps: `x` in `clone!(x)`,
    /// `select!(T, x.f)` in `clone!(select!(T, x.f))`.
    fn reference_macro_arg(mac: &syn::Macro) -> Option<Expr> {
        mac.parse_body_with(Expr::parse).ok()
    }

    /// The data-sourced index names of the selection a view expression wraps,
    /// looking through the forms that re-point at the same value (`clone!`,
    /// `into_ref!`, `.clone()`, `&x`, parentheses, `x?`). Empty when there is
    /// no selection or its indexes are all literal.
    fn view_index_roots(expr: &Expr) -> Vec<String> {
        match expr {
            Expr::Macro(expr_macro) if Self::is_selection_macro(&expr_macro.mac) => {
                Self::selection_macro_index_roots(&expr_macro.mac)
            }
            Expr::Macro(expr_macro) if Self::is_reference_macro(&expr_macro.mac) => {
                Self::reference_macro_arg(&expr_macro.mac)
                    .map(|inner| Self::view_index_roots(&inner))
                    .unwrap_or_default()
            }
            Expr::Paren(paren) => Self::view_index_roots(&paren.expr),
            Expr::Reference(reference) => Self::view_index_roots(&reference.expr),
            Expr::Try(try_expr) => Self::view_index_roots(&try_expr.expr),
            Expr::MethodCall(call) => Self::view_index_roots(&call.receiver),
            _ => Vec::new(),
        }
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
        let source_name = Self::extract_binding_name(&node.pat);
        // What this `let` records is named for the binding it introduces, and
        // what its initializer reads for the bindings already in scope.
        let binding_name = source_name.as_ref().map(|name| self.bind_name(name));

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
                        let root = self.binding_of(&root);
                        self.selection_aliases.push((name.clone(), root));
                    }
                    if let Some(path) = self.selection_macro_path(&expr_macro.mac) {
                        self.selection_paths.push((name.clone(), path));
                    }
                    let index_roots =
                        self.bindings_of(Self::selection_macro_index_roots(&expr_macro.mac));
                    if !index_roots.is_empty() {
                        self.selection_index_sources
                            .push((name.clone(), index_roots));
                    }
                } else if Self::is_reference_macro(&expr_macro.mac) {
                    if let Some(root) = Self::reference_macro_root(&expr_macro.mac) {
                        let root = self.binding_of(&root);
                        self.record_view_alias(name, root, &init.expr);
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
                        let root = self.binding_of(&root);
                        self.record_view_alias(name, root, &init.expr);
                    }
                }
            }
        }

        // The local's declared type, for lowering later byte-page selections:
        // a view's is known from the expression, a call result's from the
        // callee's signature once the call has been recorded.
        let view_ty = node.init.as_ref().and_then(|init| self.view_type(&init.expr));
        let calls_before = self.call_infos.len();

        // Set the current binding context before visiting the initializer
        self.current_binding = binding_name.clone();

        // Visit the initializer expression (this will trigger visit_expr_call /
        // visit_expr_macro if there's a call or macro invocation)
        if let Some(init) = &node.init {
            self.visit_expr(&init.expr);
        }

        // Clear the binding context after processing
        self.current_binding = None;

        // A `let` binding an integer constant brings it into scope for later
        // range bounds; any other `let` shadows whatever constant it names.
        // A `let mut` is no constant: a later assignment would leave the
        // declared bound stale while the macro records the new value.
        let constant = node
            .init
            .as_ref()
            .and_then(|init| self.eval_const(&init.expr));
        let mut bound = Vec::new();
        Self::pattern_bindings(&node.pat, &mut bound);
        for name in bound {
            self.constants.remove(&name);
            // Every name the pattern binds shadows the old binding; one that
            // is not the `let`'s single name records nothing, so it resolves
            // as a value with no upstream.
            if source_name.as_deref() != Some(name.as_str()) {
                let binding = self.bind_name(&name);
                self.enter_binding(&name, binding);
            }
        }
        if let (Some(name), Some(binding)) = (source_name.as_ref(), binding_name.as_ref()) {
            self.enter_binding(name, binding.clone());
        }
        if let (Some(name), Some(value)) = (source_name.as_ref(), constant) {
            if matches!(
                Self::unwrap_type_pat(&node.pat),
                Pat::Ident(pat_ident) if pat_ident.mutability.is_none()
            ) {
                self.constants.insert(name.clone(), value);
            }
        }

        if let (Some(name), Some(binding)) = (source_name, binding_name) {
            let call_ty = self.call_infos[calls_before..]
                .iter()
                .rev()
                .find(|call| call.result_binding.as_deref() == Some(binding.as_str()))
                .and_then(|call| self.types.fn_outputs.get(&call.callee).cloned());
            // A shadowing `let` with no known type must not keep the old one.
            match view_ty.or(call_ty) {
                Some(ty) => self.types.locals.insert(name, ty),
                None => self.types.locals.remove(&name),
            };
        }
    }

    /// Parameters are a body's first bindings: a `let` of the same name
    /// shadows them.
    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        for input in &node.sig.inputs {
            if let FnArg::Typed(pat_type) = input {
                let mut names = Vec::new();
                Self::pattern_bindings(&pat_type.pat, &mut names);
                for name in names {
                    let binding = self.bind_name(&name);
                    self.enter_binding(&name, binding);
                }
            }
        }
        syn::visit::visit_item_fn(self, node);
    }

    /// A block's `const` items are in scope throughout it, so `visit_block`
    /// records them all before its statements.
    fn visit_item_const(&mut self, _node: &'ast syn::ItemConst) {}

    /// A nested block's `let`s and `const`s go out of scope at its end: the
    /// constants, local types and bindings they bound or shadowed revert to
    /// the outer ones. The body's own block keeps them: the return is classified after
    /// the visit.
    fn visit_block(&mut self, node: &'ast syn::Block) {
        self.block_depth += 1;
        let outer = (self.block_depth > 1).then(|| {
            (
                self.constants.clone(),
                self.types.locals.clone(),
                self.scope.clone(),
            )
        });
        self.record_consts(node.stmts.iter().filter_map(|stmt| match stmt {
            syn::Stmt::Item(syn::Item::Const(item)) => Some(item),
            _ => None,
        }));
        syn::visit::visit_block(self, node);
        if let Some((constants, locals, scope)) = outer {
            self.constants = constants;
            self.types.locals = locals;
            self.scope = scope;
        }
        self.block_depth -= 1;
    }

    fn visit_expr_macro(&mut self, node: &'ast ExprMacro) {
        if let Some(call_kind) = Self::macro_call_kind(&node.mac) {
            if let Some((
                callee,
                arguments,
                argument_kinds,
                chunk,
                output,
                carries_state,
            )) =
                self.parse_call_macro_args(&node.mac)
            {
                let result_binding = self.current_binding.take();
                self.call_infos.push(CallInfo {
                    callee,
                    arguments,
                    argument_kinds,
                    result_binding,
                    call_kind,
                    chunk,
                    // A site whose own output *is* its carried state: `state`
                    // with no `output`.
                    state_is_output: carries_state && output.is_none(),
                    output,
                    carries_state,
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
            if let Some((
                callee,
                arguments,
                argument_kinds,
                chunk,
                output,
                carries_state,
            )) =
                self.parse_call_macro_args(&node.mac)
            {
                // current_binding is None here — bare statements have no let binding.
                self.call_infos.push(CallInfo {
                    callee,
                    arguments,
                    argument_kinds,
                    result_binding: None,
                    call_kind,
                    chunk,
                    // A site whose own output *is* its carried state: `state`
                    // with no `output`.
                    state_is_output: carries_state && output.is_none(),
                    output,
                    carries_state,
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
        rooted_at(root, Some(Vec::new()))
    }

    fn rooted_at(root: &str, path: Option<Vec<SelectorSegment>>) -> CallArgumentKind {
        CallArgumentKind::Rooted {
            root: root.to_string(),
            path,
        }
    }

    fn classify_return_of(code: &str) -> Option<ReturnExpr> {
        let item: syn::ItemFn = syn::parse_str(code).expect("Failed to parse test fn");
        CallVisitor::new().classify_return(&item.block)
    }

    fn rooted_return(root: &str) -> Option<ReturnExpr> {
        rooted_return_at(root, vec![])
    }

    fn rooted_return_at(root: &str, path: Vec<SelectorSegment>) -> Option<ReturnExpr> {
        Some(ReturnExpr::Rooted {
            root: root.to_string(),
            path,
        })
    }

    fn field(name: &str) -> SelectorSegment {
        SelectorSegment::Field(name.to_string())
    }

    /// Every form a value-returning `main` ends with in `examples/` and
    /// `raster-inference`: a binding, `Ok(binding)`, a tail call, a selection.
    #[test]
    fn return_forms_that_bind() {
        assert_eq!(
            classify_return_of("fn main() -> u64 { let s = call!(plan, b); s }"),
            rooted_return("s")
        );
        assert_eq!(
            classify_return_of("fn main() -> Result<X> { let p = call!(f, a); Ok(p) }"),
            rooted_return("p")
        );
        assert_eq!(
            classify_return_of("fn main() -> u64 { call!(plan, b) }"),
            Some(ReturnExpr::TailCall)
        );
        assert_eq!(
            classify_return_of("fn main() -> Result<X> { Ok(call!(plan, b)?) }"),
            Some(ReturnExpr::TailCall)
        );
        assert_eq!(
            classify_return_of("fn main() -> u64 { let s = call!(f, b); select!(u64, s.count) }"),
            rooted_return_at("s", vec![field("count")])
        );
        // The path is lowered as the `select!` macro lowers it.
        assert_eq!(
            classify_return_of(
                "fn main() -> u64 { let s = call!(f, b); select!(u64, s.rows[2].cells.0) }"
            ),
            rooted_return_at(
                "s",
                vec![
                    field("rows"),
                    SelectorSegment::Index(2),
                    field("cells"),
                    SelectorSegment::Index(0)
                ]
            )
        );
        assert_eq!(
            classify_return_of("fn main() -> X { let s = call!(f, b); select!(X, s.rows[1..3]) }"),
            rooted_return_at("s", vec![field("rows"), SelectorSegment::Range { start: 1, end: 3 }])
        );
        // A `.clone()` base contributes nothing, as in the macro.
        assert_eq!(
            classify_return_of("fn main() -> u32 { let s = call!(f, b); select!(u32, s.clone().count) }"),
            rooted_return_at("s", vec![field("count")])
        );
        assert_eq!(
            classify_return_of("fn main() -> X { let s = call!(f, b); clone!(s) }"),
            rooted_return("s")
        );
        // A cloned selection returns the selected field, not the whole binding.
        assert_eq!(
            classify_return_of("fn main() -> u64 { let s = call!(f, b); clone!(select!(u64, s.count)) }"),
            rooted_return_at("s", vec![field("count")])
        );
        assert_eq!(
            classify_return_of("fn main() -> u64 { let s = call!(f, b); select!(u64, s.count).clone() }"),
            rooted_return_at("s", vec![field("count")])
        );
        assert_eq!(
            classify_return_of("fn main() -> u64 { let s = call!(f, b); return s; }"),
            rooted_return("s")
        );
    }

    #[test]
    fn return_forms_that_do_not_bind() {
        assert!(matches!(
            classify_return_of("fn main() -> u64 { 42 }"),
            Some(ReturnExpr::Unbound { .. })
        ));
        assert!(matches!(
            classify_return_of("fn main() -> R { let d = call!(f, new!(R)); finalize(d) }"),
            Some(ReturnExpr::Unbound { .. })
        ));
        // A data-sourced index would need its citation carried to ProgramEnd.
        assert!(matches!(
            classify_return_of("fn main() -> u64 { let s = call!(f, b); select!(u64, s.rows[i]) }"),
            Some(ReturnExpr::Unbound { .. })
        ));
        // A raw field access would narrow without a path: not bound, rather
        // than bound to the whole object.
        assert!(matches!(
            classify_return_of("fn main() -> u64 { let s = call!(f, b); s.count }"),
            Some(ReturnExpr::Unbound { .. })
        ));
        // A unit body returns nothing to bind.
        assert_eq!(classify_return_of("fn main() { call!(f, b); }"), None);
    }

    /// A reference macro keeps the selection it wraps. `clone!(select!(T,
    /// data.name))` hands the tile `data.name`, so the argument's path is
    /// `[name]`: an empty path would bind the whole of `data`, which the
    /// runtime never passes, and reject the honest step.
    #[test]
    fn cloned_selection_arguments_keep_their_paths() {
        let calls = parse_calls(
            "fn s(data: D) { \
             call!(t, clone!(select!(String, data.name))); \
             call!(t, into_ref!(select!(String, data.name))); \
             call!(t, clone!(select!(String, data.clone().name))); \
             call!(t, select!(String, data.name).clone()); \
             call!(t, clone!(data)); \
             call!(t, clone!(select!(u64, data.rows[i]))); }",
        );
        let kinds: Vec<CallArgumentKind> = calls
            .into_iter()
            .map(|call| call.argument_kinds[0].clone())
            .collect();
        let name = || Some(vec![field("name")]);
        assert_eq!(
            kinds,
            vec![
                rooted_at("data", name()),
                rooted_at("data", name()),
                rooted_at("data", name()),
                rooted_at("data", name()),
                rooted("data"),
                // A data-sourced index still has no static path.
                rooted_at("data", None),
            ]
        );
    }

    /// The same for a local bound to a cloned selection: the alias carries
    /// the selection's path and index sources, as a bare `select!` alias does.
    #[test]
    fn cloned_selection_aliases_keep_their_paths() {
        let file: syn::File = syn::parse_str(
            "fn seq() { let s = call!(f, b); \
             let n = clone!(select!(String, s.name)); \
             let k = select!(u64, s.count).clone(); \
             let c = clone!(s); \
             let q = clone!(select!(u64, s.rows[i])); }",
        )
        .expect("parse");
        let mut visitor = CallVisitor::new();
        visitor.visit_file(&file);
        let aliases = visitor.get_selection_aliases();
        for name in ["n", "k", "c", "q"] {
            assert!(
                aliases.contains(&(name.to_string(), "s".to_string())),
                "{name} should alias s"
            );
        }
        assert_eq!(
            visitor.get_selection_paths(),
            vec![
                ("n".to_string(), vec![field("name")]),
                ("k".to_string(), vec![field("count")]),
                ("c".to_string(), vec![]),
                // `q`'s index is data-sourced: no static path is recorded.
            ]
        );
        assert_eq!(
            visitor.get_selection_index_sources(),
            vec![("q".to_string(), vec!["i".to_string()])]
        );
    }

    /// A visitor reading `code` with `model: Model` in scope, `Model` holding a
    /// `weights: Bytes<4>` region, and a tile `load` returning a `Model`.
    fn byte_page_visitor(code: &str) -> CallVisitor {
        let structs = vec![StructAstItem {
            name: "Model".to_string(),
            fields: vec![StructFieldAst {
                name: "weights".to_string(),
                ty: "Bytes < 4 >".to_string(),
            }],
        }];
        let outputs = HashMap::from([("load".to_string(), "Model".to_string())]);
        let types = TypeContext::for_function(
            Rc::new(structs),
            Rc::new(outputs),
            [("model".to_string(), "Model".to_string())],
        );
        let file: syn::File = syn::parse_str(code).expect("parse");
        let mut visitor = CallVisitor::with_types(types);
        visitor.visit_file(&file);
        visitor
    }

    /// `select!` reads a byte offset into a `Bytes<N>` as a page of its
    /// `pages` list, `[weights, 4]` becoming `[weights, pages, 1]`; the CFS must
    /// declare the path the macro records, or the guest's exact path checks
    /// reject the honest step.
    #[test]
    fn byte_page_selections_lower_like_the_macro() {
        let visitor = byte_page_visitor(
            "fn s(model: Model) { \
             call!(t, select!(BytesPage, model.weights[4])); \
             call!(t, select!(Block<BytesPage>, model.clone().weights[4..12])); \
             call!(t, clone!(select!(BytesPage, model.weights[8]))); \
             let w = select!(Bytes<4>, model.weights); \
             call!(t, select!(BytesPage, w[12])); \
             let m = call!(load, model); \
             call!(t, select!(BytesPage, m.weights[0])); \
             call!(t, select!(BytesPage, model.weights.pages[2])); \
             call!(t, select!(List<BytesPage>, model.weights.pages)); \
             call!(t, select!(BytesPage, model.weights[i])); }",
        );
        assert!(visitor.get_selection_errors().is_empty(), "{:?}", visitor.get_selection_errors());
        let pages = || field("pages");
        let kinds: Vec<CallArgumentKind> = visitor
            .get_call_infos()
            .into_iter()
            .filter(|call| call.callee == "t")
            .map(|call| call.argument_kinds[0].clone())
            .collect();
        assert_eq!(
            kinds,
            vec![
                rooted_at("model", Some(vec![field("weights"), pages(), SelectorSegment::Index(1)])),
                rooted_at(
                    "model",
                    Some(vec![field("weights"), pages(), SelectorSegment::Range { start: 1, end: 3 }])
                ),
                rooted_at("model", Some(vec![field("weights"), pages(), SelectorSegment::Index(2)])),
                // The region itself, through an alias typed by its `select!`.
                rooted_at("w", Some(vec![pages(), SelectorSegment::Index(3)])),
                // A tile output, typed by the tile's signature.
                rooted_at("m", Some(vec![field("weights"), pages(), SelectorSegment::Index(0)])),
                // Already a page index: nothing to lower.
                rooted_at("model", Some(vec![field("weights"), pages(), SelectorSegment::Index(2)])),
                rooted_at("model", Some(vec![field("weights"), pages()])),
                // A data-sourced index has no static path either way.
                rooted_at("model", None),
            ]
        );
    }

    /// A binding inside a nested block does not outlive it: the outer `model`
    /// is in scope again after the block, with its type.
    #[test]
    fn byte_page_selection_sees_the_outer_type_after_a_shadowing_block() {
        let visitor = byte_page_visitor(
            "fn s(model: Model) { \
             { let model = 0usize; } \
             call!(t, select!(BytesPage, model.weights[4])); }",
        );
        assert!(visitor.get_selection_errors().is_empty(), "{:?}", visitor.get_selection_errors());
        assert_eq!(
            visitor.get_call_infos()[0].argument_kinds,
            vec![rooted_at(
                "model",
                Some(vec![field("weights"), field("pages"), SelectorSegment::Index(1)])
            )]
        );
    }

    /// A byte offset whose region's page size the source does not show is an
    /// error, not a guessed path.
    #[test]
    fn unresolvable_byte_page_selection_is_an_error() {
        let visitor = byte_page_visitor(
            "fn s(other: Unknown) { call!(t, select!(BytesPage, other.weights[4])); }",
        );
        let errors = visitor.get_selection_errors();
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].contains(".pages[k]"), "{errors:?}");
    }

    #[test]
    fn selection_aliases_record_their_static_paths() {
        let file: syn::File = syn::parse_str(
            "fn seq() { let s = call!(f, b); let w = select!(W, s.window); \
             let m = select!(u64, w.max); let c = clone!(m); \
             let r = select!(u64, s.rows[i]); }",
        )
        .expect("parse");
        let mut visitor = CallVisitor::new();
        visitor.visit_file(&file);
        assert_eq!(
            visitor.get_selection_paths(),
            vec![
                ("w".to_string(), vec![field("window")]),
                ("m".to_string(), vec![field("max")]),
                ("c".to_string(), vec![]),
                // `r`'s index is data-sourced: no static path is recorded.
            ]
        );
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
            "fn seq() { let result = call_recur!(tile = build, input = items, output, args = (needle,)); }",
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
            "fn seq() { let result = call_recur!(tile = collect, input = items, chunk = 2, output, args = (title,)); }",
        );
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].callee, "collect");
        assert_eq!(calls[0].call_kind, CallKind::RecursiveTile);
        assert_eq!(calls[0].result_binding.as_deref(), Some("result"));
        // A created object takes an inline argument slot.
        assert_eq!(calls[0].arguments, vec!["items", "output", "title"]);
        assert_eq!(calls[0].argument_kinds[1], CallArgumentKind::Inline);
        assert_eq!(calls[0].output, Some(SiteOutputKind::Create));
        assert_eq!(calls[0].chunk, Some(2));
    }

    #[test]
    fn test_call_recur_without_chunk_has_no_chunk() {
        let calls = parse_calls(
            "fn seq() { let result = call_recur!(tile = build, input = items, output, args = (needle,)); }",
        );
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].chunk, None);
    }

    #[test]
    fn test_call_recur_derived_output_is_its_base_argument() {
        let calls = parse_calls(
            "fn seq() { let doc = call_recur!(tile = build, input = items, chunk = 4, output = base, args = (needle,)); }",
        );
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].callee, "build");
        assert_eq!(calls[0].arguments, vec!["items", "base", "needle"]);
        assert_eq!(
            calls[0].argument_kinds[1],
            CallArgumentKind::Rooted {
                root: "base".to_string(),
                path: Some(Vec::new()),
            }
        );
        assert_eq!(calls[0].output, Some(SiteOutputKind::Derive));
        assert_eq!(calls[0].chunk, Some(4));
    }

    #[test]
    #[should_panic(expected = "must be an integer literal")]
    fn test_call_recur_rejects_non_literal_chunk() {
        parse_calls(
            "fn seq() { let result = call_recur!(tile = collect, input = items, chunk = size, output, args = (title,)); }",
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
        // provenance and the path it narrows by; a literal has neither.
        assert_eq!(
            calls[0].argument_kinds,
            vec![
                rooted_at("source", Some(vec![SelectorSegment::Field("name".into())])),
                CallArgumentKind::Inline,
            ]
        );
    }

    /// A data-sourced index has no static path: the argument stays rooted, and
    /// the path is unknown rather than empty.
    #[test]
    fn test_argument_path_is_unknown_through_a_data_sourced_index() {
        let calls = parse_calls(
            r#"fn seq() { let r = call!(t, select!(Row, table.rows[wanted]), select!(Row, table.rows[3].cells[0..2])); }"#,
        );
        assert_eq!(
            calls[0].argument_kinds,
            vec![
                rooted_at("table", None),
                rooted_at(
                    "table",
                    Some(vec![
                        SelectorSegment::Field("rows".into()),
                        SelectorSegment::Index(3),
                        SelectorSegment::Field("cells".into()),
                        SelectorSegment::Range { start: 0, end: 2 },
                    ])
                ),
            ]
        );
    }

    /// The single function in `code`, parsed the way a project file is: the
    /// file's `const` items in scope, shadowed by the function's parameters.
    fn parse_only_function(code: &str) -> FunctionAstItem {
        let file: syn::File = syn::parse_str(code).expect("Failed to parse test code");
        let mut functions = ProjectAst::parse_functions(
            &file,
            PathBuf::from("test.rs"),
            &Rc::new(Vec::new()),
            &Rc::new(HashMap::new()),
        );
        assert_eq!(functions.len(), 1);
        functions.remove(0)
    }

    fn rows_range(start: u64, end: u64) -> Vec<SelectorSegment> {
        vec![
            SelectorSegment::Field("rows".into()),
            SelectorSegment::Range { start, end },
        ]
    }

    /// `select!` records a range with local bounds as the values they hold, so
    /// the declared path must hold them too — not read the whole of `table`.
    #[test]
    fn test_range_bounds_bound_by_locals_are_evaluated() {
        let function = parse_only_function(
            r#"fn main(table: Table) {
                let start = 0usize;
                let end: usize = 2;
                call!(consume, select!(Block<Row>, table.rows[start..end]));
                let rows = select!(Block<Row>, table.rows[(start + 1)..(end * 2) as usize]);
                call!(consume, rows);
            }"#,
        );
        assert!(function.selection_errors.is_empty(), "{:?}", function.selection_errors);
        assert_eq!(
            function.call_infos[0].argument_kinds,
            vec![rooted_at("table", Some(rows_range(0, 2)))]
        );
        assert_eq!(
            function.selection_paths,
            vec![("rows".to_string(), rows_range(1, 4))]
        );
    }

    #[test]
    fn test_range_bounds_read_file_and_body_constants() {
        let function = parse_only_function(
            r#"
            const START: usize = 1;
            const LEN: usize = START + 2;
            fn main(table: Table) {
                const END: usize = START + LEN;
                call!(consume, select!(Block<Row>, table.rows[START..END]));
            }"#,
        );
        assert!(function.selection_errors.is_empty(), "{:?}", function.selection_errors);
        assert_eq!(
            function.call_infos[0].argument_kinds,
            vec![rooted_at("table", Some(rows_range(1, 4)))]
        );
    }

    /// A `const` may name one declared after it, at file or block scope, and
    /// a block's constants are in scope before their declaration.
    #[test]
    fn test_range_bounds_read_forward_constants() {
        let function = parse_only_function(
            r#"
            const END: usize = START + 2;
            const START: usize = 0;
            fn main(table: Table) {
                call!(consume, select!(Block<Row>, table.rows[START..END]));
                call!(consume, select!(Block<Row>, table.rows[LOW..HIGH]));
                const HIGH: usize = LOW + END;
                const LOW: usize = 1;
            }"#,
        );
        assert!(function.selection_errors.is_empty(), "{:?}", function.selection_errors);
        assert_eq!(
            function.call_infos[0].argument_kinds,
            vec![rooted_at("table", Some(rows_range(0, 2)))]
        );
        assert_eq!(
            function.call_infos[1].argument_kinds,
            vec![rooted_at("table", Some(rows_range(1, 3)))]
        );
    }

    /// A bound the compiler cannot evaluate leaves the path unknown, and the
    /// selection is an error rather than a read of the whole root.
    #[test]
    fn test_unevaluable_range_bounds_are_selection_errors() {
        let cases = [
            // A shadowing `let` with no constant value.
            r#"fn main(table: Table) {
                let start = 0usize;
                let start = call!(first, table);
                call!(consume, select!(Block<Row>, table.rows[start..2]));
            }"#,
            // A constant bound in a nested block is gone after it.
            r#"fn main(table: Table) {
                { let end = 2usize; }
                call!(consume, select!(Block<Row>, table.rows[0..end]));
            }"#,
            // A parameter shadows the file constant it names.
            r#"
            const END: usize = 2;
            fn main(table: Table, END: u64) {
                call!(consume, select!(Block<Row>, table.rows[0..END]));
            }"#,
            // A mutable local may be reassigned before the selection.
            r#"fn main(table: Table) {
                let mut end = 2usize;
                end = 3;
                call!(consume, select!(Block<Row>, table.rows[0..end]));
            }"#,
            // Constants that only name each other never resolve.
            r#"
            const START: usize = END - 2;
            const END: usize = START + 2;
            fn main(table: Table) {
                call!(consume, select!(Block<Row>, table.rows[START..END]));
            }"#,
            // Underflow.
            r#"fn main(table: Table) {
                call!(consume, select!(Block<Row>, table.rows[0..(1 - 2)]));
            }"#,
        ];
        for code in cases {
            let function = parse_only_function(code);
            assert_eq!(function.selection_errors.len(), 1, "{code}");
            assert_eq!(
                function.call_infos.last().unwrap().argument_kinds,
                vec![rooted_at("table", None)],
                "{code}"
            );
        }
    }

    /// A data-sourced index has no static path by design; its range bounds are
    /// not an error.
    #[test]
    fn test_unknown_range_under_a_data_sourced_index_is_not_an_error() {
        let function = parse_only_function(
            r#"fn main(table: Table, i: u64) {
                call!(consume, select!(Block<u64>, table.rows[i].cells[0..n]));
            }"#,
        );
        assert!(function.selection_errors.is_empty(), "{:?}", function.selection_errors);
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
                // Raw field access passes the root whole (it does not compile
                // on a sequence binding; only `select!` narrows).
                rooted("other"),
                rooted("third"),
                rooted_at("nested", Some(vec![SelectorSegment::Field("id".into())])),
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
