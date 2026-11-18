//! Проба кандидатов опроса вкладки для живого разбора 16b2 (`--candidate-probe each-attempt`).
//!
//! На лентах коротких видео (TikTok, Instagram) передача в 16b1 отвечала «видео нет» 71 раз из 71, и
//! из чтения кода причину не вывести: отказ дают разные правила (тихий короткий ролик, короткий на
//! паузе, источник, которого перехват не видел, отброшенный список фрейма). Проба перед каждой попыткой
//! передачи печатает три слоя, чтобы один прогон их различил:
//! 1. `frame` — сырой ответ изолированного мира каждого фрейма (`fastiplayerBridgeDescribeVideoCandidates`):
//!    признаки каждого кандидата как есть, плюс `document.visibilityState` фрейма;
//! 2. `element` — сами `<video>` документа глазами изолированного мира (пауза, громкость, `readyState`,
//!    схема адреса): с ними видно, каких элементов нет среди кандидатов и почему признак такой;
//! 3. `survey` — ответ фона на ту же команду `survey_tab`, что шлёт попап: вердикт каждого видео и главное
//!    видео считает боевой код фона, второй копии правил здесь нет.
//!
//! Адреса не печатаются: только хост фрейма и схема адреса медиа. Вывод в отчёты не копируется.

use browser_bridge_bench::browsers::{BrowserUnderTest, TabHandle};

use crate::youtube_page::ensure_service_page;

/// Скрипт служебной страницы. Аргумент — префикс адреса вкладки сайта (как у робота передачи):
/// первая вкладка http(s) не годится, если перед сайтом открыта вкладка прогрева.
const CANDIDATE_PROBE_SCRIPT: &str = r#"async (api, urlPrefix) => {
  const tabs = await api.tabs.query({});
  const tab = tabs.find((candidate) => typeof candidate.url === "string" && candidate.url.startsWith(urlPrefix));
  if (!tab) return "no site tab";
  const lines = ["tab active=" + tab.active + " audible=" + tab.audible + " muted_tab=" + Boolean(tab.mutedInfo && tab.mutedInfo.muted)];
  // Слой 1 и 2: внедрение во все фреймы без ожидания загрузки, как опрос экстеншена с 15c.
  const describeFrame = async (functionName) => {
    const schemeOf = (address) => { try { return address ? new URL(address, location.href).protocol : "none"; } catch (error) { return "unparsable"; } };
    const areaOf = (element) => {
      const rect = element.getBoundingClientRect();
      const width = Math.max(0, Math.min(rect.right, innerWidth) - Math.max(rect.left, 0));
      const height = Math.max(0, Math.min(rect.bottom, innerHeight) - Math.max(rect.top, 0));
      return Math.round(width * height);
    };
    const elements = Array.from(document.getElementsByTagName("video")).map((element) => ({
      paused: element.paused,
      ended: element.ended,
      muted: element.muted,
      volume: element.volume,
      loop: element.loop,
      ready: element.readyState,
      duration: Number.isFinite(element.duration) ? Math.round(element.duration * 10) / 10 : String(element.duration),
      time: Math.round(element.currentTime * 10) / 10,
      scheme: schemeOf(element.currentSrc),
      connected: element.isConnected,
      rect_area_px: areaOf(element),
    }));
    const facts = { host: location.host || location.protocol, visibility: document.visibilityState, elements };
    const describe = globalThis[functionName];
    if (typeof describe !== "function") return JSON.stringify({ ...facts, script: false });
    try {
      return JSON.stringify({ ...facts, script: true, answer: await describe() });
    } catch (error) {
      return JSON.stringify({ ...facts, script: true, error: String(error) });
    }
  };
  let injected = [];
  try {
    injected = await api.scripting.executeScript({ target: { tabId: tab.id, allFrames: true }, injectImmediately: true, func: describeFrame, args: ["fastiplayerBridgeDescribeVideoCandidates"] });
  } catch (error) {
    lines.push("frames: injection failed: " + String(error));
  }
  for (const frame of injected) {
    if (typeof frame.result !== "string") { lines.push("frame " + frame.frameId + ": no answer " + String(frame.error ?? "")); continue; }
    const answer = JSON.parse(frame.result);
    const candidates = answer.answer && Array.isArray(answer.answer.candidates) ? answer.answer.candidates : [];
    lines.push("frame " + frame.frameId + " host=" + answer.host + " visibility=" + answer.visibility + " script=" + answer.script + " videos=" + answer.elements.length + " candidates=" + candidates.length + (answer.error ? " error=" + answer.error : "") + (answer.answer && answer.answer.script_absent ? " script_absent" : ""));
    answer.elements.forEach((element, index) => lines.push("  element " + index + " " + JSON.stringify(element)));
    for (const candidate of candidates) {
      lines.push("  candidate " + JSON.stringify({
        element: candidate.element, media: candidate.media, playing: candidate.playing, area: candidate.visible_area_px,
        duration: candidate.duration_s, muted: candidate.muted, loop: candidate.loop, size: candidate.video_width_px + "x" + candidate.video_height_px,
        drm: candidate.drm, mime: candidate.source_mime_types,
      }));
    }
  }
  // Слой 3: вердикты боевого кода фона — та же команда, что шлёт попап.
  try {
    const reply = await api.runtime.sendMessage({ kind: "survey_tab", tab_id: tab.id });
    if (!reply || reply.kind !== "surveyed") { lines.push("survey: " + JSON.stringify(reply)); }
    else {
      lines.push("survey: main=" + JSON.stringify(reply.main) + " videos=" + reply.videos.length + " in_player=" + reply.in_player.kind);
      for (const video of reply.videos) {
        lines.push("  verdict " + video.verdict + " frame=" + video.frame_id + " " + JSON.stringify({ element: video.element, media: video.media.kind, playing: video.playing, area: video.visible_area_px, duration: video.duration_s, muted: video.muted, loop: video.loop }));
      }
    }
  } catch (error) {
    lines.push("survey: failed: " + String(error));
  }
  return lines.join("\n");
}"#;

/// Напечатать пробу кандидатов с меткой попытки; сбой пробы — строка в выводе, прогон продолжается.
pub(crate) fn report_candidates(
    browser: &mut dyn BrowserUnderTest,
    tab: &TabHandle,
    service_page_open: &mut bool,
    page_url_prefix: &str,
    attempt_label: &str,
) {
    if !ensure_service_page(browser, tab, service_page_open, "candidate probe") {
        return;
    }
    let script = match serde_json::to_string(page_url_prefix) {
        // Префикс уходит литералом JSON, чтобы кавычки в адресе не ломали скрипт.
        Ok(prefix_literal) => {
            format!("async (api) => ({CANDIDATE_PROBE_SCRIPT})(api, {prefix_literal})")
        }
        Err(error) => {
            println!("candidate probe [{attempt_label}]: prefix literal failed: {error}");
            return;
        }
    };
    match browser.run_in_service_page(&script) {
        Ok(reply) => {
            for line in reply.lines() {
                println!("candidate probe [{attempt_label}]: {line}");
            }
        }
        Err(error) => println!("candidate probe [{attempt_label}]: failed: {error}"),
    }
}
