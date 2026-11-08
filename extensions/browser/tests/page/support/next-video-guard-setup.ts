// Общая обвязка тестов сторожа следующего видео (сессии 09c2 и 11b2): НАСТОЯЩАЯ тень на фейковом DOM.
import assert from "node:assert/strict";
import type { DomReaders, ViewportRect } from "../../../src/page/dom-readers.ts";
import type { PointerPress, ViewerActivationReader } from "../../../src/page/auto-handoff/viewer-activation.ts";
import type { ElementSource } from "../../../src/page/element-source.ts";
import type { ElementRegistry } from "../../../src/page/element-registry.ts";
import { ViewerPlayWatch } from "../../../src/page/auto-handoff/viewer-play-watch.ts";
import { NextVideoGuard } from "../../../src/page/next-video-guard.ts";
import { heldShadowStarterFor, playWatchSeamFor } from "../../../src/page/shadow-assembly.ts";
import type { VideoCandidates } from "../../../src/page/video-candidates.ts";
import type { PageEvent, VideoCandidate } from "../../../src/shared/handoff-contract.ts";
import { DEFAULT_CANDIDATE_FACTS } from "../../support/video-candidate-defaults.ts";
import type { FakeVideo } from "../shadow/fake-media-dom.ts";
import { createCountingGuard, createShadowHarness, type ShadowHarness } from "../shadow/shadow-harness.ts";

export const FILE_URL = "https://cdn.example/ad.mp4";
export const LEASE_MS = 3_000;

/** Прямоугольник каждого видео в тестах сторожа (все лежат в одном месте экрана, клики судятся по нему). */
export const VIDEO_RECT: ViewportRect = { left: 0, top: 0, right: 640, bottom: 360 };
/** Точка внутри `VIDEO_RECT` (кнопка Play поверх видео) и точка вне его (лайк под видео). */
export const POINT_ON_VIDEO = { x: 320, y: 180 } as const;
export const POINT_OFF_VIDEO = { x: 320, y: 500 } as const;

/**
 * Ввод человека под управлением теста (решение 262). `clickOnVideo = true` — человек только что нажал мышью
 * по видео (в точке `POINT_ON_VIDEO`, по часам теста); `false` — нажатий не было. `press` можно задать и
 * прямо (клик вне видео, давний клик).
 */
export interface FakeActivation extends ViewerActivationReader {
  press: PointerPress | null;
  lastInputAtMs: number;
  clickOnVideo: boolean;
}

export function createFakeActivation(now: () => number): FakeActivation {
  return {
    press: null,
    lastInputAtMs: Number.NEGATIVE_INFINITY,
    get clickOnVideo(): boolean {
      return this.press !== null;
    },
    set clickOnVideo(value: boolean) {
      if (!value) {
        this.press = null;
        return;
      }
      this.press = { atMs: now(), ...POINT_ON_VIDEO };
      this.lastInputAtMs = now();
    },
    lastPointerPress() {
      return this.press;
    },
    lastTrustedInputAtMs() {
      return this.lastInputAtMs;
    },
  };
}

/** Сигнал предложения «Ask» под управлением теста: что встало, сколько раз ушло, принял ли изолированный мир. */
export interface FakeViewerOfferSignals {
  readonly stood: object[];
  ended: number;
  /** `false` — изолированный мир о предложении не узнал (имени нет, элемент вне документа). */
  accepts: boolean;
  offerStood(element: object): boolean;
  offerEnded(): void;
}

function createFakeViewerOfferSignals(): FakeViewerOfferSignals {
  return {
    stood: [],
    ended: 0,
    accepts: true,
    offerStood(element) {
      if (!this.accepts) return false;
      this.stood.push(element);
      return true;
    },
    offerEnded() {
      this.ended += 1;
    },
  };
}

/** Страница и настоящая тень вокруг сторожа. `collectOddities` — копить странности вместо провала теста. */
export function setupGuard(options: { readonly collectOddities?: boolean } = {}) {
  const harness: ShadowHarness = createShadowHarness();
  const sent: PageEvent[] = [];
  const videos: FakeVideo[] = [];
  const protectedVideos = new Set<object>();
  const sources = new Map<object, ElementSource>();
  const mediaSession = createCountingGuard();
  const environment = {
    win: harness.dom.window as unknown as Window,
    originals: harness.originals,
    mediaSession: mediaSession.guard,
    now: () => harness.clock.nowMs,
    scheduleTimeout: harness.timers.schedule,
  };
  const keyOf = (element: object): number => videos.indexOf(element as FakeVideo) + 1;
  const dom = {
    isVideoElement: (candidate: unknown) => videos.includes(candidate as FakeVideo),
    isPaused: (element: object) => (element as FakeVideo).real.paused,
    isEnded: () => false,
    readyStateOf: (element: object) => (element as FakeVideo).real.readyState,
    currentTimeSeconds: (element: object) => (element as FakeVideo).real.currentTime,
    durationSecondsOf: (element: object) => (element as FakeVideo).real.duration,
    isSilent: (element: object) => (element as FakeVideo).real.muted === true,
    isLooping: (element: object) => (element as unknown as { loop?: boolean }).loop === true,
    currentSrcOf: (element: object) => String((element as FakeVideo).real.src),
  } as unknown as DomReaders;
  const describeVideos = (): VideoCandidate[] =>
    videos.map((video) => ({
      ...DEFAULT_CANDIDATE_FACTS,
      document: "doc-a",
      element: keyOf(video),
      media: { kind: "file" },
      playing: !video.real.paused,
      visible_area_px: 400_000,
    }));
  const activation = createFakeActivation(() => harness.clock.nowMs);
  const viewerOffer = createFakeViewerOfferSignals();
  const oddities: string[] = [];
  const viewerPlay = new ViewerPlayWatch({ activation, now: () => harness.clock.nowMs, rectOf: () => VIDEO_RECT });
  const guard = new NextVideoGuard({
    dom,
    viewerPlay,
    registry: {
      findVideoElements: () => videos,
      keyOf,
      elementByKey: (key: number) => videos[key - 1] ?? null,
      isMediaSourceObjectUrl: () => true,
    } as unknown as ElementRegistry,
    candidates: {
      sourceOf: (element: Element) => sources.get(element) ?? { kind: "file", url: FILE_URL },
      describe: describeVideos,
    } as unknown as VideoCandidates,
    startHeldShadow: heldShadowStarterFor(environment),
    seam: playWatchSeamFor(environment),
    isProtected: (element) => protectedVideos.has(element),
    viewerOffer,
    send: (event) => void sent.push(event),
    now: () => harness.clock.nowMs,
    reportOddity: (line) => (options.collectOddities === true ? void oddities.push(line) : assert.fail(`oddity: ${line}`)),
    documentEvents: harness.dom.window,
  });
  const addVideo = (configure: (video: FakeVideo) => void = () => {}): FakeVideo => {
    const video = harness.dom.createVideo() as unknown as FakeVideo;
    configure(video);
    videos.push(video);
    return video;
  };
  return { harness, guard, sent, addVideo, sources, protectedVideos, keyOf, activation, viewerPlay, viewerOffer, oddities, mediaSessionCalls: mediaSession.calls };
}

/** Страница зовёт `play()` как обычно: через прототип с нашими переопределениями. */
export function pagePlay(video: FakeVideo): Promise<void> {
  return (video as unknown as { play(): Promise<void> }).play();
}

export const browserPlays = (video: FakeVideo): number => video.realCalls.filter((call) => call.kind === "play").length;

