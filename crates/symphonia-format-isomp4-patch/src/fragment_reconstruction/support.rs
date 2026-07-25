//! Общие checked helpers без знания ISO box layout.

use super::error::{
    FragmentArithmeticOperation, FragmentInspectionError, FragmentInspectionLimitKind,
};
use super::model::FragmentInspectionRequest;

/// Проверяет injected cancellation.
pub(super) fn check_cancelled(
    request: &FragmentInspectionRequest<'_, '_>,
) -> Result<(), FragmentInspectionError> {
    if request.is_cancelled() {
        Err(FragmentInspectionError::Cancelled)
    } else {
        Ok(())
    }
}

/// Проверяет usize addition.
pub(super) fn checked_add(
    left: usize,
    right: usize,
    operation: FragmentArithmeticOperation,
) -> Result<usize, FragmentInspectionError> {
    left.checked_add(right)
        .ok_or(FragmentInspectionError::ArithmeticOverflow { operation })
}

/// Проверяет usize multiplication.
pub(super) fn checked_multiply(
    left: usize,
    right: usize,
    operation: FragmentArithmeticOperation,
) -> Result<usize, FragmentInspectionError> {
    left.checked_mul(right)
        .ok_or(FragmentInspectionError::ArithmeticOverflow { operation })
}

/// Применяет обязательный budget.
pub(super) fn enforce_limit(
    kind: FragmentInspectionLimitKind,
    limit: usize,
    observed: usize,
) -> Result<(), FragmentInspectionError> {
    if observed > limit {
        Err(FragmentInspectionError::LimitExceeded {
            kind,
            limit: limit as u64,
            observed: observed as u64,
        })
    } else {
        Ok(())
    }
}
