//! Счётчики событий декодера: всё, что пропущено или исправлено, считается, а не теряется молча.

/// Накопленная статистика одной дорожки. Счётчики только растут.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PgsDecodeStatistics {
    /// Блоков, у которых нарезка на сегменты оборвалась (целые сегменты до обрыва учтены).
    pub blocks_with_framing_errors: u64,
    /// Сегментов, содержимое которых не удалось разобрать (пропущены).
    pub malformed_segments: u64,
    /// Сегментов неизвестного типа (пропущены).
    pub unknown_segments: u64,
    /// Сегментов вне набора отображения (до первого PCS или после END), пропущены.
    pub orphan_segments: u64,
    /// Наборов, закрытых не сегментом END, а появлением следующего PCS.
    pub implicit_end_sets: u64,
    /// Применённых наборов отображения.
    pub display_sets_applied: u64,
    /// Выданных композиций (включая «очистки»).
    pub compositions_emitted: u64,
    /// Наборов Normal, проигнорированных: ещё не было Epoch Start / Acquisition Point
    /// (начало потока или seek).
    pub sets_ignored_until_entry_point: u64,
    /// Композиций, пропущенных из-за отсутствия объекта или палитры.
    pub compositions_skipped_missing_resource: u64,
    /// Объектов, у которых после обрезки и окна не осталось видимой области.
    pub objects_dropped_no_visible_area: u64,
    /// Объектов, не дособранных до конца набора (потерян фрагмент).
    pub incomplete_objects: u64,
    /// Фрагментов-продолжений ODS без начала.
    pub orphan_object_fragments: u64,
    /// Объектов, начатых заново до завершения предыдущего экземпляра с тем же id.
    pub restarted_objects: u64,
    /// Объектов, длина данных которых не совпала с объявленной (оставлены как есть).
    pub object_length_mismatches: u64,
    /// Объектов, отвергнутых лимитом размера (объекта или эпохи).
    pub objects_rejected_over_limit: u64,
    /// PCS, отвергнутых из-за кадра больше предела формата (`MAX_REFERENCE_WIDTH`/`HEIGHT`).
    pub oversized_reference_frames: u64,
    /// Объектов, у которых картинка больше кадра композиции (отвергнуты).
    pub objects_larger_than_reference_frame: u64,
    /// PDS с хвостом, не составившим целую запись палитры (целые записи использованы).
    pub palettes_with_partial_trailing_entry: u64,
    /// PCS с неизвестным состоянием (обработан как Normal).
    pub unknown_composition_states: u64,
    /// Вызовов `reset_after_discontinuity`.
    pub discontinuities: u64,
}
