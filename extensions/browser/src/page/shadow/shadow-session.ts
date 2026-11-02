// Контроллер тени одного переданного элемента: связывает модель часов, настоящий элемент,
// события страницы и сообщения наружу.
//
// Кто чем владеет:
// - `ShadowTimeline` — что видит страница (позиция, пауза, перемотка, readyState) и какие
//   события ей положены; контроллер только исполняет её сигналы.
// - Контроллер — настоящий элемент (пауза, заглушка, подтягивание, возврат), обещания `play()`,
//   счётчики кадров, переводы «страница сделала X» в `PageEvent` наружу.
// - Оригиналы — единственный способ трогать настоящий элемент (страница могла подменить прототипы).

import type { HandoffEndCause, HandoffEndPosition, HandoffEndResume, PageEvent, ShadowCommand } from "../../shared/handoff-contract.ts";
import { MAX_PLAYBACK_RATE_PERMILLE, MIN_PLAYBACK_RATE_PERMILLE } from "../../protocol/limits.ts";
import type { MediaTimeMicros, PlaybackRatePermille } from "../../protocol/time.ts";
import type { MediaSessionGuard } from "../media-session-guard.ts";
import { type GateDecision, installEventGate, PLAYBACK_EVENT_TYPES, READINESS_EVENT_TYPES, SOURCE_RESET_EVENT_TYPE } from "./event-gate.ts";
import { EventDispatchQueue } from "./event-dispatch-queue.ts";
import { type FrameCallback, type FrameCountBaseline, ShadowFrameCallbacks, shadowTotalFrames } from "./frame-stats.ts";
import { shadowProblemReporter } from "./once-reporter.ts";
import { callOriginal, type ElementOriginals, readOriginal, writeOriginal } from "./originals.ts";
import { ensureShadowOverrides, type ShadowBehavior, type ShadowRegistry, type SourceProperty } from "./prototype-overrides.ts";
import { DetachedElementWatch } from "./detached-element-watch.ts";
import { RealElementFollower } from "./real-element-follower.ts";
import { ShadowTimeline, READY_STATE, type ReadyStateValue, type TimelineSignal } from "./shadow-timeline.ts";
import { classifySourceReset, type MediaSourceObjectUrlCheck } from "./source-reset-policy.ts";

/** Окружение тени (шов A1↔A2): всё внешнее подаётся снаружи, чтобы тесты шли на фейках. */
export interface ShadowEnvironment {
  readonly win: Window;
  readonly originals: ElementOriginals;
  readonly mediaSession: MediaSessionGuard;
  /**
   * `performance.now` (мс). Своих таймеров тень не заводит: подтягивание и проверки идут на тиках
   * часов плеера (таймеры скрытой вкладки Chromium будит раз в минуту).
   */
  readonly now: () => number;
  /**
   * Адрес — blob-адрес MediaSource этого документа (точный ответ реестра MAIN-мира, A1). Нужен,
   * чтобы отличить новое поколение на том же элементе от blob-адреса обычного файла.
   */
  readonly isMediaSourceObjectUrl: MediaSourceObjectUrlCheck;
}

/** Управление тенью для MAIN-мира. */
export interface ShadowSession {
  apply(command: ShadowCommand): void;
  /** Снять тень по команде `end_handoff`; шлёт `handoff_ended{returned}`. Повтор — ничего. */
  end(position: HandoffEndPosition, resume: HandoffEndResume): void;
}

const MICROS_PER_SECOND = 1_000_000;
/** Скорость в протоколе — тысячные доли (‰): 1000 = обычная, 1500 = 1,5×. */
const PERMILLE_PER_UNIT_RATE = 1_000;

/** Скорость элемента (1,5) → скорость протокола в ‰ (1500); `null` — вне диапазона протокола (0, 0,05×, 20×). */
function protocolRateOf(rate: number): PlaybackRatePermille | null {
  const permille = Math.round(rate * PERMILLE_PER_UNIT_RATE);
  if (!(permille >= MIN_PLAYBACK_RATE_PERMILLE && permille <= MAX_PLAYBACK_RATE_PERMILLE)) return null;
  return permille;
}
/** `HTMLMediaElement.NETWORK_EMPTY`: у элемента нет источника, «алгоритм загрузки» не шлёт `emptied`. */
const NETWORK_EMPTY = 0;
/**
 * При возврате настоящий элемент переставляется, только если он дальше 1 мс от цели: лишняя
 * перестановка — это видимые странице `seeking`/`seeked` и декод кадров без нужды.
 */
const SAME_POSITION_TOLERANCE_US = 1_000;

const PLAY_INTERRUPTED_MESSAGE = "The play() request was interrupted.";

/** Ожидающее обещание `play()` страницы. */
interface PendingPlay {
  readonly resolve: () => void;
  readonly reject: (reason: unknown) => void;
}

/** Ставит элемент в тень. `emit` — события наружу (page_seek, player_seek_applied, page_playback, handoff_ended). */
export function startShadow(element: HTMLMediaElement, emit: (event: PageEvent) => void, env: ShadowEnvironment): ShadowSession {
  const registry = ensureShadowOverrides(env.originals);
  if (registry.isShadowed(element)) throw new Error("shadow: element is already shadowed");
  const controller = new ShadowController(element, emit, env, registry);
  return {
    apply: (command) => controller.apply(command),
    end: (position, resume) => controller.end(position, resume),
  };
}

class ShadowController implements ShadowBehavior {
  readonly #element: HTMLMediaElement;
  readonly #emitOutward: (event: PageEvent) => void;
  readonly #env: ShadowEnvironment;
  readonly #originals: ElementOriginals;
  readonly #registry: ShadowRegistry;
  readonly #queue: EventDispatchQueue;
  readonly #timeline: ShadowTimeline;
  readonly #frameCallbacks = new ShadowFrameCallbacks();
  readonly #frameBaseline: FrameCountBaseline;
  /** Прототип настоящего `VideoPlaybackQuality`: поддельный отчёт проходит `instanceof`. */
  readonly #qualityPrototype: object;
  readonly #follower: RealElementFollower;
  readonly #detachedWatch = new DetachedElementWatch();
  readonly #disposeGate: () => void;
  #pendingPlays: PendingPlay[] = [];
  /** `muted`, как его видит страница; настоящий элемент на время тени заглушен. */
  #pageMuted: boolean;
  /** Сколько `emptied` уже обработано синхронно в `load()`/`src=` и ещё придёт от браузера. */
  #expectedEmptiedEvents = 0;
  /** Наша перестановка настоящего элемента ещё не «устоялась»: его `canplay*` — не для страницы. */
  #realRepositionInFlight = false;
  #active = true;

  constructor(element: HTMLMediaElement, emit: (event: PageEvent) => void, env: ShadowEnvironment, registry: ShadowRegistry) {
    this.#element = element;
    this.#emitOutward = emit;
    this.#env = env;
    this.#originals = env.originals;
    this.#registry = registry;
    this.#queue = new EventDispatchQueue(element, env.originals);

    const media = this.#originals.media;
    const nowMs = env.now();
    const realPaused = readOriginal(media.paused, element) === true;
    const realReadyState = Number(readOriginal(media.readyState, element));
    this.#pageMuted = readOriginal(media.muted, element) === true;
    const baselineQuality = this.#readRealQuality();
    this.#frameBaseline = baselineQuality.baseline;
    this.#qualityPrototype = baselineQuality.prototype;
    this.#timeline = new ShadowTimeline(
      {
        positionUs: secondsToMicros(Number(readOriginal(media.currentTime, element))),
        paused: realPaused,
        pageRate: Number(readOriginal(media.playbackRate, element)),
        // Тень хранит только 1/2/4: «будущих данных» без «достаточно» у неё не бывает.
        readyState: realReadyState >= READY_STATE.HAVE_FUTURE_DATA ? READY_STATE.HAVE_ENOUGH_DATA : (realReadyState as ReadyStateValue),
        nowMs,
      },
      () => this.#durationUs(),
      (signal) => this.#onTimelineSignal(signal),
    );
    this.#follower = new RealElementFollower(
      {
        canReposition: () => !this.#timeline.seeking && Number(readOriginal(media.readyState, element)) > READY_STATE.HAVE_NOTHING,
        shadowPositionUs: () => this.#timeline.positionUs(env.now()),
        realPositionUs: () => secondsToMicros(Number(readOriginal(media.currentTime, element))),
        repositionReal: (positionUs) => this.#repositionReal(positionUs),
      },
      nowMs,
    );

    // Порядок важен: сначала тень в таблице и шлагбаум, потом наши действия над настоящим
    // элементом — их доверенные события (`pause`, `volumechange`) придут позже и будут погашены.
    // Если шлагбаум не встал, регистрация откатывается: иначе переопределения прототипа
    // обслуживали бы элемент мёртвым контроллером. `startShadow` тогда бросает — передача честно
    // кончается у контроллера MAIN-мира.
    registry.attach(element, this);
    try {
      this.#disposeGate = installEventGate(element, env.win, this.#originals, (type) => this.#onTrustedRealEvent(type));
    } catch (problem) {
      registry.detach(element);
      throw problem;
    }
    this.#attempt("start: pause and mute real element", () => {
      if (!realPaused) callOriginal(media.pause, element);
      writeOriginal(media.muted, element, true);
    });
    this.#attempt("start: suspend mediaSession", () => env.mediaSession.suspend());
  }

  // -------------------------------------------------------------------------------------------
  // Команды плеера (через фон и изолированный мир).
  // -------------------------------------------------------------------------------------------

  apply(command: ShadowCommand): void {
    if (!this.#active) return;
    this.#attempt(`apply ${command.kind}`, () => {
      const nowMs = this.#env.now();
      switch (command.kind) {
        case "clock":
          if (this.#elementLeftDocument(nowMs)) {
            this.#releaseToPage();
            return;
          }
          this.#timeline.onClock(command, nowMs);
          this.#follower.onClockTick(nowMs);
          return;
        case "play":
          this.#timeline.play("player", nowMs);
          return;
        case "pause":
          this.#timeline.pause(nowMs);
          return;
        case "set_rate":
          this.#timeline.setRateFromPlayer(command.rate, nowMs);
          return;
        case "seek": {
          const targetUs = this.#timeline.seek("player_seek", command.target_us, nowMs);
          this.#emit({ kind: "player_seek_applied", seek_id: command.seek_id, target_us: targetUs });
          return;
        }
      }
    });
  }

  end(position: HandoffEndPosition, resume: HandoffEndResume): void {
    if (!this.#active) return;
    const nowMs = this.#env.now();
    const targetUs = position.kind === "at" ? position.position_us : this.#timeline.positionUs(nowMs);
    const pageSawPlaying = !this.#timeline.paused;
    const resumePlaying = resume.kind === "exactly" ? resume.resume === "playing" : pageSawPlaying;
    this.#detach();
    this.#attempt("end: restore real element", () => this.#returnRealElement(targetUs, resumePlaying, pageSawPlaying));
    this.#finish("returned");
  }

  // -------------------------------------------------------------------------------------------
  // ShadowBehavior: что видит и делает страница.
  // -------------------------------------------------------------------------------------------

  currentTime(): number {
    return this.#timeline.positionUs(this.#env.now()) / MICROS_PER_SECOND;
  }

  setCurrentTime(value: unknown): void {
    this.#pageSeek(this.#toFiniteSeconds(value, "set the 'currentTime' property"));
  }

  fastSeek(value: unknown): void {
    // У тени нет «быстрой неточной» перемотки: она всегда точная, как `currentTime`.
    this.#pageSeek(this.#toFiniteSeconds(value, "execute 'fastSeek'"));
  }

  paused(): boolean {
    return this.#timeline.paused;
  }

  ended(): boolean {
    return this.#timeline.ended;
  }

  seeking(): boolean {
    return this.#timeline.seeking;
  }

  playbackRate(): number {
    return this.#timeline.pageRate;
  }

  setPlaybackRate(value: unknown): void {
    const media = this.#originals.media;
    // Проверку диапазона делает браузер: оригинальный сеттер на стоящем настоящем элементе
    // бросит ровно ту ошибку, что и без нас (его `ratechange` погасит шлагбаум).
    writeOriginal(media.playbackRate, this.#element, value);
    const rate = Number(readOriginal(media.playbackRate, this.#element));
    this.#attempt("page playbackRate", () => {
      if (!this.#timeline.setPageRate(rate)) return;
      const ratePermille = protocolRateOf(rate);
      // Скорость 0 (Chromium её разрешает) протокол передать не может: плеер остаётся на своей.
      if (ratePermille === null) return;
      this.#emit({ kind: "page_playback", request: { kind: "set_rate", rate: ratePermille } });
    });
  }

  readyState(): number {
    // Нет метаданных у настоящего элемента — нет их и у тени: данные потока общие с браузером.
    const realReadyState = Number(readOriginal(this.#originals.media.readyState, this.#element));
    return realReadyState === READY_STATE.HAVE_NOTHING ? READY_STATE.HAVE_NOTHING : this.#timeline.readyState;
  }

  muted(): boolean {
    return this.#pageMuted;
  }

  setMuted(value: unknown): void {
    const muted = Boolean(value);
    if (muted === this.#pageMuted) return;
    this.#pageMuted = muted;
    this.#queue.enqueueEvent("volumechange");
  }

  volume(): number {
    // Громкость страницы хранит сам настоящий элемент: он заглушен, громкость не слышна.
    return Number(readOriginal(this.#originals.media.volume, this.#element));
  }

  setVolume(value: unknown): void {
    const media = this.#originals.media;
    const before = Number(readOriginal(media.volume, this.#element));
    // Проверка диапазона (IndexSizeError) — у браузера.
    writeOriginal(media.volume, this.#element, value);
    if (Number(readOriginal(media.volume, this.#element)) !== before) this.#queue.enqueueEvent("volumechange");
  }

  play(): Promise<void> {
    const PagePromise = this.#originals.PromiseConstructor;
    let pending: PendingPlay | undefined;
    const promise = new PagePromise<void>((resolve, reject) => {
      pending = { resolve: () => resolve(), reject };
    });
    if (pending !== undefined) this.#pendingPlays.push(pending);
    this.#attempt("page play", () => {
      const result = this.#timeline.play("page", this.#env.now());
      if (result.restartedAtUs !== undefined) this.#emit({ kind: "page_seek", target_us: result.restartedAtUs });
      if (result.unpaused) this.#emit({ kind: "page_playback", request: { kind: "play" } });
    });
    return promise;
  }

  pause(): void {
    this.#attempt("page pause", () => {
      if (this.#timeline.pause(this.#env.now())) this.#emit({ kind: "page_playback", request: { kind: "pause" } });
    });
  }

  load(): void {
    const willFireEmptied = this.#realNetworkState() !== NETWORK_EMPTY;
    callOriginal(this.#originals.media.load, this.#element);
    this.#afterLoadAlgorithm(willFireEmptied);
  }

  setSource(property: SourceProperty, value: unknown): void {
    const accessor = property === "src" ? this.#originals.media.src : this.#originals.media.srcObject;
    // Без `srcObject` в браузере переопределение для него и не ставится; на всякий — честный отказ.
    if (accessor === undefined) return;
    const willFireEmptied = this.#realNetworkState() !== NETWORK_EMPTY;
    writeOriginal(accessor, this.#element, value);
    this.#afterLoadAlgorithm(willFireEmptied);
  }

  getVideoPlaybackQuality(): unknown {
    const nowMs = this.#env.now();
    const quality = Object.create(this.#qualityPrototype) as object;
    // Собственные поля закрывают геттеры прототипа (они бросили бы на «не настоящем» объекте).
    Object.defineProperties(quality, {
      creationTime: { value: nowMs, enumerable: true },
      totalVideoFrames: { value: this.#totalFrames(nowMs), enumerable: true },
      droppedVideoFrames: { value: this.#frameBaseline.droppedFrames, enumerable: true },
      corruptedVideoFrames: { value: this.#frameBaseline.corruptedFrames, enumerable: true },
    });
    return quality;
  }

  requestVideoFrameCallback(callback: unknown): number {
    if (typeof callback !== "function") {
      throw new this.#originals.TypeErrorConstructor(
        "Failed to execute 'requestVideoFrameCallback' on 'HTMLVideoElement': parameter 1 is not of type 'Function'.",
      );
    }
    return this.#frameCallbacks.request(callback as FrameCallback);
  }

  cancelVideoFrameCallback(handle: unknown): void {
    if (this.#frameCallbacks.cancel(Number(handle))) return;
    // Номер не наш: его выдал браузер настоящему элементу до передачи.
    const original = this.#originals.video.cancelVideoFrameCallback;
    if (original !== undefined) callOriginal(original, this.#element, [handle]);
  }

  webkitDecodedFrameCount(): number {
    return this.#totalFrames(this.#env.now());
  }

  webkitDroppedFrameCount(): number {
    return this.#frameBaseline.droppedFrames;
  }

  // -------------------------------------------------------------------------------------------
  // Внутреннее.
  // -------------------------------------------------------------------------------------------

  #pageSeek(seconds: number): void {
    this.#attempt("page seek", () => {
      const targetUs = this.#timeline.seek("page_seek", secondsToMicros(seconds), this.#env.now());
      this.#emit({ kind: "page_seek", target_us: targetUs });
    });
  }

  /** Сигналы модели → DOM-события и исходы обещаний (в одном порядке, одной микрозадачей). */
  #onTimelineSignal(signal: TimelineSignal): void {
    switch (signal.kind) {
      case "event":
        this.#queue.enqueueEvent(signal.type);
        return;
      case "settle_play_promises":
        this.#settlePendingPlays(signal.outcome);
        return;
      case "frame_presented":
        this.#queue.enqueueStep(() => this.#presentFrame());
        return;
    }
  }

  /** Как «take pending play promises» стандарта: забрать ожидающие сейчас, исполнить по очереди событий. */
  #settlePendingPlays(outcome: "resolve" | "reject"): void {
    if (this.#pendingPlays.length === 0) return;
    const taken = this.#pendingPlays;
    this.#pendingPlays = [];
    this.#queue.enqueueStep(() => {
      for (const pending of taken) {
        if (outcome === "resolve") pending.resolve();
        else pending.reject(new this.#originals.DOMExceptionConstructor(PLAY_INTERRUPTED_MESSAGE, "AbortError"));
      }
    });
  }

  #presentFrame(): void {
    const nowMs = this.#env.now();
    const video = this.#originals.video;
    this.#frameCallbacks.presentFrame(
      nowMs,
      {
        presentationTime: nowMs,
        expectedDisplayTime: nowMs,
        width: video.videoWidth === undefined ? 0 : Number(readOriginal(video.videoWidth, this.#element)),
        height: video.videoHeight === undefined ? 0 : Number(readOriginal(video.videoHeight, this.#element)),
        mediaTime: this.#timeline.positionUs(nowMs) / MICROS_PER_SECOND,
        presentedFrames: this.#totalFrames(nowMs),
        processingDuration: 0,
      },
      // Как у браузера: исключение колбэка страницы уходит в её обработчик ошибок, не к нам.
      (problem) =>
        this.#originals.queueMicrotask(() => {
          throw problem;
        }),
    );
  }

  /** Доверенное событие настоящего элемента: гасить или пропустить странице. */
  #onTrustedRealEvent(type: string): GateDecision {
    if (type === SOURCE_RESET_EVENT_TYPE) {
      if (this.#expectedEmptiedEvents > 0) {
        // Уже обработано синхронно в `load()`/`src=`: это просто запоздавшее событие браузера.
        this.#expectedEmptiedEvents -= 1;
      } else {
        // Источник сменили мимо наших переопределений (`setAttribute("src")`, `<source>`).
        this.#attempt("emptied", () => this.#handleSourceReset());
      }
      return "pass";
    }
    if (READINESS_EVENT_TYPES.includes(type)) {
      if (!this.#realRepositionInFlight) return "pass";
      if (type === "canplaythrough") this.#realRepositionInFlight = false;
      return "suppress";
    }
    if (type === "play") {
      // Настоящий элемент кто-то запустил мимо тени (оригинал из другого фрейма) — снова пауза.
      this.#attempt("re-pause real element", () => callOriginal(this.#originals.media.pause, this.#element));
    }
    return PLAYBACK_EVENT_TYPES.includes(type) ? "suppress" : "pass";
  }

  /** «Алгоритм загрузки» уже прошёл в оригинале синхронно: тень повторяет его сразу. */
  #afterLoadAlgorithm(willFireEmptied: boolean): void {
    if (willFireEmptied) this.#expectedEmptiedEvents += 1;
    this.#attempt("source reset", () => this.#handleSourceReset());
  }

  #handleSourceReset(): void {
    if (!this.#active) return;
    if (classifySourceReset(this.#element, this.#originals, this.#env.isMediaSourceObjectUrl) === "released") {
      this.#releaseToPage();
      return;
    }
    this.#realRepositionInFlight = false;
    // Скорость после загрузки = `defaultPlaybackRate`; её уже выставил браузер настоящему элементу.
    const realRate = Number(readOriginal(this.#originals.media.playbackRate, this.#element));
    if (this.#timeline.resetForNewSource(realRate, this.#env.now())) {
      const ratePermille = protocolRateOf(realRate);
      if (ratePermille !== null) this.#emit({ kind: "page_playback", request: { kind: "set_rate", rate: ratePermille } });
    }
  }

  /**
   * Страница бросила элемент (убрала источник или поставила не-MSE): тень уходит, элемент снова
   * настоящий. Стандарт уже поставил его на паузу без события `pause` и отверг ожидающие
   * `play()` — тень делает то же со своими обещаниями.
   */
  #releaseToPage(): void {
    this.#detach();
    this.#settlePendingPlays("reject");
    this.#attempt("release: restore muted", () => writeOriginal(this.#originals.media.muted, this.#element, this.#pageMuted));
    this.#finish("element_released");
  }

  /** Элемент вне документа дольше допуска (проверка оригинальным `isConnected` на тике). */
  #elementLeftDocument(nowMs: number): boolean {
    const accessor = this.#originals.isConnected;
    if (accessor === undefined) return false;
    return this.#detachedWatch.onClockTick(readOriginal(accessor, this.#element) === true, nowMs);
  }

  /** Убрать тень с элемента: переопределения и шлагбаум больше его не касаются. */
  #detach(): void {
    this.#active = false;
    this.#registry.detach(this.#element);
    this.#disposeGate();
  }

  /** Возврат: громкость, скорость, позиция, игра/пауза настоящего элемента. */
  #returnRealElement(targetUs: MediaTimeMicros, resumePlaying: boolean, pageSawPlaying: boolean): void {
    const media = this.#originals.media;
    const element = this.#element;
    // Находка 00c (Ashdi): вернуть именно `muted` страницы, а не просто снять наш.
    writeOriginal(media.muted, element, this.#pageMuted);
    if (Number(readOriginal(media.playbackRate, element)) !== this.#timeline.pageRate) {
      writeOriginal(media.playbackRate, element, this.#timeline.pageRate);
    }
    const realUs = secondsToMicros(Number(readOriginal(media.currentTime, element)));
    if (Math.abs(realUs - targetUs) > SAME_POSITION_TOLERANCE_US) {
      writeOriginal(media.currentTime, element, targetUs / MICROS_PER_SECOND);
    }
    this.#handOverFrameCallbacks();
    if (resumePlaying) {
      this.#resumeRealPlayback(pageSawPlaying);
      return;
    }
    callOriginal(media.pause, element);
    this.#settlePendingPlays("reject");
    // Страница видела «играет», а итог — пауза: настоящий элемент уже стоял и сам `pause` не пошлёт.
    if (pageSawPlaying) {
      this.#queue.enqueueEvent("timeupdate");
      this.#queue.enqueueEvent("pause");
    }
  }

  /** Возврат с игрой: ожидающие `play()` страницы исполняются исходом настоящего `play()`. */
  #resumeRealPlayback(pageSawPlaying: boolean): void {
    const waiting = this.#pendingPlays;
    this.#pendingPlays = [];
    const realPlay = callOriginal(this.#originals.media.play, this.#element) as Promise<void> | undefined;
    if (realPlay === undefined || typeof realPlay.then !== "function") {
      for (const pending of waiting) pending.resolve();
      return;
    }
    realPlay.then(
      () => {
        for (const pending of waiting) pending.resolve();
      },
      (problem: unknown) => {
        // Браузер не дал играть (политика автозапуска): страница должна узнать, что стоит.
        shadowProblemReporter.report("end: real play() rejected", problem);
        for (const pending of waiting) pending.reject(problem);
        if (pageSawPlaying) this.#queue.enqueueEvent("pause");
      },
    );
  }

  /** Ожидающие `requestVideoFrameCallback` уходят настоящему элементу — цикл страницы не рвётся. */
  #handOverFrameCallbacks(): void {
    const original = this.#originals.video.requestVideoFrameCallback;
    if (original === undefined) return;
    this.#frameCallbacks.drainInto((callback) => callOriginal(original, this.#element, [callback]));
  }

  #finish(cause: HandoffEndCause): void {
    this.#attempt("restore mediaSession", () => this.#env.mediaSession.restore());
    this.#emit({ kind: "handoff_ended", cause });
  }

  #repositionReal(positionUs: MediaTimeMicros): void {
    this.#realRepositionInFlight = true;
    writeOriginal(this.#originals.media.currentTime, this.#element, positionUs / MICROS_PER_SECOND);
  }

  #readRealQuality(): { readonly baseline: FrameCountBaseline; readonly prototype: object } {
    const getQuality = this.#originals.video.getVideoPlaybackQuality;
    if (getQuality !== undefined) {
      const quality = callOriginal(getQuality, this.#element) as VideoPlaybackQuality;
      return {
        baseline: {
          totalFrames: quality.totalVideoFrames,
          droppedFrames: quality.droppedVideoFrames,
          corruptedFrames: quality.corruptedVideoFrames,
        },
        prototype: Object.getPrototypeOf(quality) as object,
      };
    }
    const decoded = this.#originals.video.webkitDecodedFrameCount;
    const dropped = this.#originals.video.webkitDroppedFrameCount;
    return {
      baseline: {
        totalFrames: decoded === undefined ? 0 : Number(readOriginal(decoded, this.#element)),
        droppedFrames: dropped === undefined ? 0 : Number(readOriginal(dropped, this.#element)),
        corruptedFrames: 0,
      },
      prototype: Object.prototype,
    };
  }

  #totalFrames(nowMs: number): number {
    return shadowTotalFrames(this.#frameBaseline, this.#timeline.playedUs(nowMs));
  }

  #durationUs(): MediaTimeMicros | undefined {
    const seconds = Number(readOriginal(this.#originals.media.duration, this.#element));
    return Number.isFinite(seconds) && seconds >= 0 ? Math.floor(seconds * MICROS_PER_SECOND) : undefined;
  }

  #realNetworkState(): number {
    return Number(readOriginal(this.#originals.media.networkState, this.#element));
  }

  /** Как WebIDL `double` (не `unrestricted`): NaN и бесконечность — TypeError мира страницы. */
  #toFiniteSeconds(value: unknown, operation: string): number {
    const seconds = Number(value);
    if (!Number.isFinite(seconds)) {
      throw new this.#originals.TypeErrorConstructor(
        `Failed to ${operation} on 'HTMLMediaElement': The provided double value is non-finite.`,
      );
    }
    return seconds;
  }

  #emit(event: PageEvent): void {
    this.#attempt(`emit ${event.kind}`, () => this.#emitOutward(event));
  }

  /**
   * Наша внутренняя ошибка не должна долететь до страницы (хук не бросает в страницу): одна
   * строка отладки за жизнь. Ошибки ПРОВЕРКИ аргументов страницы бросаются до `#attempt`.
   */
  #attempt(context: string, action: () => void): void {
    try {
      action();
    } catch (problem) {
      shadowProblemReporter.report(`shadow: ${context}`, problem);
    }
  }
}

function secondsToMicros(seconds: number): number {
  return Number.isFinite(seconds) ? Math.round(seconds * MICROS_PER_SECOND) : 0;
}
