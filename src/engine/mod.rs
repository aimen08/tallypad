//! The notepad calculator engine: plain text in, one result per line out.

mod eval;
mod lexer;
pub mod units;
mod value;

pub use eval::{Hl, LineOutput, Plot, evaluate_document_with};
pub use value::Value;
