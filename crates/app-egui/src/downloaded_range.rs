//! «Что уже скачано»: где плеер окна может перематываться, никого не прося.
//!
//! В обычном окне плеер перематывается сам — файл или сеть у него под рукой. В окне
//! браузерного режима байты качает страница: перемотка туда, где байтов нет, — это
//! просьба странице перемотаться (`seek` по мосту). Живое перетаскивание по таймлайну
//! шлёт плееру десятки целей за жест, поэтому окну нужно знать, какие цели лягут на
//! уже скачанное (решение владельца 31.10.2026: «кадры бегут по полке», странице за
//! жест — ничего).
//!
//! Это знание принадлежит источнику (полка `pushed-media-source`), а окно видит его
//! только через [`DownloadedRangeProbe`] — ни типов источника, ни сеанса моста здесь
//! нет. Ответ — про «сейчас»: полка растёт с каждым сегментом и выселяет давно
//! показанное, поэтому окно спрашивает в момент каждой цели, а не хранит копию.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

/// Вопрос к владельцу байтов: сядет ли перемотка к позиции на уже скачанное.
pub(crate) trait DownloadedRangeProbe: Send + Sync {
    /// Перемотка к `position` (время медиа) не попросит никого докачать байты.
    fn holds(&self, position: Duration) -> bool;
}

/// Что окно знает о скачанном у текущего медиа.
#[derive(Clone, Default)]
pub(crate) enum DownloadedRange {
    /// Ничего не известно: медиа нет, это файл `<video src>` (байты по запросам),
    /// эфир или поколение, которое ещё открывается. Живое перетаскивание там, где оно
    /// зависит от скачанного, выключено — перемотка только на отпускании.
    #[default]
    Unknown,
    /// Владелец байтов отвечает на вопрос «скачано ли» в момент вопроса.
    Known(Arc<dyn DownloadedRangeProbe>),
}

impl DownloadedRange {
    /// Скачанное известно владельцу этих байтов.
    pub(crate) fn known(probe: Arc<dyn DownloadedRangeProbe>) -> Self {
        Self::Known(probe)
    }

    /// Можно ли вообще спрашивать о скачанном.
    pub(crate) const fn is_known(&self) -> bool {
        matches!(self, Self::Known(_))
    }

    /// Лежит ли `position` на скачанном. Неизвестное — никогда (осторожный ответ:
    /// лишняя перемотка на отпускании лучше, чем просьба странице посреди жеста).
    pub(crate) fn covers(&self, position: Duration) -> bool {
        match self {
            Self::Unknown => false,
            Self::Known(probe) => probe.holds(position),
        }
    }
}

impl fmt::Debug for DownloadedRange {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown => formatter.write_str("DownloadedRange::Unknown"),
            Self::Known(_) => formatter.write_str("DownloadedRange::Known"),
        }
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    //! Подделка владельца байтов для тестов окна: скачанное — набор полуоткрытых отрезков.

    use std::ops::Range;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use super::{DownloadedRange, DownloadedRangeProbe};

    /// Скачанное — отрезки `[start, end)`; можно менять на ходу (полка растёт).
    #[derive(Default)]
    pub(crate) struct FakeDownloadedSpans {
        spans: Mutex<Vec<Range<Duration>>>,
    }

    impl FakeDownloadedSpans {
        /// Скачанное из отрезков и сведение для окна.
        pub(crate) fn range(spans: Vec<Range<Duration>>) -> (Arc<Self>, DownloadedRange) {
            let probe = Arc::new(Self {
                spans: Mutex::new(spans),
            });
            let range = DownloadedRange::known(probe.clone());
            (probe, range)
        }

        /// Заменяет скачанное.
        pub(crate) fn replace(&self, spans: Vec<Range<Duration>>) {
            *self.spans.lock().expect("spans lock") = spans;
        }
    }

    impl DownloadedRangeProbe for FakeDownloadedSpans {
        fn holds(&self, position: Duration) -> bool {
            self.spans
                .lock()
                .expect("spans lock")
                .iter()
                .any(|span| span.contains(&position))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::test_support::FakeDownloadedSpans;
    use super::*;

    #[test]
    fn unknown_covers_nothing_and_is_not_known() {
        let range = DownloadedRange::default();
        assert!(!range.is_known());
        assert!(!range.covers(Duration::ZERO));
        assert!(!range.covers(Duration::from_secs(5)));
    }

    #[test]
    fn a_known_range_answers_from_its_owner_at_the_moment_of_the_question() {
        let (spans, range) = FakeDownloadedSpans::range(vec![
            Duration::from_secs(0)..Duration::from_secs(2),
            Duration::from_secs(4)..Duration::from_secs(6),
        ]);
        assert!(range.is_known());
        assert!(range.covers(Duration::from_secs(1)));
        assert!(!range.covers(Duration::from_secs(3)));
        assert!(range.covers(Duration::from_secs(5)));
        // Полка выросла: тот же `DownloadedRange` видит новое, копии нет.
        spans.replace(vec![Duration::from_secs(0)..Duration::from_secs(6)]);
        assert!(range.covers(Duration::from_secs(3)));
    }
}
