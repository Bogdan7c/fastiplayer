//! Узкий orchestration boundary fragment inspection-а.

use super::budget::InspectionBudget;
use super::error::{FragmentInspectionError, FragmentInspectionLimitKind};
use super::model::{FragmentInspectionRequest, NormalizedFragmentPlan};
use super::normalize::normalize_samples;
use super::parse::parse_top_level;
use super::support::{check_cancelled, enforce_limit};

/// Проверяет Smooth/PIFF media fragment и строит canonical normalized plan.
pub(crate) fn inspect_media_fragment<'input>(
    request: &FragmentInspectionRequest<'input, '_>,
) -> Result<NormalizedFragmentPlan<'input>, FragmentInspectionError> {
    check_cancelled(request)?;
    enforce_limit(
        FragmentInspectionLimitKind::InputBytes,
        request.limits().max_input_bytes(),
        request.input().len(),
    )?;

    let mut budget = InspectionBudget::new(request.limits());
    let parsed_fragment = parse_top_level(request, &mut budget)?;
    let plan = normalize_samples(request, parsed_fragment)?;

    check_cancelled(request)?;
    Ok(plan)
}
