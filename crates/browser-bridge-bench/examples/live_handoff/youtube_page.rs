//! Страница YouTube из помощника живой проверки: смена качества его же API (13c) и проба «что сейчас на
//! странице» (15a) — какой ролик, какая озвучка и качество. Скрипты работают через служебную страницу
//! экстеншена в ГЛАВНОМ мире вкладки YouTube; адресов и названий проба не печатает, только id ролика,
//! код и подпись озвучки и уровни качества.

use std::time::Duration;

use browser_bridge_bench::browsers::{BrowserUnderTest, TabHandle};

/// Смена качества на странице YouTube (13c): API плеера сайта, как ручной выбор в его меню.
pub(crate) struct YoutubeQualityStep {
    /// Через сколько после передачи.
    pub(crate) after: Duration,
    /// Уровень YouTube: `tiny` (144p), `small` (240p), `medium` (360p), `large` (480p), `hd720`…
    pub(crate) level: String,
}

/// Функция служебной страницы: находит вкладку YouTube и в ГЛАВНОМ мире страницы просит её плеер
/// держать уровень `__LEVEL__` (`setPlaybackQualityRange`). Возвращает строку для журнала помощника.
const YOUTUBE_QUALITY_SCRIPT: &str = r#"async (api) => {
  const tabs = await api.tabs.query({ url: "*://*.youtube.com/*" });
  if (tabs.length === 0) return "no YouTube tab";
  const [injection] = await api.scripting.executeScript({
    target: { tabId: tabs[0].id },
    world: "MAIN",
    args: ["__LEVEL__"],
    func: (level) => {
      const player = document.getElementById("movie_player");
      if (!player || typeof player.setPlaybackQualityRange !== "function") return "no player API";
      player.setPlaybackQualityRange(level, level);
      return "asked " + level + ", the page says " + player.getPlaybackQuality();
    },
  });
  return String(injection && injection.result);
}"#;

/// Функция служебной страницы (15a): что сейчас на вкладке YouTube — id ролика, озвучка (код языка и
/// подпись из вложенного описания дорожки, найденного по форме, как у адаптера), выбор качества человеком
/// и качество на экране, число вызовов `setAudioTrack` через API плеера с первой пробы (их делает только
/// адаптер: выбор зрителя и память). Возвращает одну строку для журнала помощника.
const YOUTUBE_REPORT_SCRIPT: &str = r#"async (api) => {
  const tabs = await api.tabs.query({ url: "*://*.youtube.com/*" });
  if (tabs.length === 0) return "no YouTube tab";
  const [injection] = await api.scripting.executeScript({
    target: { tabId: tabs[0].id },
    world: "MAIN",
    func: () => {
      const player = [...document.querySelectorAll(".html5-video-player")].find((candidate) => candidate.getVideoData?.()?.video_id);
      if (!player) return "no active player";
      // Счётчик вызовов setAudioTrack через API элемента: так зовёт только адаптер моста (YouTube сам
      // переносит язык внутри себя). Обёртка ставится один раз и переживает переход на следующий ролик.
      if (!player.__fastiplayerProbeWrapped && typeof player.setAudioTrack === "function") {
        const original = player.setAudioTrack;
        window.__fastiplayerProbeAudioSets = 0;
        player.setAudioTrack = function (...callArguments) { window.__fastiplayerProbeAudioSets += 1; return original.apply(this, callArguments); };
        player.__fastiplayerProbeWrapped = true;
      }
      const track = player.getAudioTrack?.();
      const info = track && Object.values(track).find((value) => value && typeof value.name === "string" && typeof value.id === "string");
      const audio = info ? info.id.split(".")[0] + " (" + info.name + ")" : "none";
      const languages = (player.getAvailableAudioTracks?.() ?? []).map((candidate) => Object.values(candidate).find((value) => value && typeof value.id === "string" && typeof value.name === "string")?.id.split(".")[0]).filter(Boolean).join(",");
      // Субтитры (15a2): что включено у YouTube и сколько документов timedtext он скачал (размер тела, без адресов).
      let captionTrack = "none";
      try { const current = player.getOption?.("captions", "track"); if (current && current.vss_id) captionTrack = current.vss_id + (current.translationLanguage ? ">" + current.translationLanguage.languageCode : ""); } catch (error) { captionTrack = "unavailable"; }
      const timedtext = performance.getEntriesByType("resource").filter((entry) => entry.name.includes("/api/timedtext")).map((entry) => String(entry.encodedBodySize)).join(",");
      return "video=" + player.getVideoData().video_id + " audio=" + audio + " api_audio_sets=" + window.__fastiplayerProbeAudioSets + " languages=" + (languages || "none") + " preference=" + player.getUserPlaybackQualityPreference?.() + " on screen=" + player.getPlaybackQuality?.() + " captions=" + captionTrack + " timedtext_bodies=[" + timedtext + "]";
    },
  });
  // Память выбора экстеншена (14b2): сколько сериалов запомнено и какие подписи — без отпечатков.
  const stored = (await api.storage.local.get("rememberedChoices")).rememberedChoices;
  const records = stored && stored.series ? Object.values(stored.series) : [];
  const remembered = records.map((record) => "audio:" + (record.audio ?? "-") + "/quality:" + (record.quality ?? "-")).join(";");
  return String(injection && injection.result) + " memory=" + records.length + "[" + remembered + "]";
}"#;

/// Открыть служебную страницу один раз за прогон; `false` — не открылась (причина напечатана).
pub(crate) fn ensure_service_page(
    browser: &mut dyn BrowserUnderTest,
    tab: &TabHandle,
    service_page_open: &mut bool,
    what: &str,
) -> bool {
    if *service_page_open {
        return true;
    }
    match browser.open_service_page(tab) {
        Ok(()) => {
            *service_page_open = true;
            true
        }
        Err(error) => {
            println!("{what}: the service page did not open: {error}");
            false
        }
    }
}

/// Пробa страницы YouTube (`--youtube-report-every`): итог или сбой печатается, прогон не рвётся.
pub(crate) fn report_youtube_page(
    browser: &mut dyn BrowserUnderTest,
    tab: &TabHandle,
    service_page_open: &mut bool,
) {
    if !ensure_service_page(browser, tab, service_page_open, "youtube page") {
        return;
    }
    match browser.run_in_service_page(YOUTUBE_REPORT_SCRIPT) {
        Ok(reply) => println!("youtube page: {reply}"),
        Err(error) => println!("youtube page: failed: {error}"),
    }
}

/// Шаг `--youtube-quality <с>:<уровень>`. Уровень — только буквы и цифры: он попадает в текст скрипта.
pub(crate) fn parse_youtube_quality(value: &str) -> Result<YoutubeQualityStep, String> {
    let (after, level) = value
        .split_once(':')
        .ok_or_else(|| format!("--youtube-quality expects <seconds>:<level>, got {value}"))?;
    let after: u64 = after
        .parse()
        .map_err(|error| format!("--youtube-quality {value}: {error}"))?;
    if level.is_empty() || !level.chars().all(|symbol| symbol.is_ascii_alphanumeric()) {
        return Err(format!(
            "--youtube-quality {value}: the level must be like small or hd720"
        ));
    }
    Ok(YoutubeQualityStep {
        after: Duration::from_secs(after),
        level: level.to_owned(),
    })
}

/// Просит плеер YouTube на вкладке сменить качество (служебная страница экстеншена открывается
/// фоновой один раз). Итог и сбои печатаются: это помощник человека.
pub(crate) fn ask_youtube_quality(
    browser: &mut dyn BrowserUnderTest,
    tab: &TabHandle,
    service_page_open: &mut bool,
    level: &str,
) {
    if !ensure_service_page(
        browser,
        tab,
        service_page_open,
        &format!("youtube quality {level}"),
    ) {
        return;
    }
    let script = YOUTUBE_QUALITY_SCRIPT.replace("__LEVEL__", level);
    match browser.run_in_service_page(&script) {
        Ok(reply) => println!("youtube quality {level}: {reply}"),
        Err(error) => println!("youtube quality {level}: failed: {error}"),
    }
}
