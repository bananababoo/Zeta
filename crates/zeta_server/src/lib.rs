//! Headless authoritative Zeta server: everything that performs input or output on behalf
//! of the rules in `zeta_core`.
//!
//! Architecture: `docs/architecture/technical_architecture.md` Section 1. The binaries in
//! this crate stay thin and call into these modules.

pub mod ai;
pub mod bridge;
pub mod config;
pub mod db;
pub mod dice;
pub mod net;
pub mod settings;
pub mod world;
