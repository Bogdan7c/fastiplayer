// Страж navigator.mediaSession на фейковом MediaSession: что видит «система» (браузер) и страница.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { installMediaSessionGuard } from "../../../src/page/media-session-guard.ts";
import { createFakeMediaSessionWindow } from "./fake-media-session.ts";

const createFakeWindow = createFakeMediaSessionWindow;

describe("страж mediaSession", () => {
  it("до передачи прозрачен: вызовы страницы доходят до системы", () => {
    const { win, mediaSession, FakeMediaMetadata } = createFakeWindow();
    installMediaSessionGuard(win);
    const metadata = new FakeMediaMetadata({ title: "Фильм" });
    mediaSession.metadata = metadata;
    mediaSession.playbackState = "playing";
    const onPause = () => {};
    mediaSession.setActionHandler("pause", onPause);
    assert.equal(mediaSession.systemMetadata, metadata);
    assert.equal(mediaSession.systemPlaybackState, "playing");
    assert.equal(mediaSession.systemHandlers.get("pause"), onPause);
    assert.equal(mediaSession.metadata, metadata);
  });

  it("suspend гасит у системы, restore возвращает то, что ставила страница", () => {
    const { win, mediaSession, FakeMediaMetadata } = createFakeWindow();
    const guard = installMediaSessionGuard(win);
    const metadata = new FakeMediaMetadata({ title: "Фильм" });
    const onPlay = () => {};
    mediaSession.metadata = metadata;
    mediaSession.playbackState = "playing";
    mediaSession.setActionHandler("play", onPlay);
    mediaSession.setPositionState({ duration: 100, position: 10 });

    guard.suspend();
    assert.equal(mediaSession.systemMetadata, null);
    assert.equal(mediaSession.systemPlaybackState, "none");
    assert.equal(mediaSession.systemHandlers.size, 0);
    assert.equal(mediaSession.systemPositionState, undefined);
    // Страница видит своё, а не наш null.
    assert.equal(mediaSession.metadata, metadata);
    assert.equal(mediaSession.playbackState, "playing");

    guard.restore();
    assert.equal(mediaSession.systemMetadata, metadata);
    assert.equal(mediaSession.systemPlaybackState, "playing");
    assert.equal(mediaSession.systemHandlers.get("play"), onPlay);
    assert.deepEqual(mediaSession.systemPositionState, { duration: 100, position: 10 });
  });

  it("пока погашено, новые вызовы страницы только запоминаются и возвращаются при restore", () => {
    const { win, mediaSession, FakeMediaMetadata } = createFakeWindow();
    const guard = installMediaSessionGuard(win);
    mediaSession.setActionHandler("play", () => {});
    guard.suspend();

    const nextEpisode = new FakeMediaMetadata({ title: "Серия 2" });
    const onSeek = () => {};
    mediaSession.metadata = nextEpisode;
    mediaSession.playbackState = "paused";
    mediaSession.setActionHandler("seekto", onSeek);
    mediaSession.setActionHandler("play", null);
    const callsBefore = mediaSession.systemPositionCalls.length;
    mediaSession.setPositionState({ duration: 50, position: 1 });
    assert.equal(mediaSession.systemPositionCalls.length, callsBefore, "the position never reached the browser, not even for a moment");
    assert.equal(mediaSession.systemMetadata, null);
    assert.equal(mediaSession.systemPlaybackState, "none");
    assert.equal(mediaSession.systemHandlers.size, 0);
    assert.equal(mediaSession.systemPositionState, undefined);
    assert.equal(mediaSession.metadata, nextEpisode);

    guard.restore();
    assert.equal(mediaSession.systemMetadata, nextEpisode);
    assert.equal(mediaSession.systemPlaybackState, "paused");
    assert.deepEqual([...mediaSession.systemHandlers.keys()], ["seekto"]);
    assert.deepEqual(mediaSession.systemPositionState, { duration: 50, playbackRate: 1, position: 1 });
  });

  it("пока погашено, setPositionState проверяется как у браузера и не трогает систему", () => {
    const { win, mediaSession } = createFakeWindow();
    const guard = installMediaSessionGuard(win);
    guard.suspend();
    for (const invalid of [
      { position: 1 },
      { duration: -1 },
      { duration: 10, position: 11 },
      { duration: 10, position: -1 },
      { duration: 10, playbackRate: 0 },
      { duration: 10, position: Number.NaN },
      42,
    ]) {
      assert.throws(() => mediaSession.setPositionState(invalid as unknown as { duration: number; position: number }), TypeError, JSON.stringify(invalid));
    }
    mediaSession.setPositionState({ duration: Number.POSITIVE_INFINITY, position: 3, playbackRate: 2 } as { duration: number; position: number });
    mediaSession.setPositionState({} as { duration: number; position: number }); // пустой словарь — очистка
    assert.deepEqual(mediaSession.systemPositionCalls, []);
    guard.restore();
    assert.equal(mediaSession.systemPositionState, undefined, "cleared by the page during the handoff");
  });

  it("ошибки страницы остаются её ошибками и ничего не запоминают — и до, и во время передачи", () => {
    const { win, mediaSession } = createFakeWindow();
    const guard = installMediaSessionGuard(win);
    assert.throws(() => mediaSession.setActionHandler("dance", () => {}), TypeError);
    guard.suspend();
    assert.throws(() => {
      mediaSession.metadata = { title: "не MediaMetadata" };
    }, TypeError);
    assert.throws(() => mediaSession.setActionHandler("dance", () => {}), TypeError);
    assert.throws(() => mediaSession.setActionHandler("play", 42 as unknown as () => void), TypeError);
    assert.throws(() => mediaSession.setPositionState({ duration: 1, position: 5 }), TypeError);
    guard.restore();
    assert.equal(mediaSession.systemMetadata, null);
    assert.equal(mediaSession.systemHandlers.size, 0);
    assert.equal(mediaSession.systemPositionState, undefined);
  });

  it("без navigator.mediaSession — пустышка, ничего не бросает", () => {
    const guard = installMediaSessionGuard({ navigator: {} } as unknown as Window);
    guard.suspend();
    guard.restore();
  });
});

describe("страж mediaSession: медиаклавиша за браузер (решение 194)", () => {
  /** Обработчик, который запоминает каждый вызов: аргументы и `this`. */
  function recordingHandler() {
    const calls: { details: unknown; self: unknown }[] = [];
    const handler = function (this: unknown, details: unknown): void {
      calls.push({ details, self: this });
    };
    return { handler, calls };
  }

  it("поставленный обработчик вызывается с { action } и без this — как на системную клавишу", () => {
    const { win, mediaSession } = createFakeWindow();
    const guard = installMediaSessionGuard(win);
    const onPlay = recordingHandler();
    const onPause = recordingHandler();
    mediaSession.setActionHandler("play", onPlay.handler);
    mediaSession.setActionHandler("pause", onPause.handler);

    assert.deepEqual(guard.invokePageAction("play"), { kind: "invoked" });
    assert.deepEqual(onPlay.calls, [{ details: { action: "play" }, self: undefined }]);
    assert.deepEqual(onPause.calls, [], "only the handler of the pressed key runs");
    assert.deepEqual(guard.invokePageAction("pause"), { kind: "invoked" });
    assert.deepEqual(onPause.calls, [{ details: { action: "pause" }, self: undefined }]);
  });

  it("обработчика нет или страница его сняла — no_handler, а не invoked", () => {
    const { win, mediaSession } = createFakeWindow();
    const guard = installMediaSessionGuard(win);
    assert.deepEqual(guard.invokePageAction("play"), { kind: "no_handler" });
    const onPlay = recordingHandler();
    mediaSession.setActionHandler("play", onPlay.handler);
    mediaSession.setActionHandler("play", null);
    assert.deepEqual(guard.invokePageAction("play"), { kind: "no_handler" });
    assert.deepEqual(onPlay.calls, []);
  });

  it("обработчик бросил — handler_failed с его ошибкой, исключение наружу не летит", () => {
    const { win, mediaSession } = createFakeWindow();
    const guard = installMediaSessionGuard(win);
    const problem = new Error("player of the site is broken");
    mediaSession.setActionHandler("pause", () => {
      throw problem;
    });
    assert.deepEqual(guard.invokePageAction("pause"), { kind: "handler_failed", problem });
  });

  it("пока погашено: вызывается обработчик, поставленный и до, и во время передачи; состояние стража не меняется", () => {
    const { win, mediaSession } = createFakeWindow();
    const guard = installMediaSessionGuard(win);
    const beforeHandoff = recordingHandler();
    mediaSession.setActionHandler("pause", beforeHandoff.handler);
    guard.suspend();
    const duringHandoff = recordingHandler();
    mediaSession.setActionHandler("play", duringHandoff.handler);

    assert.deepEqual(guard.invokePageAction("pause"), { kind: "invoked" });
    assert.deepEqual(guard.invokePageAction("play"), { kind: "invoked" });
    assert.equal(beforeHandoff.calls.length, 1);
    assert.equal(duringHandoff.calls.length, 1);
    // Вызов ничего не вернул браузеру: медиаклавиши по-прежнему у плеера.
    assert.equal(mediaSession.systemHandlers.size, 0);

    guard.restore();
    assert.equal(mediaSession.systemHandlers.get("pause"), beforeHandoff.handler);
    assert.equal(mediaSession.systemHandlers.get("play"), duringHandoff.handler);
  });

  it("без navigator.mediaSession — всегда no_handler", () => {
    const guard = installMediaSessionGuard({ navigator: {} } as unknown as Window);
    assert.deepEqual(guard.invokePageAction("play"), { kind: "no_handler" });
  });
});
