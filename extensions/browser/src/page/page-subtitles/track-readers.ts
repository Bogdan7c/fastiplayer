// Чтение `<track>` и `TextTrack` переданного видео оригинальными геттерами, снятыми на document_start.
//
// Страница (и плееры вроде hls.js) вольна подменять свойства этих прототипов; нам нужны настоящие
// значения. Любое чтение может бросить (чужой «бренд» объекта) — вызывающий ловит.

import { NO_CUE_SETTINGS, type CueSettingsSnapshot, type CueSnapshot } from "./web-vtt-builder.ts";

/** Окно с типами `<track>` и `TextTrack`; чего нет — читателей нет вовсе (`null`). */
export interface TrackReadersWindow {
  readonly Element: { readonly prototype: object };
  readonly HTMLMediaElement: { readonly prototype: object };
  readonly HTMLTrackElement?: { readonly prototype: object };
  readonly TextTrack?: { readonly prototype: object };
  readonly TextTrackCue?: { readonly prototype: object };
  readonly VTTCue?: { readonly prototype: object };
}

/** Режим `TextTrack.mode`. */
export type TextTrackMode = "disabled" | "hidden" | "showing";

/** Как плееру сайта показан `<track src>`: нужные поля элемента. */
export interface TrackElementFacts {
  /** `src` — абсолютный адрес ("" — атрибута нет). */
  readonly src: string;
  readonly srclang: string;
  readonly label: string;
}

/** Чтения дорожек. */
export interface TrackReaders {
  /** Все `<track>` — потомки видео (в порядке документа). */
  trackElementsOf(video: object): readonly object[];
  trackElementFacts(trackElement: object): TrackElementFacts;
  /** `TextTrack` этого `<track>` (`null` — нет). */
  textTrackOfElement(trackElement: object): object | null;
  /** Список `TextTrackList` видео. */
  textTrackListOf(video: object): object;
  /** Дорожки списка по порядку. */
  textTracksIn(list: object): readonly object[];
  modeOf(track: object): TextTrackMode;
  kindOf(track: object): string;
  languageOf(track: object): string;
  labelOf(track: object): string;
  /** Число реплик; `null` — реплики не загружены / дорожка выключена (`cues === null`). */
  cueCountOf(track: object): number | null;
  /** Реплики (не больше `limit`) простыми данными; `null` — реплик нет. */
  cuesOf(track: object, limit: number): readonly CueSnapshot[] | null;
}

/** Найти аксессор по цепочке прототипов. */
function findGetter(prototype: object | undefined, name: string): ((target: unknown) => unknown) | undefined {
  let current: object | null = prototype ?? null;
  while (current !== null) {
    const getter = Object.getOwnPropertyDescriptor(current, name)?.get;
    if (getter !== undefined) return (target) => Reflect.apply(getter, target, []);
    current = Object.getPrototypeOf(current) as object | null;
  }
  return undefined;
}

function requiredGetter(prototype: object | undefined, name: string): (target: unknown) => unknown {
  const getter = findGetter(prototype, name);
  if (getter === undefined) throw new TypeError(`no getter ${name}`);
  return getter;
}

function findMethod(prototype: object, name: string): (target: unknown, ...args: unknown[]) => unknown {
  let current: object | null = prototype;
  while (current !== null) {
    const method: unknown = Object.getOwnPropertyDescriptor(current, name)?.value;
    if (typeof method === "function") return (target, ...args) => Reflect.apply(method, target, args);
    current = Object.getPrototypeOf(current) as object | null;
  }
  throw new TypeError(`no method ${name}`);
}

/** Значения по умолчанию VTTCue: их в настройки документа не пишем. */
const DEFAULT_CUE_ALIGN = "center";
const DEFAULT_CUE_SIZE = 100;
const DEFAULT_CUE_VERTICAL = "";

function stringOr(value: unknown, fallback: string): string {
  return typeof value === "string" ? value : fallback;
}

function numberOr(value: unknown, fallback: number): number {
  return typeof value === "number" ? value : fallback;
}

/**
 * Элементы списков DOM (`NodeList`, `TextTrackList`, `TextTrackCueList`) по индексам, не больше `limit`;
 * чужие итераторы не зовутся. Длина читается обычным свойством списка: прототип списка страница вольна
 * подменить, но тогда она лишь скроет от нас свои же дорожки, а перебор всё равно ограничен `limit`.
 */
function elementsOf(list: unknown, limit: number): unknown[] {
  const indexed = list as { readonly length: unknown; readonly [index: number]: unknown };
  const length = typeof indexed.length === "number" && Number.isInteger(indexed.length) && indexed.length > 0 ? indexed.length : 0;
  const items: unknown[] = [];
  for (let index = 0; index < Math.min(length, limit); index += 1) items.push(indexed[index]);
  return items;
}

/** Длина списка DOM тем же правилом, что `elementsOf`. */
function lengthOfList(list: object): number {
  const length = (list as { readonly length: unknown }).length;
  return typeof length === "number" && Number.isInteger(length) && length > 0 ? length : 0;
}

/** Предел числа дорожек, которые обходим у одного видео (настоящих — единицы). */
const MAX_TRACKS_PER_VIDEO = 128;

export function captureTrackReaders(win: TrackReadersWindow): TrackReaders | null {
  if (win.HTMLTrackElement === undefined || win.TextTrack === undefined) return null;
  try {
    const querySelectorAll = findMethod(win.Element.prototype, "querySelectorAll");
    const trackSrc = requiredGetter(win.HTMLTrackElement.prototype, "src");
    const trackSrclang = requiredGetter(win.HTMLTrackElement.prototype, "srclang");
    const trackLabel = requiredGetter(win.HTMLTrackElement.prototype, "label");
    const trackTextTrack = requiredGetter(win.HTMLTrackElement.prototype, "track");
    const mediaTextTracks = requiredGetter(win.HTMLMediaElement.prototype, "textTracks");
    const mode = requiredGetter(win.TextTrack.prototype, "mode");
    const kind = requiredGetter(win.TextTrack.prototype, "kind");
    const language = requiredGetter(win.TextTrack.prototype, "language");
    const label = requiredGetter(win.TextTrack.prototype, "label");
    const cues = requiredGetter(win.TextTrack.prototype, "cues");
    const cueBase = win.TextTrackCue?.prototype ?? win.VTTCue?.prototype;
    const cueId = requiredGetter(cueBase, "id");
    const cueStart = requiredGetter(cueBase, "startTime");
    const cueEnd = requiredGetter(cueBase, "endTime");
    const cueText = findGetter(win.VTTCue?.prototype, "text");
    const cueAlign = findGetter(win.VTTCue?.prototype, "align");
    const cueLine = findGetter(win.VTTCue?.prototype, "line");
    const cueSnapToLines = findGetter(win.VTTCue?.prototype, "snapToLines");
    const cuePosition = findGetter(win.VTTCue?.prototype, "position");
    const cueSize = findGetter(win.VTTCue?.prototype, "size");
    const cueVertical = findGetter(win.VTTCue?.prototype, "vertical");

    /** Настройки реплики, отличающиеся от умолчаний. */
    const settingsOf = (cue: unknown): CueSettingsSnapshot => {
      if (cueAlign === undefined) return NO_CUE_SETTINGS;
      const align = stringOr(cueAlign(cue), DEFAULT_CUE_ALIGN);
      const line = cueLine?.(cue);
      const position = cuePosition?.(cue);
      const size = numberOr(cueSize?.(cue), DEFAULT_CUE_SIZE);
      const vertical = stringOr(cueVertical?.(cue), DEFAULT_CUE_VERTICAL);
      return {
        align: align === DEFAULT_CUE_ALIGN ? null : align,
        line: typeof line === "number" ? line : null,
        lineIsPercent: cueSnapToLines?.(cue) === false,
        position: typeof position === "number" ? position : null,
        size: size === DEFAULT_CUE_SIZE ? null : size,
        vertical: vertical === DEFAULT_CUE_VERTICAL ? null : vertical,
      };
    };

    return {
      trackElementsOf: (video) => {
        return elementsOf(querySelectorAll(video, "track"), MAX_TRACKS_PER_VIDEO).filter((item): item is object => typeof item === "object" && item !== null);
      },
      trackElementFacts: (trackElement) => ({
        src: stringOr(trackSrc(trackElement), ""),
        srclang: stringOr(trackSrclang(trackElement), ""),
        label: stringOr(trackLabel(trackElement), ""),
      }),
      textTrackOfElement: (trackElement) => {
        const track = trackTextTrack(trackElement);
        return typeof track === "object" && track !== null ? track : null;
      },
      textTrackListOf: (video) => {
        const list = mediaTextTracks(video);
        if (typeof list !== "object" || list === null) throw new TypeError("no textTracks");
        return list;
      },
      textTracksIn: (list) =>
        elementsOf(list, MAX_TRACKS_PER_VIDEO).filter((item): item is object => typeof item === "object" && item !== null),
      modeOf: (track) => {
        const value = mode(track);
        return value === "showing" || value === "hidden" ? value : "disabled";
      },
      kindOf: (track) => stringOr(kind(track), ""),
      languageOf: (track) => stringOr(language(track), ""),
      labelOf: (track) => stringOr(label(track), ""),
      cueCountOf: (track) => {
        const list = cues(track);
        return typeof list === "object" && list !== null ? lengthOfList(list) : null;
      },
      cuesOf: (track, limit) => {
        const list = cues(track);
        if (typeof list !== "object" || list === null || cueText === undefined) return null;
        const snapshots: CueSnapshot[] = [];
        for (const cue of elementsOf(list, limit)) {
          if (typeof cue !== "object" || cue === null) continue;
          const text = cueText(cue);
          if (typeof text !== "string") continue;
          snapshots.push({
            id: stringOr(cueId(cue), ""),
            startSeconds: numberOr(cueStart(cue), Number.NaN),
            endSeconds: numberOr(cueEnd(cue), Number.NaN),
            text,
            settings: settingsOf(cue),
          });
        }
        return snapshots;
      },
    };
  } catch {
    return null;
  }
}
