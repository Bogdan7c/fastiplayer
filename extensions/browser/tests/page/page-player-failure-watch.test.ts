// Сторож «плеер страницы сломался» (11a, решение 237): правило по тикам часов плеера и его действие через
// всю сборку MAIN-мира (контроллер + фейковая тень + настоящий канал).
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import {
  PAGE_PLAYER_FAILURE_GRACE_MS,
  PAGE_PLAYER_FAILURE_RELEASE_WINDOW_MS,
  PagePlayerFailureWatch,
  pagePlayerFailureWatchStarterFor,
  type PagePlayerFailureReaders,
} from "../../src/page/page-player-failure-watch.ts";
import type { PageEvent } from "../../src/shared/handoff-contract.ts";
import { handshake, startHarness, type BridgeHarness, type IsolatedSide } from "./support/bridge-harness.ts";
import { addPlayingMseVideo, describeAndBegin, playerClock } from "./support/handoff-steps.ts";

/** Элемент и часы под управлением теста. */
function fakeReaders() {
  const state = { nowMs: 0, hasError: false, listener: null as (() => void) | null, unsubscribed: 0 };
  const readers: PagePlayerFailureReaders = {
    onElementError: (_element, listener) => {
      state.listener = listener;
      return () => void (state.unsubscribed += 1);
    },
    hasPlaybackFailureError: () => state.hasError,
    now: () => state.nowMs,
  };
  const fail = (): void => {
    state.hasError = true;
    state.listener?.();
  };
  return { state, readers, fail };
}

describe("правило сторожа", () => {
  it("ошибка держится до допуска — конец ровно на допуске, и причина любого конца потом — page_player_failed", () => {
    const { state, readers, fail } = fakeReaders();
    const watch = new PagePlayerFailureWatch({}, readers);
    assert.equal(watch.onClockTick(), "keep_handoff", "без ошибки сторож спит");
    fail();
    state.nowMs = PAGE_PLAYER_FAILURE_GRACE_MS - 1;
    assert.equal(watch.onClockTick(), "keep_handoff");
    state.nowMs = PAGE_PLAYER_FAILURE_GRACE_MS;
    assert.equal(watch.onClockTick(), "end_for_page_player_failure");
    assert.equal(watch.onClockTick(), "keep_handoff", "решение принимается один раз");
    assert.equal(watch.endCauseOf("returned"), "page_player_failed");
    watch.mediaSourceArrived();
    assert.equal(watch.endCauseOf("returned"), "page_player_failed", "решённое не отменяется");
  });

  it("ошибка, потом страница бросила источник: в окне — page_player_failed, после окна — обычный element_released", () => {
    const { state, readers, fail } = fakeReaders();
    const watch = new PagePlayerFailureWatch({}, readers);
    fail();
    state.hasError = false; // страница сняла источник: алгоритм загрузки сбросил ошибку
    state.nowMs = PAGE_PLAYER_FAILURE_GRACE_MS + 1_000;
    assert.equal(watch.onClockTick(), "keep_handoff", "ошибки уже нет — сам не кончает");
    assert.equal(watch.endCauseOf("element_released"), "page_player_failed");
    assert.equal(watch.endCauseOf("returned"), "returned", "наш end_handoff не переписывается");
    state.nowMs = PAGE_PLAYER_FAILURE_RELEASE_WINDOW_MS + 1;
    assert.equal(watch.endCauseOf("element_released"), "element_released");
  });

  it("ошибка, потом новый MediaSource — плеер оправился: ни конца, ни переписанной причины", () => {
    const { state, readers, fail } = fakeReaders();
    const watch = new PagePlayerFailureWatch({}, readers);
    fail();
    state.hasError = false;
    watch.mediaSourceArrived();
    state.nowMs = PAGE_PLAYER_FAILURE_GRACE_MS * 3;
    assert.equal(watch.onClockTick(), "keep_handoff");
    assert.equal(watch.endCauseOf("element_released"), "element_released");
  });

  it("новая ошибка после попытки оправиться — свой допуск от неё", () => {
    const { state, readers, fail } = fakeReaders();
    const watch = new PagePlayerFailureWatch({}, readers);
    fail();
    state.nowMs = 4_000;
    fail();
    state.nowMs = PAGE_PLAYER_FAILURE_GRACE_MS + 1;
    assert.equal(watch.onClockTick(), "keep_handoff");
    state.nowMs = 4_000 + PAGE_PLAYER_FAILURE_GRACE_MS;
    assert.equal(watch.onClockTick(), "end_for_page_player_failure");
  });

  it("элемент уже сломан до передачи — отсчёт с начала передачи; stop снимает подписку", () => {
    const { state, readers } = fakeReaders();
    state.hasError = true;
    const watch = new PagePlayerFailureWatch({}, readers);
    state.nowMs = PAGE_PLAYER_FAILURE_GRACE_MS;
    assert.equal(watch.onClockTick(), "end_for_page_player_failure");
    watch.stop();
    assert.equal(state.unsubscribed, 1);
  });

  it("подписка слышит только доверенную ошибку самого элемента (не его <source>, не синтетику страницы)", () => {
    const element = new EventTarget();
    const child = { name: "source" };
    // Цель события читается оригинальным геттером `Event.prototype.target`: у фейка — поле `testTarget`.
    const eventPrototype = Object.defineProperty({}, "target", {
      get(this: { testTarget?: unknown }) {
        return this.testTarget;
      },
    });
    // На старте ошибки нет (иначе сторож запомнил бы её сразу), дальше у элемента ошибка декодирования.
    let elementHasPlaybackError = false;
    const starter = pagePlayerFailureWatchStarterFor(
      { EventTarget, Event: { prototype: eventPrototype } },
      () => elementHasPlaybackError,
      () => 0,
    );
    assert.ok(starter !== null);
    const watch = starter(element);
    elementHasPlaybackError = true;
    // Видна ли ошибка сторожу: ошибка в окне превращает element_released в page_player_failed.
    const sawError = (): boolean => watch.endCauseOf("element_released") === "page_player_failed";
    const errorEvent = (options: { trusted: boolean; target: unknown }): Event => {
      const event = new Event("error");
      Object.defineProperty(event, "isTrusted", { value: options.trusted });
      Object.defineProperty(event, "testTarget", { value: options.target });
      return event;
    };
    element.dispatchEvent(errorEvent({ trusted: false, target: element }));
    assert.equal(sawError(), false, "синтетическое событие страницы");
    element.dispatchEvent(errorEvent({ trusted: true, target: child }));
    assert.equal(sawError(), false, "ошибка дочернего <source> — не ошибка элемента");
    element.dispatchEvent(errorEvent({ trusted: true, target: element }));
    assert.equal(sawError(), true);
    watch.stop();
  });

  it("окружение без оригиналов событий — сторожа нет, передача работает как раньше", () => {
    assert.equal(pagePlayerFailureWatchStarterFor({ EventTarget: { prototype: {} }, Event: { prototype: {} } }, () => false, () => 0), null);
  });
});

// ------------------------------------------------------------------ сборка MAIN-мира

async function withMseHandoff(body: (harness: BridgeHarness, isolated: IsolatedSide, video: ReturnType<typeof addPlayingMseVideo>["video"]) => Promise<void>): Promise<void> {
  const harness = startHarness({ useWindowClock: true });
  const isolated = handshake(harness.page);
  try {
    const { video } = addPlayingMseVideo(harness);
    await describeAndBegin(isolated);
    assert.equal(harness.shadow.started.length, 1);
    await body(harness, isolated, video);
  } finally {
    isolated.close();
  }
}

function endedEvents(isolated: IsolatedSide): PageEvent[] {
  return isolated.received.filter((message): message is PageEvent => message.kind === "handoff_ended");
}

describe("сторож в сборке MAIN-мира", () => {
  it("ошибка держится 5 с — тень снята на её позиции на паузе, фону — handoff_ended{page_player_failed}", async () => {
    await withMseHandoff(async (harness, isolated, video) => {
      video.testFail();
      harness.page.clock.advance(PAGE_PLAYER_FAILURE_GRACE_MS - 1);
      playerClock(isolated);
      await isolated.settle();
      assert.deepEqual(harness.shadow.ended, [], "до допуска передача идёт");
      harness.page.clock.advance(1);
      playerClock(isolated);
      await isolated.settle();
      assert.deepEqual(harness.shadow.ended, [{ position: { kind: "shadow_position" }, resume: { kind: "exactly", resume: "paused" } }]);
      assert.deepEqual(endedEvents(isolated), [{ kind: "handoff_ended", cause: "page_player_failed" }]);
      assert.equal(harness.bridge.controller.isHandingOff, false);
    });
  });

  it("ошибка, потом страница бросила источник, и тень отдала видео — причина переписана", async () => {
    await withMseHandoff(async (harness, isolated, video) => {
      video.testFail();
      video.srcObject = null; // страница сбросила элемент: ошибка ушла вместе с источником
      harness.page.clock.advance(PAGE_PLAYER_FAILURE_GRACE_MS + 500);
      playerClock(isolated);
      await isolated.settle();
      assert.deepEqual(harness.shadow.ended, [], "ошибки нет — сторож сам не кончает");
      harness.shadow.started[0]!.emit({ kind: "handoff_ended", cause: "element_released" });
      await isolated.settle();
      assert.deepEqual(endedEvents(isolated), [{ kind: "handoff_ended", cause: "page_player_failed" }]);
    });
  });

  it("без ошибки страница, забравшая видео, остаётся element_released (решение 179 в силе)", async () => {
    await withMseHandoff(async (harness, isolated) => {
      harness.shadow.started[0]!.emit({ kind: "handoff_ended", cause: "element_released" });
      await isolated.settle();
      assert.deepEqual(endedEvents(isolated), [{ kind: "handoff_ended", cause: "element_released" }]);
    });
  });

  it("страница убрала источник (`src = \"\"`, ошибка «источник не годится») — это не поломка: ни конца, ни переписи", async () => {
    await withMseHandoff(async (harness, isolated, video) => {
      // Браузер на `src = ""` ставит MEDIA_ERR_SRC_NOT_SUPPORTED (4) и шлёт доверенное `error`.
      video.testFail(4);
      harness.page.clock.advance(PAGE_PLAYER_FAILURE_GRACE_MS * 2);
      playerClock(isolated);
      await isolated.settle();
      assert.deepEqual(harness.shadow.ended, [], "ошибка источника держится, но плеер страницы не сломан");
      harness.shadow.started[0]!.emit({ kind: "handoff_ended", cause: "element_released" });
      await isolated.settle();
      assert.deepEqual(endedEvents(isolated), [{ kind: "handoff_ended", cause: "element_released" }], "решение 179 в силе");
    });
  });

  it("ошибка, потом новый MediaSource на том же элементе — плеер страницы оправился, передача идёт", async () => {
    await withMseHandoff(async (harness, isolated, video) => {
      video.testFail();
      const next = harness.page.attachMediaSource(video);
      next.addSourceBuffer('video/mp4; codecs="avc1.42E01E"');
      // Внутри окна после ошибки: без «оправился» отдача видео стала бы page_player_failed.
      harness.page.clock.advance(PAGE_PLAYER_FAILURE_GRACE_MS + 1_000);
      playerClock(isolated);
      await isolated.settle();
      assert.deepEqual(harness.shadow.ended, []);
      assert.equal(harness.bridge.controller.isHandingOff, true);
      harness.shadow.started[0]!.emit({ kind: "handoff_ended", cause: "element_released" });
      await isolated.settle();
      assert.deepEqual(endedEvents(isolated), [{ kind: "handoff_ended", cause: "element_released" }]);
    });
  });

  it("файл под тенью не сторожится: ошибка элемента — наша (источник снят нами), передача идёт", async () => {
    const harness = startHarness({ useWindowClock: true });
    const isolated = handshake(harness.page);
    try {
      const video = harness.page.addVideo();
      video.rect = { left: 0, top: 0, right: 640, bottom: 360 };
      video.src = "https://cdn.example/media/movie.mp4";
      video.testSetState({ readyState: 4, paused: false });
      await describeAndBegin(isolated, "file");
      assert.equal(harness.shadow.started[0]?.source, "file");
      video.testFail();
      harness.page.clock.advance(PAGE_PLAYER_FAILURE_GRACE_MS * 2);
      playerClock(isolated);
      await isolated.settle();
      assert.deepEqual(harness.shadow.ended, []);
      harness.shadow.started[0]!.emit({ kind: "handoff_ended", cause: "element_released" });
      await isolated.settle();
      assert.deepEqual(endedEvents(isolated), [{ kind: "handoff_ended", cause: "element_released" }]);
    } finally {
      isolated.close();
    }
  });
});
