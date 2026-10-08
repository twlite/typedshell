//! Type-check and lower the supported TypeScript subset into the backend IR.

use std::collections::{HashMap, HashSet};

use oxc_allocator::Allocator;
use oxc_ast::ast::{
    Argument, AssignmentOperator, AssignmentTarget, BinaryOperator, BindingPattern,
    Class as OxcClass, ClassElement, Comment, Declaration, Expression, ForStatementInit,
    Function as OxcFunction, LogicalOperator, MethodDefinitionKind, ObjectPropertyKind,
    PropertyKey, SimpleAssignmentTarget, Statement, TSType, TSTypeName, UnaryOperator,
    UpdateOperator, VariableDeclarationKind,
};
use oxc_diagnostics::Diagnostic;
use oxc_parser::Parser;
use oxc_semantic::SemanticBuilder;
use oxc_span::{GetSpan, SourceType, Span};

use crate::{
    compiler::{
        diagnostics::CompileError,
        resolver::{BuiltinModule, ImportBinding, ImportTarget, SourceUnit},
    },
    ir::{self, Expr as IrExpr, ExprKind, Place, Stmt, Type},
    options::{Comments, CompileOptions, TargetOs},
};

#[derive(Clone)]
struct Binding {
    ir_name: String,
    ty: Type,
    mutable: bool,
    imported: bool,
}

#[derive(Clone)]
struct ParamInfo {
    source_name: String,
    ty: Type,
    optional: bool,
}

#[derive(Clone)]
struct FunctionSignature {
    ir_name: String,
    params: Vec<ParamInfo>,
    return_type: Option<Type>,
}

#[derive(Clone)]
struct FieldInfo {
    ty: Type,
    readonly: bool,
}

#[derive(Clone)]
struct MethodSignature {
    function: FunctionSignature,
    is_static: bool,
    ir_method_name: String,
}

#[derive(Clone)]
struct ClassSignature {
    ir_name: String,
    fields: HashMap<String, FieldInfo>,
    constructor: FunctionSignature,
    methods: HashMap<String, MethodSignature>,
}

#[derive(Clone)]
enum ExportedSymbol {
    Function(FunctionSignature),
    Class(ClassSignature),
    Variable(Binding),
}

#[derive(Default, Clone)]
struct ModuleSurface {
    symbols: HashMap<String, ExportedSymbol>,
}

struct FunctionDef<'p, 'a> {
    name: String,
    signature: FunctionSignature,
    ast: &'p OxcFunction<'a>,
}

#[derive(Clone)]
struct MethodDef<'p, 'a> {
    source_name: String,
    signature: FunctionSignature,
    ast: &'p OxcFunction<'a>,
    is_static: bool,
    parameter_properties: Vec<(String, String)>,
}

#[derive(Clone)]
struct ClassDef<'p, 'a> {
    source_name: String,
    signature: ClassSignature,
    ast: &'p OxcClass<'a>,
    constructor: Option<MethodDef<'p, 'a>>,
    methods: Vec<MethodDef<'p, 'a>>,
    initializers: HashMap<String, &'p Expression<'a>>,
    field_order: Vec<String>,
}

#[derive(Clone)]
struct GlobalDef {
    source_name: String,
    binding: Binding,
    initializer: Option<IrExpr>,
    span: Span,
}

#[derive(Default)]
struct Ids {
    variable: usize,
    function: usize,
    class: usize,
    method: usize,
}

struct ScopeStack {
    scopes: Vec<HashMap<String, Binding>>,
}

impl ScopeStack {
    fn new(global: HashMap<String, Binding>) -> Self {
        Self {
            scopes: vec![global],
        }
    }
    fn push(&mut self) {
        self.scopes.push(HashMap::new());
    }
    fn pop(&mut self) {
        if self.scopes.len() > 1 {
            self.scopes.pop();
        }
    }
    fn current(&self) -> &HashMap<String, Binding> {
        self.scopes.last().expect("scope stack is non-empty")
    }
    fn current_mut(&mut self) -> &mut HashMap<String, Binding> {
        self.scopes.last_mut().expect("scope stack is non-empty")
    }
    fn get(&self, name: &str) -> Option<&Binding> {
        self.scopes.iter().rev().find_map(|scope| scope.get(name))
    }
}

/// Parse and lower all already-resolved source units. Units are expected in
/// dependency-before-importer order, as supplied by the resolver.
pub fn analyze(
    units: &[SourceUnit],
    options: &CompileOptions,
) -> Result<ir::Program, CompileError> {
    let mut program = ir::Program::default();
    let mut surfaces: HashMap<std::path::PathBuf, ModuleSurface> = HashMap::new();
    let mut ids = Ids::default();

    for unit in units {
        let allocator = Allocator::default();
        let parsed = Parser::new(&allocator, &unit.source, SourceType::ts()).parse();
        if let Some(error) = parsed.diagnostics.errors().next() {
            return Err(oxc_error(unit, error));
        }
        let semantic = SemanticBuilder::new()
            .with_check_syntax_error(true)
            .build(&parsed.program);
        if let Some(error) = semantic.diagnostics.errors().next() {
            return Err(oxc_error(unit, error));
        }

        let mut module = ModuleAnalyzer::new(unit, &parsed.program, options, &surfaces, &mut ids);
        module.collect_class_skeletons()?;
        module.resolve_imports()?;
        module.collect_declarations()?;
        module.validate_import_conflicts()?;
        module.resolve_function_returns()?;
        module.prepare_globals()?;
        module.lower_into(&mut program)?;

        let surface = module.export_surface()?;
        surfaces.insert(unit.path.clone(), surface);
    }
    Ok(program)
}

fn oxc_error(unit: &SourceUnit, error: &oxc_diagnostics::OxcDiagnostic) -> CompileError {
    let (start, len) = error
        .labels()
        .first()
        .map(|label| (label.offset() as usize, label.len() as usize))
        .unwrap_or((0, 0));
    CompileError::new(
        unit.path.display().to_string(),
        &unit.original,
        start,
        len,
        error.to_string(),
    )
}

fn err(unit: &SourceUnit, span: Span, message: impl Into<String>) -> CompileError {
    CompileError::new(
        unit.path.display().to_string(),
        &unit.original,
        span.start as usize,
        span.end.saturating_sub(span.start) as usize,
        message,
    )
}

struct ModuleAnalyzer<'p, 'a, 'ctx> {
    unit: &'p SourceUnit,
    program: &'p oxc_ast::ast::Program<'a>,
    options: &'ctx CompileOptions,
    surfaces: &'ctx HashMap<std::path::PathBuf, ModuleSurface>,
    ids: &'ctx mut Ids,
    functions: Vec<FunctionDef<'p, 'a>>,
    function_indices: HashMap<String, usize>,
    classes: Vec<ClassDef<'p, 'a>>,
    class_indices: HashMap<String, usize>,
    imports: HashMap<String, ExportedSymbol>,
    type_only_imports: HashSet<String>,
    globals: Vec<GlobalDef>,
    global_indices: HashMap<String, usize>,
}

impl<'p, 'a, 'ctx> ModuleAnalyzer<'p, 'a, 'ctx> {
    fn new(
        unit: &'p SourceUnit,
        program: &'p oxc_ast::ast::Program<'a>,
        options: &'ctx CompileOptions,
        surfaces: &'ctx HashMap<std::path::PathBuf, ModuleSurface>,
        ids: &'ctx mut Ids,
    ) -> Self {
        Self {
            unit,
            program,
            options,
            surfaces,
            ids,
            functions: Vec::new(),
            function_indices: HashMap::new(),
            classes: Vec::new(),
            class_indices: HashMap::new(),
            imports: HashMap::new(),
            type_only_imports: HashSet::new(),
            globals: Vec::new(),
            global_indices: HashMap::new(),
        }
    }

    fn collect_class_skeletons(&mut self) -> Result<(), CompileError> {
        // Assign class identities first so annotations may refer forward to classes.
        for statement in &self.program.body {
            if let Some((class, _)) = class_declaration(statement) {
                let Some(id) = &class.id else {
                    return Err(err(
                        self.unit,
                        class.span,
                        "class declarations must have a name",
                    ));
                };
                let source_name = id.name.as_str().to_owned();
                if self.class_indices.contains_key(&source_name) {
                    return Err(err(
                        self.unit,
                        id.span,
                        format!("duplicate class `{source_name}`"),
                    ));
                }
                let ir_name = format!("{}_{}", shell_identifier(&source_name), self.ids.class);
                self.ids.class += 1;
                let ctor = FunctionSignature {
                    ir_name: format!("ctor_{ir_name}"),
                    params: Vec::new(),
                    return_type: Some(Type::Void),
                };
                let signature = ClassSignature {
                    ir_name,
                    fields: HashMap::new(),
                    constructor: ctor,
                    methods: HashMap::new(),
                };
                self.class_indices
                    .insert(source_name.clone(), self.classes.len());
                self.classes.push(ClassDef {
                    source_name,
                    signature,
                    ast: class,
                    constructor: None,
                    methods: Vec::new(),
                    initializers: HashMap::new(),
                    field_order: Vec::new(),
                });
            }
        }
        Ok(())
    }

    fn collect_declarations(&mut self) -> Result<(), CompileError> {
        for statement in &self.program.body {
            if let Some((function, _)) = function_declaration(statement) {
                self.add_function(function)?;
            }
        }
        // Collect classes before globals so global initializers like
        // `const a = c.next()` can infer instance-method return types.
        // Field initializers with annotations do not need globals for this.
        for index in 0..self.classes.len() {
            self.collect_class(index)?;
        }
        // Module-level vars are declared in one lexical environment and receive
        // globally unique IR names before any function body is lowered.
        for statement in &self.program.body {
            if let Some(variable) = variable_declaration(statement) {
                self.collect_global_declarations(variable)?;
            }
        }
        Ok(())
    }

    fn add_function(&mut self, ast: &'p OxcFunction<'a>) -> Result<(), CompileError> {
        let Some(id) = &ast.id else {
            return Err(err(
                self.unit,
                ast.span,
                "function declarations must have a name",
            ));
        };
        let source_name = id.name.as_str().to_owned();
        if self.function_indices.contains_key(&source_name)
            || self.class_indices.contains_key(&source_name)
        {
            return Err(err(
                self.unit,
                id.span,
                format!("duplicate declaration `{source_name}`"),
            ));
        }
        reject_function_modifiers(self.unit, ast)?;
        let ir_name = format!("{}_{}", shell_identifier(&source_name), self.ids.function);
        self.ids.function += 1;
        let params = self.parameter_info(&ast.params.items, ast.span)?;
        let return_type = match ast.return_type.as_deref() {
            Some(annotation) => {
                Some(self.annotation_type(&annotation.type_annotation, annotation.span)?)
            }
            None => None,
        };
        let signature = FunctionSignature {
            ir_name,
            params,
            return_type,
        };
        let index = self.functions.len();
        self.function_indices.insert(source_name.clone(), index);
        self.functions.push(FunctionDef {
            name: source_name,
            signature,
            ast,
        });
        Ok(())
    }

    fn collect_class(&mut self, index: usize) -> Result<(), CompileError> {
        let class = self.classes[index].ast;
        if !class.decorators.is_empty() {
            return Err(err(
                self.unit,
                class.decorators[0].span,
                "class decorators are not supported",
            ));
        }
        if class.type_parameters.is_some() {
            return Err(err(
                self.unit,
                class.span,
                "generic classes are not supported",
            ));
        }
        if !class.implements.is_empty() {
            return Err(err(
                self.unit,
                class.span,
                "class implements clauses are not supported",
            ));
        }
        if class.r#abstract {
            return Err(err(
                self.unit,
                class.span,
                "abstract classes are not supported",
            ));
        }
        if class.declare {
            return Err(err(
                self.unit,
                class.span,
                "declare classes are not supported",
            ));
        }
        let source_name = self.classes[index].source_name.clone();
        if class.heritage.is_some() {
            return Err(err(
                self.unit,
                class.span,
                "class inheritance is not supported",
            ));
        }

        let mut fields = HashMap::<String, FieldInfo>::new();
        let mut initializers = HashMap::<String, &'p Expression<'a>>::new();
        let mut field_order = Vec::new();
        let mut constructor = None;
        let mut methods = Vec::new();
        for element in &class.body.body {
            match element {
                ClassElement::PropertyDefinition(property) => {
                    if !property.decorators.is_empty() {
                        return Err(err(
                            self.unit,
                            property.decorators[0].span,
                            "property decorators are not supported",
                        ));
                    }
                    if property.r#type != oxc_ast::ast::PropertyDefinitionType::PropertyDefinition {
                        return Err(err(
                            self.unit,
                            property.span,
                            "abstract fields are not supported",
                        ));
                    }
                    if property.r#static {
                        return Err(err(
                            self.unit,
                            property.span,
                            "static fields are not supported",
                        ));
                    }
                    if property.computed {
                        return Err(err(
                            self.unit,
                            property.span,
                            "computed class fields are not supported",
                        ));
                    }
                    if property.declare
                        || property.r#override
                        || property.optional
                        || property.definite
                    {
                        return Err(err(
                            self.unit,
                            property.span,
                            "declare, override, optional, and definite fields are not supported",
                        ));
                    }
                    if property.readonly {
                        return Err(err(
                            self.unit,
                            property.span,
                            "readonly fields are not supported",
                        ));
                    }
                    let Some(name) = property_name(&property.key) else {
                        return Err(err(
                            self.unit,
                            property.span,
                            "class field names must be identifiers or string literals",
                        ));
                    };
                    if property.accessibility.is_some_and(|value| {
                        matches!(
                            value,
                            oxc_ast::ast::TSAccessibility::Private
                                | oxc_ast::ast::TSAccessibility::Protected
                        )
                    }) {
                        return Err(err(
                            self.unit,
                            property.span,
                            "private and protected fields are not supported",
                        ));
                    }
                    let ty = if let Some(annotation) = property.type_annotation.as_deref() {
                        self.annotation_type(&annotation.type_annotation, annotation.span)?
                    } else if let Some(value) = &property.value {
                        self.infer_expression_type(value, None, None)?
                            .ok_or_else(|| {
                                err(
                                    self.unit,
                                    value.span(),
                                    "cannot infer class field type; add an annotation",
                                )
                            })?
                    } else {
                        return Err(err(
                            self.unit,
                            property.span,
                            format!(
                                "field `{name}` without an initializer needs an explicit type annotation"
                            ),
                        ));
                    };
                    if let Some(value) = &property.value {
                        let old = initializers.insert(name.clone(), value);
                        if old.is_some() {
                            return Err(err(
                                self.unit,
                                property.span,
                                format!("duplicate field `{name}`"),
                            ));
                        }
                    }
                    if fields
                        .insert(
                            name.clone(),
                            FieldInfo {
                                ty,
                                readonly: property.readonly,
                            },
                        )
                        .is_some()
                    {
                        return Err(err(
                            self.unit,
                            property.span,
                            format!("duplicate field `{name}`"),
                        ));
                    }
                    field_order.push(name.clone());
                }
                ClassElement::MethodDefinition(method) => {
                    if !method.decorators.is_empty() {
                        return Err(err(
                            self.unit,
                            method.decorators[0].span,
                            "method decorators are not supported",
                        ));
                    }
                    if method.r#type != oxc_ast::ast::MethodDefinitionType::MethodDefinition {
                        return Err(err(
                            self.unit,
                            method.span,
                            "abstract methods are not supported",
                        ));
                    }
                    if method.r#override || method.optional {
                        return Err(err(
                            self.unit,
                            method.span,
                            "override and optional methods are not supported",
                        ));
                    }
                    if method.computed {
                        return Err(err(
                            self.unit,
                            method.span,
                            "computed class methods are not supported",
                        ));
                    }
                    let Some(name) = property_name(&method.key) else {
                        return Err(err(
                            self.unit,
                            method.span,
                            "class method names must be identifiers",
                        ));
                    };
                    match method.kind {
                        MethodDefinitionKind::Constructor => {
                            if method.r#static {
                                return Err(err(
                                    self.unit,
                                    method.span,
                                    "constructors cannot be static",
                                ));
                            }
                            if constructor.is_some() {
                                return Err(err(
                                    self.unit,
                                    method.span,
                                    "a class can only have one constructor",
                                ));
                            }
                            let mut def =
                                self.method_def(name, &method.value, false, true, index)?;
                            let props = method
                                .value
                                .params
                                .items
                                .iter()
                                .filter_map(|param| {
                                    if param.accessibility.is_some() {
                                        binding_pattern_name(&param.pattern).map(|parameter_name| {
                                            (parameter_name, param.accessibility, param.readonly)
                                        })
                                    } else {
                                        None
                                    }
                                })
                                .collect::<Vec<_>>();
                            for (property_name, access, readonly) in props {
                                if readonly {
                                    return Err(err(
                                        self.unit,
                                        method.span,
                                        "readonly parameter properties are not supported",
                                    ));
                                }
                                if access.is_some_and(|value| {
                                    matches!(
                                        value,
                                        oxc_ast::ast::TSAccessibility::Private
                                            | oxc_ast::ast::TSAccessibility::Protected
                                    )
                                }) {
                                    return Err(err(
                                        self.unit,
                                        method.span,
                                        "private and protected parameter properties are not supported",
                                    ));
                                }
                                let parameter = def
                                    .signature
                                    .params
                                    .iter()
                                    .find(|param| param.source_name == property_name)
                                    .ok_or_else(|| {
                                        err(
                                            self.unit,
                                            method.span,
                                            format!("invalid parameter property `{property_name}`"),
                                        )
                                    })?;
                                if fields
                                    .insert(
                                        property_name.clone(),
                                        FieldInfo {
                                            ty: parameter.ty.clone(),
                                            readonly: false,
                                        },
                                    )
                                    .is_some()
                                {
                                    return Err(err(
                                        self.unit,
                                        method.span,
                                        format!("duplicate field `{property_name}`"),
                                    ));
                                }
                                field_order.push(property_name.clone());
                                def.parameter_properties
                                    .push((property_name, parameter.source_name.clone()));
                            }
                            constructor = Some(def);
                        }
                        MethodDefinitionKind::Method => {
                            if method.accessibility.is_some_and(|value| {
                                matches!(
                                    value,
                                    oxc_ast::ast::TSAccessibility::Private
                                        | oxc_ast::ast::TSAccessibility::Protected
                                )
                            }) {
                                return Err(err(
                                    self.unit,
                                    method.span,
                                    "private and protected methods are not supported",
                                ));
                            }
                            let def = self.method_def(
                                name,
                                &method.value,
                                method.r#static,
                                false,
                                index,
                            )?;
                            methods.push(def);
                        }
                        _ => {
                            return Err(err(
                                self.unit,
                                method.span,
                                "getters and setters are not supported",
                            ));
                        }
                    }
                }
                _ => return Err(err(self.unit, element.span(), "unsupported class member")),
            }
        }

        if let Some(def) = &constructor {
            for (field, _) in &def.parameter_properties {
                if initializers.contains_key(field) {
                    return Err(err(
                        self.unit,
                        def.ast.span,
                        format!("parameter property `{field}` duplicates an initialized field"),
                    ));
                }
            }
        }
        // A declaration without an initializer must be assigned on every
        // constructor path. We currently certify only unconditional direct
        // assignments, and reject anything that cannot be proven statically.
        let assigned = constructor
            .as_ref()
            .map(|def| constructor_assignments(def.ast))
            .unwrap_or_default();
        for (name, field) in &fields {
            if !initializers.contains_key(name)
                && !constructor.as_ref().is_some_and(|def| {
                    def.parameter_properties
                        .iter()
                        .any(|(field, _)| field == name)
                })
                && !assigned.contains(name)
            {
                return Err(err(
                    self.unit,
                    class.span,
                    format!("field `{name}` is not initialized on every constructor path"),
                ));
            }
            if !initializers.contains_key(name) && !assigned.contains(name) && constructor.is_none()
            {
                return Err(err(
                    self.unit,
                    class.span,
                    format!("field `{name}` is not initialized"),
                ));
            }
            if field.ty == Type::Void {
                return Err(err(self.unit, class.span, "fields cannot have type void"));
            }
        }

        let ir_name = self.classes[index].signature.ir_name.clone();
        let constructor_sig = constructor
            .as_ref()
            .map(|def| def.signature.clone())
            .unwrap_or_else(|| FunctionSignature {
                ir_name: format!("ctor_{ir_name}"),
                params: Vec::new(),
                return_type: Some(Type::Void),
            });
        let mut method_signatures = HashMap::new();
        for method in &methods {
            if method_signatures.contains_key(&method.source_name) {
                return Err(err(
                    self.unit,
                    method.ast.span,
                    format!("duplicate method `{}`", method.source_name),
                ));
            }
            method_signatures.insert(
                method.source_name.clone(),
                MethodSignature {
                    function: method.signature.clone(),
                    is_static: method.is_static,
                    ir_method_name: method.signature.ir_name.clone(),
                },
            );
        }
        let signature = ClassSignature {
            ir_name,
            fields,
            constructor: constructor_sig,
            methods: method_signatures,
        };
        let class = &mut self.classes[index];
        class.signature = signature;
        class.constructor = constructor;
        class.methods = methods;
        class.initializers = initializers;
        class.field_order = field_order;
        let _ = source_name;
        Ok(())
    }

    fn method_def(
        &mut self,
        source_name: String,
        ast: &'p OxcFunction<'a>,
        is_static: bool,
        is_constructor: bool,
        _class_index: usize,
    ) -> Result<MethodDef<'p, 'a>, CompileError> {
        reject_function_modifiers(self.unit, ast)?;
        let ir_name = if is_constructor {
            format!("ctor_{}", self.classes[_class_index].signature.ir_name)
        } else {
            let name = format!("{}_{}", shell_identifier(&source_name), self.ids.method);
            self.ids.method += 1;
            name
        };
        let params = self.parameter_info(&ast.params.items, ast.span)?;
        let return_type = if is_constructor {
            Some(Type::Void)
        } else {
            match ast.return_type.as_deref() {
                Some(annotation) => {
                    Some(self.annotation_type(&annotation.type_annotation, annotation.span)?)
                }
                None => None,
            }
        };
        let parameter_properties = Vec::new();
        if !is_constructor
            && ast
                .params
                .items
                .iter()
                .any(|parameter| parameter.accessibility.is_some())
        {
            return Err(err(
                self.unit,
                ast.span,
                "parameter properties are only supported in constructors",
            ));
        }
        Ok(MethodDef {
            source_name,
            signature: FunctionSignature {
                ir_name,
                params,
                return_type,
            },
            ast,
            is_static,
            parameter_properties,
        })
    }

    fn parameter_info(
        &self,
        params: &[oxc_ast::ast::FormalParameter<'a>],
        span: Span,
    ) -> Result<Vec<ParamInfo>, CompileError> {
        let mut output = Vec::new();
        for parameter in params {
            if !parameter.decorators.is_empty() {
                return Err(err(
                    self.unit,
                    parameter.decorators[0].span,
                    "parameter decorators are not supported",
                ));
            }
            let Some(name) = binding_pattern_name(&parameter.pattern) else {
                return Err(err(
                    self.unit,
                    parameter.span,
                    "parameters must use simple identifiers",
                ));
            };
            if parameter.optional {
                return Err(err(
                    self.unit,
                    parameter.span,
                    "optional parameters are not supported",
                ));
            }
            if parameter.initializer.is_some() {
                return Err(err(
                    self.unit,
                    parameter.span,
                    "default parameter values are not supported",
                ));
            }
            if parameter.readonly || parameter.r#override {
                return Err(err(
                    self.unit,
                    parameter.span,
                    "readonly and override parameter modifiers are not supported",
                ));
            }
            let ty = if let Some(annotation) = parameter.type_annotation.as_deref() {
                self.annotation_type(&annotation.type_annotation, annotation.span)?
            } else {
                return Err(err(
                    self.unit,
                    parameter.span,
                    format!("parameter `{name}` needs a type annotation"),
                ));
            };
            if ty == Type::Void {
                return Err(err(
                    self.unit,
                    parameter.span,
                    "parameters cannot have type void",
                ));
            }
            output.push(ParamInfo {
                source_name: name,
                ty,
                optional: false,
            });
        }
        if params.len() > output.len() {
            return Err(err(self.unit, span, "unsupported parameter list"));
        }
        Ok(output)
    }

    fn annotation_type(&self, ty: &TSType<'a>, span: Span) -> Result<Type, CompileError> {
        match ty {
            TSType::TSStringKeyword(_) => Ok(Type::String),
            TSType::TSNumberKeyword(_) => Ok(Type::Number),
            TSType::TSBooleanKeyword(_) => Ok(Type::Boolean),
            TSType::TSVoidKeyword(_) => Ok(Type::Void),
            TSType::TSParenthesizedType(parenthesized) => {
                self.annotation_type(&parenthesized.type_annotation, span)
            }
            TSType::TSTypeReference(reference) => {
                let name = match &reference.type_name {
                    TSTypeName::IdentifierReference(identifier) => identifier.name.as_str(),
                    _ => {
                        return Err(err(
                            self.unit,
                            span,
                            "qualified type names are not supported",
                        ));
                    }
                };
                self.lookup_class(name)
                    .map(|class| Type::Class(class.ir_name.clone()))
                    .ok_or_else(|| err(self.unit, span, format!("unknown class type `{name}`")))
            }
            _ => Err(err(
                self.unit,
                span,
                "only string, number, boolean, void, and class types are supported",
            )),
        }
    }

    fn collect_global_declarations(
        &mut self,
        declaration: &'p oxc_ast::ast::VariableDeclaration<'a>,
    ) -> Result<(), CompileError> {
        if declaration.kind != VariableDeclarationKind::Const
            && declaration.kind != VariableDeclarationKind::Let
        {
            return Err(err(
                self.unit,
                declaration.span,
                "only const and let declarations are supported",
            ));
        }
        for declarator in &declaration.declarations {
            let Some(source_name) = binding_pattern_name(&declarator.id) else {
                return Err(err(
                    self.unit,
                    declarator.span,
                    "variables must use simple identifiers",
                ));
            };
            if self.global_indices.contains_key(&source_name)
                || self.function_indices.contains_key(&source_name)
                || self.class_indices.contains_key(&source_name)
            {
                return Err(err(
                    self.unit,
                    declarator.span,
                    format!("duplicate declaration `{source_name}`"),
                ));
            }
            if declarator.init.is_none() {
                return Err(err(
                    self.unit,
                    declarator.span,
                    "uninitialized variable declarations are not supported",
                ));
            }
            let annotation = declarator
                .type_annotation
                .as_deref()
                .map(|ann| self.annotation_type(&ann.type_annotation, ann.span))
                .transpose()?;
            let ty = if let Some(ty) = annotation {
                ty
            } else {
                self.infer_expression_type(
                    declarator.init.as_ref().expect("checked above"),
                    None,
                    None,
                )?
                .ok_or_else(|| {
                    err(
                        self.unit,
                        declarator.span,
                        format!("cannot infer type of `{source_name}`; add an annotation"),
                    )
                })?
            };
            if ty == Type::Void {
                return Err(err(
                    self.unit,
                    declarator.span,
                    "variables cannot have type void",
                ));
            }
            let ir_name = self.next_var(&source_name);
            let binding = Binding {
                ir_name,
                ty,
                mutable: declaration.kind == VariableDeclarationKind::Let,
                imported: false,
            };
            self.global_indices
                .insert(source_name.clone(), self.globals.len());
            self.globals.push(GlobalDef {
                source_name,
                binding,
                initializer: None,
                span: declarator.span,
            });
        }
        Ok(())
    }

    fn prepare_globals(&mut self) -> Result<(), CompileError> {
        let mut scope = self.imported_global_scope();
        for statement in &self.program.body {
            if let Some(declaration) = variable_declaration(statement) {
                for declarator in &declaration.declarations {
                    let Some(name) = binding_pattern_name(&declarator.id) else {
                        continue;
                    };
                    let index = *self.global_indices.get(&name).ok_or_else(|| {
                        err(
                            self.unit,
                            declarator.span,
                            format!("unknown module variable `{name}`"),
                        )
                    })?;
                    let expected = self.globals[index].binding.ty.clone();
                    let mut lowerer = Lowerer::new(
                        self.unit,
                        self.options,
                        self.imports.clone(),
                        self.function_lookup(),
                        self.class_lookup(),
                        &mut self.ids.variable,
                        scope.clone(),
                        self.program.comments.iter().copied().collect(),
                    );
                    let value = lowerer.lower_expression(
                        declarator
                            .init
                            .as_ref()
                            .expect("global initializers are checked"),
                        None,
                    )?;
                    ensure_type(self.unit, declarator.span, &expected, &value.ty)?;
                    self.globals[index].initializer = Some(value);
                    scope.insert(name, self.globals[index].binding.clone());
                }
            }
        }
        // A function may read or mutate a top-level binding that appears later
        // in source. All of its bindings are made visible after initializers are
        // checked in source order.
        Ok(())
    }

    fn resolve_imports(&mut self) -> Result<(), CompileError> {
        for import in &self.unit.imports {
            let symbol = match &import.target {
                ImportTarget::Local(path) => self
                    .surfaces
                    .get(path)
                    .and_then(|surface| surface.symbols.get(&import.imported))
                    .cloned()
                    .ok_or_else(|| {
                        CompileError::new(
                            self.unit.path.display().to_string(),
                            &self.unit.original,
                            import.span.offset(),
                            import.span.len(),
                            format!("`{}` is not a supported runtime export", import.imported),
                        )
                    })?,
                ImportTarget::Builtin(_) => continue,
            };
            if import.type_only {
                if !matches!(symbol, ExportedSymbol::Class(_)) {
                    return Err(CompileError::new(
                        self.unit.path.display().to_string(),
                        &self.unit.original,
                        import.span.offset(),
                        import.span.len(),
                        "type-only imports currently support class types only",
                    ));
                }
                self.type_only_imports.insert(import.local.clone());
            }
            let symbol = match symbol {
                ExportedSymbol::Variable(mut binding) => {
                    binding.imported = true;
                    binding.mutable = false;
                    ExportedSymbol::Variable(binding)
                }
                other => other,
            };
            self.imports.insert(import.local.clone(), symbol);
        }
        Ok(())
    }

    fn validate_import_conflicts(&self) -> Result<(), CompileError> {
        for import in &self.unit.imports {
            if self.global_indices.contains_key(&import.local)
                || self.function_indices.contains_key(&import.local)
                || self.class_indices.contains_key(&import.local)
            {
                return Err(CompileError::new(
                    self.unit.path.display().to_string(),
                    &self.unit.original,
                    import.span.offset(),
                    import.span.len(),
                    format!(
                        "import `{}` conflicts with a local declaration",
                        import.local
                    ),
                ));
            }
        }
        Ok(())
    }

    fn resolve_function_returns(&mut self) -> Result<(), CompileError> {
        // A recursive inferred return can be learned from its nonrecursive base
        // branch. Revisit declarations until signatures stop gaining types.
        for _ in 0..self
            .functions
            .len()
            .saturating_add(self.classes.len())
            .saturating_add(1)
        {
            let mut inferred_functions = Vec::new();
            let mut inferred_methods = Vec::new();
            for (index, function) in self.functions.iter().enumerate() {
                if function.signature.return_type.is_none()
                    && let Some(ty) = infer_function_return(
                        self.unit,
                        function.ast,
                        &function.signature.params,
                        &self.globals,
                        &self.imports,
                        &self.functions,
                        &self.classes,
                        None,
                    )
                {
                    inferred_functions.push((index, ty));
                }
            }
            for (class_index, class) in self.classes.iter().enumerate() {
                for (method_index, method) in class.methods.iter().enumerate() {
                    if method.signature.return_type.is_none()
                        && let Some(ty) = infer_function_return(
                            self.unit,
                            method.ast,
                            &method.signature.params,
                            &self.globals,
                            &self.imports,
                            &self.functions,
                            &self.classes,
                            Some(&class.signature.ir_name),
                        )
                    {
                        inferred_methods.push((class_index, method_index, ty));
                    }
                }
            }
            if inferred_functions.is_empty() && inferred_methods.is_empty() {
                break;
            }
            for (index, ty) in inferred_functions {
                self.functions[index].signature.return_type = Some(ty);
            }
            for (class_index, method_index, ty) in inferred_methods {
                self.classes[class_index].methods[method_index]
                    .signature
                    .return_type = Some(ty);
            }
            for class in &mut self.classes {
                for method in &class.methods {
                    if let Some(signature) = class.signature.methods.get_mut(&method.source_name) {
                        signature.function.return_type = method.signature.return_type.clone();
                    }
                }
            }
        }
        for function in &self.functions {
            if function.signature.return_type.is_none() {
                return Err(err(
                    self.unit,
                    function.ast.span,
                    format!(
                        "cannot infer return type of `{}`; add an annotation",
                        function.name
                    ),
                ));
            }
        }
        for class in &self.classes {
            for method in &class.methods {
                if method.signature.return_type.is_none() {
                    return Err(err(
                        self.unit,
                        method.ast.span,
                        format!(
                            "cannot infer return type of `{}.{}`; add an annotation",
                            class.source_name, method.source_name
                        ),
                    ));
                }
            }
        }
        Ok(())
    }

    /// Comments in `start..end` filtered by the active `--comments` mode.
    /// Directives are never preserved. This mirrors
    /// [`Lowerer::comments_between`] for spans outside any function body,
    /// such as the gaps between class members.
    fn leading_comments(&self, start: usize, end: usize) -> Vec<String> {
        if self.options.comments == Comments::None {
            return Vec::new();
        }
        let mut output = Vec::new();
        for comment in self.program.comments.iter().copied() {
            let cstart = comment.span.start as usize;
            let cend = comment.span.end as usize;
            if cstart < start || cend > end {
                continue;
            }
            let Some(text) = self.unit.source.get(cstart..cend) else {
                continue;
            };
            if is_compiler_directive(text) {
                continue;
            }
            let is_doc = text.starts_with("/**");
            if self.options.comments == Comments::Doc && !is_doc {
                continue;
            }
            output.push(text.to_owned());
        }
        output
    }

    fn lower_into(&mut self, output: &mut ir::Program) -> Result<(), CompileError> {
        // Lower helper functions and class members before script statements.
        // Remember where this module's items land so leading comments found
        // while walking the script body can be attached to their owners
        // without borrowing `self` alongside the statement `Lowerer`.
        let function_base = output.functions.len();
        for index in 0..self.functions.len() {
            let signature = self.functions[index].signature.clone();
            let ast = self.functions[index].ast;
            let mut lowerer = self.lowerer();
            output
                .functions
                .push(lowerer.lower_function(&signature, ast, None, false, &[])?);
        }
        let class_base = output.classes.len();
        for index in 0..self.classes.len() {
            let class_ir = self.lower_class(index)?;
            output.classes.push(class_ir);
        }
        let mut function_comments: HashMap<String, usize> = HashMap::new();
        for (offset, function) in self.functions.iter().enumerate() {
            function_comments.insert(function.name.clone(), function_base + offset);
        }
        let mut class_comments: HashMap<String, usize> = HashMap::new();
        for (offset, class) in self.classes.iter().enumerate() {
            class_comments.insert(class.source_name.clone(), class_base + offset);
        }

        let globals = self.globals.clone();
        let global_indices = self.global_indices.clone();
        let global_scope = self.imported_global_scope();
        let mut lowerer = Lowerer::new(
            self.unit,
            self.options,
            self.imports.clone(),
            self.function_lookup(),
            self.class_lookup(),
            &mut self.ids.variable,
            global_scope,
            self.program.comments.iter().copied().collect(),
        );
        let mut body = Vec::new();
        let mut pending_comments = Vec::new();
        let mut previous_end = 0usize;
        for statement in &self.program.body {
            let span = statement.span();
            pending_comments.extend(lowerer.comments_between(previous_end, span.start as usize));
            let before = std::mem::take(&mut pending_comments);
            // Leading comments of functions and classes travel with their
            // generated definitions. The definitions themselves are emitted
            // before the script body, so leaving the comments here would
            // detach them from their owners.
            if let Some((function, _)) = function_declaration(statement) {
                let comments: Vec<Stmt> = before.into_iter().map(Stmt::Comment).collect();
                let name = function
                    .id
                    .as_ref()
                    .map(|id| id.name.as_str().to_owned())
                    .unwrap_or_default();
                if let Some(target) = function_comments
                    .get(&name)
                    .and_then(|index| output.functions.get_mut(*index))
                {
                    target.body.splice(0..0, comments);
                } else {
                    body.extend(comments);
                }
                previous_end = span.end as usize;
                pending_comments.extend(lowerer.comments_between(previous_end, previous_end));
                continue;
            }
            if let Some((class, _)) = class_declaration(statement) {
                let comments: Vec<Stmt> = before.into_iter().map(Stmt::Comment).collect();
                let name = class
                    .id
                    .as_ref()
                    .map(|id| id.name.as_str().to_owned())
                    .unwrap_or_default();
                if let Some(target) = class_comments
                    .get(&name)
                    .and_then(|index| output.classes.get_mut(*index))
                {
                    target.constructor.body.splice(0..0, comments);
                } else {
                    body.extend(comments);
                }
                previous_end = span.end as usize;
                pending_comments.extend(lowerer.comments_between(previous_end, previous_end));
                continue;
            }
            body.extend(before.into_iter().map(Stmt::Comment));
            match statement {
                Statement::ImportDeclaration(_) | Statement::ExportNamedDeclaration(_) => {}
                Statement::VariableDeclaration(_) => lower_global_statement(
                    self.unit,
                    &globals,
                    &global_indices,
                    &mut lowerer,
                    statement,
                    &mut body,
                )?,
                Statement::ExportDeclaration(export) => {
                    if let Declaration::VariableDeclaration(declaration) = &export.declaration {
                        lower_global_declaration(
                            self.unit,
                            &globals,
                            &global_indices,
                            &mut lowerer,
                            declaration,
                            &mut body,
                        )?;
                    }
                }
                _ => body.extend(lowerer.lower_statement(statement, None)?),
            }
            previous_end = span.end as usize;
            pending_comments.extend(lowerer.comments_between(previous_end, previous_end));
        }
        body.extend(
            lowerer
                .comments_between(previous_end, self.unit.source.len())
                .into_iter()
                .map(Stmt::Comment),
        );
        output.body.extend(body);
        Ok(())
    }

    fn lower_class(&mut self, index: usize) -> Result<ir::Class, CompileError> {
        let def = self.classes[index].clone();
        let class_name = def.signature.ir_name.clone();
        // Collect comments between class members in source order so doc
        // comments on methods (and fields) survive lowering. Fields have no
        // body of their own, so their leading comments are kept with the
        // constructor; each method keeps its own leading comments.
        let mut cursor = def.ast.span.start as usize;
        let mut field_leading: Vec<String> = Vec::new();
        let mut ctor_leading: Vec<String> = Vec::new();
        let mut method_leading: HashMap<String, Vec<String>> = HashMap::new();
        for element in &def.ast.body.body {
            let span = element.span();
            let leading = self.leading_comments(cursor, span.start as usize);
            match element {
                ClassElement::PropertyDefinition(_) => field_leading.extend(leading),
                ClassElement::MethodDefinition(definition) => match definition.kind {
                    MethodDefinitionKind::Constructor => ctor_leading.extend(leading),
                    MethodDefinitionKind::Method => {
                        let name = property_name(&definition.key).unwrap_or_default();
                        method_leading.entry(name).or_default().extend(leading);
                    }
                    _ => {}
                },
                _ => {}
            }
            cursor = span.end as usize;
        }
        let trailing = self.leading_comments(cursor, def.ast.span.end as usize);
        let mut fields = Vec::new();
        for field_name in &def.field_order {
            let info = &def.signature.fields[field_name];
            let initial = if let Some(expression) = def.initializers.get(field_name) {
                let mut lowerer = self.lowerer();
                lowerer.current_class = Some(class_name.clone());
                let value = lowerer.lower_expression(expression, None)?;
                ensure_type(self.unit, expression.span(), &info.ty, &value.ty)?;
                Some(value)
            } else {
                None
            };
            fields.push(ir::Field {
                name: field_name.clone(),
                ty: info.ty.clone(),
                initial,
            });
        }
        let mut constructor = if let Some(constructor) = &def.constructor {
            let mut lowerer = self.lowerer();
            lowerer.lower_function(
                &constructor.signature,
                constructor.ast,
                Some(&class_name),
                false,
                &constructor.parameter_properties,
            )?
        } else {
            ir::Function {
                name: def.signature.constructor.ir_name.clone(),
                params: Vec::new(),
                return_type: Type::Void,
                body: Vec::new(),
            }
        };
        let prefix: Vec<Stmt> = field_leading
            .into_iter()
            .chain(ctor_leading)
            .map(Stmt::Comment)
            .collect();
        constructor.body.splice(0..0, prefix);
        let mut methods = Vec::new();
        for method in &def.methods {
            let mut lowerer = self.lowerer();
            let mut function = lowerer.lower_function(
                &method.signature,
                method.ast,
                Some(&class_name),
                method.is_static,
                &[],
            )?;
            if let Some(leading) = method_leading.remove(&method.source_name) {
                function
                    .body
                    .splice(0..0, leading.into_iter().map(Stmt::Comment));
            }
            methods.push(ir::Method {
                function,
                is_static: method.is_static,
            });
        }
        if !trailing.is_empty() {
            let comments: Vec<Stmt> = trailing.into_iter().map(Stmt::Comment).collect();
            if let Some(last) = methods.last_mut() {
                last.function.body.extend(comments);
            } else {
                constructor.body.extend(comments);
            }
        }
        methods.sort_by(|a, b| a.function.name.cmp(&b.function.name));
        Ok(ir::Class {
            name: class_name,
            fields,
            constructor,
            methods,
        })
    }

    fn lowerer(&mut self) -> Lowerer<'_, '_, '_> {
        let global_scope = self.global_scope();
        Lowerer::new(
            self.unit,
            self.options,
            self.imports.clone(),
            self.function_lookup(),
            self.class_lookup(),
            &mut self.ids.variable,
            global_scope,
            self.program.comments.iter().copied().collect(),
        )
    }

    fn global_scope(&self) -> HashMap<String, Binding> {
        let mut scope = self.imported_global_scope();
        scope.extend(
            self.globals
                .iter()
                .map(|global| (global.source_name.clone(), global.binding.clone())),
        );
        scope
    }

    fn imported_global_scope(&self) -> HashMap<String, Binding> {
        self.imports
            .iter()
            .filter_map(|(name, symbol)| match symbol {
                ExportedSymbol::Variable(binding) => Some((name.clone(), binding.clone())),
                _ => None,
            })
            .collect()
    }

    fn function_lookup(&self) -> HashMap<String, FunctionSignature> {
        let mut result = self
            .functions
            .iter()
            .map(|function| (function.name.clone(), function.signature.clone()))
            .collect::<HashMap<_, _>>();
        for (name, symbol) in &self.imports {
            if let ExportedSymbol::Function(signature) = symbol {
                result.insert(name.clone(), signature.clone());
            }
        }
        result
    }

    fn class_lookup(&self) -> HashMap<String, ClassSignature> {
        let mut result = self
            .classes
            .iter()
            .map(|class| (class.source_name.clone(), class.signature.clone()))
            .collect::<HashMap<_, _>>();
        for (name, symbol) in &self.imports {
            if let ExportedSymbol::Class(signature) = symbol {
                result.insert(name.clone(), signature.clone());
            }
        }
        result
    }

    fn lookup_class(&self, name: &str) -> Option<&ClassSignature> {
        if let Some(index) = self.class_indices.get(name) {
            return Some(&self.classes[*index].signature);
        }
        match self.imports.get(name) {
            Some(ExportedSymbol::Class(signature)) => Some(signature),
            _ => None,
        }
    }

    fn export_surface(&self) -> Result<ModuleSurface, CompileError> {
        let mut surface = ModuleSurface::default();
        for name in &self.unit.exports {
            let symbol = if let Some(index) = self.function_indices.get(name) {
                ExportedSymbol::Function(self.functions[*index].signature.clone())
            } else if let Some(index) = self.class_indices.get(name) {
                ExportedSymbol::Class(self.classes[*index].signature.clone())
            } else if let Some(index) = self.global_indices.get(name) {
                ExportedSymbol::Variable(self.globals[*index].binding.clone())
            } else {
                // Type-only exports are erased. They remain unavailable for
                // runtime imports, while the resolver still validates spelling.
                continue;
            };
            surface.symbols.insert(name.clone(), symbol);
        }
        Ok(surface)
    }

    fn next_var(&mut self, source_name: &str) -> String {
        let name = format!(
            "__tsh_v_{}_{}",
            shell_identifier(source_name),
            self.ids.variable
        );
        self.ids.variable += 1;
        name
    }

    fn infer_expression_type(
        &self,
        expression: &Expression<'a>,
        current_class: Option<&str>,
        this_ty: Option<&Type>,
    ) -> Result<Option<Type>, CompileError> {
        infer_expr_type(
            self.unit,
            expression,
            &self.globals,
            &self.imports,
            &self.functions,
            &self.classes,
            current_class,
            this_ty,
        )
    }
}

struct Lowerer<'u, 'opts, 'ids> {
    unit: &'u SourceUnit,
    options: &'opts CompileOptions,
    imports: HashMap<String, ExportedSymbol>,
    import_bindings: Vec<ImportBinding>,
    functions: HashMap<String, FunctionSignature>,
    classes: HashMap<String, ClassSignature>,
    variable_ids: &'ids mut usize,
    scopes: ScopeStack,
    comments: Vec<Comment>,
    loop_depth: usize,
    current_return: Option<Type>,
    current_class: Option<String>,
    current_static: bool,
}

impl<'u, 'opts, 'ids> Lowerer<'u, 'opts, 'ids> {
    // Internal lowering context; the wide parameter list mirrors the shared
    // analysis state rather than a public API.
    #[allow(clippy::too_many_arguments)]
    fn new(
        unit: &'u SourceUnit,
        options: &'opts CompileOptions,
        imports: HashMap<String, ExportedSymbol>,
        functions: HashMap<String, FunctionSignature>,
        classes: HashMap<String, ClassSignature>,
        variable_ids: &'ids mut usize,
        global_scope: HashMap<String, Binding>,
        comments: Vec<Comment>,
    ) -> Self {
        Self {
            unit,
            options,
            imports,
            import_bindings: unit.imports.clone(),
            functions,
            classes,
            variable_ids,
            scopes: ScopeStack::new(global_scope),
            comments,
            loop_depth: 0,
            current_return: None,
            current_class: None,
            current_static: false,
        }
    }

    fn lower_function<'a>(
        &mut self,
        signature: &FunctionSignature,
        ast: &OxcFunction<'a>,
        class: Option<&str>,
        is_static: bool,
        parameter_properties: &[(String, String)],
    ) -> Result<ir::Function, CompileError> {
        let return_type = signature
            .return_type
            .clone()
            .ok_or_else(|| err(self.unit, ast.span, "function return type was not resolved"))?;
        let body = ast.body.as_deref().ok_or_else(|| {
            err(
                self.unit,
                ast.span,
                "function declarations need an implementation",
            )
        })?;
        if ast.params.items.len() != signature.params.len() {
            return Err(err(
                self.unit,
                ast.span,
                "function parameter metadata does not match its declaration",
            ));
        }
        self.scopes.push();
        self.loop_depth = 0;
        self.current_return = Some(return_type.clone());
        self.current_class = class.map(str::to_owned);
        self.current_static = is_static;
        let mut params = Vec::new();
        let mut param_bindings = HashMap::new();
        for (ast_param, param_info) in ast.params.items.iter().zip(&signature.params) {
            let source_name = binding_pattern_name(&ast_param.pattern).ok_or_else(|| {
                err(
                    self.unit,
                    ast_param.span,
                    "parameters must use simple identifiers",
                )
            })?;
            if source_name != param_info.source_name {
                return Err(err(
                    self.unit,
                    ast_param.span,
                    "function parameter names do not match collected metadata",
                ));
            }
            let ir_name = self.next_var(&source_name);
            let binding = Binding {
                ir_name: ir_name.clone(),
                ty: param_info.ty.clone(),
                mutable: true,
                imported: false,
            };
            self.scopes
                .current_mut()
                .insert(source_name.clone(), binding.clone());
            param_bindings.insert(source_name, binding.clone());
            params.push(ir::Param {
                name: ir_name,
                ty: param_info.ty.clone(),
            });
        }
        let mut statements = Vec::new();
        for (field_name, parameter_name) in parameter_properties {
            let binding = param_bindings.get(parameter_name).ok_or_else(|| {
                err(
                    self.unit,
                    ast.span,
                    format!("missing parameter property value `{parameter_name}`"),
                )
            })?;
            let class_name = class.ok_or_else(|| {
                err(
                    self.unit,
                    ast.span,
                    "parameter properties are only allowed in constructors",
                )
            })?;
            statements.push(Stmt::Assign {
                place: Place::Field {
                    object: IrExpr {
                        kind: ExprKind::Variable("__tsh_this".to_owned()),
                        ty: Type::Class(class_name.to_owned()),
                    },
                    class: class_name.to_owned(),
                    field: field_name.clone(),
                },
                value: IrExpr {
                    kind: ExprKind::Variable(binding.ir_name.clone()),
                    ty: binding.ty.clone(),
                },
            });
        }
        let start = body.span.start as usize + 1;
        let end = body.span.end.saturating_sub(1) as usize;
        statements.extend(self.lower_sequence(&body.statements, start, end)?);
        if return_type != Type::Void && !always_returns(&statements) {
            return Err(err(
                self.unit,
                ast.span,
                format!(
                    "function `{}` does not return a value on every path",
                    ast.id
                        .as_ref()
                        .map(|id| id.name.as_str())
                        .unwrap_or("<method>")
                ),
            ));
        }
        self.scopes.pop();
        self.current_return = None;
        self.current_class = None;
        self.current_static = false;
        Ok(ir::Function {
            name: signature.ir_name.clone(),
            params,
            return_type,
            body: statements,
        })
    }

    fn lower_sequence<'a>(
        &mut self,
        statements: &[Statement<'a>],
        start: usize,
        end: usize,
    ) -> Result<Vec<Stmt>, CompileError> {
        let mut output = Vec::new();
        let mut cursor = start;
        for statement in statements {
            let span = statement.span();
            output.extend(
                self.comments_between(cursor, span.start as usize)
                    .into_iter()
                    .map(Stmt::Comment),
            );
            output.extend(self.lower_statement(statement, None)?);
            cursor = span.end as usize;
        }
        output.extend(
            self.comments_between(cursor, end)
                .into_iter()
                .map(Stmt::Comment),
        );
        Ok(output)
    }

    fn comments_between(&self, start: usize, end: usize) -> Vec<String> {
        if self.options.comments == Comments::None {
            return Vec::new();
        }
        let mut output = Vec::new();
        for comment in &self.comments {
            let cstart = comment.span.start as usize;
            let cend = comment.span.end as usize;
            if cstart < start || cend > end {
                continue;
            }
            let Some(text) = self.unit.source.get(cstart..cend) else {
                continue;
            };
            if is_compiler_directive(text) {
                continue;
            }
            let is_doc = text.starts_with("/**");
            if self.options.comments == Comments::Doc && !is_doc {
                continue;
            }
            output.push(text.to_owned());
        }
        output
    }

    fn lower_statement<'a>(
        &mut self,
        statement: &Statement<'a>,
        expected_return: Option<&Type>,
    ) -> Result<Vec<Stmt>, CompileError> {
        match statement {
            Statement::EmptyStatement(_) | Statement::DebuggerStatement(_) => Ok(Vec::new()),
            Statement::BlockStatement(block) => {
                self.scopes.push();
                let start = block.span.start as usize + 1;
                let end = block.span.end.saturating_sub(1) as usize;
                let body = self.lower_sequence(&block.body, start, end)?;
                self.scopes.pop();
                Ok(vec![Stmt::Block(body)])
            }
            Statement::VariableDeclaration(declaration) => {
                self.lower_variable_declaration(declaration)
            }
            Statement::ExpressionStatement(expression_statement) => {
                self.lower_expression_statement(&expression_statement.expression)
            }
            Statement::IfStatement(statement) => {
                let condition = self.lower_expression(&statement.test, None)?;
                ensure_type(
                    self.unit,
                    statement.test.span(),
                    &Type::Boolean,
                    &condition.ty,
                )?;
                let then_body = self.lower_scoped_arm(&statement.consequent)?;
                let else_body = if let Some(alternate) = &statement.alternate {
                    self.lower_scoped_arm(alternate)?
                } else {
                    Vec::new()
                };
                Ok(vec![Stmt::If {
                    condition,
                    then_body,
                    else_body,
                }])
            }
            Statement::WhileStatement(statement) => {
                let condition = self.lower_expression(&statement.test, None)?;
                ensure_type(
                    self.unit,
                    statement.test.span(),
                    &Type::Boolean,
                    &condition.ty,
                )?;
                self.loop_depth += 1;
                let body = self.lower_scoped_arm(&statement.body)?;
                self.loop_depth -= 1;
                Ok(vec![Stmt::While { condition, body }])
            }
            Statement::ForStatement(statement) => self.lower_for(statement),
            Statement::BreakStatement(statement) => {
                if statement.label.is_some() {
                    return Err(err(
                        self.unit,
                        statement.span,
                        "labeled break is not supported",
                    ));
                }
                if self.loop_depth == 0 {
                    return Err(err(
                        self.unit,
                        statement.span,
                        "break is only valid inside a loop",
                    ));
                }
                Ok(vec![Stmt::Break])
            }
            Statement::ContinueStatement(statement) => {
                if statement.label.is_some() {
                    return Err(err(
                        self.unit,
                        statement.span,
                        "labeled continue is not supported",
                    ));
                }
                if self.loop_depth == 0 {
                    return Err(err(
                        self.unit,
                        statement.span,
                        "continue is only valid inside a loop",
                    ));
                }
                Ok(vec![Stmt::Continue])
            }
            Statement::ReturnStatement(statement) => {
                let expected = self
                    .current_return
                    .as_ref()
                    .or(expected_return)
                    .ok_or_else(|| {
                        err(
                            self.unit,
                            statement.span,
                            "return is only valid inside a function",
                        )
                    })?
                    .clone();
                let value = statement
                    .argument
                    .as_ref()
                    .map(|expr| self.lower_expression(expr, Some(&expected)))
                    .transpose()?;
                match (&expected, &value) {
                    (Type::Void, None) => Ok(vec![Stmt::Return(None)]),
                    (Type::Void, Some(expr)) => Err(err(
                        self.unit,
                        statement.span,
                        format!("void function cannot return {}", type_name(&expr.ty)),
                    )),
                    (_, None) => Err(err(
                        self.unit,
                        statement.span,
                        format!("function must return {}", type_name(&expected)),
                    )),
                    (_, Some(expr)) => {
                        ensure_type(self.unit, statement.span, &expected, &expr.ty)?;
                        Ok(vec![Stmt::Return(Some(expr.clone()))])
                    }
                }
            }
            Statement::ImportDeclaration(_) | Statement::ExportNamedDeclaration(_) => {
                Ok(Vec::new())
            }
            _ => Err(err(
                self.unit,
                statement.span(),
                format!(
                    "unsupported statement `{}`",
                    source_fragment(self.unit, statement.span())
                ),
            )),
        }
    }

    fn lower_scoped_arm<'a>(
        &mut self,
        statement: &Statement<'a>,
    ) -> Result<Vec<Stmt>, CompileError> {
        self.scopes.push();
        let result = self.lower_statement(statement, None)?;
        self.scopes.pop();
        match result.as_slice() {
            [Stmt::Block(body)] => Ok(body.clone()),
            _ => Ok(result),
        }
    }

    fn lower_variable_declaration<'a>(
        &mut self,
        declaration: &oxc_ast::ast::VariableDeclaration<'a>,
    ) -> Result<Vec<Stmt>, CompileError> {
        if declaration.kind != VariableDeclarationKind::Const
            && declaration.kind != VariableDeclarationKind::Let
        {
            return Err(err(
                self.unit,
                declaration.span,
                "only const and let declarations are supported",
            ));
        }
        let mut output = Vec::new();
        for declarator in &declaration.declarations {
            let Some(source_name) = binding_pattern_name(&declarator.id) else {
                return Err(err(
                    self.unit,
                    declarator.span,
                    "variables must use simple identifiers",
                ));
            };
            if self.scopes.current().contains_key(&source_name) {
                return Err(err(
                    self.unit,
                    declarator.span,
                    format!("duplicate declaration `{source_name}`"),
                ));
            }
            let init = declarator.init.as_ref().ok_or_else(|| {
                err(
                    self.unit,
                    declarator.span,
                    "uninitialized variable declarations are not supported",
                )
            })?;
            let annotation = declarator
                .type_annotation
                .as_deref()
                .map(|a| self.annotation_type(&a.type_annotation, a.span))
                .transpose()?;
            let value = self.lower_expression(init, annotation.as_ref())?;
            let ty = annotation.unwrap_or_else(|| value.ty.clone());
            ensure_type(self.unit, declarator.span, &ty, &value.ty)?;
            if ty == Type::Void {
                return Err(err(
                    self.unit,
                    declarator.span,
                    "variables cannot have type void",
                ));
            }
            let ir_name = self.next_var(&source_name);
            self.scopes.current_mut().insert(
                source_name,
                Binding {
                    ir_name: ir_name.clone(),
                    ty,
                    mutable: declaration.kind == VariableDeclarationKind::Let,
                    imported: false,
                },
            );
            output.push(Stmt::Let {
                name: ir_name,
                value,
            });
        }
        Ok(output)
    }

    fn lower_for<'a>(
        &mut self,
        statement: &oxc_ast::ast::ForStatement<'a>,
    ) -> Result<Vec<Stmt>, CompileError> {
        self.scopes.push();
        let mut init = Vec::new();
        if let Some(initializer) = &statement.init {
            match initializer {
                ForStatementInit::VariableDeclaration(declaration) => {
                    init.extend(self.lower_variable_declaration(declaration)?)
                }
                _ => init.extend(self.lower_for_expression(initializer.to_expression())?),
            }
        }
        let condition = statement
            .test
            .as_ref()
            .map(|expr| {
                let result = self.lower_expression(expr, None)?;
                ensure_type(self.unit, expr.span(), &Type::Boolean, &result.ty)?;
                Ok(result)
            })
            .transpose()?;
        let update = statement
            .update
            .as_ref()
            .map(|expr| self.lower_for_expression(expr))
            .transpose()?
            .unwrap_or_default();
        self.loop_depth += 1;
        let body = self.lower_scoped_arm(&statement.body)?;
        self.loop_depth -= 1;
        self.scopes.pop();
        Ok(vec![Stmt::For {
            init,
            condition,
            update,
            body,
        }])
    }

    fn lower_for_expression<'a>(
        &mut self,
        expression: &Expression<'a>,
    ) -> Result<Vec<Stmt>, CompileError> {
        match expression {
            Expression::AssignmentExpression(assignment) => self.lower_assignment(assignment),
            Expression::UpdateExpression(update) => Ok(vec![self.lower_update(update)?]),
            _ => Err(err(
                self.unit,
                expression.span(),
                "for-loop initializer and update must be assignments or increments",
            )),
        }
    }

    fn lower_expression_statement<'a>(
        &mut self,
        expression: &Expression<'a>,
    ) -> Result<Vec<Stmt>, CompileError> {
        match expression {
            Expression::CallExpression(call) => {
                if is_raw_macro(&call.callee) {
                    let raw = self.lower_raw_call(call)?;
                    return Ok(vec![Stmt::Raw(raw)]);
                }
                if let Some(name) = direct_identifier(&call.callee)
                    && let Some(command) = command_for_name(name)
                    && !self.functions.contains_key(name)
                {
                    return self.lower_command(command, call);
                }
                let result = self.lower_expression(expression, None)?;
                Ok(vec![Stmt::Expr(result)])
            }
            Expression::AssignmentExpression(assignment) => self.lower_assignment(assignment),
            Expression::UpdateExpression(update) => Ok(vec![self.lower_update(update)?]),
            _ => {
                let result = self.lower_expression(expression, None)?;
                if result.ty == Type::Void {
                    return Err(err(
                        self.unit,
                        expression.span(),
                        "void expression is not a statement",
                    ));
                }
                Ok(vec![Stmt::Expr(result)])
            }
        }
    }

    fn lower_command<'a>(
        &mut self,
        command: ir::Command,
        call: &oxc_ast::ast::CallExpression<'a>,
    ) -> Result<Vec<Stmt>, CompileError> {
        let name = direct_identifier(&call.callee).unwrap_or("command");
        let mut recursive = false;
        let path_args: Vec<IrExpr> = match command {
            ir::Command::Mkdir | ir::Command::Rm => {
                let mut path_args = Vec::new();
                for argument in &call.arguments {
                    match argument_expression(self.unit, argument)? {
                        Expression::ObjectExpression(object) => {
                            recursive = parse_recursive_option(self.unit, object.as_ref())?;
                        }
                        expression => path_args.push(self.lower_expression(expression, None)?),
                    }
                }
                path_args
            }
            ir::Command::Run => {
                if call.arguments.len() != 2 {
                    return Err(err(
                        self.unit,
                        call.span,
                        "run expects a command string and an array of argument strings",
                    ));
                }
                let command_ast = argument_expression(self.unit, &call.arguments[0])?;
                let command_expr = self.lower_expression(command_ast, Some(&Type::String))?;
                ensure_type(
                    self.unit,
                    call.arguments[0].span(),
                    &Type::String,
                    &command_expr.ty,
                )?;
                let mut result = vec![command_expr];
                let array_ast = argument_expression(self.unit, &call.arguments[1])?;
                let Expression::ArrayExpression(array) = array_ast else {
                    return Err(err(
                        self.unit,
                        call.arguments[1].span(),
                        "run arguments must be a string array literal",
                    ));
                };
                for item in &array.elements {
                    let expression = match item {
                        oxc_ast::ast::ArrayExpressionElement::SpreadElement(_)
                        | oxc_ast::ast::ArrayExpressionElement::Elision(_) => {
                            return Err(err(
                                self.unit,
                                item.span(),
                                "run argument arrays cannot contain spreads or holes",
                            ));
                        }
                        _ => item.to_expression(),
                    };
                    result.push(self.lower_expression(expression, Some(&Type::String))?);
                    ensure_type(
                        self.unit,
                        expression.span(),
                        &Type::String,
                        &result.last().expect("just pushed").ty,
                    )?;
                }
                result
            }
            ir::Command::Exit => {
                let args = self.lower_command_args(call)?;
                if args.len() > 1 {
                    return Err(err(
                        self.unit,
                        call.span,
                        "exit accepts zero or one numeric status",
                    ));
                }
                for arg in &args {
                    ensure_type(self.unit, call.span, &Type::Number, &arg.ty)?;
                }
                args
            }
            ir::Command::Chmod => {
                if !self.import_bindings.iter().any(|binding| {
                    binding.imported == "chmod"
                        && matches!(binding.target, ImportTarget::Builtin(BuiltinModule::Unix))
                }) {
                    return Err(err(
                        self.unit,
                        call.span,
                        "chmod must be imported from `tsh:unix`",
                    ));
                }
                if self.options.os == TargetOs::Windows {
                    return Err(err(self.unit, call.span, "chmod is unavailable on Windows"));
                }
                let args = self.lower_command_args(call)?;
                if args.len() != 2 {
                    return Err(err(self.unit, call.span, "chmod expects a mode and a path"));
                }
                ensure_type(self.unit, call.span, &Type::String, &args[0].ty)?;
                if !matches!(args[1].ty, Type::Number | Type::String) {
                    return Err(err(
                        self.unit,
                        call.span,
                        "chmod mode must be a number or string",
                    ));
                }
                args
            }
            ir::Command::Cp | ir::Command::Mv => {
                let args = self.lower_command_args(call)?;
                if args.len() != 2 {
                    return Err(err(
                        self.unit,
                        call.span,
                        format!("{name} expects exactly two paths"),
                    ));
                }
                for arg in &args {
                    ensure_type(self.unit, call.span, &Type::String, &arg.ty)?;
                }
                args
            }
            ir::Command::Cd => {
                let args = self.lower_command_args(call)?;
                if args.len() != 1 {
                    return Err(err(self.unit, call.span, "cd expects exactly one path"));
                }
                ensure_type(self.unit, call.span, &Type::String, &args[0].ty)?;
                args
            }
            ir::Command::Echo => {
                let args = self.lower_command_args(call)?;
                if args.is_empty() {
                    return Err(err(self.unit, call.span, "echo expects at least one value"));
                }
                for arg in &args {
                    ensure_scalar(self.unit, call.span, &arg.ty)?;
                }
                args
            }
        };
        if matches!(command, ir::Command::Mkdir | ir::Command::Rm) {
            if path_args.is_empty() {
                return Err(err(
                    self.unit,
                    call.span,
                    format!("{name} expects at least one path"),
                ));
            }
            for arg in &path_args {
                ensure_type(self.unit, call.span, &Type::String, &arg.ty)?;
            }
        }
        Ok(vec![Stmt::Command {
            kind: command,
            args: path_args,
            recursive,
        }])
    }

    fn lower_command_args<'a>(
        &mut self,
        call: &oxc_ast::ast::CallExpression<'a>,
    ) -> Result<Vec<IrExpr>, CompileError> {
        call.arguments
            .iter()
            .map(|argument| self.lower_argument(argument))
            .collect()
    }

    fn lower_argument<'a>(&mut self, argument: &Argument<'a>) -> Result<IrExpr, CompileError> {
        if matches!(argument, Argument::SpreadElement(_)) {
            return Err(err(
                self.unit,
                argument.span(),
                "spread arguments are not supported",
            ));
        }
        self.lower_expression(argument.to_expression(), None)
    }

    fn lower_expression<'a>(
        &mut self,
        expression: &Expression<'a>,
        expected: Option<&Type>,
    ) -> Result<IrExpr, CompileError> {
        let result = match expression {
            Expression::StringLiteral(literal) => {
                let value = literal.value.as_str().to_owned();
                reject_nul(self.unit, literal.span, &value)?;
                IrExpr {
                    kind: ExprKind::String(value),
                    ty: Type::String,
                }
            }
            Expression::NumericLiteral(literal) => {
                let value = literal
                    .raw
                    .as_ref()
                    .and_then(|raw| parse_integer_literal(raw.as_str()))
                    .ok_or_else(|| {
                        err(
                            self.unit,
                            literal.span,
                            "only exact signed 64-bit integer literals are supported",
                        )
                    })?;
                IrExpr {
                    kind: ExprKind::Number(value),
                    ty: Type::Number,
                }
            }
            Expression::BooleanLiteral(literal) => IrExpr {
                kind: ExprKind::Boolean(literal.value),
                ty: Type::Boolean,
            },
            Expression::Identifier(identifier) => {
                let name = identifier.name.as_str();
                if let Some(binding) = self.scopes.get(name) {
                    IrExpr {
                        kind: ExprKind::Variable(binding.ir_name.clone()),
                        ty: binding.ty.clone(),
                    }
                } else if let Some(ExportedSymbol::Variable(binding)) = self.imports.get(name) {
                    IrExpr {
                        kind: ExprKind::Variable(binding.ir_name.clone()),
                        ty: binding.ty.clone(),
                    }
                } else {
                    return Err(err(
                        self.unit,
                        identifier.span,
                        format!("unknown variable `{name}`"),
                    ));
                }
            }
            Expression::TemplateLiteral(template) => {
                let mut parts = Vec::new();
                for (index, quasi) in template.quasis.iter().enumerate() {
                    let value = quasi
                        .value
                        .cooked
                        .as_ref()
                        .unwrap_or(&quasi.value.raw)
                        .as_str()
                        .to_owned();
                    reject_nul(self.unit, quasi.span, &value)?;
                    if !value.is_empty() {
                        parts.push(IrExpr {
                            kind: ExprKind::String(value),
                            ty: Type::String,
                        });
                    }
                    if let Some(expression) = template.expressions.get(index) {
                        parts.push(self.lower_expression(expression, None)?);
                    }
                }
                for part in &parts {
                    ensure_scalar(self.unit, template.span, &part.ty)?;
                }
                IrExpr {
                    kind: ExprKind::Template(parts),
                    ty: Type::String,
                }
            }
            Expression::BinaryExpression(binary) => {
                let left = self.lower_expression(&binary.left, None)?;
                let right = self.lower_expression(&binary.right, None)?;
                self.lower_binary(binary.operator, left, right, binary.span)?
            }
            Expression::LogicalExpression(logical) => {
                if logical.operator == LogicalOperator::Coalesce {
                    return Err(err(
                        self.unit,
                        logical.span,
                        "nullish coalescing is not supported",
                    ));
                }
                let left = self.lower_expression(&logical.left, Some(&Type::Boolean))?;
                let right = self.lower_expression(&logical.right, Some(&Type::Boolean))?;
                ensure_type(self.unit, logical.left.span(), &Type::Boolean, &left.ty)?;
                ensure_type(self.unit, logical.right.span(), &Type::Boolean, &right.ty)?;
                IrExpr {
                    kind: ExprKind::Binary {
                        op: if logical.operator == LogicalOperator::And {
                            "&&"
                        } else {
                            "||"
                        }
                        .to_owned(),
                        left: Box::new(left),
                        right: Box::new(right),
                    },
                    ty: Type::Boolean,
                }
            }
            Expression::UnaryExpression(unary) => {
                if unary.operator == UnaryOperator::UnaryNegation
                    && let Expression::NumericLiteral(literal) = &unary.argument
                    && literal.raw.as_ref().is_some_and(|raw| {
                        parse_integer_magnitude(raw.as_str()) == Some(1u64 << 63)
                    })
                {
                    return Ok(IrExpr {
                        kind: ExprKind::Number(i64::MIN),
                        ty: Type::Number,
                    });
                }
                let value = self.lower_expression(&unary.argument, None)?;
                match unary.operator {
                    UnaryOperator::LogicalNot => {
                        ensure_type(self.unit, unary.span, &Type::Boolean, &value.ty)?;
                        IrExpr {
                            kind: ExprKind::Unary {
                                op: "!".to_owned(),
                                value: Box::new(value),
                            },
                            ty: Type::Boolean,
                        }
                    }
                    UnaryOperator::UnaryPlus => {
                        ensure_type(self.unit, unary.span, &Type::Number, &value.ty)?;
                        IrExpr {
                            kind: ExprKind::Unary {
                                op: "+".to_owned(),
                                value: Box::new(value),
                            },
                            ty: Type::Number,
                        }
                    }
                    UnaryOperator::UnaryNegation => {
                        ensure_type(self.unit, unary.span, &Type::Number, &value.ty)?;
                        IrExpr {
                            kind: ExprKind::Unary {
                                op: "-".to_owned(),
                                value: Box::new(value),
                            },
                            ty: Type::Number,
                        }
                    }
                    _ => return Err(err(self.unit, unary.span, "unsupported unary operator")),
                }
            }
            Expression::ParenthesizedExpression(parenthesized) => {
                self.lower_expression(&parenthesized.expression, expected)?
            }
            Expression::ThisExpression(this_expression) => {
                let class = self.current_class.as_ref().ok_or_else(|| {
                    err(
                        self.unit,
                        this_expression.span,
                        "this is only available inside an instance method or constructor",
                    )
                })?;
                if self.current_static {
                    return Err(err(
                        self.unit,
                        this_expression.span,
                        "this is not available in a static method",
                    ));
                }
                IrExpr {
                    kind: ExprKind::Variable("__tsh_this".to_owned()),
                    ty: Type::Class(class.clone()),
                }
            }
            Expression::StaticMemberExpression(member) => {
                self.lower_field_access(&member.object, member.property.name.as_str(), member.span)?
            }
            Expression::CallExpression(call) => self.lower_call(call)?,
            Expression::NewExpression(new_expression) => self.lower_new(new_expression)?,
            Expression::TSNonNullExpression(non_null) => {
                self.lower_expression(&non_null.expression, expected)?
            }
            Expression::TSAsExpression(assertion) => {
                let value = self.lower_expression(&assertion.expression, None)?;
                let annotation =
                    self.annotation_type(&assertion.type_annotation, assertion.span)?;
                ensure_type(self.unit, assertion.span, &annotation, &value.ty)?;
                value
            }
            Expression::TSSatisfiesExpression(assertion) => {
                let value = self.lower_expression(&assertion.expression, None)?;
                let annotation =
                    self.annotation_type(&assertion.type_annotation, assertion.span)?;
                ensure_type(self.unit, assertion.span, &annotation, &value.ty)?;
                value
            }
            _ => {
                return Err(err(
                    self.unit,
                    expression.span(),
                    format!(
                        "unsupported expression `{}`",
                        source_fragment(self.unit, expression.span())
                    ),
                ));
            }
        };
        if let Some(expected) = expected {
            ensure_type(self.unit, expression.span(), expected, &result.ty)?;
        }
        Ok(result)
    }

    fn lower_binary(
        &self,
        operator: BinaryOperator,
        left: IrExpr,
        right: IrExpr,
        span: Span,
    ) -> Result<IrExpr, CompileError> {
        let (op, ty) = match operator {
            BinaryOperator::Addition if left.ty == Type::String && right.ty == Type::String => {
                ("+", Type::String)
            }
            BinaryOperator::Addition
            | BinaryOperator::Subtraction
            | BinaryOperator::Multiplication
            | BinaryOperator::Division
            | BinaryOperator::Remainder => {
                ensure_type(self.unit, span, &Type::Number, &left.ty)?;
                ensure_type(self.unit, span, &Type::Number, &right.ty)?;
                (operator.as_str(), Type::Number)
            }
            BinaryOperator::Equality
            | BinaryOperator::StrictEquality
            | BinaryOperator::Inequality
            | BinaryOperator::StrictInequality => {
                if left.ty != right.ty || matches!(left.ty, Type::Void | Type::Class(_)) {
                    return Err(err(
                        self.unit,
                        span,
                        format!(
                            "cannot compare {} with {}",
                            type_name(&left.ty),
                            type_name(&right.ty)
                        ),
                    ));
                }
                (operator.as_str(), Type::Boolean)
            }
            BinaryOperator::LessThan
            | BinaryOperator::LessEqualThan
            | BinaryOperator::GreaterThan
            | BinaryOperator::GreaterEqualThan => {
                if left.ty != right.ty || !matches!(left.ty, Type::Number | Type::String) {
                    return Err(err(
                        self.unit,
                        span,
                        "relational comparisons require two numbers or two strings",
                    ));
                }
                (operator.as_str(), Type::Boolean)
            }
            _ => {
                return Err(err(
                    self.unit,
                    span,
                    format!("operator `{}` is not supported", operator.as_str()),
                ));
            }
        };
        Ok(IrExpr {
            kind: ExprKind::Binary {
                op: op.to_owned(),
                left: Box::new(left),
                right: Box::new(right),
            },
            ty,
        })
    }

    fn lower_call<'a>(
        &mut self,
        call: &oxc_ast::ast::CallExpression<'a>,
    ) -> Result<IrExpr, CompileError> {
        if is_raw_macro(&call.callee) {
            return Err(err(
                self.unit,
                call.span,
                "raw! is only valid as a standalone statement",
            ));
        }
        if let Some(name) = direct_identifier(&call.callee) {
            if name == "raw" {
                return Err(err(
                    self.unit,
                    call.span,
                    "use raw!(...) with one literal string or template",
                ));
            }
            if name == "env" {
                if call.arguments.len() != 1 {
                    return Err(err(
                        self.unit,
                        call.span,
                        "env expects exactly one variable name",
                    ));
                }
                let argument = self.lower_argument(&call.arguments[0])?;
                ensure_type(self.unit, call.span, &Type::String, &argument.ty)?;
                return Ok(IrExpr {
                    kind: ExprKind::Env(Box::new(argument)),
                    ty: Type::String,
                });
            }
            if let Some(signature) = self.functions.get(name).cloned() {
                let args = self.lower_call_args(call, &signature.params)?;
                let ty = signature.return_type.clone().ok_or_else(|| {
                    err(
                        self.unit,
                        call.span,
                        format!("return type of `{name}` is unresolved"),
                    )
                })?;
                return Ok(IrExpr {
                    kind: ExprKind::Call {
                        function: signature.ir_name,
                        args,
                    },
                    ty,
                });
            }
            if command_for_name(name).is_some() {
                return Err(err(
                    self.unit,
                    call.span,
                    format!("command `{name}` can only be used as a standalone statement"),
                ));
            }
            if self.scopes.get(name).is_some()
                || matches!(self.imports.get(name), Some(ExportedSymbol::Variable(_)))
            {
                return Err(err(
                    self.unit,
                    call.span,
                    format!("value `{name}` is not callable"),
                ));
            }
            return Err(err(
                self.unit,
                call.span,
                format!("unknown function `{name}`"),
            ));
        }
        if let Expression::StaticMemberExpression(member) = &call.callee {
            let method_name = member.property.name.as_str();
            if let Some(class_name) = direct_identifier(&member.object)
                && let Some(class) = self.classes.get(class_name).cloned()
            {
                let method = class.methods.get(method_name).ok_or_else(|| {
                    err(
                        self.unit,
                        member.span,
                        format!("class `{class_name}` has no method `{method_name}`"),
                    )
                })?;
                if !method.is_static {
                    return Err(err(
                        self.unit,
                        member.span,
                        format!("`{class_name}.{method_name}` is an instance method"),
                    ));
                }
                let args = self.lower_call_args(call, &method.function.params)?;
                let ty =
                    method.function.return_type.clone().ok_or_else(|| {
                        err(self.unit, call.span, "method return type is unresolved")
                    })?;
                return Ok(IrExpr {
                    kind: ExprKind::Method {
                        object: None,
                        class: class.ir_name,
                        method: method.ir_method_name.clone(),
                        args,
                    },
                    ty,
                });
            }
            let object = self.lower_expression(&member.object, None)?;
            let Type::Class(class_name) = object.ty.clone() else {
                return Err(err(
                    self.unit,
                    member.span,
                    format!(
                        "method receiver has type {}, expected a class instance",
                        type_name(&object.ty)
                    ),
                ));
            };
            let class = self
                .classes
                .values()
                .find(|class| class.ir_name == class_name)
                .ok_or_else(|| err(self.unit, member.span, "unknown class instance"))?;
            let method = class.methods.get(method_name).ok_or_else(|| {
                err(
                    self.unit,
                    member.span,
                    format!("class has no method `{method_name}`"),
                )
            })?;
            if method.is_static {
                return Err(err(
                    self.unit,
                    member.span,
                    format!("static method `{method_name}` must be called on its class"),
                ));
            }
            let signature = method.function.clone();
            let ir_method_name = method.ir_method_name.clone();
            let args = self.lower_call_args(call, &signature.params)?;
            let ty = signature
                .return_type
                .clone()
                .ok_or_else(|| err(self.unit, call.span, "method return type is unresolved"))?;
            return Ok(IrExpr {
                kind: ExprKind::Method {
                    object: Some(Box::new(object)),
                    class: class_name,
                    method: ir_method_name,
                    args,
                },
                ty,
            });
        }
        Err(err(
            self.unit,
            call.span,
            "only named functions and class methods can be called",
        ))
    }

    fn lower_call_args<'a>(
        &mut self,
        call: &oxc_ast::ast::CallExpression<'a>,
        params: &[ParamInfo],
    ) -> Result<Vec<IrExpr>, CompileError> {
        let required = params.iter().filter(|param| !param.optional).count();
        if call.arguments.len() < required || call.arguments.len() > params.len() {
            return Err(err(
                self.unit,
                call.span,
                format!(
                    "expected {}{} arguments, got {}",
                    required,
                    if required == params.len() {
                        String::new()
                    } else {
                        format!(" to {}", params.len())
                    },
                    call.arguments.len()
                ),
            ));
        }
        let mut output = Vec::new();
        for (argument, parameter) in call.arguments.iter().zip(params) {
            let value = self.lower_argument(argument)?;
            ensure_type(self.unit, argument.span(), &parameter.ty, &value.ty)?;
            output.push(value);
        }
        Ok(output)
    }

    fn lower_new<'a>(
        &mut self,
        expression: &oxc_ast::ast::NewExpression<'a>,
    ) -> Result<IrExpr, CompileError> {
        let Some(name) = direct_identifier(&expression.callee) else {
            return Err(err(
                self.unit,
                expression.span,
                "new requires a named class",
            ));
        };
        let class = self.classes.get(name).cloned().ok_or_else(|| {
            err(
                self.unit,
                expression.span,
                format!("unknown class `{name}`"),
            )
        })?;
        let args = self.lower_call_args_for_new(expression, &class.constructor.params)?;
        Ok(IrExpr {
            kind: ExprKind::New {
                class: class.ir_name.clone(),
                args,
            },
            ty: Type::Class(class.ir_name),
        })
    }

    fn lower_call_args_for_new<'a>(
        &mut self,
        call: &oxc_ast::ast::NewExpression<'a>,
        params: &[ParamInfo],
    ) -> Result<Vec<IrExpr>, CompileError> {
        let required = params.iter().filter(|param| !param.optional).count();
        if call.arguments.len() < required || call.arguments.len() > params.len() {
            return Err(err(
                self.unit,
                call.span,
                format!(
                    "constructor expects {}{} arguments, got {}",
                    required,
                    if required == params.len() {
                        String::new()
                    } else {
                        format!(" to {}", params.len())
                    },
                    call.arguments.len()
                ),
            ));
        }
        let mut output = Vec::new();
        for (argument, parameter) in call.arguments.iter().zip(params) {
            let value = self.lower_argument(argument)?;
            ensure_type(self.unit, argument.span(), &parameter.ty, &value.ty)?;
            output.push(value);
        }
        Ok(output)
    }

    fn lower_field_access<'a>(
        &mut self,
        object_ast: &Expression<'a>,
        field: &str,
        span: Span,
    ) -> Result<IrExpr, CompileError> {
        if direct_identifier(object_ast).is_some_and(|name| self.classes.contains_key(name)) {
            return Err(err(
                self.unit,
                span,
                "static class fields are not supported",
            ));
        }
        let object = self.lower_expression(object_ast, None)?;
        let Type::Class(class_name) = object.ty.clone() else {
            return Err(err(
                self.unit,
                span,
                format!(
                    "field access requires a class instance, got {}",
                    type_name(&object.ty)
                ),
            ));
        };
        let class = self
            .classes
            .values()
            .find(|class| class.ir_name == class_name)
            .ok_or_else(|| err(self.unit, span, "unknown class instance"))?;
        let info = class
            .fields
            .get(field)
            .ok_or_else(|| err(self.unit, span, format!("class has no field `{field}`")))?;
        Ok(IrExpr {
            kind: ExprKind::Field {
                object: Box::new(object),
                class: class_name,
                field: field.to_owned(),
            },
            ty: info.ty.clone(),
        })
    }

    fn lower_assignment<'a>(
        &mut self,
        assignment: &oxc_ast::ast::AssignmentExpression<'a>,
    ) -> Result<Vec<Stmt>, CompileError> {
        let (place, target_type, mutable, old_value, target_span) =
            self.lower_assignment_target(&assignment.left)?;
        if !mutable {
            return Err(err(
                self.unit,
                target_span,
                "cannot assign to an immutable binding or readonly field",
            ));
        }
        let mut value = self.lower_expression(&assignment.right, Some(&target_type))?;
        if assignment.operator != AssignmentOperator::Assign {
            let op = assignment.operator.to_binary_operator().ok_or_else(|| {
                err(
                    self.unit,
                    assignment.span,
                    "logical and bitwise compound assignments are not supported",
                )
            })?;
            if matches!(place, Place::Field { .. }) && !field_receiver_is_simple(&old_value) {
                return Err(err(
                    self.unit,
                    assignment.span,
                    "compound assignment on a field with a computed receiver is not supported",
                ));
            }
            value = self.lower_binary(op, old_value, value, assignment.span)?;
            ensure_type(self.unit, assignment.span, &target_type, &value.ty)?;
        }
        Ok(vec![Stmt::Assign { place, value }])
    }

    fn lower_assignment_target<'a>(
        &mut self,
        target: &AssignmentTarget<'a>,
    ) -> Result<(Place, Type, bool, IrExpr, Span), CompileError> {
        match target {
            AssignmentTarget::AssignmentTargetIdentifier(identifier) => {
                let name = identifier.name.as_str();
                let binding = self
                    .scopes
                    .get(name)
                    .cloned()
                    .or_else(|| match self.imports.get(name) {
                        Some(ExportedSymbol::Variable(binding)) => Some(binding.clone()),
                        _ => None,
                    })
                    .ok_or_else(|| {
                        err(
                            self.unit,
                            identifier.span,
                            format!("unknown variable `{name}`"),
                        )
                    })?;
                let old = IrExpr {
                    kind: ExprKind::Variable(binding.ir_name.clone()),
                    ty: binding.ty.clone(),
                };
                Ok((
                    Place::Variable(binding.ir_name),
                    binding.ty,
                    binding.mutable && !binding.imported,
                    old,
                    identifier.span,
                ))
            }
            AssignmentTarget::StaticMemberExpression(member) => {
                self.lower_field_place(&member.object, member.property.name.as_str(), member.span)
            }
            AssignmentTarget::TSAsExpression(assertion) => {
                self.lower_assignment_target_from_expression(&assertion.expression, assertion.span)
            }
            AssignmentTarget::TSNonNullExpression(assertion) => {
                self.lower_assignment_target_from_expression(&assertion.expression, assertion.span)
            }
            _ => Err(err(
                self.unit,
                target.span(),
                "assignment target must be a variable or class field",
            )),
        }
    }

    fn lower_assignment_target_from_expression<'a>(
        &mut self,
        expression: &Expression<'a>,
        span: Span,
    ) -> Result<(Place, Type, bool, IrExpr, Span), CompileError> {
        match expression {
            Expression::Identifier(identifier) => {
                let name = identifier.name.as_str();
                let binding = self
                    .scopes
                    .get(name)
                    .cloned()
                    .or_else(|| match self.imports.get(name) {
                        Some(ExportedSymbol::Variable(binding)) => Some(binding.clone()),
                        _ => None,
                    })
                    .ok_or_else(|| {
                        err(
                            self.unit,
                            identifier.span,
                            format!("unknown variable `{name}`"),
                        )
                    })?;
                let old = IrExpr {
                    kind: ExprKind::Variable(binding.ir_name.clone()),
                    ty: binding.ty.clone(),
                };
                Ok((
                    Place::Variable(binding.ir_name),
                    binding.ty,
                    binding.mutable && !binding.imported,
                    old,
                    span,
                ))
            }
            Expression::StaticMemberExpression(member) => {
                self.lower_field_place(&member.object, member.property.name.as_str(), span)
            }
            _ => Err(err(
                self.unit,
                span,
                "assignment target must be a variable or class field",
            )),
        }
    }

    fn lower_field_place<'a>(
        &mut self,
        object_ast: &Expression<'a>,
        field: &str,
        span: Span,
    ) -> Result<(Place, Type, bool, IrExpr, Span), CompileError> {
        if direct_identifier(object_ast).is_some_and(|name| self.classes.contains_key(name)) {
            return Err(err(
                self.unit,
                span,
                "static class field assignment is not supported",
            ));
        }
        let object = self.lower_expression(object_ast, None)?;
        let Type::Class(class_name) = object.ty.clone() else {
            return Err(err(
                self.unit,
                span,
                format!(
                    "field assignment requires a class instance, got {}",
                    type_name(&object.ty)
                ),
            ));
        };
        let class = self
            .classes
            .values()
            .find(|class| class.ir_name == class_name)
            .ok_or_else(|| err(self.unit, span, "unknown class instance"))?;
        let info = class
            .fields
            .get(field)
            .ok_or_else(|| err(self.unit, span, format!("class has no field `{field}`")))?;
        let value = IrExpr {
            kind: ExprKind::Field {
                object: Box::new(object.clone()),
                class: class_name.clone(),
                field: field.to_owned(),
            },
            ty: info.ty.clone(),
        };
        Ok((
            Place::Field {
                object,
                class: class_name,
                field: field.to_owned(),
            },
            info.ty.clone(),
            !info.readonly,
            value,
            span,
        ))
    }

    fn lower_update<'a>(
        &mut self,
        update: &oxc_ast::ast::UpdateExpression<'a>,
    ) -> Result<Stmt, CompileError> {
        let (place, ty, mutable, old, span) = match &update.argument {
            SimpleAssignmentTarget::AssignmentTargetIdentifier(identifier) => {
                let name = identifier.name.as_str();
                let binding = self
                    .scopes
                    .get(name)
                    .cloned()
                    .or_else(|| match self.imports.get(name) {
                        Some(ExportedSymbol::Variable(binding)) => Some(binding.clone()),
                        _ => None,
                    })
                    .ok_or_else(|| {
                        err(
                            self.unit,
                            identifier.span,
                            format!("unknown variable `{name}`"),
                        )
                    })?;
                let old = IrExpr {
                    kind: ExprKind::Variable(binding.ir_name.clone()),
                    ty: binding.ty.clone(),
                };
                (
                    Place::Variable(binding.ir_name),
                    binding.ty,
                    binding.mutable && !binding.imported,
                    old,
                    identifier.span,
                )
            }
            SimpleAssignmentTarget::StaticMemberExpression(member) => {
                self.lower_field_place(&member.object, member.property.name.as_str(), member.span)?
            }
            _ => {
                return Err(err(
                    self.unit,
                    update.span,
                    "increment and decrement require a variable or class field",
                ));
            }
        };
        if !mutable {
            return Err(err(
                self.unit,
                span,
                "cannot update an immutable binding or readonly field",
            ));
        }
        ensure_type(self.unit, update.span, &Type::Number, &ty)?;
        if matches!(place, Place::Field { .. }) && !field_receiver_is_simple(&old) {
            return Err(err(
                self.unit,
                update.span,
                "increment on a field with a computed receiver is not supported",
            ));
        }
        let one = IrExpr {
            kind: ExprKind::Number(1),
            ty: Type::Number,
        };
        let op = if update.operator == UpdateOperator::Increment {
            "+"
        } else {
            "-"
        };
        let value = IrExpr {
            kind: ExprKind::Binary {
                op: op.to_owned(),
                left: Box::new(old),
                right: Box::new(one),
            },
            ty: Type::Number,
        };
        Ok(Stmt::Assign { place, value })
    }

    fn lower_raw_call<'a>(
        &self,
        call: &oxc_ast::ast::CallExpression<'a>,
    ) -> Result<String, CompileError> {
        if call.arguments.len() != 1 {
            return Err(err(
                self.unit,
                call.span,
                "raw! expects exactly one literal string or template",
            ));
        }
        let expression = argument_expression(self.unit, &call.arguments[0])?;
        match expression {
            Expression::StringLiteral(literal) => {
                let text = literal.value.as_str();
                reject_nul(self.unit, literal.span, text)?;
                Ok(text.to_owned())
            }
            Expression::TemplateLiteral(template)
                if template.expressions.is_empty() && template.quasis.len() == 1 =>
            {
                let text = template.quasis[0].value.raw.as_str();
                reject_nul(self.unit, template.span, text)?;
                Ok(text.to_owned())
            }
            _ => Err(err(
                self.unit,
                expression.span(),
                "raw! requires a literal string or a template without substitutions",
            )),
        }
    }

    fn annotation_type(&self, ty: &TSType<'_>, span: Span) -> Result<Type, CompileError> {
        match ty {
            TSType::TSStringKeyword(_) => Ok(Type::String),
            TSType::TSNumberKeyword(_) => Ok(Type::Number),
            TSType::TSBooleanKeyword(_) => Ok(Type::Boolean),
            TSType::TSVoidKeyword(_) => Ok(Type::Void),
            TSType::TSParenthesizedType(parenthesized) => {
                self.annotation_type(&parenthesized.type_annotation, span)
            }
            TSType::TSTypeReference(reference) => {
                let name = match &reference.type_name {
                    TSTypeName::IdentifierReference(identifier) => identifier.name.as_str(),
                    _ => {
                        return Err(err(
                            self.unit,
                            span,
                            "qualified type names are not supported",
                        ));
                    }
                };
                self.classes
                    .get(name)
                    .map(|class| Type::Class(class.ir_name.clone()))
                    .ok_or_else(|| err(self.unit, span, format!("unknown class type `{name}`")))
            }
            _ => Err(err(
                self.unit,
                span,
                "only string, number, boolean, void, and class types are supported",
            )),
        }
    }

    fn next_var(&mut self, source_name: &str) -> String {
        let name = format!(
            "__tsh_v_{}_{}",
            shell_identifier(source_name),
            *self.variable_ids
        );
        *self.variable_ids += 1;
        name
    }
}

fn function_declaration<'p, 'a>(
    statement: &'p Statement<'a>,
) -> Option<(&'p OxcFunction<'a>, bool)> {
    match statement {
        Statement::FunctionDeclaration(function) => Some((function, false)),
        Statement::ExportDeclaration(export) => match &export.declaration {
            Declaration::FunctionDeclaration(function) => Some((function, true)),
            _ => None,
        },
        _ => None,
    }
}

fn class_declaration<'p, 'a>(statement: &'p Statement<'a>) -> Option<(&'p OxcClass<'a>, bool)> {
    match statement {
        Statement::ClassDeclaration(class) => Some((class, false)),
        Statement::ExportDeclaration(export) => match &export.declaration {
            Declaration::ClassDeclaration(class) => Some((class, true)),
            _ => None,
        },
        _ => None,
    }
}

fn variable_declaration<'p, 'a>(
    statement: &'p Statement<'a>,
) -> Option<&'p oxc_ast::ast::VariableDeclaration<'a>> {
    match statement {
        Statement::VariableDeclaration(declaration) => Some(declaration),
        Statement::ExportDeclaration(export) => match &export.declaration {
            Declaration::VariableDeclaration(declaration) => Some(declaration),
            _ => None,
        },
        _ => None,
    }
}

fn binding_pattern_name(pattern: &BindingPattern<'_>) -> Option<String> {
    match pattern {
        BindingPattern::BindingIdentifier(identifier) => Some(identifier.name.as_str().to_owned()),
        _ => None,
    }
}

fn property_name(key: &PropertyKey<'_>) -> Option<String> {
    match key {
        PropertyKey::StaticIdentifier(identifier) => Some(identifier.name.as_str().to_owned()),
        PropertyKey::StringLiteral(literal) => Some(literal.value.as_str().to_owned()),
        _ => None,
    }
}

fn direct_identifier<'a>(expression: &Expression<'a>) -> Option<&'a str> {
    match expression {
        Expression::Identifier(identifier) => Some(identifier.name.as_str()),
        _ => None,
    }
}

fn is_raw_macro(expression: &Expression<'_>) -> bool {
    matches!(expression, Expression::TSNonNullExpression(non_null) if direct_identifier(&non_null.expression) == Some("raw"))
}

fn command_for_name(name: &str) -> Option<ir::Command> {
    Some(match name {
        "echo" => ir::Command::Echo,
        "mkdir" => ir::Command::Mkdir,
        "rm" => ir::Command::Rm,
        "cp" => ir::Command::Cp,
        "mv" => ir::Command::Mv,
        "cd" => ir::Command::Cd,
        "run" => ir::Command::Run,
        "exit" => ir::Command::Exit,
        "chmod" => ir::Command::Chmod,
        _ => return None,
    })
}

fn reject_function_modifiers(
    unit: &SourceUnit,
    function: &OxcFunction<'_>,
) -> Result<(), CompileError> {
    if function.r#async {
        return Err(err(
            unit,
            function.span,
            "async functions are not supported",
        ));
    }
    if function.generator {
        return Err(err(
            unit,
            function.span,
            "generator functions are not supported",
        ));
    }
    if function.declare {
        return Err(err(
            unit,
            function.span,
            "declare functions are not supported",
        ));
    }
    if function.type_parameters.is_some() {
        return Err(err(
            unit,
            function.span,
            "generic functions are not supported",
        ));
    }
    if function.this_param.is_some() {
        return Err(err(
            unit,
            function.span,
            "explicit this parameters are not supported",
        ));
    }
    if function.params.rest.is_some() {
        return Err(err(
            unit,
            function.span,
            "rest parameters are not supported",
        ));
    }
    if function.body.is_none() {
        return Err(err(
            unit,
            function.span,
            "function declarations need an implementation",
        ));
    }
    Ok(())
}

fn constructor_assignments(function: &OxcFunction<'_>) -> HashSet<String> {
    let mut output = HashSet::new();
    let Some(body) = function.body.as_deref() else {
        return output;
    };
    for statement in &body.statements {
        let Statement::ExpressionStatement(expression_statement) = statement else {
            continue;
        };
        let Expression::AssignmentExpression(assignment) = &expression_statement.expression else {
            continue;
        };
        if assignment.operator != AssignmentOperator::Assign {
            continue;
        }
        let AssignmentTarget::StaticMemberExpression(member) = &assignment.left else {
            continue;
        };
        if !matches!(member.object, Expression::ThisExpression(_)) {
            continue;
        }
        output.insert(member.property.name.as_str().to_owned());
    }
    output
}

fn shell_identifier(source: &str) -> String {
    let mut result = String::new();
    for ch in source.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            result.push(ch);
        } else {
            result.push('_');
        }
    }
    if result.is_empty() {
        result.push_str("value");
    }
    if result.as_bytes()[0].is_ascii_digit() {
        result.insert(0, 'v');
    }
    result
}

fn source_fragment(unit: &SourceUnit, span: Span) -> String {
    let Some(text) = unit.source.get(span.start as usize..span.end as usize) else {
        return "syntax".to_owned();
    };
    let single = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if single.len() > 72 {
        format!("{}…", &single[..72])
    } else {
        single
    }
}

fn is_compiler_directive(text: &str) -> bool {
    let content = text
        .trim_start_matches('/')
        .trim_start_matches('*')
        .trim_end_matches('/')
        .trim_end_matches('*')
        .trim();
    content.starts_with("#if")
        || content.starts_with("#elif")
        || content.starts_with("#else")
        || content.starts_with("#endif")
        || content.starts_with("@cfg")
        || content.starts_with("@ts-")
        || content.starts_with("tsh:")
}

fn type_name(ty: &Type) -> String {
    match ty {
        Type::String => "string".to_owned(),
        Type::Number => "number".to_owned(),
        Type::Boolean => "boolean".to_owned(),
        Type::Void => "void".to_owned(),
        Type::Class(name) => format!("class {name}"),
    }
}

fn ensure_type(
    unit: &SourceUnit,
    span: Span,
    expected: &Type,
    actual: &Type,
) -> Result<(), CompileError> {
    if expected == actual {
        Ok(())
    } else {
        Err(err(
            unit,
            span,
            format!(
                "expected {}, found {}",
                type_name(expected),
                type_name(actual)
            ),
        ))
    }
}

fn ensure_scalar(unit: &SourceUnit, span: Span, ty: &Type) -> Result<(), CompileError> {
    if matches!(ty, Type::String | Type::Number | Type::Boolean) {
        Ok(())
    } else {
        Err(err(
            unit,
            span,
            format!("{} cannot be used as a string value", type_name(ty)),
        ))
    }
}

fn reject_nul(unit: &SourceUnit, span: Span, value: &str) -> Result<(), CompileError> {
    if value.contains('\0') {
        Err(err(
            unit,
            span,
            "NUL bytes cannot be represented in Bash values",
        ))
    } else {
        Ok(())
    }
}

fn argument_expression<'a>(
    unit: &SourceUnit,
    argument: &'a Argument<'a>,
) -> Result<&'a Expression<'a>, CompileError> {
    if matches!(argument, Argument::SpreadElement(_)) {
        return Err(err(
            unit,
            argument.span(),
            "spread arguments are not supported",
        ));
    }
    Ok(argument.to_expression())
}

fn parse_integer_magnitude(raw: &str) -> Option<u64> {
    let compact = raw.replace('_', "");
    if let Some(value) = compact
        .strip_prefix("0x")
        .or_else(|| compact.strip_prefix("0X"))
    {
        u64::from_str_radix(value, 16).ok()
    } else if let Some(value) = compact
        .strip_prefix("0o")
        .or_else(|| compact.strip_prefix("0O"))
    {
        u64::from_str_radix(value, 8).ok()
    } else if let Some(value) = compact
        .strip_prefix("0b")
        .or_else(|| compact.strip_prefix("0B"))
    {
        u64::from_str_radix(value, 2).ok()
    } else {
        compact.parse::<u64>().ok()
    }
}

fn parse_integer_literal(raw: &str) -> Option<i64> {
    let magnitude = parse_integer_magnitude(raw)?;
    i64::try_from(magnitude).ok()
}

fn parse_recursive_option(
    unit: &SourceUnit,
    object: &oxc_ast::ast::ObjectExpression<'_>,
) -> Result<bool, CompileError> {
    let mut recursive = false;
    let mut seen = false;
    for property in &object.properties {
        let ObjectPropertyKind::ObjectProperty(property) = property else {
            return Err(err(
                unit,
                property.span(),
                "spread command options are not supported",
            ));
        };
        if property.computed || property.method || property.shorthand {
            return Err(err(
                unit,
                property.span,
                "command options must be simple key-value properties",
            ));
        }
        let Some(name) = property_name(&property.key) else {
            return Err(err(
                unit,
                property.span,
                "command option names must be identifiers",
            ));
        };
        if name != "recursive" {
            return Err(err(
                unit,
                property.span,
                format!("unknown command option `{name}`"),
            ));
        }
        if seen {
            return Err(err(unit, property.span, "duplicate `recursive` option"));
        }
        let Expression::BooleanLiteral(value) = &property.value else {
            return Err(err(
                unit,
                property.span,
                "`recursive` option must be boolean",
            ));
        };
        recursive = value.value;
        seen = true;
    }
    Ok(recursive)
}

fn lower_global_statement(
    unit: &SourceUnit,
    globals: &[GlobalDef],
    indices: &HashMap<String, usize>,
    lowerer: &mut Lowerer<'_, '_, '_>,
    statement: &Statement<'_>,
    output: &mut Vec<Stmt>,
) -> Result<(), CompileError> {
    let declaration = variable_declaration(statement).ok_or_else(|| {
        err(
            unit,
            statement.span(),
            "invalid module variable declaration",
        )
    })?;
    lower_global_declaration(unit, globals, indices, lowerer, declaration, output)
}

fn lower_global_declaration(
    unit: &SourceUnit,
    globals: &[GlobalDef],
    indices: &HashMap<String, usize>,
    lowerer: &mut Lowerer<'_, '_, '_>,
    declaration: &oxc_ast::ast::VariableDeclaration<'_>,
    output: &mut Vec<Stmt>,
) -> Result<(), CompileError> {
    for declarator in &declaration.declarations {
        let name = binding_pattern_name(&declarator.id).ok_or_else(|| {
            err(
                unit,
                declarator.span,
                "variables must use simple identifiers",
            )
        })?;
        let index = *indices.get(&name).ok_or_else(|| {
            err(
                unit,
                declarator.span,
                format!("unknown module variable `{name}`"),
            )
        })?;
        let global = &globals[index];
        let value = global
            .initializer
            .clone()
            .ok_or_else(|| err(unit, global.span, "missing initializer"))?;
        output.push(Stmt::Let {
            name: global.binding.ir_name.clone(),
            value,
        });
        lowerer
            .scopes
            .current_mut()
            .insert(name, global.binding.clone());
    }
    Ok(())
}

fn field_receiver_is_simple(value: &IrExpr) -> bool {
    matches!(&value.kind, ExprKind::Field { object, .. } if matches!(object.kind, ExprKind::Variable(_)))
}

fn always_returns(statements: &[Stmt]) -> bool {
    statements.iter().any(|statement| match statement {
        Stmt::Return(_) => true,
        Stmt::Block(body) => always_returns(body),
        Stmt::If {
            then_body,
            else_body,
            ..
        } => !else_body.is_empty() && always_returns(then_body) && always_returns(else_body),
        _ => false,
    })
}

// Return-type inference threads the same module tables as lowering.
#[allow(clippy::too_many_arguments)]
fn infer_function_return<'p, 'a>(
    unit: &SourceUnit,
    function: &OxcFunction<'a>,
    params: &[ParamInfo],
    globals: &[GlobalDef],
    imports: &HashMap<String, ExportedSymbol>,
    functions: &[FunctionDef<'p, 'a>],
    classes: &[ClassDef<'p, 'a>],
    current_class: Option<&str>,
) -> Option<Type> {
    let body = function.body.as_deref()?;
    let mut locals = HashMap::new();
    for param in params {
        locals.insert(param.source_name.clone(), param.ty.clone());
    }
    let mut returns = Vec::new();
    for statement in &body.statements {
        collect_return_expressions(statement, &mut returns);
    }
    if returns.is_empty() {
        return Some(Type::Void);
    }
    let mut had_void = false;
    let mut inferred = Vec::new();
    for return_value in returns {
        if let Some(expression) = return_value {
            if let Ok(Some(ty)) = infer_expr_type_with_locals(
                unit,
                expression,
                globals,
                imports,
                functions,
                classes,
                &locals,
                current_class,
            ) {
                inferred.push(ty);
            }
        } else {
            had_void = true;
        }
    }
    if had_void && inferred.is_empty() {
        return Some(Type::Void);
    }
    if let Some(first) = inferred.first()
        && inferred.iter().all(|ty| ty == first)
    {
        return Some(first.clone());
    }
    None
}

fn collect_return_expressions<'s, 'a>(
    statement: &'s Statement<'a>,
    output: &mut Vec<Option<&'s Expression<'a>>>,
) {
    match statement {
        Statement::ReturnStatement(return_statement) => {
            output.push(return_statement.argument.as_ref())
        }
        Statement::BlockStatement(block) => {
            for nested in &block.body {
                collect_return_expressions(nested, output);
            }
        }
        Statement::IfStatement(if_statement) => {
            collect_return_expressions(&if_statement.consequent, output);
            if let Some(alternate) = &if_statement.alternate {
                collect_return_expressions(alternate, output);
            }
        }
        Statement::WhileStatement(loop_statement) => {
            collect_return_expressions(&loop_statement.body, output)
        }
        Statement::ForStatement(loop_statement) => {
            collect_return_expressions(&loop_statement.body, output)
        }
        _ => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn infer_expr_type<'a>(
    unit: &SourceUnit,
    expression: &Expression<'a>,
    globals: &[GlobalDef],
    imports: &HashMap<String, ExportedSymbol>,
    functions: &[FunctionDef<'_, 'a>],
    classes: &[ClassDef<'_, 'a>],
    current_class: Option<&str>,
    this_type: Option<&Type>,
) -> Result<Option<Type>, CompileError> {
    infer_expr_type_with_locals(
        unit,
        expression,
        globals,
        imports,
        functions,
        classes,
        &HashMap::new(),
        current_class,
    )
    .map(|ty| ty.or_else(|| this_type.cloned()))
}

#[allow(clippy::too_many_arguments)]
fn infer_expr_type_with_locals<'a>(
    unit: &SourceUnit,
    expression: &Expression<'a>,
    globals: &[GlobalDef],
    imports: &HashMap<String, ExportedSymbol>,
    functions: &[FunctionDef<'_, 'a>],
    classes: &[ClassDef<'_, 'a>],
    locals: &HashMap<String, Type>,
    current_class: Option<&str>,
) -> Result<Option<Type>, CompileError> {
    let infer = |expression: &Expression<'a>| {
        infer_expr_type_with_locals(
            unit,
            expression,
            globals,
            imports,
            functions,
            classes,
            locals,
            current_class,
        )
    };
    let result = match expression {
        Expression::StringLiteral(literal) => Some({
            reject_nul(unit, literal.span, literal.value.as_str())?;
            Type::String
        }),
        Expression::NumericLiteral(literal) => literal
            .raw
            .as_ref()
            .and_then(|raw| parse_integer_literal(raw.as_str()))
            .map(|_| Type::Number),
        Expression::BooleanLiteral(_) => Some(Type::Boolean),
        Expression::Identifier(identifier) => {
            let name = identifier.name.as_str();
            locals
                .get(name)
                .cloned()
                .or_else(|| {
                    globals
                        .iter()
                        .find(|global| global.source_name == name)
                        .map(|global| global.binding.ty.clone())
                })
                .or_else(|| match imports.get(name) {
                    Some(ExportedSymbol::Variable(binding)) => Some(binding.ty.clone()),
                    _ => None,
                })
        }
        Expression::TemplateLiteral(template) => Some({
            for expression in &template.expressions {
                if let Some(ty) = infer(expression)? {
                    ensure_scalar(unit, expression.span(), &ty)?;
                }
            }
            Type::String
        }),
        Expression::ParenthesizedExpression(parenthesized) => infer(&parenthesized.expression)?,
        Expression::ThisExpression(_) => current_class.map(|name| Type::Class(name.to_owned())),
        Expression::UnaryExpression(unary) => {
            let ty = infer(&unary.argument)?;
            match unary.operator {
                UnaryOperator::LogicalNot if ty == Some(Type::Boolean) => Some(Type::Boolean),
                UnaryOperator::UnaryPlus | UnaryOperator::UnaryNegation
                    if ty == Some(Type::Number) =>
                {
                    Some(Type::Number)
                }
                _ => None,
            }
        }
        Expression::BinaryExpression(binary) => {
            let left = infer(&binary.left)?;
            let right = infer(&binary.right)?;
            match (binary.operator, left, right) {
                (BinaryOperator::Addition, Some(Type::String), Some(Type::String)) => {
                    Some(Type::String)
                }
                (
                    BinaryOperator::Addition
                    | BinaryOperator::Subtraction
                    | BinaryOperator::Multiplication
                    | BinaryOperator::Division
                    | BinaryOperator::Remainder,
                    Some(Type::Number),
                    Some(Type::Number),
                ) => Some(Type::Number),
                (
                    BinaryOperator::Equality
                    | BinaryOperator::StrictEquality
                    | BinaryOperator::Inequality
                    | BinaryOperator::StrictInequality,
                    Some(a),
                    Some(b),
                ) if a == b => Some(Type::Boolean),
                (
                    BinaryOperator::LessThan
                    | BinaryOperator::LessEqualThan
                    | BinaryOperator::GreaterThan
                    | BinaryOperator::GreaterEqualThan,
                    Some(a),
                    Some(b),
                ) if a == b && matches!(a, Type::Number | Type::String) => Some(Type::Boolean),
                _ => None,
            }
        }
        Expression::LogicalExpression(logical) => {
            if logical.operator == LogicalOperator::Coalesce {
                None
            } else if infer(&logical.left)? == Some(Type::Boolean)
                && infer(&logical.right)? == Some(Type::Boolean)
            {
                Some(Type::Boolean)
            } else {
                None
            }
        }
        Expression::TSNonNullExpression(non_null) => infer(&non_null.expression)?,
        Expression::TSAsExpression(assertion) => infer(&assertion.expression)?,
        Expression::TSSatisfiesExpression(assertion) => infer(&assertion.expression)?,
        Expression::NewExpression(new_expression) => direct_identifier(&new_expression.callee)
            .and_then(|name| {
                lookup_class_signature(name, imports, classes)
                    .map(|class| Type::Class(class.ir_name.clone()))
            }),
        Expression::StaticMemberExpression(member) => {
            let object_ty = if let Some(name) = direct_identifier(&member.object) {
                if let Some(class) = lookup_class_signature(name, imports, classes) {
                    Some(Type::Class(class.ir_name.clone()))
                } else {
                    infer(&member.object)?
                }
            } else {
                infer(&member.object)?
            };
            match object_ty {
                Some(Type::Class(class_name)) => lookup_class_by_ir(&class_name, imports, classes)
                    .and_then(|class| {
                        class
                            .fields
                            .get(member.property.name.as_str())
                            .map(|field| field.ty.clone())
                    }),
                _ => None,
            }
        }
        Expression::CallExpression(call) => {
            if let Some(name) = direct_identifier(&call.callee) {
                if name == "env" {
                    Some(Type::String)
                } else {
                    lookup_function_signature(name, imports, functions)
                        .and_then(|function| function.return_type.clone())
                }
            } else if let Expression::StaticMemberExpression(member) = &call.callee {
                let method_name = member.property.name.as_str();
                if let Some(class_name) = direct_identifier(&member.object)
                    && lookup_class_signature(class_name, imports, classes).is_some()
                {
                    return Ok(lookup_class_signature(class_name, imports, classes)
                        .and_then(|class| class.methods.get(method_name).cloned())
                        .filter(|method| method.is_static)
                        .and_then(|method| method.function.return_type));
                }
                match infer(&member.object)? {
                    Some(Type::Class(class_name)) => {
                        lookup_class_by_ir(&class_name, imports, classes)
                            .and_then(|class| class.methods.get(method_name).cloned())
                            .filter(|method| !method.is_static)
                            .and_then(|method| method.function.return_type)
                    }
                    _ => None,
                }
            } else {
                None
            }
        }
        _ => None,
    };
    Ok(result)
}

fn lookup_function_signature<'a>(
    name: &str,
    imports: &HashMap<String, ExportedSymbol>,
    functions: &[FunctionDef<'_, 'a>],
) -> Option<FunctionSignature> {
    functions
        .iter()
        .find(|function| function.name == name)
        .map(|function| function.signature.clone())
        .or_else(|| match imports.get(name) {
            Some(ExportedSymbol::Function(signature)) => Some(signature.clone()),
            _ => None,
        })
}

fn lookup_class_signature<'a>(
    name: &str,
    imports: &HashMap<String, ExportedSymbol>,
    classes: &[ClassDef<'_, 'a>],
) -> Option<ClassSignature> {
    classes
        .iter()
        .find(|class| class.source_name == name)
        .map(|class| class.signature.clone())
        .or_else(|| match imports.get(name) {
            Some(ExportedSymbol::Class(signature)) => Some(signature.clone()),
            _ => None,
        })
}

fn lookup_class_by_ir<'a>(
    ir_name: &str,
    imports: &HashMap<String, ExportedSymbol>,
    classes: &[ClassDef<'_, 'a>],
) -> Option<ClassSignature> {
    classes
        .iter()
        .find(|class| class.signature.ir_name == ir_name)
        .map(|class| class.signature.clone())
        .or_else(|| {
            imports.values().find_map(|symbol| match symbol {
                ExportedSymbol::Class(signature) if signature.ir_name == ir_name => {
                    Some(signature.clone())
                }
                _ => None,
            })
        })
}
