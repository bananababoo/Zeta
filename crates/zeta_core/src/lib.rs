//! Game rules and data shared by the Zeta server and client.
//!
//! This crate performs no input or output: no network, files, database, system clock,
//! randomness, or printing. Callers pass in the current UTC time and every dice result,
//! so the same inputs always produce the same result, which Event Log replay and
//! deterministic tests depend on. `clippy.toml` in this crate enforces the boundary.
//!
//! Conventions for all code here: `docs/architecture/code_style.md`.

pub mod ai;
pub mod catalog;
pub mod entity;
pub mod event_log;
pub mod grid;
pub mod inspect;
pub mod item_rules;
pub mod movement;
pub mod pending_resolution;
pub mod protocol;
pub mod resolution;
pub mod ruleset;
pub mod settings;
pub mod snapshot;
pub mod time;
pub mod validation;
pub mod wire;
pub mod world;

#[cfg(test)]
mod test_support;
