//! Pipit Compliance — Regulatory Compliance Code Agents
//!
//! Task 9.1: Regulation document parser (GDPR, HIPAA, PCI-DSS).
//! Task 9.2: Taint analysis + compliance code generation.

pub mod regulation;
pub mod taint;
pub mod codegen;

pub use regulation::{ComplianceRequirement, RegulationParser, RegulationKind};
pub use taint::{TaintAnalysis, TaintSource, TaintSink};
pub use codegen::{generate_compliance_code, ComplianceCodePlan};
