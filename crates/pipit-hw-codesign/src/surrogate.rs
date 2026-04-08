//! Synthesis Surrogate Model — Task HW-2
//!
//! **STATUS: PLACEHOLDER / NOT ML-TRAINED**
//!
//! Estimates FPGA synthesis outcomes from Verilog AST features using
//! hardcoded linear weights. This is a development placeholder — the
//! predictions are rough heuristics, not trained model outputs.
//!
//! For production use, this should be replaced with a gradient-boosted
//! tree trained on actual synthesis data. The API shape (SynthesisFeatures
//! → SynthesisPrediction) is stable and will be preserved.
//!
//! Prediction: O(d) = O(15). No training step.

use serde::{Deserialize, Serialize};

/// Features extracted from Verilog for synthesis prediction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SynthesisFeatures {
    pub module_count: u32,
    pub hierarchy_depth: u32,
    pub total_signals: u32,
    pub max_fan_out: u32,
    pub adder_count: u32,
    pub multiplier_count: u32,
    pub mux_width_max: u32,
    pub register_count: u32,
    pub fsm_state_count: u32,
    pub memory_depth: u32,
    pub total_lines: u32,
    pub always_block_count: u32,
    pub generate_block_count: u32,
    pub parameter_count: u32,
    pub instantiation_count: u32,
}

/// Predicted synthesis results.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SynthesisPrediction {
    pub max_frequency_mhz: f64,
    pub lut_utilization: f64,
    pub bram_blocks: u32,
    pub estimated_power_mw: f64,
    pub confidence: f64,
}

/// Simple surrogate model using weighted linear combination of features.
/// (In production, this would be a GBT trained on synthesis data.)
pub struct SurrogateModel {
    feature_weights: Vec<f64>,
}

impl SurrogateModel {
    /// Create a model with empirically-derived default weights.
    pub fn default_model() -> Self {
        Self {
            feature_weights: vec![
                -2.0,  // module_count (more modules → lower freq due to cross-module paths)
                -5.0,  // hierarchy_depth
                -0.01, // total_signals
                -0.5,  // max_fan_out
                -3.0,  // adder_count
                -10.0, // multiplier_count (multipliers are expensive)
                -1.0,  // mux_width_max
                0.1,   // register_count (pipelining helps frequency)
                -2.0,  // fsm_state_count
                -0.5,  // memory_depth
                0.0,   // total_lines (not predictive)
                -1.0,  // always_block_count
                -0.5,  // generate_block_count
                0.0,   // parameter_count
                -1.5,  // instantiation_count
            ],
        }
    }

    /// Predict synthesis outcomes from features. O(d) = O(15).
    pub fn predict(&self, features: &SynthesisFeatures) -> SynthesisPrediction {
        let feat_vec = features.to_vec();
        let dot: f64 = self
            .feature_weights
            .iter()
            .zip(&feat_vec)
            .map(|(w, f)| w * *f)
            .sum();

        // Base frequency 500MHz + weighted feature contribution
        let freq = (500.0 + dot).max(10.0).min(1000.0);

        // LUT estimation: roughly proportional to combinational elements
        let luts = (features.adder_count as f64 * 8.0
            + features.multiplier_count as f64 * 100.0
            + features.mux_width_max as f64 * features.total_signals as f64 * 0.1
            + features.total_signals as f64 * 0.5)
            .ceil() as f64;
        let lut_util = (luts / 50000.0).min(1.0); // Assume 50K LUT device

        // BRAM estimation
        let bram = if features.memory_depth > 0 {
            ((features.memory_depth as f64 * 32.0) / 36864.0).ceil() as u32 // 36Kbit BRAMs
        } else {
            0
        };

        // Power: roughly proportional to frequency × utilization
        let power = freq * lut_util * 0.5;

        // Confidence: lower for complex designs
        let complexity = feat_vec.iter().sum::<f64>();
        let confidence = (1.0 - complexity / 10000.0).max(0.1).min(0.95);

        SynthesisPrediction {
            max_frequency_mhz: freq,
            lut_utilization: lut_util,
            bram_blocks: bram,
            estimated_power_mw: power,
            confidence,
        }
    }

    /// Rank design variants by predicted frequency (for evolutionary selection).
    pub fn rank_variants(&self, variants: &[SynthesisFeatures]) -> Vec<(usize, f64)> {
        let mut ranked: Vec<(usize, f64)> = variants
            .iter()
            .enumerate()
            .map(|(i, f)| (i, self.predict(f).max_frequency_mhz))
            .collect();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        ranked
    }
}

impl SynthesisFeatures {
    pub fn to_vec(&self) -> Vec<f64> {
        vec![
            self.module_count as f64,
            self.hierarchy_depth as f64,
            self.total_signals as f64,
            self.max_fan_out as f64,
            self.adder_count as f64,
            self.multiplier_count as f64,
            self.mux_width_max as f64,
            self.register_count as f64,
            self.fsm_state_count as f64,
            self.memory_depth as f64,
            self.total_lines as f64,
            self.always_block_count as f64,
            self.generate_block_count as f64,
            self.parameter_count as f64,
            self.instantiation_count as f64,
        ]
    }

    /// Extract features from Verilog source (pattern-based).
    pub fn from_verilog(source: &str) -> Self {
        Self {
            module_count: source.matches("module ").count() as u32,
            hierarchy_depth: 1,
            total_signals: (source.matches("wire ").count()
                + source.matches("reg ").count()
                + source.matches("logic ").count()) as u32,
            max_fan_out: 4,
            adder_count: source.matches(" + ").count() as u32,
            multiplier_count: source.matches(" * ").count() as u32,
            mux_width_max: source.matches("case").count().max(1) as u32,
            register_count: source.matches("reg ").count() as u32,
            fsm_state_count: source.matches("state").count() as u32,
            memory_depth: 0,
            total_lines: source.lines().count() as u32,
            always_block_count: source.matches("always").count() as u32,
            generate_block_count: source.matches("generate").count() as u32,
            parameter_count: source.matches("parameter").count() as u32,
            instantiation_count: source
                .matches("(")
                .count()
                .saturating_sub(source.matches("module").count())
                as u32,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_prediction_reasonable() {
        let model = SurrogateModel::default_model();
        let simple = SynthesisFeatures {
            module_count: 1,
            hierarchy_depth: 1,
            total_signals: 10,
            max_fan_out: 4,
            adder_count: 2,
            multiplier_count: 0,
            mux_width_max: 2,
            register_count: 5,
            fsm_state_count: 0,
            memory_depth: 0,
            total_lines: 50,
            always_block_count: 2,
            generate_block_count: 0,
            parameter_count: 1,
            instantiation_count: 0,
        };
        let pred = model.predict(&simple);
        assert!(
            pred.max_frequency_mhz > 100.0 && pred.max_frequency_mhz < 600.0,
            "Simple design freq: {}",
            pred.max_frequency_mhz
        );
        assert!(
            pred.lut_utilization < 0.5,
            "Simple design util: {}",
            pred.lut_utilization
        );
    }

    #[test]
    fn test_complex_design_slower() {
        let model = SurrogateModel::default_model();
        let simple = SynthesisFeatures {
            module_count: 1,
            hierarchy_depth: 1,
            total_signals: 10,
            max_fan_out: 2,
            adder_count: 1,
            multiplier_count: 0,
            mux_width_max: 1,
            register_count: 5,
            fsm_state_count: 0,
            memory_depth: 0,
            total_lines: 20,
            always_block_count: 1,
            generate_block_count: 0,
            parameter_count: 0,
            instantiation_count: 0,
        };
        let complex = SynthesisFeatures {
            module_count: 10,
            hierarchy_depth: 5,
            total_signals: 500,
            max_fan_out: 32,
            adder_count: 20,
            multiplier_count: 8,
            mux_width_max: 16,
            register_count: 200,
            fsm_state_count: 12,
            memory_depth: 1024,
            total_lines: 2000,
            always_block_count: 50,
            generate_block_count: 5,
            parameter_count: 10,
            instantiation_count: 30,
        };
        let simple_pred = model.predict(&simple);
        let complex_pred = model.predict(&complex);
        assert!(
            simple_pred.max_frequency_mhz > complex_pred.max_frequency_mhz,
            "Simple {} > Complex {}",
            simple_pred.max_frequency_mhz,
            complex_pred.max_frequency_mhz
        );
    }

    #[test]
    fn test_feature_extraction() {
        let src = "module adder(input [7:0] a, input [7:0] b, output reg [8:0] sum);\n  always @(*) sum = a + b;\nendmodule";
        let features = SynthesisFeatures::from_verilog(src);
        assert_eq!(features.module_count, 1);
        assert!(features.adder_count >= 1);
        assert!(features.register_count >= 1);
    }
}
