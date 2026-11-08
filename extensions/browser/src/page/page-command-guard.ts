// Проверка формы входящих сообщений участка 1 (изолированный мир → MAIN-мир).
//
// Порт личный, но MAIN-мир всё равно не доверяет форме: страница теоретически могла подсунуть свой
// порт раньше изолированного мира (мы принимаем только первый), а сбой где угодно выше не должен
// превращаться в исключение на странице. Мусор (лишние поля, не те типы, числа вне диапазона)
// молча отбрасывается: `null`.

import type {
  DescribeCandidatesQuery,
  HandoffEndPosition,
  HandoffEndResume,
  NextVideoCommand,
  PageCommand,
  ShadowCommand,
} from "../shared/handoff-contract.ts";
import { MAX_JSON_SAFE_INTEGER, MAX_PLAYBACK_RATE_PERMILLE, MIN_PLAYBACK_RATE_PERMILLE } from "../protocol/limits.ts";
import type { ClockState, ResumeState } from "../protocol/session/player.ts";
import type { LiveSeekable } from "../protocol/session/live-window.ts";
import { isDocumentTag, MAX_NEXT_VIDEO_LEASE_MS } from "../shared/page-message-validation.ts";
import type { PointerQuery } from "../shared/entry-contract.ts";
import { parsePointerQuery } from "../shared/entry-validation.ts";
import { parseWatchHandedOffVideo, type WatchHandedOffVideoMessage } from "../shared/handed-off-video-contract.ts";

/** Что MAIN-мир принимает по порту. */
export type IncomingPageMessage = DescribeCandidatesQuery | PageCommand | NextVideoCommand;

/** Наибольший u32 (номера запросов и перемоток в протоколе — u32). */
const MAX_U32 = 0xffff_ffff;

const CLOCK_STATES: readonly ClockState[] = ["playing", "paused", "waiting", "ended"];
const RESUME_STATES: readonly ResumeState[] = ["playing", "paused"];

type Fields = Readonly<Record<string, unknown>>;

/** Простой объект ровно с этими собственными полями (ни больше, ни меньше). */
function exactObject(candidate: unknown, fieldNames: readonly string[]): Fields | null {
  if (typeof candidate !== "object" || candidate === null || Array.isArray(candidate)) return null;
  const ownNames = Object.keys(candidate);
  if (ownNames.length !== fieldNames.length) return null;
  if (!fieldNames.every((name) => Object.hasOwn(candidate, name))) return null;
  return candidate as Fields;
}

/** Метка `kind` объекта (`null` — нет или не строка). */
function kindOf(candidate: unknown): string | null {
  if (typeof candidate !== "object" || candidate === null || !Object.hasOwn(candidate, "kind")) return null;
  const kind: unknown = (candidate as Fields)["kind"];
  return typeof kind === "string" ? kind : null;
}

function isIntegerInRange(value: unknown, min: number, max: number): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= min && value <= max;
}

const isU32 = (value: unknown): value is number => isIntegerInRange(value, 0, MAX_U32);
const isMediaTime = (value: unknown): value is number => isIntegerInRange(value, 0, MAX_JSON_SAFE_INTEGER);
const isPlaybackRate = (value: unknown): value is number => isIntegerInRange(value, MIN_PLAYBACK_RATE_PERMILLE, MAX_PLAYBACK_RATE_PERMILLE);

function isOneOf<T extends string>(value: unknown, allowed: readonly T[]): value is T {
  return typeof value === "string" && (allowed as readonly string[]).includes(value);
}

function parseShadowCommand(candidate: unknown): ShadowCommand | null {
  switch (kindOf(candidate)) {
    case "clock": {
      const fields = exactObject(candidate, ["kind", "position_us", "state", "rate"]);
      if (fields === null) return null;
      const { position_us: position, state, rate } = fields;
      if (!isMediaTime(position) || !isOneOf(state, CLOCK_STATES) || !isPlaybackRate(rate)) return null;
      return { kind: "clock", position_us: position, state, rate };
    }
    case "play":
      return exactObject(candidate, ["kind"]) === null ? null : { kind: "play" };
    case "pause":
      return exactObject(candidate, ["kind"]) === null ? null : { kind: "pause" };
    case "set_rate": {
      const fields = exactObject(candidate, ["kind", "rate"]);
      if (fields === null || !isPlaybackRate(fields["rate"])) return null;
      return { kind: "set_rate", rate: fields["rate"] };
    }
    case "seek": {
      const fields = exactObject(candidate, ["kind", "seek_id", "target_us"]);
      if (fields === null) return null;
      const { seek_id: seekId, target_us: target } = fields;
      if (!isU32(seekId) || !isMediaTime(target)) return null;
      return { kind: "seek", seek_id: seekId, target_us: target };
    }
    case "live_window": {
      const fields = exactObject(candidate, ["kind", "edge_us", "seekable"]);
      if (fields === null || !isMediaTime(fields["edge_us"])) return null;
      const seekable = parseLiveSeekable(fields["seekable"], fields["edge_us"]);
      return seekable === null ? null : { kind: "live_window", edge_us: fields["edge_us"], seekable };
    }
    default:
      return null;
  }
}

/** Окно эфира (протокол 1.2): договор `start_us <= end_us <= edge_us`, как при разборе протокола. */
function parseLiveSeekable(candidate: unknown, edgeUs: number): LiveSeekable | null {
  switch (kindOf(candidate)) {
    case "none":
      return exactObject(candidate, ["kind"]) === null ? null : { kind: "none" };
    case "range": {
      const fields = exactObject(candidate, ["kind", "start_us", "end_us"]);
      if (fields === null) return null;
      const { start_us: start, end_us: end } = fields;
      if (!isMediaTime(start) || !isMediaTime(end) || start > end || end > edgeUs) return null;
      return { kind: "range", start_us: start, end_us: end };
    }
    default:
      return null;
  }
}

function parseEndPosition(candidate: unknown): HandoffEndPosition | null {
  switch (kindOf(candidate)) {
    case "at": {
      const fields = exactObject(candidate, ["kind", "position_us"]);
      if (fields === null || !isMediaTime(fields["position_us"])) return null;
      return { kind: "at", position_us: fields["position_us"] };
    }
    case "shadow_position":
      return exactObject(candidate, ["kind"]) === null ? null : { kind: "shadow_position" };
    default:
      return null;
  }
}

function parseEndResume(candidate: unknown): HandoffEndResume | null {
  switch (kindOf(candidate)) {
    case "exactly": {
      const fields = exactObject(candidate, ["kind", "resume"]);
      if (fields === null || !isOneOf(fields["resume"], RESUME_STATES)) return null;
      return { kind: "exactly", resume: fields["resume"] };
    }
    case "as_shadow":
      return exactObject(candidate, ["kind"]) === null ? null : { kind: "as_shadow" };
    default:
      return null;
  }
}

/** Разобрать сообщение порта. `null` — не по форме (игнорируется). */
export function parseIncomingPageMessage(candidate: unknown): IncomingPageMessage | null {
  switch (kindOf(candidate)) {
    case "describe_candidates": {
      const fields = exactObject(candidate, ["kind", "request_id"]);
      if (fields === null || !isU32(fields["request_id"])) return null;
      return { kind: "describe_candidates", request_id: fields["request_id"] };
    }
    case "begin_handoff": {
      const fields = exactObject(candidate, ["kind", "request_id", "document", "element"]);
      if (fields === null) return null;
      const { request_id: requestId, document, element } = fields;
      if (!isU32(requestId) || !isDocumentTag(document) || !isIntegerInRange(element, 1, MAX_JSON_SAFE_INTEGER)) return null;
      return { kind: "begin_handoff", request_id: requestId, document, element };
    }
    case "shadow": {
      const fields = exactObject(candidate, ["kind", "command"]);
      const command = fields === null ? null : parseShadowCommand(fields["command"]);
      return command === null ? null : { kind: "shadow", command };
    }
    case "end_handoff": {
      const fields = exactObject(candidate, ["kind", "position", "resume"]);
      if (fields === null) return null;
      const position = parseEndPosition(fields["position"]);
      const resume = parseEndResume(fields["resume"]);
      if (position === null || resume === null) return null;
      return { kind: "end_handoff", position, resume };
    }
    case "arm_next_video": {
      const fields = exactObject(candidate, ["kind", "lease_ms"]);
      if (fields === null || !isIntegerInRange(fields["lease_ms"], 1, MAX_NEXT_VIDEO_LEASE_MS)) return null;
      return { kind: "arm_next_video", lease_ms: fields["lease_ms"] };
    }
    case "release_held_video": {
      const fields = exactObject(candidate, ["kind", "element"]);
      if (fields === null || !isIntegerInRange(fields["element"], 1, MAX_JSON_SAFE_INTEGER)) return null;
      return { kind: "release_held_video", element: fields["element"] };
    }
    default:
      return null;
  }
}

/**
 * Всё, что MAIN-мир принимает по порту: команды контроллера передачи, вопросы про указатель (10c) и имя
 * события якоря переданного видео (11a). Последние два — местные дела миров, фону они не уходят.
 */
export type ChannelMessage = IncomingPageMessage | PointerQuery | WatchHandedOffVideoMessage;

/**
 * Разобрать сообщение порта целиком. Вопросы про указатель (`describe_right_clicked`, `video_at_point`)
 * идут мимо контроллера передачи: на них отвечает `PointerVideo`; имя якоря (`watch_handed_off_video`)
 * берёт якорь переданного видео. `null` — не по форме (игнорируется).
 */
export function parseChannelMessage(candidate: unknown): ChannelMessage | null {
  return parsePointerQuery(candidate) ?? parseWatchHandedOffVideo(candidate) ?? parseIncomingPageMessage(candidate);
}

export function isPointerQuery(message: ChannelMessage): message is PointerQuery {
  return message.kind === "describe_right_clicked" || message.kind === "video_at_point";
}
