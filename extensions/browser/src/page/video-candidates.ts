// Описание видеоэлементов документа для правила «какое видео отдать» (пункт 9 задания 09a) и
// поиск живого MediaSource элемента.
//
// «Живой» MediaSource элемента: источник элемента (`src` или `currentSrc`, оригинальными
// геттерами) — blob-адрес нашего отслеживаемого MediaSource, у которого есть хотя бы один
// SourceBuffer и который не `closed` (то есть ещё подключён к элементу).

import type { DocumentTag, VideoCandidate } from "../shared/handoff-contract.ts";
import { MAX_VIDEO_CANDIDATES_PER_DOCUMENT } from "../shared/page-message-validation.ts";
import type { DomReaders, ViewportRect } from "./dom-readers.ts";
import type { ElementRegistry } from "./element-registry.ts";
import type { MediaSourceReaders } from "./media-source-readers.ts";
import type { MediaSourceRecord, MseTracker } from "./mse-tracker.ts";
import type { OddityReporter } from "./prototype-wrap.ts";

/** Видимая площадь: пересечение прямоугольника элемента с окном, в CSS-пикселях (целое). */
export function visibleAreaPx(rect: ViewportRect, viewport: { readonly width: number; readonly height: number }): number {
  const visibleWidth = Math.min(rect.right, viewport.width) - Math.max(rect.left, 0);
  const visibleHeight = Math.min(rect.bottom, viewport.height) - Math.max(rect.top, 0);
  if (!(visibleWidth > 0) || !(visibleHeight > 0)) return 0;
  return Math.round(visibleWidth * visibleHeight);
}

export class VideoCandidates {
  readonly #registry: ElementRegistry;
  readonly #tracker: MseTracker;
  readonly #dom: DomReaders;
  readonly #mediaSources: MediaSourceReaders;
  readonly #reportOddity: OddityReporter;
  /** Метка этого документа (одна на всю его жизнь). */
  readonly documentTag: DocumentTag;

  constructor(
    registry: ElementRegistry,
    tracker: MseTracker,
    dom: DomReaders,
    mediaSources: MediaSourceReaders,
    documentTag: DocumentTag,
    reportOddity: OddityReporter,
  ) {
    this.documentTag = documentTag;
    this.#registry = registry;
    this.#tracker = tracker;
    this.#dom = dom;
    this.#mediaSources = mediaSources;
    this.#reportOddity = reportOddity;
  }

  /**
   * Описать `<video>` документа (включая shadow root). Сломанный элемент пропускается.
   * Изолированный мир принимает не больше MAX_VIDEO_CANDIDATES_PER_DOCUMENT кандидатов (иначе
   * отвергает весь ответ), поэтому при избытке остаются самые вероятные по правилу пункта 9:
   * с живым MediaSource, затем играющие, затем по видимой площади.
   */
  describe(): VideoCandidate[] {
    const candidates = this.#describeAll();
    if (candidates.length <= MAX_VIDEO_CANDIDATES_PER_DOCUMENT) return candidates;
    const ranked = [...candidates].sort(
      (left, right) =>
        Number(right.has_live_media) - Number(left.has_live_media) ||
        Number(right.playing) - Number(left.playing) ||
        right.visible_area_px - left.visible_area_px,
    );
    return ranked.slice(0, MAX_VIDEO_CANDIDATES_PER_DOCUMENT);
  }

  #describeAll(): VideoCandidate[] {
    const candidates: VideoCandidate[] = [];
    for (const element of this.#registry.findVideoElements()) {
      try {
        candidates.push({
          document: this.documentTag,
          element: this.#registry.keyOf(element),
          has_live_media: this.liveMediaSourceOf(element) !== null,
          playing: !this.#dom.isPaused(element) && !this.#dom.isEnded(element),
          visible_area_px: visibleAreaPx(this.#dom.rectOf(element), this.#dom.viewportSize()),
        });
      } catch {
        this.#reportOddity("could not describe a video element");
      }
    }
    return candidates;
  }

  /** Живой MediaSource элемента (`null` — нет: не MSE, не наш, без SourceBuffer или closed). */
  liveMediaSourceOf(element: Element): MediaSourceRecord | null {
    for (const url of this.#dom.mediaSourceUrlsOf(element)) {
      const mediaSource = this.#registry.mediaSourceAt(url);
      if (mediaSource === null) continue;
      const record = this.#tracker.recordOf(mediaSource);
      if (record === null || this.#tracker.sourceBufferCountOf(record) === 0) continue;
      const state = this.#mediaSources.readyStateOf(mediaSource);
      if (state === null || state === "closed") continue;
      return record;
    }
    return null;
  }
}
