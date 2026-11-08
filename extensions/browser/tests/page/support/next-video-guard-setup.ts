// Общая обвязка тестов сторожа следующего видео (сессии 09c2 и 11b2): НАСТОЯЩАЯ тень на фейковом DOM.
import assert from "node:assert/strict";
import type { DomReaders } from "../../../src/page/dom-readers.ts";
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

/** Ввод человека под управлением теста: что сказал бы браузер и когда был последний доверенный ввод. */
export interface FakeActivation {
  transient: boolean;
  lastInputAtMs: number;
  hasTransientActivation(): boolean;
  lastTrustedInputAtMs(): number;
}

export function createFakeActivation(): FakeActivation {
  return {
    transient: false,
    lastInputAtMs: Number.NEGATIVE_INFINITY,
    hasTransientActivation() {
      return this.transient;
    },
    lastTrustedInputAtMs() {
      return this.lastInputAtMs;
    },
  };
}

/** Страница и настоящая тень вокруг сторожа. */
export function setupGuard() {
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
  const activation = createFakeActivation();
  const viewerPlay = new ViewerPlayWatch({ activation, now: () => harness.clock.nowMs });
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
    send: (event) => void sent.push(event),
    now: () => harness.clock.nowMs,
    reportOddity: (line) => assert.fail(`oddity: ${line}`),
    documentEvents: harness.dom.window,
  });
  const addVideo = (configure: (video: FakeVideo) => void = () => {}): FakeVideo => {
    const video = harness.dom.createVideo() as unknown as FakeVideo;
    configure(video);
    videos.push(video);
    return video;
  };
  return { harness, guard, sent, addVideo, sources, protectedVideos, keyOf, activation, viewerPlay, mediaSessionCalls: mediaSession.calls };
}

/** Страница зовёт `play()` как обычно: через прототип с нашими переопределениями. */
export function pagePlay(video: FakeVideo): Promise<void> {
  return (video as unknown as { play(): Promise<void> }).play();
}

export const browserPlays = (video: FakeVideo): number => video.realCalls.filter((call) => call.kind === "play").length;

