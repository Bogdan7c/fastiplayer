// Наблюдатель «эфир превратился в запись» (сессия 16d1): общая часть страниц `mse-live-ends` и
// `hls-live-ends`.
//
// Что видит страница, когда прямой эфир кончается:
//  * пока эфир идёт, `video.duration === Infinity`;
//  * в конце `duration` становится конечной (событие `durationchange`), а затем элемент доигрывает
//    до конца и сообщает `ended`.
// Под тенью экстеншена те же факты обязана показывать тень (для HLS — по сообщению плеера
// `recording_window`, протокол 1.11). Наблюдатель только СМОТРИТ и пишет метрики отчёта; судья
// стенда (`live_ends/judge.rs`) читает эти метрики по именам из `METRICS` ниже.
//
// Метрики отчёта (все числа конечные; бесконечную длину нельзя класть в метрики, поэтому флаги 0/1):
//   on_air_duration_infinite       1, если в момент отметки «в эфире» `duration === Infinity`
//   duration_changes_after_on_air  сколько событий `durationchange` пришло после отметки
//   final_duration_finite          1, если в конце `duration` конечна
//   final_duration_s               конечная длина, с (только если она конечна)
//   ended_seen                     1, если элемент сообщил `ended`

import { waitFor, sleep } from './bench-checks.js';

/** Сколько ждём после конечной длины, чтобы поймать ЛИШНИЕ `durationchange`, мс. */
const SETTLE_AFTER_DURATION_MS = 1500;

/** События элемента, которые пишем в журнал страницы. */
const LOGGED_EVENTS = ['durationchange', 'ended', 'emptied', 'error', 'waiting', 'stalled'];

/** Строка «что видит страница» для журнала. */
function describe(video) {
  return `duration=${String(video.duration)} currentTime=${video.currentTime.toFixed(2)} paused=${video.paused} ended=${video.ended}`
    + `${video.error ? ` error=${video.error.code} '${video.error.message}'` : ''}`;
}

export class LiveEndsObserver {
  constructor(context, video) {
    this.context = context;
    this.video = video;
    /** Все `durationchange` с момента создания: {at, duration, finite}. */
    this.durationChanges = [];
    /** Сколько `durationchange` было к отметке «в эфире». */
    this.changesAtOnAir = null;
    this.endedEvents = 0;
    for (const name of LOGGED_EVENTS) {
      video.addEventListener(name, () => context.log(`event ${name}: ${describe(video)}`));
    }
    video.addEventListener('durationchange', () => {
      this.durationChanges.push({ at: performance.now(), duration: video.duration, finite: Number.isFinite(video.duration) });
    });
    video.addEventListener('ended', () => { this.endedEvents += 1; });
  }

  /** Эфир ещё идёт: `duration` бесконечна. Запоминает точку отсчёта для подсчёта `durationchange`. */
  markOnAir(checkName) {
    this.changesAtOnAir = this.durationChanges.length;
    const infinite = this.video.duration === Infinity;
    this.context.check(checkName, infinite, describe(this.video));
    this.context.metric('on_air_duration_infinite', infinite ? 1 : 0);
    return infinite;
  }

  /**
   * Эфир кончился: ждём конечную длину и `ended`, пишем метрики.
   * @param {number} waits.durationWaitMs сколько ждём конечную длину, мс
   * @param {number} waits.endedWaitMs сколько ждём `ended` после конечной длины (доиграть остаток), мс.
   *   Обе паузы подбирает страница так, чтобы сумма уложилась в ожидание ответа робота (45 с).
   * @param {boolean} exactlyOneChange требовать РОВНО одно `durationchange` после отметки «в эфире»
   *   (так должна вести себя тень; у обычного браузера число не фиксируем, только пишем)
   */
  async expectRecording({ exactlyOneChange, durationWaitMs, endedWaitMs }) {
    const { context, video } = this;
    const mark = this.changesAtOnAir ?? 0;
    const finite = await waitFor(() => Number.isFinite(video.duration), durationWaitMs);
    // Дадим лишним `durationchange` прийти: «ровно одно» нельзя доказать, не подождав.
    if (finite) await sleep(SETTLE_AFTER_DURATION_MS);
    const changes = this.durationChanges.slice(mark);
    context.metric('duration_changes_after_on_air', changes.length);
    context.metric('final_duration_finite', finite ? 1 : 0);
    if (finite) context.metric('final_duration_s', video.duration);
    context.check('recording-duration-is-finite', finite, describe(video));
    const changeSummary = changes.map((change) => String(change.duration)).join(', ') || 'none';
    context.check('recording-has-a-duration-change', changes.length >= 1 && changes.some((change) => change.finite),
      `durationchange events after on-air: ${changes.length} (${changeSummary})`);
    if (exactlyOneChange) {
      context.check('recording-has-exactly-one-duration-change', changes.length === 1 && changes[0].finite,
        `durationchange events after on-air: ${changes.length} (${changeSummary})`);
    }
    const ended = await waitFor(() => video.ended || this.endedEvents > 0, endedWaitMs);
    context.metric('ended_seen', ended ? 1 : 0);
    context.check('recording-reaches-ended', ended, describe(video));
  }
}
