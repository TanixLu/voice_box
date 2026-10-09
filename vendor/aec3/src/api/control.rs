//! Echo control API definitions ported from `api/echo_control.h`.

/// Metrics exposed by an echo controller.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metrics {
    pub echo_return_loss: f64,
    pub echo_return_loss_enhancement: f64,
    pub delay_ms: i32,
    /// Minimum number of consecutive render calls between capture calls.
    pub render_jitter_min: i32,
    /// Maximum number of consecutive render calls between capture calls.
    pub render_jitter_max: i32,
    /// Minimum number of consecutive capture calls between render calls.
    pub capture_jitter_min: i32,
    /// Maximum number of consecutive capture calls between render calls.
    pub capture_jitter_max: i32,
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            echo_return_loss: 0.0,
            echo_return_loss_enhancement: 0.0,
            delay_ms: 0,
            render_jitter_min: 0,
            render_jitter_max: 0,
            capture_jitter_min: 0,
            capture_jitter_max: 0,
        }
    }
}
