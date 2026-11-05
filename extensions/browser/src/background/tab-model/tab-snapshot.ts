// Снимок модели вкладки: форма, сборка из опроса фреймов, хвост ≤ 64 записей и разбор из хранилища.
//
// Снимок — «факты о видео вкладки на момент последнего опроса», без DOM-ссылок и без функций:
// он целиком кладётся в `storage.session` и читается попапом (сессия 10) и читателем рекламы (09d)
// только через методы модели (tab-model.ts), а не напрямую.

import type { CandidateMedia, DocumentTag, ElementKey, UnsupportedMediaForm, VideoCandidate } from "../../shared/handoff-contract.ts";
import { chooseVideo, classifySurvey, type ClassifiedVideo, type FrameCandidates } from "../handoff/video-choice.ts";
import type { VideoVerdict } from "./video-verdict.ts";

/**
 * Сколько записей о видео держим на вкладку. Страница может описать сотни элементов (один документ
 * до 256, вкладка — до предела опроса); в хранилище и в попап уходят самые вероятные.
 */
export const MAX_TAB_VIDEO_RECORDS = 64;

/** Ссылка на видео вкладки: фрейм, метка документа и ключ элемента (как в `begin_handoff`). */
export interface VideoRef {
  readonly frame_id: number;
  readonly document: DocumentTag;
  readonly element: ElementKey;
}

/** Запись о видео: все факты кандидата + фрейм + вердикт классификатора. */
export interface TabVideoRecord extends VideoCandidate {
  readonly frame_id: number;
  readonly verdict: VideoVerdict;
}

/**
 * Закрытый список причин, по которым последняя передача вкладки не состоялась. Не свободные строки:
 * попап и 09d различают их по `kind`. `not_supported_yet` несёт форму источника (подписи
 * «not supported yet» в попапе, пункт 7 сессии 09c).
 */
export type TabRefusal =
  | { readonly kind: "no_video" }
  | { readonly kind: "not_supported_yet"; readonly form: UnsupportedMediaForm }
  | { readonly kind: "protected" }
  | { readonly kind: "ad_playing" }
  | { readonly kind: "local_address_refused" }
  | { readonly kind: "local_address_needs_consent"; readonly host: string }
  | { readonly kind: "player_unavailable" }
  /** Страница залила нас событиями или байтами сверх лимитов (решение 200): сеанс закрыт. */
  | { readonly kind: "page_flood" }
  | { readonly kind: "tab_not_scriptable" }
  | { readonly kind: "survey_timed_out" }
  | { readonly kind: "failed" };

/** Все значения `TabRefusal["kind"]` — для разбора из хранилища. */
const REFUSAL_KINDS: readonly TabRefusal["kind"][] = [
  "no_video",
  "not_supported_yet",
  "protected",
  "ad_playing",
  "local_address_refused",
  "local_address_needs_consent",
  "player_unavailable",
  "page_flood",
  "tab_not_scriptable",
  "survey_timed_out",
  "failed",
];

/** Снимок модели одной вкладки. */
export interface TabSnapshot {
  readonly tab_id: number;
  /** Часы фона (мс) последнего опроса; `null` — вкладку ещё не опрашивали (есть только отказ). */
  readonly surveyed_at_ms: number | null;
  /** Не больше `MAX_TAB_VIDEO_RECORDS` самых вероятных записей. */
  readonly videos: readonly TabVideoRecord[];
  /** Сколько записей не вошло в хвост (учёт: `videos.length + videos_omitted` = найдено). */
  readonly videos_omitted: number;
  /** Главное видео по правилу выбора; `null` — передавать нечего. */
  readonly main: VideoRef | null;
  /** Причина последнего отказа передачи; `null` — отказа не было с последнего опроса. */
  readonly last_refusal: TabRefusal | null;
}

/** Порядок важности вердиктов при отсечении хвоста. */
const VERDICT_PRIORITY: readonly VideoVerdict[] = ["main", "protected", "unsupported", "ad", "junk", "empty"];

/** Ссылка на запись. */
export function refOf(record: Pick<TabVideoRecord, "frame_id" | "document" | "element">): VideoRef {
  return { frame_id: record.frame_id, document: record.document, element: record.element };
}

/** Совпадают ли ссылки. */
export function sameRef(left: VideoRef, right: VideoRef): boolean {
  return left.frame_id === right.frame_id && left.document === right.document && left.element === right.element;
}

function toRecord(entry: ClassifiedVideo): TabVideoRecord {
  return { ...entry.candidate, frame_id: entry.frameId, verdict: entry.verdict };
}

/**
 * Хвост: главное видео первым, дальше по важности вердикта, затем играющие, затем по площади.
 * Сортировка устойчива — равные остаются в порядке опроса.
 */
function rankForTail(records: readonly TabVideoRecord[], main: VideoRef | null): TabVideoRecord[] {
  const priorityOf = (record: TabVideoRecord): number => VERDICT_PRIORITY.indexOf(record.verdict);
  return [...records].sort((left, right) => {
    const leftIsMain = main !== null && sameRef(refOf(left), main);
    const rightIsMain = main !== null && sameRef(refOf(right), main);
    if (leftIsMain !== rightIsMain) return leftIsMain ? -1 : 1;
    if (priorityOf(left) !== priorityOf(right)) return priorityOf(left) - priorityOf(right);
    if (left.playing !== right.playing) return left.playing ? -1 : 1;
    return right.visible_area_px - left.visible_area_px;
  });
}

/** Снимок из свежего опроса. Отказ сбрасывается: он относился к прошлой попытке. */
export function buildSnapshot(tabId: number, frames: readonly FrameCandidates[], surveyedAtMs: number): TabSnapshot {
  const records = classifySurvey(frames).map(toRecord);
  const choice = chooseVideo(frames);
  const main: VideoRef | null =
    choice.kind === "hand_off" ? { frame_id: choice.video.frameId, document: choice.video.document, element: choice.video.element } : null;
  const kept = rankForTail(records, main).slice(0, MAX_TAB_VIDEO_RECORDS);
  return {
    tab_id: tabId,
    surveyed_at_ms: surveyedAtMs,
    videos: kept,
    videos_omitted: records.length - kept.length,
    main,
    last_refusal: null,
  };
}

/** Пустой снимок вкладки, которую ещё не опрашивали (нужен, чтобы записать отказ). */
export function emptySnapshot(tabId: number): TabSnapshot {
  return { tab_id: tabId, surveyed_at_ms: null, videos: [], videos_omitted: 0, main: null, last_refusal: null };
}

// ------------------------------------------------------------------ разбор из хранилища

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isMediaShape(value: unknown): value is CandidateMedia {
  return isRecord(value) && typeof value["kind"] === "string";
}

/** Разбор отказа из хранилища; кривой — `undefined`. */
function parseRefusal(value: unknown): TabRefusal | undefined {
  if (!isRecord(value)) return undefined;
  const kind = value["kind"];
  if (!REFUSAL_KINDS.some((known) => known === kind)) return undefined;
  if (kind === "not_supported_yet" && typeof value["form"] !== "string") return undefined;
  if (kind === "local_address_needs_consent" && typeof value["host"] !== "string") return undefined;
  return value as unknown as TabRefusal;
}

function parseRecord(value: unknown): TabVideoRecord | undefined {
  if (!isRecord(value)) return undefined;
  const verdict = value["verdict"];
  const verdictKnown = VERDICT_PRIORITY.some((known) => known === verdict);
  const shapeOk =
    typeof value["frame_id"] === "number" &&
    typeof value["document"] === "string" &&
    typeof value["element"] === "number" &&
    isMediaShape(value["media"]) &&
    typeof value["playing"] === "boolean" &&
    typeof value["visible_area_px"] === "number";
  return verdictKnown && shapeOk ? (value as unknown as TabVideoRecord) : undefined;
}

/**
 * Разбор снимка, прочитанного из `storage.session`. Хранилище наше, но мог остаться снимок старой
 * версии расширения: кривой снимок считается отсутствующим (модель — кэш, опрос его восстановит).
 */
export function parseSnapshot(value: unknown, tabId: number): TabSnapshot | undefined {
  if (!isRecord(value) || value["tab_id"] !== tabId) return undefined;
  const surveyedAt = value["surveyed_at_ms"];
  if (surveyedAt !== null && typeof surveyedAt !== "number") return undefined;
  if (!Array.isArray(value["videos"]) || typeof value["videos_omitted"] !== "number") return undefined;
  const videos: TabVideoRecord[] = [];
  for (const item of value["videos"]) {
    const record = parseRecord(item);
    if (record === undefined) return undefined;
    videos.push(record);
  }
  const mainValue = value["main"];
  let main: VideoRef | null = null;
  if (mainValue !== null) {
    if (!isRecord(mainValue) || typeof mainValue["frame_id"] !== "number" || typeof mainValue["document"] !== "string" || typeof mainValue["element"] !== "number") {
      return undefined;
    }
    main = { frame_id: mainValue["frame_id"], document: mainValue["document"], element: mainValue["element"] };
  }
  const refusalValue = value["last_refusal"];
  const refusal = refusalValue === null ? null : parseRefusal(refusalValue);
  if (refusal === undefined) return undefined;
  return { tab_id: tabId, surveyed_at_ms: surveyedAt, videos, videos_omitted: value["videos_omitted"], main, last_refusal: refusal };
}
