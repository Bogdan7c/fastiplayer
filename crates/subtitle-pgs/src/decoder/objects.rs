//! Сборка объектов из фрагментов ODS.

use std::collections::HashMap;

use super::{PgsDecodeStatistics, PgsDecoderLimits};
use crate::ods::ObjectFragment;

/// Полностью собранный объект: размеры и склеенные RLE-байты.
#[derive(Debug)]
pub(super) struct AssembledObject {
    pub(super) object_id: u16,
    pub(super) width: u16,
    pub(super) height: u16,
    pub(super) rle_bytes: Vec<u8>,
}

struct InProgress {
    width: u16,
    height: u16,
    expected_rle_len: usize,
    bytes: Vec<u8>,
}

/// Склейка фрагментов по `object_id`. Память ограничена лимитом одного объекта.
#[derive(Default)]
pub(super) struct ObjectAssembler {
    in_progress: HashMap<u16, InProgress>,
}

impl ObjectAssembler {
    /// Принимает фрагмент; возвращает объект, если этот фрагмент его завершил.
    pub(super) fn feed(
        &mut self,
        fragment: &ObjectFragment<'_>,
        limits: PgsDecoderLimits,
        stats: &mut PgsDecodeStatistics,
    ) -> Option<AssembledObject> {
        let object_id = fragment.object_id;
        if let Some(header) = fragment.header {
            if self.in_progress.remove(&object_id).is_some() {
                stats.restarted_objects += 1;
            }
            if header.expected_rle_len() > limits.max_object_rle_bytes {
                stats.objects_rejected_over_limit += 1;
                return None;
            }
            self.in_progress.insert(
                object_id,
                InProgress {
                    width: header.width,
                    height: header.height,
                    expected_rle_len: header.expected_rle_len(),
                    bytes: Vec::new(),
                },
            );
        }
        let Some(current) = self.in_progress.get_mut(&object_id) else {
            // Продолжение без начала (или начало отвергнуто лимитом).
            if fragment.header.is_none() {
                stats.orphan_object_fragments += 1;
            }
            return None;
        };
        // Принимаем не больше лимита: даже при заниженной «объявленной» длине память ограничена.
        if current.bytes.len().saturating_add(fragment.rle_bytes.len())
            > limits.max_object_rle_bytes
        {
            self.in_progress.remove(&object_id);
            stats.objects_rejected_over_limit += 1;
            return None;
        }
        current.bytes.extend_from_slice(fragment.rle_bytes);
        if !fragment.position.ends_object() {
            return None;
        }
        let finished = self.in_progress.remove(&object_id)?;
        if finished.bytes.len() != finished.expected_rle_len {
            stats.object_length_mismatches += 1;
        }
        Some(AssembledObject {
            object_id,
            width: finished.width,
            height: finished.height,
            rle_bytes: finished.bytes,
        })
    }

    /// Конец набора: недособранное считается потерянным. Возвращает, сколько объектов брошено.
    pub(super) fn discard_unfinished(&mut self) -> usize {
        let count = self.in_progress.len();
        self.in_progress.clear();
        count
    }
}
