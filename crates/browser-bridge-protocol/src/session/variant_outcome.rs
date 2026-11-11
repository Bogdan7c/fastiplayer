//! Исход выбора на странице (протокол 1.8, сессия 15c2, решения 298, 301).
//!
//! Плеер просит страницу выполнить выбор зрителя командами 1.0 `select_variant` и
//! `next_episode`, а страница может его не выполнить: у новой озвучки нет этой серии
//! (решение 295), сайт принимает только настоящий клик человека, кнопки не сработали.
//! С 1.8 экстеншен отвечает на каждую такую просьбу сообщением `variant_outcome`:
//!
//! ```json
//! {"type":"variant_outcome","request":{"kind":"select_variant","menu":"audio","variant_id":"dub-b"},"outcome":"no_such_episode"}
//! {"type":"variant_outcome","request":{"kind":"next_episode"},"outcome":"done"}
//! ```
//!
//! - `request` — на какую просьбу плеера это ответ: тот же вид меню и пункт, что в
//!   `select_variant`, или `next_episode`. Плеер по нему узнаёт свой ожидающий выбор;
//!   ответ на устаревшую просьбу (зритель успел выбрать другое) он пропускает.
//! - `outcome` — что сделала страница ([`VariantOutcome`]).
//!
//! Шлётся только исход выбора ЗРИТЕЛЯ из окна плеера; свой выбор экстеншена (память выбора,
//! 14b2) плееру не отчитывается. Сообщение не привязано к поколению потока (как
//! `page_navigation`). При общей версии ниже 1.8 экстеншен его не шлёт, а сторож плеера
//! отвергает как нарушение — плеер тогда ведёт себя как в 1.7 (сразу «Audio: …»).

use serde::{Deserialize, Serialize};

use crate::session::variants::VariantKind;
use crate::strict_shape::{strict_label, strict_object};
use crate::text::VariantId;
use crate::version::ProtocolVersion;

// Строгая форма JSON (`crate::strict_shape`): просьба — только объектом, исход — только строкой.
strict_object!(VariantRequest);
strict_label!(VariantOutcome);

/// Версия протокола, в которой появился `variant_outcome`.
pub const VARIANT_OUTCOME_INTRODUCED_IN: ProtocolVersion = ProtocolVersion::V1_8;

/// На какую просьбу плеера отвечает страница.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(
    remote = "Self",
    tag = "kind",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum VariantRequest {
    /// Ответ на `select_variant`: те же меню и пункт, что были в просьбе.
    SelectVariant {
        /// Какое меню (поле `kind` в `select_variant`; здесь имя занято меткой просьбы).
        menu: VariantKind,
        /// Какой пункт.
        variant_id: VariantId,
    },
    /// Ответ на `next_episode`.
    NextEpisode {},
}

/// Что сделала страница с просьбой плеера.
///
/// Значения согласованы с решением 301. Ничего не случилось — это не исход: страница
/// отвечает ровно один раз на каждую просьбу, которую она приняла к исполнению.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(remote = "Self", rename_all = "snake_case")]
pub enum VariantOutcome {
    /// Страница выполнила просьбу.
    Done,
    /// У выбранной озвучки нет текущей серии (решение 295): страница осталась, где была.
    /// Только для `select_variant`; экстеншен не называет так «пункта больше нет в меню».
    NoSuchEpisode,
    /// Сайт принимает этот выбор только от настоящего человека (настоящая мышь, жест):
    /// зрителю нужно выбрать на самой странице.
    PersonOnly,
    /// Не вышло по другой причине: пункт пропал из меню, кнопки не сработали, адаптер упал,
    /// следующей серии уже нет.
    Failed,
}
