//! Animation engine: shimmer, stalled detection, reduced-motion awareness.

pub mod accessibility;
pub mod shimmer;
pub mod stalled;

pub use accessibility::AccessibilityMode;
pub use shimmer::ShimmerEngine;
pub use stalled::StalledDetector;
