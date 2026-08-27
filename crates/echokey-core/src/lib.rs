//! EchoKey core: everything platform-independent.
//!
//! The formatter and dictionary are governed by the behavioural contract in
//! `shared/*.json` — change the vectors first, then make the code pass.

#[cfg(test)]
mod adversarial_r6_data;
mod adversarial_r7_store;
mod adversarial_r8_data;
mod adversarial_r9_data;
mod adversarial_r10_data;
mod adversarial_r11_data;
#[cfg(test)]
mod adversarial_r12_data;
#[cfg(test)]
mod adversarial_r13_data;

pub mod dictionary;
pub mod formatter;
pub mod history;
pub mod search;
pub mod settings;
pub mod types;

pub use types::*;
