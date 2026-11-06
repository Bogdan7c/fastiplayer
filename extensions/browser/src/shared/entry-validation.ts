// Проверка формы сообщений входов (сессия 10c). Всё, что пришло из MAIN-мира или от контент-скрипта, —
// недоверенный ввод: страница может подделать что угодно в своём мире. Мусор -> `undefined`.

import { MAX_JSON_SAFE_INTEGER } from "../protocol/limits.ts";
import { isDocumentTag } from "./page-message-validation.ts";
import {
  OVERLAY_BUTTON_PRESSED_KIND,
  SHOW_ENTRY_RESULT_KIND,
  type EntryReport,
  type OverlayButtonPressedMessage,
  type PageUiRect,
  type PointerQuery,
  type RightClickedReply,
  type RightClickedVideo,
  type ShowEntryResultMessage,
  type VideoAtPoint,
  type VideoAtPointReply,
} from "./entry-contract.ts";

/** Наибольший номер запроса (u32). */
const MAX_U32 = 0xffff_ffff;

/** Предел размера одной стороны прямоугольника и координат: экраны меньше, число — против переполнения. */
const MAX_COORDINATE_PX = 1_000_000;

/** Пределы текста плашки: тексты наши, но канал публичный для страницы-подделки не должен быть бездонным. */
const MAX_REPORT_ID_LENGTH = 64;
const MAX_REPORT_LINE_LENGTH = 300;
const MAX_REPORT_DETAILS = 8;

type PlainRecord = Readonly<Record<string, unknown>>;

function isPlainRecord(value: unknown): value is PlainRecord {
  return typeof value === "object" && value !== null && !Array.isArray(value) && !ArrayBuffer.isView(value);
}

function hasExactKeys(record: PlainRecord, keys: readonly string[]): boolean {
  const ownKeys = Object.keys(record);
  return ownKeys.length === keys.length && keys.every((key) => Object.hasOwn(record, key));
}

const isRequestId = (value: unknown): value is number => typeof value === "number" && Number.isInteger(value) && value >= 0 && value <= MAX_U32;
const isElementKey = (value: unknown): value is number => typeof value === "number" && Number.isInteger(value) && value >= 1 && value <= MAX_JSON_SAFE_INTEGER;
const isCoordinate = (value: unknown): value is number => typeof value === "number" && Number.isFinite(value) && Math.abs(value) <= MAX_COORDINATE_PX;
const isSize = (value: unknown): value is number => typeof value === "number" && Number.isFinite(value) && value >= 0 && value <= MAX_COORDINATE_PX;

/** Видео без привязки к фрейму: метка документа и ключ элемента. */
export function parseRightClickedVideo(value: unknown): RightClickedVideo | undefined {
  if (!isPlainRecord(value) || !hasExactKeys(value, ["document", "element"])) return undefined;
  const { document, element } = value;
  return isDocumentTag(document) && isElementKey(element) ? { document, element } : undefined;
}

function parseRect(value: unknown): PageUiRect | undefined {
  if (!isPlainRecord(value) || !hasExactKeys(value, ["left", "top", "width", "height"])) return undefined;
  const { left, top, width, height } = value;
  return isCoordinate(left) && isCoordinate(top) && isSize(width) && isSize(height) ? { left, top, width, height } : undefined;
}

function parseVideoAtPoint(value: unknown): VideoAtPoint | undefined {
  if (!isPlainRecord(value) || !hasExactKeys(value, ["document", "element", "rect", "in_player"])) return undefined;
  const { document, element, rect: rawRect, in_player: inPlayer } = value;
  const rect = parseRect(rawRect);
  if (!isDocumentTag(document) || !isElementKey(element) || rect === undefined || typeof inPlayer !== "boolean") return undefined;
  return { document, element, rect, in_player: inPlayer };
}

/** Ответ MAIN-мира `right_clicked`. */
export function parseRightClickedReply(value: unknown): RightClickedReply | undefined {
  if (!isPlainRecord(value) || value["kind"] !== "right_clicked" || !hasExactKeys(value, ["kind", "request_id", "video"])) return undefined;
  const requestId = value["request_id"];
  if (!isRequestId(requestId)) return undefined;
  if (value["video"] === null) return { kind: "right_clicked", request_id: requestId, video: null };
  const video = parseRightClickedVideo(value["video"]);
  return video === undefined ? undefined : { kind: "right_clicked", request_id: requestId, video };
}

/** Ответ MAIN-мира `video_at_point_result`. */
export function parseVideoAtPointReply(value: unknown): VideoAtPointReply | undefined {
  if (!isPlainRecord(value) || value["kind"] !== "video_at_point_result" || !hasExactKeys(value, ["kind", "request_id", "video"])) return undefined;
  const requestId = value["request_id"];
  if (!isRequestId(requestId)) return undefined;
  if (value["video"] === null) return { kind: "video_at_point_result", request_id: requestId, video: null };
  const video = parseVideoAtPoint(value["video"]);
  return video === undefined ? undefined : { kind: "video_at_point_result", request_id: requestId, video };
}

/** Запрос изолированного мира к MAIN (MAIN проверяет форму на своей стороне этим же разбором). */
export function parsePointerQuery(value: unknown): PointerQuery | undefined {
  if (!isPlainRecord(value)) return undefined;
  if (value["kind"] === "describe_right_clicked") {
    const requestId = value["request_id"];
    return hasExactKeys(value, ["kind", "request_id"]) && isRequestId(requestId) ? { kind: "describe_right_clicked", request_id: requestId } : undefined;
  }
  if (value["kind"] === "video_at_point") {
    const { request_id: requestId, x, y } = value;
    return hasExactKeys(value, ["kind", "request_id", "x", "y"]) && isRequestId(requestId) && isCoordinate(x) && isCoordinate(y)
      ? { kind: "video_at_point", request_id: requestId, x, y }
      : undefined;
  }
  return undefined;
}

/** Нажатие кнопки поверх видео, пришедшее фону от контент-скрипта. */
export function parseOverlayButtonPressed(value: unknown): OverlayButtonPressedMessage | undefined {
  if (!isPlainRecord(value) || value["kind"] !== OVERLAY_BUTTON_PRESSED_KIND || !hasExactKeys(value, ["kind", "document", "element"])) return undefined;
  const { document, element } = value;
  return isDocumentTag(document) && isElementKey(element) ? { kind: OVERLAY_BUTTON_PRESSED_KIND, document, element } : undefined;
}

function isShortLine(value: unknown, maxLength: number): value is string {
  return typeof value === "string" && value.length > 0 && value.length <= maxLength;
}

function parseReport(value: unknown): EntryReport | undefined {
  if (!isPlainRecord(value) || !hasExactKeys(value, ["id", "tone", "headline", "details"])) return undefined;
  const { id, tone, headline, details } = value;
  if (!isShortLine(id, MAX_REPORT_ID_LENGTH) || (tone !== "success" && tone !== "refusal" && tone !== "hint") || !isShortLine(headline, MAX_REPORT_LINE_LENGTH)) return undefined;
  if (!Array.isArray(details) || details.length > MAX_REPORT_DETAILS) return undefined;
  if (!details.every((line): line is string => isShortLine(line, MAX_REPORT_LINE_LENGTH))) return undefined;
  return { id, tone, headline, details: [...details] };
}

/** Просьба фона показать плашку. */
export function parseShowEntryResult(value: unknown): ShowEntryResultMessage | undefined {
  if (!isPlainRecord(value) || value["kind"] !== SHOW_ENTRY_RESULT_KIND || !hasExactKeys(value, ["kind", "report"])) return undefined;
  const report = parseReport(value["report"]);
  return report === undefined ? undefined : { kind: SHOW_ENTRY_RESULT_KIND, report };
}
