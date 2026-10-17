//! Рисование ASS/SSA, слой 1: стиль, позиционирование, переносы, обводка и тень, слои.
//!
//! Модуль — порт поведения libass 0.17.5 (ISC) от «реплика ASS» до «план для текстового
//! конвейера». Сам конвейер (шейпинг, раскладка, растеризация) общий с SRT/WebVTT и живёт в
//! остальных модулях crate; ASS добавляет только то, что специфично для формата:
//! - [`options`] — публичные настройки: масштаб текста, чей стиль применять, размер хранения видео;
//! - `frame_scale` — масштаб «координаты сценария -> пиксели холста» (`PlayRes`, `LayoutRes`,
//!   размер хранения, `ScaledBorderAndShadow`, пропорции пикселя);
//! - `explicit` — «явные» реплики (`\pos`, `\move`, `\clip`, `\org`, `\p`, Banner/Scroll),
//!   к которым пользовательский масштаб текста не применяется;
//! - `filter_params` — значения `\blur`, `\be`, `\clip`, `\iclip`, `\pbo` (зажатие и «кто побеждает»);
//! - `clip_plan` — перевод клипов реплики из координат сценария в экранные величины для стадии
//!   `crate::clip` (она накладывается после кэша реплик);
//! - `render_state` — состояние рендера и применение тегов (`\r`, `\fs`, `\c`, `\bord` ...);
//! - `look` — перевод состояния в вид символов в пикселях, автор или пользователь;
//! - `event_plan` — обход текста реплики и сборка общего стиля реплики.
//!
//! - `animation` — время: `\move` и `\fad`/`\fade` на момент показа, ключ кэша и ближайшее
//!   изменение картинки (владелец всей логики, зависящей от момента).
//!
//! ## Что НЕ рисуется (сессия 10)
//! `\t` и караоке. Клипы `\clip`/`\iclip` рисуются стадией `crate::clip` (после кэша реплик). Размытие `\blur`/`\be` рисуется: значения попадают в `SpanLook::blur`, само размытие — в `compose` (`run::composite_run`). Рисунки `\p` рисуются: текст в режиме рисунка — один псевдоглиф (см. `event_plan`). Масштаб букв, межбуквенный
//! интервал, синтетическое утолщение/наклон и линии подчёркивания/зачёркивания рисуются
//! общим конвейером по значениям вида символа (`SpanLook`).

mod animation;
mod clip_plan;
mod drawing_scale;
mod event_plan;
mod explicit;
mod filter_params;
mod frame_scale;
mod look;
mod options;
mod render_state;
#[cfg(test)]
mod test_script;

pub(crate) use animation::{
    AssAnimation, AssAnimationKey, AssAnimationState, AssEventClock, evaluate_animation,
};
pub(crate) use clip_plan::EventClipPlan;
pub(crate) use event_plan::plan_ass_event;
pub use options::{
    AssRenderOptions, AssStorageSize, AssStyleSource, AssTextScale, InvalidAssTextScale,
};
