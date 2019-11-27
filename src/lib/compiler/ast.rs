// Copyright (c) 2019 King's College London created by the Software Development Team
// <http://soft-dev.org/>
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0>, or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, or the UPL-1.0 license <http://opensource.org/licenses/UPL>
// at your option. This file may not be copied, modified, or distributed except according to those
// terms.

use lrpar::Lexeme;

type StorageT = u32;

#[derive(Debug)]
pub struct Class {
    pub name: Lexeme<StorageT>,
    pub supername: Option<Lexeme<StorageT>>,
    pub inst_vars: Vec<Lexeme<StorageT>>,
    pub methods: Vec<Method>,
}

#[derive(Debug)]
pub struct Method {
    pub name: MethodName,
    pub body: MethodBody,
}

#[derive(Debug)]
pub enum MethodName {
    BinaryOp(Lexeme<StorageT>, Option<Lexeme<StorageT>>),
    Id(Lexeme<StorageT>),
    Keywords(Vec<(Lexeme<StorageT>, Lexeme<StorageT>)>),
}

#[derive(Debug)]
pub enum MethodBody {
    Primitive,
    Body {
        vars: Vec<Lexeme<StorageT>>,
        exprs: Vec<Expr>,
    },
}

#[derive(Debug)]
pub enum Expr {
    Assign {
        id: Lexeme<StorageT>,
        expr: Box<Expr>,
    },
    BinaryMsg {
        lhs: Box<Expr>,
        op: Lexeme<StorageT>,
        rhs: Box<Expr>,
    },
    Block {
        params: Vec<Lexeme<StorageT>>,
        vars: Vec<Lexeme<StorageT>>,
        exprs: Vec<Expr>,
    },
    Double {
        is_negative: bool,
        val: Lexeme<StorageT>,
    },
    Int {
        is_negative: bool,
        val: Lexeme<StorageT>,
    },
    KeywordMsg {
        receiver: Box<Expr>,
        msglist: Vec<(Lexeme<StorageT>, Expr)>,
    },
    UnaryMsg {
        receiver: Box<Expr>,
        ids: Vec<Lexeme<StorageT>>,
    },
    Return(Box<Expr>),
    String(Lexeme<StorageT>),
    Symbol(Lexeme<StorageT>),
    VarLookup(Lexeme<StorageT>),
}
