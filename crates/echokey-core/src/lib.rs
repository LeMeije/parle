//! EchoKey core: everything platform-independent.
//!
//! The formatter and dictionary are governed by the behavioural contract in
//! `shared/*.json` — change the vectors first, then make the code pass.

pub mod dictionary;
pub mod formatter;
pub mod history;
pub mod search;
pub mod settings;
pub mod types;

pub use types::*;
