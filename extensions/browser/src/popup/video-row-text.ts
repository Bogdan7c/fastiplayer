// Чистая строка списка найденных видео: заголовок, размер, длительность, метки. DOM тут не трогаем.

// Только тип: код фона в бандл попапа не попадает.
import type { TabVideoRecord } from "../background/tab-model/tab-snapshot.ts";
import type { VideoRef } from "../shared/handoff-contract.ts";

/** Одна строка списка. */
export interface VideoRowView {
  readonly ref: VideoRef;
  /** Заголовок вкладки (главный документ) или имя хоста фрейма. */
  readonly title: string;
  /** «1920×1080 · 1:23:45»; пусто, если ни размер, ни длительность неизвестны. */
  readonly details: string;
  /** Метки: `ad?`, `DRM`, `not supported yet`, `in player`. */
  readonly labels: readonly string[];
  /** Строку можно выбрать. DRM, неподдержанная форма и видео, которое уже в плеере, — нельзя. */
  readonly selectable: boolean;
}

/** Заголовок вкладки пуст (служебная вкладка, ещё не загружена). */
const UNTITLED_PAGE = "This page";
/** У фрейма нет имени хоста (about:blank, srcdoc). */
const UNNAMED_FRAME = "Embedded player";

/** Размер кадра «1920×1080»; `null`, если неизвестен (0×0: кадр ещё не показан). */
export function formatFrameSize(widthPx: number, heightPx: number): string | null {
  return widthPx > 0 && heightPx > 0 ? `${widthPx}×${heightPx}` : null;
}

/** Длительность «1:23:45» (с часами от часа) или «3:07»; `null` — неизвестна или бесконечна. */
export function formatDuration(seconds: number | null): string | null {
  if (seconds === null || !Number.isFinite(seconds) || seconds < 0) return null;
  const total = Math.floor(seconds);
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const rest = String(total % 60).padStart(2, "0");
  return hours > 0 ? `${hours}:${String(minutes).padStart(2, "0")}:${rest}` : `${minutes}:${rest}`;
}

/** Та же ли это ссылка (три поля равны). */
function sameVideo(left: VideoRef, right: VideoRef): boolean {
  return left.frame_id === right.frame_id && left.document === right.document && left.element === right.element;
}

function labelsOf(record: TabVideoRecord, inPlayer: boolean): string[] {
  const labels: string[] = [];
  if (record.verdict === "ad") labels.push("ad?");
  if (record.verdict === "protected") labels.push("DRM");
  if (record.verdict === "unsupported") labels.push("not supported yet");
  if (inPlayer) labels.push("in player");
  return labels;
}

/**
 * Строка по записи модели. `inPlayerVideo` — элемент, на котором стоит сеанс (`null` — сеанса нет или он
 * между видео). Строка с вердиктом `empty` сюда не попадает: её скрывает `describeVideoList`.
 */
export function describeVideoRow(record: TabVideoRecord, tabTitle: string, inPlayerVideo: VideoRef | null): VideoRowView {
  const ref: VideoRef = { frame_id: record.frame_id, document: record.document, element: record.element };
  const inPlayer = inPlayerVideo !== null && sameVideo(ref, inPlayerVideo);
  const trimmedTitle = tabTitle.trim();
  const title = record.frame_id === 0 ? (trimmedTitle === "" ? UNTITLED_PAGE : trimmedTitle) : record.frame_host === "" ? UNNAMED_FRAME : record.frame_host;
  const details = [formatFrameSize(record.video_width_px, record.video_height_px), formatDuration(record.duration_s)]
    .filter((part): part is string => part !== null)
    .join(" · ");
  const selectable = !inPlayer && record.verdict !== "protected" && record.verdict !== "unsupported";
  return { ref, title, details, labels: labelsOf(record, inPlayer), selectable };
}

/**
 * Список для попапа: порядок как у `videosFoundIn` (главное первым), вердикт `empty` скрыт, `junk` показан
 * выбираемым (человек вправе отдать и «мусор»: выбор главнее вердикта).
 */
export function describeVideoList(records: readonly TabVideoRecord[], tabTitle: string, inPlayerVideo: VideoRef | null): VideoRowView[] {
  return records.filter((record) => record.verdict !== "empty").map((record) => describeVideoRow(record, tabTitle, inPlayerVideo));
}
