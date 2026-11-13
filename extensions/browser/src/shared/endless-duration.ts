// Длительность «без конца» (мост 16d1, решение 319): одно правило на весь экстеншен.
//
// Эфир объявляет себя длительностью `+Infinity` (MSE, нативный HLS). Но не все сайты так делают:
// Twitch в Firefox ставит `MediaSource.duration` огромным конечным числом, и элемент показывает
// 9 223 372 036 854,775 с (~292 000 лет — внутренний «максимум» Firefox в микросекундах). Плеер
// получил бы запись длиной в века. Ни одна настоящая запись не длится год (круглосуточные записи
// YouTube — до 12 ч), поэтому всё, что длиннее года, — эфир.
//
// Правило спрашивают все, кто решает «эфир или запись» по длительности: длительность MSE для плеера
// (`page/mse-values.ts`), вид нативного HLS (`page/hls-stream-kind.ts`) и факты кандидата
// (`page/candidate-facts.ts` — память возврата и суд «реклама или фильм»). Разойтись они не должны.

/** Граница «без конца»: год в секундах (365 суток). */
export const ENDLESS_DURATION_THRESHOLD_SECONDS = 365 * 24 * 60 * 60;

/** Длительность эфира: `+Infinity` или конечное число длиннее года. NaN и отрицательные — нет. */
export function isEndlessDurationSeconds(seconds: number): boolean {
  if (seconds === Number.POSITIVE_INFINITY) return true;
  return Number.isFinite(seconds) && seconds > ENDLESS_DURATION_THRESHOLD_SECONDS;
}
