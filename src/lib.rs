//! RPN / XRPN calculator engine.
//!
//! HP-41CX-style stack (X/Y/Z/T + Last X), the full math / trig / log / stats /
//! base / HMS / polar command set, number formatting faithful to desktop XRPN,
//! and a FOCAL program runner (labels, GTO/XEQ/GSB/RTN, ISG/DSE, conditionals).
//!
//! Pure logic: every entry point takes a `CalcState` and returns new state, so
//! the same engine drives both frontends. The RPNx TUI (desktop) links this
//! crate directly. The RPNx phone app enables the optional `uniffi` feature,
//! which exposes the FFI surface its Kotlin shell binds to.

#[cfg(feature = "uniffi")]
uniffi::setup_scaffolding!();

mod engine;
mod format;
mod program;

pub use engine::*;
pub use program::*;
