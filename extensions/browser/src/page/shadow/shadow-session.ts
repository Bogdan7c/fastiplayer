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
// - Play/pause зрителя (решение 194, «как медиаклавиша») — правило `viewer-media-key.ts`:
//   обработчик Media Session страницы, если он есть, иначе прежняя доставка элементу.
// - Время источника (10a3) — `ShadowTimeSource`: переводит всё от плеера и к нему (у эфира HLS — со сдвигом).

import type { HandoffEndCause, HandoffEndPosition, HandoffEndResume, PageEvent, ShadowCommand } from "../../shared/handoff-contract.ts";
import { MAX_PLAYBACK_RATE_PERMILLE, MIN_PLAYBACK_RATE_PERMILLE } from "../../protocol/limits.ts";
import type { MediaTimeMicros, PlaybackRatePermille } from "../../protocol/time.ts";
import type { MediaKeyAction } from "../media-session-guard.ts";
import type { HeldShadowPlayback, PendingPlay, ShadowSourceKind } from "../handoff-seam.ts";
import { FileShadowLifecycle, type FileShadowHost, type ReturnPlan } from "./file-shadow-lifecycle.ts";
import { PageMutedState } from "./page-muted.ts";
import { FileSourceMode, type ShadowSourceView } from "./file-source-mode.ts";
import { type GateDecision, installEventGate, PLAYBACK_EVENT_TYPES, READINESS_EVENT_TYPES, SOURCE_RESET_EVENT_TYPE } from "./event-gate.ts";
import { EventDispatchQueue } from "./event-dispatch-queue.ts";
import { type FrameCallback, type FrameCountBaseline, readRealFrameQuality, ShadowFrameCallbacks, shadowTotalFrames } from "./frame-stats.ts";
import { shadowProblemReporter } from "./once-reporter.ts";
import { callOriginal, type ElementOriginals, readOriginal, writeOriginal } from "./originals.ts";
import { SERVE_AS_PLAIN_ELEMENT, type ShadowBehavior, type ShadowRegistry, type SourceProperty } from "./prototype-overrides.ts";
import { RealElementFollower } from "./real-element-follower.ts";
import { ShadowPulse } from "./shadow-pulse.ts";
import { createLiveHlsTimeSource } from "./live/index.ts";
import { applyPlayerSeek, pageSeekEventFor, PLAYER_TIME_IS_PAGE_TIME, resumesPlayingOnReturn } from "./shadow-time-source.ts";
import type { ShadowTimeSource } from "./shadow-time-source.ts";
import { ShadowTimeline, READY_STATE, type ReadyStateValue, type TimelineSignal } from "./shadow-timeline.ts";
import { returnRealElement, type RealReturnPorts } from "./real-element-return.ts";
import type { FileStopMoment, ShadowEnvironment } from "./shadow-environment.ts";
import { endCauseOfRelease, SourceTransitionWatch } from "./source-transitions.ts";
import { deliverViewerMediaKey, type ViewerMediaKeyPorts } from "./viewer-media-key.ts";

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

const PLAY_INTERRUPTED_MESSAGE = "The play() request was interrupted.";

/**
 * Что «алгоритм загрузки» браузера сделал с настоящим элементом. Элемент в NETWORK_EMPTY браузер
 * не сбрасывает (нет `emptied`, пауза и ожидающие `play()` не трогаются) — тень тоже не должна:
 * она уже сбросилась, когда источник пропал.
 */
type LoadAlgorithmEffect = "element_reset" | "element_was_empty";

export class ShadowController implements ShadowBehavior {
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
  /** Решает, когда смена источника или судьба элемента на странице — конец тени (source-transitions.ts). */
  readonly #sourceWatch: SourceTransitionWatch;
  readonly #pulse = new ShadowPulse();
  readonly #disposeGate: () => void;
  /** Тень файла (`null` — тень MSE): снимок файла, снятый источник и возврат. У эфира HLS — тоже она. */
  readonly #file: FileShadowLifecycle | null;
  /** Время источника: перевод между временем плеера и страницы, окно перемотки, позиция возврата. */
  readonly #timeSource: ShadowTimeSource;
  #pendingPlays: PendingPlay[] = [];
  /** `muted`, как его видит страница; настоящий элемент на время тени заглушен (page-muted.ts). */
  readonly #muted: PageMutedState;
  /** Сколько `emptied` уже обработано синхронно в `load()`/`src=` и ещё придёт от браузера. */
  #expectedEmptiedEvents = 0;
  /** Наша перестановка настоящего элемента ещё не «устоялась»: его `canplay*` — не для страницы. */
  #realRepositionInFlight = false;
  #active = true;
  /** Эта тень приостановила mediaSession страницы, ей и возвращать. */
  #suspendedMediaSession = false;

  constructor(
    element: HTMLMediaElement,
    emit: (event: PageEvent) => void,
    env: ShadowEnvironment,
    registry: ShadowRegistry,
    source: ShadowSourceKind,
    /** Снимок файла прежней тени, чей возврат прервала эта передача (`null` — не было). */
    interruptedFile: FileSourceMode | null,
    fileStop: FileStopMoment,
    /** С чего начинает модель: как настоящий элемент или «страница просила играть» (возврат страницы, 11b2). */
    initialPlayback: HeldShadowPlayback,
  ) {
    this.#element = element;
    this.#emitOutward = emit;
    this.#env = env;
    this.#originals = env.originals;
    this.#registry = registry;
    this.#queue = new EventDispatchQueue(element, env.originals);
    // «Серия доиграла» знает модель часов (16a): сторож спрашивает её в момент снятия источника.
    this.#sourceWatch = new SourceTransitionWatch(element, env.originals, env.isMediaSourceObjectUrl, () => this.#timeline.ended);
    // Снимок файла — до любых наших действий с элементом (пауза, шлагбаум, снятие источника).
    this.#file = source === "media_source" ? null : new FileShadowLifecycle(this.#fileSnapshot(interruptedFile), this.#fileHost());

    const media = this.#originals.media;
    const nowMs = env.now();
    // Секунда страницы в момент передачи (P0): с неё начинает модель тени, от неё считается сдвиг эфира.
    const handoffPositionUs = secondsToMicros(Number(readOriginal(media.currentTime, element)));
    this.#timeSource = this.#timeSourceFor(source, handoffPositionUs);
    const realPaused = readOriginal(media.paused, element) === true;
    const realReadyState = Number(readOriginal(media.readyState, element));
    this.#muted = new PageMutedState(element, env.originals);
    const baselineQuality = readRealFrameQuality(element, this.#originals);
    this.#frameBaseline = baselineQuality.baseline;
    this.#qualityPrototype = baselineQuality.prototype;
    this.#timeline = new ShadowTimeline(
      {
        positionUs: handoffPositionUs,
        // Настоящий элемент тень всё равно ставит на паузу (ниже): «играет» — это только модель для страницы.
        paused: realPaused && initialPlayback === "as_real_element",
        pageRate: Number(readOriginal(media.playbackRate, element)),
        // Тень хранит только 1/2/4: «будущих данных» без «достаточно» у неё не бывает.
        readyState: realReadyState >= READY_STATE.HAVE_FUTURE_DATA ? READY_STATE.HAVE_ENOUGH_DATA : (realReadyState as ReadyStateValue),
        nowMs,
      },
      () => this.#durationUs(),
      (signal) => this.#onTimelineSignal(signal),
      () => this.#timeSource.pageSeekRange(),
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
    // Задержанное видео пока ничьё: mediaSession и файл тронем, только когда фон его возьмёт.
    if (fileStop === "stop_file_now") this.#takeOverFromBrowser();
  }

  /** Страж mediaSession и снятие файла. Решение 186: шлагбаум уже гасит события этой записи. */
  #takeOverFromBrowser(): void {
    this.#suspendedMediaSession = true;
    this.#attempt("start: suspend mediaSession", () => this.#env.mediaSession.suspend());
    this.#file?.start();
  }

  /**
   * Фон взял задержанное видео. Файл снимается только сейчас, а часы плеера ещё несут старое поколение
   * (конец рекламы): тень держит свою позицию, пока плеер не признает новый источник.
   */
  commitHold(): void {
    this.#takeOverFromBrowser();
    this.#timeline.awaitPlayerOnNewSource(this.#env.now());
  }

  pageSawPlaying(): boolean {
    return !this.#timeline.paused;
  }

  /** Предложение «Ask» (12b): модель задержанной тени показывает странице паузу или игру, элемент не трогается. */
  showHeldPagePlayback(action: MediaKeyAction): void {
    this.#attempt(`held ${action}`, () => this.#showPlayerPlayState(action));
  }

  /** Обещания `play()`, выданные страницей до тени: теперь их исполняет модель часов. */
  takeOverPendingPlays(plays: readonly PendingPlay[]): void {
    this.#pendingPlays.push(...plays);
  }

  // -------------------------------------------------------------------------------------------
  // Команды плеера (через фон и изолированный мир).
  // -------------------------------------------------------------------------------------------

  apply(command: ShadowCommand): void {
    if (!this.#active) return;
    this.#attempt(`apply ${command.kind}`, () => {
      const nowMs = this.#env.now();
      switch (command.kind) {
        case "clock": {
          const transition = this.#sourceWatch.onClockTick(nowMs);
          if (transition !== "keep_shadow") {
            this.#releaseToPage(endCauseOfRelease(transition));
            return;
          }
          this.#timeline.onClock(this.#timeSource.pageTickOf(command), nowMs);
          this.#follower.onClockTick(nowMs);
          // Пульс — последним: если отправка оборвёт передачу, после него тень уже ничего не делает.
          if (this.#pulse.onClockTick(nowMs)) this.#emit({ kind: "shadow_alive" });
          return;
        }
        case "play":
        case "pause":
          // Решение 194: зритель нажал play/pause — странице это приходит как медиаклавиша.
          deliverViewerMediaKey(command.kind, this.#viewerMediaKeyPorts());
          return;
        case "set_rate":
          this.#timeline.setRateFromPlayer(command.rate, nowMs);
          return;
        case "seek":
          this.#emit(applyPlayerSeek(this.#timeSource, this.#timeline, command, nowMs));
          return;
        case "live_window":
          this.#timeSource.acceptLiveWindow(command);
          return;
        case "recording_window":
          this.#timeSource.acceptRecordingWindow(command);
          return;
      }
    });
  }

  end(position: HandoffEndPosition, resume: HandoffEndResume): void {
    if (!this.#active) return;
    const nowMs = this.#env.now();
    const returnTo = this.#timeSource.returnPositionFor(position, this.#timeline.positionUs(nowMs));
    const pageSawPlaying = !this.#timeline.paused;
    // Эфир HLS — всегда с игрой (решение 234, см. `resumesPlayingOnReturn`).
    const resumePlaying = resumesPlayingOnReturn(returnTo, resume.kind === "exactly" ? resume.resume === "playing" : pageSawPlaying);
    if (this.#file !== null) {
      this.#file.end({ position: returnTo, resumePlaying, pageSawPlaying });
      return;
    }
    this.#detach();
    this.#attempt("end: restore real element", () => returnRealElement(this.#realReturnPorts(), returnTo, resumePlaying, pageSawPlaying));
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

  play(): Promise<void> | typeof SERVE_AS_PLAIN_ELEMENT {
    if (this.#file?.pageRewroteSource === true) {
      // Страница уже поставила другой файл, а тень об этом ещё не знает: ответ старого ролика был бы
      // ложью (и перемоткой закончившейся рекламы на начало у плеера).
      this.#attempt("page rewrote the file source", () => this.#releaseToPage("element_released"));
      return SERVE_AS_PLAIN_ELEMENT;
    }
    const PagePromise = this.#originals.PromiseConstructor;
    let pending: PendingPlay | undefined;
    const promise = new PagePromise<void>((resolve, reject) => {
      pending = { resolve: () => resolve(), reject };
    });
    if (pending !== undefined) this.#pendingPlays.push(pending);
    this.#attempt("page play", () => {
      const result = this.#timeline.play("page", this.#env.now());
      if (result.restartedAtUs !== undefined) this.#emitPageSeek(result.restartedAtUs);
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
    return this.#timeSource.sourceView(this.#file?.view ?? null);
  }

  // -------------------------------------------------------------------------------------------
  // Внутреннее.
  // -------------------------------------------------------------------------------------------

  /** Шов доставки play/pause зрителя (viewer-media-key.ts): страж, прежний путь, жива ли тень. */
  #viewerMediaKeyPorts(): ViewerMediaKeyPorts {
    return {
      invokePageAction: (action) => this.#env.mediaSession.invokePageAction(action),
      deliverToElement: (action) => this.#showPlayerPlayState(action),
      shadowActive: () => this.#active,
      reportProblem: (context, problem) => shadowProblemReporter.report(`shadow: ${context}`, problem),
    };
  }

  /** Прежняя доставка: тень сама показывает странице игру или паузу по команде плеера. */
  #showPlayerPlayState(action: MediaKeyAction): void {
    const nowMs = this.#env.now();
    if (action === "play") this.#timeline.play("player", nowMs);
    else this.#timeline.pause(nowMs);
  }

  #pageSeek(seconds: number): void {
    this.#attempt("page seek", () => {
      // Перематывать некуда (эфир без окна) — как у браузера с пустым `seekable`: ничего не происходит.
      const outcome = this.#timeline.pageSeek(secondsToMicros(seconds), this.#env.now());
      if (outcome.kind === "seeking") this.#emitPageSeek(outcome.targetUs);
    });
  }

  /** `page_seek` плееру — цель во времени плеера (shadow-time-source.ts). */
  #emitPageSeek(pageTargetUs: MediaTimeMicros): void {
    const event = pageSeekEventFor(this.#timeSource, pageTargetUs);
    if (event !== undefined) this.#emit(event);
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
    // Сторож спрашивает «серия доиграла?» ДО повтора алгоритма загрузки: тот сбросит `ended` модели.
    const decision = this.#sourceWatch.onSourceReset(nowMs);
    if (decision === "release_to_page") {
      this.#releaseToPage("element_released");
      return;
    }
    this.#repeatLoadAlgorithm(effect, nowMs);
    // Последним: если отправка оборвёт передачу, тень уже повторила всё, что сделал браузер.
    if (decision === "await_next_episode") this.#emit({ kind: "next_episode_wait_began" });
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

  /**
   * Страница бросила элемент (убрала источник или поставила не-MSE): тень уходит, элемент снова
   * настоящий. Стандарт уже поставил его на паузу без события `pause` и отверг ожидающие
   * `play()` — тень делает то же со своими обещаниями. Причина — для фона: `next_episode_not_started`
   * значит «серия доиграла, следующей нет» (16a), а не «страница забрала видео».
   */
  #releaseToPage(cause: Extract<HandoffEndCause, "element_released" | "next_episode_not_started">): void {
    this.#file?.beforeRelease();
    this.#detach();
    this.#settlePendingPlays("reject");
    this.#finish(cause);
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
      releaseToPage: () => this.#releaseToPage("element_released"),
      detach: () => this.#detach(),
      reportReturned: () => this.#finish("returned"),
      returnRealElement: (plan: ReturnPlan) => returnRealElement(this.#realReturnPorts(), plan.position, plan.resumePlaying, plan.pageSawPlaying),
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

  /** Шов возврата настоящего элемента (real-element-return.ts): состояние остаётся у контроллера. */
  #realReturnPorts(): RealReturnPorts {
    return {
      element: this.#element,
      originals: this.#originals,
      pageRate: this.#timeline.pageRate,
      takePendingPlays: () => {
        const waiting = this.#pendingPlays;
        this.#pendingPlays = [];
        return waiting;
      },
      rejectPendingPlays: () => this.#settlePendingPlays("reject"),
      handOverFrameCallbacks: () => this.#handOverFrameCallbacks(),
      enqueueEvent: (type) => this.#queue.enqueueEvent(type),
    };
  }

  /** Ожидающие `requestVideoFrameCallback` уходят настоящему элементу — цикл страницы не рвётся. */
  #handOverFrameCallbacks(): void {
    const original = this.#originals.video.requestVideoFrameCallback;
    if (original === undefined) return;
    this.#frameCallbacks.drainInto((callback) => callOriginal(original, this.#element, [callback]));
  }

  #finish(cause: HandoffEndCause): void {
    // Отпущенное задержанное видео mediaSession не трогало: рядом могла стоять живая тень рекламы.
    if (this.#suspendedMediaSession) this.#attempt("restore mediaSession", () => this.#env.mediaSession.restore());
    this.#emit({ kind: "handoff_ended", cause });
  }

  #repositionReal(positionUs: MediaTimeMicros): void {
    this.#realRepositionInFlight = true;
    writeOriginal(this.#originals.media.currentTime, this.#element, positionUs / MICROS_PER_SECOND);
  }

  #totalFrames(nowMs: number): number {
    return shadowTotalFrames(this.#frameBaseline, this.#timeline.playedUs(nowMs));
  }

  /** Время источника по виду передачи: эфир HLS — со сдвигом к P0, остальные — время плеера как есть. */
  #timeSourceFor(source: ShadowSourceKind, handoffPositionUs: MediaTimeMicros): ShadowTimeSource {
    if (source !== "live_hls") return PLAYER_TIME_IS_PAGE_TIME;
    return createLiveHlsTimeSource(handoffPositionUs, {
      originals: this.#originals,
      pagePositionUs: () => this.#timeline.positionUs(this.#env.now()),
      announceDurationChange: () => this.#queue.enqueueEvent("durationchange"),
    });
  }

  #durationUs(): MediaTimeMicros | undefined {
    // Длину отвечает вид источника: у файла — снимок, у эфира HLS — `Infinity`, пока он не стал записью (16d1).
    const seconds =
      this.#file !== null ? (this.sourceView()?.duration() ?? Number.NaN) : Number(readOriginal(this.#originals.media.duration, this.#element));
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
