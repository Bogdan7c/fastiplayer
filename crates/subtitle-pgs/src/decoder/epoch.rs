//! Состояние эпохи: окна, палитры и объекты, живущие между наборами отображения.

use std::collections::HashMap;
use std::sync::Arc;

use subtitle_core::{IndexedPicture, PicturePalette};

use crate::pcs::CompositionObject;
use crate::wds::WindowDefinition;

/// Всё, что композиции Normal могут переиспользовать из прежних наборов.
#[derive(Default)]
pub(super) struct EpochState {
    pub(super) windows: HashMap<u8, WindowDefinition>,
    pub(super) palettes: HashMap<u8, Arc<PicturePalette>>,
    pub(super) objects: HashMap<u16, Arc<IndexedPicture>>,
    /// Список объектов последней применённой композиции: его наследует палитровое обновление.
    pub(super) last_composition_objects: Vec<CompositionObject>,
    /// Суммарный размер RLE объектов эпохи (для лимита памяти).
    pub(super) rle_bytes: usize,
}

impl EpochState {
    /// Полный сброс (начало эпохи или разрыв потока).
    pub(super) fn clear(&mut self) {
        *self = Self::default();
    }

    /// Кладёт объект, заменяя прежний с тем же id; `false`, если превышен лимит эпохи
    /// (тогда состояние не меняется).
    pub(super) fn try_insert_object(
        &mut self,
        object_id: u16,
        picture: Arc<IndexedPicture>,
        max_epoch_rle_bytes: usize,
    ) -> bool {
        let replaced = self
            .objects
            .get(&object_id)
            .map_or(0, |existing| existing.encoded_len());
        let new_total = self
            .rle_bytes
            .saturating_sub(replaced)
            .saturating_add(picture.encoded_len());
        if new_total > max_epoch_rle_bytes {
            return false;
        }
        self.rle_bytes = new_total;
        self.objects.insert(object_id, picture);
        true
    }
}
