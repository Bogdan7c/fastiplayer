use std::collections::VecDeque;

/// Однопоточное состояние скользящего RAM-окна prefetch-буфера.
#[derive(Debug, Clone)]
pub struct PrefetchBufferState {
    /// Чанки лежат строго в порядке абсолютных offsets и добавляются только в хвост.
    chunks: VecDeque<Vec<u8>>,

    /// Абсолютный offset первого byte-а в `chunks`.
    base_offset: u64,

    /// Абсолютный offset следующего byte-а, который прочитает caller.
    read_cursor: u64,

    /// Абсолютный offset конца source-а, если upstream уже сообщил EOF.
    eof_offset: Option<u64>,

    /// Суммарная длина всех чанков в `chunks`, чтобы не пересчитывать её при каждом запросе.
    total_len: u64,

    /// Сколько bytes впереди cursor-а буфер старается иметь до EOF.
    window_bytes: u64,

    /// Сколько bytes позади cursor-а нужно сохранять для коротких backward-read.
    lookback_bytes: u64,
}

impl PrefetchBufferState {
    /// Создаёт пустой буфер, начинающийся с указанного absolute offset.
    #[must_use]
    pub fn new(start_offset: u64, window_bytes: u64, lookback_bytes: u64) -> Self {
        assert!(
            window_bytes > 0,
            "prefetch window должен быть больше нуля, иначе буфер никогда не запросит данные"
        );

        Self {
            chunks: VecDeque::new(),
            base_offset: start_offset,
            read_cursor: start_offset,
            eof_offset: None,
            total_len: 0,
            window_bytes,
            lookback_bytes,
        }
    }

    /// Возвращает absolute offset сразу после последнего buffered byte-а.
    #[must_use]
    pub fn buffered_end(&self) -> u64 {
        Self::checked_offset_add(self.base_offset, self.total_len, "buffered_end overflow")
    }

    /// Возвращает absolute offset, с которого future worker должен читать следующий chunk.
    #[must_use]
    pub fn next_fetch_offset(&self) -> u64 {
        self.buffered_end()
    }

    /// Возвращает число bytes, доступных для чтения из текущего cursor-а.
    #[must_use]
    pub fn available_from_cursor(&self) -> u64 {
        let buffered_end = self.buffered_end();

        if self.read_cursor < self.base_offset || self.read_cursor > buffered_end {
            return 0;
        }

        buffered_end - self.read_cursor
    }

    /// Проверяет, нужно ли дочитывать source, чтобы восстановить целевое окно впереди cursor-а.
    #[must_use]
    pub fn needs_fetch(&self) -> bool {
        self.eof_offset.is_none() && self.available_from_cursor() < self.window_bytes
    }

    /// Добавляет непустой contiguous chunk в хвост buffered range.
    pub fn append_chunk(&mut self, chunk_bytes: Vec<u8>) {
        assert!(
            !chunk_bytes.is_empty(),
            "пустой chunk запрещён: EOF фиксируется через mark_eof_at_fetch_offset"
        );
        assert!(
            self.eof_offset.is_none(),
            "нельзя добавлять chunk после EOF: это ломает absolute eof_offset"
        );

        let chunk_len = Self::usize_to_u64(chunk_bytes.len());
        Self::checked_offset_add(self.buffered_end(), chunk_len, "append_chunk overflow");

        self.total_len = self
            .total_len
            .checked_add(chunk_len)
            .expect("total_len overflow при append_chunk");
        self.chunks.push_back(chunk_bytes);
    }

    /// Помечает EOF на текущем fetch offset, не добавляя фиктивный пустой chunk.
    pub fn mark_eof_at_fetch_offset(&mut self) {
        self.eof_offset = Some(self.buffered_end());
    }

    /// Проверяет, стоит ли cursor на известном EOF либо за ним после legal seek-а.
    #[must_use]
    pub fn is_eof_at_cursor(&self) -> bool {
        self.eof_offset
            .is_some_and(|eof_offset| self.read_cursor >= eof_offset)
    }

    /// Копирует bytes из cursor-а в `output`, продвигает cursor и удаляет устаревшие head chunks.
    pub fn copy_to(&mut self, output: &mut [u8]) -> usize {
        let bytes_to_copy = self.copy_len_for(output);

        if bytes_to_copy == 0 {
            return 0;
        }

        let copied_bytes = self.copy_available_bytes(output, bytes_to_copy);
        debug_assert_eq!(
            copied_bytes, bytes_to_copy,
            "available_from_cursor обещал больше bytes, чем нашлось в chunks"
        );

        self.read_cursor = Self::checked_offset_add(
            self.read_cursor,
            Self::usize_to_u64(copied_bytes),
            "read_cursor overflow после copy_to",
        );
        self.evict_before_lookback();

        copied_bytes
    }

    /// Проверяет, входит ли absolute offset в buffered half-open range.
    #[must_use]
    pub fn contains(&self, offset: u64) -> bool {
        offset >= self.base_offset && offset < self.buffered_end()
    }

    /// Переставляет cursor внутри buffered range, включая позицию ровно в конце range.
    pub fn set_cursor_within(&mut self, offset: u64) {
        let buffered_end = self.buffered_end();

        assert!(
            offset >= self.base_offset && offset <= buffered_end,
            "cursor должен оставаться внутри [{}, {}], получен {}",
            self.base_offset,
            buffered_end,
            offset
        );

        self.read_cursor = offset;
        self.evict_before_lookback();
    }

    /// Ставит logical cursor впереди готового буфера, пока active fetch несёт этот offset.
    ///
    /// Проверку диапазона active fetch-а выполняет `PrefetchingByteSource`, потому
    /// buffer намеренно не знает о worker token/lifecycle. До append-а чтение будет
    /// ждать на condvar; после append-а обычный `available_from_cursor` увидит bytes.
    pub fn stage_cursor_ahead(&mut self, offset: u64) {
        assert!(
            offset > self.buffered_end(),
            "staged cursor должен находиться строго впереди готового буфера"
        );
        self.read_cursor = offset;
    }

    /// Полностью сбрасывает буфер и начинает новый contiguous range с указанного offset.
    pub fn reset_to(&mut self, offset: u64) {
        self.chunks.clear();
        self.base_offset = offset;
        self.read_cursor = offset;
        self.eof_offset = None;
        self.total_len = 0;
    }

    /// Вычисляет, сколько bytes можно безопасно скопировать в output за текущий вызов.
    fn copy_len_for(&self, output: &[u8]) -> usize {
        let available_bytes = self.available_from_cursor();
        let output_len = Self::usize_to_u64(output.len());
        let requested_bytes = available_bytes.min(output_len);

        usize::try_from(requested_bytes).expect("usize меньше u64 на этой платформе")
    }

    /// Копирует уже рассчитанное количество bytes из chunk queue без изменения cursor-а.
    fn copy_available_bytes(&self, output: &mut [u8], bytes_to_copy: usize) -> usize {
        let mut bytes_left_before_cursor = self.read_cursor - self.base_offset;
        let mut copied_bytes = 0;

        for chunk_bytes in &self.chunks {
            let chunk_len = Self::usize_to_u64(chunk_bytes.len());

            if bytes_left_before_cursor >= chunk_len {
                bytes_left_before_cursor -= chunk_len;
                continue;
            }

            let chunk_start_index = usize::try_from(bytes_left_before_cursor)
                .expect("chunk offset должен помещаться в usize");
            let readable_chunk_bytes = &chunk_bytes[chunk_start_index..];
            let output_remaining = bytes_to_copy - copied_bytes;
            let bytes_from_chunk = readable_chunk_bytes.len().min(output_remaining);

            output[copied_bytes..copied_bytes + bytes_from_chunk]
                .copy_from_slice(&readable_chunk_bytes[..bytes_from_chunk]);
            copied_bytes += bytes_from_chunk;

            if copied_bytes == bytes_to_copy {
                break;
            }

            bytes_left_before_cursor = 0;
        }

        copied_bytes
    }

    /// Удаляет head chunks, которые полностью лежат перед разрешённым lookback range.
    fn evict_before_lookback(&mut self) {
        let keep_from_offset = self.read_cursor.saturating_sub(self.lookback_bytes);

        while let Some(front_chunk) = self.chunks.front() {
            let front_len = Self::usize_to_u64(front_chunk.len());
            let front_end =
                Self::checked_offset_add(self.base_offset, front_len, "front chunk end overflow");

            if front_end > keep_from_offset {
                break;
            }

            self.chunks.pop_front();
            self.base_offset = front_end;
            self.total_len = self
                .total_len
                .checked_sub(front_len)
                .expect("total_len underflow при eviction");
        }
    }

    /// Складывает absolute offset и длину, явно падая на невозможном для буфера overflow.
    fn checked_offset_add(offset: u64, len: u64, context: &'static str) -> u64 {
        offset.checked_add(len).expect(context)
    }

    /// Преобразует размер allocation из `usize` в absolute byte count.
    fn usize_to_u64(value: usize) -> u64 {
        u64::try_from(value).expect("usize должен помещаться в u64")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buffer_with_small_window() -> PrefetchBufferState {
        PrefetchBufferState::new(0, 6, 2)
    }

    #[test]
    fn append_and_copy_reads_original_bytes_across_chunk_boundaries() {
        let mut buffer = buffer_with_small_window();
        buffer.append_chunk(vec![1, 2, 3]);
        buffer.append_chunk(vec![4, 5]);
        buffer.append_chunk(vec![6, 7, 8]);

        let mut output = [0; 8];
        let copied = buffer.copy_to(&mut output);

        assert_eq!(copied, 8);
        assert_eq!(output, [1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(buffer.available_from_cursor(), 0);
    }

    #[test]
    fn available_from_cursor_tracks_bytes_before_and_after_copy() {
        let mut buffer = buffer_with_small_window();
        buffer.append_chunk(vec![10, 11, 12, 13]);

        assert_eq!(buffer.available_from_cursor(), 4);

        let mut output = [0; 2];
        let copied = buffer.copy_to(&mut output);

        assert_eq!(copied, 2);
        assert_eq!(output, [10, 11]);
        assert_eq!(buffer.available_from_cursor(), 2);
    }

    #[test]
    fn needs_fetch_follows_window_ahead_of_cursor() {
        let mut buffer = buffer_with_small_window();

        assert!(buffer.needs_fetch());

        buffer.append_chunk(vec![1, 2, 3, 4, 5, 6]);
        assert!(!buffer.needs_fetch());

        let mut output = [0; 1];
        assert_eq!(buffer.copy_to(&mut output), 1);
        assert!(buffer.needs_fetch());
    }

    #[test]
    #[should_panic(expected = "пустой chunk запрещён")]
    fn append_chunk_rejects_empty_data() {
        let mut buffer = buffer_with_small_window();

        buffer.append_chunk(Vec::new());
    }

    #[test]
    fn eviction_drops_head_chunks_outside_lookback_and_preserves_backward_window() {
        let mut buffer = PrefetchBufferState::new(0, 12, 3);
        buffer.append_chunk(vec![0, 1, 2, 3]);
        buffer.append_chunk(vec![4, 5, 6, 7]);
        buffer.append_chunk(vec![8, 9, 10, 11]);

        let mut output = [0; 9];
        assert_eq!(buffer.copy_to(&mut output), 9);

        assert_eq!(buffer.base_offset, 4);
        assert!(buffer.contains(6));
        assert!(!buffer.contains(3));

        buffer.set_cursor_within(6);
        let mut reread_output = [0; 3];
        assert_eq!(buffer.copy_to(&mut reread_output), 3);
        assert_eq!(reread_output, [6, 7, 8]);
    }

    #[test]
    fn eof_is_reached_after_copying_to_marked_fetch_offset() {
        let mut buffer = buffer_with_small_window();
        buffer.append_chunk(vec![1, 2, 3]);
        buffer.mark_eof_at_fetch_offset();

        assert!(!buffer.needs_fetch());
        assert!(!buffer.is_eof_at_cursor());

        let mut output = [0; 3];
        assert_eq!(buffer.copy_to(&mut output), 3);

        assert_eq!(output, [1, 2, 3]);
        assert!(buffer.is_eof_at_cursor());
    }

    #[test]
    fn contains_and_set_cursor_within_support_backward_reads_inside_window() {
        let mut buffer = PrefetchBufferState::new(100, 8, 4);
        buffer.append_chunk(vec![1, 2, 3, 4]);
        buffer.append_chunk(vec![5, 6, 7, 8]);

        let mut skipped_output = [0; 6];
        assert_eq!(buffer.copy_to(&mut skipped_output), 6);

        assert!(buffer.contains(102));
        assert!(buffer.contains(105));
        assert!(!buffer.contains(108));

        buffer.set_cursor_within(103);

        let mut reread_output = [0; 3];
        assert_eq!(buffer.copy_to(&mut reread_output), 3);
        assert_eq!(reread_output, [4, 5, 6]);
    }

    #[test]
    fn set_cursor_within_accepts_buffered_end_and_copy_returns_zero() {
        let mut buffer = buffer_with_small_window();
        buffer.append_chunk(vec![1, 2, 3]);

        buffer.set_cursor_within(buffer.buffered_end());

        let mut output = [0; 2];
        assert_eq!(buffer.copy_to(&mut output), 0);
        assert_eq!(output, [0, 0]);
    }

    #[test]
    fn reset_to_clears_buffer_and_moves_offsets() {
        let mut buffer = buffer_with_small_window();
        buffer.append_chunk(vec![1, 2, 3, 4]);
        buffer.mark_eof_at_fetch_offset();

        buffer.reset_to(42);

        assert_eq!(buffer.buffered_end(), 42);
        assert_eq!(buffer.next_fetch_offset(), 42);
        assert_eq!(buffer.available_from_cursor(), 0);
        assert!(buffer.needs_fetch());
        assert!(!buffer.is_eof_at_cursor());
        assert!(!buffer.contains(41));
    }

    #[test]
    fn staged_forward_cursor_reads_bytes_after_active_chunk_arrives() {
        let mut buffer = PrefetchBufferState::new(0, 32, 16);
        buffer.append_chunk((0_u8..8).collect());

        buffer.stage_cursor_ahead(12);
        assert_eq!(buffer.available_from_cursor(), 0);

        buffer.append_chunk((8_u8..24).collect());
        let mut output = [0_u8; 4];
        assert_eq!(buffer.copy_to(&mut output), output.len());
        assert_eq!(output, [12, 13, 14, 15]);
    }

    #[test]
    fn staged_cursor_beyond_short_read_observes_upstream_eof() {
        let mut buffer = PrefetchBufferState::new(0, 32, 16);
        buffer.append_chunk((0_u8..8).collect());
        buffer.stage_cursor_ahead(12);

        buffer.mark_eof_at_fetch_offset();

        assert!(buffer.is_eof_at_cursor());
    }
}
