// Контроллер тени одного переданного элемента: связывает модель часов, настоящий элемент,
// события страницы и сообщения наружу.
//
// Кто чем владеет:
// - `ShadowTimeline` — что видит страница (позиция, пауза, перемотка, readyState) и какие
//   события ей положены; контроллер только исполняет её сигналы.
// - Контроллер — настоящий элемент (пауза, заглушка, подтягивание, возврат), обещания `play()`,
//   счётчики кадров, переводы «страница сделала X» в `PageEvent` наружу.
// - Оригиналы — единственный способ трогать настоящий элемент (страница могла подменить прототипы).
// - Тень файла (сессия 09b): всё, что она делает иначе, — `FileShadowLifecycle` (снимок файла,
//   снятие и возврат источника); контроллер зовёт её одной строкой в своих точках.

import type { HandoffEndCause, HandoffEndPosition, HandoffEndResume, PageEvent, ShadowCommand } from "../../shared/handoff-contract.ts";
import { MAX_PLAYBACK_RATE_PERMILLE, MIN_PLAYBACK_RATE_PERMILLE } from "../../protocol/limits.ts";
import type { MediaTimeMicros, PlaybackRatePermille } from "../../protocol/time.ts";
import type { MediaSessionGuard } from "../media-session-guard.ts";
import type { ShadowSourceKind } from "../handoff-seam.ts";
import type { TimeoutScheduler } from "./file-return.ts";
import { FileShadowLifecycle, interruptFileReturn, type FileShadowHost, type ReturnPlan } from "./file-shadow-lifecycle.ts";
import { PageMutedState } from "./page-muted.ts";
import { FileSourceMode, type ShadowSourceView } from "./file-source-mode.ts";
import { type GateDecision, installEventGate, PLAYBACK_EVENT_TYPES, READINESS_EVENT_TYPES, SOURCE_RESET_EVENT_TYPE } from "./event-gate.ts";
import { EventDispatchQueue } from "./event-dispatch-queue.ts";
import { type FrameCallback, type FrameCountBaseline, readRealFrameQuality, ShadowFrameCallbacks, shadowTotalFrames } from "./frame-stats.ts";
import { shadowProblemReporter } from "./once-reporter.ts";
import { callOriginal, type ElementOriginals, readOriginal, writeOriginal } from "./originals.ts";
import { installShadowOverrides, type ShadowBehavior, type ShadowRegistry, type SourceProperty } from "./prototype-overrides.ts";
import { DetachedElementWatch } from "./detached-element-watch.ts";
import { EmptySourceWatch } from "./empty-source-watch.ts";
import { RealElementFollower } from "./real-element-follower.ts";
import { ShadowPulse } from "./shadow-pulse.ts";
import { ShadowTimeline, READY_STATE, type ReadyStateValue, type TimelineSignal } from "./shadow-timeline.ts";
import { classifyElementSource, type MediaSourceObjectUrlCheck, type SourceVerdict } from "./source-reset-policy.ts";

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
  /** Таймер окна — только страховка возврата файла (`FILE_RETURN_TIMEOUT_MS`). */
  readonly scheduleTimeout: TimeoutScheduler;
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

/**
 * Что «алгоритм загрузки» браузера сделал с настоящим элементом. Элемент в NETWORK_EMPTY браузер
 * не сбрасывает (нет `emptied`, пауза и ожидающие `play()` не трогаются) — тень тоже не должна:
 * она уже сбросилась, когда источник пропал.
 */
type LoadAlgorithmEffect = "element_reset" | "element_was_empty";

/** Ожидающее обещание `play()` страницы. */
interface PendingPlay {
  readonly resolve: () => void;
  readonly reject: (reason: unknown) => void;
}

/**
 * Ставит элемент в тень. `emit` — события наружу (page_seek, player_seek_applied, page_playback,
 * handoff_ended). `source` — что передаётся: MSE или файл `<video src>` (handoff-seam.ts).
 */
export function startShadow(
  element: HTMLMediaElement,
  emit: (event: PageEvent) => void,
  env: ShadowEnvironment,
  source: ShadowSourceKind,
): ShadowSession {
  const registry = installShadowOverrides(env.originals);
  // Прежняя тень файла ещё возвращает элемент (находка 6 ревью 09b): закончить её возврат сейчас.
  const interruptedFile = interruptFileReturn(element);
  if (registry.isShadowed(element)) throw new Error("shadow: element is already shadowed");
  const controller = new ShadowController(element, emit, env, registry, source, interruptedFile);
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
  readonly #emptySourceWatch = new EmptySourceWatch();
  readonly #pulse = new ShadowPulse();
  readonly #disposeGate: () => void;
  /** Тень файла (`null` — тень MSE): снимок файла, снятый источник и возврат. */
  readonly #file: FileShadowLifecycle | null;
  #pendingPlays: PendingPlay[] = [];
  /** `muted`, как его видит страница; настоящий элемент на время тени заглушен (page-muted.ts). */
  readonly #muted: PageMutedState;
  /** Сколько `emptied` уже обработано синхронно в `load()`/`src=` и ещё придёт от браузера. */
  #expectedEmptiedEvents = 0;
  /** Наша перестановка настоящего элемента ещё не «устоялась»: его `canplay*` — не для страницы. */
  #realRepositionInFlight = false;
  #active = true;

  constructor(
    element: HTMLMediaElement,
    emit: (event: PageEvent) => void,
    env: ShadowEnvironment,
    registry: ShadowRegistry,
    source: ShadowSourceKind,
    /** Снимок файла прежней тени, чей возврат прервала эта передача (`null` — не было). */
    interruptedFile: FileSourceMode | null,
  ) {
    this.#element = element;
    this.#emitOutward = emit;
    this.#env = env;
    this.#originals = env.originals;
    this.#registry = registry;
    this.#queue = new EventDispatchQueue(element, env.originals);
    // Снимок файла — до любых наших действий с элементом (пауза, шлагбаум, снятие источника).
    this.#file = source === "file" ? new FileShadowLifecycle(this.#fileSnapshot(interruptedFile), this.#fileHost()) : null;

    const media = this.#originals.media;
    const nowMs = env.now();
    const realPaused = readOriginal(media.paused, element) === true;
    const realReadyState = Number(readOriginal(media.readyState, element));
    this.#muted = new PageMutedState(element, env.originals);
    const baselineQuality = readRealFrameQuality(element, this.#originals);
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
        // У файла настоящий элемент без файла: переставлять нечего (и буфера, который чистится, нет).
        canReposition: () =>
          this.#file === null && !this.#timeline.seeking && Number(readOriginal(media.readyState, element)) > READY_STATE.HAVE_NOTHING,
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
      this.#disposeGate = installEventGate(
        element,
        env.win,
        this.#originals,
        (type) => this.#onTrustedRealEvent(type),
        this.#file === null ? "playback_events" : "all_media_events",
      );
    } catch (problem) {
      registry.detach(element);
      throw problem;
    }
    this.#attempt("start: pause and mute real element", () => {
      if (!realPaused) callOriginal(media.pause, element);
      this.#muted.muteRealElement();
    });
    this.#attempt("start: suspend mediaSession", () => env.mediaSession.suspend());
    // Решение 186: файл снимается, когда шлагбаум уже гасит события этой записи.
    this.#file?.start();
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
          if (this.#pageTookElementBack(nowMs)) {
            this.#releaseToPage();
            return;
          }
          this.#timeline.onClock(command, nowMs);
          this.#follower.onClockTick(nowMs);
          // Пульс — последним: если отправка оборвёт передачу, после него тень уже ничего не делает.
          if (this.#pulse.onClockTick(nowMs)) this.#emit({ kind: "shadow_alive" });
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
    if (this.#file !== null) {
      this.#file.end({ targetUs, resumePlaying, pageSawPlaying });
      return;
    }
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
    // У файла настоящий элемент без файла: готовность — только модели тени.
    if (this.#file !== null) return this.#timeline.readyState;
    // Нет метаданных у настоящего элемента — нет их и у тени: данные потока общие с браузером.
    const realReadyState = Number(readOriginal(this.#originals.media.readyState, this.#element));
    return realReadyState === READY_STATE.HAVE_NOTHING ? READY_STATE.HAVE_NOTHING : this.#timeline.readyState;
  }

  muted(): boolean {
    return this.#muted.pageMuted;
  }

  setMuted(value: unknown): void {
    if (this.#muted.setFromPage(Boolean(value)) === "changed") this.#queue.enqueueEvent("volumechange");
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
    if (this.#file !== null) {
      this.#file.pageWritesSource({ kind: "load" });
      return;
    }
    const willFireEmptied = this.#realNetworkState() !== NETWORK_EMPTY;
    callOriginal(this.#originals.media.load, this.#element);
    this.#afterLoadAlgorithm(willFireEmptied);
  }

  setSource(property: SourceProperty, value: unknown): void {
    const accessor = property === "src" ? this.#originals.media.src : this.#originals.media.srcObject;
    // Без `srcObject` в браузере переопределение для него и не ставится; на всякий — честный отказ.
    if (accessor === undefined) return;
    if (this.#file !== null) {
      this.#file.pageWritesSource({ kind: "set", property, value });
      return;
    }
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

  sourceView(): ShadowSourceView | null {
    return this.#file?.view ?? null;
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
        width: this.#file?.view.videoWidth() ?? (video.videoWidth === undefined ? 0 : Number(readOriginal(video.videoWidth, this.#element))),
        height: this.#file?.view.videoHeight() ?? (video.videoHeight === undefined ? 0 : Number(readOriginal(video.videoHeight, this.#element))),
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
    if (this.#file !== null) return this.#file.onTrustedEvent(type);
    if (type === SOURCE_RESET_EVENT_TYPE) {
      if (this.#expectedEmptiedEvents > 0) {
        // Уже обработано синхронно в `load()`/`src=`: это просто запоздавшее событие браузера.
        this.#expectedEmptiedEvents -= 1;
      } else {
        // Источник сменили мимо наших переопределений (`setAttribute("src")`, `<source>`).
        this.#attempt("emptied", () => this.#handleSourceReset("element_reset"));
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
    const effect: LoadAlgorithmEffect = willFireEmptied ? "element_reset" : "element_was_empty";
    this.#attempt("source reset", () => this.#handleSourceReset(effect));
  }

  /**
   * Страница запустила «алгоритм загрузки». Инвариант тени: элемент без источника — пауза решения
   * страницы, а не конец (Д-1); конец — только чужой источник или пустота дольше допуска.
   */
  #handleSourceReset(effect: LoadAlgorithmEffect): void {
    if (!this.#active) return;
    const nowMs = this.#env.now();
    switch (this.#classifySource()) {
      case "not_media_source":
        this.#releaseToPage();
        return;
      case "no_source":
        this.#emptySourceWatch.sourceRemoved(nowMs);
        break;
      case "media_source":
        this.#emptySourceWatch.mediaSourceArrived();
        break;
    }
    this.#repeatLoadAlgorithm(effect, nowMs);
  }

  /** Тень повторяет то, что браузер сделал с настоящим элементом: сброс часов и скорость. */
  #repeatLoadAlgorithm(effect: LoadAlgorithmEffect, nowMs: number): void {
    this.#realRepositionInFlight = false;
    // Скорость после загрузки = `defaultPlaybackRate`; её уже выставил браузер настоящему элементу.
    const realRate = Number(readOriginal(this.#originals.media.playbackRate, this.#element));
    const rateChanged = effect === "element_reset" ? this.#timeline.resetForNewSource(realRate, nowMs) : this.#timeline.setPageRate(realRate);
    if (!rateChanged) return;
    const ratePermille = protocolRateOf(realRate);
    if (ratePermille !== null) this.#emit({ kind: "page_playback", request: { kind: "set_rate", rate: ratePermille } });
  }

  #classifySource(): SourceVerdict {
    return classifyElementSource(this.#element, this.#originals, this.#env.isMediaSourceObjectUrl);
  }

  /**
   * Страница бросила элемент (убрала источник или поставила не-MSE): тень уходит, элемент снова
   * настоящий. Стандарт уже поставил его на паузу без события `pause` и отверг ожидающие
   * `play()` — тень делает то же со своими обещаниями.
   */
  #releaseToPage(): void {
    this.#file?.beforeRelease();
    this.#detach();
    this.#settlePendingPlays("reject");
    this.#finish("element_released");
  }

  /**
   * Тик часов: страница забрала элемент — вынула его из документа дольше допуска, оставила без
   * источника дольше допуска или (пока ждали) поставила чужой источник мимо наших переопределений.
   */
  #pageTookElementBack(nowMs: number): boolean {
    if (this.#elementLeftDocument(nowMs)) return true;
    return this.#emptySourceWatch.onClockTick(() => this.#classifySource(), nowMs) === "release_to_page";
  }

  /** Элемент вне документа дольше допуска (проверка оригинальным `isConnected` на тике). */
  #elementLeftDocument(nowMs: number): boolean {
    const accessor = this.#originals.isConnected;
    if (accessor === undefined) return false;
    return this.#detachedWatch.onClockTick(readOriginal(accessor, this.#element) === true, nowMs);
  }

  /** Снимок файла: с элемента или (новая передача посреди возврата) у прежней тени. */
  #fileSnapshot(interruptedFile: FileSourceMode | null): FileSourceMode {
    const originals = this.#originals;
    return interruptedFile === null ? FileSourceMode.capture(this.#element, originals) : FileSourceMode.inheriting(this.#element, originals, interruptedFile);
  }

  /** Что жизни тени файла можно делать с контроллером (её шов, file-shadow-lifecycle.ts). */
  #fileHost(): FileShadowHost {
    return {
      element: this.#element,
      originals: this.#originals,
      scheduleTimeout: this.#env.scheduleTimeout,
      isActive: () => this.#active,
      stopCommands: () => {
        this.#active = false;
      },
      releaseToPage: () => this.#releaseToPage(),
      detach: () => this.#detach(),
      reportReturned: () => this.#finish("returned"),
      returnRealElement: (plan: ReturnPlan) => this.#returnRealElement(plan.targetUs, plan.resumePlaying, plan.pageSawPlaying),
      handOverWithoutPosition: () => {
        this.#settlePendingPlays("reject");
        this.#handOverFrameCallbacks();
      },
      attempt: (context, action) => this.#attempt(context, action),
    };
  }

  /**
   * Убрать тень с элемента: переопределения и шлагбаум больше его не касаются, а `muted` страницы
   * возвращается настоящему элементу. Это единственная точка, через которую проходит любой выход
   * тени, поэтому `muted` не останется нашим ни на одном пути (находка 3 ревью 09b).
   */
  #detach(): void {
    this.#active = false;
    this.#registry.detach(this.#element);
    this.#disposeGate();
    // Находка 00c (Ashdi): вернуть именно `muted` страницы, а не просто снять наш.
    this.#attempt("detach: restore muted", () => this.#muted.restoreToPage());
  }

  /** Возврат: громкость, скорость, позиция, игра/пауза настоящего элемента. */
  #returnRealElement(targetUs: MediaTimeMicros, resumePlaying: boolean, pageSawPlaying: boolean): void {
    const media = this.#originals.media;
    const element = this.#element;
    // `muted` страницы уже вернул `#detach` (он всегда раньше возврата позиции).
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

  #totalFrames(nowMs: number): number {
    return shadowTotalFrames(this.#frameBaseline, this.#timeline.playedUs(nowMs));
  }

  #durationUs(): MediaTimeMicros | undefined {
    if (this.#file !== null) return this.#file.view.durationUs;
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
