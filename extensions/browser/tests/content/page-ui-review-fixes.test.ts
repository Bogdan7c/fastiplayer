// Дефекты ревью слоя страницы (10c): подписка на мышь, повторное нажатие, мерцание, не-HTML документ, слияние настроек.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { createDocumentPointerSource, type PointerEventLike } from "../../src/content/page-ui/document-pointer-source.ts";
import { createOverlayButtonController } from "../../src/content/page-ui/overlay-button-controller.ts";
import { withEntrySetting } from "../../src/options/entry-setting-control.ts";
import { OVERLAY_BUTTON_LABEL } from "../../src/content/page-ui/overlay-button-view.ts";
import { SHOW_ENTRY_RESULT_KIND } from "../../src/shared/entry-contract.ts";
import { deferred, FakeUiEvent, flushMicrotasks, type FakeElement } from "./support/fake-ui-dom.ts";
import { OWN_EXTENSION_ID, setupPageUi, VIDEO_AT_POINT } from "./support/page-ui-harness.ts";

describe("мышь документа", () => {
  function fakeTarget() {
    const added: { type: string; capture: boolean; handler: (event: PointerEventLike) => void }[] = [];
    return {
      added,
      target: {
        addEventListener: (type: string, handler: (event: PointerEventLike) => void, options: { capture: boolean }) => void added.push({ type, capture: options.capture, handler }),
        removeEventListener: (type: string, handler: (event: PointerEventLike) => void) => {
          const at = added.findIndex((entry) => entry.type === type && entry.handler === handler);
          if (at >= 0) added.splice(at, 1);
        },
      },
    };
  }

  it("mousemove слушается в capture, синтетические движения отбрасываются, настоящие доходят", () => {
    const { added, target } = fakeTarget();
    const moves: { x: number; y: number }[] = [];
    const stop = createDocumentPointerSource(target).onMouseMove((event) => void moves.push({ x: event.x, y: event.y }));
    const [entry] = added;
    assert.equal(entry?.type, "mousemove");
    assert.equal(entry?.capture, true);
    entry?.handler({ isTrusted: false, clientX: 1, clientY: 1, target: null });
    entry?.handler({ isTrusted: true, clientX: 5, clientY: 6, target: null });
    assert.deepEqual(moves, [{ x: 5, y: 6 }]);
    stop();
    assert.equal(added.length, 0);
  });

  it("потеря указателя не требует корневого элемента и полностью снимается", () => {
    const { added, target } = fakeTarget();
    let lost = 0;
    const stop = createDocumentPointerSource(target).onPointerLost(() => void lost++);
    for (const entry of [...added]) entry.handler({ isTrusted: true, target: null, relatedTarget: null });
    assert.equal(lost, 2);
    stop();
    assert.equal(added.length, 0);
  });

  it("уход мыши с элемента внутри документа — не потеря: иначе кнопка прячется под курсором перед кликом", () => {
    const { added, target } = fakeTarget();
    let lost = 0;
    createDocumentPointerSource(target).onPointerLost(() => void lost++);
    const leave = added.find((entry) => entry.type === "mouseleave");
    assert.notEqual(leave, undefined);
    // Capture на документе видит `mouseleave` любого элемента: мышь ушла с видео на нашу кнопку.
    leave?.handler({ isTrusted: true, target: "video", relatedTarget: "overlay-host" });
    assert.equal(lost, 0);
    leave?.handler({ isTrusted: true, target: "html", relatedTarget: null });
    assert.equal(lost, 1);
  });
});

describe("кнопка: повторное нажатие и подписка", () => {
  it("ответ, летевший в момент нажатия, кнопку не возвращает: второго входа нет", async () => {
    const harness = await setupPageUi({ overlayEnabled: true });
    harness.pointer.move(300, 200);
    await flushMicrotasks();
    const slow = deferred<typeof VIDEO_AT_POINT | null>();
    harness.answer.next = () => slow.promise;
    await harness.advance(100);
    harness.pointer.move(310, 210); // вопрос улетел и ждёт ответа
    const button = harness.doc.shadowElements().find((e) => e.attributes.get("aria-label") === OVERLAY_BUTTON_LABEL) as FakeElement;
    button.dispatch("click", new FakeUiEvent(true));
    slow.resolve(VIDEO_AT_POINT);
    await flushMicrotasks();
    assert.equal(button.css("display"), "none !important");
    assert.equal(harness.sent.length, 1);
  });

  it("сбой второй подписки снимает первую; следующее включение не вешает дубль", () => {
    const live = new Set<number>();
    let failSecond = true;
    const notes: string[] = [];
    const controller = createOverlayButtonController({
      pointer: {
        onMouseMove: () => { live.add(1); return () => void live.delete(1); },
        onPointerLost: () => { if (failSecond) throw new Error("no root"); live.add(2); return () => void live.delete(2); },
      },
      view: { show: () => {}, hide: () => {}, measure: () => null },
      timers: { setTimeout: () => 0, clearTimeout: () => {} },
      now: () => 0,
      isOwnElement: () => false,
      videoAtPoint: () => Promise.resolve(null),
      sendPressed: () => Promise.resolve(),
      note: (message) => void notes.push(message),
    });
    controller.setEnabled(true);
    assert.equal(live.size, 0);
    assert.deepEqual(notes, ["overlay button listeners were not installed"]);
    failSecond = false;
    controller.setEnabled(true);
    assert.deepEqual([...live].sort(), [1, 2]);
    controller.setEnabled(false);
    assert.equal(live.size, 0);
  });
});

describe("слой: поднятие и не-HTML документ", () => {
  it("перемещение видимой кнопки не трогает popover; после скрытия и нового показа поднимается снова", async () => {
    const harness = await setupPageUi({ overlayEnabled: true });
    harness.pointer.move(300, 200);
    await flushMicrotasks();
    const shows = harness.doc.hosts[0]?.popoverShows;
    assert.equal(shows, 1);
    await harness.advance(100);
    harness.pointer.move(320, 220);
    await flushMicrotasks();
    assert.equal(harness.doc.hosts[0]?.popoverShows, 1);
    harness.pointer.lose();
    await harness.advance(100);
    harness.pointer.move(320, 220);
    await flushMicrotasks();
    assert.equal(harness.doc.hosts[0]?.popoverShows, 2);
  });

  it("attachShadow бросает (XML/SVG): плашка и кнопка молча не показываются, отклонений нет", async () => {
    const rejections: unknown[] = [];
    const onRejection = (reason: unknown): void => void rejections.push(reason);
    process.on("unhandledRejection", onRejection);
    const harness = await setupPageUi({ overlayEnabled: true });
    const originalCreate = harness.doc.createHost.bind(harness.doc);
    harness.doc.createHost = (tag) => {
      const host = originalCreate(tag);
      host.attachShadow = () => { throw new Error("not an HTML document"); };
      return host;
    };
    harness.ui.onMessage({ kind: SHOW_ENTRY_RESULT_KIND, report: { id: "opened", tone: "success", headline: "h", details: [] } }, { id: OWN_EXTENSION_ID });
    harness.pointer.move(300, 200);
    await flushMicrotasks();
    await new Promise((done) => setImmediate(done));
    process.off("unhandledRejection", onRejection);
    assert.deepEqual(rejections, []);
    assert.equal(harness.doc.hosts.length, 1, "второй попытки создать хост нет");
    assert.equal(harness.doc.hosts[0]?.isConnected, false);
    assert.equal(harness.ui.probe().overlay_button, null);
  });
});

describe("настройки входов: слияние", () => {
  it("меняет только своё поле, соседние берёт из текущих значений хранилища", () => {
    const current = { video_menu: false, link_menu: false, overlay_button: false };
    assert.deepEqual(withEntrySetting(current, "overlay_button", true), { video_menu: false, link_menu: false, overlay_button: true });
    assert.deepEqual(current, { video_menu: false, link_menu: false, overlay_button: false }, "исходный объект не тронут");
  });
});
