// Колесо над заглушкой на «настоящей» странице: переходник `real-page-scroll-block.ts` (чтения раскладки) и
// правило `wheel-pass-through.ts` вместе, на дереве элементов той же формы, что у браузера. Проверяется, какой
// элемент страницы получил прокрутку и погашено ли родное действие колеса.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { pageScrollBlockUnder, type ScrollBlockElement, type ScrollBlockPage, type ScrollBlockStyle } from "../../src/content/page-ui/real-page-scroll-block.ts";
import { passWheelToPage } from "../../src/content/page-ui/wheel-pass-through.ts";
import { FakeUiWheelEvent } from "./support/fake-ui-dom.ts";

const VISIBLE: ScrollBlockStyle = { overflowX: "visible", overflowY: "visible", scrollSnapType: "none" };

/** Элемент страницы: раскладку задаёт тест, прокрутки копятся. */
class PageElement implements ScrollBlockElement {
  assignedSlot: PageElement | null = null;
  parentElement: PageElement | null = null;
  /** Хозяин теневого корня, если элемент — прямой ребёнок корня тени. */
  shadowHost: PageElement | null = null;
  style: ScrollBlockStyle = VISIBLE;
  scrollTop = 0;
  scrollLeft = 0;
  scrollHeight = 0;
  scrollWidth = 0;
  clientHeight = 0;
  clientWidth = 0;
  readonly scrolls: { left: number; top: number; behavior: string }[] = [];
  readonly name: string;
  constructor(name: string, facts: Partial<Pick<PageElement, "style" | "scrollTop" | "scrollHeight" | "clientHeight">> = {}) {
    this.name = name;
    Object.assign(this, facts);
  }
  getRootNode(): { host?: PageElement | null } {
    return this.shadowHost === null ? {} : { host: this.shadowHost };
  }
  scrollBy(options: { left: number; top: number; behavior: string }): void {
    this.scrolls.push({ ...options });
  }
  /** Вложить `child` в этот элемент обычным ребёнком. */
  holds<Child extends PageElement>(child: Child): Child {
    child.parentElement = this;
    return child;
  }
}

/** Страница: `<html>` -> `<body>` -> …; под точкой лежат `stack` (сверху вниз). */
function pageOf(html: PageElement, body: PageElement, stack: readonly PageElement[]): ScrollBlockPage {
  return {
    documentElement: html,
    scrollingElement: html,
    body,
    elementsFromPoint: () => stack,
    styleOf: (element) => (element as PageElement).style,
  };
}

/** Колесо вниз на 144 px над заглушкой: что вышло. */
function wheelOver(page: ScrollBlockPage, layerHost: PageElement) {
  const wheel = new FakeUiWheelEvent({ clientX: 500, clientY: 400, deltaY: 144 });
  const outcome = passWheelToPage(wheel, (clientX, clientY) => pageScrollBlockUnder(page, clientX, clientY, layerHost));
  return { outcome, prevented: wheel.prevented };
}

const FEED_STYLE: ScrollBlockStyle = { overflowX: "hidden", overflowY: "auto", scrollSnapType: "y mandatory" };

describe("колесо над заглушкой на настоящем дереве страницы", () => {
  it("Instagram: под заглушкой видео в блоке ленты — блок ленты прокручен плавно, наш слой под курсором не считается", () => {
    const html = new PageElement("html", { scrollHeight: 1_011, clientHeight: 955 });
    const body = html.holds(new PageElement("body"));
    const feed = body.holds(new PageElement("feed", { style: FEED_STYLE, scrollTop: 32, scrollHeight: 3_612, clientHeight: 955 }));
    const video = feed.holds(new PageElement("section")).holds(new PageElement("video"));
    const layerHost = html.holds(new PageElement("fastiplayer-bridge-ui"));
    const page = pageOf(html, body, [layerHost, video, feed, body, html]);
    assert.deepEqual(wheelOver(page, layerHost), { outcome: "page_block_scrolled", prevented: true });
    assert.deepEqual(feed.scrolls, [{ left: 0, top: 144, behavior: "smooth" }]);
    assert.deepEqual(html.scrolls, []);
  });

  it("обычная страница: до прокрутки документа блоков нет — колесо браузеру", () => {
    const html = new PageElement("html", { scrollHeight: 6_000, clientHeight: 955 });
    const body = html.holds(new PageElement("body"));
    const video = body.holds(new PageElement("player")).holds(new PageElement("video"));
    const layerHost = html.holds(new PageElement("fastiplayer-bridge-ui"));
    const page = pageOf(html, body, [layerHost, video, body, html]);
    assert.deepEqual(wheelOver(page, layerHost), { outcome: "left_to_browser", prevented: false });
  });

  it("`body { height: 100%; overflow: auto }` при `overflow: visible` у корня — это прокрутка окна: колесо браузеру, `body` не трогаем", () => {
    const html = new PageElement("html", { scrollHeight: 6_000, clientHeight: 955 });
    const body = html.holds(new PageElement("body", { style: { ...VISIBLE, overflowY: "auto" }, scrollHeight: 6_000, clientHeight: 955 }));
    const video = body.holds(new PageElement("video"));
    const layerHost = html.holds(new PageElement("fastiplayer-bridge-ui"));
    const page = pageOf(html, body, [layerHost, video, body, html]);
    assert.deepEqual(wheelOver(page, layerHost), { outcome: "left_to_browser", prevented: false });
    assert.deepEqual(body.scrolls, []);
  });

  it("корень с `overflow: hidden`, листается сам `body` — он обычный блок: прокручен нами", () => {
    const html = new PageElement("html", { style: { ...VISIBLE, overflowX: "hidden", overflowY: "hidden" }, scrollHeight: 955, clientHeight: 955 });
    const body = html.holds(new PageElement("body", { style: { ...VISIBLE, overflowY: "auto" }, scrollHeight: 6_000, clientHeight: 955 }));
    const video = body.holds(new PageElement("video"));
    const layerHost = html.holds(new PageElement("fastiplayer-bridge-ui"));
    const page = pageOf(html, body, [layerHost, video, body, html]);
    assert.deepEqual(wheelOver(page, layerHost), { outcome: "page_block_scrolled", prevented: true });
    assert.deepEqual(body.scrolls, [{ left: 0, top: 144, behavior: "instant" }]);
  });

  it("`overflow: hidden` колесом не листается, хотя содержимое длиннее: поиск идёт выше, к блоку приложения", () => {
    const html = new PageElement("html", { scrollHeight: 955, clientHeight: 955 });
    const body = html.holds(new PageElement("body"));
    const appShell = body.holds(new PageElement("app", { style: { ...VISIBLE, overflowY: "scroll" }, scrollHeight: 4_000, clientHeight: 955 }));
    const clipped = appShell.holds(new PageElement("clip", { style: { ...VISIBLE, overflowY: "hidden" }, scrollHeight: 1_500, clientHeight: 600 }));
    const video = clipped.holds(new PageElement("video"));
    const layerHost = html.holds(new PageElement("fastiplayer-bridge-ui"));
    const page = pageOf(html, body, [layerHost, video]);
    assert.equal(wheelOver(page, layerHost).outcome, "page_block_scrolled");
    assert.deepEqual(clipped.scrolls, []);
    assert.equal(appShell.scrolls.length, 1);
  });

  it("видео в теневом дереве страницы: путь к блоку ленты идёт через хозяина тени", () => {
    const html = new PageElement("html", { scrollHeight: 955, clientHeight: 955 });
    const body = html.holds(new PageElement("body"));
    const feed = body.holds(new PageElement("feed", { style: FEED_STYLE, scrollHeight: 3_000, clientHeight: 955 }));
    const playerHost = feed.holds(new PageElement("x-player"));
    const video = new PageElement("video");
    video.shadowHost = playerHost;
    const layerHost = html.holds(new PageElement("fastiplayer-bridge-ui"));
    const page = pageOf(html, body, [layerHost, video]);
    assert.equal(wheelOver(page, layerHost).outcome, "page_block_scrolled");
    assert.equal(feed.scrolls.length, 1);
  });

  it("блок ленты докручен до конца — колесо вниз уходит браузеру, блок не трогаем", () => {
    const html = new PageElement("html", { scrollHeight: 955, clientHeight: 955 });
    const body = html.holds(new PageElement("body"));
    const feed = body.holds(new PageElement("feed", { style: FEED_STYLE, scrollTop: 2_657, scrollHeight: 3_612, clientHeight: 955 }));
    const video = feed.holds(new PageElement("video"));
    const layerHost = html.holds(new PageElement("fastiplayer-bridge-ui"));
    const page = pageOf(html, body, [layerHost, video]);
    assert.deepEqual(wheelOver(page, layerHost), { outcome: "left_to_browser", prevented: false });
    assert.deepEqual(feed.scrolls, []);
  });

  it("под точкой только наш слой — колесо браузеру", () => {
    const html = new PageElement("html");
    const body = html.holds(new PageElement("body"));
    const layerHost = html.holds(new PageElement("fastiplayer-bridge-ui"));
    assert.deepEqual(wheelOver(pageOf(html, body, [layerHost]), layerHost), { outcome: "left_to_browser", prevented: false });
  });
});
