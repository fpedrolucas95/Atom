//! JavaScript AST node types.
//!
//! A compact tree the recursive-descent parser produces and the tree-walking
//! interpreter consumes. Function bodies are `Rc`-shared so closures clone
//! cheaply.

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

#[derive(Clone)]
pub enum Stmt {
    Expr(Expr),
    /// `var`/`let`/`const` — one entry per declarator.
    VarDecl {
        function_scoped: bool,
        decls: Vec<(String, Option<Expr>)>,
    },
    FuncDecl(Rc<FnDef>),
    Block(Vec<Stmt>),
    If {
        cond: Expr,
        then: Box<Stmt>,
        els: Option<Box<Stmt>>,
    },
    While {
        cond: Expr,
        body: Box<Stmt>,
    },
    DoWhile {
        body: Box<Stmt>,
        cond: Expr,
    },
    For {
        init: Option<Box<Stmt>>,
        cond: Option<Expr>,
        step: Option<Expr>,
        body: Box<Stmt>,
    },
    /// `for (v in o)` / `for (v of o)`.
    ForIn {
        decl: bool,
        var: String,
        of: bool,
        obj: Expr,
        body: Box<Stmt>,
    },
    Return(Option<Expr>),
    Break,
    Continue,
    Throw(Expr),
    Try {
        body: Vec<Stmt>,
        catch_var: Option<String>,
        catch: Option<Vec<Stmt>>,
        finally: Option<Vec<Stmt>>,
    },
    Switch {
        disc: Expr,
        cases: Vec<(Option<Expr>, Vec<Stmt>)>,
    },
    Empty,
}

#[derive(Clone)]
pub enum Expr {
    Num(f64),
    Str(Rc<str>),
    Bool(bool),
    Null,
    Undefined,
    Ident(String),
    This,
    Array(Vec<Expr>),
    Object(Vec<(String, Expr)>),
    Func(Rc<FnDef>),
    Template(Vec<TplPart>),
    /// Regex literals parse to an inert object so scripts that merely build
    /// them don't abort; matching is not implemented.
    Regex(Rc<str>),
    Unary(UnOp, Box<Expr>),
    /// `++x` / `x--` — target must be assignable.
    Update {
        inc: bool,
        prefix: bool,
        target: Box<Expr>,
    },
    Binary(BinOp, Box<Expr>, Box<Expr>),
    Logical(LogOp, Box<Expr>, Box<Expr>),
    Cond(Box<Expr>, Box<Expr>, Box<Expr>),
    /// `target op= value`; `op` is `None` for plain `=`.
    Assign {
        op: Option<BinOp>,
        target: Box<Expr>,
        value: Box<Expr>,
    },
    Call {
        callee: Box<Expr>,
        args: Vec<Expr>,
    },
    New {
        callee: Box<Expr>,
        args: Vec<Expr>,
    },
    Member {
        obj: Box<Expr>,
        prop: MemberProp,
        /// `?.` — short-circuit to `undefined` on null/undefined base.
        optional: bool,
    },
    Sequence(Vec<Expr>),
}

#[derive(Clone)]
pub enum MemberProp {
    Dot(String),
    Index(Box<Expr>),
}

#[derive(Clone)]
pub enum TplPart {
    Str(String),
    Expr(Box<Expr>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Plus,
    Not,
    BitNot,
    TypeOf,
    Void,
    Delete,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Pow,
    Eq,
    Ne,
    StrictEq,
    StrictNe,
    Lt,
    Gt,
    Le,
    Ge,
    Shl,
    Shr,
    UShr,
    BitAnd,
    BitOr,
    BitXor,
    In,
    InstanceOf,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LogOp {
    And,
    Or,
    /// `??`
    Nullish,
}

/// A function definition: shared by declarations, expressions, and arrows.
pub struct FnDef {
    pub name: Option<String>,
    pub params: Vec<Param>,
    pub body: FnBody,
    pub is_arrow: bool,
}

pub struct Param {
    pub name: String,
    pub default: Option<Expr>,
}

pub enum FnBody {
    Block(Vec<Stmt>),
    /// Arrow shorthand `x => expr`.
    Expr(Box<Expr>),
}
