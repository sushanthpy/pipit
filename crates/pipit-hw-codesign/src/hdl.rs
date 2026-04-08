//! HDL Generation + Validation — Task HW-1
//!
//! Generate Verilog/VHDL from specs, validate via iverilog.
//! Syntax check: O(n) in file size, <1 second for 10K lines.
//! Simulation: O(n·t) where t = simulation steps.

use serde::{Deserialize, Serialize};
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HdlLanguage {
    Verilog,
    Vhdl,
    SystemVerilog,
}

/// An HDL module description.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HdlModule {
    pub name: String,
    pub language: HdlLanguage,
    pub ports: Vec<HdlPort>,
    pub source: String,
    pub testbench: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HdlPort {
    pub name: String,
    pub direction: PortDirection,
    pub width: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum PortDirection {
    Input,
    Output,
    Inout,
}

/// Validation result from iverilog.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationResult {
    pub valid: bool,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    pub simulator_available: bool,
}

/// Validate Verilog source via iverilog. O(n) in source size.
pub fn validate_verilog(source: &str) -> ValidationResult {
    let tmp = std::env::temp_dir().join("pipit-hdl-check.v");
    if std::fs::write(&tmp, source).is_err() {
        return ValidationResult {
            valid: false,
            errors: vec!["Failed to write temp file".into()],
            warnings: vec![],
            simulator_available: false,
        };
    }

    let output = Command::new("iverilog")
        .args(["-g2012", "-o", "/dev/null"])
        .arg(&tmp)
        .output();

    let _ = std::fs::remove_file(&tmp);

    match output {
        Ok(result) => {
            let stderr = String::from_utf8_lossy(&result.stderr);
            let errors: Vec<String> = stderr
                .lines()
                .filter(|l| l.contains("error"))
                .map(String::from)
                .collect();
            let warnings: Vec<String> = stderr
                .lines()
                .filter(|l| l.contains("warning"))
                .map(String::from)
                .collect();

            ValidationResult {
                valid: result.status.success(),
                errors,
                warnings,
                simulator_available: true,
            }
        }
        Err(_) => ValidationResult {
            valid: false,
            errors: vec![],
            warnings: vec![],
            simulator_available: false,
        },
    }
}

/// Generate a Verilog module template from port specification.
pub fn generate_verilog_template(module: &HdlModule) -> String {
    let mut v = format!("module {} (\n", module.name);
    for (i, port) in module.ports.iter().enumerate() {
        let dir = match port.direction {
            PortDirection::Input => "input",
            PortDirection::Output => "output",
            PortDirection::Inout => "inout",
        };
        let width = if port.width > 1 {
            format!(" [{}: 0]", port.width - 1)
        } else {
            String::new()
        };
        let comma = if i < module.ports.len() - 1 { "," } else { "" };
        v.push_str(&format!("    {}{} {}{}\n", dir, width, port.name, comma));
    }
    v.push_str(");\n\n    // TODO: Implementation\n\nendmodule\n");
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_template_generation() {
        let module = HdlModule {
            name: "adder".into(),
            language: HdlLanguage::Verilog,
            ports: vec![
                HdlPort {
                    name: "a".into(),
                    direction: PortDirection::Input,
                    width: 8,
                },
                HdlPort {
                    name: "b".into(),
                    direction: PortDirection::Input,
                    width: 8,
                },
                HdlPort {
                    name: "sum".into(),
                    direction: PortDirection::Output,
                    width: 9,
                },
            ],
            source: String::new(),
            testbench: None,
        };
        let v = generate_verilog_template(&module);
        assert!(v.contains("module adder"));
        assert!(v.contains("input [7: 0] a"));
        assert!(v.contains("output [8: 0] sum"));
        assert!(v.contains("endmodule"));
    }
}
