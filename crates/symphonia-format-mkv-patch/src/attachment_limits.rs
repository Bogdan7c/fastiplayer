// Fastiplayer additive patch: лимиты чтения вложений Matroska (шрифты и т.п.).
//
// Вложение, которое не помещается в лимит, пропускается БЕЗ чтения FileData в память:
// элемент перешагивается через `ignore_bytes`, а в отчёт попадают имя и объявленный размер.
// Без лимитов (по умолчанию) поведение патча идентично upstream.

use std::cell::RefCell;

/// Лимиты на суммарный объём вложений, читаемых в память.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct AttachmentReadLimits {
    /// Максимальный размер FileData одного вложения в байтах.
    pub max_bytes_per_attachment: u64,
    /// Максимальная сумма FileData всех принятых вложений в байтах.
    pub max_total_bytes: u64,
}

impl AttachmentReadLimits {
    /// Без ограничений: ровно upstream-поведение.
    pub const UNLIMITED: Self =
        Self { max_bytes_per_attachment: u64::MAX, max_total_bytes: u64::MAX };

    /// Создаёт лимиты с явными именованными значениями.
    pub const fn new(max_bytes_per_attachment: u64, max_total_bytes: u64) -> Self {
        Self { max_bytes_per_attachment, max_total_bytes }
    }
}

impl Default for AttachmentReadLimits {
    fn default() -> Self {
        Self::UNLIMITED
    }
}

/// Почему вложение было пропущено без чтения содержимого.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SkippedAttachmentReason {
    /// Размер FileData превышает лимит на одно вложение.
    TooLarge { size_bytes: u64, limit_bytes: u64 },
    /// Вложение влезает в личный лимит, но не влезает в суммарный.
    TotalLimitExceeded { size_bytes: u64, accepted_total_bytes: u64, limit_bytes: u64 },
}

/// Отчёт об одном пропущенном вложении.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkippedAttachment {
    /// Имя файла вложения (пустое, если контейнер его не указал).
    pub file_name: String,
    /// Причина пропуска.
    pub reason: SkippedAttachmentReason,
}

/// Бюджет чтения вложений одного элемента `Attachments`.
#[derive(Debug)]
pub(crate) struct AttachmentBudget {
    limits: AttachmentReadLimits,
    accepted_total_bytes: u64,
}

impl AttachmentBudget {
    pub(crate) fn new(limits: AttachmentReadLimits) -> Self {
        Self { limits, accepted_total_bytes: 0 }
    }

    /// Решает, можно ли читать вложение с объявленным размером; при согласии резервирует бюджет.
    pub(crate) fn admit(&mut self, size_bytes: u64) -> Result<(), SkippedAttachmentReason> {
        if size_bytes > self.limits.max_bytes_per_attachment {
            return Err(SkippedAttachmentReason::TooLarge {
                size_bytes,
                limit_bytes: self.limits.max_bytes_per_attachment,
            });
        }

        // `checked_add`: переполнение u64 тоже означает «не влезает».
        match self.accepted_total_bytes.checked_add(size_bytes) {
            Some(new_total) if new_total <= self.limits.max_total_bytes => {
                self.accepted_total_bytes = new_total;
                Ok(())
            }
            _ => Err(SkippedAttachmentReason::TotalLimitExceeded {
                size_bytes,
                accepted_total_bytes: self.accepted_total_bytes,
                limit_bytes: self.limits.max_total_bytes,
            }),
        }
    }
}

/// Состояние активной области лимитов текущего потока.
struct ActiveScope {
    limits: AttachmentReadLimits,
    skipped: Vec<SkippedAttachment>,
}

thread_local! {
    /// Область лимитов нужна, потому что generic `Probe` Symphonia создаёт `MkvReader`
    /// через статический `try_probe_new` без возможности передать параметры.
    static ACTIVE_SCOPE: RefCell<Option<ActiveScope>> = const { RefCell::new(None) };
}

/// Возвращает лимиты активной области или `UNLIMITED`, если области нет.
pub(crate) fn scoped_limits() -> AttachmentReadLimits {
    ACTIVE_SCOPE.with(|scope| {
        scope.borrow().as_ref().map_or(AttachmentReadLimits::UNLIMITED, |active| active.limits)
    })
}

/// Сообщает активной области о пропущенных вложениях созданного reader-а.
pub(crate) fn report_skipped_to_scope(skipped: &[SkippedAttachment]) {
    if skipped.is_empty() {
        return;
    }
    ACTIVE_SCOPE.with(|scope| {
        if let Some(active) = scope.borrow_mut().as_mut() {
            active.skipped.extend_from_slice(skipped);
        }
    });
}

/// Возвращает предыдущую область при выходе (в том числе при panic внутри замыкания).
struct ScopeRestore {
    previous: Option<ActiveScope>,
}

impl Drop for ScopeRestore {
    fn drop(&mut self) {
        let previous = self.previous.take();
        ACTIVE_SCOPE.with(|scope| *scope.borrow_mut() = previous);
    }
}

/// Выполняет `operation` (например, generic `Probe::probe`) с заданными лимитами вложений.
///
/// Все `MkvReader`, созданные внутри замыкания в этом потоке, применяют `limits`.
/// Возвращает результат замыкания и список вложений, пропущенных этими reader-ами.
pub fn with_attachment_read_limits<T>(
    limits: AttachmentReadLimits,
    operation: impl FnOnce() -> T,
) -> (T, Vec<SkippedAttachment>) {
    let previous =
        ACTIVE_SCOPE.with(|scope| scope.borrow_mut().replace(ActiveScope { limits, skipped: Vec::new() }));
    let restore = ScopeRestore { previous };

    let result = operation();

    // Забираем накопленный отчёт до восстановления предыдущей области.
    let skipped = ACTIVE_SCOPE
        .with(|scope| scope.borrow_mut().as_mut().map(|active| std::mem::take(&mut active.skipped)))
        .unwrap_or_default();
    drop(restore);
    (result, skipped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_rejects_oversized_attachment_without_consuming_budget() {
        let mut budget = AttachmentBudget::new(AttachmentReadLimits::new(10, 15));

        assert_eq!(
            budget.admit(11),
            Err(SkippedAttachmentReason::TooLarge { size_bytes: 11, limit_bytes: 10 })
        );
        // Отказ не тратит суммарный бюджет: два файла по 8 байт не помещаются, но 7 + 8 да.
        assert_eq!(budget.admit(7), Ok(()));
        assert_eq!(budget.admit(8), Ok(()));
    }

    #[test]
    fn budget_reports_total_limit_with_accepted_total() {
        let mut budget = AttachmentBudget::new(AttachmentReadLimits::new(10, 15));
        assert_eq!(budget.admit(10), Ok(()));

        assert_eq!(
            budget.admit(6),
            Err(SkippedAttachmentReason::TotalLimitExceeded {
                size_bytes: 6,
                accepted_total_bytes: 10,
                limit_bytes: 15,
            })
        );
        assert_eq!(budget.admit(5), Ok(()));
    }

    #[test]
    fn unlimited_budget_accepts_everything_without_overflow_panic() {
        let mut budget = AttachmentBudget::new(AttachmentReadLimits::UNLIMITED);
        assert_eq!(budget.admit(u64::MAX / 2), Ok(()));
        assert_eq!(budget.admit(u64::MAX / 2), Ok(()));
    }

    #[test]
    fn scope_applies_limits_collects_report_and_restores_previous_state() {
        assert_eq!(scoped_limits(), AttachmentReadLimits::UNLIMITED);

        let limits = AttachmentReadLimits::new(1, 2);
        let (seen_limits, skipped) = with_attachment_read_limits(limits, || {
            report_skipped_to_scope(&[SkippedAttachment {
                file_name: "a.ttf".to_owned(),
                reason: SkippedAttachmentReason::TooLarge { size_bytes: 5, limit_bytes: 1 },
            }]);
            scoped_limits()
        });

        assert_eq!(seen_limits, limits);
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].file_name, "a.ttf");
        assert_eq!(scoped_limits(), AttachmentReadLimits::UNLIMITED);
    }

    #[test]
    fn nested_scopes_restore_outer_limits() {
        let outer = AttachmentReadLimits::new(3, 4);
        let (inner_after, _) = with_attachment_read_limits(outer, || {
            let _ = with_attachment_read_limits(AttachmentReadLimits::new(1, 1), || ());
            scoped_limits()
        });
        assert_eq!(inner_after, outer);
    }
}
