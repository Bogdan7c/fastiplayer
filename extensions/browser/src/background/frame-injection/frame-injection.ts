// Внедрение функции в каждый фрейм вкладки по отдельности (сессия 15c, решения 297 и 299).
//
// Почему не «во все фреймы разом» (`executeScript({allFrames: true})`). Такой вызов отвечает, только
// когда ответили ВСЕ фреймы. Живой klonua в Chromium: ленивый `<iframe loading="lazy">` ниже экрана
// навсегда остаётся на начальном пустом документе, а Chromium по умолчанию ждёт загрузки (document_idle)
// каждого фрейма — общий ответ не приходил никогда, и передача кончалась «страница не ответила».
//
// Здесь два средства, и каждое закрывает свою половину:
// 1. «Не ждать загрузки» (`injectImmediately` в настоящем порту): недогруженный и ленивый фрейм отвечает
//    сразу — тем, что в нём есть сейчас. Все наши потребители опрашивают вкладку по кругу, поэтому
//    недогруженная страница просто ответит полнее на следующем круге.
// 2. «По одному фрейму со сроком»: фрейм, который по-настоящему замёрз (сломанный скрипт сайта держит
//    поток), молчит один и не мешает остальным ответить.
//
// Модуль не знает, ЧТО спрашивают у фреймов: это решают потребители (опрос видео, поздний вход, возврат
// страницы). Он владеет только тем, КАК спросить: список фреймов, внедрение, срок и три исхода фрейма.

import type { Scheduler, TimerHandle } from "../host-link/native-port.ts";

/** Номер верхнего фрейма вкладки в обоих браузерах. */
export const TOP_FRAME_ID = 0;

/**
 * Сколько фреймов одной вкладки спрашиваем. Настоящие страницы держат единицы-десятки фреймов (klonua —
 * четыре); враждебная страница могла бы завести тысячи, и каждый стал бы отдельным вызовом браузера.
 * Верхний фрейм входит всегда: только он судит, есть ли наш скрипт на вкладке.
 */
export const MAX_FRAMES_PER_TAB = 256;

/** Что внедрить: самодостаточная функция (браузер сериализует её текст) и её аргументы. */
export interface FrameInjection<Args extends unknown[]> {
  readonly func: (...args: Args) => unknown;
  readonly args: Args;
}

/**
 * Ответ браузера на внедрение в один фрейм. Firefox не отклоняет внедрение в свои страницы
 * (`about:preferences`), а возвращает пустой элемент вместо ответа (живая проверка 10b): это `undefined`.
 */
export interface SingleFrameResult {
  readonly result?: unknown;
}

/** Браузерная сторона: список фреймов вкладки и внедрение в один фрейм. Настоящая — browser-frame-injection.ts. */
export interface FrameInjectionPort {
  /** Номера живых фреймов вкладки. Отказ промиса — вкладки нет или список браузер не дал. */
  listFrames(tabId: number): Promise<readonly number[]>;
  /** Внедрить в один фрейм без ожидания его загрузки. Отказ промиса — фрейм внедрение не принял. */
  injectIntoFrame<Args extends unknown[]>(tabId: number, frameId: number, injection: FrameInjection<Args>): Promise<SingleFrameResult | undefined>;
}

/** Исход одного фрейма. */
export type FrameReply =
  /** Фрейм ответил; `result` пришёл через страницу вкладки, верить ему можно только после проверки формы. */
  | { readonly kind: "answered"; readonly frameId: number; readonly result: unknown }
  /** Внедрение отклонено: служебный документ, нет прав на адрес, фрейм ушёл между списком и внедрением. */
  | { readonly kind: "refused"; readonly frameId: number; readonly error: string }
  /** Фрейм не ответил до срока: его поток занят, ответ если и придёт, то уже никому не нужен. */
  | { readonly kind: "silent"; readonly frameId: number };

/** Итог опроса вкладки по фреймам. */
export type TabFrameReplies =
  | {
      readonly kind: "replies";
      /** По одному исходу на каждый спрошенный фрейм, в порядке списка браузера. */
      readonly replies: readonly FrameReply[];
      /** Фреймов было больше `MAX_FRAMES_PER_TAB`: лишние не спрашивали. */
      readonly framesTruncated: boolean;
    }
  /** Список фреймов браузер не дал: вкладка закрыта, выгружена или чужая. */
  | { readonly kind: "frames_unknown"; readonly error: string };

/** Часть ответа `getAllFrames`, которая нужна выбору фреймов; Firefox жизненный цикл документа не сообщает. */
export interface ListedFrame {
  readonly frameId: number;
  readonly documentLifecycle?: string;
}

/**
 * Фреймы, в которые есть смысл внедрять. Chromium перечисляет и документы вне показа: страницу в кэше
 * «назад-вперёд», предзагрузку, документ на удалении. Внедрение в них отклоняется, а видео зрителя там нет.
 */
export function liveFrameIds(frames: readonly ListedFrame[] | null): number[] {
  // `null` по документации — вкладки с таким номером нет.
  if (frames === null) throw new Error("the browser knows no tab with this id");
  return frames.filter((frame) => frame.documentLifecycle === undefined || frame.documentLifecycle === "active").map((frame) => frame.frameId);
}

/** Верхний фрейм всегда в выборке, остальные — по порядку браузера, пока не кончится лимит. */
function framesToAsk(frameIds: readonly number[]): { frameIds: number[]; truncated: boolean } {
  const unique = [...new Set(frameIds)];
  if (unique.length <= MAX_FRAMES_PER_TAB) return { frameIds: unique, truncated: false };
  const others = unique.filter((frameId) => frameId !== TOP_FRAME_ID).slice(0, MAX_FRAMES_PER_TAB - 1);
  const withTop = unique.includes(TOP_FRAME_ID) ? [TOP_FRAME_ID, ...others] : unique.slice(0, MAX_FRAMES_PER_TAB);
  return { frameIds: withTop, truncated: true };
}

/** Спросить один фрейм и превратить любой ответ браузера в исход фрейма. */
async function askOneFrame<Args extends unknown[]>(
  port: FrameInjectionPort,
  tabId: number,
  frameId: number,
  injection: FrameInjection<Args>,
): Promise<FrameReply> {
  try {
    const answer = await port.injectIntoFrame(tabId, frameId, injection);
    if (answer === undefined) return { kind: "refused", frameId, error: "the frame did not accept the injection" };
    return { kind: "answered", frameId, result: answer.result };
  } catch (error) {
    return { kind: "refused", frameId, error: String(error) };
  }
}

/** Срок опроса: промис, который исполнится по сроку, и снятие таймера. */
interface Deadline {
  readonly reached: Promise<void>;
  cancel(): void;
}

/** Завести срок сразу, синхронно: он считается от начала опроса, а не от прихода списка фреймов. */
function startDeadline(scheduler: Scheduler, deadlineMs: number): Deadline {
  let timer: TimerHandle | undefined;
  const reached = new Promise<void>((resolve) => {
    timer = scheduler.setTimeout(resolve, deadlineMs);
  });
  // Таймер снимается в любом случае, чтобы не держать фон живым зря.
  return { reached, cancel: () => scheduler.clearTimeout(timer) };
}

/**
 * Внедрить функцию в каждый фрейм вкладки: все параллельно, общий срок `deadlineMs` от начала опроса.
 * Не бросает: любой сбой — это исход фрейма или `frames_unknown`.
 *
 * Список фреймов не обрывается сроком: его даёт процесс браузера, страница задержать его не может.
 */
export async function askEachFrame<Args extends unknown[]>(
  tabId: number,
  port: FrameInjectionPort,
  scheduler: Scheduler,
  injection: FrameInjection<Args>,
  deadlineMs: number,
): Promise<TabFrameReplies> {
  const deadline = startDeadline(scheduler, deadlineMs);
  try {
    let listed: readonly number[];
    try {
      listed = await port.listFrames(tabId);
    } catch (error) {
      return { kind: "frames_unknown", error: String(error) };
    }
    const { frameIds, truncated } = framesToAsk(listed);
    // Исходы фреймов, которые уже ответили или отказали; кто не попал сюда к сроку — молчит.
    const settled = new Map<number, FrameReply>();
    const asked = frameIds.map((frameId) => askOneFrame(port, tabId, frameId, injection).then((reply) => void settled.set(frameId, reply)));
    await Promise.race([Promise.all(asked), deadline.reached]);
    const replies = frameIds.map((frameId): FrameReply => settled.get(frameId) ?? { kind: "silent", frameId });
    return { kind: "replies", replies, framesTruncated: truncated };
  } finally {
    deadline.cancel();
  }
}

/**
 * Результаты ответивших фреймов — для потребителей, которым важен только итог «что нашлось» (поздний
 * вход, возврат страницы). Молчащий фрейм просто не даёт ответа. Отказ промиса — вкладка без списка
 * фреймов или ни один фрейм не принял внедрение: для них это «читать нечего», как и раньше.
 */
export async function answersOfEachFrame<Args extends unknown[]>(
  tabId: number,
  port: FrameInjectionPort,
  scheduler: Scheduler,
  injection: FrameInjection<Args>,
  deadlineMs: number,
): Promise<SingleFrameResult[]> {
  const outcome = await askEachFrame(tabId, port, scheduler, injection, deadlineMs);
  if (outcome.kind === "frames_unknown") throw new Error(`the frames of the tab are unknown: ${outcome.error}`);
  const answered = outcome.replies.flatMap((reply) => (reply.kind === "answered" ? [{ result: reply.result }] : []));
  if (answered.length === 0 && outcome.replies.length > 0 && outcome.replies.every((reply) => reply.kind === "refused")) {
    throw new Error("no frame of the tab accepted the injection");
  }
  return answered;
}
