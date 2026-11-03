// Play/pause зрителя «как медиаклавиша» (решение 194): правило доставки на фейках и вся тень
// с настоящим стражем mediaSession на фейковом MediaSession.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { installMediaSessionGuard, type MediaKeyAction, type PageActionOutcome } from "../../../src/page/media-session-guard.ts";
import { deliverViewerMediaKey, type ViewerMediaKeyPorts } from "../../../src/page/shadow/viewer-media-key.ts";
import { createFakeMediaSessionWindow } from "./fake-media-session.ts";
import { flushMicrotasks, recordEvents } from "./fake-media-dom.ts";
import { createShadowHarness, emittedWithoutPulse, playingVideo } from "./shadow-harness.ts";

const PLAY_PAUSE_EVENTS = ["play", "playing", "pause", "waiting"];

/** Фейковые порты доставки: всё, что с ними сделали, копится в журналах. */
function fakePorts(outcome: PageActionOutcome, options: { readonly shadowActive?: boolean } = {}) {
  const invoked: MediaKeyAction[] = [];
  const delivered: MediaKeyAction[] = [];
  const problems: { context: string; problem: unknown }[] = [];
  const ports: ViewerMediaKeyPorts = {
    invokePageAction: (action) => {
      invoked.push(action);
      return outcome;
    },
    deliverToElement: (action) => void delivered.push(action),
    shadowActive: () => options.shadowActive ?? true,
    reportProblem: (context, problem) => void problems.push({ context, problem }),
  };
  return { ports, invoked, delivered, problems };
}

describe("правило медиаклавиши (viewer-media-key)", () => {
  it("обработчик вызван — элемент тень не трогает, отладки нет", () => {
    const fake = fakePorts({ kind: "invoked" });
    assert.equal(deliverViewerMediaKey("play", fake.ports), "page_handler");
    assert.deepEqual(fake.invoked, ["play"]);
    assert.deepEqual(fake.delivered, []);
    assert.deepEqual(fake.problems, []);
  });

  it("обработчика нет — прежняя доставка элементу, без отладки", () => {
    const fake = fakePorts({ kind: "no_handler" });
    assert.equal(deliverViewerMediaKey("pause", fake.ports), "element");
    assert.deepEqual(fake.delivered, ["pause"]);
    assert.deepEqual(fake.problems, []);
  });

  it("обработчик бросил — одна строка отладки с его ошибкой и откат на элемент", () => {
    const problem = new Error("site player is broken");
    const fake = fakePorts({ kind: "handler_failed", problem });
    assert.equal(deliverViewerMediaKey("play", fake.ports), "element");
    assert.deepEqual(fake.delivered, ["play"]);
    assert.equal(fake.problems.length, 1);
    assert.equal(fake.problems[0]?.problem, problem);
    assert.match(fake.problems[0]?.context ?? "", /play handler threw/);
  });

  it("обработчик бросил, успев снять тень, — элементу ничего (тени уже нет), отладка есть", () => {
    const fake = fakePorts({ kind: "handler_failed", problem: "boom" }, { shadowActive: false });
    assert.equal(deliverViewerMediaKey("pause", fake.ports), "shadow_gone");
    assert.deepEqual(fake.delivered, []);
    assert.equal(fake.problems.length, 1);
  });
});

/** Тень с настоящим стражем: страница ставит обработчики на фейковый `navigator.mediaSession`. */
function shadowWithMediaSession() {
  const sessionWindow = createFakeMediaSessionWindow();
  const guard = installMediaSessionGuard(sessionWindow.win);
  const harness = createShadowHarness({ mediaSession: guard });
  return { harness, mediaSession: sessionWindow.mediaSession };
}

/** Видео на паузе (страница сама остановила его до передачи). */
function pausedVideo(harness: ReturnType<typeof createShadowHarness>) {
  const video = playingVideo(harness.dom, 10);
  video.real.paused = true;
  return video;
}

describe("тень: play/pause зрителя как медиаклавиша", () => {
  it("play зрителя вызывает обработчик страницы с { action: 'play' } и НЕ запускает элемент сам", async () => {
    const { harness, mediaSession } = shadowWithMediaSession();
    const video = pausedVideo(harness);
    const details: unknown[] = [];
    mediaSession.setActionHandler("play", (detail: unknown) => void details.push(detail));
    const session = harness.start(video);
    const page = recordEvents(video, PLAY_PAUSE_EVENTS);

    session.apply({ kind: "play" });
    await flushMicrotasks();

    assert.deepEqual(details, [{ action: "play" }]);
    assert.equal(video.paused, true, "the page decides what to do; the shadow did not play the element itself");
    assert.deepEqual(page.seen, []);
    assert.deepEqual(emittedWithoutPulse(harness), [], "nothing goes back to the player: it is already playing");
  });

  it("pause зрителя вызывает обработчик pause и НЕ ставит паузу сама", async () => {
    const { harness, mediaSession } = shadowWithMediaSession();
    const video = playingVideo(harness.dom, 10);
    const details: unknown[] = [];
    mediaSession.setActionHandler("pause", (detail: unknown) => void details.push(detail));
    const session = harness.start(video);
    const page = recordEvents(video, PLAY_PAUSE_EVENTS);

    session.apply({ kind: "pause" });
    await flushMicrotasks();

    assert.deepEqual(details, [{ action: "pause" }]);
    assert.equal(video.paused, false);
    assert.deepEqual(page.seen, []);
    assert.deepEqual(emittedWithoutPulse(harness), []);
  });

  it("обработчик сам зовёт video.play() — ровно один page_playback{play}, без петли", async () => {
    const { harness, mediaSession } = shadowWithMediaSession();
    const video = pausedVideo(harness);
    let handlerRuns = 0;
    mediaSession.setActionHandler("play", () => {
      handlerRuns += 1;
      void video.play();
    });
    const session = harness.start(video);
    const page = recordEvents(video, PLAY_PAUSE_EVENTS);

    session.apply({ kind: "play" });
    await flushMicrotasks();

    assert.equal(handlerRuns, 1);
    assert.equal(video.paused, false);
    assert.equal(video.real.paused, true, "the real element stays paused under the shadow");
    assert.deepEqual(page.types().filter((type) => type === "play"), ["play"], "exactly one play event for the page");
    assert.deepEqual(emittedWithoutPulse(harness), [{ kind: "page_playback", request: { kind: "play" } }]);
  });

  it("обработчик pause сам зовёт video.pause() — ровно один page_playback{pause}", async () => {
    const { harness, mediaSession } = shadowWithMediaSession();
    const video = playingVideo(harness.dom, 10);
    mediaSession.setActionHandler("pause", () => video.pause());
    const session = harness.start(video);

    session.apply({ kind: "pause" });
    await flushMicrotasks();

    assert.equal(video.paused, true);
    assert.deepEqual(emittedWithoutPulse(harness), [{ kind: "page_playback", request: { kind: "pause" } }]);
  });

  it("обработчика нет — как раньше: тень сама показывает игру, плееру ничего не уходит", async () => {
    const { harness } = shadowWithMediaSession();
    const video = pausedVideo(harness);
    const session = harness.start(video);
    const page = recordEvents(video, PLAY_PAUSE_EVENTS);

    session.apply({ kind: "play" });
    await flushMicrotasks();

    assert.equal(video.paused, false);
    assert.deepEqual(page.types().slice(0, 2), ["play", "playing"]);
    assert.deepEqual(emittedWithoutPulse(harness), []);
  });

  it("обработчик поставлен и снят (null) — снова прежний путь элементу", async () => {
    const { harness, mediaSession } = shadowWithMediaSession();
    const video = playingVideo(harness.dom, 10);
    let handlerRuns = 0;
    mediaSession.setActionHandler("pause", () => void (handlerRuns += 1));
    const session = harness.start(video);
    mediaSession.setActionHandler("pause", null);

    session.apply({ kind: "pause" });
    await flushMicrotasks();

    assert.equal(handlerRuns, 0);
    assert.equal(video.paused, true);
  });

  it("обработчик бросил — откат на элемент: нажатие зрителя не теряется, страница ошибки не видит", async () => {
    const { harness, mediaSession } = shadowWithMediaSession();
    const video = pausedVideo(harness);
    mediaSession.setActionHandler("play", () => {
      throw new Error("site player is broken");
    });
    const session = harness.start(video);
    const page = recordEvents(video, PLAY_PAUSE_EVENTS);

    assert.doesNotThrow(() => session.apply({ kind: "play" }));
    await flushMicrotasks();

    assert.equal(video.paused, false);
    assert.deepEqual(page.types().slice(0, 2), ["play", "playing"]);
  });

  it("медиаклавиши не трогают стража: обработчики возвращаются браузеру при возврате", async () => {
    const { harness, mediaSession } = shadowWithMediaSession();
    const video = playingVideo(harness.dom, 10);
    const onPause = () => video.pause();
    mediaSession.setActionHandler("pause", onPause);
    const session = harness.start(video);
    assert.equal(mediaSession.systemHandlers.size, 0, "suspended during the handoff");

    session.apply({ kind: "pause" });
    await flushMicrotasks();
    assert.equal(mediaSession.systemHandlers.size, 0, "invoking did not give the keys back to the browser");

    session.end({ kind: "at", position_us: 10_000_000 }, { kind: "exactly", resume: "paused" });
    assert.equal(mediaSession.systemHandlers.get("pause"), onPause);
  });
});
