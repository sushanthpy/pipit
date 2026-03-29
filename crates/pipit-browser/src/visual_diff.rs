//! Visual Regression Testing — before/after screenshot diff (BROWSER-2).
//!
//! Computes pixel-level diff between two screenshots using
//! Structural Similarity Index (SSIM) approximation.
//!
//! SSIM: 1.0 = identical, < 0.95 = significant change
//! Highlighted diff: count pixels where luminance delta exceeds JND threshold (2.3)

use crate::VisualDiff;

/// Compare two screenshots (as raw RGBA pixel buffers).
/// Returns a VisualDiff with SSIM score and change detection.
pub fn compare_screenshots(
    before: &[u8],
    after: &[u8],
    width: u32,
    height: u32,
) -> VisualDiff {
    let total_pixels = (width * height) as usize;
    if before.len() != after.len() || before.len() < total_pixels * 4 {
        return VisualDiff {
            ssim_score: 0.0,
            changed_pixels: total_pixels,
            total_pixels,
            significant: true,
        };
    }

    let mut changed = 0usize;
    let mut sum_sq_diff = 0.0f64;

    // Compare pixel by pixel (RGBA format, 4 bytes per pixel)
    for i in 0..total_pixels {
        let offset = i * 4;
        let lum_before = luminance(before[offset], before[offset + 1], before[offset + 2]);
        let lum_after = luminance(after[offset], after[offset + 1], after[offset + 2]);
        let delta = (lum_before - lum_after).abs();

        // JND (Just Noticeable Difference) threshold in perceptual space
        if delta > 2.3 {
            changed += 1;
        }
        sum_sq_diff += (delta * delta) as f64;
    }

    // Simplified SSIM approximation: 1 - normalized MSE
    let mse = sum_sq_diff / total_pixels as f64;
    let max_val = 255.0f64;
    let ssim = if mse < 0.001 {
        1.0
    } else {
        1.0 - (mse / (max_val * max_val)).min(1.0)
    };

    let significant = ssim < 0.95 || (changed as f64 / total_pixels as f64) > 0.05;

    VisualDiff {
        ssim_score: ssim,
        changed_pixels: changed,
        total_pixels,
        significant,
    }
}

/// Compute relative luminance from RGB (ITU BT.709).
fn luminance(r: u8, g: u8, b: u8) -> f32 {
    0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32
}

/// Generate a diff overlay image (RGBA buffer).
/// Changed pixels are highlighted in red; unchanged are dimmed.
pub fn generate_diff_overlay(
    before: &[u8],
    after: &[u8],
    width: u32,
    height: u32,
) -> Vec<u8> {
    let total_pixels = (width * height) as usize;
    let mut overlay = vec![0u8; total_pixels * 4];

    for i in 0..total_pixels {
        let offset = i * 4;
        if offset + 3 >= before.len() || offset + 3 >= after.len() {
            break;
        }

        let lum_b = luminance(before[offset], before[offset + 1], before[offset + 2]);
        let lum_a = luminance(after[offset], after[offset + 1], after[offset + 2]);
        let delta = (lum_b - lum_a).abs();

        if delta > 2.3 {
            // Red highlight for changed pixels
            overlay[offset] = 255;     // R
            overlay[offset + 1] = 50;  // G
            overlay[offset + 2] = 50;  // B
            overlay[offset + 3] = 180; // A
        } else {
            // Dim unchanged pixels
            overlay[offset] = after[offset] / 3;
            overlay[offset + 1] = after[offset + 1] / 3;
            overlay[offset + 2] = after[offset + 2] / 3;
            overlay[offset + 3] = 100;
        }
    }

    overlay
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_identical_images() {
        let pixels = vec![128u8; 4 * 100]; // 10x10 grey image
        let diff = compare_screenshots(&pixels, &pixels, 10, 10);
        assert_eq!(diff.ssim_score, 1.0);
        assert_eq!(diff.changed_pixels, 0);
        assert!(!diff.significant);
    }

    #[test]
    fn test_completely_different() {
        let before = vec![0u8; 4 * 100]; // black
        let after = vec![255u8; 4 * 100]; // white
        let diff = compare_screenshots(&before, &after, 10, 10);
        assert!(diff.ssim_score < 0.5);
        assert_eq!(diff.changed_pixels, 100);
        assert!(diff.significant);
    }

    #[test]
    fn test_small_change() {
        let mut before = vec![128u8; 4 * 100];
        let mut after = before.clone();
        // Change 2 pixels
        after[0] = 255;
        after[4] = 255;
        let diff = compare_screenshots(&before, &after, 10, 10);
        assert!(diff.ssim_score > 0.95);
        assert!(!diff.significant);
    }
}
