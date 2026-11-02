// Страж navigator.mediaSession на фейковом MediaSession: что видит «система» (браузер) и страница.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { installMediaSessionGuard } from "../../../src/page/media-session-guard.ts";

const KNOWN_ACTIONS = ["play", "pause", "seekto", "seekbackward", "seekforward", "nexttrack", "previoustrack", "stop"];

/** Фейковый MediaSession: поля `system*` — то, что браузер отдал бы системе медиакнопок. */
function createFakeWindow() {
  class FakeMediaMetadata {
    readonly title: string;
    constructor(init: { title: string }) {
      this.title = init.title;
    }
  }
  class FakeMediaSession {
    systemMetadata: unknown = null;
    systemPlaybackState = "none";
    readonly systemHandlers = new Map<string, unknown>();
    systemPositionState: unknown = undefined;
    /** Каждый вызов, дошедший до «браузера» (даже если тут же отменён). */
    readonly systemPositionCalls: unknown[] = [];
    get metadata(): unknown {
      return this.systemMetadata;
    }
    set metadata(value: unknown) {
      if (value !== null && !(value instanceof FakeMediaMetadata)) throw new TypeError("not MediaMetadata");
      this.systemMetadata = value;
    }
    get playbackState(): string {
      return this.systemPlaybackState;
    }
    set playbackState(value: string) {
      if (["none", "paused", "playing"].includes(value)) this.systemPlaybackState = value;
    }
    setActionHandler(action: string, handler: unknown): void {
      if (!KNOWN_ACTIONS.includes(action)) throw new TypeError(`unknown action ${action}`);
      if (handler === null) this.systemHandlers.delete(action);
      else this.systemHandlers.set(action, handler);
    }
    setPositionState(state?: { duration: number; position: number }): void {
      this.systemPositionCalls.push(state);
      if (state === undefined) {
        this.systemPositionState = undefined;
        return;
      }
      if (state.position > state.duration) throw new TypeError("position after duration");
      this.systemPositionState = state;
    }
  }
  const mediaSession = new FakeMediaSession();
  const win = { navigator: { mediaSession }, MediaMetadata: FakeMediaMetadata, TypeError };
  return { win: win as unknown as Window, mediaSession, FakeMediaMetadata };
}

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
