// Клиент руки-функции адаптеров в изолированном мире (сессия 14b, решение 282).
//
// Адаптер просит «вызови действие X своего белого списка» → вызов уходит MAIN-миру по личному порту → ответ
// с тем же номером закрывает обещание. Это тот же приём, что вопросы про кандидатов и указатель в
// пограничнике (`relay.ts`), но отдельным модулем: пограничник и так большой, а у руки своя граница.
//
// Граница со страницей (инвариант 1):
// - ответ проверяется строго (`parseAdapterHandReply`), мусор и ответы на незаданный вопрос отбрасываются;
// - у ответов свой бюджет (`ADAPTER_HAND_REPLIES_PER_SECOND`): страница не завалит нас «ответами»;
// - вызовов в полёте не больше `MAX_ADAPTER_HAND_CALLS_IN_FLIGHT`, у каждого срок `ADAPTER_HAND_TIMEOUT_MS`:
//   вечное обещание страницы не держит адаптер.

import type { PlayerHand, PlayerHandResult } from "../adapters/capabilities/variants/contract.ts";
import { looksLikeAdapterHandReply, parseAdapterHandReply, type AdapterHandCall } from "../shared/adapter-hand-contract.ts";
import { createTokenBucket, type MillisecondClock } from "../shared/token-bucket.ts";
import { ADAPTER_HAND_REPLIES_PER_SECOND } from "./page-event-budget.ts";
import type { RelayTimers } from "./relay.ts";

/** Срок ответа MAIN-мира на один вызов. Функции плеера сайта отвечают сразу; срок — защита от вечного обещания. */
export const ADAPTER_HAND_TIMEOUT_MS = 2_000;

/** Сколько вызовов может ждать ответа одновременно; лишний сразу получает `no_answer`. */
export const MAX_ADAPTER_HAND_CALLS_IN_FLIGHT = 16;

/** Наибольший номер вызова (u32), после него счёт идёт с 1. */
const MAX_HAND_REQUEST_ID = 0xffff_ffff;

export interface AdapterHandClientDependencies {
  /** Отправка MAIN-миру по личному порту (порт сам копит сообщения до встречи миров). */
  readonly sendToPage: (call: AdapterHandCall) => void;
  readonly timers: RelayTimers;
  readonly now: MillisecondClock;
}

/** Счётчики пограничника руки — для тестов и диагностики. */
export interface AdapterHandCounters {
  /** Ответы не по форме, на незаданный вопрос или сверх бюджета. */
  readonly rejectedReplies: number;
  /** Вызовы, на которые MAIN-мир не ответил в срок (или их было слишком много сразу). */
  readonly unanswered: number;
}

export interface AdapterHandClient {
  /** Рука одного сайта: имя сайта вшито, адаптер называет только действие. */
  handFor(siteId: string): PlayerHand;
  /**
   * Сообщение MAIN-мира. `true` — это ответ руки (принят или отброшен здесь), дальше его не передавать;
   * `false` — сообщение не наше, его разбирает пограничник передачи.
   */
  acceptPageMessage(message: unknown): boolean;
  counters(): AdapterHandCounters;
}

interface PendingCall {
  readonly resolve: (result: PlayerHandResult) => void;
  readonly timer: unknown;
}

const NO_ANSWER: PlayerHandResult = { kind: "no_answer" };

export function createAdapterHandClient(dependencies: AdapterHandClientDependencies): AdapterHandClient {
  const { timers } = dependencies;
  const pending = new Map<number, PendingCall>();
  const replyBudget = createTokenBucket(ADAPTER_HAND_REPLIES_PER_SECOND, dependencies.now);
  let nextRequestId = 1;
  let rejectedReplies = 0;
  let unanswered = 0;

  function takeRequestId(): number {
    const requestId = nextRequestId;
    nextRequestId = nextRequestId >= MAX_HAND_REQUEST_ID ? 1 : nextRequestId + 1;
    return requestId;
  }

  function settle(requestId: number, result: PlayerHandResult): boolean {
    const call = pending.get(requestId);
    if (call === undefined) return false;
    pending.delete(requestId);
    timers.clearTimeout(call.timer);
    call.resolve(result);
    return true;
  }

  function call(site: string, action: string, argument: string | null): Promise<PlayerHandResult> {
    if (pending.size >= MAX_ADAPTER_HAND_CALLS_IN_FLIGHT) {
      unanswered += 1;
      return Promise.resolve(NO_ANSWER);
    }
    const requestId = takeRequestId();
    return new Promise((resolve) => {
      const timer = timers.setTimeout(() => {
        if (settle(requestId, NO_ANSWER)) unanswered += 1;
      }, ADAPTER_HAND_TIMEOUT_MS);
      pending.set(requestId, { resolve, timer });
      try {
        dependencies.sendToPage({ kind: "adapter_hand_call", request_id: requestId, site, action, argument });
      } catch {
        // Порт бросил: ответа не будет, честно говорим это сразу, а не через срок.
        settle(requestId, NO_ANSWER);
        unanswered += 1;
      }
    });
  }

  function acceptPageMessage(message: unknown): boolean {
    if (!looksLikeAdapterHandReply(message)) return false;
    // Бюджет — до разбора: разбор значения стоит обхода дерева, флуд не должен его покупать.
    if (!replyBudget.take(1)) {
      rejectedReplies += 1;
      return true;
    }
    const reply = parseAdapterHandReply(message);
    if (reply === undefined || !settle(reply.request_id, reply.outcome)) rejectedReplies += 1;
    return true;
  }

  return {
    handFor: (siteId) => ({ call: (action, argument) => call(siteId, action, argument) }),
    acceptPageMessage,
    counters: () => ({ rejectedReplies, unanswered }),
  };
}
