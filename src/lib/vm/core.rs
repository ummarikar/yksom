// Copyright (c) 2019 King's College London created by the Software Development Team
// <http://soft-dev.org/>
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0>, or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, or the UPL-1.0 license <http://opensource.org/licenses/UPL>
// at your option. This file may not be copied, modified, or distributed except according to those
// terms.

//! The core part of the interpreter.

use std::{
    cell::UnsafeCell,
    path::{Path, PathBuf},
    process, ptr,
};

use abgc::{Gc, GcLayout};

use crate::{
    compiler::{
        compile,
        instrs::{Builtin, Instr, Primitive},
    },
    vm::{
        objects::{Block, Class, Double, Inst, MethodBody, ObjType, String_},
        val::{Val, ValResult},
    },
};

pub const SOM_EXTENSION: &str = "som";

#[derive(Debug, PartialEq)]
pub enum VMError {
    /// A value which can't be represented in an `isize`.
    CantRepresentAsBigInt,
    /// A value which can't be represented in an `f64`.
    CantRepresentAsDouble,
    /// A value which can't be represented in an `isize`.
    CantRepresentAsIsize,
    /// A value which can't be represented in an `usize`.
    CantRepresentAsUsize,
    DivisionByZero,
    /// The VM is trying to exit.
    Exit,
    /// Tried to perform a `Val::downcast` operation on a non-boxed `Val`. Note that `expected`
    /// and `got` can reference the same `ObjType`.
    GcBoxTypeError {
        expected: ObjType,
        got: ObjType,
    },
    /// Tried to do a shl or shr with a value below zero.
    NegativeShift,
    /// A specialised version of TypeError, because SOM has more than one number type (and casts
    /// between them as necessary) so the `expected` field of `TypeError` doesn't quite work.
    NotANumber {
        got: ObjType,
    },
    /// Something went wrong when trying to execute a primitive.
    PrimitiveError,
    /// Tried to do a shl that would overflow memory and/or not fit in the required integer size.
    ShiftTooBig,
    /// A dynamic type error.
    TypeError {
        expected: ObjType,
        got: ObjType,
    },
    /// Tried to read from a local variable that hasn't had a value assigned to it yet.
    UnassignedVar(usize),
    /// An unknown method.
    UnknownMethod(String),
}

#[derive(Debug)]
/// The (internal) result of a SOM send.
enum SendReturn {
    /// A closure wants to perform a return *n* frames up the call stack with a.
    ClosureReturn(usize),
    /// An error has occurred.
    Err(Box<VMError>),
    /// A return value has been left at the appropriate place on the SOM stack.
    Val,
}

/// A convenience macro for use in the `exec_*` functions.
macro_rules! stry {
    ($elem:expr) => {{
        let e = $elem;
        match e {
            Ok(o) => o,
            Err(e) => return SendReturn::Err(e),
        }
    }};
}

pub struct VM {
    classpath: Vec<String>,
    pub block_cls: Val,
    pub block2_cls: Val,
    pub block3_cls: Val,
    pub bool_cls: Val,
    pub cls_cls: Val,
    pub double_cls: Val,
    pub false_cls: Val,
    pub int_cls: Val,
    pub nil_cls: Val,
    pub obj_cls: Val,
    pub str_cls: Val,
    pub system_cls: Val,
    pub true_cls: Val,
    pub false_: Val,
    pub nil: Val,
    pub system: Val,
    pub true_: Val,
    stack: UnsafeCell<Vec<Val>>,
    frames: UnsafeCell<Vec<Frame>>,
}

impl VM {
    pub fn new(classpath: Vec<String>) -> Self {
        // The bootstrapping phase is delicate: we need to bootstrap the Object, Class, and Nil
        // classes before we can create basic objects like nil. We thus perform bootstrapping in
        // two phases: the "very delicate" phase (with very strict rules on what is possible)
        // followed by the "slightly delicate phase" (with looser, but still fairly strict, rules
        // on what is possible).
        //
        let mut vm = VM {
            classpath,
            block_cls: Val::illegal(),
            bool_cls: Val::illegal(),
            block2_cls: Val::illegal(),
            block3_cls: Val::illegal(),
            cls_cls: Val::illegal(),
            double_cls: Val::illegal(),
            false_cls: Val::illegal(),
            int_cls: Val::illegal(),
            nil_cls: Val::illegal(),
            obj_cls: Val::illegal(),
            str_cls: Val::illegal(),
            system_cls: Val::illegal(),
            true_cls: Val::illegal(),
            false_: Val::illegal(),
            nil: Val::illegal(),
            system: Val::illegal(),
            true_: Val::illegal(),
            stack: UnsafeCell::new(Vec::new()),
            frames: UnsafeCell::new(Vec::new()),
        };

        // The very delicate phase.
        //
        // Nothing in this phase must store references to the nil object or any classes earlier
        // than it in the phase.
        vm.obj_cls = vm.init_builtin_class("Object", false);
        vm.cls_cls = vm.init_builtin_class("Class", false);
        vm.nil_cls = vm.init_builtin_class("Nil", true);
        vm.nil = Inst::new(&vm, vm.nil_cls.clone());

        // The slightly delicate phase.
        //
        // Nothing in this phase must store references to any classes earlier than it in the phase.
        vm.block_cls = vm.init_builtin_class("Block", false);
        vm.block2_cls = vm.init_builtin_class("Block2", false);
        vm.block3_cls = vm.init_builtin_class("Block3", false);
        vm.bool_cls = vm.init_builtin_class("Boolean", false);
        vm.double_cls = vm.init_builtin_class("Double", false);
        vm.false_cls = vm.init_builtin_class("False", false);
        vm.int_cls = vm.init_builtin_class("Integer", false);
        vm.str_cls = vm.init_builtin_class("String", false);
        vm.system_cls = vm.init_builtin_class("System", false);
        vm.true_cls = vm.init_builtin_class("True", false);
        vm.false_ = Inst::new(&vm, vm.false_cls.clone());
        vm.system = Inst::new(&vm, vm.system_cls.clone());
        vm.true_ = Inst::new(&vm, vm.true_cls.clone());

        vm
    }

    /// Compile the file at `path`. `inst_vars_allowed` should be set to `false` only for those
    /// builtin classes which do not lead to run-time instances of `Inst`.
    pub fn compile(&self, path: &Path, inst_vars_allowed: bool) -> Val {
        let ccls = compile(path);
        if !inst_vars_allowed && ccls.num_inst_vars > 0 {
            panic!("No instance vars allowed in {}", path.to_str().unwrap());
        }
        Class::from_ccls(self, ccls).unwrap_or_else(|e| {
            panic!(
                "Fatal compilation error for {}: {:?}",
                path.to_str().unwrap(),
                e
            )
        })
    }

    fn find_class(&self, name: &str) -> Result<PathBuf, ()> {
        for dn in &self.classpath {
            let mut pb = PathBuf::new();
            pb.push(dn);
            pb.push(name);
            pb.set_extension(SOM_EXTENSION);
            if pb.is_file() {
                return Ok(pb);
            }
        }
        Err(())
    }

    /// Find and compile the builtin class 'name'.
    fn init_builtin_class(&self, name: &str, inst_vars_allowed: bool) -> Val {
        let path = self
            .find_class(name)
            .unwrap_or_else(|_| panic!("Can't find builtin class '{}'", name));
        self.compile(&path, inst_vars_allowed)
    }

    /// Inform the user of the error string `error` and then exit.
    pub fn error(&self, error: &str) -> ! {
        eprintln!("{}", error);
        process::exit(1);
    }

    /// Send the message `msg` to the receiver `rcv` with arguments `args`.
    pub fn send(&self, rcv: Val, msg: &str, args: Vec<Val>) -> ValResult {
        let cls = rcv.get_class(self);
        let (meth_cls_val, meth) = rtry!(rtry!(cls.downcast::<Class>(self)).get_method(self, msg));
        match meth.body {
            MethodBody::Primitive(_) => {
                panic!("Primitives can't be called outside of a function frame.");
            }
            MethodBody::User {
                num_vars,
                bytecode_off,
            } => {
                let meth_cls = rtry!(meth_cls_val.downcast::<Class>(self));
                let frame = Frame::new(self, true, rcv.clone(), None, num_vars, args);
                unsafe { &mut *self.frames.get() }.push(frame);
                let r = self.exec_user(rcv, meth_cls, bytecode_off);
                self.frame_pop();
                match r {
                    SendReturn::ClosureReturn(_) => unimplemented!(),
                    SendReturn::Err(e) => ValResult::from_vmerror(*e),
                    SendReturn::Val => ValResult::from_val(self.stack_pop()),
                }
            }
        }
    }

    /// Execute a SOM method. Note that the frame for this method must have been created *before*
    /// calling this function.
    fn exec_user(&self, rcv: Val, cls: &Class, meth_start_pc: usize) -> SendReturn {
        let mut pc = meth_start_pc;
        let stack_start = self.stack_len();
        while let Some(instr) = cls.instrs.get(pc) {
            match *instr {
                Instr::Block(blkinfo_off) => {
                    let blkinfo = cls.blockinfo(blkinfo_off);
                    self.stack_push(Block::new(
                        self,
                        Val::recover(cls),
                        blkinfo_off,
                        Gc::clone(&self.current_frame().closure),
                        blkinfo.num_params,
                    ));
                    pc = blkinfo.bytecode_end;
                }
                Instr::Builtin(b) => {
                    self.stack_push(match b {
                        Builtin::Nil => self.nil.clone(),
                        Builtin::False => self.false_.clone(),
                        Builtin::System => self.system.clone(),
                        Builtin::True => self.true_.clone(),
                    });
                    pc += 1;
                }
                Instr::ClosureReturn(closure_depth) => {
                    // We want to do a non-local return. Before we attempt that, we need to
                    // check that the block hasn't escaped its function (and we know we're in a
                    // block because only a block can attempt a non-local return).
                    // Fortunately, the `closure` pointer in a frame is a perfect proxy for
                    // determining this: if this frame's (i.e. block's!) parent closure is not
                    // consistent with the frame stack, then the block has escaped.
                    let v = self.stack_pop();
                    let parent_closure = self.current_frame().closure(closure_depth);
                    for (frame_depth, pframe) in
                        unsafe { &*self.frames.get() }.iter().rev().enumerate()
                    {
                        if Gc::ptr_eq(&parent_closure, &pframe.closure) {
                            self.stack_truncate(pframe.sp());
                            self.stack_push(v);
                            return SendReturn::ClosureReturn(frame_depth);
                        }
                    }
                    panic!("Return from escaped block");
                }
                Instr::Const(coff) => {
                    self.stack_push(cls.consts[coff].clone());
                    pc += 1;
                }
                Instr::Double(i) => {
                    self.stack_push(Double::new(self, i));
                    pc += 1;
                }
                Instr::InstVarLookup(n) => {
                    let inst: &Inst = rcv.downcast(self).unwrap();
                    self.stack_push(inst.inst_var_lookup(n));
                    pc += 1;
                }
                Instr::InstVarSet(n) => {
                    let inst: &Inst = rcv.downcast(self).unwrap();
                    inst.inst_var_set(n, self.stack_peek());
                    pc += 1;
                }
                Instr::Pop => {
                    self.stack_pop();
                    pc += 1;
                }
                Instr::Return => {
                    return SendReturn::Val;
                }
                Instr::Send(moff) => {
                    let (ref name, nargs) = &cls.sends[moff];
                    let args = unsafe { &mut *self.stack.get() }
                        .drain(self.stack_len() - nargs..)
                        .rev()
                        .collect::<Vec<_>>();
                    let rcv = self.stack_pop();

                    let cls = rcv.get_class(self);
                    let (meth_cls_val, meth) =
                        stry!(stry!(cls.downcast::<Class>(self)).get_method(self, &name));
                    self.current_frame().set_sp(self.stack_len());
                    let r = match meth.body {
                        MethodBody::Primitive(Primitive::Restart) => {
                            self.stack_truncate(stack_start);
                            pc = meth_start_pc;
                            continue;
                        }
                        MethodBody::Primitive(p) => self.exec_primitive(p, rcv, args),
                        MethodBody::User {
                            num_vars,
                            bytecode_off,
                        } => {
                            let meth_cls = stry!(meth_cls_val.downcast::<Class>(self));

                            let nframe = Frame::new(self, true, rcv.clone(), None, num_vars, args);
                            unsafe { &mut *self.frames.get() }.push(nframe);
                            let r = self.exec_user(rcv, meth_cls, bytecode_off);
                            self.frame_pop();
                            r
                        }
                    };
                    match r {
                        SendReturn::ClosureReturn(d) => {
                            if d > 0 {
                                return SendReturn::ClosureReturn(d - 1);
                            }
                        }
                        SendReturn::Err(e) => {
                            return SendReturn::Err(e);
                        }
                        SendReturn::Val => (),
                    }
                    pc += 1;
                }
                Instr::VarLookup(d, n) => {
                    let val = stry!(self.current_frame().var_lookup(d, n).as_result());
                    self.stack_push(val);
                    pc += 1;
                }
                Instr::VarSet(d, n) => {
                    let val = self.stack_peek();
                    self.current_frame().var_set(d, n, val);
                    pc += 1;
                }
            }
        }

        unsafe { &mut *self.frames.get() }.pop();
        SendReturn::Err(Box::new(VMError::Exit))
    }

    fn exec_primitive(&self, prim: Primitive, rcv: Val, args: Vec<Val>) -> SendReturn {
        match prim {
            Primitive::Add => {
                debug_assert_eq!(args.len(), 1);
                self.stack_push(stry!(rcv.add(self, args[0].clone()).as_result()));
                SendReturn::Val
            }
            Primitive::AsString => {
                self.stack_push(stry!(rcv.to_strval(self).as_result()));
                SendReturn::Val
            }
            Primitive::Class => {
                self.stack_push(rcv.get_class(self));
                SendReturn::Val
            }
            Primitive::Concatenate => {
                debug_assert_eq!(args.len(), 1);
                self.stack_push(stry!(stry!(rcv.downcast::<String_>(self))
                    .concatenate(self, args[0].clone())
                    .as_result()));
                SendReturn::Val
            }
            Primitive::Div => {
                debug_assert_eq!(args.len(), 1);
                self.stack_push(stry!(rcv.div(self, args[0].clone()).as_result()));
                SendReturn::Val
            }
            Primitive::Equals => {
                debug_assert_eq!(args.len(), 1);
                self.stack_push(stry!(rcv.equals(self, args[0].clone()).as_result()));
                SendReturn::Val
            }
            Primitive::GreaterThan => {
                debug_assert_eq!(args.len(), 1);
                self.stack_push(stry!(rcv.greater_than(self, args[0].clone()).as_result()));
                SendReturn::Val
            }
            Primitive::GreaterThanEquals => {
                debug_assert_eq!(args.len(), 1);
                self.stack_push(stry!(rcv
                    .greater_than_equals(self, args[0].clone())
                    .as_result()));
                SendReturn::Val
            }
            Primitive::LessThan => {
                debug_assert_eq!(args.len(), 1);
                self.stack_push(stry!(rcv.less_than(self, args[0].clone()).as_result()));
                SendReturn::Val
            }
            Primitive::LessThanEquals => {
                debug_assert_eq!(args.len(), 1);
                self.stack_push(stry!(rcv
                    .less_than_equals(self, args[0].clone())
                    .as_result()));
                SendReturn::Val
            }
            Primitive::Mul => {
                debug_assert_eq!(args.len(), 1);
                self.stack_push(stry!(rcv.mul(self, args[0].clone()).as_result()));
                SendReturn::Val
            }
            Primitive::Name => {
                self.stack_push(stry!(stry!(rcv.downcast::<Class>(self))
                    .name(self)
                    .as_result()));
                SendReturn::Val
            }
            Primitive::New => {
                debug_assert_eq!(args.len(), 0);
                self.stack_push(Inst::new(self, rcv));
                SendReturn::Val
            }
            Primitive::NotEquals => {
                debug_assert_eq!(args.len(), 1);
                self.stack_push(stry!(rcv.not_equals(self, args[0].clone()).as_result()));
                SendReturn::Val
            }
            Primitive::Restart => unreachable!(),
            Primitive::PrintNewline => {
                println!();
                self.stack_push(self.system.clone());
                SendReturn::Val
            }
            Primitive::PrintString => {
                debug_assert_eq!(args.len(), 1);
                let str_: &String_ = stry!(args[0].downcast(self));
                print!("{}", str_.as_str());
                self.stack_push(self.system.clone());
                SendReturn::Val
            }
            Primitive::Shl => {
                debug_assert_eq!(args.len(), 1);
                self.stack_push(stry!(rcv.shl(self, args[0].clone()).as_result()));
                SendReturn::Val
            }
            Primitive::Sub => {
                debug_assert_eq!(args.len(), 1);
                self.stack_push(stry!(rcv.sub(self, args[0].clone()).as_result()));
                SendReturn::Val
            }
            Primitive::Value => {
                let rcv_blk: &Block = stry!(rcv.downcast(self));
                let blk_cls: &Class = stry!(rcv_blk.blockinfo_cls.downcast(self));
                let blkinfo = blk_cls.blockinfo(rcv_blk.blockinfo_off);
                let frame = Frame::new(
                    self,
                    false,
                    rcv.clone(),
                    Some(Gc::clone(&rcv_blk.parent_closure)),
                    blkinfo.num_vars,
                    args,
                );
                unsafe { &mut *self.frames.get() }.push(frame);
                let r = self.exec_user(rcv.clone(), blk_cls, blkinfo.bytecode_off);
                self.frame_pop();
                r
            }
        }
    }

    fn current_frame(&self) -> &Frame {
        debug_assert!(!unsafe { &*self.frames.get() }.is_empty());
        let frames_len = unsafe { &*self.frames.get() }.len();
        unsafe { (&mut *self.frames.get()).get_unchecked(frames_len - 1) }
    }

    fn frame_pop(&self) {
        unsafe { &mut *self.frames.get() }.pop();
    }

    fn stack_len(&self) -> usize {
        unsafe { &*self.stack.get() }.len()
    }

    /// Returns the top-most value of the stack without removing it. If the stack is empty, calling
    /// this function will lead to undefined behaviour.
    fn stack_peek(&self) -> Val {
        // Since we know that there will be at least one element in the stack, we can use our own
        // simplified version of pop() which avoids a branch and the wrapping of values in an
        // Option.
        let stack = unsafe { &*self.stack.get() };
        debug_assert!(!stack.is_empty());
        let i = stack.len() - 1;
        unsafe { stack.get_unchecked(i) }.clone()
    }

    /// Pops the top-most value of the stack and returns it. If the stack is empty, calling
    /// this function will lead to undefined behaviour.
    fn stack_pop(&self) -> Val {
        // Since we know that there will be at least one element in the stack, we can use our own
        // simplified version of pop() which avoids a branch and the wrapping of values in an
        // Option.
        unsafe {
            let stack = &mut *self.stack.get();
            debug_assert!(!stack.is_empty());
            let i = stack.len() - 1;
            let v = ptr::read(stack.get_unchecked(i));
            stack.set_len(i);
            v
        }
    }

    /// Push `v` onto the stack.
    fn stack_push(&self, v: Val) {
        unsafe { &mut *self.stack.get() }.push(v);
    }

    fn stack_truncate(&self, i: usize) {
        unsafe { &mut *self.stack.get() }.truncate(i);
    }
}

#[derive(Debug)]
pub struct Frame {
    /// Stack pointer. Note that this is updated lazily (i.e. it might not be accurate at all
    /// points, but it is guaranteed to be correct over function calls).
    sp: UnsafeCell<usize>,
    closure: Gc<Closure>,
}

impl Frame {
    fn new(
        _: &VM,
        is_method: bool,
        self_val: Val,
        parent_closure: Option<Gc<Closure>>,
        num_vars: usize,
        args: Vec<Val>,
    ) -> Self {
        let mut vars = Vec::new();
        vars.resize(num_vars, Val::illegal());

        if is_method {
            vars[0] = self_val;
            for (i, arg) in args.iter().enumerate() {
                vars[i + 1] = arg.clone();
            }
        } else {
            for (i, arg) in args.iter().enumerate() {
                vars[i] = arg.clone();
            }
        }

        Frame {
            sp: UnsafeCell::new(0),
            closure: Gc::new(Closure::new(parent_closure, vars)),
        }
    }

    fn var_lookup(&self, depth: usize, var: usize) -> ValResult {
        let cl = self.closure(depth);
        let v = cl.get_var(var);
        if v.is_illegal() {
            ValResult::from_vmerror(VMError::UnassignedVar(var))
        } else {
            ValResult::from_val(v.clone())
        }
    }

    fn var_set(&self, depth: usize, var: usize, val: Val) {
        self.closure(depth).set_var(var, val);
    }

    /// Return the closure `depth` closures up from this frame's closure (where `depth` can be 0
    /// which returns this frame's closure).
    fn closure(&self, mut depth: usize) -> Gc<Closure> {
        let mut c = Gc::clone(&self.closure);
        while depth > 0 {
            c = Gc::clone(c.parent.as_ref().unwrap());
            depth -= 1;
        }
        c
    }

    /// Return this frame's stack pointer.
    fn sp(&self) -> usize {
        *unsafe { &*self.sp.get() }
    }

    /// Set this frame's stack pointer to `sp`.
    fn set_sp(&self, sp: usize) {
        *unsafe { &mut *self.sp.get() } = sp;
    }
}

#[derive(Debug)]
pub struct Closure {
    parent: Option<Gc<Closure>>,
    vars: Gc<Vars>,
}

#[derive(Debug)]
struct Vars(UnsafeCell<Vec<Val>>);

impl Closure {
    fn new(parent: Option<Gc<Closure>>, vars: Vec<Val>) -> Closure {
        Closure {
            parent,
            vars: Gc::new(Vars(UnsafeCell::new(vars))),
        }
    }

    fn get_var(&self, var: usize) -> Val {
        unsafe { (&*self.vars.0.get()).get_unchecked(var) }.clone()
    }

    fn set_var(&self, var: usize, val: Val) {
        unsafe { *(&mut *self.vars.0.get()).get_unchecked_mut(var) = val };
    }
}

impl GcLayout for Closure {
    fn layout(&self) -> std::alloc::Layout {
        std::alloc::Layout::new::<Self>()
    }
}

impl GcLayout for Vars {
    fn layout(&self) -> std::alloc::Layout {
        std::alloc::Layout::new::<Self>()
    }
}

#[cfg(test)]
impl VM {
    pub fn new_no_bootstrap() -> Self {
        VM {
            classpath: vec![],
            block_cls: Val::illegal(),
            block2_cls: Val::illegal(),
            block3_cls: Val::illegal(),
            bool_cls: Val::illegal(),
            cls_cls: Val::illegal(),
            double_cls: Val::illegal(),
            false_cls: Val::illegal(),
            int_cls: Val::illegal(),
            obj_cls: Val::illegal(),
            nil_cls: Val::illegal(),
            str_cls: Val::illegal(),
            system_cls: Val::illegal(),
            true_cls: Val::illegal(),
            false_: Val::illegal(),
            nil: Val::illegal(),
            system: Val::illegal(),
            true_: Val::illegal(),
            stack: UnsafeCell::new(Vec::new()),
            frames: UnsafeCell::new(Vec::new()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_frame() {
        let vm = VM::new_no_bootstrap();
        let selfv = Val::from_isize(&vm, 42).unwrap();
        let v1 = Val::from_isize(&vm, 43).unwrap();
        let v2 = Val::from_isize(&vm, 44).unwrap();
        let f = Frame::new(&vm, true, selfv, None, 4, vec![v1, v2]);
        assert_eq!(f.var_lookup(0, 0).unwrap().as_isize(&vm).unwrap(), 42);
        assert_eq!(f.var_lookup(0, 1).unwrap().as_isize(&vm).unwrap(), 43);
        assert_eq!(f.var_lookup(0, 2).unwrap().as_isize(&vm).unwrap(), 44);
        assert!(f.var_lookup(0, 3).is_err());
    }
}
