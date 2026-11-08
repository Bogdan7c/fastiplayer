//! Единый источник правды о страницах стенда: их идентификаторы и где `unsupported` допустим.

use crate::browsers::BrowserKind;

/// Страница стенда (`site/pages/<id>.html`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageScenario {
    /// Идентификатор = имя файла без `.html` = поле `scenario` в отчёте страницы.
    pub id: &'static str,
    /// В каких браузерах страница вправе честно ответить `unsupported`
    /// (иначе `unsupported` считается провалом).
    pub unsupported_in: &'static [BrowserKind],
}

impl PageScenario {
    /// Допустим ли ответ `unsupported` в этом браузере.
    pub fn may_be_unsupported_in(&self, browser: BrowserKind) -> bool {
        self.unsupported_in.contains(&browser)
    }
}

const fn page(id: &'static str) -> PageScenario {
    PageScenario {
        id,
        unsupported_in: &[],
    }
}

const fn page_unsupported_in(id: &'static str, browsers: &'static [BrowserKind]) -> PageScenario {
    PageScenario {
        id,
        unsupported_in: browsers,
    }
}

/// Все страницы стенда (контракт 3 сессии 08). Новая страница = строка здесь + файл в `site/pages/`.
pub const PAGE_SCENARIOS: &[PageScenario] = &[
    page("mse-fmp4"),
    page("mse-webm"),
    page("mse-av1"),
    page("mse-muxed"),
    // MSE в Dedicated Worker (MediaSourceHandle) есть только в Chromium.
    page_unsupported_in("mse-worker", &[BrowserKind::Firefox]),
    // 09b: классический воркер, как у Twitch (`new Worker(url)` без type).
    page_unsupported_in("mse-worker-classic", &[BrowserKind::Firefox]),
    page("mse-abr"),
    page("mse-stall-reload"),
    page("mse-generation-swap"),
    page("video-src"),
    // 09b: `<video src>` за входом на CDN — фон качает файл с куками сайта.
    page("video-src-cdn"),
    // 09b: m3u8 прямо в `src` вопреки отказу canPlayType; Firefox на Linux HLS сам не играет.
    page_unsupported_in("m3u8-in-src", &[BrowserKind::Firefox]),
    // 10a2: `blob:`-файл в src — пример формы, которую экстеншен ещё честно не передаёт (`blob_file`).
    page("blob-file"),
    // 10a2: родной HLS VOD — fMP4 с двумя качествами, шифрование AES-128, длинный ролик для
    // команд плеера. Все три играет только Chromium (Firefox на Linux HLS сам не умеет).
    page_unsupported_in("hls-fmp4-abr", &[BrowserKind::Firefox]),
    page_unsupported_in("hls-aes128", &[BrowserKind::Firefox]),
    page_unsupported_in("hls-vod-long", &[BrowserKind::Firefox]),
    // 10a3: родной HLS LIVE (скользящее окно, без ENDLIST, `duration = Infinity`); команда `hls-live`.
    page_unsupported_in("hls-live", &[BrowserKind::Firefox]),
    // 10a2: плейлист публичный, сегменты в нём абсолютные на 127.0.0.1 (сценарий `local-address`).
    page_unsupported_in("hls-local-segments", &[BrowserKind::Firefox]),
    // Нативного HLS в <video> у Firefox на Linux нет.
    page_unsupported_in("native-hls", &[BrowserKind::Firefox]),
    page("shadow-dom"),
    page("iframe-cross-origin"),
    page("cdn-guard"),
    page("ad-preroll"),
    // 09c, живая проверка: форма Ashdi — реклама с домена не из списка поверх ждущего фильма.
    page("ad-beside-film"),
    // 09c2, решения 201 и 204: реклама → фильм в одном окне (сценарий `capture`, судья `ad_hop`).
    page("ad-film-frames"),
    page("ad-film-same-element"),
    page("ad-chain"),
    page("ad-skipped-midway"),
    // 09d, решения 184, 204: органы рекламы в форме PlayerJS, плашка и пропуск из окна плеера.
    page("ad-skip"),
    page("ad-then-banner"),
    // 09c2: реклама кончилась, фильма нет — честный конец через 15 с (сценарий `page-ends`).
    page("ad-no-film"),
    page("banner-autoplay"),
    // 10c: входы правым кликом и по ссылке (сценарий `entries`, строки меню и ссылки).
    page("menu-two-videos"),
    page("menu-taken-contextmenu"),
    page("menu-site-layer"),
    page("link-source"),
    page("link-empty"),
    page("link-manual-play"),
    // 10d: поздний вход — вкладка открыта раньше экстеншена (сценарий `entries`, строки `late-*`).
    page("late-typed-text"),
    page("late-manual-play"),
    page("late-site-refuses-seek"),
    page("late-ad-then-film"),
    page("drm-probe"),
    page("drm-clearkey"),
    page("series"),
    page("spa-navigation"),
    page("pip"),
    // 09c: главное видео на паузе, рядом играют баннер и превью (сценарий `capture`).
    page("banner-with-main"),
    // 09c, решение 202: `<video src>` на `/cdn/media/` (кука И Referer своего сайта).
    page("video-src-referer"),
    // 09c, решение 199: файл на локальном адресе под «публичным» именем (сценарий `local-address`).
    page("video-src-local"),
    // 09c: приманка блокировщика (сценарий `blocker-matrix`).
    page("blocker-bait"),
    // 09c: вредные страницы (сценарий `attacks`).
    page("attack-giant-segments"),
    page("attack-event-flood"),
    page("attack-garbage"),
    page_unsupported_in("attack-worker-flood", &[BrowserKind::Firefox]),
    // Сессия 09b: плеер сайта спрашивает кодеки AV1 → VP9 → H.264; выбор проверяет сценарий `codecs`.
    page("codec-choice"),
    // Д-1: другие способы снять старый источник перед новым MediaSource (сессия 09a2).
    page("mse-generation-swap-srcobject-null"),
    page("mse-generation-swap-reload"),
    // Д-4: данные нового поколения приходят через 5 с.
    page("mse-generation-swap-delayed"),
    // Д-3/Д-8: страница сама забирает видео (проверяет сценарий `page-ends`).
    page("source-withdrawn"),
    page("source-to-file"),
    // Д-2: главный поток страницы замирает на 15 с под тенью (сценарий `page-ends`).
    page("page-freeze"),
    // Д-9: кэшированные до передачи геттер currentTime и play (сценарий `capture`).
    page("cached-descriptors"),
    // Решение 194: живой эфир как Twitch — выгрузка на паузе, загрузка только из обработчика
    // Media Session `play` (сценарий `player-commands`). Воркер — как Twitch в Chromium.
    // 11a, решения 236–238: заглушка, «Return» и честные концы (сценарий `page-return`): рабочий
    // плеер сайта и две «своих шкалы» — поток со смещённым началом и вшитая реклама.
    page("return-kit"),
    page("return-offset"),
    page("return-baked-ad"),
    // 11b2, решения 240–241: возврат закрытой вкладки (сценарий `tab-return`): страница грузится дважды.
    page("tab-return"),
    // 12a: режим 2 «автозапуск» (сценарий `auto-handoff`): страницы ждут настоящего клика по Play.
    page("auto-film"),
    page("auto-clip"),
    page("auto-unknown-duration"),
    page_unsupported_in("auto-live", &[BrowserKind::Firefox]),
    page("auto-hover-preview"),
    page("auto-feed"),
    page("auto-preroll"),
    page("live-unload"),
    page("live-unload-new-ms"),
    page_unsupported_in("live-unload-worker", &[BrowserKind::Firefox]),
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::path::Path;

    #[test]
    fn ids_are_unique_and_url_safe() {
        let mut seen = HashSet::new();
        for scenario in PAGE_SCENARIOS {
            assert!(seen.insert(scenario.id), "duplicate id {}", scenario.id);
            assert!(
                scenario
                    .id
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'),
                "bad id {}",
                scenario.id
            );
        }
    }

    #[test]
    fn unsupported_is_allowed_only_where_declared() {
        let worker = PAGE_SCENARIOS.iter().find(|s| s.id == "mse-worker");
        let worker = worker.expect("mse-worker is in the table");
        assert!(worker.may_be_unsupported_in(BrowserKind::Firefox));
        assert!(!worker.may_be_unsupported_in(BrowserKind::Chromium));
        let plain = PAGE_SCENARIOS.iter().find(|s| s.id == "mse-fmp4");
        let plain = plain.expect("mse-fmp4 is in the table");
        assert!(!plain.may_be_unsupported_in(BrowserKind::Firefox));
    }

    /// Каждая страница таблицы существует в `site/pages/` (и наоборот — нет «сирот»).
    #[test]
    fn table_matches_the_page_files_on_disk() {
        let pages_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("site/pages");
        if !pages_dir.is_dir() {
            return; // страницы ещё не написаны (пакет B)
        }
        let on_disk: HashSet<String> = std::fs::read_dir(&pages_dir)
            .expect("read pages dir")
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                name.strip_suffix(".html").map(str::to_owned)
            })
            .collect();
        for scenario in PAGE_SCENARIOS {
            assert!(
                on_disk.contains(scenario.id),
                "page {} is listed but site/pages/{}.html is missing",
                scenario.id,
                scenario.id
            );
        }
        // Страницы без самостоятельного прогона в `pages`: `iframe-inner` вложена в другие,
        // `replay-recording` запускается только командой `replay` (ей нужен файл записи).
        const HELPER_PAGES: &[&str] = &["iframe-inner", "ad-frame-inner", "replay-recording"];
        for file_id in &on_disk {
            assert!(
                HELPER_PAGES.contains(&file_id.as_str())
                    || PAGE_SCENARIOS.iter().any(|s| s.id == file_id),
                "site/pages/{file_id}.html is not in PAGE_SCENARIOS"
            );
        }
    }
}
