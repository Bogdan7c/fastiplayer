// Поддельный порт внедрения во фреймы (сессия 15c): вкладка из описанных фреймов, каждый ведёт себя
// как настоящий — отвечает, отказывает (как Firefox на своих страницах) или молчит (замёрз).

import { TOP_FRAME_ID, type FrameInjection, type FrameInjectionPort, type SingleFrameResult } from "../../src/background/frame-injection/frame-injection.ts";

/** Фрейм поддельной вкладки. По умолчанию отвечает `result`. */
export interface FakeFrame {
  readonly frameId: number;
  readonly result?: unknown;
  /** Внедрение не принято: Firefox кладёт пустой элемент вместо ответа. */
  readonly refuses?: true;
  /** Внедрение отклонено промисом (служебный документ Chromium, фрейм ушёл). */
  readonly rejects?: true;
  /** Фрейм замёрз: ответа не будет никогда. */
  readonly silent?: true;
}

/** Что порт видел: сколько раз просили список и какие внедрения пришли. */
export interface FakeFrameInjection extends FrameInjectionPort {
  readonly listings: () => number;
  readonly injections: () => readonly { readonly tabId: number; readonly frameId: number; readonly args: readonly unknown[] }[];
}

const NEVER = new Promise<never>(() => {});

/**
 * `frames()` — фреймы вкладки сейчас. Промис вместо списка значит «вкладка повисла целиком»: список
 * фреймов браузер даёт (только верхний), а сам верхний фрейм не отвечает, пока промис не исполнится.
 * `listError()` — сбой списка фреймов (вкладка закрыта); `undefined` — список даётся.
 */
export function createFakeFrameInjection(
  frames: () => readonly FakeFrame[] | Promise<readonly FakeFrame[]>,
  listError: () => Error | undefined = () => undefined,
): FakeFrameInjection {
  let listings = 0;
  const injections: { tabId: number; frameId: number; args: readonly unknown[] }[] = [];
  return {
    listFrames(_tabId) {
      listings += 1;
      const error = listError();
      if (error !== undefined) return Promise.reject(error);
      const current = frames();
      if (Array.isArray(current)) return Promise.resolve(current.map((frame) => frame.frameId));
      // Промис фреймов отвергнут — это отказ внедрения в верхний фрейм; его увидит `injectIntoFrame`, здесь он не теряется.
      void Promise.resolve(current).catch(() => undefined);
      return Promise.resolve([TOP_FRAME_ID]);
    },
    async injectIntoFrame<Args extends unknown[]>(tabId: number, frameId: number, injection: FrameInjection<Args>): Promise<SingleFrameResult | undefined> {
      injections.push({ tabId, frameId, args: injection.args });
      const current = await frames();
      const frame = current.find((candidate) => candidate.frameId === frameId);
      if (frame === undefined) throw new Error(`No frame with id ${frameId} in tab ${tabId}.`);
      if (frame.silent === true) return NEVER;
      if (frame.rejects === true) throw new Error("Cannot access contents of the page.");
      if (frame.refuses === true) return undefined;
      return { result: frame.result };
    },
    listings: () => listings,
    injections: () => injections,
  };
}
