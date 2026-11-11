// Модель «тени» без DOM: что видит страница (позиция, пауза, конец, перемотка, readyState)
// и какие медиасобытия ей положены. Источник правды — часы плеера (`clock`), тень между их
// тиками только интерполирует позицию.
//
// Модель не знает ни элемента, ни порта: она сообщает «сигналы» (событие страницы, исход
// обещаний `play()`, «показан кадр»), а контроллер тени превращает их в DOM-события и вызовы.
// Поэтому вся логика часов проверяется тестами на чистых числах.

import type { ClockState } from "../../protocol/session/player.ts";
import { MAX_JSON_SAFE_INTEGER } from "../../protocol/limits.ts";
import { type MediaTimeMicros, NORMAL_PLAYBACK_RATE_PERMILLE, type PlaybackRatePermille } from "../../protocol/time.ts";
import type { ClockRelation } from "../../shared/handoff-contract.ts";
import { holdConfirmedBy, newPositionHold, noteUnconfirmedTick, type PositionHold } from "./position-hold.ts";

// Пределы удержания живут у правил удержания (16a2); здесь — прежний адрес для тестов и соседей.
export { MAX_UNCONFIRMED_TICKS, PAGE_SEEK_CONFIRM_TOLERANCE_US, SOURCE_RESET_GIVE_UP_MS } from "./position-hold.ts";

/** Значения `HTMLMediaElement.readyState` по стандарту HTML. */
export const READY_STATE = {
  HAVE_NOTHING: 0,
  HAVE_METADATA: 1,
  HAVE_CURRENT_DATA: 2,
  HAVE_FUTURE_DATA: 3,
  HAVE_ENOUGH_DATA: 4,
} as const;

export type ReadyStateValue = (typeof READY_STATE)[keyof typeof READY_STATE];

/**
 * Не чаще одного «тикового» `timeupdate` в 250 мс: столько же даёт Chromium (стандарт —
 * «от 15 до 250 мс»). Плееры страниц рисуют по нему ползунок, чаще им не нужно, а каждое
 * событие — работа JS страницы (экономия, пункт 11). События перемотки/паузы не ограничены.
 */
export const MIN_TICK_TIMEUPDATE_INTERVAL_MS = 250;

/**
 * Сколько тень «доигрывает» позицию после последнего тика. Плеер шлёт часы ~4 раза в секунду;
 * 1,5 с покрывают пять потерянных тиков. Дальше позиция замирает: часы — правда, а убежавшая
 * вперёд тень потом прыгнула бы назад, что страница сочла бы перемоткой.
 */
export const MAX_EXTRAPOLATION_MS = 1_500;

/**
 * Дрожание интерполяции: тик может прийти чуть «позади» уже показанной позиции (задержка
 * доставки). Шаг назад до 250 мс (один период тиков) страница не видит — держим показанную
 * позицию. Больший шаг назад без перемотки — настоящий (смена поколения на старте, 03b):
 * принимаем его без `seeking`.
 */
export const BACKWARD_JITTER_TOLERANCE_US = 250_000;

/** Медиасобытия, которые тень выпускает сама (синтетические, `isTrusted=false`). */
export type ShadowMediaEventType =
  | "play"
  | "playing"
  | "pause"
  | "timeupdate"
  | "seeking"
  | "seeked"
  | "waiting"
  | "ended"
  | "ratechange"
  | "volumechange"
  | "canplay"
  | "canplaythrough";

/** Что модель просит сделать контроллер. */
export type TimelineSignal =
  | { readonly kind: "event"; readonly type: ShadowMediaEventType }
  /** Забрать текущие ожидающие обещания `play()` и исполнить/отклонить (как «take pending play promises»). */
  | { readonly kind: "settle_play_promises"; readonly outcome: "resolve" | "reject" }
  /** Плеер показал новый кадр: пора звать `requestVideoFrameCallback`. */
  | { readonly kind: "frame_presented" };

/** Тик часов плеера (команда `clock`). */
export interface ClockTick {
  readonly position_us: MediaTimeMicros;
  readonly state: ClockState;
  readonly rate: PlaybackRatePermille;
  /** К какому видео относится тик (16a2, пометка фона): часы прошлого видео не кончают и не подтверждают наше. */
  readonly relation: ClockRelation;
}

export interface TimelineInit {
  readonly positionUs: MediaTimeMicros;
  readonly paused: boolean;
  /** `playbackRate`, как его видит страница (дробное число, 1 — обычная). */
  readonly pageRate: number;
  readonly readyState: ReadyStateValue;
  readonly nowMs: number;
}

/** Конечная длительность ролика в мкс или `undefined` (неизвестна, эфир). */
export type DurationReader = () => MediaTimeMicros | undefined;

/**
 * Куда страница может перемотаться (намерение источника, а не «если эфир» внутри модели):
 * - `whole_timeline` — вся шкала `[0, длительность]` (MSE, файл, запись HLS — как было до 10a3);
 * - `range` — окно `[startUs, endUs]` во времени страницы (эфир с DVR: окно `live_window`, 10a3);
 * - `not_seekable` — перематывать некуда (эфир без DVR или окно ещё неизвестно). Как у браузера при пустом
 *   `seekable` (HTML, алгоритм seek: «если промежутков нет — `seeking = false` и выйти»): ни `seeking`, ни
 *   `page_seek`, позиция не меняется.
 */
export type PageSeekRange =
  | { readonly kind: "whole_timeline" }
  | { readonly kind: "range"; readonly startUs: MediaTimeMicros; readonly endUs: MediaTimeMicros }
  | { readonly kind: "not_seekable" };

/** Где сейчас можно перематываться странице; спрашивается на каждой перемотке (окно эфира едет). */
export type PageSeekRangeReader = () => PageSeekRange;

const WHOLE_TIMELINE: PageSeekRange = { kind: "whole_timeline" };

/** Итог перемотки страницы. */
export type PageSeekOutcome =
  /** Перемотка началась: `targetUs` — итоговая цель во времени страницы (уже прижатая к окну). */
  | { readonly kind: "seeking"; readonly targetUs: MediaTimeMicros }
  /** Перематывать некуда: ничего не изменилось, плееру говорить нечего. */
  | { readonly kind: "not_seekable" };

export type TimelineSignalSink = (signal: TimelineSignal) => void;

/** Итог `play()` для контроллера: что сообщить плееру. */
export interface PlayResult {
  /** Пауза снята этим вызовом (повод для `page_playback{play}`). */
  readonly unpaused: boolean;
  /** `play()` после конца ролика перемотал на начало (повод для `page_seek{0}`). */
  readonly restartedAtUs: MediaTimeMicros | undefined;
}

export class ShadowTimeline {
  readonly #readDurationUs: DurationReader;
  readonly #readPageSeekRange: PageSeekRangeReader;
  readonly #signal: TimelineSignalSink;
  #pagePaused: boolean;
  #ended = false;
  #pageRate: number;
  /** Последнее состояние часов плеера; до первого тика тень стоит (не интерполирует). */
  #clockState: ClockState = "paused";
  #clockRatePermille: PlaybackRatePermille = NORMAL_PLAYBACK_RATE_PERMILLE;
  /** Позиция в момент `#anchorAtMs`; от неё идёт интерполяция. */
  #anchorUs: MediaTimeMicros;
  #anchorAtMs: number;
  #lastTickAtMs: number | undefined;
  /** Последняя позиция, отданная странице (защита от мелких шагов назад). */
  #lastReportedUs: MediaTimeMicros;
  #hold: PositionHold | undefined;
  #readyState: ReadyStateValue;
  /** Сколько медиавремени «проиграно» в тени — основа поддельных счётчиков кадров. */
  #playedUs = 0;
  #lastTimeupdateAtMs = Number.NEGATIVE_INFINITY;

  /**
   * @param readPageSeekRange где странице можно перематываться (по умолчанию — вся шкала, как до 10a3).
   */
  constructor(init: TimelineInit, readDurationUs: DurationReader, signal: TimelineSignalSink, readPageSeekRange: PageSeekRangeReader = () => WHOLE_TIMELINE) {
    this.#readDurationUs = readDurationUs;
    this.#readPageSeekRange = readPageSeekRange;
    this.#signal = signal;
    this.#pagePaused = init.paused;
    this.#pageRate = init.pageRate;
    this.#anchorUs = init.positionUs;
    this.#anchorAtMs = init.nowMs;
    this.#lastReportedUs = init.positionUs;
    this.#readyState = init.readyState;
  }

  // -------------------------------------------------------------------------------------------
  // Что видит страница.
  // -------------------------------------------------------------------------------------------

  get paused(): boolean {
    return this.#pagePaused;
  }

  get ended(): boolean {
    return this.#ended;
  }

  /** Перемотка видна странице (у смены источника `seeking` нет — как у настоящего элемента). */
  get seeking(): boolean {
    return this.#hold !== undefined && this.#hold.kind !== "source_reset";
  }

  get pageRate(): number {
    return this.#pageRate;
  }

  get readyState(): ReadyStateValue {
    return this.#readyState;
  }

  /** Позиция для страницы: цель перемотки или интерполяция от последнего тика. */
  positionUs(nowMs: number): MediaTimeMicros {
    if (this.#hold !== undefined) return this.#hold.targetUs;
    const rawUs = this.#clampToTimeline(this.#anchorUs + this.#advanceSinceAnchorUs(nowMs));
    const backwardStepUs = this.#lastReportedUs - rawUs;
    if (backwardStepUs > 0 && backwardStepUs <= BACKWARD_JITTER_TOLERANCE_US) return this.#lastReportedUs;
    this.#lastReportedUs = rawUs;
    return rawUs;
  }

  /** Медиавремя, «проигранное» тенью (для счётчиков кадров). */
  playedUs(nowMs: number): number {
    return this.#playedUs + this.#advanceSinceAnchorUs(nowMs);
  }

  // -------------------------------------------------------------------------------------------
  // Часы плеера.
  // -------------------------------------------------------------------------------------------

  onClock(tick: ClockTick, nowMs: number): void {
    this.#playedUs += this.#advanceSinceAnchorUs(nowMs);
    this.#lastTickAtMs = nowMs;
    this.#anchorAtMs = nowMs;
    const state = clockStateForThisVideo(tick);
    this.#clockState = state;
    this.#clockRatePermille = tick.rate;
    const tickPositionUs = this.#clampToTimeline(tick.position_us);

    const hold = this.#hold;
    if (hold !== undefined) {
      const holdTick = { positionUs: tickPositionUs, state, relation: tick.relation };
      if (!holdConfirmedBy(hold, holdTick, nowMs)) {
        noteUnconfirmedTick(hold, holdTick);
        return;
      }
      this.#finishHold(hold, tickPositionUs, nowMs);
    } else {
      // Шаг назад без нашей перемотки (новое поколение на старте) — не перемотка: просто
      // новая позиция; мелкое дрожание гасит `positionUs`. Позиция прошлого поколения тоже принимается:
      // без удержания это смена качества новым MediaSource — то же место того же фильма (16a2).
      this.#anchorUs = tickPositionUs;
      this.#applyClockState(nowMs);
    }

    if (this.#advancing()) {
      if (nowMs - this.#lastTimeupdateAtMs >= MIN_TICK_TIMEUPDATE_INTERVAL_MS) this.#emitTimeupdate(nowMs);
      this.#signal({ kind: "frame_presented" });
    }
  }

  // -------------------------------------------------------------------------------------------
  // Команды плеера и вызовы страницы.
  // -------------------------------------------------------------------------------------------

  /** `play()` страницы или команда `play` плеера. */
  play(origin: "page" | "player", nowMs: number): PlayResult {
    let restartedAtUs: MediaTimeMicros | undefined;
    if (this.#ended) {
      // Стандарт: `play()` страницы после конца ролика сначала перематывает на начало (у эфира —
      // на начало окна; перематывать некуда — просто играть дальше, как команда плеера).
      const restart = origin === "page" ? this.pageSeek(0, nowMs) : undefined;
      if (restart?.kind === "seeking") restartedAtUs = restart.targetUs;
      else this.#ended = false;
    }
    if (!this.#pagePaused) {
      // Уже играет: обещание исполнится сразу, если данных хватает, иначе — с `playing`.
      if (this.#readyState >= READY_STATE.HAVE_FUTURE_DATA) this.#signal({ kind: "settle_play_promises", outcome: "resolve" });
      return { unpaused: false, restartedAtUs };
    }
    this.#reanchor(nowMs);
    this.#pagePaused = false;
    this.#emit("play");
    if (this.#readyState <= READY_STATE.HAVE_CURRENT_DATA) {
      this.#emit("waiting");
    } else {
      this.#notifyAboutPlaying();
    }
    return { unpaused: true, restartedAtUs };
  }

  /** `pause()` страницы или команда `pause` плеера. Возвращает, изменилось ли состояние. */
  pause(nowMs: number): boolean {
    if (this.#pagePaused) return false;
    this.#reanchor(nowMs);
    this.#pagePaused = true;
    this.#emitTimeupdate(nowMs);
    this.#emit("pause");
    this.#signal({ kind: "settle_play_promises", outcome: "reject" });
    return true;
  }

  /** Страница сменила `playbackRate` (значение уже проверено браузером). */
  setPageRate(rate: number): boolean {
    if (rate === this.#pageRate) return false;
    this.#pageRate = rate;
    this.#emit("ratechange");
    return true;
  }

  /** Плеер сменил скорость: страница видит её сразу, интерполяция идёт с новой скоростью. */
  setRateFromPlayer(ratePermille: PlaybackRatePermille, nowMs: number): void {
    this.#reanchor(nowMs);
    this.#clockRatePermille = ratePermille;
    this.setPageRate(ratePermille / 1000);
  }

  /**
   * Перемотка плеером. Цель ограничена только шкалой `[0, длительность]`, не окном перемотки страницы:
   * плеер — правда о своём потоке. Перемотка на/за конец завершится состоянием `ended`. Возвращает итоговую
   * цель (её видит страница).
   */
  playerSeek(targetUs: MediaTimeMicros, nowMs: number): MediaTimeMicros {
    return this.#startSeekHold("player_seek", this.#clampToTimeline(targetUs), nowMs);
  }

  /**
   * Перемотка страницей. Цель прижимается к окну перемотки источника (`PageSeekRange`), как это делает
   * браузер с `seekable`; окна нет — перемотки нет (`not_seekable`).
   */
  pageSeek(targetUs: MediaTimeMicros, nowMs: number): PageSeekOutcome {
    const range = this.#readPageSeekRange();
    switch (range.kind) {
      case "not_seekable":
        return { kind: "not_seekable" };
      case "whole_timeline":
        return { kind: "seeking", targetUs: this.#startSeekHold("page_seek", this.#clampToTimeline(targetUs), nowMs) };
      case "range": {
        const insideWindowUs = Math.max(range.startUs, Math.min(targetUs, range.endUs));
        return { kind: "seeking", targetUs: this.#startSeekHold("page_seek", this.#clampToTimeline(insideWindowUs), nowMs) };
      }
    }
  }

  /**
   * Страница поставила элементу новый источник (новый MediaSource на том же элементе —
   * смена поколения): повторяем «алгоритм загрузки» стандарта — пауза без события `pause`,
   * отказ ожидающих `play()`, позиция 0, скорость = `defaultPlaybackRate` (её даёт браузер).
   * Возвращает, изменилась ли видимая скорость.
   */
  resetForNewSource(pageRate: number, nowMs: number): boolean {
    this.#reanchor(nowMs);
    this.#signal({ kind: "settle_play_promises", outcome: "reject" });
    this.#pagePaused = true;
    this.#ended = false;
    // Пока плеер не признал новый источник, его часы несут позицию СТАРОГО поколения (он
    // показывает его до первых данных нового): тень держит 0 и не принимает их `ended` (Д-4).
    this.#hold = newPositionHold({ kind: "source_reset", targetUs: 0, landsAtEnd: false }, nowMs);
    this.#anchorUs = 0;
    this.#lastReportedUs = 0;
    this.#readyState = READY_STATE.HAVE_METADATA;
    const rateChanged = pageRate !== this.#pageRate;
    this.#pageRate = pageRate;
    return rateChanged;
  }

  /**
   * Тень поставлена на другой элемент, а плеер ещё показывает прошлое поколение: его часы несут чужую
   * позицию и чужой `ended` (сессия 09c2). Держать свою позицию, пока он не признает новый источник.
   * В отличие от `resetForNewSource`, страница ничего не сбрасывала: пауза и скорость остаются.
   */
  awaitPlayerOnNewSource(nowMs: number): void {
    const targetUs = this.positionUs(nowMs);
    this.#reanchor(nowMs);
    this.#hold = newPositionHold({ kind: "source_reset", targetUs, landsAtEnd: false }, nowMs);
  }

  // -------------------------------------------------------------------------------------------
  // Внутреннее.
  // -------------------------------------------------------------------------------------------

  /** Удержание позиции на уже прижатой цели перемотки. */
  #startSeekHold(kind: "player_seek" | "page_seek", clampedUs: MediaTimeMicros, nowMs: number): MediaTimeMicros {
    const durationUs = this.#readDurationUs();
    this.#startHold(newPositionHold({ kind, targetUs: clampedUs, landsAtEnd: durationUs !== undefined && clampedUs >= durationUs }, nowMs), nowMs);
    return clampedUs;
  }

  #advancing(): boolean {
    return this.#clockState === "playing" && !this.#pagePaused && !this.#ended && this.#hold === undefined;
  }

  /** Сколько мкс позиция прошла от якоря: только при игре и не дальше `MAX_EXTRAPOLATION_MS` от тика. */
  #advanceSinceAnchorUs(nowMs: number): number {
    if (!this.#advancing()) return 0;
    const extrapolationEndMs = (this.#lastTickAtMs ?? this.#anchorAtMs) + MAX_EXTRAPOLATION_MS;
    const elapsedMs = Math.max(0, Math.min(nowMs, extrapolationEndMs) - this.#anchorAtMs);
    // мс × (1000 мкс/мс × скорость/1000) = мс × тысячные доли скорости.
    return Math.round(elapsedMs * this.#clockRatePermille);
  }

  /** Перенести якорь в «сейчас» перед сменой того, что влияет на интерполяцию. */
  #reanchor(nowMs: number): void {
    const advanceUs = this.#advanceSinceAnchorUs(nowMs);
    const positionUs = this.positionUs(nowMs);
    this.#playedUs += advanceUs;
    this.#anchorUs = positionUs;
    this.#anchorAtMs = nowMs;
  }

  #clampToTimeline(positionUs: number): MediaTimeMicros {
    const durationUs = this.#readDurationUs() ?? MAX_JSON_SAFE_INTEGER;
    return Math.max(0, Math.min(positionUs, durationUs));
  }

  #derivedReadyState(): ReadyStateValue {
    if (this.#hold !== undefined) return READY_STATE.HAVE_METADATA;
    if (this.#clockState === "waiting") return READY_STATE.HAVE_CURRENT_DATA;
    return READY_STATE.HAVE_ENOUGH_DATA;
  }

  #startHold(hold: PositionHold, nowMs: number): void {
    this.#reanchor(nowMs);
    this.#hold = hold;
    this.#ended = false;
    this.#lastReportedUs = hold.targetUs;
    this.#emit("seeking");
    this.#setReadyState(READY_STATE.HAVE_METADATA, nowMs);
  }

  #finishHold(hold: PositionHold, tickPositionUs: MediaTimeMicros, nowMs: number): void {
    this.#hold = undefined;
    // Перемотка на конец стоит ровно на длительности; иначе правда — позиция часов.
    const landingUs = hold.landsAtEnd ? hold.targetUs : tickPositionUs;
    this.#anchorUs = landingUs;
    this.#lastReportedUs = landingUs;
    if (hold.kind !== "source_reset") {
      this.#emitTimeupdate(nowMs);
      this.#emit("seeked");
    }
    if (hold.landsAtEnd) this.#enterEnded({ timeupdateAlreadySent: true }, nowMs);
    this.#applyClockState(nowMs);
  }

  /** Состояние часов → `ended` и readyState (с положенными событиями переходов). */
  #applyClockState(nowMs: number): void {
    if (this.#clockState === "ended" && !this.#ended) this.#enterEnded({ timeupdateAlreadySent: false }, nowMs);
    this.#setReadyState(this.#derivedReadyState(), nowMs);
  }

  /** Конец ролика по стандарту: `timeupdate`, пауза с `pause`, затем `ended`. */
  #enterEnded(options: { readonly timeupdateAlreadySent: boolean }, nowMs: number): void {
    this.#ended = true;
    if (!options.timeupdateAlreadySent) this.#emitTimeupdate(nowMs);
    if (!this.#pagePaused) {
      this.#pagePaused = true;
      this.#emit("pause");
    }
    this.#emit("ended");
  }

  /** Переходы readyState по стандарту: падение при игре — `waiting`, подъём — `canplay`/`playing`. */
  #setReadyState(next: ReadyStateValue, nowMs: number): void {
    const previous = this.#readyState;
    if (previous === next) return;
    this.#readyState = next;
    const potentiallyPlaying = !this.#pagePaused && !this.#ended;
    if (previous >= READY_STATE.HAVE_FUTURE_DATA && next <= READY_STATE.HAVE_CURRENT_DATA && potentiallyPlaying) {
      this.#emitTimeupdate(nowMs);
      this.#emit("waiting");
    }
    if (previous <= READY_STATE.HAVE_CURRENT_DATA && next >= READY_STATE.HAVE_FUTURE_DATA) {
      this.#emit("canplay");
      if (potentiallyPlaying) this.#notifyAboutPlaying();
      if (next === READY_STATE.HAVE_ENOUGH_DATA) this.#emit("canplaythrough");
    }
  }

  /** «Notify about playing»: событие `playing`, затем исполнение ожидающих `play()`. */
  #notifyAboutPlaying(): void {
    this.#emit("playing");
    this.#signal({ kind: "settle_play_promises", outcome: "resolve" });
  }

  #emitTimeupdate(nowMs: number): void {
    this.#lastTimeupdateAtMs = nowMs;
    this.#emit("timeupdate");
  }

  #emit(type: ShadowMediaEventType): void {
    this.#signal({ kind: "event", type });
  }
}

/**
 * Состояние часов, как его видит наше видео. Конец ПРОШЛОГО видео для нашего значит только «часы стоят»
 * (`paused`: позиция не идёт, `readyState` тот же, что дал бы `ended`), а не `ended`: иначе конец баннера, пришедший в тень
 * фильма, защёлкнул бы у фильма `ended` и паузу (16a2). Это единственное место, где пометка меняет
 * состояние: дальше `ended` из прошлого видео не доходит ни до удержания, ни до `#applyClockState`.
 */
function clockStateForThisVideo(tick: ClockTick): ClockState {
  return tick.relation === "previous_video" && tick.state === "ended" ? "paused" : tick.state;
}
