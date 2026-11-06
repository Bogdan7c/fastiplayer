// Плашка с итогом входа (сессия 10c): кто может её показать, как она живёт и что реально нарисовано.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { BANNER_AUTO_HIDE_MS } from "../../src/content/page-ui/banner-controller.ts";
import { UI_HOST_TAG_NAME } from "../../src/content/page-ui/ui-layer.ts";
import { SHOW_ENTRY_RESULT_KIND, type EntryReport } from "../../src/shared/entry-contract.ts";
import { FakeElement, FakeRoot, FakeUiEvent } from "./support/fake-ui-dom.ts";
import { OWN_EXTENSION_ID, setupPageUi, type PageUiHarness } from "./support/page-ui-harness.ts";

const OPENED: EntryReport = { id: "opened", tone: "success", headline: "Opened in Fastiplayer", details: [] };
const NO_VIDEO: EntryReport = { id: "no_video", tone: "refusal", headline: "No video to hand over yet", details: ["Press play on the page, then try again."] };
const FROM_BACKGROUND = { id: OWN_EXTENSION_ID };

function deliver(harness: PageUiHarness, report: EntryReport, sender: { id?: string; tab?: unknown } = FROM_BACKGROUND): void {
  harness.ui.onMessage({ kind: SHOW_ENTRY_RESULT_KIND, report }, sender);
}

/** Тексты, которые человек видит в плашке. */
function visibleTexts(harness: PageUiHarness): string[] {
  return harness.doc.shadowElements().map((element) => element.textContent).filter((text): text is string => text !== null);
}

describe("плашка: приём сообщений", () => {
  it("сообщение своего фона показывает плашку с заголовком и деталями", async () => {
    const harness = await setupPageUi();
    deliver(harness, NO_VIDEO);
    assert.deepEqual(visibleTexts(harness).slice(0, 2), ["No video to hand over yet", "Press play on the page, then try again."]);
    assert.equal(harness.ui.probe().banner, "no_video");
  });

  it("чужой отправитель, сообщение от контент-скрипта вкладки и кривая форма игнорируются", async () => {
    const harness = await setupPageUi();
    deliver(harness, OPENED, { id: "someone-else" });
    deliver(harness, OPENED, { id: OWN_EXTENSION_ID, tab: { id: 3 } });
    deliver(harness, OPENED, {});
    harness.ui.onMessage({ kind: SHOW_ENTRY_RESULT_KIND, report: { id: "x", tone: "loud", headline: "h", details: [] } }, FROM_BACKGROUND);
    harness.ui.onMessage({ kind: "something_else" }, FROM_BACKGROUND);
    harness.ui.onMessage("text", FROM_BACKGROUND);
    assert.equal(harness.ui.probe().banner, null);
    assert.equal(harness.doc.hosts.length, 0, "ни одного нашего узла в документе");
  });

  it("плашку показывает только верхний фрейм", async () => {
    const nested = await setupPageUi({ isTopFrame: false });
    deliver(nested, OPENED);
    assert.equal(nested.ui.probe().banner, null);
    assert.equal(nested.doc.hosts.length, 0);
  });
});

describe("плашка: жизнь", () => {
  it("автоскрытие: success через 5 с, refusal через 8 с — не раньше", async () => {
    const success = await setupPageUi();
    deliver(success, OPENED);
    await success.advance(BANNER_AUTO_HIDE_MS.success - 1);
    assert.equal(success.ui.probe().banner, "opened");
    await success.advance(1);
    assert.equal(success.ui.probe().banner, null);
    assert.deepEqual(visibleTexts(success), []);

    const refusal = await setupPageUi();
    deliver(refusal, NO_VIDEO);
    await refusal.advance(BANNER_AUTO_HIDE_MS.success);
    assert.equal(refusal.ui.probe().banner, "no_video", "отказ держится дольше успеха");
    await refusal.advance(BANNER_AUTO_HIDE_MS.refusal - BANNER_AUTO_HIDE_MS.success);
    assert.equal(refusal.ui.probe().banner, null);
  });

  it("новая плашка заменяет старую, а таймер старой не гасит новую раньше её срока", async () => {
    const harness = await setupPageUi();
    deliver(harness, NO_VIDEO); // таймер 8 с от t=0
    await harness.advance(7_000);
    deliver(harness, OPENED); // таймер 5 с от t=7 с, старый снят
    assert.equal(harness.ui.probe().banner, "opened");
    assert.equal(visibleTexts(harness).includes("No video to hand over yet"), false, "старая убрана из DOM");
    await harness.advance(1_500); // t=8,5 с: срок старого таймера прошёл
    assert.equal(harness.ui.probe().banner, "opened", "новая плашка пережила таймер старой");
    await harness.advance(3_500); // t=12 с: срок новой (7+5)
    assert.equal(harness.ui.probe().banner, null);
    assert.equal(harness.clock.liveTimers(), 0);
  });

  it("«×» убирает плашку сразу и снимает её таймер", async () => {
    const harness = await setupPageUi();
    deliver(harness, NO_VIDEO);
    const close = harness.doc.shadowElements().find((element) => element.attributes.get("aria-label") === "Close");
    assert.ok(close, "кнопка закрытия нарисована");
    close.dispatch("click", new FakeUiEvent(true));
    assert.equal(harness.ui.probe().banner, null);
    assert.deepEqual(visibleTexts(harness), []);
    assert.equal(harness.clock.liveTimers(), 0);
  });

  it("«×» отправляет только то, что показано: прежняя плашка уже убрана из DOM", async () => {
    const harness = await setupPageUi();
    deliver(harness, NO_VIDEO);
    const oldClose = harness.doc.shadowElements().find((element) => element.attributes.get("aria-label") === "Close") as FakeElement;
    deliver(harness, OPENED);
    oldClose.dispatch("click", new FakeUiEvent(true)); // слушатель старой кнопки снят
    assert.equal(harness.ui.probe().banner, "opened");
  });
});

describe("слой страницы: хост", () => {
  it("хост: неопределённый тег, закрытая тень, popover manual, поток страницы не затронут (инлайн с !important)", async () => {
    const harness = await setupPageUi();
    assert.equal(harness.doc.hosts.length, 0, "до первого показа узлов нет");
    deliver(harness, OPENED);
    const host = harness.doc.hosts[0];
    assert.ok(host);
    assert.equal(host.tagName, UI_HOST_TAG_NAME);
    assert.equal(host.shadowMode, "closed");
    assert.equal(host.attributes.get("popover"), "manual");
    assert.equal(host.popoverOpen, true);
    assert.ok(host.isConnected);
    for (const [name, expected] of [["position", "fixed"], ["width", "0"], ["height", "0"], ["display", "block"], ["all", "initial"]] as const) {
      assert.equal(host.css(name), `${expected} !important`, name);
    }
  });

  it("браузер без Popover API: слой работает на position: fixed и не падает", async () => {
    const harness = await setupPageUi({ supportsPopover: false });
    deliver(harness, OPENED);
    const host = harness.doc.hosts[0];
    assert.ok(host?.isConnected);
    assert.equal(host.showPopover, undefined);
    assert.equal(host.css("position"), "fixed !important");
    assert.equal(harness.ui.probe().banner, "opened");
  });

  it("каждый показ поднимает хост наверх верхнего слоя; один хост на документ", async () => {
    const harness = await setupPageUi();
    deliver(harness, OPENED);
    deliver(harness, NO_VIDEO);
    assert.equal(harness.doc.hosts.length, 1);
    assert.equal(harness.doc.hosts[0]?.popoverShows, 2);
  });

  it("сайт убрал наш хост из документа — следующий показ возвращает его", async () => {
    const harness = await setupPageUi();
    deliver(harness, OPENED);
    harness.doc.hosts[0]?.remove();
    assert.equal(harness.doc.hosts[0]?.isConnected, false);
    deliver(harness, NO_VIDEO);
    assert.equal(harness.doc.hosts[0]?.isConnected, true);
  });

  it("документ без корневого элемента (document_start): показать нечем, но ничего не падает; позже показывается", async () => {
    const harness = await setupPageUi();
    harness.doc.root = null;
    deliver(harness, OPENED);
    assert.equal(harness.doc.hosts.length, 0);
    harness.doc.root = new FakeRoot();
    deliver(harness, NO_VIDEO);
    assert.equal(harness.ui.probe().banner, "no_video");
    assert.equal(harness.doc.hosts[0]?.isConnected, true);
  });
});
