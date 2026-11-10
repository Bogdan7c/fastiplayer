// Опрос всех фреймов вкладки «какие у тебя видео» (до передачи).
//
// Фон внедряет в изолированный мир каждого фрейма маленькую функцию, она зовёт функцию, выставленную
// нашим изолированным скриптом в его глобале (src/shared/video-candidates-probe.ts), и возвращает её
// промис — браузер дожидается его сам. Фрейм без нашего скрипта отвечает об этом явно: верхний фрейм
// такого рода значит «вкладка старше скрипта».
//
// С сессии 15c фреймы спрашиваются по одному, без ожидания их загрузки и с общим сроком
// (src/background/frame-injection/): один ленивый или замёрзший фрейм больше не держит ответ всей вкладки.
//
// Почему судит только верхний фрейм (сессия 10d). Документ, загруженный после установки, всегда получает
// скрипт раньше своих подфреймов, поэтому подфрейм без скрипта при живом верхнем — лишь ещё грузится.
// А верхний фрейм без скрипта значит, что вкладка открыта до установки или обновления.

import type { ReloadReason } from "../../shared/handoff-contract.ts";
import { parseVideoCandidates } from "../../shared/page-message-validation.ts";
import { VIDEO_CANDIDATES_FUNCTION_NAME, type ScriptlessFrameAnswer } from "../../shared/video-candidates-probe.ts";
import type { VideoCandidate } from "../../shared/handoff-contract.ts";
import type { Scheduler } from "../host-link/native-port.ts";
import { askEachFrame, TOP_FRAME_ID, type FrameInjectionPort, type FrameReply } from "../frame-injection/frame-injection.ts";
import type { FrameCandidates } from "./video-choice.ts";

/**
 * Сколько ждём ответа фреймов. Каждый фрейм отвечает за миллисекунды; замёрзший фрейм (бесконечный
 * цикл на странице, сломанный процесс) иначе подвесил бы передачу навсегда. Фреймы спрашиваются
 * параллельно, срок общий. 5 с — как ответ MAIN-мира на `begin_handoff`.
 */
export const SURVEY_TIMEOUT_MS = 5_000;

/**
 * Сколько кандидатов всех фреймов вместе принимаем. Один документ ограничен 256 (валидация), но
 * враждебная страница может завести много iframe; вкладке хватает с запасом (настоящие страницы
 * держат единицы-десятки видео), а лишнее отбрасывается по правилу «играющие и крупные выше».
 */
export const MAX_SURVEY_CANDIDATES_TOTAL = 512;

/**
 * Внедряемая функция. Браузер сериализует её текст, поэтому она самодостаточна: никаких
 * замыканий и импортов, имя функции приходит аргументом.
 */
export function describeFrameVideos(functionName: string): unknown {
  const describe: unknown = (globalThis as Record<string, unknown>)[functionName];
  if (typeof describe === "function") return describe();
  const { protocol } = globalThis.location;
  // Документ ещё разбирается: внедрение «без ожидания загрузки» могло прийти раньше нашего скрипта
  // document_start. Это не «вкладка старше скрипта», а «страница ещё грузится».
  const documentLoading = globalThis.document?.readyState === "loading";
  const scriptless: ScriptlessFrameAnswer = { script_absent: true, on_web_page: protocol === "http:" || protocol === "https:", document_loading: documentLoading };
  return scriptless;
}

/** Итог опроса вкладки. */
export type TabSurvey =
  | {
      readonly kind: "surveyed";
      readonly frames: FrameCandidates[];
      /** Кандидаты сверх `MAX_SURVEY_CANDIDATES_TOTAL` или фреймы сверх `MAX_FRAMES_PER_TAB` отброшены. */
      readonly truncated: boolean;
      /** Верхний фрейм говорит, что вкладка старше скрипта; `null` — скрипт на месте и нашей версии. */
      readonly scriptGap: ReloadReason | null;
    }
  /** Внедрение запрещено (служебная страница, магазин расширений, закрытая вкладка). */
  | { readonly kind: "not_scriptable"; readonly error: string }
  /** Верхний фрейм (а с ним и страница) не ответил за `SURVEY_TIMEOUT_MS`. */
  | { readonly kind: "timed_out" };

/** Что значит ответ одного фрейма. */
interface FrameReading {
  readonly candidates: readonly VideoCandidate[];
  readonly scriptGap: ReloadReason | null;
}

const NOTHING_READ: FrameReading = { candidates: [], scriptGap: null };

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** Разобрать ответ фрейма. Ответ шёл через страницу вкладки, поэтому кривой значит просто «видео нет». */
function readFrameAnswer(answer: unknown, scriptVersion: string): FrameReading {
  if (!isRecord(answer)) return NOTHING_READ;
  if (answer["script_absent"] === true) {
    // Пустой документ (`about:blank`) и ещё разбираемая страница — не разрыв: перезагрузка тут не поможет.
    const settledWebPage = answer["on_web_page"] === true && answer["document_loading"] !== true;
    return settledWebPage ? { candidates: [], scriptGap: "script_missing" } : NOTHING_READ;
  }
  // Со скриптом другой версии не разговариваем: сверяем раньше, чем верим его списку.
  if (answer["script_version"] !== scriptVersion) return { candidates: [], scriptGap: "script_outdated" };
  return { candidates: parseVideoCandidates(answer["candidates"]) ?? [], scriptGap: null };
}

/** Ответы фреймов, прошедшие проверку формы, и что верхний фрейм сказал о скрипте. */
function parseFrames(replies: readonly FrameReply[], scriptVersion: string): { frames: FrameCandidates[]; scriptGap: ReloadReason | null } {
  const frames: FrameCandidates[] = [];
  let scriptGap: ReloadReason | null = null;
  for (const reply of replies) {
    // Фрейм отказал или промолчал: видео в нём нам недоступно, остальные фреймы вкладки это не портит.
    if (reply.kind !== "answered") continue;
    const reading = readFrameAnswer(reply.result, scriptVersion);
    if (reply.frameId === TOP_FRAME_ID) scriptGap = reading.scriptGap;
    if (reading.candidates.length > 0) frames.push({ frameId: reply.frameId, candidates: reading.candidates });
  }
  return { frames, scriptGap };
}

/** Браузер дал фреймы, но ни один внедрение не принял: вкладка закрыта для экстеншена. */
function noFrameAccepted(replies: readonly FrameReply[]): boolean {
  return replies.length > 0 && replies.every((reply) => reply.kind === "refused");
}

/**
 * Страница не ответила: молчит верхний фрейм (только он судит о скрипте вкладки), или не ответил
 * никто. Ответы подфреймов без верхнего не дают права сказать «видео нет» или «скрипт на месте».
 */
function pageSilent(replies: readonly FrameReply[]): boolean {
  const top = replies.find((reply) => reply.frameId === TOP_FRAME_ID);
  if (top?.kind === "silent") return true;
  return replies.some((reply) => reply.kind === "silent") && !replies.some((reply) => reply.kind === "answered");
}

/**
 * Оставляет не больше `limit` кандидатов: при переполнении выигрывают играющие, затем с большей
 * видимой площадью. Порядок внутри фреймов и порядок фреймов сохраняются (выбор детерминирован).
 */
export function capCandidates(frames: readonly FrameCandidates[], limit: number): { frames: FrameCandidates[]; truncated: boolean } {
  const total = frames.reduce((sum, frame) => sum + frame.candidates.length, 0);
  if (total <= limit) return { frames: [...frames], truncated: false };
  const all = frames.flatMap((frame, frameIndex) => frame.candidates.map((candidate, candidateIndex) => ({ frameIndex, candidateIndex, candidate })));
  const winners = [...all]
    .sort((left, right) => {
      if (left.candidate.playing !== right.candidate.playing) return left.candidate.playing ? -1 : 1;
      return right.candidate.visible_area_px - left.candidate.visible_area_px;
    })
    .slice(0, limit);
  const kept = new Set(winners.map((entry) => entry.candidate));
  const capped = frames
    .map((frame) => ({ frameId: frame.frameId, candidates: frame.candidates.filter((candidate) => kept.has(candidate)) }))
    .filter((frame) => frame.candidates.length > 0);
  return { frames: capped, truncated: true };
}

/**
 * Кандидаты всех фреймов вкладки. Ограничен по времени (`SURVEY_TIMEOUT_MS`), по числу фреймов
 * (`MAX_FRAMES_PER_TAB`) и по числу кандидатов (`MAX_SURVEY_CANDIDATES_TOTAL`): ленивый, замёрзший фрейм
 * или заваленный ответ не подвешивает передачу. Молчащий подфрейм просто не даёт видео.
 */
export async function surveyTabVideos(tabId: number, frames: FrameInjectionPort, scheduler: Scheduler, scriptVersion: string): Promise<TabSurvey> {
  const injection = { func: describeFrameVideos, args: [VIDEO_CANDIDATES_FUNCTION_NAME] as [string] };
  const outcome = await askEachFrame(tabId, frames, scheduler, injection, SURVEY_TIMEOUT_MS);
  if (outcome.kind === "frames_unknown") return { kind: "not_scriptable", error: outcome.error };
  if (noFrameAccepted(outcome.replies)) return { kind: "not_scriptable", error: "no frame accepted the injection" };
  if (pageSilent(outcome.replies)) return { kind: "timed_out" };
  const parsed = parseFrames(outcome.replies, scriptVersion);
  const capped = capCandidates(parsed.frames, MAX_SURVEY_CANDIDATES_TOTAL);
  return { kind: "surveyed", frames: capped.frames, truncated: capped.truncated || outcome.framesTruncated, scriptGap: parsed.scriptGap };
}
