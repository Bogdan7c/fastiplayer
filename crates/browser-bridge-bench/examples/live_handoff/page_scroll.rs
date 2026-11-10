//! Прокрутка страницы к элементу перед передачей (`--scroll-to <селектор>`, 15c).
//!
//! Некоторые сайты ставят плеер ниже края экрана и грузят его, только когда до него докрутят
//! (klonua: iframe плеера с `data-src`). Человек прокручивает страницу колесом; помощник делает то же
//! прокруткой из служебной страницы экстеншена: верхний фрейм вкладки, `scrollIntoView` по селектору.

use browser_bridge_bench::browsers::{BrowserUnderTest, TabHandle};

use crate::youtube_page::ensure_service_page;

/// Скрипт служебной страницы; `__SELECTOR__` подставляется строкой JSON.
const SCROLL_SCRIPT: &str = r#"async (api) => {
  const tabs = (await api.tabs.query({})).filter((tab) => /^https?:/.test(tab.url ?? ""));
  if (tabs.length === 0) return "no site tab";
  const [injection] = await api.scripting.executeScript({
    target: { tabId: tabs[0].id },
    injectImmediately: true,
    args: [__SELECTOR__],
    func: (selector) => {
      const element = document.querySelector(selector);
      if (element === null) return "no element for the selector";
      element.scrollIntoView({ block: "center" });
      return "scrolled to the element";
    },
  });
  return injection?.result ?? "no answer";
}"#;

/// Прокрутить страницу к элементу; итог или сбой печатается, прогон не рвётся.
pub(crate) fn scroll_page_to(
    browser: &mut dyn BrowserUnderTest,
    tab: &TabHandle,
    service_page_open: &mut bool,
    selector: &str,
) {
    if !ensure_service_page(browser, tab, service_page_open, "page scroll") {
        return;
    }
    let script = SCROLL_SCRIPT.replace(
        "__SELECTOR__",
        &serde_json::Value::String(selector.to_owned()).to_string(),
    );
    match browser.run_in_service_page(&script) {
        Ok(reply) => println!("page scroll: {reply}"),
        Err(error) => println!("page scroll: failed: {error}"),
    }
}
