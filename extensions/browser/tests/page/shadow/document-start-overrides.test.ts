// Переопределения прототипов с document_start (решение 181, Д-9): прозрачны вне передачи,
// а дескрипторы, сохранённые страницей до передачи, после неё видят тень.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { flushMicrotasks, recordEvents } from "./fake-media-dom.ts";
import { createShadowHarness, emittedWithoutPulse, playingVideo } from "./shadow-harness.ts";

describe("тень: переопределения на document_start (решение 181, Д-9)", () => {
  /** Дескрипторы браузера, снятые до нашего document_start. */
  function harnessWithNativeDescriptors() {
    let nativeCurrentTime: PropertyDescriptor | undefined;
    let nativeVolume: PropertyDescriptor | undefined;
    let nativePlay: unknown;
    const harness = createShadowHarness({
      beforeDocumentStart: (dom) => {
        const prototype = dom.classes.FakeHTMLMediaElement.prototype;
        nativeCurrentTime = Object.getOwnPropertyDescriptor(prototype, "currentTime");
        nativeVolume = Object.getOwnPropertyDescriptor(prototype, "volume");
        nativePlay = prototype.play;
      },
    });
    assert.ok(nativeCurrentTime?.get !== undefined && nativeCurrentTime.set !== undefined && nativeVolume?.set !== undefined);
    return { harness, nativeCurrentTime, nativeVolume, nativePlay: nativePlay as (this: unknown) => Promise<void> };
  }

  /** Исход вызова: значение или класс и текст ошибки — то, что видит страница. */
  function outcomeOf(action: () => unknown): { value?: unknown; error?: string } {
    try {
      return { value: action() };
    } catch (problem) {
      return { error: `${(problem as Error).name}: ${(problem as Error).message}` };
    }
  }

  it("переопределения стоят с document_start, но до передачи прозрачны: значения, исключения, this, play(), сеттеры", async () => {
    const { harness, nativeCurrentTime, nativeVolume, nativePlay } = harnessWithNativeDescriptors();
    const prototype = harness.dom.classes.FakeHTMLMediaElement.prototype;
    const installed = Object.getOwnPropertyDescriptor(prototype, "currentTime");
    assert.notEqual(installed?.get, nativeCurrentTime.get, "переопределение поставлено сразу, а не при первой передаче");

    const video = harness.dom.createVideo();
    video.real.currentTime = 12.5;
    assert.equal(video.currentTime, 12.5);
    video.currentTime = 30;
    assert.equal(video.real.currentTime, 30, "сеттер дошёл до браузера");
    assert.ok(video.realCalls.some((call) => call.kind === "set" && call.property === "currentTime" && call.value === 30));
    // Исключения — те же, что у браузера: и проверка значения, и чужой `this`.
    assert.deepEqual(
      outcomeOf(() => (video.volume = 2)),
      outcomeOf(() => nativeVolume.set?.call(video, 2)),
    );
    assert.deepEqual(
      outcomeOf(() => installed?.get?.call({})),
      outcomeOf(() => nativeCurrentTime.get?.call({})),
    );
    // play(): обещание браузера, настоящий элемент играет, события доверенные.
    const page = recordEvents(video, ["play", "playing"]);
    const played = video.play();
    assert.equal(await played, await nativePlay.call(harness.dom.createVideo()));
    assert.equal(video.real.paused, false);
    await flushMicrotasks();
    assert.deepEqual(page.seen, [
      { type: "play", trusted: true },
      { type: "playing", trusted: true },
    ]);
    assert.deepEqual(harness.emitted, [], "без передачи наружу ничего");
  });

  it("страница сохранила дескрипторы currentTime/play до передачи: после передачи они видят тень", async () => {
    const harness = createShadowHarness();
    const prototype = harness.dom.classes.FakeHTMLMediaElement.prototype;
    // Плеер или антиадблок сохранил «настоящие» методы на DOMContentLoaded — до передачи.
    const savedCurrentTime = Object.getOwnPropertyDescriptor(prototype, "currentTime")?.get;
    const savedPlay = prototype.play;
    assert.ok(savedCurrentTime !== undefined);

    const video = playingVideo(harness.dom, 10);
    video.real.paused = true; // страница поставила паузу до передачи
    const session = harness.start(video);
    harness.tick(session, 42, "paused");
    assert.equal(savedCurrentTime.call(video), 42, "время тени, а не настоящего элемента (10 с)");
    await savedPlay.call(video);
    assert.equal(video.real.paused, true, "настоящий элемент не запущен");
    assert.equal(video.paused, false, "играет тень");
    assert.deepEqual(emittedWithoutPulse(harness).at(-1), { kind: "page_playback", request: { kind: "play" } });
  });
});
