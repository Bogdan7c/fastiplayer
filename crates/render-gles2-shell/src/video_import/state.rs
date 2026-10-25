//! Состояние склада: активный импорт, эпоха подключения декодера и учёт «что на видеослое».

use gles2_video::{VideoFrameKey, VideoLayerRenderer};
use render_shell_api::VideoImportPlan;

use super::prepared::FrameSource;

/// Номер подключения декодера (растёт при каждой установке импорта).
///
/// Номера ресурсов у нового декодера начинаются заново, поэтому «тот же ключ кадра» значит то же
/// самое только внутри одной эпохи.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct ImportEpoch(u64);

impl ImportEpoch {
    /// Эпоха до первого подключения декодера.
    pub(crate) const INITIAL: Self = Self(0);

    /// Следующая эпоха (переполнение `u64` недостижимо, но насыщение безопаснее обёртки).
    pub(crate) const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

/// Установленный импорт: план, источник ресурсов и эпоха.
#[derive(Clone)]
pub(crate) struct ActiveImport {
    plan: VideoImportPlan,
    source: FrameSource,
    epoch: ImportEpoch,
}

impl ActiveImport {
    /// Импорт по `plan` из `source`, установленный в эпоху `epoch`.
    pub(crate) const fn new(
        plan: VideoImportPlan,
        source: FrameSource,
        epoch: ImportEpoch,
    ) -> Self {
        Self {
            plan,
            source,
            epoch,
        }
    }

    /// План импорта (путь и размер пула декодера).
    pub(crate) const fn plan(&self) -> VideoImportPlan {
        self.plan
    }

    /// Источник ресурсов кадров.
    pub(crate) const fn source(&self) -> &FrameSource {
        &self.source
    }

    /// Эпоха установки.
    pub(crate) const fn epoch(&self) -> ImportEpoch {
        self.epoch
    }
}

/// Кадр, чья картинка сейчас на видеослое.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ShownFrame {
    epoch: ImportEpoch,
    key: VideoFrameKey,
}

/// Учёт того, какой кадр сейчас «текущий» на видеослое.
///
/// Видеослой знает только ключи кадров внутри одной эпохи; трекер добавляет эпоху и этим
/// отвечает на вопрос «рисует ли слой именно картинку этого ключа».
#[derive(Debug, Default)]
pub(crate) struct ShownFrameTracker {
    shown: Option<ShownFrame>,
}

impl ShownFrameTracker {
    /// Картинка кадра `key` эпохи `epoch` уже на видеослое.
    pub(crate) fn is_on_layer(&self, epoch: ImportEpoch, key: VideoFrameKey) -> bool {
        self.shown == Some(ShownFrame { epoch, key })
    }

    /// Готовит видеослой к показу кадра эпохи `epoch`.
    ///
    /// Если на слое картинка другой эпохи, ключи слоя «забываются»: иначе кадр новой эпохи с
    /// тем же номером ресурса слой принял бы за уже загруженный и нарисовал чужую картинку.
    pub(crate) fn prepare_layer_for(&mut self, epoch: ImportEpoch, video: &mut VideoLayerRenderer) {
        if self.shown.is_some_and(|shown| shown.epoch != epoch) {
            video.forget_uploaded_frames();
            self.shown = None;
        }
    }

    /// Кадр `key` эпохи `epoch` стал текущим на видеослое.
    pub(crate) fn mark_shown(&mut self, epoch: ImportEpoch, key: VideoFrameKey) {
        self.shown = Some(ShownFrame { epoch, key });
    }

    /// Состояние видеослоя неизвестно (отказ показа) или его очистили: следующий кадр покажется
    /// заново.
    pub(crate) fn forget(&mut self) {
        self.shown = None;
    }
}
