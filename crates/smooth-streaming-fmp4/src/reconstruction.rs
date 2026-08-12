//! Canonical media reconstruction и exact same-clock admission.

use core::fmt;
use core::num::NonZeroU64;

use symphonia_format_isomp4::{
    FragmentCodedCoverage, FragmentCompositionOffsetSemantics, FragmentInspectionLimits,
    FragmentReconstructionRequest, FragmentWriteLimits, ReconstructedMediaSegment,
    reconstruct_media_fragment,
};

use crate::mapping::{SmoothMappedAudioFormat, SmoothMappedMediaState};
use crate::{
    SmoothFragmentPlan, SmoothFragmentReconstructionError, SmoothManifestWindow,
    SmoothTrackIdentity,
};

/// Полная exact relation coded coverage к manifest window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SmoothTimingRelation {
    /// Start и exclusive end совпадают.
    Exact,
    /// Coded end позже manifest end.
    Overhang(NonZeroU64),
    /// Coded end раньше manifest end.
    Underrun(NonZeroU64),
    /// Coded start отличается; этот класс всегда имеет приоритет.
    StartMismatch {
        /// Manifest start.
        expected_start: u64,
        /// Coded start.
        actual_start: u64,
    },
}

/// Причина, по которой audio fragment обязан пройти через exact presentation window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SmoothAudioPresentationWindowAdjustment {
    /// Coded interval длиннее manifest window и требует удаления хвостовых PCM frames.
    ClipOverhang(NonZeroU64),
    /// Coded interval короче менее чем на один PCM frame из-за округления stream clock.
    SubsampleUnderrun(NonZeroU64),
}

/// Полный request к F1 с обязательными inspection/write budgets.
pub struct SmoothFragmentReconstructionRequest<'input, 'plan, 'policy> {
    input: &'input [u8],
    plan: &'plan SmoothFragmentPlan,
    inspection_limits: &'policy FragmentInspectionLimits,
    write_limits: FragmentWriteLimits,
    cancellation: &'policy dyn Fn() -> bool,
}

impl<'input, 'plan, 'policy> SmoothFragmentReconstructionRequest<'input, 'plan, 'policy> {
    /// Создаёт request без budget defaults и downstream policy token-а.
    pub const fn new(
        input: &'input [u8],
        plan: &'plan SmoothFragmentPlan,
        inspection_limits: &'policy FragmentInspectionLimits,
        write_limits: FragmentWriteLimits,
        cancellation: &'policy dyn Fn() -> bool,
    ) -> Self {
        Self {
            input,
            plan,
            inspection_limits,
            write_limits,
            cancellation,
        }
    }
}

/// Fragment, полностью admitted для следующего слоя.
pub struct SmoothAdmittedFragment {
    identity: SmoothTrackIdentity,
    manifest_window: SmoothManifestWindow,
    coded_coverage: FragmentCodedCoverage,
    segment: ReconstructedMediaSegment,
}

impl SmoothAdmittedFragment {
    /// Возвращает track identity.
    pub const fn identity(&self) -> SmoothTrackIdentity {
        self.identity
    }

    /// Возвращает authoritative manifest window.
    pub const fn manifest_window(&self) -> SmoothManifestWindow {
        self.manifest_window
    }

    /// Возвращает exact F1 coded coverage.
    pub const fn coded_coverage(&self) -> FragmentCodedCoverage {
        self.coded_coverage
    }

    /// Даёт canonical reconstructed bytes следующему owner-у.
    pub fn media_segment_bytes(&self) -> &[u8] {
        self.segment.as_bytes()
    }

    /// Передаёт canonical bytes без копирования.
    pub fn into_media_segment_bytes(self) -> Vec<u8> {
        self.segment.into_bytes()
    }
}

impl fmt::Debug for SmoothAdmittedFragment {
    /// Не печатает media bytes.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SmoothAdmittedFragment")
            .field("identity", &self.identity)
            .field("manifest_window", &self.manifest_window)
            .field("coded_coverage", &self.coded_coverage)
            .field("byte_length", &self.segment.as_bytes().len())
            .finish()
    }
}

/// Exact proof, который F3 обязан ограничить authoritative presentation window-ом.
pub struct SmoothPendingAudioPresentationWindow {
    identity: SmoothTrackIdentity,
    manifest_window: SmoothManifestWindow,
    coded_coverage: FragmentCodedCoverage,
    adjustment: SmoothAudioPresentationWindowAdjustment,
    timescale_ticks_per_second: u32,
    sample_rate_hz: u32,
    channel_count: u16,
    segment: ReconstructedMediaSegment,
}

impl SmoothPendingAudioPresentationWindow {
    /// Собирает полный proof из sealed plan, manifest audio format и F1 evidence.
    fn new(
        plan: &SmoothFragmentPlan,
        coded_coverage: FragmentCodedCoverage,
        adjustment: SmoothAudioPresentationWindowAdjustment,
        audio_format: SmoothMappedAudioFormat,
        segment: ReconstructedMediaSegment,
    ) -> Self {
        Self {
            identity: plan.identity(),
            manifest_window: plan.manifest_window(),
            coded_coverage,
            adjustment,
            timescale_ticks_per_second: plan.manifest_window().timescale_ticks_per_second(),
            sample_rate_hz: audio_format.sample_rate_hz,
            channel_count: audio_format.channel_count,
            segment,
        }
    }

    /// Возвращает track identity.
    pub const fn identity(&self) -> SmoothTrackIdentity {
        self.identity
    }

    /// Возвращает exact manifest window.
    pub const fn manifest_window(&self) -> SmoothManifestWindow {
        self.manifest_window
    }

    /// Возвращает exact coded coverage.
    pub const fn coded_coverage(&self) -> FragmentCodedCoverage {
        self.coded_coverage
    }

    /// Возвращает доказанную причину bounded presentation window.
    pub const fn adjustment(&self) -> SmoothAudioPresentationWindowAdjustment {
        self.adjustment
    }

    /// Возвращает stream timescale для будущего F3 proof.
    pub const fn timescale_ticks_per_second(&self) -> u32 {
        self.timescale_ticks_per_second
    }

    /// Возвращает manifest AAC sample rate.
    pub const fn sample_rate_hz(&self) -> u32 {
        self.sample_rate_hz
    }

    /// Возвращает manifest AAC channel count.
    pub const fn channel_count(&self) -> u16 {
        self.channel_count
    }

    /// Даёт неизменённые reconstructed bytes F3 owner-у.
    pub fn unchanged_media_segment_bytes(&self) -> &[u8] {
        self.segment.as_bytes()
    }

    /// Передаёт неизменённые bytes без копирования.
    pub fn into_unchanged_media_segment_bytes(self) -> Vec<u8> {
        self.segment.into_bytes()
    }
}

impl fmt::Debug for SmoothPendingAudioPresentationWindow {
    /// Не печатает media bytes.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SmoothPendingAudioPresentationWindow")
            .field("identity", &self.identity)
            .field("manifest_window", &self.manifest_window)
            .field("coded_coverage", &self.coded_coverage)
            .field("adjustment", &self.adjustment)
            .field(
                "timescale_ticks_per_second",
                &self.timescale_ticks_per_second,
            )
            .field("sample_rate_hz", &self.sample_rate_hz)
            .field("channel_count", &self.channel_count)
            .field("byte_length", &self.segment.as_bytes().len())
            .finish()
    }
}

/// Только два состояния могут покинуть adapter после reconstruction.
#[derive(Debug)]
pub enum SmoothReconstructedFragment {
    /// Segment допускается к следующему слою.
    Admitted(SmoothAdmittedFragment),
    /// Audio bytes неизменны и требуют exact F3 presentation-window proof.
    PendingAudioPresentationWindow(SmoothPendingAudioPresentationWindow),
}

/// Реконструирует media fragment и применяет строгую Smooth admission matrix.
pub fn reconstruct_smooth_fragment(
    request: SmoothFragmentReconstructionRequest<'_, '_, '_>,
) -> Result<SmoothReconstructedFragment, SmoothFragmentReconstructionError> {
    if (request.cancellation)() {
        return Err(SmoothFragmentReconstructionError::Cancelled);
    }
    let f1_request = FragmentReconstructionRequest::new(
        request.input,
        FragmentCompositionOffsetSemantics::PiffSigned32Bit,
        request.plan.reconstruction_intent(),
        request.inspection_limits,
        request.write_limits,
        request.cancellation,
    );
    let segment = reconstruct_media_fragment(f1_request)
        .map_err(SmoothFragmentReconstructionError::from_f1)?;
    if (request.cancellation)() {
        return Err(SmoothFragmentReconstructionError::Cancelled);
    }
    let coverage = segment.coded_coverage();
    let relation = classify_timing(request.plan.manifest_window(), coverage);
    match (relation, request.plan.media_state()) {
        (SmoothTimingRelation::Exact, _) => Ok(SmoothReconstructedFragment::Admitted(
            SmoothAdmittedFragment {
                identity: request.plan.identity(),
                manifest_window: request.plan.manifest_window(),
                coded_coverage: coverage,
                segment,
            },
        )),
        (
            SmoothTimingRelation::StartMismatch {
                expected_start,
                actual_start,
            },
            _,
        ) => Err(SmoothFragmentReconstructionError::StartMismatch {
            expected_start,
            actual_start,
        }),
        (
            SmoothTimingRelation::Underrun(missing_ticks),
            SmoothMappedMediaState::Audio(audio_format),
        ) if audio_underrun_is_subsample(
            missing_ticks,
            request.plan.manifest_window().timescale_ticks_per_second(),
            audio_format.sample_rate_hz,
        ) =>
        {
            Ok(SmoothReconstructedFragment::PendingAudioPresentationWindow(
                SmoothPendingAudioPresentationWindow::new(
                    request.plan,
                    coverage,
                    SmoothAudioPresentationWindowAdjustment::SubsampleUnderrun(missing_ticks),
                    audio_format,
                    segment,
                ),
            ))
        }
        (SmoothTimingRelation::Underrun(missing_ticks), _) => {
            Err(SmoothFragmentReconstructionError::Underrun { missing_ticks })
        }
        (SmoothTimingRelation::Overhang(excess_ticks), SmoothMappedMediaState::Video) => {
            Err(SmoothFragmentReconstructionError::VideoOverhang { excess_ticks })
        }
        (
            SmoothTimingRelation::Overhang(excess_ticks),
            SmoothMappedMediaState::Audio(audio_format),
        ) => Ok(SmoothReconstructedFragment::PendingAudioPresentationWindow(
            SmoothPendingAudioPresentationWindow::new(
                request.plan,
                coverage,
                SmoothAudioPresentationWindowAdjustment::ClipOverhang(excess_ticks),
                audio_format,
                segment,
            ),
        )),
    }
}

/// Доказывает, что underrun не теряет ни одного полного PCM frame.
fn audio_underrun_is_subsample(
    missing_ticks: NonZeroU64,
    timescale_ticks_per_second: u32,
    sample_rate_hz: u32,
) -> bool {
    let missing_pcm_fraction_numerator =
        u128::from(missing_ticks.get()) * u128::from(sample_rate_hz);
    missing_pcm_fraction_numerator < u128::from(timescale_ticks_per_second)
}

/// Сравнивает два interval-а без float, rescale или tolerance.
fn classify_timing(
    manifest_window: SmoothManifestWindow,
    coded_coverage: FragmentCodedCoverage,
) -> SmoothTimingRelation {
    if coded_coverage.start() != manifest_window.start() {
        return SmoothTimingRelation::StartMismatch {
            expected_start: manifest_window.start(),
            actual_start: coded_coverage.start(),
        };
    }
    if coded_coverage.end_exclusive() == manifest_window.end_exclusive() {
        return SmoothTimingRelation::Exact;
    }
    if coded_coverage.end_exclusive() > manifest_window.end_exclusive() {
        let excess = coded_coverage.end_exclusive() - manifest_window.end_exclusive();
        return SmoothTimingRelation::Overhang(
            NonZeroU64::new(excess).expect("strict greater relation даёт ненулевой overhang"),
        );
    }
    let missing = manifest_window.end_exclusive() - coded_coverage.end_exclusive();
    SmoothTimingRelation::Underrun(
        NonZeroU64::new(missing).expect("strict lesser relation даёт ненулевой underrun"),
    )
}
