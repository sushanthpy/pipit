//! Pipit HW Co-Design — Hardware-Software Co-Design Agents
//!
//! **STATUS: EXPERIMENTAL / R&D PROTOTYPE**
//!
//! This crate provides scaffolding for hardware-software co-design:
//! - `hdl`: Verilog module generation and validation (via `iverilog`)
//! - `surrogate`: Synthesis outcome prediction from Verilog features
//!
//! Current limitations:
//! - The surrogate model uses hardcoded linear weights, not a trained ML model.
//!   It provides rough order-of-magnitude estimates only. Predictions should
//!   NOT be used for production synthesis decisions.
//! - HDL template generation produces boilerplate with placeholder logic.
//!   Generated Verilog requires manual implementation of functional blocks.
//! - Validation works correctly (delegates to `iverilog`).
//!
//! Future work:
//! - Train a gradient-boosted tree on real synthesis data (Kendall's τ > 0.7)
//! - Add VHDL support
//! - Integration with real synthesis tools (Vivado, Quartus) for ground truth

pub mod hdl;
pub mod surrogate;

pub use hdl::{HdlLanguage, HdlModule, validate_verilog};
pub use surrogate::{SurrogateModel, SynthesisFeatures, SynthesisPrediction};
