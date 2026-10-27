//! Перенастройка пула выходных VA surfaces под формат нового стрима.
//!
//! Владелец пула — [`crate::output_surface_pool`]; здесь только перевод stream info
//! adapter-а (формат, coded resolution, минимум кадров кодека) в его параметры,
//! логирование на lifecycle boundary `FormatChanged` и пересчёт бюджетов decoder-а,
//! которые зависят от действующей ёмкости пула.
//!
//! Ошибка перевода — фатальна для decoder-а: если пул останется со старым
//! форматом, следующая аренда отдаст под новый стрим surface прежнего размера, и
//! драйвер декодирует в чужую память. Молча продолжать здесь нельзя.

use std::fmt;
use std::num::NonZeroUsize;

use anyhow::Result;
use cros_codecs::Resolution;
use tracing::{info, warn};

use super::VaapiSurfaceLifecycleError;
use super::config::OUTPUT_SURFACE_EXTERNAL_HOLDERS_HEADROOM_FRAMES;
use super::lock_resource_pool;
use super::surface_contract::rt_format_for_decoded_format;
use crate::codec_adapter::{VaapiAdapterStreamInfo, VaapiDecodedFormat};
use crate::output_surface_pool::{
    OutputSurfaceAllocator, OutputSurfaceFormat, OutputSurfacePool, OutputSurfaceReconfigureReport,
    StreamSurfaceMinimum,
};
use crate::resource_pool::FrameResourcePool;
use crate::zero_copy_surface_pool::capacity::ZeroCopyCapacityChange;

/// Почему stream info нельзя перевести в параметры пула.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum OutputSurfaceStreamTargetError {
    /// Adapter прислал `FormatChanged`, но не знает параметров стрима.
    MissingStreamInfo,
    /// Decoded format стрима не имеет VA RT format в пуле.
    UnsupportedDecodedFormat {
        decoded_format: VaapiDecodedFormat,
        reason: String,
    },
}

impl fmt::Display for OutputSurfaceStreamTargetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingStreamInfo => {
                write!(formatter, "FormatChanged event without adapter stream info")
            }
            Self::UnsupportedDecodedFormat {
                decoded_format,
                reason,
            } => write!(
                formatter,
                "decoded format {decoded_format:?} has no VA RT format for the output surface pool: {reason}"
            ),
        }
    }
}

impl std::error::Error for OutputSurfaceStreamTargetError {}

/// Что стрим требует от пула: формат surfaces и минимум их числа.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct OutputSurfaceStreamTarget {
    /// Формат surfaces под coded resolution и битность стрима.
    pub(super) format: OutputSurfaceFormat,
    /// Минимум surfaces, ниже которого пул не опускается.
    pub(super) minimum: StreamSurfaceMinimum,
    /// Минимум самого кодека (`StreamInfo::min_num_frames`) — для логов.
    pub(super) codec_minimum_frames: usize,
}

/// Переводит stream info adapter-а в требования к пулу.
///
/// Surfaces выделяются под coded resolution (с выравниванием кодека), а не под
/// display resolution: декодер пишет полный coded кадр.
pub(super) fn output_surface_target_for_stream(
    stream_info: Option<&VaapiAdapterStreamInfo>,
) -> Result<OutputSurfaceStreamTarget, OutputSurfaceStreamTargetError> {
    let stream_info = stream_info.ok_or(OutputSurfaceStreamTargetError::MissingStreamInfo)?;
    let rt_format = rt_format_for_decoded_format(stream_info.format).map_err(|error| {
        OutputSurfaceStreamTargetError::UnsupportedDecodedFormat {
            decoded_format: stream_info.format,
            reason: format!("{error:#}"),
        }
    })?;
    let format = OutputSurfaceFormat {
        coded_resolution: Resolution::from((
            stream_info.coded_resolution.width,
            stream_info.coded_resolution.height,
        )),
        rt_format,
    };
    Ok(OutputSurfaceStreamTarget {
        format,
        minimum: stream_surface_minimum(stream_info.min_num_frames),
        codec_minimum_frames: stream_info.min_num_frames,
    })
}

/// Минимум стрима: минимум кодека плюс кадры, которые держат плеер и рендерер.
///
/// `min_num_frames == 0` означает, что кодек минимум не сообщил: тогда действует
/// только настройка пользователя (добавка без базы смысла не имеет).
fn stream_surface_minimum(codec_minimum_frames: usize) -> StreamSurfaceMinimum {
    NonZeroUsize::new(codec_minimum_frames)
        .and_then(|codec_minimum| {
            codec_minimum.checked_add(OUTPUT_SURFACE_EXTERNAL_HOLDERS_HEADROOM_FRAMES)
        })
        .map_or(StreamSurfaceMinimum::Unknown, StreamSurfaceMinimum::Frames)
}

/// Перенастраивает пул под стрим и пишет boundary-логи.
///
/// Пул не трогается, если stream info отсутствует или формат не переводится: это
/// ошибка вызывающего слоя, а не повод оставить пул в полурабочем состоянии.
pub(super) fn reconfigure_output_surface_pool_for_stream_info<A: OutputSurfaceAllocator>(
    pool: &OutputSurfacePool<A>,
    stream_info: Option<&VaapiAdapterStreamInfo>,
    configured_surface_pool_frames: usize,
) -> Result<OutputSurfaceReconfigureReport, OutputSurfaceStreamTargetError> {
    let target = output_surface_target_for_stream(stream_info)?;
    let report = pool.reconfigure(target.format, target.minimum);
    let pool_stats = pool.stats();
    if report.raised_to_stream_minimum {
        warn!(
            configured_surface_pool_frames,
            codec_minimum_frames = target.codec_minimum_frames,
            external_holders_headroom_frames = OUTPUT_SURFACE_EXTERNAL_HOLDERS_HEADROOM_FRAMES,
            capacity = report.capacity.get(),
            "Decoder surface pool setting is below what this stream needs; \
             raising pool capacity to the stream minimum"
        );
    }
    info!(
        width = target.format.coded_resolution.width,
        height = target.format.coded_resolution.height,
        rt_format = target.format.rt_format,
        capacity = report.capacity.get(),
        codec_minimum_frames = target.codec_minimum_frames,
        format_changed = report.format_changed,
        destroyed_free_surfaces = report.destroyed_free_surfaces,
        live_surfaces = pool_stats.live_surfaces,
        surfaces_created = pool_stats.surfaces_created,
        "Output surface pool reconfigured for stream"
    );
    Ok(report)
}

/// Согласует ёмкость import-слотов zero-copy с действующей ёмкостью пула surfaces.
///
/// Инвариант: каждая живая VA surface занимает максимум один слот, поэтому слотов
/// не может быть меньше `effective_surface_capacity`. Пул surfaces сам поднимается
/// до минимума стрима, а настройка слотов остаётся нижней границей. Владелец
/// инварианта — этот boundary (место, где известна действующая ёмкость): без него
/// `zero_copy_surface_pool_slots` меньше минимума стрима убивало декодер ошибкой
/// `PoolExhausted`, хотя утечки не было.
pub(super) fn follow_output_surface_capacity(
    resource_pool: &mut FrameResourcePool,
    effective_surface_capacity: NonZeroUsize,
) -> ZeroCopyCapacityChange {
    let change = resource_pool.follow_output_surface_capacity(effective_surface_capacity.get());
    if let ZeroCopyCapacityChange::Raised { previous, current } = change {
        warn!(
            configured_zero_copy_surface_pool_slots = previous,
            effective_zero_copy_surface_pool_slots = current,
            effective_surface_pool_frames = effective_surface_capacity.get(),
            "Zero-copy import slots are below the effective output surface pool capacity; \
             raising import slots to follow the pool"
        );
    }
    change
}

impl super::VaapiVideoDecoder {
    /// Переключает пул выходных surfaces на формат стрима после `FormatChanged`.
    ///
    /// Вызывается, когда decoder-owned кадры уже отпущены. Ошибка перевода
    /// stream info — фатальная ошибка lifecycle surfaces: decoder останавливается,
    /// а не продолжает с пулом прежнего формата.
    pub(super) fn reconfigure_output_surface_pool_for_stream(&mut self) -> Result<()> {
        let stream_info = self.adapter.stream_info();
        let report = reconfigure_output_surface_pool_for_stream_info(
            &self.output_surface_pool,
            stream_info.as_ref(),
            self.runtime_config.surface_pool_frames,
        )
        .map_err(|error| {
            anyhow::Error::new(VaapiSurfaceLifecycleError::new(format!(
                "cannot reconfigure the output surface pool after FormatChanged: {error}"
            )))
        })?;
        self.adopt_effective_surface_pool_capacity(report.capacity);
        // Слоты импорта обязаны покрывать все surfaces, которые пул может выдать.
        let mut resource_pool = lock_resource_pool(&self.resource_pool, "import slot capacity")?;
        follow_output_surface_capacity(&mut resource_pool, report.capacity);
        Ok(())
    }

    /// Пересчитывает бюджеты, зависящие от действующей ёмкости пула.
    ///
    /// Ёмкость могла подняться до минимума стрима или вернуться к настройке на
    /// следующем стриме; suppressed reclaim bound и его диагностика считаются от
    /// действующего числа surfaces, а не от настройки пользователя.
    fn adopt_effective_surface_pool_capacity(&mut self, effective_capacity: NonZeroUsize) {
        let effective_runtime_config = self
            .runtime_config
            .with_effective_surface_pool_frames(effective_capacity);
        let adoption = self
            .suppressed_reclaim_state
            .adopt_runtime_config(effective_runtime_config);
        if adoption.changed() {
            info!(
                configured_surface_pool_frames = self.runtime_config.surface_pool_frames,
                effective_surface_pool_frames = effective_capacity.get(),
                previous_max_suppressed_reclaim_frames = adoption.previous_bound,
                max_suppressed_reclaim_frames = adoption.current_bound,
                "Suppressed reclaim bound follows the effective output surface pool capacity"
            );
        }
    }
}
