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

/**
 * Перемотку страницы подтверждает тик, близкий к цели: тики, отправленные плеером до того,
 * как он узнал о `page_seek`, ещё несут старую позицию и не должны «отматывать» страницу назад.
 */
export const PAGE_SEEK_CONFIRM_TOLERANCE_US = 1_000_000;

/**
 * Перемотка страницы: если плеер встал не туда (живой эфир, обрезка окна DVR), восемь тиков
 * (~2 с) без подтверждения — и следующий тик принимается как есть: перемотка не висит вечно.
 * Смену источника это правило НЕ касается (там чужие тики — часы старого поколения, Д-4).
 */
export const MAX_UNCONFIRMED_TICKS = 8;

/**
 * Страховочный выход для смены источника (Д-4), когда плеер так и не признал новый источник
 * (ни тика у цели, ни тика ожидания): через 15 с тень принимает позицию часов, но НИКОГДА — их
 * `ended`. Меньше нельзя: плеер ещё показывает старое поколение, пока данные нового (рекламы)
 * не пришли, и его часы несут позицию старого фильма; 15 с покрывают медленную загрузку первого
 * куска рекламы. Выход нужен на случай плеера, который не шлёт «0 / waiting» (иначе тень стояла
 * бы на 0 вечно).
 */
export const SOURCE_RESET_GIVE_UP_MS = 15_000;

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
}

/** Кто двигает позицию: плеер, страница или сама смена источника. */
type PositionHoldKind = "player_seek" | "page_seek" | "source_reset";

/** Позиция «заморожена» на цели, пока часы плеера не подтвердят прыжок. */
interface PositionHold {
  readonly kind: PositionHoldKind;
  readonly targetUs: MediaTimeMicros;
  /** Цель — конец ролика: после подтверждения тень переходит в `ended`. */
  readonly landsAtEnd: boolean;
  /** Перемотки: сколько ещё чужих тиков стерпеть до принятия позиции часов. */
  unconfirmedTicksLeft: number;
  /** Когда hold поставлен (страховочный выход смены источника считается от него). */
  readonly startedAtMs: number;
  /**
   * Смена источника: плеер уже прислал тик ожидания («0 / waiting», пока новое поколение не
   * показано) — значит, он признал новый источник, и следующий тик несёт уже его позицию.
   */
  playerAcknowledgedNewSource: boolean;
}

/** Часть тика, по которой решается судьба hold. */
interface HoldTick {
  readonly positionUs: MediaTimeMicros;
  readonly state: ClockState;
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

  constructor(init: TimelineInit, readDurationUs: DurationReader, signal: TimelineSignalSink) {
    this.#readDurationUs = readDurationUs;
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
    this.#clockState = tick.state;
    this.#clockRatePermille = tick.rate;
    const tickPositionUs = this.#clampToTimeline(tick.position_us);

    const hold = this.#hold;
    if (hold !== undefined) {
      if (!holdConfirmedBy(hold, { positionUs: tickPositionUs, state: tick.state }, nowMs)) {
        noteUnconfirmedTick(hold, tick.state);
        return;
      }
      this.#finishHold(hold, tickPositionUs, nowMs);
    } else {
      // Шаг назад без нашей перемотки (новое поколение на старте) — не перемотка: просто
      // новая позиция; мелкое дрожание гасит `positionUs`.
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
      if (origin === "page") {
        // Стандарт: `play()` после конца ролика сначала перематывает на начало.
        restartedAtUs = this.seek("page_seek", 0, nowMs);
      } else {
        this.#ended = false;
      }
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
   * Перемотка плеером или страницей. Цель ограничена длительностью; перемотка на/за конец
   * завершится состоянием `ended`. Возвращает итоговую цель (её видит страница).
   */
  seek(kind: "player_seek" | "page_seek", targetUs: MediaTimeMicros, nowMs: number): MediaTimeMicros {
    const clampedUs = this.#clampToTimeline(targetUs);
    const durationUs = this.#readDurationUs();
    this.#startHold(
      {
        kind,
        targetUs: clampedUs,
        landsAtEnd: durationUs !== undefined && clampedUs >= durationUs,
        unconfirmedTicksLeft: MAX_UNCONFIRMED_TICKS,
        startedAtMs: nowMs,
        playerAcknowledgedNewSource: false,
      },
      nowMs,
    );
    return clampedUs;
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
    this.#hold = {
      kind: "source_reset",
      targetUs: 0,
      landsAtEnd: false,
      unconfirmedTicksLeft: MAX_UNCONFIRMED_TICKS,
      startedAtMs: nowMs,
      playerAcknowledgedNewSource: false,
    };
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
    this.#hold = {
      kind: "source_reset",
      targetUs,
      landsAtEnd: false,
      unconfirmedTicksLeft: MAX_UNCONFIRMED_TICKS,
      startedAtMs: nowMs,
      playerAcknowledgedNewSource: false,
    };
  }

  // -------------------------------------------------------------------------------------------
  // Внутреннее.
  // -------------------------------------------------------------------------------------------

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

/** Подтверждает ли тик прыжок, которого ждёт тень. */
function holdConfirmedBy(hold: PositionHold, tick: HoldTick, nowMs: number): boolean {
  // Команды плеера и его часы идут по одному каналу по порядку: тик после `seek` — уже новый.
  if (hold.kind === "player_seek") return true;
  if (Math.abs(tick.positionUs - hold.targetUs) <= PAGE_SEEK_CONFIRM_TOLERANCE_US) return true;
  if (hold.kind === "source_reset") return sourceResetConfirmedBy(hold, tick, nowMs);
  return hold.unconfirmedTicksLeft <= 0;
}

/**
 * Смена источника, тик далеко от 0: принять его, только если плеер уже признал новый источник
 * (эфир, где новое поколение начинается не с 0), или по страховке времени — но чужой `ended`
 * не принимается никогда: это конец СТАРОГО поколения (решение 146), а не нового ролика.
 */
function sourceResetConfirmedBy(hold: PositionHold, tick: HoldTick, nowMs: number): boolean {
  if (hold.playerAcknowledgedNewSource) return true;
  if (tick.state === "ended") return false;
  return nowMs - hold.startedAtMs >= SOURCE_RESET_GIVE_UP_MS;
}

/** Учесть неподтвердивший тик: счётчик перемотки и признак «плеер признал новый источник». */
function noteUnconfirmedTick(hold: PositionHold, state: ClockState): void {
  hold.unconfirmedTicksLeft -= 1;
  if (state === "waiting") hold.playerAcknowledgedNewSource = true;
}
