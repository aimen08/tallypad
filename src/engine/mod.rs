//! The notepad calculator engine: plain text in, one result per line out.

mod eval;
mod lexer;
pub mod units;
mod value;

pub use eval::{Hl, LineOutput, evaluate_document};
pub use value::Value;
