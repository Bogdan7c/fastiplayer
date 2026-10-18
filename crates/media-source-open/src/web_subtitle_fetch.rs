//! Загрузка web-субтитров (YouTube через yt-dlp) по требованию пользователя.
//!
//! Граница модуля:
//! * наружу — безопасные [`WebSubtitleTrack`] (язык, вид, название, форматы) и
//!   [`WebSubtitleFetchHandle`] с одним намеренным методом `fetch`;
//! * внутри — подписанные ссылки, HTTP и одноразовое повторное извлечение у
//!   yt-dlp, если ссылка протухла (подпись живёт около 7 часов).
//!
//! Решения владельца: в фоне ничего не повторяем (повтор только по клику),
//! автоперевод часто отвечает 429 — это штатная ситуация с отдельной ошибкой.
//! Субтитры на диск yt-dlp не пишет никогда.

mod handle;
mod http;
mod refresh;
#[cfg(test)]
#[cfg(unix)]
pub(crate) mod tests;
mod types;

pub use handle::WebSubtitleFetchHandle;
pub(crate) use refresh::YtDlpSubtitleRefresher;
pub use types::{
    FetchedWebSubtitle, WebSubtitleFetchError, WebSubtitleFetchLimits, WebSubtitleFormat,
    WebSubtitleTrack, WebSubtitleTrackId,
};
