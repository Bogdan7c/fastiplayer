// Опрос всех фреймов вкладки «какие у тебя видео» (до передачи).
//
// Фон внедряет в изолированный мир каждого фрейма маленькую функцию (`scripting.executeScript`,
// allFrames), она зовёт функцию, выставленную нашим изолированным скриптом в его глобале
// (src/shared/video-candidates-probe.ts), и возвращает её промис — браузер дожидается его сам.
// Фрейм без нашего скрипта (не успел встать, служебная страница) отвечает `null`: видео в нём нет.

import { parseVideoCandidates } from "../../shared/page-message-validation.ts";
import { VIDEO_CANDIDATES_FUNCTION_NAME } from "../../shared/video-candidates-probe.ts";
import type { Scheduler } from "../host-link/native-port.ts";
import type { FrameCandidates } from "./video-choice.ts";

/**
 * Сколько ждём ответа всех фреймов. Каждый фрейм отвечает за миллисекунды; зависший фрейм
 * (бесконечный цикл на странице, сломанный процесс) иначе подвесил бы передачу навсегда, потому что
 * `executeScript` ждёт ВСЕ фреймы. 5 с — как ответ MAIN-мира на `begin_handoff`.
 */
export const SURVEY_TIMEOUT_MS = 5_000;

/**
 * Сколько кандидатов всех фреймов вместе принимаем. Один документ ограничен 256 (валидация), но
 * враждебная страница может завести много iframe; вкладке хватает с запасом (настоящие страницы
 * держат единицы-десятки видео), а лишнее отбрасывается по правилу «играющие и крупные выше».
 */
export const MAX_SURVEY_CANDIDATES_TOTAL = 512;

/**
 * Результат внедрения в один фрейм (InjectionResult). Firefox не отклоняет внедрение в свои
 * страницы (`about:preferences`), а кладёт в массив `undefined` вместо такого фрейма: живая
 * проверка 10b. Поэтому элемент ответа бывает пустым.
 */
export interface FrameInjectionResult {
  readonly frameId: number;
  readonly result?: unknown;
}

/** `scripting.executeScript({target: {tabId, allFrames: true}, func, args})`. */
export type ExecuteInAllFrames = (
  tabId: number,
  func: (functionName: string) => unknown,
  args: [string],
) => Promise<readonly (FrameInjectionResult | undefined)[]>;

/**
 * Внедряемая функция. Браузер сериализует её текст, поэтому она самодостаточна: никаких
 * замыканий и импортов, имя функции приходит аргументом.
 */
export function describeFrameVideos(functionName: string): unknown {
  const describe: unknown = (globalThis as Record<string, unknown>)[functionName];
  return typeof describe === "function" ? describe() : null;
}

/** Итог опроса вкладки. */
export type TabSurvey =
  | {
      readonly kind: "surveyed";
      readonly frames: FrameCandidates[];
      /** Кандидаты сверх `MAX_SURVEY_CANDIDATES_TOTAL` отброшены. */
      readonly truncated: boolean;
    }
  /** Внедрение запрещено (служебная страница, магазин расширений, закрытая вкладка). */
  | { readonly kind: "not_scriptable"; readonly error: string }
  /** Фреймы не ответили за `SURVEY_TIMEOUT_MS`. */
  | { readonly kind: "timed_out" };

/** Ответы фреймов, прошедшие проверку формы (кривой ответ — «видео нет»). */
function parseFrames(results: readonly (FrameInjectionResult | undefined)[]): FrameCandidates[] {
  const frames: FrameCandidates[] = [];
  for (const injection of results) {
    // Фрейм, куда внедриться нельзя: видео в нём нам недоступно, остальные фреймы вкладки это не портит.
    if (injection === undefined) continue;
    const candidates = parseVideoCandidates(injection.result);
    if (candidates !== undefined && candidates.length > 0) frames.push({ frameId: injection.frameId, candidates });
  }
  return frames;
}

/** Браузер ответил, но ни один фрейм внедрение не принял: вкладка закрыта для экстеншена. */
function noFrameAccepted(results: readonly (FrameInjectionResult | undefined)[]): boolean {
  return results.length > 0 && results.every((injection) => injection === undefined);
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
 * Кандидаты всех фреймов вкладки. Ограничен по времени (`SURVEY_TIMEOUT_MS`) и по числу кандидатов
 * (`MAX_SURVEY_CANDIDATES_TOTAL`): зависший фрейм или заваленный ответ не подвешивает передачу.
 */
export async function surveyTabVideos(tabId: number, execute: ExecuteInAllFrames, scheduler: Scheduler): Promise<TabSurvey> {
  const injection = execute(tabId, describeFrameVideos, [VIDEO_CANDIDATES_FUNCTION_NAME]).then(
    (results): TabSurvey => {
      if (noFrameAccepted(results)) return { kind: "not_scriptable", error: "no frame accepted the injection" };
      const capped = capCandidates(parseFrames(results), MAX_SURVEY_CANDIDATES_TOTAL);
      return { kind: "surveyed", frames: capped.frames, truncated: capped.truncated };
    },
    (error: unknown): TabSurvey => ({ kind: "not_scriptable", error: String(error) }),
  );
  let timer: unknown;
  const timeout = new Promise<TabSurvey>((resolve) => {
    timer = scheduler.setTimeout(() => resolve({ kind: "timed_out" }), SURVEY_TIMEOUT_MS);
  });
  try {
    // Запоздавший ответ после таймаута игнорируется: `injection` уже не ждут, отказ в нём обработан выше.
    return await Promise.race([injection, timeout]);
  } finally {
    scheduler.clearTimeout(timer);
  }
}
