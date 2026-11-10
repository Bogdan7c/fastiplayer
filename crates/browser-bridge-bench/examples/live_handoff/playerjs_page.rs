//! Проба страницы с плеером PlayerJS (Ashdi, Tortuga) для живой проверки адаптера 15b.
//!
//! Раз в `--playerjs-report-every` секунд служебная страница экстеншена заглядывает во ВСЕ фреймы вкладки
//! (плеер живёт в чужом iframe) и печатает, что стоит в плеере сайта: id файла плейлиста, подпись серии,
//! качество, дорожку звука и заголовки выпадающих списков (сезон, серия, озвучка). Рядом — записи памяти
//! выбора экстеншена (подписи озвучки и качества). Так видно, что выбор в окне плеера дошёл до страницы,
//! а память вернула озвучку на следующей серии. Адреса не печатаются: только хост фрейма.
//!
//! `--playerjs-pick "40:Сезон 1/Серія 3"` — через столько секунд после передачи «человек» выбирает на
//! странице пункты плейлиста по подписям, по очереди (папка сезона, потом серия), как пульт под нашим
//! окном. Сайты открывают последнюю серию, а у других озвучек её часто нет: без этого смену озвучки на
//! сериале не проверить.

use std::time::Duration;

use browser_bridge_bench::browsers::{BrowserUnderTest, TabHandle};

use crate::youtube_page::ensure_service_page;

/// Скрипт служебной страницы: все фреймы вкладок с сайтами, MAIN-мир, только фреймы с PlayerJS.
const PLAYERJS_REPORT_SCRIPT: &str = r#"async (api) => {
  const tabs = (await api.tabs.query({})).filter((tab) => /^https?:/.test(tab.url ?? ""));
  if (tabs.length === 0) return "no site tab";
  const reports = [];
  for (const tab of tabs) {
    const injections = await api.scripting.executeScript({
      target: { tabId: tab.id, allFrames: true },
      world: "MAIN",
      func: () => {
        const player = (window.pljssglobal ?? []).find((candidate) => typeof candidate?.api === "function");
        if (!player) return null;
        const ask = (command) => { try { const value = player.api(command); return typeof value === "object" ? JSON.stringify(value) : String(value); } catch (error) { return "error"; } };
        const heads = [...document.querySelectorAll("pjsdiv[me^='head_']")].map((head) => head.textContent.trim()).join(" | ");
        return location.host + ": file=" + ask("playlist_id") + " title=" + ask("playlist_title").trim() + " quality=" + ask("quality") + " audio=" + ask("audiotrack") + " heads=[" + heads + "] time=" + Math.round(Number(ask("time")));
      },
    });
    for (const injection of injections) if (injection && injection.result) reports.push(injection.result);
  }
  const stored = (await api.storage.local.get("rememberedChoices")).rememberedChoices;
  const records = stored && stored.series ? Object.values(stored.series) : [];
  const remembered = records.map((record) => "audio:" + (record.audio ?? "-") + "/quality:" + (record.quality ?? "-")).join(";");
  return (reports.join(" || ") || "no PlayerJS frame") + " memory=" + records.length + "[" + remembered + "]";
}"#;

/// Напечатать пробу PlayerJS; сбой пробы — строка в выводе, прогон продолжается.
pub(crate) fn report_playerjs_page(
    browser: &mut dyn BrowserUnderTest,
    tab: &TabHandle,
    service_page_open: &mut bool,
) {
    if !ensure_service_page(browser, tab, service_page_open, "playerjs page") {
        return;
    }
    match browser.run_in_service_page(PLAYERJS_REPORT_SCRIPT) {
        Ok(reply) => println!("playerjs page: {reply}"),
        Err(error) => println!("playerjs page: failed: {error}"),
    }
}

/// Скрипт служебной страницы: нажать пункты плейлиста PlayerJS с подписями `__LABELS__` по очереди.
const PLAYERJS_PICK_SCRIPT: &str = r#"async (api) => {
  const tabs = (await api.tabs.query({})).filter((tab) => /^https?:/.test(tab.url ?? ""));
  const labels = __LABELS__;
  const results = [];
  for (const tab of tabs) {
    const injections = await api.scripting.executeScript({
      target: { tabId: tab.id, allFrames: true },
      world: "MAIN",
      args: [labels],
      func: async (wanted) => {
        if (!document.querySelector("pjsdiv[me]")) return null;
        const pressed = [];
        for (const label of wanted) {
          const item = [...document.querySelectorAll("pjsdiv[me]")].find((candidate) => !candidate.getAttribute("me").startsWith("head_") && candidate.textContent.trim() === label);
          if (!item) { pressed.push(label + ": not found"); break; }
          const box = item.getBoundingClientRect();
          for (const type of ["pointerdown", "mousedown", "pointerup", "mouseup", "click"]) item.dispatchEvent(new MouseEvent(type, { bubbles: true, cancelable: true, view: window, clientX: box.x + 5, clientY: box.y + 5 }));
          pressed.push(label);
          await new Promise((resolve) => setTimeout(resolve, 1500));
        }
        return pressed.join(" > ");
      },
    });
    for (const injection of injections) if (injection && injection.result) results.push(injection.result);
  }
  return results.join(" || ") || "no PlayerJS frame";
}"#;

/// Выбор на странице из `--playerjs-pick`: когда (после передачи) и какие пункты по подписям.
pub(crate) struct PlayerJsPick {
    pub(crate) after: Duration,
    pub(crate) labels: Vec<String>,
}

/// Разбор `--playerjs-pick "<секунды>:<подпись>/<подпись>"`; подписи — только буквы, цифры, пробелы и `-`.
pub(crate) fn parse_playerjs_pick(value: &str) -> Result<PlayerJsPick, String> {
    let (after, labels) = value
        .split_once(':')
        .ok_or_else(|| format!("--playerjs-pick expects <seconds>:<labels>, got {value}"))?;
    let after: u64 = after
        .parse()
        .map_err(|error| format!("--playerjs-pick {value}: {error}"))?;
    let labels: Vec<String> = labels
        .split('/')
        .map(|label| label.trim().to_owned())
        .collect();
    let plain = |label: &String| {
        !label.is_empty()
            && label
                .chars()
                .all(|symbol| symbol.is_alphanumeric() || symbol == ' ' || symbol == '-')
    };
    if labels.iter().all(plain) {
        Ok(PlayerJsPick {
            after: Duration::from_secs(after),
            labels,
        })
    } else {
        Err(format!(
            "--playerjs-pick {value}: labels separated by '/', letters and digits only"
        ))
    }
}

/// Нажать на странице пункты плейлиста по подписям, как человек на странице-пульте.
pub(crate) fn pick_on_playerjs_page(
    browser: &mut dyn BrowserUnderTest,
    tab: &TabHandle,
    service_page_open: &mut bool,
    labels: &[String],
) {
    if !ensure_service_page(browser, tab, service_page_open, "playerjs pick") {
        return;
    }
    // Подписи уже проверены `parse_playerjs_pick`: кавычек и обратных косых в них нет.
    let list = labels
        .iter()
        .map(|label| format!("\"{label}\""))
        .collect::<Vec<_>>()
        .join(",");
    let script = PLAYERJS_PICK_SCRIPT.replace("__LABELS__", &format!("[{list}]"));
    match browser.run_in_service_page(&script) {
        Ok(reply) => println!("playerjs pick: {reply}"),
        Err(error) => println!("playerjs pick: failed: {error}"),
    }
}
