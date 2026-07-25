//! Mandatory F1 budgets fragment reconstruction.

use symphonia_format_isomp4::{FragmentInspectionLimits, FragmentWriteLimits};

/// Caller-owned F1 inspection/write policy без production defaults.
#[derive(Clone, Debug)]
pub struct SmoothFragmentSourcePolicy {
    pub(crate) inspection_limits: FragmentInspectionLimits,
    pub(crate) write_limits: FragmentWriteLimits,
}

impl SmoothFragmentSourcePolicy {
    /// Собирает полную policy из двух independently validated limits.
    #[must_use]
    pub const fn new(
        inspection_limits: FragmentInspectionLimits,
        write_limits: FragmentWriteLimits,
    ) -> Self {
        Self {
            inspection_limits,
            write_limits,
        }
    }
}
