// Опрос всех фреймов вкладки «какие у тебя видео» (до передачи).
//
// Фон внедряет в изолированный мир каждого фрейма маленькую функцию (`scripting.executeScript`,
// allFrames), она зовёт функцию, выставленную нашим изолированным скриптом в его глобале
// (src/shared/video-candidates-probe.ts), и возвращает её промис — браузер дожидается его сам.
// Фрейм без нашего скрипта (не успел встать, служебная страница) отвечает `null`: видео в нём нет.

import { parseVideoCandidates } from "../../shared/page-message-validation.ts";
import { VIDEO_CANDIDATES_FUNCTION_NAME } from "../../shared/video-candidates-probe.ts";
import type { FrameCandidates } from "./video-choice.ts";

/** Результат внедрения в один фрейм (InjectionResult). */
export interface FrameInjectionResult {
  readonly frameId: number;
  readonly result?: unknown;
}

/** `scripting.executeScript({target: {tabId, allFrames: true}, func, args})`. */
export type ExecuteInAllFrames = (
  tabId: number,
  func: (functionName: string) => unknown,
  args: [string],
) => Promise<readonly FrameInjectionResult[]>;

/**
 * Внедряемая функция. Браузер сериализует её текст, поэтому она самодостаточна: никаких
 * замыканий и импортов, имя функции приходит аргументом.
 */
export function describeFrameVideos(functionName: string): unknown {
  const describe: unknown = (globalThis as Record<string, unknown>)[functionName];
  return typeof describe === "function" ? describe() : null;
}

/** Кандидаты всех фреймов вкладки; ответ фрейма не договорной формы считается «видео нет». */
export async function surveyTabVideos(tabId: number, execute: ExecuteInAllFrames): Promise<FrameCandidates[]> {
  const results = await execute(tabId, describeFrameVideos, [VIDEO_CANDIDATES_FUNCTION_NAME]);
  const frames: FrameCandidates[] = [];
  for (const injection of results) {
    const candidates = parseVideoCandidates(injection.result);
    if (candidates !== undefined && candidates.length > 0) frames.push({ frameId: injection.frameId, candidates });
  }
  return frames;
}
