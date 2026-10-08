//! Target-independent, typed language semantics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Type {
    String,
    Number,
    Boolean,
    Void,
    Class(String),
}
#[derive(Debug, Clone)]
pub struct Expr {
    pub kind: ExprKind,
    pub ty: Type,
}
#[derive(Debug, Clone)]
pub enum ExprKind {
    String(String),
    Number(i64),
    Boolean(bool),
    Variable(String),
    Template(Vec<Expr>),
    Binary {
        op: String,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Unary {
        op: String,
        value: Box<Expr>,
    },
    Call {
        function: String,
        args: Vec<Expr>,
    },
    New {
        class: String,
        args: Vec<Expr>,
    },
    Field {
        object: Box<Expr>,
        class: String,
        field: String,
    },
    Method {
        object: Option<Box<Expr>>,
        class: String,
        method: String,
        args: Vec<Expr>,
    },
    Env(Box<Expr>),
}
#[derive(Debug, Clone)]
pub enum Place {
    Variable(String),
    Field {
        object: Expr,
        class: String,
        field: String,
    },
}
#[derive(Debug, Clone)]
pub enum Stmt {
    Let {
        name: String,
        value: Expr,
    },
    Assign {
        place: Place,
        value: Expr,
    },
    Expr(Expr),
    If {
        condition: Expr,
        then_body: Vec<Stmt>,
        else_body: Vec<Stmt>,
    },
    While {
        condition: Expr,
        body: Vec<Stmt>,
    },
    For {
        init: Vec<Stmt>,
        condition: Option<Expr>,
        update: Vec<Stmt>,
        body: Vec<Stmt>,
    },
    Return(Option<Expr>),
    Break,
    Continue,
    Command {
        kind: Command,
        args: Vec<Expr>,
        recursive: bool,
    },
    Raw(String),
    Comment(String),
    Block(Vec<Stmt>),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Echo,
    Mkdir,
    Rm,
    Cp,
    Mv,
    Cd,
    Run,
    Exit,
    Chmod,
}
#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub ty: Type,
}
#[derive(Debug, Clone)]
pub struct Function {
    pub name: String,
    pub params: Vec<Param>,
    pub return_type: Type,
    pub body: Vec<Stmt>,
}
#[derive(Debug, Clone)]
pub struct Field {
    pub name: String,
    pub ty: Type,
    pub initial: Option<Expr>,
}
#[derive(Debug, Clone)]
pub struct Method {
    pub function: Function,
    pub is_static: bool,
}
#[derive(Debug, Clone)]
pub struct Class {
    pub name: String,
    pub fields: Vec<Field>,
    pub constructor: Function,
    pub methods: Vec<Method>,
}
#[derive(Debug, Clone, Default)]
pub struct Program {
    pub functions: Vec<Function>,
    pub classes: Vec<Class>,
    pub body: Vec<Stmt>,
}
