// Сопоставление часов плеера и времени страницы в эфире HLS (сессия 10a3, решение владельца 232, п. 1).
//
// У эфира два независимых нуля: у нативного элемента Chromium — начало первого сегмента, который он увидел
// (разведка пакета D 10a3), у плеера — начало первого плейлиста, который скачал плеер. Поэтому «секунда 15»
// страницы и «секунда 15» плеера — разные моменты эфира, и показывать странице часы плеера как есть нельзя:
// `currentTime` прыгнул бы в момент передачи.
//
// Правило (единственный владелец сдвига — этот класс):
// - при передаче тень запоминает секунду страницы P0;
// - пока плеер не показал поколение, его тики — не его настоящие часы (он ещё ставит поток или повторяет
//   цель страницы): страница видит P0 и ожидание;
// - плеер шлёт `live_window` только после показа поколения и по тому же каналу, что часы, поэтому первый
//   тик ПОСЛЕ первого окна — настоящие часы J. На нём фиксируется `offset = P0 − J`, и странице этот тик
//   показывается ровно как P0: время не прыгает;
// - дальше время страницы = время плеера + offset, а цель перемотки страницы для плеера = цель − offset.
//
// Сдвиг бывает отрицательным (плеер начал отсчёт раньше элемента). Время страницы не бывает меньше нуля
// (`currentTime` медиаэлемента неотрицателен), поэтому моменты эфира раньше нуля страницы прижимаются к нулю —
// это и есть граница того, что странице можно показать.

import type { MediaTimeMicros } from "../../../protocol/time.ts";
import type { MappedTime } from "../shadow-time-source.ts";
import type { ClockTick } from "../shadow-timeline.ts";

export class LiveClockAlignment {
  /** Секунда страницы в момент передачи (P0), мкс. */
  readonly #handoffPageUs: MediaTimeMicros;
  /** Плеер прислал первое окно эфира: значит, поколение показано, и следующий тик — его настоящие часы. */
  #playerShowsGeneration = false;
  /** `P0 − J`, мкс; `undefined` — настоящего тика ещё не было. */
  #offsetUs: number | undefined;

  constructor(handoffPageUs: MediaTimeMicros) {
    this.#handoffPageUs = handoffPageUs;
  }

  /** Сдвиг известен: можно переводить моменты в обе стороны. */
  get isAligned(): boolean {
    return this.#offsetUs !== undefined;
  }

  /** Пришло окно эфира (поколение сверил фон): плеер показывает поколение. Повторы ничего не меняют. */
  notePlayerShowsGeneration(): void {
    this.#playerShowsGeneration = true;
  }

  /**
   * Тик плеера → тик для страницы. До показа поколения — P0 и ожидание (скорость плеера как есть: она не
   * зависит от шкалы). Первый настоящий тик фиксирует сдвиг и показывается ровно как P0.
   */
  pageTickOf(tick: ClockTick): ClockTick {
    if (this.#offsetUs === undefined) {
      // Пометка «чьи часы» (16a2) проходит как есть: перевод времени её не касается.
      if (!this.#playerShowsGeneration) return { position_us: this.#handoffPageUs, state: "waiting", rate: tick.rate, relation: tick.relation };
      this.#offsetUs = this.#handoffPageUs - tick.position_us;
    }
    return { position_us: this.#toPageUs(tick.position_us, this.#offsetUs), state: tick.state, rate: tick.rate, relation: tick.relation };
  }

  /** Момент плеера → момент страницы (не меньше нуля). */
  pageTimeOf(playerUs: MediaTimeMicros): MappedTime {
    const offsetUs = this.#offsetUs;
    return offsetUs === undefined ? { kind: "offset_unknown" } : { kind: "mapped", us: this.#toPageUs(playerUs, offsetUs) };
  }

  /** Момент страницы → момент плеера (не меньше нуля: плеер не знает моментов до своего начала). */
  playerTimeOf(pageUs: MediaTimeMicros): MappedTime {
    const offsetUs = this.#offsetUs;
    return offsetUs === undefined ? { kind: "offset_unknown" } : { kind: "mapped", us: Math.max(0, pageUs - offsetUs) };
  }

  /** Момент плеера раньше нуля страницы (до прижатия). Сдвиг неизвестен — `false`: сравнивать не с чем. */
  isBeforePageStart(playerUs: MediaTimeMicros): boolean {
    return this.#offsetUs !== undefined && playerUs + this.#offsetUs < 0;
  }

  #toPageUs(playerUs: MediaTimeMicros, offsetUs: number): MediaTimeMicros {
    return Math.max(0, playerUs + offsetUs);
  }
}
