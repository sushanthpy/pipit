//! Pipit HW Co-Design — Hardware-Software Co-Design Agents
//!
//! Bet 3: Verilog/VHDL generation + synthesis surrogate model.

pub mod hdl;
pub mod surrogate;

pub use hdl::{HdlModule, HdlLanguage, validate_verilog};
pub use surrogate::{SurrogateModel, SynthesisFeatures, SynthesisPrediction};
