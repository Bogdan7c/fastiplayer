//! Проба опроса фреймов вкладки для живого разбора 15c (`--frame-survey-probe`).
//!
//! Перед передачей служебная страница экстеншена опрашивает вкладку тремя способами и печатает время
//! каждого. Так видно, какой фрейм держит общий ответ `scripting.executeScript` и почему:
//! 1. `all-frames` — тот же вид вызова, что у опроса экстеншена до 15c (все фреймы, ожидание `document_idle`);
//! 2. `immediate` — все фреймы без ожидания загрузки (`injectImmediately`): список фреймов, их
//!    `document.readyState`, хост и есть ли там наш скрипт;
//! 3. `frame <id>` — каждый фрейм из списка `immediate` отдельно, с обычным ожиданием загрузки.
//!
//! Каждая попытка ограничена сроком: ответ, которого нет, печатается как `timed out`. Адреса не печатаются,
//! только хост фрейма, и в отчёты вывод не копируется.

use browser_bridge_bench::browsers::{BrowserUnderTest, TabHandle};

use crate::youtube_page::ensure_service_page;

/// Скрипт служебной страницы. Срок на попытку 8 с — больше срока опроса экстеншена (5 с):
/// так отличаем «отвечает медленно» от «не отвечает вовсе».
const FRAME_SURVEY_PROBE_SCRIPT: &str = r#"async (api) => {
  const tabs = (await api.tabs.query({})).filter((tab) => /^https?:/.test(tab.url ?? ""));
  if (tabs.length === 0) return "no site tab";
  const tabId = tabs[0].id;
  const ATTEMPT_LIMIT_MS = 8000;
  const timed = async (attempt) => {
    const started = performance.now();
    let timer;
    const limit = new Promise((resolve) => { timer = setTimeout(() => resolve({ timedOut: true }), ATTEMPT_LIMIT_MS); });
    const outcome = await Promise.race([attempt().then((results) => ({ results }), (error) => ({ error: String(error) })), limit]);
    clearTimeout(timer);
    return { ms: Math.round(performance.now() - started), ...outcome };
  };
  const describeFrame = (functionName) => {
    // Чем фрейм виден родителю: тег, есть ли src и пустой ли документ. Только форма, без адресов.
    let owner = "cross-origin";
    try {
      const element = window.frameElement;
      if (element === null) owner = "top";
      else {
        // Схема и хост адреса фрейма (без пути), `loading` и `sandbox`: этого хватает, чтобы понять, почему документ не пришёл.
        const source = element.getAttribute("src");
        let target = "none";
        if (source !== null) { try { const parsed = new URL(source, location.href); target = parsed.protocol + "//" + parsed.host; } catch (error) { target = "unparsable"; } }
        owner = element.tagName.toLowerCase() + "[src=" + target + " loading=" + element.getAttribute("loading") + " sandbox=" + element.getAttribute("sandbox") + "]";
      }
    } catch (error) { owner = "hidden"; }
    return {
      host: location.host || location.protocol,
      state: document.readyState,
      script: typeof globalThis[functionName] === "function",
      owner,
      nodes: document.getElementsByTagName("*").length,
    };
  };
  const functionName = "fastiplayerBridgeDescribeVideoCandidates";
  const say = (label, outcome, render) => {
    if (outcome.timedOut) return label + ": timed out after " + outcome.ms + " ms";
    if (outcome.error) return label + ": error after " + outcome.ms + " ms: " + outcome.error;
    return label + ": " + outcome.ms + " ms " + render(outcome.results);
  };
  const lines = [];
  const allFrames = await timed(() => api.scripting.executeScript({ target: { tabId, allFrames: true }, func: describeFrame, args: [functionName] }));
  lines.push(say("all-frames", allFrames, (results) => results.length + " frames"));
  const immediate = await timed(() => api.scripting.executeScript({ target: { tabId, allFrames: true }, injectImmediately: true, func: describeFrame, args: [functionName] }));
  lines.push(say("immediate", immediate, (results) => results.length + " frames"));
  const frames = immediate.results ?? [];
  const perFrame = await Promise.all(frames.filter(Boolean).map(async (frame) => {
    const own = await timed(() => api.scripting.executeScript({ target: { tabId, frameIds: [frame.frameId] }, func: describeFrame, args: [functionName] }));
    const about = frame.result ? frame.result.host + " " + frame.result.state + " script=" + frame.result.script + " owner=" + frame.result.owner + " nodes=" + frame.result.nodes : "no answer";
    return say("frame " + frame.frameId + " (" + about + ")", own, () => "answered");
  }));
  return lines.concat(perFrame).join("\n");
}"#;

/// Напечатать пробу опроса фреймов; сбой пробы — строка в выводе, прогон продолжается.
pub(crate) fn report_frame_survey(
    browser: &mut dyn BrowserUnderTest,
    tab: &TabHandle,
    service_page_open: &mut bool,
) {
    if !ensure_service_page(browser, tab, service_page_open, "frame survey probe") {
        return;
    }
    match browser.run_in_service_page(FRAME_SURVEY_PROBE_SCRIPT) {
        Ok(reply) => {
            for line in reply.lines() {
                println!("frame survey probe: {line}");
            }
        }
        Err(error) => println!("frame survey probe: failed: {error}"),
    }
}
