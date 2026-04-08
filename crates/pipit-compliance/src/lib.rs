//! Pipit Compliance — Regulatory Compliance Code Agents
//!
//! Task 9.1: Regulation document parser (GDPR, HIPAA, PCI-DSS).
//! Task 9.2: Taint analysis + compliance code generation.

pub mod codegen;
pub mod regulation;
pub mod taint;

pub use codegen::{ComplianceCodePlan, generate_compliance_code};
pub use regulation::{ComplianceRequirement, RegulationKind, RegulationParser};
pub use taint::{TaintAnalysis, TaintSink, TaintSource};
