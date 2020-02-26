#![allow(clippy::new_ret_no_self)]

use std::convert::TryInto;
use abgc_derive::GcLayout;

use crate::vm::{
    core::{VMError, VM},
    objects::{Obj, ObjType, StaticObjType},
    val::{NotUnboxable, Val},
};

#[derive(Debug, GcLayout)]
pub struct Array {
    arr: Vec<Val>,
    length: usize,
}

impl Obj for Array {
    fn dyn_objtype(&self) -> ObjType {
        ObjType::Array
    }

    fn get_class(&self, vm: &VM) -> Val {
        vm.arr_cls.clone()
    }
}

impl NotUnboxable for Array {}

impl StaticObjType for Array {
    fn static_objtype() -> ObjType {
        ObjType::Array
    }
}

impl Array {
    pub fn new(vm: &VM, len: Val, arr: Vec<Val>) -> Result<Val, Box<VMError>> {
        let length = len.as_usize(vm).unwrap(); 
        Ok(Val::from_obj(vm, Array { length, arr }))
    }

    pub fn at(&self, vm: &VM, at: Val) -> Result<Val, Box<VMError>> {
        let index = at.as_usize(vm).unwrap();
        if index < 1 && index > self.length && index > self.arr.len() + 1 {
            return Err(Box::new(VMError::IndexOutOfBounds));
        }

        Ok(self.arr[index - 1].clone())
    }

    pub fn put(&self, vm: &VM, put: Val, at: Val) -> Result<Val, Box<VMError>> {
        let index = at.as_usize(vm).unwrap();
        if index < 1 && index > self.length && index > self.arr.len() + 1 {
            return Err(Box::new(VMError::IndexOutOfBounds));
        }

        let mut arr = self.arr.to_vec();
        if index == self.arr.len() + 1 {
            arr.push(put);
        } else {
            arr.remove(index - 1);
            arr.insert(index - 1, put);
        }

        Array::new(vm, Val::from_isize(vm, self.length.try_into().unwrap()).unwrap(), arr)
    }

    pub fn length(&self, vm: &VM) -> Result<Val, Box<VMError>> {
        Val::from_isize(vm, self.length.try_into().unwrap())
    }
}
