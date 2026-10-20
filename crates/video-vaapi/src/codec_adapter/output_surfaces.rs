//! Выдача выходных surfaces пула в cros-codecs `decode()` для всех codec adapters.

use cros_codecs::decoder::stateless::DecodeError;

use super::VaapiAdapterDecodeError;
use crate::internal_vaapi_frame::InternalVaapiFrame;
use crate::output_surface_pool::{
    OutputSurfaceAllocationError, OutputSurfaceLeaseOutcome, VaapiOutputSurfacePool,
};

/// Alloc-callback, который cros-codecs вызывает на каждую новую picture.
pub(super) type OutputFrameAllocator<'callback> =
    dyn FnMut() -> Option<InternalVaapiFrame> + 'callback;

/// Вызывает один cros `decode()` с выходными кадрами из ограниченного пула.
///
/// cros-codecs понимает только `Option`: `None` превращается в
/// `NotEnoughOutputBuffers`. Чтобы не слить два разных исхода в один, helper
/// запоминает ошибку драйвера и после `decode()` отдаёт её как терминальную
/// [`VaapiAdapterDecodeError::OutputSurfaceAllocation`]. Обычное исчерпание пула
/// остаётся `NotEnoughOutputBuffers` → backpressure и повтор того же packet-а.
pub(super) fn decode_with_pooled_output_surfaces(
    surface_pool: &VaapiOutputSurfacePool,
    decode: impl FnOnce(&mut OutputFrameAllocator<'_>) -> Result<usize, DecodeError>,
) -> Result<usize, VaapiAdapterDecodeError> {
    let mut allocation_failure: Option<OutputSurfaceAllocationError> = None;
    let mut lease_output_frame = || match surface_pool.try_lease() {
        OutputSurfaceLeaseOutcome::Leased(lease) => {
            Some(InternalVaapiFrame::from_leased_surface(lease))
        }
        OutputSurfaceLeaseOutcome::Exhausted => None,
        OutputSurfaceLeaseOutcome::AllocationFailed(error) => {
            allocation_failure.get_or_insert(error);
            None
        }
    };
    let decode_result = decode(&mut lease_output_frame);
    classify_decode_result(decode_result, allocation_failure)
}

/// Переводит результат `decode()` в adapter error с учётом ошибки аллокации.
fn classify_decode_result(
    decode_result: Result<usize, DecodeError>,
    allocation_failure: Option<OutputSurfaceAllocationError>,
) -> Result<usize, VaapiAdapterDecodeError> {
    match (decode_result, allocation_failure) {
        (Err(DecodeError::NotEnoughOutputBuffers(_)), Some(error)) => Err(
            VaapiAdapterDecodeError::OutputSurfaceAllocation(error.to_string()),
        ),
        (decode_result, _) => decode_result.map_err(VaapiAdapterDecodeError::from),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output_surface_pool::OutputSurfaceFormat;
    use cros_codecs::Resolution;
    use cros_codecs::libva::VA_RT_FORMAT_YUV420;

    fn allocation_error() -> OutputSurfaceAllocationError {
        OutputSurfaceAllocationError {
            format: OutputSurfaceFormat {
                coded_resolution: Resolution::from((64, 64)),
                rt_format: VA_RT_FORMAT_YUV420,
            },
            message: "out of video memory".to_string(),
        }
    }

    #[test]
    fn exhausted_pool_stays_retryable_backpressure() {
        let result = classify_decode_result(Err(DecodeError::NotEnoughOutputBuffers(1)), None);

        assert!(matches!(
            result,
            Err(VaapiAdapterDecodeError::NotEnoughOutputBuffers(1))
        ));
    }

    #[test]
    fn driver_allocation_failure_is_terminal_not_backpressure() {
        let result = classify_decode_result(
            Err(DecodeError::NotEnoughOutputBuffers(1)),
            Some(allocation_error()),
        );

        let Err(VaapiAdapterDecodeError::OutputSurfaceAllocation(message)) = result else {
            panic!("allocation failure must not be reported as backpressure: {result:?}");
        };
        assert!(message.contains("out of video memory"), "{message}");
    }

    #[test]
    fn accepted_packet_ignores_unrelated_allocation_state() {
        let result = classify_decode_result(Ok(42), Some(allocation_error()));

        assert!(matches!(result, Ok(42)));
    }
}
