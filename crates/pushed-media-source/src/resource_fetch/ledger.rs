//! «Какие номера выдала эта очередь» — для маршрутизации рамок у приложения и для
//! строгости после того, как запись запроса уже убрана.
//!
//! Номера общие на сеанс (тот же счётчик, что у файла по запросам байтов), поэтому
//! очередь получает их не подряд. Храним отрезки подряд идущих номеров: обычно
//! очередь выдаёт номера сериями, и память растёт с числом «перемежений» с файлом,
//! а не с числом запросов. Номера одной очереди выдаются под её замком по растущему
//! счётчику, поэтому отрезки почти всегда дописываются в конец.

/// Номера, выданные очередью, отрезками `[first, last]` по возрастанию.
#[derive(Debug, Default)]
pub(super) struct OwnedRequestIds {
    /// Отрезки без пересечений и без соседства, по возрастанию.
    runs: Vec<IdRun>,
}

/// Отрезок подряд идущих номеров (оба конца включены).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct IdRun {
    /// Первый номер.
    first: u32,
    /// Последний номер.
    last: u32,
}

impl OwnedRequestIds {
    /// Запоминает номер, выданный очередью.
    pub(super) fn record(&mut self, id: u32) {
        // Обычный случай: номер продолжает последний отрезок или идёт после него.
        if let Some(tail) = self.runs.last_mut() {
            if tail.last.checked_add(1) == Some(id) {
                tail.last = id;
                return;
            }
            if id > tail.last {
                self.runs.push(IdRun {
                    first: id,
                    last: id,
                });
                return;
            }
        } else {
            self.runs.push(IdRun {
                first: id,
                last: id,
            });
            return;
        }
        // Номер не по порядку (счётчик так не выдаёт, но правило должно держаться и
        // тогда): вставляем на своё место без слияния — `owns` от этого не страдает.
        if self.owns(id) {
            return;
        }
        let position = self.runs.partition_point(|run| run.last < id);
        self.runs.insert(
            position,
            IdRun {
                first: id,
                last: id,
            },
        );
    }

    /// Выдавала ли номер эта очередь.
    pub(super) fn owns(&self, id: u32) -> bool {
        let position = self.runs.partition_point(|run| run.last < id);
        self.runs.get(position).is_some_and(|run| run.first <= id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interleaved_numbers_are_told_apart_and_series_merge_into_one_run() {
        let mut owned = OwnedRequestIds::default();
        // Очередь выдала 1–3, файл — 4, очередь — 5 и 6, файл — 7, очередь — 9.
        for id in [1, 2, 3, 5, 6, 9] {
            owned.record(id);
        }
        let ours: Vec<u32> = (0..12).filter(|id| owned.owns(*id)).collect();
        assert_eq!(ours, [1, 2, 3, 5, 6, 9]);
        // Серии сливаются: три отрезка, а не шесть записей.
        assert_eq!(owned.runs.len(), 3);
    }

    #[test]
    fn out_of_order_number_still_counts_as_ours_and_keeps_others_intact() {
        let mut owned = OwnedRequestIds::default();
        for id in [10, 20, 15, 20] {
            owned.record(id);
        }
        let ours: Vec<u32> = (0..25).filter(|id| owned.owns(*id)).collect();
        assert_eq!(ours, [10, 15, 20]);
    }

    #[test]
    fn the_largest_number_does_not_overflow_the_run() {
        let mut owned = OwnedRequestIds::default();
        owned.record(u32::MAX - 1);
        owned.record(u32::MAX);
        assert!(owned.owns(u32::MAX));
        assert!(!owned.owns(u32::MAX - 2));
    }
}
