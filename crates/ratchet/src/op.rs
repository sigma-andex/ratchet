use crate::{CompiledOp, Tensor};

#[derive(Debug)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
}

#[derive(Debug)]
pub enum UnaryOp {
    Gelu,
}

#[derive(Debug)]
pub enum LazyOp {
    Empty,
    Binary(Tensor, Tensor, BinaryOp),
    Unary(Tensor, UnaryOp),
}

impl LazyOp {
    pub fn compile(&self) -> CompiledOp {
        todo!()
    }
}