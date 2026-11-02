// Сборка тени на фейковом DOM: часы под управлением теста, журнал событий наружу и страж mediaSession.

import type { PageEvent, ShadowCommand } from "../../../src/shared/handoff-contract.ts";
import type { MediaSessionGuard } from "../../../src/page/media-session-guard.ts";
import { captureElementOriginals, startShadow, type ElementOriginals, type ShadowSession } from "../../../src/page/shadow/index.ts";
import { createFakeMediaDom, type FakeMediaDom, type FakeVideo } from "./fake-media-dom.ts";

export interface FakeClock {
  nowMs: number;
  advance(ms: number): void;
}

export function createFakeClock(startMs = 1_000): FakeClock {
  return {
    nowMs: startMs,
    advance(ms) {
      this.nowMs += ms;
    },
  };
}

/** Страж mediaSession, который только считает вызовы (сам страж проверяется отдельно). */
export function createCountingGuard() {
  const calls: ("suspend" | "restore")[] = [];
  const guard: MediaSessionGuard = {
    suspend: () => void calls.push("suspend"),
    restore: () => void calls.push("restore"),
  };
  return { guard, calls };
}

export interface ShadowHarness {
  readonly dom: FakeMediaDom;
  readonly originals: ElementOriginals;
  readonly clock: FakeClock;
  readonly emitted: PageEvent[];
  readonly guardCalls: ("suspend" | "restore")[];
  /** Blob-адреса, которые «реестр MAIN-мира» знает как MediaSource (видео из playingVideo — ms-1). */
  readonly mediaSourceUrls: Set<string>;
  start(video: FakeVideo): ShadowSession;
  /** Тик часов плеера. */
  tick(session: ShadowSession, positionSeconds: number, state: "playing" | "paused" | "waiting" | "ended", ratePermille?: number): void;
}

/** Blob-адрес MediaSource, с которым фейковый DOM создаёт видео (fake-media-dom.ts). */
export const PLAYING_VIDEO_MEDIA_SOURCE_URL = "blob:https://site.example/ms-1";

/** Новый DOM, оригиналы сняты «на document_start» (до любых подмен теста). */
export function createShadowHarness(): ShadowHarness {
  const dom = createFakeMediaDom();
  const originals = captureElementOriginals(dom.window as unknown as Window);
  const clock = createFakeClock();
  const emitted: PageEvent[] = [];
  const { guard, calls } = createCountingGuard();
  const mediaSourceUrls = new Set<string>([PLAYING_VIDEO_MEDIA_SOURCE_URL]);
  return {
    dom,
    originals,
    clock,
    emitted,
    guardCalls: calls,
    mediaSourceUrls,
    start(video) {
      return startShadow(video as unknown as HTMLMediaElement, (event) => void emitted.push(event), {
        win: dom.window as unknown as Window,
        originals,
        mediaSession: guard,
        now: () => clock.nowMs,
        isMediaSourceObjectUrl: (url) => mediaSourceUrls.has(url),
      });
    },
    tick(session, positionSeconds, state, ratePermille = 1000) {
      const command: ShadowCommand = { kind: "clock", position_us: Math.round(positionSeconds * 1_000_000), state, rate: ratePermille };
      session.apply(command);
    },
  };
}

/** Видео, которое «играло» на странице до передачи. */
export function playingVideo(dom: FakeMediaDom, atSeconds = 10): FakeVideo {
  const video = dom.createVideo();
  video.real.paused = false;
  video.real.currentTime = atSeconds;
  return video;
}
