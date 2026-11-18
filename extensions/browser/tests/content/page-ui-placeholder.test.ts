// Заглушка «Playing in Fastiplayer» поверх переданного видео (11a, решение 236) — слой страницы целиком:
// фейковые DOM, часы и сигналы сдвига; «браузер» здесь — тест, он двигает видео и кликает.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { PLACEHOLDER_LAYOUT_RECHECK_MS, PLACEHOLDER_LEAVE_CLICK_SHIELD_MS } from "../../src/content/page-ui/placeholder-controller.ts";
import { PLACEHOLDER_TEXT, RETURN_BUTTON_LABEL } from "../../src/content/page-ui/placeholder-view.ts";
import { WHEEL_DELTA_MODE, WHEEL_LINE_PX } from "../../src/content/page-ui/wheel-pass-through.ts";
import { setupPageUi, type PageUiHarness } from "./support/page-ui-harness.ts";
import { FakePageBlock, FakeUiEvent, FakeUiWheelEvent, FakeVideoTarget, type FakeElement } from "./support/fake-ui-dom.ts";

const VIDEO_RECT = { left: 100, top: 50, width: 640, height: 360 };

/** Панель заглушки и её кнопка в тени слоя. */
function placeholderParts(harness: PageUiHarness): { panel: FakeElement; button: FakeElement } {
  const text = harness.doc.shadowElements().find((element) => element.textContent === PLACEHOLDER_TEXT);
  const button = harness.doc.shadowElements().find((element) => element.textContent === RETURN_BUTTON_LABEL);
  assert.ok(text?.parent !== null && text?.parent !== undefined && button !== undefined, "заглушка построена");
  return { panel: text.parent as FakeElement, button };
}

/** Где панель стоит по своим стилям (то, что видит браузер). */
function placedRect(panel: FakeElement) {
  return { left: panel.css("left"), top: panel.css("top"), width: panel.css("width"), height: panel.css("height"), display: panel.css("display") };
}

const shownAt = (rect: typeof VIDEO_RECT) => ({
  left: `${rect.left}px !important`,
  top: `${rect.top}px !important`,
  width: `${rect.width}px !important`,
  height: `${rect.height}px !important`,
  display: "flex !important",
});

async function handedOff(rect = VIDEO_RECT) {
  const harness = await setupPageUi();
  const video = new FakeVideoTarget(rect);
  harness.ui.handedOffVideo.videoHandedOff(video);
  return { harness, video };
}

describe("заглушка: показ и слежение", () => {
  it("без передачи в документе нет ни одного нашего узла и никаких подписок", async () => {
    const harness = await setupPageUi();
    assert.equal(harness.doc.hosts.length, 0);
    assert.equal(harness.signals.liveWatchers(), 0);
    assert.deepEqual(harness.ui.probe().placeholder, { handed_off: false, visible: false, panel: null, return_button: null });
  });

  it("встаёт ровно на прямоугольник видео, непрозрачная, с текстом и кнопкой, поверх верхнего слоя", async () => {
    const { harness } = await handedOff();
    const { panel } = placeholderParts(harness);
    assert.deepEqual(placedRect(panel), shownAt(VIDEO_RECT));
    assert.equal(panel.css("pointer-events"), "auto !important", "клики по видео перехватывает заглушка");
    assert.match(panel.css("background") ?? "", /^#[0-9a-f]{6} !important$/, "непрозрачный цвет, без прозрачности");
    assert.equal(harness.doc.hosts[0]?.popoverOpen, true, "слой поднят в верхний слой");
  });

  it("следует за видео: сигнал сдвига (прокрутка, размер) переставляет панель сразу", async () => {
    const { harness, video } = await handedOff();
    const { panel } = placeholderParts(harness);
    video.rect = { left: 100, top: -200, width: 640, height: 360 };
    harness.signals.emit("layout_changed");
    assert.deepEqual(placedRect(panel), shownAt(video.rect));
    video.rect = { left: 0, top: 0, width: 1280, height: 720 };
    harness.signals.emit("layout_changed");
    assert.deepEqual(placedRect(panel), shownAt(video.rect));
  });

  it("сдвиг вёрстки без событий догоняет периодический перемер", async () => {
    const { harness, video } = await handedOff();
    const { panel } = placeholderParts(harness);
    video.rect = { left: 100, top: 300, width: 640, height: 360 };
    await harness.advance(PLACEHOLDER_LAYOUT_RECHECK_MS - 1);
    assert.deepEqual(placedRect(panel), shownAt(VIDEO_RECT), "до срока ещё старое место");
    await harness.advance(1);
    assert.deepEqual(placedRect(panel), shownAt(video.rect));
  });

  it("видео убрали из документа или сжали в ноль — прячется; вернули, пока передача идёт, — снова на месте", async () => {
    const { harness, video } = await handedOff();
    const { panel } = placeholderParts(harness);
    video.isConnected = false;
    harness.signals.emit("layout_changed");
    assert.equal(panel.css("display"), "none !important");
    assert.equal(harness.ui.probe().placeholder.handed_off, true, "передача та же — заглушка ждёт");
    video.isConnected = true;
    video.rect = { left: 5, top: 6, width: 0, height: 360 };
    await harness.advance(PLACEHOLDER_LAYOUT_RECHECK_MS);
    assert.equal(panel.css("display"), "none !important", "нулевая ширина — нечего закрывать");
    video.rect = { left: 5, top: 6, width: 320, height: 180 };
    await harness.advance(PLACEHOLDER_LAYOUT_RECHECK_MS);
    assert.deepEqual(placedRect(panel), shownAt(video.rect));
  });

  it("полноэкранный режим страницы поднимает слой заново (над её полноэкранным элементом)", async () => {
    const { harness } = await handedOff();
    const shows = harness.doc.hosts[0]!.popoverShows;
    harness.signals.emit("fullscreen_changed");
    assert.equal(harness.doc.hosts[0]!.popoverShows, shows + 1);
  });

  it("передача ушла — после щита заглушка спрятана, подписки и перемер сняты (часы больше ничего не двигают)", async () => {
    const { harness, video } = await handedOff();
    const { panel } = placeholderParts(harness);
    harness.ui.handedOffVideo.videoLeft();
    assert.equal(harness.signals.liveWatchers(), 0, "подписки сняты сразу, щит в них не нуждается");
    await harness.advance(PLACEHOLDER_LEAVE_CLICK_SHIELD_MS);
    assert.equal(panel.css("display"), "none !important");
    assert.equal(harness.signals.liveWatchers(), 0);
    assert.equal(harness.clock.liveTimers(), 0, "периодический перемер остановлен");
    video.rect = { left: 1, top: 2, width: 300, height: 200 };
    await harness.advance(PLACEHOLDER_LAYOUT_RECHECK_MS * 4);
    assert.equal(panel.css("display"), "none !important");
    assert.equal(harness.ui.probe().placeholder.handed_off, false);
  });

  it("новое видео той же вкладки: заглушка переезжает на него, от старого отписана", async () => {
    const { harness, video } = await handedOff();
    const { panel } = placeholderParts(harness);
    harness.ui.handedOffVideo.videoLeft();
    const next = new FakeVideoTarget({ left: 20, top: 30, width: 400, height: 225 });
    harness.ui.handedOffVideo.videoHandedOff(next);
    assert.deepEqual(placedRect(panel), shownAt(next.rect));
    assert.equal(harness.signals.watchers.get(video)?.size ?? 0, 0);
    assert.equal(harness.signals.watchers.get(next)?.size, 1);
  });
});

describe("заглушка: щит после конца передачи (поздний клик)", () => {
  it("конец передачи: панель невидима, но клик по месту видео ещё гасится и возврат не просит; потом исчезает", async () => {
    const { harness } = await handedOff();
    const { panel, button } = placeholderParts(harness);
    harness.ui.handedOffVideo.videoLeft();
    assert.equal(panel.css("display"), "flex !important", "щит стоит на месте видео");
    assert.equal(panel.css("background"), "transparent !important", "и его не видно");
    assert.equal(button.css("visibility"), "hidden !important", "кнопки не видно");
    assert.deepEqual(harness.ui.probe().placeholder, { handed_off: false, visible: false, panel: null, return_button: null });
    const lateClick = new FakeUiEvent(true, panel);
    panel.dispatch("pointerdown", lateClick);
    panel.dispatch("click", lateClick);
    assert.ok(lateClick.stopped && lateClick.immediatelyStopped, "запоздалый клик до плеера сайта не дошёл");
    button.dispatch("click", new FakeUiEvent(true, button));
    assert.equal(harness.returnRequests.count, 0, "передачи нет — и просить нечего");
    await harness.advance(PLACEHOLDER_LEAVE_CLICK_SHIELD_MS - 1);
    assert.equal(panel.css("display"), "flex !important", "до срока щит ещё стоит");
    await harness.advance(1);
    assert.equal(panel.css("display"), "none !important");
    assert.equal(harness.clock.liveTimers(), 0);
  });

  it("новая передача во время щита: обычная заглушка над новым видео, старый срок её не прячет", async () => {
    const { harness } = await handedOff();
    const { panel, button } = placeholderParts(harness);
    harness.ui.handedOffVideo.videoLeft();
    const next = new FakeVideoTarget({ left: 20, top: 30, width: 400, height: 225 });
    harness.ui.handedOffVideo.videoHandedOff(next);
    assert.equal(panel.css("background"), "#111114 !important", "снова непрозрачная");
    assert.equal(button.css("visibility"), "visible !important");
    await harness.advance(PLACEHOLDER_LEAVE_CLICK_SHIELD_MS * 2);
    assert.deepEqual(placedRect(panel), shownAt(next.rect));
    assert.equal(harness.ui.probe().placeholder.handed_off, true);
  });

  it("заглушки на экране не было (видео нулевого размера) — прятать нечего: щита и таймера нет", async () => {
    const { harness, video } = await handedOff();
    const { panel } = placeholderParts(harness);
    video.rect = { left: 0, top: 0, width: 0, height: 0 };
    harness.signals.emit("layout_changed");
    harness.ui.handedOffVideo.videoLeft();
    assert.equal(panel.css("display"), "none !important");
    assert.equal(harness.clock.liveTimers(), 0);
  });

  it("после щита следующий показ — обычная непрозрачная панель", async () => {
    const { harness } = await handedOff();
    const { panel, button } = placeholderParts(harness);
    harness.ui.handedOffVideo.videoLeft();
    await harness.advance(PLACEHOLDER_LEAVE_CLICK_SHIELD_MS);
    harness.ui.handedOffVideo.videoHandedOff(new FakeVideoTarget(VIDEO_RECT));
    assert.equal(panel.css("background"), "#111114 !important");
    assert.equal(button.css("visibility"), "visible !important");
    assert.deepEqual(placedRect(panel), shownAt(VIDEO_RECT));
  });
});

describe("заглушка: «Return»", () => {
  it("настоящий клик просит возврат ровно один раз за передачу; клик дальше страницы не идёт", async () => {
    const { harness } = await handedOff();
    const { panel, button } = placeholderParts(harness);
    const click = new FakeUiEvent(true, button);
    button.dispatch("click", click);
    panel.dispatch("click", click);
    assert.equal(harness.returnRequests.count, 1);
    assert.ok(click.stopped && click.immediatelyStopped, "панель гасит клик");
    button.dispatch("click", new FakeUiEvent(true, button));
    assert.equal(harness.returnRequests.count, 1, "второй клик ничего не шлёт");
    assert.equal(button.attributes.get("aria-disabled"), "true");
  });

  it("синтетический клик (скрипт страницы) возврат не просит", async () => {
    const { harness } = await handedOff();
    const { button } = placeholderParts(harness);
    button.dispatch("click", new FakeUiEvent(false, button));
    assert.equal(harness.returnRequests.count, 0);
  });

  it("просьба не ушла (передачи с портом ещё нет) — кнопка не умирает: следующий клик пробует снова", async () => {
    const { harness } = await handedOff();
    const { button } = placeholderParts(harness);
    harness.returnDelivery.next = "no_handoff";
    button.dispatch("click", new FakeUiEvent(true, button));
    harness.returnDelivery.next = "sent";
    button.dispatch("click", new FakeUiEvent(true, button));
    button.dispatch("click", new FakeUiEvent(true, button));
    assert.equal(harness.returnRequests.count, 2);
  });

  it("следующая передача снова принимает «Return»", async () => {
    const { harness } = await handedOff();
    const { button } = placeholderParts(harness);
    button.dispatch("click", new FakeUiEvent(true, button));
    harness.ui.handedOffVideo.videoLeft();
    harness.ui.handedOffVideo.videoHandedOff(new FakeVideoTarget(VIDEO_RECT));
    assert.equal(button.attributes.get("aria-disabled"), "false");
    button.dispatch("click", new FakeUiEvent(true, button));
    assert.equal(harness.returnRequests.count, 2);
  });

  it("клики по заглушке гасятся", async () => {
    const { harness } = await handedOff();
    const { panel } = placeholderParts(harness);
    for (const type of ["pointerdown", "mousedown", "mouseup", "click", "dblclick", "contextmenu"]) {
      assert.equal(panel.listeners.get(type)?.size, 1, `${type} гасится`);
    }
  });
});

/** Лента Instagram под заглушкой: видео -> секция -> блок ленты с прилипанием -> неподвижный документ. */
function instagramFeedUnderPlaceholder() {
  const documentScroller = new FakePageBlock({ isDocumentScroller: true, scrollsVertically: true, maxScrollTop: 56 });
  const feedBlock = new FakePageBlock({ parent: documentScroller, scrollsVertically: true, snapsToPositions: true, scrollTop: 32, maxScrollTop: 2_657 });
  const section = new FakePageBlock({ parent: feedBlock });
  const video = new FakePageBlock({ parent: section });
  return { documentScroller, feedBlock, video };
}

describe("заглушка: колесо мыши листает страницу под ней (находка живой проверки Instagram)", () => {
  it("лента в блоке внутри неподвижного документа: колесо над заглушкой листает блок ленты, документ не едет", async () => {
    const { harness } = await handedOff();
    const { panel } = placeholderParts(harness);
    const { documentScroller, feedBlock, video } = instagramFeedUnderPlaceholder();
    harness.doc.pageBlockUnderPlaceholder = video;
    const wheel = new FakeUiWheelEvent({ clientX: 420, clientY: 230, deltaY: 144 });
    panel.dispatch("wheel", wheel);
    assert.deepEqual(feedBlock.scrolls, [{ left: 0, top: 144, motion: "smooth" }], "блок ленты получил прокрутку на величину колеса");
    assert.deepEqual(documentScroller.scrolls, [], "документ сами не двигаем");
    assert.equal(wheel.prevented, true, "родное действие погашено: документ от того же колеса не поедет");
    assert.deepEqual(harness.doc.pageBlockLookups, [{ clientX: 420, clientY: 230, layerHost: harness.doc.hosts[0] }], "блок ищут под курсором, не считая нашего слоя");
  });

  it("слушатель колеса не пассивный: иначе браузер не дал бы погасить родную прокрутку документа", async () => {
    const { harness } = await handedOff();
    const { panel } = placeholderParts(harness);
    assert.deepEqual(panel.listenerOptions.get("wheel"), { passive: false });
  });

  it("обычная страница (листается документ): колесо оставлено браузеру — родная прокрутка, как раньше", async () => {
    const { harness } = await handedOff();
    const { panel } = placeholderParts(harness);
    const documentScroller = new FakePageBlock({ isDocumentScroller: true, scrollsVertically: true, maxScrollTop: 5_000 });
    const video = new FakePageBlock({ parent: new FakePageBlock({ parent: documentScroller }) });
    harness.doc.pageBlockUnderPlaceholder = video;
    const wheel = new FakeUiWheelEvent({ deltaY: 144 });
    panel.dispatch("wheel", wheel);
    assert.equal(wheel.prevented, false);
    assert.deepEqual(documentScroller.scrolls, []);
  });

  it("колесо вверх листает ленту назад, у верхнего края блока уходит дальше — документу", async () => {
    const { harness } = await handedOff();
    const { panel } = placeholderParts(harness);
    const { feedBlock, video } = instagramFeedUnderPlaceholder();
    harness.doc.pageBlockUnderPlaceholder = video;
    panel.dispatch("wheel", new FakeUiWheelEvent({ deltaY: -144 }));
    assert.deepEqual(feedBlock.scrolls, [{ left: 0, top: -144, motion: "smooth" }]);
    // Блок докручен до самого верха: вверх ему ехать некуда, колесо достаётся браузеру.
    feedBlock.scrollTop = 0;
    feedBlock.scrolls.length = 0;
    const atTop = new FakeUiWheelEvent({ deltaY: -144 });
    panel.dispatch("wheel", atTop);
    assert.deepEqual(feedBlock.scrolls, []);
    assert.equal(atTop.prevented, false);
  });

  it("блок без прилипания двигается сразу и ровно на величину колеса; блок, который колесом не листается, пропускается", async () => {
    const { harness } = await handedOff();
    const { panel } = placeholderParts(harness);
    const documentScroller = new FakePageBlock({ isDocumentScroller: true });
    const appShell = new FakePageBlock({ parent: documentScroller, scrollsVertically: true, maxScrollTop: 3_000 });
    // `overflow: hidden` с длинным содержимым: колесом не листается, хотя место есть.
    const clipped = new FakePageBlock({ parent: appShell, scrollsVertically: false, maxScrollTop: 400 });
    harness.doc.pageBlockUnderPlaceholder = new FakePageBlock({ parent: clipped });
    panel.dispatch("wheel", new FakeUiWheelEvent({ deltaY: 100 }));
    assert.deepEqual(clipped.scrolls, []);
    assert.deepEqual(appShell.scrolls, [{ left: 0, top: 100, motion: "instant" }]);
  });

  it("колесо в строках и в страницах переводится в пиксели блока", async () => {
    const { harness } = await handedOff();
    const { panel } = placeholderParts(harness);
    const { feedBlock, video } = instagramFeedUnderPlaceholder();
    feedBlock.clientHeight = 955;
    harness.doc.pageBlockUnderPlaceholder = video;
    panel.dispatch("wheel", new FakeUiWheelEvent({ deltaY: 3, deltaMode: WHEEL_DELTA_MODE.line }));
    panel.dispatch("wheel", new FakeUiWheelEvent({ deltaY: 1, deltaMode: WHEEL_DELTA_MODE.page }));
    assert.deepEqual(feedBlock.scrolls.map((scroll) => scroll.top), [3 * WHEEL_LINE_PX, 955]);
  });

  it("поддельное колесо (скрипт страницы) ничего не листает", async () => {
    const { harness } = await handedOff();
    const { panel } = placeholderParts(harness);
    const { feedBlock, video } = instagramFeedUnderPlaceholder();
    harness.doc.pageBlockUnderPlaceholder = video;
    const forged = new FakeUiWheelEvent({ deltaY: 144, isTrusted: false });
    panel.dispatch("wheel", forged);
    assert.deepEqual(feedBlock.scrolls, []);
    assert.equal(forged.prevented, false);
  });

  it("под заглушкой только наш слой — колесо оставлено браузеру, ничего не ломается", async () => {
    const { harness } = await handedOff();
    const { panel } = placeholderParts(harness);
    harness.doc.pageBlockUnderPlaceholder = null;
    const wheel = new FakeUiWheelEvent({ deltaY: 144 });
    panel.dispatch("wheel", wheel);
    assert.equal(wheel.prevented, false);
  });

  it("колесо листает, а клик по-прежнему до страницы не доходит (решения 236, 239)", async () => {
    const { harness } = await handedOff();
    const { panel } = placeholderParts(harness);
    const { feedBlock, video } = instagramFeedUnderPlaceholder();
    harness.doc.pageBlockUnderPlaceholder = video;
    panel.dispatch("wheel", new FakeUiWheelEvent({ deltaY: 144 }));
    assert.equal(feedBlock.scrolls.length, 1);
    const click = new FakeUiEvent(true, panel);
    panel.dispatch("click", click);
    assert.equal(click.stopped && click.immediatelyStopped, true, "клик погашен на заглушке");
    assert.equal(panel.css("pointer-events"), "auto !important", "заглушка по-прежнему ловит указатель");
  });

  it("щит после конца передачи тоже листает ленту колесом", async () => {
    const { harness } = await handedOff();
    const { panel } = placeholderParts(harness);
    const { feedBlock, video } = instagramFeedUnderPlaceholder();
    harness.doc.pageBlockUnderPlaceholder = video;
    harness.ui.handedOffVideo.videoLeft();
    panel.dispatch("wheel", new FakeUiWheelEvent({ deltaY: 144 }));
    assert.equal(feedBlock.scrolls.length, 1);
  });
});

describe("заглушка: зонд стенда", () => {
  it("отдаёт прямоугольники панели и кнопки с экрана (простыми объектами), видна — пока показана", async () => {
    const { harness } = await handedOff();
    const { panel, button } = placeholderParts(harness);
    // «Браузер нарисовал»: прямоугольники, которые вернул бы getBoundingClientRect.
    panel.rect = VIDEO_RECT;
    button.rect = { left: 380, top: 240, width: 80, height: 28 };
    assert.deepEqual(harness.ui.probe().placeholder, { handed_off: true, visible: true, panel: VIDEO_RECT, return_button: button.rect });
    harness.ui.handedOffVideo.videoLeft();
    // Скрытое браузер не рисует.
    panel.rect = { left: 0, top: 0, width: 0, height: 0 };
    button.rect = { left: 0, top: 0, width: 0, height: 0 };
    assert.deepEqual(harness.ui.probe().placeholder, { handed_off: false, visible: false, panel: null, return_button: null });
  });
});
