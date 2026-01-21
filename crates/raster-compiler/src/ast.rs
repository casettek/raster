use anyhow::Result;
use cargo_toml::Manifest;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use syn::{parse_file, visit::Visit, Attribute, Expr, ExprCall, ExprLit, FnArg, Lit, Meta, Pat};
use walkdir::WalkDir;

#[derive(Debug, Clone)]
pub struct ProjectAst {
    pub name: String,
    pub root_path: PathBuf,
    pub functions: Vec<FunctionAstItem>,
}

#[derive(Debug, Clone)]
pub struct FunctionAstItem {
    pub name: String,
    pub path: PathBuf,
    pub calls: Vec<String>,
    pub macros: Vec<MacroAstItem>,
    pub inputs: Vec<Type>,
    pub output: Option<Type>,
    pub signature: String,
}

type Type = String;

#[derive(Debug, Clone)]
pub struct MacroAstItem {
    pub name: String,
    pub args: HashMap<String, String>,
}

// TODO: Project AST Explorer should contain function with full path resulution (like mod's)

impl ProjectAst {
    pub fn new(project_root: &Path) -> Result<Self> {
        let cargo_toml_path = project_root.join("Cargo.toml");

        let manifest = Manifest::from_path(&cargo_toml_path).unwrap();

        let package = manifest.package.as_ref().unwrap();

        let files_paths = Self::find_all_rs_files(project_root);

        let functions = files_paths
            .iter()
            .flat_map(|path| Self::parse_file(&path))
            .collect::<Vec<FunctionAstItem>>();

        Ok(Self {
            name: package.name.clone(),
            root_path: project_root.to_path_buf(),
            functions,
        })
    }

    fn parse_file(path: &Path) -> Vec<FunctionAstItem> {
        let content = std::fs::read_to_string(path).unwrap();
        let ast = parse_file(&content).unwrap();
        let functions = Self::parse_functions(&ast, path.to_path_buf());

        functions
    }

    fn find_all_rs_files(project_root: &Path) -> Vec<PathBuf> {
        let src_dir = project_root.join("src");

        WalkDir::new(&src_dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map_or(false, |ext| ext == "rs"))
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
                    .map(|attr| Self::parse_macro(attr))
                    .collect();

                let inputs: Vec<Type> = func
                    .sig
                    .inputs
                    .iter()
                    .filter_map(|arg| {
                        if let FnArg::Typed(pat_type) = arg {
                            let ty = &pat_type.ty;
                            Some(quote::quote!(#ty).to_string())
                        } else {
                            None
                        }
                    })
                    .collect();

                let output = if let syn::ReturnType::Type(_, ty) = &func.sig.output {
                    Some(quote::quote!(#ty).to_string())
                } else {
                    None
                };

                let sig = &func.sig;
                let signature = quote::quote!(#sig).to_string();

                let mut visitor = CallVisitor::new();
                visitor.visit_item_fn(func);
                let calls = visitor.get_calls();
                let function_info = FunctionAstItem {
                    name,
                    path: path.clone(),
                    calls,
                    macros,
                    inputs,
                    output,
                    signature,
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
}

pub struct CallVisitor(Vec<String>);

impl CallVisitor {
    fn new() -> Self {
        Self(Vec::new())
    }

    fn get_calls(&self) -> Vec<String> {
        self.0.clone()
    }
}

impl<'ast> Visit<'ast> for CallVisitor {
    fn visit_expr_call(&mut self, node: &'ast ExprCall) {
        if let Expr::Path(path) = &*node.func {
            if let Some(ident) = path.path.get_ident() {
                let name = ident.to_string();

                self.0.push(name);
            }
        }
        syn::visit::visit_expr_call(self, node);
    }
}
