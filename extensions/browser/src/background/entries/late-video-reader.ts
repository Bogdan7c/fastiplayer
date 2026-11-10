// Чтение и постановка позиции видео для позднего входа (сессия 10d).
//
// Работает из ИЗОЛИРОВАННОГО мира через DOM, а не через наш MAIN-скрипт, потому что до перезагрузки в Chrome
// скрипта на странице нет. Подмены сайта и наши подмены прототипов живут в MAIN-мире, поэтому отсюда
// `currentTime` — настоящий геттер и сеттер браузера: сайт увидит обычную перемотку.
//
// Элементы не сохраняются: фон сравнивает видео только по длительности, а ссылка на элемент не пережила бы
// перезагрузку. Длительность — единственное, что одинаково до и после неё и отличает фильм от рекламы.

/** Секунд на расхождение при сравнении длительностей и позиций: так задано планом, ±2 с. */
export const LATE_POSITION_TOLERANCE_S = 2;

/**
 * Сколько ждём ответа фреймов при чтении и постановке позиции (сессия 15c). Обход видео во фрейме —
 * миллисекунды; срок нужен только против замёрзшего фрейма, и он тот же, что у опроса вкладки (5 с).
 */
export const LATE_VIDEO_VISIT_DEADLINE_MS = 5_000;

/** Что фон узнал об одном видео страницы. */
export interface LateVideoFact {
  readonly position_s: number;
  /** `null` — длительность неизвестна или бесконечна (эфир). */
  readonly duration_s: number | null;
  readonly playing: boolean;
  /** Видимая площадь, CSS-пиксели: выбрать видео, на которое человек смотрел. */
  readonly visible_area_px: number;
}

/** Просьба поставить позицию: видео этой длительности встаёт на эту секунду. */
export interface LateSeekTarget {
  readonly duration_s: number;
  readonly position_s: number;
}

/** Результат внедрения в один фрейм; Firefox кладёт `undefined` вместо фрейма, куда внедрить нельзя. */
export interface LateFrameResult {
  readonly result?: unknown;
}

/** Внедрение `visitFrameVideos` во фреймы вкладки; ответы фреймов, которые ответили до срока. */
export type ExecuteVideoVisit = (tabId: number, target: LateSeekTarget | null) => Promise<readonly (LateFrameResult | undefined)[]>;

export interface LateVideoReader {
  /** Все видео всех фреймов. Сбой внедрения — отказ промиса. */
  read(tabId: number): Promise<LateVideoFact[]>;
  /** Поставить видео с такой длительностью на секунду. Сбой внедрения — отказ промиса. */
  seek(tabId: number, target: LateSeekTarget): Promise<void>;
}

/**
 * Внедряемая функция: обойти видео фрейма и, если задано, поставить нужное на секунду. Браузер сериализует
 * её текст, поэтому она самодостаточна, а допуск приходит вместе с целью. Заходит и в закрытые shadow root:
 * изолированному миру это разрешено, а без него видео такого плеера мы бы просто не увидели.
 */
export function visitFrameVideos(target: LateSeekTarget | null, durationToleranceS: number): unknown {
  interface ShadowDomApi {
    readonly dom?: { readonly openOrClosedShadowRoot?: (element: Element) => ShadowRoot | null };
  }
  const scope = globalThis as unknown as { browser?: ShadowDomApi; chrome?: ShadowDomApi };
  const extensionApi = scope.browser ?? scope.chrome;
  const shadowRootOf = (element: Element): ShadowRoot | null => {
    try {
      return extensionApi?.dom?.openOrClosedShadowRoot?.(element) ?? element.shadowRoot;
    } catch {
      // Chrome бросает на элементах, где shadow root не бывает (например, SVG): открытый корень всё равно есть в DOM.
      return element.shadowRoot;
    }
  };
  const videos: HTMLVideoElement[] = [];
  const collect = (root: ParentNode): void => {
    for (const element of root.querySelectorAll("*")) {
      if (element.localName === "video") videos.push(element as HTMLVideoElement);
      const shadowRoot = shadowRootOf(element);
      if (shadowRoot !== null) collect(shadowRoot);
    }
  };
  collect(document);
  const visibleAreaOf = (video: HTMLVideoElement): number => {
    const rect = video.getBoundingClientRect();
    const width = Math.min(rect.right, window.innerWidth) - Math.max(rect.left, 0);
    const height = Math.min(rect.bottom, window.innerHeight) - Math.max(rect.top, 0);
    return width > 0 && height > 0 ? Math.round(width * height) : 0;
  };
  return videos.map((video) => {
    if (target !== null && Math.abs(video.duration - target.duration_s) <= durationToleranceS) video.currentTime = target.position_s;
    return {
      position_s: video.currentTime,
      duration_s: Number.isFinite(video.duration) ? video.duration : null,
      playing: !video.paused && !video.ended,
      visible_area_px: visibleAreaOf(video),
    };
  });
}

function isFiniteNonNegative(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value) && value >= 0;
}

/** Строгий разбор одного факта: ответ шёл через страницу, кривая запись просто отбрасывается. */
function parseVideoFact(value: unknown): LateVideoFact | undefined {
  if (typeof value !== "object" || value === null) return undefined;
  const { position_s: positionS, duration_s: durationS, playing, visible_area_px: visibleAreaPx } = value as Record<string, unknown>;
  const valid = isFiniteNonNegative(positionS) && (durationS === null || isFiniteNonNegative(durationS)) && typeof playing === "boolean" && isFiniteNonNegative(visibleAreaPx);
  return valid ? { position_s: positionS, duration_s: durationS as number | null, playing, visible_area_px: visibleAreaPx } : undefined;
}

/** Факты всех фреймов. Фрейм без ответа (Firefox, служебный документ) просто не даёт фактов. */
export function parseVideoFacts(results: readonly (LateFrameResult | undefined)[]): LateVideoFact[] {
  const facts: LateVideoFact[] = [];
  for (const frame of results) {
    if (!Array.isArray(frame?.result)) continue;
    for (const entry of frame.result as readonly unknown[]) {
      const fact = parseVideoFact(entry);
      if (fact !== undefined) facts.push(fact);
    }
  }
  return facts;
}

export function createLateVideoReader(execute: ExecuteVideoVisit): LateVideoReader {
  return {
    read: async (tabId) => parseVideoFacts(await execute(tabId, null)),
    seek: async (tabId, target) => void (await execute(tabId, target)),
  };
}
