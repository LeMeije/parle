//! App-side sync: the parts the protocol crate deliberately leaves out.
//!
//! `echokey-sync` is transport and crypto only. Three things it cannot own,
//! because they need state that outlives a single pairing run or knowledge of
//! the user's data, live here:
//!
//! - [`guard`] — expiry and rate limiting on pairing attempts. Without it the
//!   6-digit code is only as strong as the number of guesses we allow.
//! - [`keystore`] — paired keys in the OS credential store, not in settings.
//! - the exclusion rule: password-manager entries and Concealed/Transient
//!   clipboard content must never leave the machine. That is enforced before
//!   anything is handed to the protocol, never after.

pub mod deadline;
pub mod guard;
pub mod manager;
pub mod keystore;
pub mod pair_flow;
pub mod replicate;
pub mod wire_tcp;

#[cfg(test)]
mod adv5b;

#[cfg(test)]
mod adversarial_r6;

#[cfg(test)]
mod adversarial_r6_sec;

#[cfg(test)]
mod adversarial_r7_scale;

#[cfg(test)]
mod adversarial_r8_sec;

#[cfg(test)]
mod adversarial_r8_conc;

#[cfg(test)]
mod adversarial_r8_platform;

#[cfg(test)]
mod adversarial_r8_data;

#[cfg(test)]
mod adversarial_r9_sec;

#[cfg(test)]
mod adversarial_r9_conc;

#[cfg(test)]
mod adversarial_r9_data;

#[cfg(test)]
mod adversarial_r10_sec;

#[cfg(test)]
mod adversarial_r10_data;

#[cfg(test)]
mod adversarial_r11_sec;

#[cfg(test)]
mod adversarial_r11_data;
