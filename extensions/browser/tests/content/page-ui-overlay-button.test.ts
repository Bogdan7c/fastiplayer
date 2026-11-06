// Кнопка поверх видео (сессия 10c): от настройки до нажатия, на фейковых DOM/мыши/часах.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { placeButtonAnchor } from "../../src/content/page-ui/button-placement.ts";
import { HOVER_POLL_MS } from "../../src/content/page-ui/overlay-button-controller.ts";
import { watchOverlaySetting } from "../../src/content/page-ui/overlay-setting-watch.ts";
import { OVERLAY_BUTTON_LABEL } from "../../src/content/page-ui/overlay-button-view.ts";
import { OVERLAY_BUTTON_PRESSED_KIND } from "../../src/shared/entry-contract.ts";
import { deferred, FakeUiEvent, flushMicrotasks, type FakeElement } from "./support/fake-ui-dom.ts";
import { setupPageUi, VIDEO_AT_POINT, type PageUiHarness } from "./support/page-ui-harness.ts";

/** Кнопка в тени: по подписи. */
function findButton(harness: PageUiHarness): FakeElement | undefined {
  return harness.doc.shadowElements().find((element) => element.tagName === "button" && element.attributes.get("aria-label") === OVERLAY_BUTTON_LABEL);
}

/** Кнопка показана: сделана видимой и измерима. Прямоугольник ей «рисует браузер» — задаём в тесте. */
function isShown(harness: PageUiHarness): boolean {
  const button = findButton(harness);
  return button !== undefined && button.css("display") !== "none !important";
}

/** Мышь над видео; ждём ответа MAIN-мира. */
async function hoverVideo(harness: PageUiHarness, x = 300, y = 200): Promise<void> {
  harness.pointer.move(x, y);
  await flushMicrotasks();
}

describe("кнопка поверх видео: сон при выключенной настройке", () => {
  it("настройка выключена по умолчанию: ни слушателей мыши, ни узлов, ни вопросов; движение мыши ничего не вызывает", async () => {
    const harness = await setupPageUi();
    assert.equal(harness.pointer.liveListeners(), 0);
    assert.equal(harness.doc.hosts.length, 0);
    harness.pointer.move(10, 10);
    await flushMicrotasks();
    assert.deepEqual(harness.asked, []);
    assert.equal(harness.ui.probe().overlay_button, null);
  });

  it("явно выключена в хранилище — то же самое", async () => {
    const harness = await setupPageUi({ overlayEnabled: false });
    assert.equal(harness.pointer.liveListeners(), 0);
    assert.equal(harness.doc.hosts.length, 0);
  });

  it("включена: слушатели повешены; фону при этом ничего не ушло", async () => {
    const harness = await setupPageUi({ overlayEnabled: true });
    assert.ok(harness.pointer.moveListeners.size > 0);
    assert.ok(harness.pointer.lostListeners.size > 0);
    assert.deepEqual(harness.sent, []);
  });

  it("включили на лету — кнопка оживает сразу, выключили — слушатели, таймер и кнопка уходят", async () => {
    const harness = await setupPageUi();
    harness.changeOverlaySetting(true);
    assert.ok(harness.pointer.liveListeners() > 0);
    await hoverVideo(harness);
    assert.equal(isShown(harness), true);

    harness.pointer.move(5, 5); // повисший «хвостовой» опрос
    harness.changeOverlaySetting(false);
    assert.equal(harness.pointer.liveListeners(), 0);
    assert.equal(isShown(harness), false);
    assert.equal(harness.clock.liveTimers(), 0);
    const askedBefore = harness.asked.length;
    harness.pointer.move(6, 6); // ушло в никуда: слушателей нет
    await harness.advance(1_000);
    assert.equal(harness.asked.length, askedBefore);
  });

  it("изменение чужого ключа хранилища настройку не трогает", async () => {
    const harness = await setupPageUi({ overlayEnabled: true });
    for (const listener of harness.storageListeners) listener({ codecSetting: { newValue: "auto" } });
    assert.ok(harness.pointer.liveListeners() > 0);
  });

  it("повторное включение/выключение идемпотентно: слушатели не множатся", async () => {
    const harness = await setupPageUi({ overlayEnabled: true });
    const before = harness.pointer.liveListeners();
    harness.changeOverlaySetting(true);
    harness.changeOverlaySetting(true);
    assert.equal(harness.pointer.liveListeners(), before);
  });
});

describe("кнопка поверх видео: появление и скрытие", () => {
  it("над видео появляется: правый верхний угол с отступом, прямоугольник виден зонду", async () => {
    const harness = await setupPageUi({ overlayEnabled: true });
    await hoverVideo(harness, 300, 200);
    assert.deepEqual(harness.asked, [{ x: 300, y: 200 }]);
    const button = findButton(harness);
    assert.ok(button);
    // Правый край видео 100+640=740, отступ 8: якорь 732; верх 50+8.
    assert.equal(button.css("left"), "732px !important");
    assert.equal(button.css("top"), "58px !important");
    button.rect = { left: 600, top: 58, width: 132, height: 28 };
    assert.deepEqual(harness.ui.probe().overlay_button, { left: 600, top: 58, width: 132, height: 28 });
  });

  it("зонд отдаёт простой объект, а не DOMRect: через executeScript настоящий DOMRect приходил пустым `{}` (живой прогон 10c)", async () => {
    // Настоящий DOMRect хранит left/top/width/height геттерами на прототипе: JSON и клонирование видят пустой объект.
    class RealisticRect {
      get left(): number { return 600; }
      get top(): number { return 58; }
      get width(): number { return 132; }
      get height(): number { return 28; }
    }
    const harness = await setupPageUi({ overlayEnabled: true });
    await hoverVideo(harness, 300, 200);
    const button = findButton(harness);
    assert.ok(button);
    button.rect = new RealisticRect();
    const reply: unknown = JSON.parse(JSON.stringify(harness.ui.probe().overlay_button));
    assert.deepEqual(reply, { left: 600, top: 58, width: 132, height: 28 });
  });

  it("зонд отдаёт то, что нарисовано: пока кнопки нет или она скрыта, там null", async () => {
    const harness = await setupPageUi({ overlayEnabled: true });
    assert.equal(harness.ui.probe().overlay_button, null);
    await hoverVideo(harness);
    const button = findButton(harness) as FakeElement;
    button.rect = { left: 1, top: 2, width: 100, height: 20 };
    assert.notEqual(harness.ui.probe().overlay_button, null);
    harness.answer.next = () => Promise.resolve(null);
    await harness.advance(HOVER_POLL_MS);
    await hoverVideo(harness, 900, 900);
    button.rect = { left: 0, top: 0, width: 0, height: 0 }; // скрытая кнопка браузер измеряет нулём
    assert.equal(harness.ui.probe().overlay_button, null);
  });

  it("мышь ушла с видео — кнопка скрыта", async () => {
    const harness = await setupPageUi({ overlayEnabled: true });
    await hoverVideo(harness);
    assert.equal(isShown(harness), true);
    harness.answer.next = () => Promise.resolve(null);
    await harness.advance(HOVER_POLL_MS);
    await hoverVideo(harness, 2000, 2000);
    assert.equal(isShown(harness), false);
  });

  it("мышь покинула окно или страница прокручена — кнопка скрыта, пока не придёт новое движение", async () => {
    const harness = await setupPageUi({ overlayEnabled: true });
    await hoverVideo(harness);
    harness.pointer.lose();
    assert.equal(isShown(harness), false);
    await harness.advance(HOVER_POLL_MS);
    await hoverVideo(harness);
    assert.equal(isShown(harness), true);
  });

  it("видео уже в плеере — кнопки нет, а была — убрана", async () => {
    const harness = await setupPageUi({ overlayEnabled: true });
    await hoverVideo(harness);
    assert.equal(isShown(harness), true);
    harness.answer.next = () => Promise.resolve({ ...VIDEO_AT_POINT, in_player: true });
    await harness.advance(HOVER_POLL_MS);
    await hoverVideo(harness);
    assert.equal(isShown(harness), false);
  });

  it("мышь с видео на саму кнопку: не скрывается и новых вопросов не задаётся", async () => {
    const harness = await setupPageUi({ overlayEnabled: true });
    await hoverVideo(harness);
    const asked = harness.asked.length;
    await harness.advance(HOVER_POLL_MS * 3);
    harness.pointer.move(730, 60, harness.doc.hosts[0]); // над кнопкой цель события — наш хост
    await flushMicrotasks();
    assert.equal(isShown(harness), true);
    assert.equal(harness.asked.length, asked);
    assert.equal(harness.clock.liveTimers(), 0);
  });

  it("слишком частые движения: вопрос не чаще HOVER_POLL_MS, последняя точка не теряется", async () => {
    const harness = await setupPageUi({ overlayEnabled: true });
    harness.pointer.move(1, 1);
    harness.pointer.move(2, 2);
    harness.pointer.move(3, 3);
    await flushMicrotasks();
    assert.deepEqual(harness.asked, [{ x: 1, y: 1 }], "сразу ушёл только первый");
    await harness.advance(HOVER_POLL_MS - 1);
    assert.equal(harness.asked.length, 1);
    await harness.advance(1);
    assert.deepEqual(harness.asked, [{ x: 1, y: 1 }, { x: 3, y: 3 }], "хвостовой опрос берёт самую свежую точку");
  });

  it("устаревший ответ: мышь ушла раньше, чем пришёл ответ про прежнюю точку — кнопка не появляется", async () => {
    const harness = await setupPageUi({ overlayEnabled: true });
    const slow = deferred<typeof VIDEO_AT_POINT | null>();
    harness.answer.next = () => slow.promise;
    harness.pointer.move(300, 200);
    await flushMicrotasks();
    harness.answer.next = () => Promise.resolve(null);
    harness.pointer.move(5000, 5000); // мышь ушла (опрос пойдёт хвостом)
    slow.resolve(VIDEO_AT_POINT); // запоздалый ответ про старую точку
    await flushMicrotasks();
    assert.equal(isShown(harness), false, "про старую точку кнопку не рисуем");
    assert.equal(findButton(harness), undefined, "кнопка даже не создавалась");
    await harness.advance(HOVER_POLL_MS);
    assert.equal(isShown(harness), false);
  });

  it("ответ, летевший в момент выключения настройки, кнопку не показывает", async () => {
    const harness = await setupPageUi({ overlayEnabled: true });
    const slow = deferred<typeof VIDEO_AT_POINT | null>();
    harness.answer.next = () => slow.promise;
    harness.pointer.move(300, 200);
    harness.changeOverlaySetting(false);
    slow.resolve(VIDEO_AT_POINT);
    await flushMicrotasks();
    assert.equal(findButton(harness), undefined);
    assert.equal(harness.doc.hosts.length, 0);
  });
});

describe("кнопка поверх видео: нажатие", () => {
  it("настоящее нажатие шлёт фону верное сообщение и не доходит до страницы", async () => {
    const harness = await setupPageUi({ overlayEnabled: true });
    await hoverVideo(harness);
    const press = new FakeUiEvent(true);
    (findButton(harness) as FakeElement).dispatch("click", press);
    await flushMicrotasks();
    assert.deepEqual(harness.sent, [{ kind: OVERLAY_BUTTON_PRESSED_KIND, document: "doc-1", element: 7 }]);
    assert.equal(press.stopped, true);
    assert.equal(press.immediatelyStopped, true);
    assert.equal(press.prevented, true);
    assert.equal(isShown(harness), false, "после нажатия кнопка скрыта: второй раз не нажать");
  });

  it("нажатие без isTrusted (подделка) фону не уходит, но и странице не достаётся", async () => {
    const harness = await setupPageUi({ overlayEnabled: true });
    await hoverVideo(harness);
    const fake = new FakeUiEvent(false);
    (findButton(harness) as FakeElement).dispatch("click", fake);
    await flushMicrotasks();
    assert.deepEqual(harness.sent, []);
    assert.equal(fake.stopped && fake.prevented, true);
    assert.equal(isShown(harness), true, "подделка не прячет кнопку");
  });

  it("остальные события мыши на кнопке до страницы не доходят", async () => {
    const harness = await setupPageUi({ overlayEnabled: true });
    await hoverVideo(harness);
    for (const type of ["mousedown", "mouseup", "pointerdown", "pointerup", "dblclick", "contextmenu"]) {
      const event = new FakeUiEvent(true);
      (findButton(harness) as FakeElement).dispatch(type, event);
      assert.equal(event.stopped, true, type);
    }
    assert.deepEqual(harness.sent, []);
  });

  it("фон недоступен: нажатие не роняет страницу, сбой записан для разработчика", async () => {
    const notes: unknown[] = [];
    const harness = await setupPageUi({ overlayEnabled: true, sendFails: true, notes });
    await hoverVideo(harness);
    (findButton(harness) as FakeElement).dispatch("click", new FakeUiEvent(true));
    await flushMicrotasks();
    assert.deepEqual(notes, ["overlay button press was not delivered"]);
  });

  it("кнопка нажимается на то видео, над которым показана сейчас (после перехода на другое)", async () => {
    const harness = await setupPageUi({ overlayEnabled: true });
    await hoverVideo(harness);
    harness.answer.next = () => Promise.resolve({ ...VIDEO_AT_POINT, element: 9, document: "doc-2" });
    await harness.advance(HOVER_POLL_MS);
    await hoverVideo(harness, 400, 300);
    (findButton(harness) as FakeElement).dispatch("click", new FakeUiEvent(true));
    await flushMicrotasks();
    assert.deepEqual(harness.sent, [{ kind: OVERLAY_BUTTON_PRESSED_KIND, document: "doc-2", element: 9 }]);
  });

  it("кнопка в потоке страницы не участвует: хост и кнопка заданы инлайном с !important", async () => {
    const harness = await setupPageUi({ overlayEnabled: true });
    await hoverVideo(harness);
    const host = harness.doc.hosts[0];
    assert.equal(host?.css("position"), "fixed !important");
    assert.equal(host?.css("width"), "0 !important");
    const button = findButton(harness);
    assert.equal(button?.css("position"), "fixed !important");
    assert.equal(button?.css("display"), "flex !important");
  });
});

describe("расположение кнопки", () => {
  it("видео шире окна: кнопка прижимается к видимому краю, а не уходит за экран", () => {
    assert.deepEqual(placeButtonAnchor({ left: -200, top: -100, width: 3000, height: 800 }, { width: 1280, height: 720 }), { right: 1272, top: 8 });
  });
});

describe("слежение за настройкой", () => {
  it("событие об изменении, пришедшее до ответа первичного чтения, побеждает запоздавший старый ответ", async () => {
    const read = deferred<{ video_menu: boolean; link_menu: boolean; overlay_button: boolean }>();
    const applied: boolean[] = [];
    let emit: (changes: Record<string, { newValue?: unknown }>) => void = () => {};
    watchOverlaySetting({
      readSettings: () => read.promise,
      onLocalStorageChanged: (listener) => {
        emit = listener;
        return () => {};
      },
      applyEnabled: (enabled) => void applied.push(enabled),
      note: () => {},
    });
    emit({ entrySettings: { newValue: { overlay_button: true } } });
    read.resolve({ video_menu: true, link_menu: true, overlay_button: false }); // старое значение до клика человека
    await flushMicrotasks();
    assert.deepEqual(applied, [true]);
  });

  it("чтение хранилища упало: кнопка остаётся спящей, сбой записан", async () => {
    const notes: string[] = [];
    const applied: boolean[] = [];
    watchOverlaySetting({
      readSettings: () => Promise.reject(new Error("storage unavailable")),
      onLocalStorageChanged: () => () => {},
      applyEnabled: (enabled) => void applied.push(enabled),
      note: (message) => void notes.push(message),
    });
    await flushMicrotasks();
    assert.deepEqual(applied, []);
    assert.deepEqual(notes, ["could not read the entry settings"]);
  });
});
