// Copyright (c) 2019 King's College London created by the Software Development Team
// <http://soft-dev.org/>
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0>, or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, or the UPL-1.0 license <http://opensource.org/licenses/UPL>
// at your option. This file may not be copied, modified, or distributed except according to those
// terms.

use std::{
    cell::UnsafeCell,
    cmp::max,
    collections::hash_map::{self, HashMap},
    path::Path,
};

use abgc::Gc;
use indexmap::{self, map::IndexMap};
use itertools::Itertools;
use lrpar::{Lexeme, Lexer};

use crate::{
    compiler::{
        ast,
        instrs::{Builtin, Instr, Primitive},
        StorageT,
    },
    vm::{
        objects::{BlockInfo, Class, Method, MethodBody, String_},
        VM,
    },
};

pub struct Compiler<'a> {
    lexer: &'a dyn Lexer<StorageT>,
    path: &'a Path,
    instrs: Vec<Instr>,
    /// We collect all message sends together so that repeated calls of e.g. "println" take up
    /// constant space, no matter how many calls there are.
    sends: IndexMap<(String, usize), usize>,
    /// We map strings to an offset so that we only store them once, no matter how many times they
    /// are referenced in source code.
    strings: IndexMap<String, usize>,
    /// All the blocks a class contains.
    blocks: Vec<BlockInfo>,
    /// The stack of variables at the current point of evaluation.
    vars_stack: Vec<HashMap<&'a str, usize>>,
    /// Since SOM's "^" operator returns from the enclosed method, we need to track whether we are
    /// in a closure -- and, if so, how many nested closures we are inside at the current point of
    /// evaluation.
    closure_depth: usize,
}

impl<'a> Compiler<'a> {
    pub fn compile(
        vm: &VM,
        lexer: &dyn Lexer<StorageT>,
        path: &Path,
        astcls: &ast::Class,
    ) -> Result<Class, String> {
        let mut compiler = Compiler {
            lexer,
            path,
            instrs: Vec::new(),
            sends: IndexMap::new(),
            strings: IndexMap::new(),
            blocks: Vec::new(),
            vars_stack: Vec::new(),
            closure_depth: 0,
        };

        let mut errs = vec![];
        let name = lexer.lexeme_str(&astcls.name).to_owned();
        let supercls;
        if name != "Object" {
            if let Some(n) = astcls.supername.map(|x| lexer.lexeme_str(&x)) {
                supercls = match n {
                    "Block" => Some(vm.block_cls.clone()),
                    "Boolean" => Some(vm.bool_cls.clone()),
                    "nil" => None,
                    _ => unimplemented!(),
                };
            } else {
                supercls = Some(vm.obj_cls.clone());
            }
            // Whatever superclass has been chosen, it must have been initialised already or else
            // bad things will happen.
            debug_assert!(!supercls.as_ref().unwrap().is_illegal());
        } else {
            supercls = None;
        }

        let mut inst_vars = HashMap::with_capacity(astcls.inst_vars.len());
        for lexeme in &astcls.inst_vars {
            let vars_len = inst_vars.len();
            inst_vars.insert(lexer.lexeme_str(&lexeme), vars_len);
        }
        compiler.vars_stack.push(inst_vars);

        let mut methods = HashMap::with_capacity(astcls.methods.len());
        for astmeth in &astcls.methods {
            match compiler.c_method(&astmeth) {
                Ok(m) => {
                    methods.insert(m.name.clone(), Gc::new(m));
                }
                Err(mut e) => {
                    errs.extend(e.drain(..));
                }
            }
        }

        if !errs.is_empty() {
            let err_strs = errs
                .iter()
                .map(|(lexeme, msg)| {
                    let (line_off, col) = compiler.lexer.line_col(lexeme.start());
                    let line = compiler.lexer.surrounding_line_str(lexeme.start());
                    format!(
                        "File '{}', line {}, column {}:\n  {}\n{}",
                        compiler.path.to_str().unwrap(),
                        line_off,
                        col,
                        line.trim(),
                        msg
                    )
                })
                .join("\n\n");
            return Err(err_strs);
        }

        Ok(Class {
            name: String_::new(vm, name),
            path: compiler.path.to_path_buf(),
            supercls,
            num_inst_vars: astcls.inst_vars.len(),
            methods,
            instrs: compiler.instrs,
            blockinfos: compiler.blocks,
            sends: compiler.sends.into_iter().map(|(k, _)| k).collect(),
            strings: compiler
                .strings
                .into_iter()
                .map(|(k, _)| String_::new(vm, k))
                .collect(),
        })
    }

    fn string_off(&mut self, c: String) -> usize {
        let off = self.strings.len();
        match self.strings.entry(c) {
            indexmap::map::Entry::Occupied(e) => *e.get(),
            indexmap::map::Entry::Vacant(e) => {
                e.insert(off);
                off
            }
        }
    }

    fn send_off(&mut self, m: (String, usize)) -> usize {
        let off = self.sends.len();
        match self.sends.entry(m) {
            indexmap::map::Entry::Occupied(e) => *e.get(),
            indexmap::map::Entry::Vacant(e) => {
                e.insert(off);
                off
            }
        }
    }

    fn c_method(
        &mut self,
        astmeth: &ast::Method,
    ) -> Result<Method, Vec<(Lexeme<StorageT>, String)>> {
        let (name, args) = match astmeth.name {
            ast::MethodName::BinaryOp(op, arg) => {
                let arg_v = match arg {
                    Some(l) => vec![l],
                    None => vec![],
                };
                ((op, self.lexer.lexeme_str(&op).to_string()), arg_v)
            }
            ast::MethodName::Id(lexeme) => {
                ((lexeme, self.lexer.lexeme_str(&lexeme).to_string()), vec![])
            }
            ast::MethodName::Keywords(ref pairs) => {
                let name = pairs
                    .iter()
                    .map(|x| self.lexer.lexeme_str(&x.0))
                    .collect::<String>();
                let args = pairs.iter().map(|x| x.1).collect::<Vec<_>>();
                ((pairs[0].0, name), args)
            }
        };
        let body = self.c_body((name.0, &name.1), args, &astmeth.body)?;
        Ok(Method { name: name.1, body })
    }

    fn c_body(
        &mut self,
        name: (Lexeme<StorageT>, &str),
        params: Vec<Lexeme<StorageT>>,
        body: &ast::MethodBody,
    ) -> Result<MethodBody, Vec<(Lexeme<StorageT>, String)>> {
        // We check the number of arguments at compile-time so that we don't have to check them
        // continuously at run-time.
        let requires_args = |n: usize| -> Result<(), Vec<(Lexeme<StorageT>, String)>> {
            if params.len() != n {
                Err(vec![(
                    name.0,
                    format!("Expected {} parameters, got {}", n, params.len()),
                )])
            } else {
                Ok(())
            }
        };

        match body {
            ast::MethodBody::Primitive => match name.1 {
                "+" => {
                    requires_args(1)?;
                    Ok(MethodBody::Primitive(Primitive::Add))
                }
                "-" => {
                    requires_args(1)?;
                    Ok(MethodBody::Primitive(Primitive::Sub))
                }
                "*" => {
                    requires_args(1)?;
                    Ok(MethodBody::Primitive(Primitive::Mul))
                }
                "/" => {
                    requires_args(1)?;
                    Ok(MethodBody::Primitive(Primitive::Div))
                }
                "%" => {
                    requires_args(1)?;
                    Ok(MethodBody::Primitive(Primitive::Mod))
                }
                "=" => {
                    requires_args(1)?;
                    Ok(MethodBody::Primitive(Primitive::Equals))
                }
                "==" => {
                    requires_args(1)?;
                    Ok(MethodBody::Primitive(Primitive::RefEquals))
                }
                "~=" => {
                    requires_args(1)?;
                    Ok(MethodBody::Primitive(Primitive::NotEquals))
                }
                "<<" => {
                    requires_args(1)?;
                    Ok(MethodBody::Primitive(Primitive::Shl))
                }
                "<" => {
                    requires_args(1)?;
                    Ok(MethodBody::Primitive(Primitive::LessThan))
                }
                "<=" => {
                    requires_args(1)?;
                    Ok(MethodBody::Primitive(Primitive::LessThanEquals))
                }
                ">" => {
                    requires_args(1)?;
                    Ok(MethodBody::Primitive(Primitive::GreaterThan))
                }
                ">=" => {
                    requires_args(1)?;
                    Ok(MethodBody::Primitive(Primitive::GreaterThanEquals))
                }
                "&" => {
                    requires_args(1)?;
                    Ok(MethodBody::Primitive(Primitive::And))
                }
                "bitXor:" => {
                    requires_args(1)?;
                    Ok(MethodBody::Primitive(Primitive::BitXor))
                }
                "sqrt" => Ok(MethodBody::Primitive(Primitive::Sqrt)),
                "asString" => Ok(MethodBody::Primitive(Primitive::AsString)),
                "class" => Ok(MethodBody::Primitive(Primitive::Class)),
                "concatenate:" => Ok(MethodBody::Primitive(Primitive::Concatenate)),
                "halt" => Ok(MethodBody::Primitive(Primitive::Halt)),
                "hashcode" => Ok(MethodBody::Primitive(Primitive::Hashcode)),
                "inspect" => Ok(MethodBody::Primitive(Primitive::Inspect)),
                "instVarAt:" => Ok(MethodBody::Primitive(Primitive::InstVarAt)),
                "instVarAt:put:" => Ok(MethodBody::Primitive(Primitive::InstVarAtPut)),
                "instVarNamed:" => Ok(MethodBody::Primitive(Primitive::InstVarNamed)),
                "name" => Ok(MethodBody::Primitive(Primitive::Name)),
                "new" => Ok(MethodBody::Primitive(Primitive::New)),
                "objectSize" => Ok(MethodBody::Primitive(Primitive::ObjectSize)),
                "perform:" => Ok(MethodBody::Primitive(Primitive::Perform)),
                "perform:inSuperclass:" => {
                    Ok(MethodBody::Primitive(Primitive::PerformInSuperClass))
                }
                "perform:withArguments:" => {
                    Ok(MethodBody::Primitive(Primitive::PerformWithArguments))
                }
                "perform:withArguments:inSuperclass:" => Ok(MethodBody::Primitive(
                    Primitive::PerformWithArgumentsInSuperClass,
                )),
                "printNewline" => Ok(MethodBody::Primitive(Primitive::PrintNewline)),
                "printString:" => Ok(MethodBody::Primitive(Primitive::PrintString)),
                "restart" => Ok(MethodBody::Primitive(Primitive::Restart)),
                "value" => Ok(MethodBody::Primitive(Primitive::Value(0))),
                "value:" => Ok(MethodBody::Primitive(Primitive::Value(1))),
                "value:with:" => Ok(MethodBody::Primitive(Primitive::Value(2))),
                _ => Err(vec![(name.0, format!("Unknown primitive '{}'", name.1))]),
            },
            ast::MethodBody::Body {
                vars: vars_lexemes,
                exprs,
            } => {
                let bytecode_off = self.instrs.len();
                let (num_vars, max_stack) = self.c_block(true, &params, vars_lexemes, exprs)?;
                Ok(MethodBody::User {
                    num_vars,
                    bytecode_off,
                    max_stack,
                })
            }
        }
    }

    /// Evaluate an expression, returning `Ok(max_stack_size)` if successful. Note that there is an
    /// implicit assumption that primitives never need more stack size than they take in (e.g. if
    /// they push an item on to the stack, they must have popped at least one element off it
    /// beforehand).
    fn c_block(
        &mut self,
        is_method: bool,
        params: &[Lexeme<StorageT>],
        vars_lexemes: &[Lexeme<StorageT>],
        exprs: &[ast::Expr],
    ) -> Result<(usize, usize), Vec<(Lexeme<StorageT>, String)>> {
        let mut vars = HashMap::new();
        if is_method {
            // The VM assumes that the first variable of a method is "self".
            vars.insert("self", 0);
        }

        let mut process_var = |lexeme| {
            let vars_len = vars.len();
            let var_str = self.lexer.lexeme_str(&lexeme);
            match vars.entry(var_str) {
                hash_map::Entry::Occupied(_) => Err(vec![(
                    lexeme,
                    format!("Variable '{}' shadows another of the same name", var_str),
                )]),
                hash_map::Entry::Vacant(e) => {
                    e.insert(vars_len);
                    Ok(vars_len)
                }
            }
        };

        // The VM assumes that a blocks's arguments are stored in variables
        // 0..n and a method's arguments in 1..n+1.
        for lexeme in params.iter() {
            process_var(*lexeme)?;
        }

        for lexeme in vars_lexemes {
            process_var(*lexeme)?;
        }

        let num_vars = vars.len();
        self.vars_stack.push(vars);
        let mut max_stack = 0;
        for (i, e) in exprs.iter().enumerate() {
            // We deliberately bomb out at the first error in a method on the basis that
            // it's likely to lead to many repetitive errors.
            let stack_size = self.c_expr(e)?;
            max_stack = max(max_stack, stack_size);
            if i != exprs.len() - 1 {
                self.instrs.push(Instr::Pop);
            }
        }
        // Blocks return the value of the last statement, but methods return `self`.
        if is_method {
            self.instrs.push(Instr::Pop);
            debug_assert_eq!(*self.vars_stack.last().unwrap().get("self").unwrap(), 0);
            self.instrs.push(Instr::VarLookup(0, 0));
            max_stack = max(max_stack, 1);
        }
        self.instrs.push(Instr::Return);
        self.vars_stack.pop();

        Ok((num_vars, max_stack))
    }

    /// Evaluate an expression, returning `Ok(max_stack_size)` if successful.
    fn c_expr(&mut self, expr: &ast::Expr) -> Result<usize, Vec<(Lexeme<StorageT>, String)>> {
        match expr {
            ast::Expr::Assign { id, expr } => {
                let (depth, var_num) = self.find_var(&id)?;
                let max_stack = self.c_expr(expr)?;
                if depth == self.vars_stack.len() - 1 {
                    self.instrs.push(Instr::InstVarSet(var_num));
                } else {
                    self.instrs.push(Instr::VarSet(depth, var_num));
                }
                debug_assert!(max_stack > 0);
                Ok(max_stack)
            }
            ast::Expr::BinaryMsg { lhs, op, rhs } => {
                let mut stack_size = self.c_expr(lhs)?;
                stack_size = max(stack_size, 1 + self.c_expr(rhs)?);
                let send_off = self.send_off((self.lexer.lexeme_str(&op).to_string(), 1));
                self.instrs
                    .push(Instr::Send(send_off, UnsafeCell::new(None)));
                debug_assert!(stack_size > 0);
                Ok(stack_size)
            }
            ast::Expr::Block {
                params,
                vars,
                exprs,
            } => {
                let block_off = self.blocks.len();
                self.instrs.push(Instr::Block(block_off));
                self.blocks.push(BlockInfo {
                    bytecode_off: self.instrs.len(),
                    bytecode_end: 0,
                    num_params: params.len(),
                    num_vars: 0,
                    max_stack: 0,
                });
                self.closure_depth += 1;
                let (num_vars, max_stack) = self.c_block(false, params, vars, exprs)?;
                self.closure_depth -= 1;
                self.blocks[block_off].bytecode_end = self.instrs.len();
                self.blocks[block_off].num_vars = num_vars;
                self.blocks[block_off].max_stack = max_stack;
                Ok(1)
            }
            ast::Expr::Double { is_negative, val } => {
                let s = if *is_negative {
                    format!("-{}", self.lexer.lexeme_str(&val))
                } else {
                    self.lexer.lexeme_str(&val).to_owned()
                };
                match s.parse::<f64>() {
                    Ok(i) => {
                        self.instrs.push(Instr::Double(i));
                        Ok(1)
                    }
                    Err(e) => Err(vec![(*val, format!("{}", e))]),
                }
            }
            ast::Expr::Int { is_negative, val } => {
                match self.lexer.lexeme_str(&val).parse::<isize>() {
                    Ok(mut i) => {
                        if *is_negative {
                            // With twos complement, `0-i` will always succeed, but just in case...
                            i = 0isize.checked_sub(i).unwrap();
                        }
                        self.instrs.push(Instr::Int(i));
                        Ok(1)
                    }
                    Err(e) => Err(vec![(*val, format!("{}", e))]),
                }
            }
            ast::Expr::KeywordMsg { receiver, msglist } => {
                let mut max_stack = self.c_expr(receiver)?;
                let mut mn = String::new();
                for (i, (kw, expr)) in msglist.iter().enumerate() {
                    mn.push_str(self.lexer.lexeme_str(&kw));
                    let expr_stack = self.c_expr(expr)?;
                    max_stack = max(max_stack, 1 + i + expr_stack);
                }
                let send_off = self.send_off((mn, msglist.len()));
                self.instrs
                    .push(Instr::Send(send_off, UnsafeCell::new(None)));
                debug_assert!(max_stack > 0);
                Ok(max_stack)
            }
            ast::Expr::UnaryMsg { receiver, ids } => {
                let max_stack = self.c_expr(receiver)?;
                for id in ids {
                    let send_off = self.send_off((self.lexer.lexeme_str(&id).to_string(), 0));
                    self.instrs
                        .push(Instr::Send(send_off, UnsafeCell::new(None)));
                }
                debug_assert!(max_stack > 0);
                Ok(max_stack)
            }
            ast::Expr::Return(expr) => {
                let max_stack = self.c_expr(expr)?;
                if self.closure_depth == 0 {
                    self.instrs.push(Instr::Return);
                } else {
                    self.instrs.push(Instr::ClosureReturn(self.closure_depth));
                }
                debug_assert!(max_stack > 0);
                Ok(max_stack)
            }
            ast::Expr::String(lexeme) => {
                // XXX are there string escaping rules we need to take account of?
                let s_orig = self.lexer.lexeme_str(&lexeme);
                // Strip off the beginning/end quotes.
                let s = s_orig[1..s_orig.len() - 1].to_owned();
                let string_off = self.string_off(s);
                self.instrs.push(Instr::String(string_off));
                Ok(1)
            }
            ast::Expr::VarLookup(lexeme) => {
                match self.find_var(&lexeme) {
                    Ok((depth, var_num)) => {
                        if depth == self.vars_stack.len() - 1 {
                            self.instrs.push(Instr::InstVarLookup(var_num));
                        } else {
                            self.instrs.push(Instr::VarLookup(depth, var_num));
                        }
                    }
                    Err(e) => match self.lexer.lexeme_str(&lexeme) {
                        "nil" => self.instrs.push(Instr::Builtin(Builtin::Nil)),
                        "false" => self.instrs.push(Instr::Builtin(Builtin::False)),
                        "system" => self.instrs.push(Instr::Builtin(Builtin::System)),
                        "true" => self.instrs.push(Instr::Builtin(Builtin::True)),
                        _ => return Err(e),
                    },
                }
                Ok(1)
            }
        }
    }

    /// Find the variable `name` in the variable stack returning a tuple `Some((depth, var_num))`
    /// or `Err` if the variable isn't found. `depth` is the number of closures away from the
    /// "current" one that the variable is found.
    fn find_var(
        &self,
        lexeme: &Lexeme<StorageT>,
    ) -> Result<(usize, usize), Vec<(Lexeme<StorageT>, String)>> {
        let name = self.lexer.lexeme_str(lexeme);
        for (depth, vars) in self.vars_stack.iter().enumerate().rev() {
            if let Some(n) = vars.get(name) {
                return Ok((self.vars_stack.len() - depth - 1, *n));
            }
        }
        Err(vec![(*lexeme, format!("Unknown variable '{}'", name))])
    }
}
