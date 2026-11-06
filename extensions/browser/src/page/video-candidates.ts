// Описание видеоэлементов документа для правила «какое видео отдать» (пункт 9 задания 09a),
// поиск живого MediaSource элемента и формы его источника (сессия 09b, `element-source.ts`).
//
// «Живой» MediaSource элемента: источник элемента (`src` или `currentSrc`, оригинальными
// геттерами) — blob-адрес нашего отслеживаемого MediaSource, у которого есть хотя бы один
// SourceBuffer и который не `closed` (то есть ещё подключён к элементу).

import type { CandidateMedia, DocumentTag, VideoCandidate } from "../shared/handoff-contract.ts";
import { MAX_SOURCE_MIME_TYPES, MAX_VIDEO_CANDIDATES_PER_DOCUMENT } from "../shared/page-message-validation.ts";
import { finiteDurationSeconds, frameDimensionPx, hostOfHttpUrl } from "./candidate-facts.ts";
import type { DomReaders, ViewportRect } from "./dom-readers.ts";
import type { ElementRegistry } from "./element-registry.ts";
import { candidateMediaOf, elementSourceOf, type ElementSource } from "./element-source.ts";
import type { MediaSourceReaders } from "./media-source-readers.ts";
import type { MediaSourceRecord, MseTracker } from "./mse-tracker.ts";
import type { OddityReporter } from "./prototype-wrap.ts";
import type { ScriptArrival } from "./script-arrival.ts";
import type { WorkerMediaSources } from "./worker-capture/index.ts";

/**
 * Порядок форм при отборе кандидатов сверх предела документа: чем меньше, тем вероятнее его
 * отдадут (как у выбора фона: MSE, затем файл; неподдержанные — чтобы фон мог честно сказать
 * «not supported yet»).
 */
const MEDIA_RANK: Readonly<Record<CandidateMedia["kind"], number>> = { media_source: 0, file: 1, unsupported: 2, none: 3 };

/** Видимая площадь: пересечение прямоугольника элемента с окном, в CSS-пикселях (целое). */
export function visibleAreaPx(rect: ViewportRect, viewport: { readonly width: number; readonly height: number }): number {
  const visibleWidth = Math.min(rect.right, viewport.width) - Math.max(rect.left, 0);
  const visibleHeight = Math.min(rect.bottom, viewport.height) - Math.max(rect.top, 0);
  if (!(visibleWidth > 0) || !(visibleHeight > 0)) return 0;
  return Math.round(visibleWidth * visibleHeight);
}

/** Что описанию кандидатов нужно знать о DRM (владелец метки — `DrmMarks`). */
export interface ProtectionReader {
  isProtected(element: object): boolean;
}

export class VideoCandidates {
  readonly #registry: ElementRegistry;
  readonly #tracker: MseTracker;
  readonly #dom: DomReaders;
  readonly #mediaSources: MediaSourceReaders;
  readonly #reportOddity: OddityReporter;
  readonly #workerMediaSources: WorkerMediaSources;
  readonly #protection: ProtectionReader;
  readonly #scriptArrival: ScriptArrival;
  /** Метка этого документа (одна на всю его жизнь). */
  readonly documentTag: DocumentTag;

  constructor(
    registry: ElementRegistry,
    tracker: MseTracker,
    dom: DomReaders,
    mediaSources: MediaSourceReaders,
    documentTag: DocumentTag,
    reportOddity: OddityReporter,
    workerMediaSources: WorkerMediaSources,
    protection: ProtectionReader,
    scriptArrival: ScriptArrival,
  ) {
    this.documentTag = documentTag;
    this.#registry = registry;
    this.#tracker = tracker;
    this.#dom = dom;
    this.#mediaSources = mediaSources;
    this.#reportOddity = reportOddity;
    this.#workerMediaSources = workerMediaSources;
    this.#protection = protection;
    this.#scriptArrival = scriptArrival;
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
        MEDIA_RANK[left.media.kind] - MEDIA_RANK[right.media.kind] ||
        Number(right.playing) - Number(left.playing) ||
        right.visible_area_px - left.visible_area_px,
    );
    return ranked.slice(0, MAX_VIDEO_CANDIDATES_PER_DOCUMENT);
  }

  #describeAll(): VideoCandidate[] {
    const candidates: VideoCandidate[] = [];
    // Одна проба скрытости на весь опрос: общие предки видео читаются один раз (visibility-probe.ts).
    const isHidden = this.#dom.createVisibilityProbe();
    const frameHost = this.#dom.documentHostName();
    for (const element of this.#registry.findVideoElements()) {
      try {
        candidates.push(this.#describeElement(element, isHidden, frameHost));
      } catch {
        this.#reportOddity("could not describe a video element");
      }
    }
    return candidates;
  }

  /** Все факты одного элемента; любое чтение может бросить — ловит `#describeAll`. */
  #describeElement(element: Element, isHidden: (element: object) => boolean, frameHost: string): VideoCandidate {
    const source = this.sourceOf(element);
    const frameSize = this.#dom.videoSizeOf(element);
    return {
      document: this.documentTag,
      element: this.#registry.keyOf(element),
      media: candidateMediaOf(source),
      playing: !this.#dom.isPaused(element) && !this.#dom.isEnded(element),
      visible_area_px: this.#visibleAreaOf(element, isHidden),
      duration_s: finiteDurationSeconds(this.#dom.durationSecondsOf(element)),
      muted: this.#dom.isSilent(element),
      loop: this.#dom.isLooping(element),
      video_width_px: frameDimensionPx(frameSize.width),
      video_height_px: frameDimensionPx(frameSize.height),
      drm: this.#protection.isProtected(element),
      frame_host: frameHost,
      file_host: source.kind === "file" ? hostOfHttpUrl(source.url) : null,
      source_mime_types: source.kind === "media_source" ? this.#tracker.sourceMimeTypesOf(source.record).slice(0, MAX_SOURCE_MIME_TYPES) : [],
    };
  }

  /**
   * Видимая площадь: пересечение с окном, 0 для скрытого (`display:none`, `visibility:hidden`,
   * `opacity:0` у элемента или предка — решение 68). Стили читаем только у тех, кто попал в окно:
   * за экраном площадь и так 0, а чтение стилей — самое дорогое в опросе.
   */
  #visibleAreaOf(element: Element, isHidden: (element: object) => boolean): number {
    const area = visibleAreaPx(this.#dom.rectOf(element), this.#dom.viewportSize());
    return area > 0 && isHidden(element) ? 0 : area;
  }

  /** Форма источника элемента (чтения могут бросить — ловит вызывающий). */
  sourceOf(element: Element): ElementSource {
    return elementSourceOf(element, {
      dom: this.#dom,
      liveMediaSourceOf: (candidate) => this.liveMediaSourceOf(candidate),
      isMediaSourceObjectUrl: (url) => this.#registry.isMediaSourceObjectUrl(url),
      resolveWorkerHandle: (candidate) => this.#workerMediaSources.resolveHandle(candidate),
      scriptArrival: this.#scriptArrival,
    });
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
