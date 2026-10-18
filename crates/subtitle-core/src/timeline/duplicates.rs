//! Обнаружение дубликатов при пакетной загрузке за O(n log n).
//!
//! Дубликат — событие с равными `(start, end, layer, content)`. Прямое сравнение каждого
//! события со всеми ранее принятыми с тем же началом квадратично (тысячи реплик с одним
//! `start` — обычное дело для плохих файлов). Поэтому внутри группы одного `start` события
//! раскладываются по корзинам грубого ключа [`bucket_key`], а полное `==` выполняется только
//! между событиями одной корзины.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};

use crate::event::{SubtitleContent, SubtitleEvent};

/// Грубый ключ корзины. Инвариант: из `a == b` всегда следует `bucket_key(a) == bucket_key(b)`
/// (ключ считается только из полей, которые входят в сравнение), обратное не требуется —
/// коллизии разрешает полное сравнение. Содержимое хешируется «дёшево»: для текста берутся
/// сами строки, для картинок — размеры, без прохода по пикселям.
fn bucket_key(event: &SubtitleEvent) -> u64 {
    let mut hasher = DefaultHasher::new();
    event.end().as_micros().hash(&mut hasher);
    event.layer().get().hash(&mut hasher);
    match event.content() {
        SubtitleContent::Text(cue) => {
            0u8.hash(&mut hasher);
            for span in cue.text.spans() {
                if let crate::styled_text::SpanContent::Text(text) = &span.content {
                    text.hash(&mut hasher);
                }
                // Разделитель, чтобы «ab»+«c» и «a»+«bc» не склеивались в один поток байтов.
                0xFFu8.hash(&mut hasher);
            }
        }
        SubtitleContent::Ass(cue) => {
            1u8.hash(&mut hasher);
            cue.read_order.hash(&mut hasher);
            cue.name.hash(&mut hasher);
            cue.effect.hash(&mut hasher);
            cue.text.len().hash(&mut hasher);
        }
        SubtitleContent::Picture(cue) => {
            2u8.hash(&mut hasher);
            cue.picture().encoded_len().hash(&mut hasher);
        }
    }
    hasher.finish()
}

/// Корзины событий одного `start`: ключ -> уже принятые события.
#[derive(Default)]
pub(super) struct DuplicateIndex<'a> {
    buckets: HashMap<u64, Vec<&'a SubtitleEvent>>,
}

impl<'a> DuplicateIndex<'a> {
    /// Запоминает принятое событие (без проверки на дубликат).
    pub(super) fn remember(&mut self, event: &'a SubtitleEvent) {
        self.buckets
            .entry(bucket_key(event))
            .or_default()
            .push(event);
    }

    /// Есть ли среди принятых событие, равное `event`.
    pub(super) fn contains(&self, event: &SubtitleEvent) -> bool {
        self.buckets
            .get(&bucket_key(event))
            .is_some_and(|bucket| bucket.contains(&event))
    }
}
