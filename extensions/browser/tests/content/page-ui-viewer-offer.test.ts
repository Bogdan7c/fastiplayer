// Панель предложения «Ask» поверх замершего видео (12b) — слой страницы целиком: фейковые DOM, часы и сигналы
// сдвига; «браузер» — тест: он двигает видео и кликает.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { PLACEHOLDER_LEAVE_CLICK_SHIELD_MS } from "../../src/content/page-ui/placeholder-controller.ts";
import { RETURN_BUTTON_LABEL } from "../../src/content/page-ui/placeholder-view.ts";
import { VIEWER_OFFER_BUTTON_LABELS, VIEWER_OFFER_OPENING_TEXT, VIEWER_OFFER_QUESTION } from "../../src/content/page-ui/viewer-offer-view.ts";
import type { ViewerOfferChoice } from "../../src/shared/viewer-offer-contract.ts";
import { setupPageUi, type PageUiHarness } from "./support/page-ui-harness.ts";
import { FakeUiEvent, FakeVideoTarget, type FakeElement } from "./support/fake-ui-dom.ts";

const VIDEO_RECT = { left: 100, top: 50, width: 640, height: 360 };

function elementWithText(harness: PageUiHarness, text: string): FakeElement {
  const element = harness.doc.shadowElements().find((candidate) => candidate.textContent === text);
  assert.ok(element !== undefined, `в тени слоя есть «${text}»`);
  return element;
}

const buttonOf = (harness: PageUiHarness, choice: ViewerOfferChoice): FakeElement => elementWithText(harness, VIEWER_OFFER_BUTTON_LABELS[choice]);
const panelOf = (harness: PageUiHarness): FakeElement => elementWithText(harness, VIEWER_OFFER_QUESTION).parent as FakeElement;

async function offered(rect = VIDEO_RECT) {
  const harness = await setupPageUi();
  const video = new FakeVideoTarget(rect);
  harness.ui.viewerOffer.viewerOfferStood(video);
  return { harness, video };
}

describe("панель «Ask»: показ", () => {
  it("без предложения нет ни узлов, ни подписок; зонд честно пуст", async () => {
    const harness = await setupPageUi();
    assert.equal(harness.doc.hosts.length, 0);
    assert.equal(harness.signals.liveWatchers(), 0);
    assert.deepEqual(harness.ui.probe().viewer_offer, { offered: false, answered: false, visible: false, panel: null, buttons: null });
  });

  it("встаёт на прямоугольник видео с вопросом и четырьмя кнопками; зонд отдаёт их прямоугольники", async () => {
    const { harness } = await offered();
    const panel = panelOf(harness);
    assert.equal(panel.css("left"), "100px !important");
    assert.equal(panel.css("width"), "640px !important");
    assert.equal(panel.css("display"), "flex !important");
    // Непрозрачный фон: сквозь полупрозрачный Firefox «видит» видео и отдаёт клик своей кнопке «Картинка в картинке».
    assert.match(panel.css("background") ?? "", /^#[0-9a-f]{6} !important$/);
    // Поддельный DOM раскладку не считает: прямоугольники «рисует» тест, зонд должен прочитать их с экрана.
    panel.rect = VIDEO_RECT;
    const fastiplayerRect = { left: 300, top: 200, width: 150, height: 30 };
    buttonOf(harness, "fastiplayer").rect = fastiplayerRect;
    buttonOf(harness, "never").rect = { left: 420, top: 240, width: 120, height: 18 };
    const probe = harness.ui.probe().viewer_offer;
    assert.equal(probe.offered, true);
    assert.equal(probe.visible, true);
    assert.deepEqual(probe.buttons?.fastiplayer, fastiplayerRect);
    assert.equal(probe.buttons?.here, null, "кнопка без размера — не видна");
    assert.notEqual(probe.buttons?.never, null);
    assert.equal(harness.doc.hosts[0]?.popoverOpen, true, "слой поднят в верхний слой");
  });

  it("следует за видео по сигналу сдвига", async () => {
    const { harness, video } = await offered();
    video.rect = { left: 100, top: -120, width: 640, height: 360 };
    harness.signals.emit("layout_changed");
    assert.equal(panelOf(harness).css("top"), "-120px !important");
  });
});

describe("панель «Ask»: выбор", () => {
  it("настоящий клик по кнопке уходит пограничнику один раз; кнопки гаснут, для «в плеер» — «Opening…»", async () => {
    const { harness } = await offered();
    const click = new FakeUiEvent(true, buttonOf(harness, "fastiplayer"));
    buttonOf(harness, "fastiplayer").dispatch("click", click);
    panelOf(harness).dispatch("click", click);
    assert.ok(click.stopped && click.immediatelyStopped, "клик до страницы не дошёл");
    buttonOf(harness, "here").dispatch("click", new FakeUiEvent(true, buttonOf(harness, "here")));
    assert.deepEqual(harness.offerChoices, ["fastiplayer"], "второе нажатие ничего не решает");
    assert.equal(elementWithText(harness, VIEWER_OFFER_OPENING_TEXT).css("display"), "block !important");
    assert.equal(buttonOf(harness, "here").attributes.get("aria-disabled"), "true");
    assert.equal(harness.ui.probe().viewer_offer.answered, true);
  });

  it("«Never» и «Play here» уходят как есть, «Opening…» не показывается", async () => {
    for (const choice of ["never", "here"] as const) {
      const { harness } = await offered();
      buttonOf(harness, choice).dispatch("click", new FakeUiEvent(true, buttonOf(harness, choice)));
      assert.deepEqual(harness.offerChoices, [choice]);
      assert.equal(elementWithText(harness, VIEWER_OFFER_OPENING_TEXT).css("display"), "none !important");
    }
  });

  it("синтетический клик страницы (`isTrusted: false`) — не выбор человека", async () => {
    const { harness } = await offered();
    buttonOf(harness, "always").dispatch("click", new FakeUiEvent(false, buttonOf(harness, "always")));
    assert.deepEqual(harness.offerChoices, []);
    assert.equal(harness.ui.probe().viewer_offer.answered, false);
  });

  it("предложение ушло: панель становится щитом от запоздалого клика и прячется через 300 мс", async () => {
    const { harness } = await offered();
    harness.ui.viewerOffer.viewerOfferLeft();
    const panel = panelOf(harness);
    assert.equal(panel.css("display"), "flex !important", "щит ещё на месте");
    assert.equal(harness.ui.probe().viewer_offer.visible, false, "для стенда панели уже нет");
    const lateClick = new FakeUiEvent(true, panel);
    panel.dispatch("click", lateClick);
    assert.ok(lateClick.stopped, "запоздалый клик погашен");
    await harness.advance(PLACEHOLDER_LEAVE_CLICK_SHIELD_MS);
    assert.equal(panel.css("display"), "none !important");
    assert.equal(harness.signals.liveWatchers(), 0, "подписки на сдвиги сняты");
  });

  it("новое предложение принимает выбор заново", async () => {
    const { harness } = await offered();
    buttonOf(harness, "here").dispatch("click", new FakeUiEvent(true, buttonOf(harness, "here")));
    harness.ui.viewerOffer.viewerOfferLeft();
    harness.ui.viewerOffer.viewerOfferStood(new FakeVideoTarget(VIDEO_RECT));
    buttonOf(harness, "fastiplayer").dispatch("click", new FakeUiEvent(true, buttonOf(harness, "fastiplayer")));
    assert.deepEqual(harness.offerChoices, ["here", "fastiplayer"]);
  });

  it("предложение и заглушка живут независимо: над переданным A — «Return», над замершим B — вопрос", async () => {
    const harness = await setupPageUi();
    harness.ui.handedOffVideo.videoHandedOff(new FakeVideoTarget({ left: 0, top: 0, width: 320, height: 180 }));
    harness.ui.viewerOffer.viewerOfferStood(new FakeVideoTarget(VIDEO_RECT));
    harness.ui.viewerOffer.viewerOfferLeft();
    await harness.advance(PLACEHOLDER_LEAVE_CLICK_SHIELD_MS);
    const placeholderPanel = elementWithText(harness, RETURN_BUTTON_LABEL).parent as FakeElement;
    assert.equal(placeholderPanel.css("display"), "flex !important", "уход предложения заглушку не снял");
    assert.equal(panelOf(harness).css("display"), "none !important", "а панель предложения ушла");
    assert.equal(harness.ui.probe().placeholder.handed_off, true);
  });
});
