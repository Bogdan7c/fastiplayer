//! Текстуры интерфейса fake-рендера: честная модель «что загружено в рендер» без GPU
//! (сессия 02a). Помнит размер каждой загруженной текстуры, как egui-wgpu помнит свои.

use std::collections::HashMap;

use egui::TexturesDelta;
use egui::epaint::TextureId;

use crate::EguiFrameContent;

/// Итог применения `set` одного кадра.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FakeUiTextureSync {
    /// Все изменения применены.
    InSync,
    /// Была частичная правка текстуры, которой нет (или которая в неё не помещается).
    OutOfSync,
}

/// Как fake обходится с частичной правкой неизвестной текстуры.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FakeMissingTexturePolicy {
    /// Честно: сообщает о рассинхронизации.
    Report,
    /// Дефект: молча «создаёт» текстуру из правки.
    SilentlyAccept,
}

/// Загруженные в fake-рендер текстуры интерфейса.
#[derive(Debug, Default)]
pub(super) struct FakeUiTextures {
    /// Размер каждой загруженной текстуры.
    sizes: HashMap<TextureId, [usize; 2]>,
}

impl FakeUiTextures {
    /// Применяет `set` кадра по порядку дельт каждого id (целая загрузка задаёт размер).
    pub(super) fn apply_set(
        &mut self,
        textures_delta: &TexturesDelta,
        policy: FakeMissingTexturePolicy,
    ) -> FakeUiTextureSync {
        let mut sync = FakeUiTextureSync::InSync;
        for (texture_id, image_deltas) in &textures_delta.set {
            for image_delta in image_deltas {
                let size = image_delta.image.size();
                let Some([x, y]) = image_delta.pos else {
                    self.sizes.insert(*texture_id, size);
                    continue;
                };
                let fits = self
                    .sizes
                    .get(texture_id)
                    .is_some_and(|known| x + size[0] <= known[0] && y + size[1] <= known[1]);
                match (fits, policy) {
                    (true, _) => {}
                    (false, FakeMissingTexturePolicy::SilentlyAccept) => {
                        self.sizes.insert(*texture_id, [x + size[0], y + size[1]]);
                    }
                    (false, FakeMissingTexturePolicy::Report) => {
                        sync = FakeUiTextureSync::OutOfSync;
                    }
                }
            }
        }
        sync
    }

    /// Применяет `free` кадра и очищает дельту (последний шаг её жизни).
    pub(super) fn apply_free_and_finish(&mut self, mut textures_delta: TexturesDelta) {
        for texture_id in &textures_delta.free {
            self.sizes.remove(texture_id);
        }
        textures_delta.clear();
    }
}

/// Интерфейс кадра после первого шага `render_frame`.
pub(super) enum FakeUiFrame {
    /// Изменения текстур уже применены (честный порядок).
    Applied(FakeAppliedUi),
    /// Дефект: изменения ждут, дойдёт ли кадр до рисования.
    Deferred(EguiFrameContent),
}

/// Что осталось от интерфейса кадра после применения текстур.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FakeAppliedUi {
    /// Свежий интерфейс: итог применения его текстур.
    Fresh(FakeUiTextureSync),
    /// Повтор удержанного интерфейса (текстур нет).
    ReusePrevious,
}

impl FakeUiFrame {
    /// Кадр отклонён до рисования: отложенные (дефект) изменения теряются, как у рендера,
    /// применяющего текстуры слишком поздно.
    pub(super) fn discard_on_rejection(self) {
        if let Self::Deferred(EguiFrameContent::Fresh {
            mut textures_delta, ..
        }) = self
        {
            textures_delta.clear();
        }
    }
}

impl FakeUiTextures {
    /// Первый шаг кадра: применяет `set` и `free` свежего интерфейса.
    pub(super) fn apply_frame(
        &mut self,
        egui: EguiFrameContent,
        policy: FakeMissingTexturePolicy,
    ) -> FakeAppliedUi {
        match egui {
            EguiFrameContent::Fresh { textures_delta, .. } => {
                let sync = self.apply_set(&textures_delta, policy);
                // Рисования у fake нет: освобождать можно сразу после `set`.
                self.apply_free_and_finish(textures_delta);
                FakeAppliedUi::Fresh(sync)
            }
            EguiFrameContent::ReusePrevious => FakeAppliedUi::ReusePrevious,
        }
    }

    /// Доводит кадр до «текстуры применены» (для отложенного дефектом — сейчас).
    pub(super) fn finish_frame(
        &mut self,
        frame: FakeUiFrame,
        policy: FakeMissingTexturePolicy,
    ) -> FakeAppliedUi {
        match frame {
            FakeUiFrame::Applied(applied) => applied,
            FakeUiFrame::Deferred(egui) => self.apply_frame(egui, policy),
        }
    }
}
