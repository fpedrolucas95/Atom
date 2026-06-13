//! JavaScript parser: tokens → AST.
//!
//! Recursive descent for statements, Pratt-style precedence climbing for
//! expressions. Implements the parts of the grammar real page scripts use:
//! functions (declarations, expressions, arrows with lookahead disambiguation
//! from parenthesized expressions), the full operator set including `**`,
//! `??` and `?.`, object/array literals with shorthand and method properties,
//! template literals, `switch`, `try/catch/finally`, and automatic semicolon
//! insertion (newline-, `}`- and EOF-triggered, plus the restricted `return`
//! production). Unsupported syntax (classes, destructuring, generators,
//! modules) fails the parse; the page still renders, with the error reported
//! on the console.

use alloc::boxed::Box;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use super::ast::*;
use super::lexer::{lex, RawTplPart, Tok, Token};

/// Maximum syntactic nesting, bounding parser recursion on the 512 KiB stack.
const MAX_PARSE_DEPTH: u32 = 120;

pub fn parse_program(src: &str) -> Result<Vec<Stmt>, String> {
    let toks = lex(src)?;
    let mut p = Parser {
        toks,
        pos: 0,
        depth: 0,
    };
    let mut stmts = Vec::new();
    while !p.at_eof() {
        stmts.push(p.statement()?);
    }
    Ok(stmts)
}

struct Parser {
    toks: Vec<Token>,
    pos: usize,
    depth: u32,
}

type PResult<T> = Result<T, String>;

impl Parser {
    // ── Token plumbing ──────────────────────────────────────────────────────

    fn cur(&self) -> &Tok {
        &self.toks[self.pos.min(self.toks.len() - 1)].kind
    }

    fn nl_before(&self) -> bool {
        self.toks[self.pos.min(self.toks.len() - 1)].nl_before
    }

    fn at_eof(&self) -> bool {
        matches!(self.cur(), Tok::Eof)
    }

    fn advance(&mut self) {
        if self.pos < self.toks.len() - 1 {
            self.pos += 1;
        }
    }

    fn is_punct(&self, p: &str) -> bool {
        matches!(self.cur(), Tok::Punct(q) if *q == p)
    }

    fn eat_punct(&mut self, p: &str) -> bool {
        if self.is_punct(p) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn expect_punct(&mut self, p: &str) -> PResult<()> {
        if self.eat_punct(p) {
            Ok(())
        } else {
            Err(format!("expected `{p}`"))
        }
    }

    fn is_kw(&self, kw: &str) -> bool {
        matches!(self.cur(), Tok::Ident(n) if n == kw)
    }

    fn eat_kw(&mut self, kw: &str) -> bool {
        if self.is_kw(kw) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn ident(&mut self) -> PResult<String> {
        match self.cur() {
            Tok::Ident(n) => {
                let n = n.clone();
                self.advance();
                Ok(n)
            }
            _ => Err(String::from("expected identifier")),
        }
    }

    /// Consume a statement terminator, applying ASI.
    fn semicolon(&mut self) -> PResult<()> {
        if self.eat_punct(";") {
            return Ok(());
        }
        if self.is_punct("}") || self.at_eof() || self.nl_before() {
            return Ok(()); // inserted
        }
        Err(String::from("expected `;`"))
    }

    fn enter(&mut self) -> PResult<()> {
        self.depth += 1;
        if self.depth > MAX_PARSE_DEPTH {
            return Err(String::from("nesting too deep"));
        }
        Ok(())
    }

    fn leave(&mut self) {
        self.depth -= 1;
    }

    // ── Statements ──────────────────────────────────────────────────────────

    fn statement(&mut self) -> PResult<Stmt> {
        self.enter()?;
        let r = self.statement_inner();
        self.leave();
        r
    }

    fn statement_inner(&mut self) -> PResult<Stmt> {
        if self.eat_punct(";") {
            return Ok(Stmt::Empty);
        }
        if self.is_punct("{") {
            return Ok(Stmt::Block(self.block()?));
        }
        match self.cur() {
            Tok::Ident(name) => match name.as_str() {
                "var" | "let" | "const" => return self.var_decl(),
                "function" => {
                    self.advance();
                    let def = self.function_rest(true, false)?;
                    return Ok(Stmt::FuncDecl(Rc::new(def)));
                }
                "if" => return self.if_stmt(),
                "while" => return self.while_stmt(),
                "do" => return self.do_while(),
                "for" => return self.for_stmt(),
                "return" => {
                    self.advance();
                    // Restricted production: a newline ends the statement.
                    let value = if self.is_punct(";")
                        || self.is_punct("}")
                        || self.at_eof()
                        || self.nl_before()
                    {
                        None
                    } else {
                        Some(self.expression()?)
                    };
                    self.semicolon()?;
                    return Ok(Stmt::Return(value));
                }
                "break" => {
                    self.advance();
                    self.semicolon()?;
                    return Ok(Stmt::Break);
                }
                "continue" => {
                    self.advance();
                    self.semicolon()?;
                    return Ok(Stmt::Continue);
                }
                "throw" => {
                    self.advance();
                    let e = self.expression()?;
                    self.semicolon()?;
                    return Ok(Stmt::Throw(e));
                }
                "try" => return self.try_stmt(),
                "switch" => return self.switch_stmt(),
                "class" => return Err(String::from("`class` is not supported")),
                _ => {}
            },
            _ => {}
        }
        let e = self.expression()?;
        self.semicolon()?;
        Ok(Stmt::Expr(e))
    }

    fn block(&mut self) -> PResult<Vec<Stmt>> {
        self.expect_punct("{")?;
        let mut stmts = Vec::new();
        while !self.is_punct("}") && !self.at_eof() {
            stmts.push(self.statement()?);
        }
        self.expect_punct("}")?;
        Ok(stmts)
    }

    fn var_decl(&mut self) -> PResult<Stmt> {
        let kw = self.ident()?;
        let function_scoped = kw == "var";
        let mut decls = Vec::new();
        loop {
            let name = self.ident()?;
            let init = if self.eat_punct("=") {
                Some(self.assignment()?)
            } else {
                None
            };
            decls.push((name, init));
            if !self.eat_punct(",") {
                break;
            }
        }
        self.semicolon()?;
        Ok(Stmt::VarDecl {
            function_scoped,
            decls,
        })
    }

    fn if_stmt(&mut self) -> PResult<Stmt> {
        self.advance(); // if
        self.expect_punct("(")?;
        let cond = self.expression()?;
        self.expect_punct(")")?;
        let then = Box::new(self.statement()?);
        let els = if self.eat_kw("else") {
            Some(Box::new(self.statement()?))
        } else {
            None
        };
        Ok(Stmt::If { cond, then, els })
    }

    fn while_stmt(&mut self) -> PResult<Stmt> {
        self.advance();
        self.expect_punct("(")?;
        let cond = self.expression()?;
        self.expect_punct(")")?;
        let body = Box::new(self.statement()?);
        Ok(Stmt::While { cond, body })
    }

    fn do_while(&mut self) -> PResult<Stmt> {
        self.advance();
        let body = Box::new(self.statement()?);
        if !self.eat_kw("while") {
            return Err(String::from("expected `while`"));
        }
        self.expect_punct("(")?;
        let cond = self.expression()?;
        self.expect_punct(")")?;
        let _ = self.eat_punct(";");
        Ok(Stmt::DoWhile { body, cond })
    }

    fn for_stmt(&mut self) -> PResult<Stmt> {
        self.advance(); // for
        self.expect_punct("(")?;

        // for (… in/of …)? Look for the declaration / lone identifier form.
        let decl_kw = match self.cur() {
            Tok::Ident(n) if n == "var" || n == "let" || n == "const" => true,
            _ => false,
        };
        let save = self.pos;
        if decl_kw || matches!(self.cur(), Tok::Ident(_)) {
            if decl_kw {
                self.advance();
            }
            if let Tok::Ident(var) = self.cur() {
                let var = var.clone();
                let next_is = |p: &Parser| p.is_kw("in") || p.is_kw("of");
                self.advance();
                if next_is(self) {
                    let of = self.eat_kw("of");
                    if !of {
                        self.advance(); // `in`
                    }
                    let obj = self.expression()?;
                    self.expect_punct(")")?;
                    let body = Box::new(self.statement()?);
                    return Ok(Stmt::ForIn {
                        decl: decl_kw,
                        var,
                        of,
                        obj,
                        body,
                    });
                }
            }
            self.pos = save;
        }

        // Classic three-clause form.
        let init = if self.eat_punct(";") {
            None
        } else if decl_kw {
            Some(Box::new(self.var_decl()?)) // consumes its own `;` via ASI
        } else {
            let e = self.expression()?;
            self.expect_punct(";")?;
            Some(Box::new(Stmt::Expr(e)))
        };
        let cond = if self.is_punct(";") {
            None
        } else {
            Some(self.expression()?)
        };
        self.expect_punct(";")?;
        let step = if self.is_punct(")") {
            None
        } else {
            Some(self.expression()?)
        };
        self.expect_punct(")")?;
        let body = Box::new(self.statement()?);
        Ok(Stmt::For {
            init,
            cond,
            step,
            body,
        })
    }

    fn try_stmt(&mut self) -> PResult<Stmt> {
        self.advance(); // try
        let body = self.block()?;
        let mut catch_var = None;
        let mut catch = None;
        if self.eat_kw("catch") {
            if self.eat_punct("(") {
                catch_var = Some(self.ident()?);
                self.expect_punct(")")?;
            }
            catch = Some(self.block()?);
        }
        let finally = if self.eat_kw("finally") {
            Some(self.block()?)
        } else {
            None
        };
        if catch.is_none() && finally.is_none() {
            return Err(String::from("`try` without catch or finally"));
        }
        Ok(Stmt::Try {
            body,
            catch_var,
            catch,
            finally,
        })
    }

    fn switch_stmt(&mut self) -> PResult<Stmt> {
        self.advance(); // switch
        self.expect_punct("(")?;
        let disc = self.expression()?;
        self.expect_punct(")")?;
        self.expect_punct("{")?;
        let mut cases = Vec::new();
        while !self.is_punct("}") && !self.at_eof() {
            let test = if self.eat_kw("case") {
                let e = self.expression()?;
                Some(e)
            } else if self.eat_kw("default") {
                None
            } else {
                return Err(String::from("expected `case` or `default`"));
            };
            self.expect_punct(":")?;
            let mut body = Vec::new();
            while !self.is_punct("}")
                && !self.is_kw("case")
                && !self.is_kw("default")
                && !self.at_eof()
            {
                body.push(self.statement()?);
            }
            cases.push((test, body));
        }
        self.expect_punct("}")?;
        Ok(Stmt::Switch { disc, cases })
    }

    /// Parse the rest of a `function` (after the keyword): name?, params, body.
    fn function_rest(&mut self, named: bool, is_arrow: bool) -> PResult<FnDef> {
        let name = if named && matches!(self.cur(), Tok::Ident(_)) && !self.is_punct("(") {
            Some(self.ident()?)
        } else {
            None
        };
        let params = self.param_list()?;
        let body = FnBody::Block(self.block()?);
        Ok(FnDef {
            name,
            params,
            body,
            is_arrow,
        })
    }

    fn param_list(&mut self) -> PResult<Vec<Param>> {
        self.expect_punct("(")?;
        let mut params = Vec::new();
        while !self.is_punct(")") {
            let name = self.ident()?;
            let default = if self.eat_punct("=") {
                Some(self.assignment()?)
            } else {
                None
            };
            params.push(Param { name, default });
            if !self.eat_punct(",") {
                break;
            }
        }
        self.expect_punct(")")?;
        Ok(params)
    }

    // ── Expressions ─────────────────────────────────────────────────────────

    fn expression(&mut self) -> PResult<Expr> {
        let first = self.assignment()?;
        if !self.is_punct(",") {
            return Ok(first);
        }
        let mut seq = alloc::vec![first];
        while self.eat_punct(",") {
            seq.push(self.assignment()?);
        }
        Ok(Expr::Sequence(seq))
    }

    fn assignment(&mut self) -> PResult<Expr> {
        self.enter()?;
        let r = self.assignment_inner();
        self.leave();
        r
    }

    fn assignment_inner(&mut self) -> PResult<Expr> {
        // Arrow with a single bare parameter: `x => …`.
        if let Tok::Ident(name) = self.cur() {
            if !is_reserved(name) && matches!(self.peek_kind(1), Tok::Punct("=>")) {
                let name = name.clone();
                self.advance();
                self.advance();
                return self.arrow_body(alloc::vec![Param {
                    name,
                    default: None
                }]);
            }
        }
        // Arrow with a parenthesized list: scan ahead for `) =>`.
        if self.is_punct("(") {
            if let Some(close) = self.matching_paren(self.pos) {
                if matches!(self.toks[close + 1].kind, Tok::Punct("=>")) {
                    let params = self.param_list()?;
                    self.expect_punct("=>")?;
                    return self.arrow_body(params);
                }
            }
        }

        let target = self.conditional()?;
        let op: Option<Option<BinOp>> = match self.cur() {
            Tok::Punct("=") => Some(None),
            Tok::Punct("+=") => Some(Some(BinOp::Add)),
            Tok::Punct("-=") => Some(Some(BinOp::Sub)),
            Tok::Punct("*=") => Some(Some(BinOp::Mul)),
            Tok::Punct("/=") => Some(Some(BinOp::Div)),
            Tok::Punct("%=") => Some(Some(BinOp::Rem)),
            Tok::Punct("**=") => Some(Some(BinOp::Pow)),
            Tok::Punct("&=") => Some(Some(BinOp::BitAnd)),
            Tok::Punct("|=") => Some(Some(BinOp::BitOr)),
            Tok::Punct("^=") => Some(Some(BinOp::BitXor)),
            Tok::Punct("<<=") => Some(Some(BinOp::Shl)),
            Tok::Punct(">>=") => Some(Some(BinOp::Shr)),
            Tok::Punct(">>>=") => Some(Some(BinOp::UShr)),
            _ => None,
        };
        if let Some(op) = op {
            if !is_assign_target(&target) {
                return Err(String::from("invalid assignment target"));
            }
            self.advance();
            let value = Box::new(self.assignment()?);
            return Ok(Expr::Assign {
                op,
                target: Box::new(target),
                value,
            });
        }
        Ok(target)
    }

    fn arrow_body(&mut self, params: Vec<Param>) -> PResult<Expr> {
        let body = if self.is_punct("{") {
            FnBody::Block(self.block()?)
        } else {
            FnBody::Expr(Box::new(self.assignment()?))
        };
        Ok(Expr::Func(Rc::new(FnDef {
            name: None,
            params,
            body,
            is_arrow: true,
        })))
    }

    /// Index of the `)` matching the `(` at token index `open`.
    fn matching_paren(&self, open: usize) -> Option<usize> {
        let mut depth = 0i32;
        for (off, t) in self.toks[open..].iter().enumerate() {
            match t.kind {
                Tok::Punct("(") => depth += 1,
                Tok::Punct(")") => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(open + off);
                    }
                }
                Tok::Eof => return None,
                _ => {}
            }
        }
        None
    }

    fn peek_kind(&self, ahead: usize) -> &Tok {
        &self.toks[(self.pos + ahead).min(self.toks.len() - 1)].kind
    }

    fn conditional(&mut self) -> PResult<Expr> {
        let cond = self.binary(0)?;
        if self.eat_punct("?") {
            let then = self.assignment()?;
            self.expect_punct(":")?;
            let els = self.assignment()?;
            return Ok(Expr::Cond(Box::new(cond), Box::new(then), Box::new(els)));
        }
        Ok(cond)
    }

    /// Precedence-climbing binary expression parser.
    fn binary(&mut self, min_prec: u8) -> PResult<Expr> {
        self.enter()?;
        let r = self.binary_inner(min_prec);
        self.leave();
        r
    }

    fn binary_inner(&mut self, min_prec: u8) -> PResult<Expr> {
        let mut left = self.unary()?;
        loop {
            let Some((prec, kind)) = self.binop_here() else {
                break;
            };
            if prec < min_prec {
                break;
            }
            self.advance();
            match kind {
                OpKind::Logical(op) => {
                    let right = self.binary(prec + 1)?;
                    left = Expr::Logical(op, Box::new(left), Box::new(right));
                }
                OpKind::Bin(op) => {
                    // `**` is right-associative; everything else left.
                    let next = if op == BinOp::Pow { prec } else { prec + 1 };
                    let right = self.binary(next)?;
                    left = Expr::Binary(op, Box::new(left), Box::new(right));
                }
            }
        }
        Ok(left)
    }

    fn binop_here(&self) -> Option<(u8, OpKind)> {
        let (prec, kind) = match self.cur() {
            Tok::Punct("??") => (1, OpKind::Logical(LogOp::Nullish)),
            Tok::Punct("||") => (1, OpKind::Logical(LogOp::Or)),
            Tok::Punct("&&") => (2, OpKind::Logical(LogOp::And)),
            Tok::Punct("|") => (3, OpKind::Bin(BinOp::BitOr)),
            Tok::Punct("^") => (4, OpKind::Bin(BinOp::BitXor)),
            Tok::Punct("&") => (5, OpKind::Bin(BinOp::BitAnd)),
            Tok::Punct("==") => (6, OpKind::Bin(BinOp::Eq)),
            Tok::Punct("!=") => (6, OpKind::Bin(BinOp::Ne)),
            Tok::Punct("===") => (6, OpKind::Bin(BinOp::StrictEq)),
            Tok::Punct("!==") => (6, OpKind::Bin(BinOp::StrictNe)),
            Tok::Punct("<") => (7, OpKind::Bin(BinOp::Lt)),
            Tok::Punct(">") => (7, OpKind::Bin(BinOp::Gt)),
            Tok::Punct("<=") => (7, OpKind::Bin(BinOp::Le)),
            Tok::Punct(">=") => (7, OpKind::Bin(BinOp::Ge)),
            Tok::Ident(n) if n == "in" => (7, OpKind::Bin(BinOp::In)),
            Tok::Ident(n) if n == "instanceof" => (7, OpKind::Bin(BinOp::InstanceOf)),
            Tok::Punct("<<") => (8, OpKind::Bin(BinOp::Shl)),
            Tok::Punct(">>") => (8, OpKind::Bin(BinOp::Shr)),
            Tok::Punct(">>>") => (8, OpKind::Bin(BinOp::UShr)),
            Tok::Punct("+") => (9, OpKind::Bin(BinOp::Add)),
            Tok::Punct("-") => (9, OpKind::Bin(BinOp::Sub)),
            Tok::Punct("*") => (10, OpKind::Bin(BinOp::Mul)),
            Tok::Punct("/") => (10, OpKind::Bin(BinOp::Div)),
            Tok::Punct("%") => (10, OpKind::Bin(BinOp::Rem)),
            Tok::Punct("**") => (11, OpKind::Bin(BinOp::Pow)),
            _ => return None,
        };
        Some((prec, kind))
    }

    fn unary(&mut self) -> PResult<Expr> {
        self.enter()?;
        let r = self.unary_inner();
        self.leave();
        r
    }

    fn unary_inner(&mut self) -> PResult<Expr> {
        let op = match self.cur() {
            Tok::Punct("!") => Some(UnOp::Not),
            Tok::Punct("-") => Some(UnOp::Neg),
            Tok::Punct("+") => Some(UnOp::Plus),
            Tok::Punct("~") => Some(UnOp::BitNot),
            Tok::Ident(n) if n == "typeof" => Some(UnOp::TypeOf),
            Tok::Ident(n) if n == "void" => Some(UnOp::Void),
            Tok::Ident(n) if n == "delete" => Some(UnOp::Delete),
            _ => None,
        };
        if let Some(op) = op {
            self.advance();
            return Ok(Expr::Unary(op, Box::new(self.unary()?)));
        }
        if self.is_punct("++") || self.is_punct("--") {
            let inc = self.is_punct("++");
            self.advance();
            let target = self.unary()?;
            if !is_assign_target(&target) {
                return Err(String::from("invalid update target"));
            }
            return Ok(Expr::Update {
                inc,
                prefix: true,
                target: Box::new(target),
            });
        }
        self.postfix()
    }

    fn postfix(&mut self) -> PResult<Expr> {
        let e = self.call_member(None)?;
        // No-newline restriction on postfix ++/--.
        if (self.is_punct("++") || self.is_punct("--")) && !self.nl_before() && is_assign_target(&e)
        {
            let inc = self.is_punct("++");
            self.advance();
            return Ok(Expr::Update {
                inc,
                prefix: false,
                target: Box::new(e),
            });
        }
        Ok(e)
    }

    /// Parse a primary expression followed by any chain of calls, member
    /// accesses, and indexing. `new_callee` carries a pending `new`.
    fn call_member(&mut self, seed: Option<Expr>) -> PResult<Expr> {
        let mut e = match seed {
            Some(e) => e,
            None => {
                if self.eat_kw("new") {
                    // `new f(args)` binds the call to the new.
                    let callee = self.call_member_no_call()?;
                    let args = if self.is_punct("(") {
                        self.arguments()?
                    } else {
                        Vec::new()
                    };
                    Expr::New {
                        callee: Box::new(callee),
                        args,
                    }
                } else {
                    self.primary()?
                }
            }
        };
        loop {
            if self.eat_punct(".") {
                let name = self.ident()?;
                e = Expr::Member {
                    obj: Box::new(e),
                    prop: MemberProp::Dot(name),
                    optional: false,
                };
            } else if self.eat_punct("?.") {
                if self.is_punct("(") {
                    let args = self.arguments()?;
                    e = Expr::Call {
                        callee: Box::new(e),
                        args,
                    };
                } else {
                    let name = self.ident()?;
                    e = Expr::Member {
                        obj: Box::new(e),
                        prop: MemberProp::Dot(name),
                        optional: true,
                    };
                }
            } else if self.is_punct("[") {
                self.advance();
                let idx = self.expression()?;
                self.expect_punct("]")?;
                e = Expr::Member {
                    obj: Box::new(e),
                    prop: MemberProp::Index(Box::new(idx)),
                    optional: false,
                };
            } else if self.is_punct("(") {
                let args = self.arguments()?;
                e = Expr::Call {
                    callee: Box::new(e),
                    args,
                };
            } else {
                break;
            }
        }
        Ok(e)
    }

    /// Member chain without consuming a call — for `new X.Y.Z(args)`.
    fn call_member_no_call(&mut self) -> PResult<Expr> {
        let mut e = self.primary()?;
        loop {
            if self.eat_punct(".") {
                let name = self.ident()?;
                e = Expr::Member {
                    obj: Box::new(e),
                    prop: MemberProp::Dot(name),
                    optional: false,
                };
            } else if self.is_punct("[") {
                self.advance();
                let idx = self.expression()?;
                self.expect_punct("]")?;
                e = Expr::Member {
                    obj: Box::new(e),
                    prop: MemberProp::Index(Box::new(idx)),
                    optional: false,
                };
            } else {
                break;
            }
        }
        Ok(e)
    }

    fn arguments(&mut self) -> PResult<Vec<Expr>> {
        self.expect_punct("(")?;
        let mut args = Vec::new();
        while !self.is_punct(")") {
            args.push(self.assignment()?);
            if !self.eat_punct(",") {
                break;
            }
        }
        self.expect_punct(")")?;
        Ok(args)
    }

    fn primary(&mut self) -> PResult<Expr> {
        let e = match self.cur() {
            Tok::Num(n) => {
                let n = *n;
                self.advance();
                Expr::Num(n)
            }
            Tok::Str(s) => {
                let s = s.clone();
                self.advance();
                Expr::Str(s)
            }
            Tok::Regex(s) => {
                let s = s.clone();
                self.advance();
                Expr::Regex(s)
            }
            Tok::Template(_) => {
                let Tok::Template(parts) =
                    core::mem::replace(&mut self.toks[self.pos].kind, Tok::Punct("`"))
                else {
                    unreachable!()
                };
                self.advance();
                let mut out = Vec::new();
                for part in parts {
                    match part {
                        RawTplPart::Str(s) => out.push(TplPart::Str(s)),
                        RawTplPart::Expr(src) => {
                            let toks = lex(&src)?;
                            let mut sub = Parser {
                                toks,
                                pos: 0,
                                depth: self.depth,
                            };
                            out.push(TplPart::Expr(Box::new(sub.expression()?)));
                        }
                    }
                }
                Expr::Template(out)
            }
            Tok::Punct("(") => {
                self.advance();
                let e = self.expression()?;
                self.expect_punct(")")?;
                e
            }
            Tok::Punct("[") => {
                self.advance();
                let mut items = Vec::new();
                while !self.is_punct("]") {
                    if self.is_punct(",") {
                        // Elision → undefined slot.
                        items.push(Expr::Undefined);
                        self.advance();
                        continue;
                    }
                    items.push(self.assignment()?);
                    if !self.eat_punct(",") {
                        break;
                    }
                }
                self.expect_punct("]")?;
                Expr::Array(items)
            }
            Tok::Punct("{") => return self.object_literal(),
            Tok::Ident(name) => match name.as_str() {
                "true" => {
                    self.advance();
                    Expr::Bool(true)
                }
                "false" => {
                    self.advance();
                    Expr::Bool(false)
                }
                "null" => {
                    self.advance();
                    Expr::Null
                }
                "undefined" => {
                    self.advance();
                    Expr::Undefined
                }
                "this" => {
                    self.advance();
                    Expr::This
                }
                "function" => {
                    self.advance();
                    let def = self.function_rest(true, false)?;
                    Expr::Func(Rc::new(def))
                }
                "new" => return self.call_member(None),
                _ => {
                    let n = name.clone();
                    self.advance();
                    Expr::Ident(n)
                }
            },
            Tok::Punct(p) => return Err(format!("unexpected `{p}`")),
            Tok::Eof => return Err(String::from("unexpected end of script")),
        };
        Ok(e)
    }

    fn object_literal(&mut self) -> PResult<Expr> {
        self.expect_punct("{")?;
        let mut props = Vec::new();
        while !self.is_punct("}") {
            let key = match self.cur() {
                Tok::Ident(n) => {
                    let n = n.clone();
                    self.advance();
                    n
                }
                Tok::Str(s) => {
                    let s = s.to_string();
                    self.advance();
                    s
                }
                Tok::Num(n) => {
                    let s = super::value::num_to_string(*n);
                    self.advance();
                    s
                }
                _ => return Err(String::from("invalid object key")),
            };
            let value = if self.eat_punct(":") {
                self.assignment()?
            } else if self.is_punct("(") {
                // Method shorthand: `{ f() { … } }`.
                let params = self.param_list()?;
                let body = FnBody::Block(self.block()?);
                Expr::Func(Rc::new(FnDef {
                    name: Some(key.clone()),
                    params,
                    body,
                    is_arrow: false,
                }))
            } else {
                // Shorthand `{ a }`.
                Expr::Ident(key.clone())
            };
            props.push((key, value));
            if !self.eat_punct(",") {
                break;
            }
        }
        self.expect_punct("}")?;
        Ok(Expr::Object(props))
    }
}

enum OpKind {
    Bin(BinOp),
    Logical(LogOp),
}

fn is_assign_target(e: &Expr) -> bool {
    matches!(e, Expr::Ident(_) | Expr::Member { .. })
}

fn is_reserved(name: &str) -> bool {
    matches!(
        name,
        "true"
            | "false"
            | "null"
            | "undefined"
            | "this"
            | "function"
            | "new"
            | "typeof"
            | "void"
            | "delete"
            | "in"
            | "of"
            | "instanceof"
            | "var"
            | "let"
            | "const"
            | "if"
            | "else"
            | "while"
            | "do"
            | "for"
            | "return"
            | "break"
            | "continue"
            | "throw"
            | "try"
            | "catch"
            | "finally"
            | "switch"
            | "case"
            | "default"
            | "class"
    )
}
