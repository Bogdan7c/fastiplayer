//! Показ аппаратного кадра: порядок «идентичность → кэш → (промах) дескриптор → импорт».
//!
//! Порядок тот же, что у эталона wgpu (`dma_buf_materializer`), и он нарочно экономный:
//! 1. **Идентичность** без dup fd (`try_resource_identity_lookup`): Busy/Missing/Fatal
//!    завершают показ сразу, `Unsupported` значит «провайдер идентичность не отдаёт».
//! 2. **Кэш** видеослоя по паспорту: попадание не требует ни dup fd, ни GL-вызовов, содержимое
//!    буфера видно при рисовании (контракт [`gles2_video::DmaBufSharingContract`]).
//! 3. Только на промахе — **дескриптор** с dup fd (`try_resource_descriptor_lookup`),
//!    проверка контракта кадра, разбор слоёв и **импорт**. Если идентичности не было, паспорт
//!    снимается с дескриптора и кэш спрашивается ещё раз (как у wgpu).
//!
//! Проверка контракта кадра по идентичности выполняется ДО показа из кэша: показ делает кадр
//! текущим (побочный эффект), а кадр с чужим размером или раскладкой показывать нельзя.
//!
//! # Где закрываются fd
//! Дескриптор (и его dup fd) живёт в локальной переменной до конца функции и закрывается ПОСЛЕ
//! импорта: EGL fd не забирает (`EGL_EXT_image_dma_buf_import`, issue 3), текстура держит буфер
//! сама. Закрытие fd не освобождает кадр декодеру: это делает `Gles2FrameReleaseBinding` после
//! fence кадра показа.
//!
//! Выполнение вынесено за узкий шов [`DmaBufShowTarget`], чтобы порядок проверялся без GL.

use std::time::Duration;

use gles2_egui::Gles2Context;
use gles2_video::{
    CachedDmaBufFrame, DmaBufBufferIdentity, DmaBufImportCacheCapacity, DmaBufImportOutcome,
    DmaBufNv12Frame, VideoFrameKey, VideoImportError, VideoLayerRenderer,
};
use video_backend_api::{
    PresentFrameResourceDescriptorLookup, PresentFrameResourceIdentityLookup,
    PresentFrameResourceProviderHandle,
};
use video_core::{
    DecodedFrame, FrameResourceDescriptor, FrameResourceIdentity,
    validate_dma_buf_descriptor_against_frame_contract,
    validate_dma_buf_identity_against_frame_contract,
};

use super::error::Nv12DmaBufError;
use super::frame::{dma_buf_frame_key, ensure_nv12_frame_contract, nv12_dma_buf_frame};
use super::outcome::{
    DmaBufMaterialization, DmaBufMaterializeOutcome, DmaBufShownFrom, DmaBufUnsupportedReason,
};
use super::passport::dma_buf_buffer_identity;

/// Ёмкость кэша импортов для пула из `surface_pool_slots` поверхностей декодера.
///
/// Кэш должен быть ровно размером пула: меньше — LRU на круговом пуле промахивается всегда
/// (импорт на каждый кадр), больше — держит текстуры буферов, которых у декодера уже нет.
/// Это то же правило, что `DmaBufImportCacheCapacity::matching_surface_pool` у wgpu. Ноль
/// поднимается до одного. Применять: `VideoLayerRenderer::set_dma_buf_import_capacity`.
#[must_use]
pub const fn dma_buf_import_capacity_for(surface_pool_slots: usize) -> DmaBufImportCacheCapacity {
    DmaBufImportCacheCapacity::matching_decoder_surface_pool(surface_pool_slots)
}

/// Шов к видеослою: то, что материализатору нужно от рендера (и что подменяют тесты).
pub(super) trait DmaBufShowTarget {
    /// Показать кадр по паспорту без дескрипторов.
    fn show_cached(
        &mut self,
        passport: &DmaBufBufferIdentity,
        key: VideoFrameKey,
    ) -> CachedDmaBufFrame;
    /// Импортировать буфер и показать кадр.
    fn import(
        &mut self,
        frame: &DmaBufNv12Frame<'_>,
    ) -> Result<DmaBufImportOutcome, VideoImportError>;
}

/// Настоящий видеослой с его контекстом.
struct RendererTarget<'a> {
    renderer: &'a mut VideoLayerRenderer,
    context: &'a Gles2Context,
}

impl DmaBufShowTarget for RendererTarget<'_> {
    fn show_cached(
        &mut self,
        passport: &DmaBufBufferIdentity,
        key: VideoFrameKey,
    ) -> CachedDmaBufFrame {
        self.renderer.show_cached_dma_buf_frame(passport, key)
    }

    fn import(
        &mut self,
        frame: &DmaBufNv12Frame<'_>,
    ) -> Result<DmaBufImportOutcome, VideoImportError> {
        self.renderer.import_dma_buf_frame(self.context, frame)
    }
}

/// Сделать аппаратный кадр NV12 текущим в видеослое без копии.
///
/// `provider` — провайдер ресурсов декодера (в плеере — обёрнутый
/// `wrap_video_backend_for_gles2_release`, чтобы освобождение шло по fence). Порядок и
/// исходы — в документации модуля и [`DmaBufMaterializeOutcome`].
#[must_use]
pub fn materialize_dma_buf_frame(
    renderer: &mut VideoLayerRenderer,
    context: &Gles2Context,
    provider: &PresentFrameResourceProviderHandle,
    frame: &DecodedFrame,
) -> DmaBufMaterialization {
    let mut target = RendererTarget { renderer, context };
    materialize_with_target(&mut target, provider, frame)
}

/// Тело [`materialize_dma_buf_frame`] над швом [`DmaBufShowTarget`].
pub(super) fn materialize_with_target(
    target: &mut impl DmaBufShowTarget,
    provider: &PresentFrameResourceProviderHandle,
    frame: &DecodedFrame,
) -> DmaBufMaterialization {
    let mut resource_pool_lock_wait = Duration::ZERO;
    let outcome = run_materialization(target, provider, frame, &mut resource_pool_lock_wait);
    DmaBufMaterialization {
        outcome,
        resource_pool_lock_wait,
    }
}

fn run_materialization(
    target: &mut impl DmaBufShowTarget,
    provider: &PresentFrameResourceProviderHandle,
    frame: &DecodedFrame,
    lock_wait: &mut Duration,
) -> DmaBufMaterializeOutcome {
    // Формат контракта известен без провайдера: P010 и не-NV12 отвергаем, не трогая его.
    if let Err(error) = ensure_nv12_frame_contract(frame) {
        return unsupported_format(error);
    }
    let key = dma_buf_frame_key(frame);

    // Шаг 1: дешёвая идентичность без dup fd.
    let identity_lookup = provider.try_resource_identity_lookup(frame.resource_handle);
    *lock_wait = lock_wait.saturating_add(identity_lookup.resource_pool_lock_wait());
    let known_passport = match identity_lookup {
        PresentFrameResourceIdentityLookup::Ready {
            identity: FrameResourceIdentity::DmaBuf(identity),
            ..
        } => {
            if let Err(rejection) = validate_dma_buf_identity_against_frame_contract(
                frame.frame_contract,
                frame.width,
                frame.height,
                &identity,
            ) {
                return DmaBufMaterializeOutcome::Unsupported(
                    DmaBufUnsupportedReason::DescriptorRejected(rejection),
                );
            }
            match dma_buf_buffer_identity(&identity) {
                Ok(passport) => Some(passport),
                Err(error) => return unsupported_format(error),
            }
        }
        PresentFrameResourceIdentityLookup::Unsupported => None,
        PresentFrameResourceIdentityLookup::Busy { .. } => return DmaBufMaterializeOutcome::Busy,
        PresentFrameResourceIdentityLookup::Missing { .. } => {
            return DmaBufMaterializeOutcome::Missing;
        }
        PresentFrameResourceIdentityLookup::Fatal { .. } => {
            return DmaBufMaterializeOutcome::ProviderFatal;
        }
    };

    // Шаг 2: кэш видеослоя по паспорту — без дескриптора.
    if let Some(passport) = &known_passport
        && let Some(shown) = shown_from_cache(target, passport, key)
    {
        return DmaBufMaterializeOutcome::Shown(shown);
    }

    // Шаг 3: промах — дескриптор с dup fd. Он живёт до конца функции (см. «Где закрываются fd»).
    let descriptor_lookup = provider.try_resource_descriptor_lookup(frame.resource_handle);
    *lock_wait = lock_wait.saturating_add(descriptor_lookup.resource_pool_lock_wait());
    let descriptor = match descriptor_lookup {
        PresentFrameResourceDescriptorLookup::Ready { descriptor, .. } => descriptor,
        PresentFrameResourceDescriptorLookup::Busy { .. } => return DmaBufMaterializeOutcome::Busy,
        PresentFrameResourceDescriptorLookup::Missing { .. } => {
            return DmaBufMaterializeOutcome::Missing;
        }
        PresentFrameResourceDescriptorLookup::Fatal { .. } => {
            return DmaBufMaterializeOutcome::ProviderFatal;
        }
    };
    let dma_buf_descriptor = match &descriptor {
        FrameResourceDescriptor::DmaBuf(dma_buf_descriptor) => dma_buf_descriptor,
        FrameResourceDescriptor::HostPlanar(_) => {
            return DmaBufMaterializeOutcome::Unsupported(
                DmaBufUnsupportedReason::HostPlanarDescriptor,
            );
        }
    };
    if let Err(rejection) = validate_dma_buf_descriptor_against_frame_contract(
        frame.frame_contract,
        frame.width,
        frame.height,
        dma_buf_descriptor,
    ) {
        return DmaBufMaterializeOutcome::Unsupported(DmaBufUnsupportedReason::DescriptorRejected(
            rejection,
        ));
    }

    // Провайдер без идентичности: паспорт снимается с дескриптора, кэш спрашивается теперь.
    let passport = match known_passport {
        Some(passport) => passport,
        None => match dma_buf_buffer_identity(&dma_buf_descriptor.identity()) {
            Ok(passport) => {
                if let Some(shown) = shown_from_cache(target, &passport, key) {
                    return DmaBufMaterializeOutcome::Shown(shown);
                }
                passport
            }
            Err(error) => return unsupported_format(error),
        },
    };

    let nv12_frame = match nv12_dma_buf_frame(frame, dma_buf_descriptor, &passport) {
        Ok(nv12_frame) => nv12_frame,
        Err(error) => return unsupported_format(error),
    };
    match target.import(&nv12_frame) {
        Ok(DmaBufImportOutcome::Imported(report)) => {
            DmaBufMaterializeOutcome::Shown(DmaBufShownFrom::FreshImport(report))
        }
        Ok(DmaBufImportOutcome::ReusedCachedImport) => {
            DmaBufMaterializeOutcome::Shown(DmaBufShownFrom::CacheHit)
        }
        Ok(DmaBufImportOutcome::AlreadyCurrent) => {
            DmaBufMaterializeOutcome::Shown(DmaBufShownFrom::AlreadyCurrent)
        }
        Err(error) => DmaBufMaterializeOutcome::from_import_error(error),
    }
    // `descriptor` (а с ним dup fd) закрывается здесь — после импорта.
}

/// Попытка показать кадр из кэша; `None` — буфера в кэше нет.
fn shown_from_cache(
    target: &mut impl DmaBufShowTarget,
    passport: &DmaBufBufferIdentity,
    key: VideoFrameKey,
) -> Option<DmaBufShownFrom> {
    match target.show_cached(passport, key) {
        CachedDmaBufFrame::Shown => Some(DmaBufShownFrom::CacheHit),
        CachedDmaBufFrame::AlreadyCurrent => Some(DmaBufShownFrom::AlreadyCurrent),
        CachedDmaBufFrame::NotCached => None,
    }
}

fn unsupported_format(error: Nv12DmaBufError) -> DmaBufMaterializeOutcome {
    DmaBufMaterializeOutcome::Unsupported(DmaBufUnsupportedReason::FrameFormat(error))
}
