//! Conditional compilation and compile-time module resolution.

use crate::{
    compiler::{cfg, diagnostics::CompileError},
    options::{CompileOptions, TargetOs},
};
use miette::SourceSpan;
use oxc_allocator::Allocator;
use oxc_ast::ast::{
    BindingPattern, Declaration, ImportDeclarationSpecifier, ModuleExportName, Statement,
};
use oxc_parser::Parser;
use oxc_span::{GetSpan, SourceType};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Component, Path, PathBuf},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BuiltinModule {
    Fs,
    Process,
    Unix,
    Windows,
    Linux,
    Macos,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportTarget {
    Local(PathBuf),
    Builtin(BuiltinModule),
}

#[derive(Debug, Clone)]
pub struct ImportBinding {
    pub imported: String,
    pub local: String,
    pub target: ImportTarget,
    pub span: SourceSpan,
    /// Type-only imports are retained for the frontend's symbol visibility
    /// analysis, even though they do not introduce a runtime binding.
    pub type_only: bool,
}

#[derive(Debug, Clone)]
pub struct SourceUnit {
    pub path: PathBuf,
    /// Preprocessed source. Its byte length and offsets match `original`.
    pub source: String,
    /// Original source text for user-facing diagnostics.
    pub original: String,
    pub imports: Vec<ImportBinding>,
    pub exports: Vec<String>,
}

#[derive(Debug)]
struct PendingBinding {
    imported: String,
    local: String,
    span: SourceSpan,
    type_only: bool,
}

#[derive(Debug)]
struct PendingImport {
    target: ImportTarget,
    specifier: String,
    span: SourceSpan,
    bindings: Vec<PendingBinding>,
}

#[derive(Default)]
struct Loader {
    units: Vec<SourceUnit>,
    exports: HashMap<PathBuf, HashSet<String>>,
    visited: HashSet<PathBuf>,
    stack: Vec<PathBuf>,
}

/// Load a `.tsh` or `.ts` entrypoint and all relative module dependencies.
/// Dependencies appear before the importing module in the returned list.
pub fn load(path: &Path, options: &CompileOptions) -> Result<Vec<SourceUnit>, CompileError> {
    let path = fs::canonicalize(path).map_err(|error| {
        CompileError::plain(format!(
            "cannot open source file `{}`: {error}",
            path.display()
        ))
    })?;
    let source = fs::read_to_string(&path).map_err(|error| {
        CompileError::plain(format!(
            "cannot read source file `{}`: {error}",
            path.display()
        ))
    })?;
    load_source(&path, &source, options)
}

/// Resolve a source string with the same preprocessing and local-import rules
/// as [`load`]. `name` is also the base directory for relative imports.
pub fn load_source(
    name: &Path,
    source: &str,
    options: &CompileOptions,
) -> Result<Vec<SourceUnit>, CompileError> {
    let path = if name.is_absolute() {
        normalized(name)
    } else {
        normalized(&std::env::current_dir().unwrap_or_default().join(name))
    };
    let mut loader = Loader::default();
    loader.visit(path, source.to_owned(), options, None)?;
    Ok(loader.units)
}

impl Loader {
    fn visit(
        &mut self,
        path: PathBuf,
        original: String,
        options: &CompileOptions,
        via: Option<(&Path, &str, usize, usize)>,
    ) -> Result<(), CompileError> {
        if self.visited.contains(&path) {
            return Ok(());
        }
        if let Some(cycle_start) = self.stack.iter().position(|entry| entry == &path) {
            let chain = self.stack[cycle_start..]
                .iter()
                .chain(std::iter::once(&path))
                .map(|entry| entry.display().to_string())
                .collect::<Vec<_>>()
                .join(" -> ");
            if let Some((importer_path, importer_source, start, len)) = via {
                return Err(CompileError::new(
                    importer_path.display().to_string(),
                    importer_source,
                    start,
                    len,
                    format!("module cycle detected: {chain}"),
                ));
            }
            return Err(CompileError::plain(format!(
                "module cycle detected: {chain}"
            )));
        }

        let source = cfg::preprocess(&path, &original, options)?;
        let (pending_imports, exports) = module_metadata(&path, &source, options)?;
        self.stack.push(path.clone());

        // Visit imports in source order; DFS naturally places dependencies
        // before this unit and de-duplicates diamond-shaped module graphs.
        for import in &pending_imports {
            if let ImportTarget::Local(target) = &import.target {
                let imported_source = fs::read_to_string(target).map_err(|error| {
                    CompileError::new(
                        path.display().to_string(),
                        &original,
                        import.span.offset(),
                        import.span.len(),
                        format!(
                            "cannot read imported module `{}`: {error}",
                            target.display()
                        ),
                    )
                })?;
                self.visit(
                    target.clone(),
                    imported_source,
                    options,
                    Some((&path, &original, import.span.offset(), import.span.len())),
                )?;
            }
        }

        let mut imports = Vec::new();
        for import in &pending_imports {
            match &import.target {
                ImportTarget::Builtin(module) => {
                    for binding in &import.bindings {
                        if !builtin_exports(*module).contains(&binding.imported.as_str()) {
                            return Err(CompileError::new(
                                path.display().to_string(),
                                &original,
                                binding.span.offset(),
                                binding.span.len(),
                                format!(
                                    "`{}` is not exported by `{}`",
                                    binding.imported, import.specifier
                                ),
                            ));
                        }
                        imports.push(ImportBinding {
                            imported: binding.imported.clone(),
                            local: binding.local.clone(),
                            target: import.target.clone(),
                            span: binding.span,
                            type_only: binding.type_only,
                        });
                    }
                }
                ImportTarget::Local(target) => {
                    let target_exports = self.exports.get(target).cloned().unwrap_or_default();
                    for binding in &import.bindings {
                        if !target_exports.contains(&binding.imported) {
                            return Err(CompileError::new(
                                path.display().to_string(),
                                &original,
                                binding.span.offset(),
                                binding.span.len(),
                                format!(
                                    "`{}` is not exported by `{}`",
                                    binding.imported,
                                    target.display()
                                ),
                            ));
                        }
                        imports.push(ImportBinding {
                            imported: binding.imported.clone(),
                            local: binding.local.clone(),
                            target: import.target.clone(),
                            span: binding.span,
                            type_only: binding.type_only,
                        });
                    }
                }
            }
        }

        self.stack.pop();
        self.visited.insert(path.clone());
        self.exports
            .insert(path.clone(), exports.iter().cloned().collect());
        self.units.push(SourceUnit {
            path,
            source,
            original,
            imports,
            exports,
        });
        Ok(())
    }
}

fn module_metadata(
    path: &Path,
    source: &str,
    options: &CompileOptions,
) -> Result<(Vec<PendingImport>, Vec<String>), CompileError> {
    // The file extension is deliberately ignored: `.tsh` contains TypeScript
    // syntax and is parsed with the same Oxc TypeScript source type as `.ts`.
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, source, SourceType::ts()).parse();
    let mut imports = Vec::new();
    let mut exports = Vec::new();
    let mut exported = HashSet::new();
    let mut declared = HashSet::new();
    let mut named_exports = Vec::new();

    for statement in &parsed.program.body {
        match statement {
            Statement::ImportDeclaration(declaration) => {
                let import = parse_import(path, source, declaration, options)?;
                declared.extend(import.bindings.iter().map(|binding| binding.local.clone()));
                imports.push(import);
            }
            Statement::ExportDeclaration(export) => {
                collect_declaration_names(&export.declaration, &mut declared);
                collect_declaration_exports(&export.declaration, &mut exports);
            }
            Statement::ExportNamedDeclaration(export) => {
                for specifier in &export.specifiers {
                    let local = export_name(&specifier.local);
                    let name = export_name(&specifier.exported);
                    if local != name {
                        return Err(CompileError::new(
                            path.display().to_string(),
                            source,
                            specifier.span.start as usize,
                            (specifier.span.end - specifier.span.start) as usize,
                            "export aliases are not supported yet",
                        ));
                    }
                    let span: SourceSpan = (
                        specifier.span.start as usize,
                        (specifier.span.end - specifier.span.start) as usize,
                    )
                        .into();
                    named_exports.push((name, span));
                }
            }
            Statement::ExportFromDeclaration(export) => {
                return Err(CompileError::new(
                    path.display().to_string(),
                    source,
                    export.span.start as usize,
                    (export.span.end - export.span.start) as usize,
                    "re-export declarations are not supported yet",
                ));
            }
            Statement::ExportAllDeclaration(export) => {
                return Err(CompileError::new(
                    path.display().to_string(),
                    source,
                    export.span.start as usize,
                    (export.span.end - export.span.start) as usize,
                    "star exports are not supported yet",
                ));
            }
            Statement::ExportDefaultDeclaration(export) => {
                return Err(CompileError::new(
                    path.display().to_string(),
                    source,
                    export.span.start as usize,
                    (export.span.end - export.span.start) as usize,
                    "default exports are not supported yet",
                ));
            }
            Statement::VariableDeclaration(declaration) => {
                collect_variable_names(declaration, &mut declared);
            }
            Statement::FunctionDeclaration(function) => {
                if let Some(id) = &function.id {
                    declared.insert(id.name.as_str().to_owned());
                }
            }
            Statement::ClassDeclaration(class) => {
                if let Some(id) = &class.id {
                    declared.insert(id.name.as_str().to_owned());
                }
            }
            Statement::TSTypeAliasDeclaration(alias) => {
                declared.insert(alias.id.name.as_str().to_owned());
            }
            Statement::TSInterfaceDeclaration(interface) => {
                declared.insert(interface.id.name.as_str().to_owned());
            }
            Statement::TSEnumDeclaration(enumeration) => {
                declared.insert(enumeration.id.name.as_str().to_owned());
            }
            _ => {}
        }
    }
    for (name, span) in named_exports {
        if !declared.contains(&name) {
            return Err(CompileError::new(
                path.display().to_string(),
                source,
                span.offset(),
                span.len(),
                format!("cannot export `{name}` because this module does not declare or import it"),
            ));
        }
        exports.push(name);
    }
    for name in &exports {
        if !exported.insert(name.clone()) {
            return Err(CompileError::new(
                path.display().to_string(),
                source,
                0,
                0,
                format!("duplicate export `{name}`"),
            ));
        }
    }
    Ok((imports, exports))
}

fn parse_import(
    path: &Path,
    source: &str,
    declaration: &oxc_ast::ast::ImportDeclaration<'_>,
    options: &CompileOptions,
) -> Result<PendingImport, CompileError> {
    let specifier = declaration.source.value.as_str().to_owned();
    let span: SourceSpan = (
        declaration.span.start as usize,
        (declaration.span.end - declaration.span.start) as usize,
    )
        .into();
    let Some(specifiers) = declaration.specifiers.as_ref() else {
        return Err(CompileError::new(
            path.display().to_string(),
            source,
            span.offset(),
            span.len(),
            "side-effect imports are not supported; import named symbols instead",
        ));
    };

    let mut bindings = Vec::new();
    for specifier_node in specifiers {
        let ImportDeclarationSpecifier::ImportSpecifier(specifier_node) = specifier_node else {
            return Err(CompileError::new(
                path.display().to_string(),
                source,
                specifier_node.span().start as usize,
                (specifier_node.span().end - specifier_node.span().start) as usize,
                "default and namespace imports are not supported; use named imports",
            ));
        };
        let imported = match &specifier_node.imported {
            ModuleExportName::IdentifierName(name) => name.name.as_str().to_owned(),
            ModuleExportName::IdentifierReference(name) => name.name.as_str().to_owned(),
            ModuleExportName::StringLiteral(_) => {
                return Err(CompileError::new(
                    path.display().to_string(),
                    source,
                    specifier_node.span.start as usize,
                    (specifier_node.span.end - specifier_node.span.start) as usize,
                    "string-named imports are not supported",
                ));
            }
        };
        let local = specifier_node.local.name.as_str().to_owned();
        if imported != local {
            return Err(CompileError::new(
                path.display().to_string(),
                source,
                specifier_node.span.start as usize,
                (specifier_node.span.end - specifier_node.span.start) as usize,
                "import aliases are not supported yet; imported and local names must match",
            ));
        }
        let type_only = declaration.import_kind == oxc_ast::ast::ImportOrExportKind::Type
            || specifier_node.import_kind == oxc_ast::ast::ImportOrExportKind::Type;
        let specifier_span: SourceSpan = (
            specifier_node.span.start as usize,
            (specifier_node.span.end - specifier_node.span.start) as usize,
        )
            .into();
        bindings.push(PendingBinding {
            imported,
            local,
            span: specifier_span,
            type_only,
        });
    }
    // Diagnose unsupported binding syntax before attempting filesystem
    // resolution so a typo in an import form has a direct explanation.
    let target = resolve_target(
        path,
        source,
        &specifier,
        declaration.span.start as usize,
        (declaration.span.end - declaration.span.start) as usize,
        options,
    )?;
    Ok(PendingImport {
        target,
        specifier,
        span,
        bindings,
    })
}

fn resolve_target(
    importer: &Path,
    source: &str,
    specifier: &str,
    start: usize,
    len: usize,
    options: &CompileOptions,
) -> Result<ImportTarget, CompileError> {
    if let Some(module) = builtin_module(specifier) {
        if !builtin_compatible(module, options.os) {
            return Err(CompileError::new(
                importer.display().to_string(),
                source,
                start,
                len,
                format!(
                    "module `{specifier}` is unavailable for target OS `{}`",
                    os_name(options.os)
                ),
            ));
        }
        return Ok(ImportTarget::Builtin(module));
    }
    if specifier.starts_with("tsh:") {
        return Err(CompileError::new(
            importer.display().to_string(),
            source,
            start,
            len,
            format!("unknown built-in module `{specifier}`"),
        ));
    }
    if !(specifier.starts_with("./") || specifier.starts_with("../")) {
        return Err(CompileError::new(
            importer.display().to_string(),
            source,
            start,
            len,
            format!(
                "module `{specifier}` is not supported; imports must be relative `.tsh`/`.ts` files or `tsh:` built-ins"
            ),
        ));
    }
    let requested = Path::new(specifier);
    if requested
        .extension()
        .is_some_and(|extension| extension != "tsh" && extension != "ts")
    {
        return Err(CompileError::new(
            importer.display().to_string(),
            source,
            start,
            len,
            format!("local module `{specifier}` must use `.tsh` or `.ts` extension"),
        ));
    }
    let base = importer.parent().unwrap_or_else(|| Path::new("."));
    let joined = base.join(requested);
    let candidates = if requested.extension().is_some() {
        vec![joined]
    } else {
        vec![
            joined.with_extension("tsh"),
            joined.with_extension("ts"),
            joined.join("index.tsh"),
            joined.join("index.ts"),
        ]
    };
    for candidate in candidates {
        if candidate.is_file() {
            return fs::canonicalize(&candidate)
                .map(ImportTarget::Local)
                .map_err(|error| {
                    CompileError::new(
                        importer.display().to_string(),
                        source,
                        start,
                        len,
                        format!("cannot resolve module `{specifier}`: {error}"),
                    )
                });
        }
    }
    Err(CompileError::new(
        importer.display().to_string(),
        source,
        start,
        len,
        format!("cannot resolve local module `{specifier}` (tried `.tsh` and `.ts`)"),
    ))
}

fn builtin_module(specifier: &str) -> Option<BuiltinModule> {
    Some(match specifier {
        "tsh:fs" => BuiltinModule::Fs,
        "tsh:process" => BuiltinModule::Process,
        "tsh:unix" => BuiltinModule::Unix,
        "tsh:windows" => BuiltinModule::Windows,
        "tsh:linux" => BuiltinModule::Linux,
        "tsh:macos" => BuiltinModule::Macos,
        _ => return None,
    })
}

fn builtin_compatible(module: BuiltinModule, os: TargetOs) -> bool {
    match module {
        BuiltinModule::Fs | BuiltinModule::Process => true,
        BuiltinModule::Unix => os.is_unix(),
        BuiltinModule::Windows => os == TargetOs::Windows,
        BuiltinModule::Linux => os == TargetOs::Linux,
        BuiltinModule::Macos => os == TargetOs::Macos,
    }
}

fn builtin_exports(module: BuiltinModule) -> &'static [&'static str] {
    match module {
        BuiltinModule::Fs => &["echo", "mkdir", "rm", "cp", "mv", "cd"],
        BuiltinModule::Process => &["run", "env", "exit"],
        BuiltinModule::Unix => &["chmod"],
        BuiltinModule::Windows | BuiltinModule::Linux | BuiltinModule::Macos => &[],
    }
}

fn collect_declaration_exports(declaration: &Declaration<'_>, exports: &mut Vec<String>) {
    match declaration {
        Declaration::FunctionDeclaration(function) => {
            if let Some(id) = &function.id {
                exports.push(id.name.as_str().to_owned());
            }
        }
        Declaration::ClassDeclaration(class) => {
            if let Some(id) = &class.id {
                exports.push(id.name.as_str().to_owned());
            }
        }
        Declaration::TSTypeAliasDeclaration(alias) => {
            exports.push(alias.id.name.as_str().to_owned())
        }
        Declaration::TSInterfaceDeclaration(interface) => {
            exports.push(interface.id.name.as_str().to_owned())
        }
        Declaration::TSEnumDeclaration(enumeration) => {
            exports.push(enumeration.id.name.as_str().to_owned())
        }
        Declaration::VariableDeclaration(variable) => {
            for name in variable_names(variable) {
                exports.push(name);
            }
        }
        _ => {}
    }
}

fn collect_declaration_names(declaration: &Declaration<'_>, names: &mut HashSet<String>) {
    match declaration {
        Declaration::VariableDeclaration(variable) => collect_variable_names(variable, names),
        Declaration::FunctionDeclaration(function) => {
            if let Some(id) = &function.id {
                names.insert(id.name.as_str().to_owned());
            }
        }
        Declaration::ClassDeclaration(class) => {
            if let Some(id) = &class.id {
                names.insert(id.name.as_str().to_owned());
            }
        }
        Declaration::TSTypeAliasDeclaration(alias) => {
            names.insert(alias.id.name.as_str().to_owned());
        }
        Declaration::TSInterfaceDeclaration(interface) => {
            names.insert(interface.id.name.as_str().to_owned());
        }
        Declaration::TSEnumDeclaration(enumeration) => {
            names.insert(enumeration.id.name.as_str().to_owned());
        }
        _ => {}
    }
}

fn collect_variable_names(
    variable: &oxc_ast::ast::VariableDeclaration<'_>,
    names: &mut HashSet<String>,
) {
    names.extend(variable_names(variable));
}

fn variable_names(variable: &oxc_ast::ast::VariableDeclaration<'_>) -> Vec<String> {
    variable
        .declarations
        .iter()
        .filter_map(|declarator| {
            if let BindingPattern::BindingIdentifier(identifier) = &declarator.id {
                Some(identifier.name.as_str().to_owned())
            } else {
                None
            }
        })
        .collect()
}

fn export_name(name: &ModuleExportName<'_>) -> String {
    match name {
        ModuleExportName::IdentifierName(identifier) => identifier.name.as_str().to_owned(),
        ModuleExportName::IdentifierReference(identifier) => identifier.name.as_str().to_owned(),
        ModuleExportName::StringLiteral(literal) => literal.value.as_str().to_owned(),
    }
}

fn os_name(os: TargetOs) -> &'static str {
    match os {
        TargetOs::Windows => "windows",
        TargetOs::Linux => "linux",
        TargetOs::Macos => "macos",
        TargetOs::Freebsd => "freebsd",
    }
}

fn normalized(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            other => result.push(other.as_os_str()),
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::options::{Comments, Target};
    use std::fs;

    fn options(os: TargetOs) -> CompileOptions {
        CompileOptions {
            target: Target::Bash,
            os,
            comments: Comments::All,
        }
    }

    #[test]
    fn local_dependencies_are_bundled_before_importers_and_exports_are_checked() {
        let dir = tempfile::tempdir().unwrap();
        let lib = dir.path().join("lib.tsh");
        fs::write(
            &lib,
            "export function add(a: number, b: number): number { return a + b; }",
        )
        .unwrap();
        let main = dir.path().join("main.tsh");
        let source = "import { add } from './lib';\nconst result = add(1, 2);\n";
        let units = load_source(&main, source, &options(TargetOs::Linux)).unwrap();
        assert_eq!(units.len(), 2);
        assert!(units[0].path.ends_with("lib.tsh"));
        assert_eq!(units[1].imports[0].imported, "add");

        let missing = "import { nope } from './lib';\n";
        assert!(load_source(&main, missing, &options(TargetOs::Linux)).is_err());
    }

    #[test]
    fn rejects_os_incompatible_empty_imports_and_unknown_builtin_exports() {
        let path = Path::new("main.tsh");
        assert!(
            load_source(
                path,
                "import {} from 'tsh:windows';",
                &options(TargetOs::Linux)
            )
            .is_err()
        );
        assert!(
            load_source(
                path,
                "import { chmod } from 'tsh:unix';",
                &options(TargetOs::Windows)
            )
            .is_err()
        );
        assert!(
            load_source(
                path,
                "import { readFile } from 'tsh:fs';",
                &options(TargetOs::Linux)
            )
            .is_err()
        );
    }

    #[test]
    fn inactive_cfg_import_does_not_resolve_a_missing_file() {
        let source =
            "// @cfg(windows)\nimport { missing } from './does-not-exist';\nconst available = 1;\n";
        let units = load_source(Path::new("main.tsh"), source, &options(TargetOs::Linux)).unwrap();
        assert_eq!(units.len(), 1);
        assert!(units[0].source.contains("const available = 1;"));
        assert!(units[0].imports.is_empty());
    }

    #[test]
    fn named_export_must_refer_to_a_local_declaration_or_import() {
        let path = Path::new("main.tsh");
        assert!(
            load_source(path, "export { missing };", &options(TargetOs::Linux))
                .unwrap_err()
                .to_string()
                .contains("does not declare or import")
        );
        let units = load_source(
            path,
            "const value = 1; export { value };",
            &options(TargetOs::Linux),
        )
        .unwrap();
        assert_eq!(units[0].exports, vec!["value"]);
    }

    #[test]
    fn rejects_aliases_packages_and_cycles() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.tsh");
        let b = dir.path().join("b.ts");
        fs::write(&b, "import { a } from './a'; export function b() {}\n").unwrap();
        fs::write(&a, "import { b } from './b'; export function a() {}\n").unwrap();
        assert!(
            load(&a, &options(TargetOs::Linux))
                .unwrap_err()
                .to_string()
                .contains("cycle")
        );
        assert!(
            load_source(
                Path::new("main.tsh"),
                "import { x as y } from './x';",
                &options(TargetOs::Linux)
            )
            .unwrap_err()
            .to_string()
            .contains("aliases")
        );
        assert!(
            load_source(
                Path::new("main.tsh"),
                "import { x } from 'npm-package';",
                &options(TargetOs::Linux)
            )
            .unwrap_err()
            .to_string()
            .contains("relative")
        );
    }
}
