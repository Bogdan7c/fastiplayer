// Бюджет событий страницы на одну передачу (решение 200, «страница — враг»): изолированный мир
// считает события MAIN-мира по видам. Настоящему видео лимиты не мешают, а флуд не доходит до фона.
//
// Владеет только счётом: что делать при превышении, решает пограничник (relay.ts). Байты здесь не
// считаются: у них свой потолок (очередь сеанса в фоне и счётчик копий в MAIN-мире).
//
// Откуда числа. Замер 11 записей живых сайтов и стенда (Twitch в Firefox и Chrome, YouTube, 24 минуты
// суммарно) — пик за скользящую секунду и за 100 мс:
//   appendBuffer (`bytes`)            246/с, 87 за 100 мс, 2,0 МиБ/с, 1,1 МиБ за 100 мс, кусок ≤ 0,74 МиБ
//   remove (`mse`)                    6/с, 4 за 100 мс
//   duration, addSourceBuffer (`mse`) 2/с
//   перемотки страницы (`page_seek`)  2/с
//   play/pause/rate (`page_playback`) 5/с
// Все 11 записей, прогнанные через этот бюджет с их настоящими отметками времени, не потратили и
// 2 % ни одного ведра. Лимиты подняты в разы (решение 200): вредная страница успевает немного
// погрузить свою вкладку, зато настоящее видео не обрывается никогда.

import type { PageEventBudgetKind } from "../shared/handoff-contract.ts";
import { MAX_JOURNAL_ENTRIES } from "../shared/page-message-validation.ts";
import { createTokenBucket, type MillisecondClock, type TokenBucket } from "../shared/token-bucket.ts";

/**
 * Лимиты событий в секунду по видам.
 * - `mse`: в записях 6/с, но сайт вправе менять `timestampOffset` или чистить буфер на КАЖДЫЙ кусок,
 *   поэтому опорой служит пик кусков 246/с: 4 096/с — это 16 раз от него.
 * - `page_seek`: 2/с в записях; страница, которая тащит ползунок по кадрам экрана, даст до 144/с.
 * - `page_playback`: 5/с в записях (старт плеера).
 * - `player_seek_applied`: ответ на перемотку плеера, плеер шлёт их не чаще своих перемоток.
 * - `file_source`, `hls_source` и `shadow_alive`: раз на передачу и раз в секунду по договору.
 * - `malformed`: в записях 0. Лимит не нулевой только для того, чтобы случайный сбой формы не рвал
 *   передачу; сотни в секунду — уже не случайность.
 */
export const PAGE_EVENT_LIMITS_PER_SECOND: Readonly<Record<PageEventBudgetKind, number>> = {
  mse: 4_096,
  page_seek: 512,
  page_playback: 256,
  player_seek_applied: 256,
  file_source: 16,
  hls_source: 16,
  shadow_alive: 16,
  malformed: 256,
};

/** События снимка в начале повтора: на каждый из 8 SourceBuffer `added` и смещение, плюс длительность и конец. */
const JOURNAL_SNAPSHOT_EVENTS = 18;

/**
 * Разовый запас событий `mse` на старт передачи: повтор журнала (решение 176).
 *
 * Повтор — это один синхронный цикл, поэтому весь журнал проходит за один проход потока страницы.
 * Замер худшего случая: 64 МиБ журнала самыми мелкими кусками из записей (7,5 КБ) — 8 900 записей, а с
 * событием `mse` на каждый кусок вдвое больше. Пограничник пропускает их за 13 мс, то есть события
 * идут в сотни раз быстрее обычного лимита. Он здесь не годится: настоящий журнал не должен рвать передачу.
 *
 * Метке «это повтор» верить нельзя: её поставил бы MAIN-мир, то есть страница. Поэтому запас считается
 * штуками, а не временем, и выдаётся один раз на передачу: сперва тратится обычное ведро, потом запас.
 * Вредная страница тоже может его потратить, но только один раз и только на `MAX_JOURNAL_ENTRIES` событий,
 * после чего действует обычный лимит. Флудить вечно под видом повтора нельзя, а настоящий повтор
 * лимит не заденет никогда: журнал не держит больше записей, чем этот запас.
 */
export const JOURNAL_REPLAY_EVENT_CREDIT = MAX_JOURNAL_ENTRIES + JOURNAL_SNAPSHOT_EVENTS;

/** Итог проверки: событие можно пропускать, или бюджет кончился и передачу пора заканчивать. */
export type BudgetVerdict = "admitted" | "exceeded";

/** Бюджет одной передачи. */
export interface PageEventBudget {
  /** Учесть событие вида `kind`. */
  admit(kind: PageEventBudgetKind): BudgetVerdict;
}

/** Параметры бюджета; по умолчанию — боевые лимиты (в тестах — свои, чтобы не гонять тысячи событий). */
export interface PageEventBudgetParameters {
  readonly now: MillisecondClock;
  readonly limitsPerSecond?: Readonly<Record<PageEventBudgetKind, number>>;
  readonly replayEventCredit?: number;
}

export function createPageEventBudget(parameters: PageEventBudgetParameters): PageEventBudget {
  const limits = parameters.limitsPerSecond ?? PAGE_EVENT_LIMITS_PER_SECOND;
  const buckets = new Map<PageEventBudgetKind, TokenBucket>();
  let replayCredit = parameters.replayEventCredit ?? JOURNAL_REPLAY_EVENT_CREDIT;

  /** Ведро вида; создаётся при первом событии вида, чтобы редкие виды ничего не стоили. */
  function bucketOf(kind: PageEventBudgetKind): TokenBucket {
    const existing = buckets.get(kind);
    if (existing !== undefined) return existing;
    const bucket = createTokenBucket(limits[kind], parameters.now);
    buckets.set(kind, bucket);
    return bucket;
  }

  return {
    admit(kind) {
      if (bucketOf(kind).take(1)) return "admitted";
      if (kind === "mse" && replayCredit > 0) {
        replayCredit -= 1;
        return "admitted";
      }
      return "exceeded";
    },
  };
}
