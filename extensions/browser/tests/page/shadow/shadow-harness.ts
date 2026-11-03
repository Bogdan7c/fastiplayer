// Сборка тени на фейковом DOM: часы под управлением теста, журнал событий наружу и страж mediaSession.

import type { PageEvent, ShadowCommand } from "../../../src/shared/handoff-contract.ts";
import type { MediaSessionGuard } from "../../../src/page/media-session-guard.ts";
import { captureElementOriginals, installShadowOverrides, startShadow, type ElementOriginals, type ShadowSession } from "../../../src/page/shadow/index.ts";
import type { ShadowSourceKind } from "../../../src/page/handoff-seam.ts";
import type { TimeoutScheduler } from "../../../src/page/shadow/file-return.ts";
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

/** Таймеры окна под управлением теста (страховка возврата файла). */
export interface FakeTimers {
  readonly schedule: TimeoutScheduler;
  /** Сколько отложенных вызовов ждут. */
  pending(): number;
  /** Исполнить все ждущие вызовы (как будто срок вышел). */
  fireAll(): void;
}

export function createFakeTimers(): FakeTimers {
  const waiting = new Set<() => void>();
  return {
    schedule: (callback) => {
      const entry = (): void => callback();
      waiting.add(entry);
      return () => void waiting.delete(entry);
    },
    pending: () => waiting.size,
    fireAll: () => {
      const due = [...waiting];
      waiting.clear();
      for (const callback of due) callback();
    },
  };
}

export interface ShadowHarness {
  readonly dom: FakeMediaDom;
  readonly originals: ElementOriginals;
  readonly clock: FakeClock;
  readonly emitted: PageEvent[];
  readonly guardCalls: ("suspend" | "restore")[];
  /** Blob-адреса, которые «реестр MAIN-мира» знает как MediaSource (видео из playingVideo — ms-1). */
  readonly mediaSourceUrls: Set<string>;
  readonly timers: FakeTimers;
  start(video: FakeVideo): ShadowSession;
  /** Тень файла `<video src>` (сессия 09b). */
  startFile(video: FakeVideo): ShadowSession;
  /** Тик часов плеера. */
  tick(session: ShadowSession, positionSeconds: number, state: "playing" | "paused" | "waiting" | "ended", ratePermille?: number): void;
}

/** Blob-адрес MediaSource, с которым фейковый DOM создаёт видео (fake-media-dom.ts). */
export const PLAYING_VIDEO_MEDIA_SOURCE_URL = "blob:https://site.example/ms-1";

export interface ShadowHarnessOptions {
  /** Что успело случиться ДО нашего document_start (например, подмена прототипа другим экстеншеном). */
  readonly beforeDocumentStart?: (dom: FakeMediaDom) => void;
}

/**
 * Новый DOM, «document_start» как в `page/main.ts`: оригиналы сняты и переопределения прототипов
 * поставлены сразу (решение 181), до любых подмен теста.
 */
export function createShadowHarness(options: ShadowHarnessOptions = {}): ShadowHarness {
  const dom = createFakeMediaDom();
  options.beforeDocumentStart?.(dom);
  const originals = captureElementOriginals(dom.window as unknown as Window);
  installShadowOverrides(originals);
  const clock = createFakeClock();
  const emitted: PageEvent[] = [];
  const { guard, calls } = createCountingGuard();
  const mediaSourceUrls = new Set<string>([PLAYING_VIDEO_MEDIA_SOURCE_URL]);
  const timers = createFakeTimers();
  const startWith = (video: FakeVideo, source: ShadowSourceKind): ShadowSession =>
    startShadow(
      video as unknown as HTMLMediaElement,
      (event) => void emitted.push(event),
      {
        win: dom.window as unknown as Window,
        originals,
        mediaSession: guard,
        now: () => clock.nowMs,
        isMediaSourceObjectUrl: (url) => mediaSourceUrls.has(url),
        scheduleTimeout: timers.schedule,
      },
      source,
    );
  return {
    dom,
    originals,
    clock,
    emitted,
    guardCalls: calls,
    mediaSourceUrls,
    timers,
    start: (video) => startWith(video, "media_source"),
    startFile: (video) => startWith(video, "file"),
    tick(session, positionSeconds, state, ratePermille = 1000) {
      const command: ShadowCommand = { kind: "clock", position_us: Math.round(positionSeconds * 1_000_000), state, rate: ratePermille };
      session.apply(command);
    },
  };
}

/** Сколько пульсов `shadow_alive` ушло наружу. */
export function pulseCount(harness: ShadowHarness): number {
  return harness.emitted.filter((event) => event.kind === "shadow_alive").length;
}

/** События наружу без пульса (для проверок «что ещё сказала тень»). */
export function emittedWithoutPulse(harness: ShadowHarness): PageEvent[] {
  return harness.emitted.filter((event) => event.kind !== "shadow_alive");
}

/** Видео, которое «играло» на странице до передачи. */
export function playingVideo(dom: FakeMediaDom, atSeconds = 10): FakeVideo {
  const video = dom.createVideo();
  video.real.paused = false;
  video.real.currentTime = atSeconds;
  return video;
}
