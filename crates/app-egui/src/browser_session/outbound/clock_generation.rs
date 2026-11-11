//! Номер поколения в тике часов (протокол 1.10, сессия 16a2, решение 311).
//!
//! Тик `clock` до 1.10 не говорил, к какому ролику он относится, и тень страницы принимала
//! часы ПРОШЛОГО поколения (конец баннера) за часы нового видео. С 1.10 тик называет
//! поколение, которое он описывает. Какое это поколение, решает медиа-часть по правилам
//! `session_media::page_clock` (ждущее или загруженное); здесь — только «можно ли это
//! сказать странице».
//!
//! **Версия.** Поле `generation` уходит только странице, договорившейся не ниже
//! [`CLOCK_GENERATION_INTRODUCED_IN`]: экстеншен 1.9 отверг бы незнакомое поле (строгий
//! разбор). Страница, чья версия ещё не известна (не было `Attached`), получает тик без поля:
//! тик без номера законен и при 1.10 («номер неизвестен»), а тик с полем странице 1.9 —
//! нарушение протокола.

use browser_bridge_protocol::StreamGeneration;
use browser_bridge_protocol::session::CLOCK_GENERATION_INTRODUCED_IN;

use super::PageOutbound;
use super::resource_requests::PageProtocol;

impl PageOutbound {
    /// Значение поля `generation` тика, описывающего поколение `described`: номер — только
    /// странице 1.10+, иначе поля нет (`None` = ключа нет на проводе).
    pub(super) const fn clock_generation_field(
        &self,
        described: StreamGeneration,
    ) -> Option<StreamGeneration> {
        match self.page_protocol {
            PageProtocol::Agreed(version) if version.includes(CLOCK_GENERATION_INTRODUCED_IN) => {
                Some(described)
            }
            // Страница старше 1.10 номера не знает; версия неизвестна — молчим о номере.
            PageProtocol::Agreed(_) | PageProtocol::NotAttachedYet => None,
        }
    }
}

#[cfg(test)]
#[path = "clock_generation_tests.rs"]
mod tests;
