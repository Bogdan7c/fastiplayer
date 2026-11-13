//! Проба конца эфира (16d1): что плеер сайта делает с `MediaSource` и `<video>`, когда эфир кончается
//! или становится записью. Ставит наблюдатель в ГЛАВНОМ мире каждого фрейма вкладки (через служебную
//! страницу экстеншена) и при каждом вызове печатает новые события и состояние всех `<video>`.
//!
//! Что видит наблюдатель:
//! - вызовы `MediaSource`: запись `duration`, `endOfStream`, `setLiveSeekableRange`,
//!   `clearLiveSeekableRange`, `addSourceBuffer`, `removeSourceBuffer`, события `sourceended` /
//!   `sourceclose` и новый `MediaSource`, отданный элементу через `URL.createObjectURL`;
//! - события элемента (`durationchange`, `emptied`, `loadstart`, `ended` и др.) — пассивно, слушателем.
//!
//! Свойства элемента (`src`, `srcObject`, `load`) наблюдатель НЕ оборачивает: их подменяет тень моста,
//! вторая подмена поверх могла бы изменить её поведение. Методы `MediaSource` хук моста уже оборачивает с
//! вызовом оригинала, ещё одна такая обёртка ничего не меняет. Адресов наблюдатель не печатает: только
//! имя хоста фрейма, вид источника (`blob`/`url`/`none`) и числа.

use std::time::Duration;

use browser_bridge_bench::browsers::{BrowserUnderTest, TabHandle};

use crate::youtube_page::ensure_service_page;

/// Сколько событий наблюдатель держит в одном фрейме: эфир шлёт `addSourceBuffer` редко, а события
/// элемента — единицы в минуту, поэтому предел срабатывает только на зацикленной странице.
const MAX_EVENTS_PER_FRAME: usize = 4_000;

/// Шаблон функции служебной страницы. `__TAB_PATTERN__` — шаблон адреса вкладки (`*://<хост>/*`),
/// `__MAX_EVENTS__` — предел журнала. Возвращает по строке JSON на фрейм.
const LIVE_END_PROBE_SCRIPT: &str = r#"async (api) => {
  const tabs = await api.tabs.query({ url: "__TAB_PATTERN__" });
  if (tabs.length === 0) return "no tab for __TAB_PATTERN__";
  const injections = await api.scripting.executeScript({
    target: { tabId: tabs[0].id, allFrames: true },
    world: "MAIN",
    args: [__MAX_EVENTS__],
    func: (maxEvents) => {
      if (!window.__fastiplayerLiveEndProbe) {
        const log = [];
        const push = (kind, extra) => {
          if (log.length < maxEvents) log.push({ t_ms: Math.round(performance.now()), kind, ...extra });
        };
        const prototype = MediaSource.prototype;
        const durationOf = Object.getOwnPropertyDescriptor(prototype, "duration");
        const sourceIds = new WeakMap();
        let nextSourceId = 1;
        const idOf = (source) => {
          if (!sourceIds.has(source)) {
            sourceIds.set(source, nextSourceId++);
            for (const name of ["sourceopen", "sourceended", "sourceclose"]) {
              source.addEventListener(name, () => push("ms." + name, { ms: sourceIds.get(source), duration: String(durationOf.get.call(source)) }));
            }
          }
          return sourceIds.get(source);
        };
        Object.defineProperty(prototype, "duration", {
          configurable: true,
          enumerable: durationOf.enumerable,
          get() { return durationOf.get.call(this); },
          set(value) {
            push("ms.duration=", { ms: idOf(this), value: String(value), was: String(durationOf.get.call(this)) });
            durationOf.set.call(this, value);
          },
        });
        for (const name of ["endOfStream", "setLiveSeekableRange", "clearLiveSeekableRange", "addSourceBuffer", "removeSourceBuffer"]) {
          const original = prototype[name];
          if (typeof original !== "function") continue;
          prototype[name] = function (...callArguments) {
            const shown = callArguments.map((value) => (typeof value === "object" ? "object" : String(value))).join(",").slice(0, 80);
            push("ms." + name, { ms: idOf(this), args: shown, ready: this.readyState, duration: String(durationOf.get.call(this)) });
            return original.apply(this, callArguments);
          };
        }
        const createObjectUrl = URL.createObjectURL;
        URL.createObjectURL = function (object) {
          const url = createObjectUrl.call(this, object);
          if (object instanceof MediaSource) push("createObjectURL(ms)", { ms: idOf(object) });
          return url;
        };
        for (const name of ["durationchange", "emptied", "loadstart", "loadedmetadata", "ended", "abort", "error", "waiting", "stalled", "pause", "play"]) {
          document.addEventListener(name, (event) => {
            const element = event.target;
            if (!(element instanceof HTMLMediaElement)) return;
            push("el." + name, { duration: String(element.duration), at_s: Math.round((element.currentTime || 0) * 10) / 10 });
          }, true);
        }
        window.__fastiplayerLiveEndProbe = { log, read: 0, installed_ms: Math.round(performance.now()) };
      }
      const probe = window.__fastiplayerLiveEndProbe;
      const fresh = probe.log.slice(probe.read);
      probe.read = probe.log.length;
      const kindOf = (element) => (element.currentSrc.startsWith("blob:") ? "blob" : element.currentSrc ? "url" : "none");
      const rangeOf = (ranges) => (ranges.length ? ranges.start(0).toFixed(0) + "-" + ranges.end(ranges.length - 1).toFixed(0) : "none");
      const videos = [...document.querySelectorAll("video")].map((element) =>
        `duration=${element.duration} at=${element.currentTime.toFixed(1)} ready=${element.readyState} paused=${element.paused} ended=${element.ended} src=${kindOf(element)} seekable=${rangeOf(element.seekable)} buffered=${rangeOf(element.buffered)}`);
      return JSON.stringify({ frame: location.hostname, installed_ms: probe.installed_ms, videos, fresh });
    },
  });
  return injections.map((injection) => String(injection && injection.result)).join("\n");
}"#;

/// Шаблон адреса вкладки для `tabs.query` по адресу, который открыл помощник (`*://<хост>/*`).
pub(crate) fn tab_pattern_of(url: &str) -> Result<String, String> {
    let without_scheme = url
        .split_once("://")
        .map(|(_, rest)| rest)
        .ok_or_else(|| format!("--live-end-probe-every: {url} has no scheme"))?;
    let host = without_scheme
        .split(['/', '?', '#'])
        .next()
        .filter(|host| !host.is_empty())
        .ok_or_else(|| format!("--live-end-probe-every: {url} has no host"))?;
    // Хост попадает в текст скрипта: пропускаем только символы имени хоста.
    if !host
        .chars()
        .all(|symbol| symbol.is_ascii_alphanumeric() || matches!(symbol, '.' | '-' | ':'))
    {
        return Err(format!("--live-end-probe-every: unexpected host {host}"));
    }
    Ok(format!("*://{host}/*"))
}

/// Проба `--live-end-probe-every <с>`: первый вызов ставит наблюдатель, каждый — печатает новое.
/// Сбой печатается, прогон не рвётся: это помощник разработчика.
pub(crate) fn report_live_end(
    browser: &mut dyn BrowserUnderTest,
    tab: &TabHandle,
    service_page_open: &mut bool,
    tab_pattern: &str,
    since_hand_off: Duration,
) {
    if !ensure_service_page(browser, tab, service_page_open, "live end probe") {
        return;
    }
    let script = LIVE_END_PROBE_SCRIPT
        .replace("__TAB_PATTERN__", tab_pattern)
        .replace("__MAX_EVENTS__", &MAX_EVENTS_PER_FRAME.to_string());
    let at = since_hand_off.as_secs_f64();
    match browser.run_in_service_page(&script) {
        Ok(reply) => {
            for frame in reply.lines() {
                println!("[{at:>7.1} s] live end probe: {frame}");
            }
        }
        Err(error) => println!("[{at:>7.1} s] live end probe: failed: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tab_pattern_keeps_only_the_host() {
        assert_eq!(
            tab_pattern_of("https://www.twitch.tv/somebody?x=1").as_deref(),
            Ok("*://www.twitch.tv/*")
        );
    }

    #[test]
    fn a_host_with_script_characters_is_refused() {
        assert!(tab_pattern_of("https://evil\"host/").is_err());
    }
}
