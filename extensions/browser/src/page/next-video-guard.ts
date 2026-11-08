// Сторож следующего видео (сессия 09c2, решения 201, 204): MAIN-мир.
//
// Зачем. Реклама кончилась, и страница в ту же миллисекунду зовёт `play()` у следующего видео (замер
// живых Ashdi и Tortuga: фильм стартует той же задачей, что убирает рекламу). Элемент настоящий, тени на
// нём нет, и браузер успел бы показать кадр и выдать звук. Поэтому пока сеанс может переехать, сторож
// ставит тень сам, раньше первого кадра, а решение «берём или нет» принимает фон.
//
// Одно правило: тень встаёт, когда видео хочет играть И у него есть метаданные.
// - Метаданные есть (`play()` у загруженного фильма) — тень встаёт прямо в вызове `play()`, настоящий
//   `play()` не зовётся вовсе.
// - Метаданных нет (Ashdi и Tortuga ставят рекламе `src` и сразу зовут `play()`) — настоящий `play()`
//   проходит, элемент качает начало файла, а тень встаёт на `loadedmetadata`. Раньше нельзя: тень файла
//   показывает снимок длительности и размера, а фону для вердикта нужна длительность. Опасности нет:
//   до `loadedmetadata` браузеру нечего ни показывать, ни играть.
// - Запуск мимо `play()` (атрибут `autoplay`, чужой фрейм, поздно взведённый сторож) ловят доверенные
//   события и обход видео при каждом взводе: там задержка на миллисекунды, зато это запасной путь.
//
// Страница получает своё обещание вместо настоящего, пока метаданных нет: тень ставит настоящий элемент
// на паузу, и настоящее обещание отверглось бы `AbortError`. Плеер сайта принял бы это за сбой.
//
// Взвод живёт сроком (`lease_ms`), а команды «снять» нет: фон может умереть в любой момент, а страница
// не должна остаться со взведённым сторожем навсегда. Часы читаются только при событии, своих таймеров
// у сторожа нет: таймеры скрытой вкладки Chromium будит раз в минуту.
//
// Отпущенное видео не задерживается снова, пока не сменит источник или не станет видимым: иначе скрытый
// зеркальный плеер (решение 68) ходил бы по кругу «задержан — отпущен» на каждый взвод. Память отказов
// своя у каждой причины задержки (`HeldVideoTrigger`): отказ переезда («это не следующее видео сеанса») не
// отменяет нового нажатия play человеком (режим 2). Иначе хвост взвода сеанса, задержавший вернувшееся после
// «Return» видео, навсегда отнимал бы его у режима 2 (находка стенда 12a, строка `after-return`).
//
// Режим 2 «автозапуск» (сессия 12a): вне взвода фона сторож тоже задерживает видео, но только если охрана
// включена настройкой, а запустил видео со звуком САМ человек (`ViewerPlayWatch`: свежий жест или кредит
// клика). Механика та же (тень в вызове `play()`, на `loadedmetadata`, по доверенному `play`), донесение
// фону несёт `trigger: "viewer_play"`. Охрана выключена — `play()` строго прозрачен, лишних слушателей нет.
// Политика «кто зритель» лежит в `auto-handoff/viewer-play-watch.ts`, правил сайтов здесь нет.
//
// Взвод возврата страницы (сессия 11b2, решения 240–241): фон открыл ушедшую страницу заново и ждёт ТО
// САМОЕ видео, что играет в окне плеера (`PageReturnTarget`). Отличия от переезда (шаги — page-return-step.ts):
// - настоящий `play()` не зовётся, пока видео не узнано: открытая фоном вкладка не имеет жеста зрителя, и
//   браузер отверг бы звучащий запуск (разведка 11b2: `NotAllowedError` в Chromium 156 и Firefox 158, даже у
//   заглушённой вкладки). Страница получает своё обещание, его заберёт тень;
// - видео другой длины (реклама, превью) не наше: отложенный `play()` уходит браузеру, ролик играет сам;
// - запись сначала честно перематывается на секунду плеера (настоящий `currentTime`, до тени), тень встаёт
//   на `seeked`: плеер сайта видит обычную перемотку и сам качает данные с этой секунды.

import type { AutoHandoffWatchState } from "../shared/auto-handoff-contract.ts";
import type { ElementKey, HeldVideoTrigger, PageEvent, PageReturnTarget, VideoCandidate } from "../shared/handoff-contract.ts";
import type { ElementAudibility, ViewerPlayVerdict, ViewerPlayWatch } from "./auto-handoff/viewer-play-watch.ts";
import type { DomReaders } from "./dom-readers.ts";
import { hlsStreamOfDuration, shadowSourceOfHls } from "./hls-stream-kind.ts";
import type { ElementRegistry } from "./element-registry.ts";
import type { ElementSource } from "./element-source.ts";
import type {
  HeldShadowHandle,
  HeldShadowPlayback,
  HeldShadowStarter,
  PendingPlay,
  PlayWatchAnswer,
  PlayWatchSeam,
  RealMediaEventType,
  ShadowSourceKind,
} from "./handoff-seam.ts";
import { pageReturnStep } from "./page-return-step.ts";
import type { OddityReporter } from "./prototype-wrap.ts";
import type { VideoCandidates } from "./video-candidates.ts";

/** `HTMLMediaElement.HAVE_METADATA`: длительность и размер кадра известны. */
const HAVE_METADATA = 1;

/**
 * Сколько видео документа можно держать одновременно. Обычно одно; потолок нужен против страницы,
 * которая запускает сотни элементов разом: каждая задержка — это тень и опрос всех видео документа.
 */
const MAX_HELD_VIDEOS = 4;

/** Что сторожу нужно снаружи. */
export interface NextVideoGuardEnvironment {
  readonly dom: DomReaders;
  readonly registry: ElementRegistry;
  readonly candidates: VideoCandidates;
  readonly startHeldShadow: HeldShadowStarter;
  readonly seam: PlayWatchSeam;
  /** Политика режима 2: охрана, кредит клика, метки возврата (12a). */
  readonly viewerPlay: ViewerPlayWatch;
  /** Защищённый элемент сторож не трогает (инвариант 4). */
  readonly isProtected: (element: object) => boolean;
  /** Отправка фону через изолированный мир. */
  readonly send: (event: PageEvent) => void;
  /** Монотонные часы, мс. */
  readonly now: () => number;
  readonly reportOddity: OddityReporter;
  /** Окно документа: на нём слушаем доверенные события. */
  readonly documentEvents: object;
}

/** Задержанное видео: тень стоит, фон думает. */
export interface HeldVideo {
  readonly session: HeldShadowHandle;
  /** С этого момента события тени идут контроллеру передачи. */
  connect(emit: (event: PageEvent) => void): void;
}

/** Запись о задержанном видео. */
interface HeldEntry extends HeldVideo {
  readonly key: ElementKey;
  /** Почему задержали: отказ фона запоминается только для этой причины. */
  readonly trigger: HeldVideoTrigger;
}

/** Ответ «не наше дело»: один объект на все вызовы. */
const NOT_MINE: PlayWatchAnswer = { kind: "not_mine" };

export class NextVideoGuard {
  readonly #environment: NextVideoGuardEnvironment;
  readonly #held = new Map<object, HeldEntry>();
  /** Отпущенные по каждой причине задержки: чем было видео в момент отказа (см. шапку). */
  readonly #declined: Readonly<Record<HeldVideoTrigger, WeakMap<object, string>>> = { next_video: new WeakMap(), viewer_play: new WeakMap() };
  /** Обещания, выданные странице до метаданных. */
  readonly #awaitingMetadata = new WeakMap<object, PendingPlay[]>();
  /** Элементы, на которых уже слушаем события напрямую (теневой DOM). */
  readonly #listenedElements = new WeakSet<object>();
  /**
   * Настоящий `play()`, который сторож НЕ позвал при взводе возврата (см. шапку): уйдёт браузеру, если видео
   * окажется не нашим или взвод кончится. Обещания страницы при этом лежат в `#awaitingMetadata`.
   */
  readonly #deferredRealPlays = new WeakMap<object, () => unknown>();
  /** Сколько раз сторож сам перематывал элемент на секунду плеера (возврат страницы). */
  readonly #returnSeeks = new WeakMap<object, number>();
  #armedUntilMs = Number.NEGATIVE_INFINITY;
  /** Цель взвода возврата страницы; `undefined` — обычный взвод переезда (09c2). */
  #pageReturn: PageReturnTarget | undefined;
  #documentListened = false;

  constructor(environment: NextVideoGuardEnvironment) {
    this.#environment = environment;
    environment.seam.watchPlayCalls((element, realPlay) => this.#interceptPlay(element, realPlay));
  }

  /**
   * Взвести на `leaseMs`. Каждый взвод обходит видео документа: фрейм мог появиться уже после запуска
   * своего ролика, а `autoplay` в теневом DOM события до окна не доносит. `pageReturn` — цель возврата
   * страницы (11b2); без неё — обычный переезд на следующее видео (09c2).
   */
  arm(leaseMs: number, pageReturn?: PageReturnTarget): void {
    this.#armedUntilMs = this.#environment.now() + leaseMs;
    this.#pageReturn = pageReturn;
    this.#listenOnDocument();
    for (const element of this.#environment.registry.findVideoElements()) this.#considerRunningVideo(element);
  }

  /** Изолированный мир сообщил: охрана режима 2 включена/выключена. Включение ставит слушатель документа. */
  setViewerPlayWatch(state: AutoHandoffWatchState): void {
    this.#environment.viewerPlay.setWatch(state);
    if (state === "on") this.#listenOnDocument();
  }

  /** Фон выдал «кредит клика»: пока он действует, запуск видео со звуком считается запуском человека. */
  grantViewerPlayCredit(leaseMs: number): void {
    this.#environment.viewerPlay.grantCredit(leaseMs);
  }

  /** Передача элемента кончилась (Return, окно закрыто, замена): повторный запуск засчитается после нового ввода. */
  noteHandoffEnded(element: object): void {
    this.#environment.viewerPlay.noteHandoffEnded(element);
  }

  /** Забрать задержанное видео для передачи (`null` — такого нет). */
  take(element: object): HeldVideo | null {
    const entry = this.#held.get(element);
    if (entry === undefined) return null;
    this.#held.delete(element);
    return entry;
  }

  /** Фон отказал или не ответил: элемент играет дальше сам, тень снята. Чужой ключ — ничего. */
  release(key: ElementKey): void {
    const element = this.#environment.registry.elementByKey(key);
    if (element !== null) this.releaseElement(element);
  }

  /** То же по самому элементу. */
  releaseElement(element: object): void {
    const entry = this.#held.get(element);
    if (entry === undefined) return;
    this.#held.delete(element);
    this.#remember(element, entry.trigger);
    try {
      entry.session.end({ kind: "shadow_position" }, { kind: "as_shadow" });
    } catch {
      this.#environment.reportOddity("could not release a held video");
    }
  }

  /** Доверенные события окна: один слушатель на документ, ставится при первом взводе или включении охраны. */
  #listenOnDocument(): void {
    if (this.#documentListened) return;
    this.#documentListened = true;
    this.#environment.seam.onRealMediaEvent(this.#environment.documentEvents, (element, eventType) => this.#considerRunningVideo(element, eventType));
  }

  #armed(): boolean {
    return this.#environment.now() < this.#armedUntilMs;
  }

  /** `play()` страницы у элемента не в тени. Всё под try/catch: наш сбой не долетает до страницы. */
  #interceptPlay(element: object, realPlay: () => unknown): PlayWatchAnswer {
    try {
      if (!this.#environment.dom.isVideoElement(element)) return NOT_MINE;
      if (!this.#armed()) {
        // Взвод возврата кончился, а отложенный `play()` ещё лежит: отдать его браузеру, страница не зависнет.
        this.#releaseDeferredPlay(element);
        return this.#interceptViewerPlay(element, realPlay);
      }
      if (this.#pageReturn !== undefined) return this.#interceptPlayForReturn(element, realPlay, this.#pageReturn);
      if (this.#environment.dom.readyStateOf(element) >= HAVE_METADATA) return this.#hold(element, "as_real_element", "next_video") ? { kind: "now_shadowed" } : NOT_MINE;
      // Источника может ещё не быть: Ashdi зовёт `play()` у пустого элемента, а `src` ставит следующей строкой.
      if (this.#environment.isProtected(element)) return NOT_MINE;
      this.#listenOnElement(element);
      return { kind: "answered", promise: this.#playAwaitingMetadata(element, realPlay) };
    } catch {
      this.#environment.reportOddity("next video guard failed on play()");
      return NOT_MINE;
    }
  }

  /**
   * `play()` вне взвода фона (режим 2, 12a). Охрана выключена или запуск не от зрителя — `play()` прозрачен.
   * Зритель запустил: метаданные есть — тень в этом же вызове; нет — настоящий `play()` проходит, а тень встанет
   * на `loadedmetadata` (метка засчитанного `play()` живёт в `ViewerPlayWatch`).
   */
  #interceptViewerPlay(element: object, realPlay: () => unknown): PlayWatchAnswer {
    const { viewerPlay, dom } = this.#environment;
    if (!viewerPlay.isWatchOn() || this.#environment.isProtected(element)) return NOT_MINE;
    if (viewerPlay.judgePlayCall(element, this.#audibilityOf(element)) !== "viewer_started") return NOT_MINE;
    if (dom.readyStateOf(element) >= HAVE_METADATA) return this.#hold(element, "as_real_element", "viewer_play") ? { kind: "now_shadowed" } : NOT_MINE;
    this.#listenOnElement(element);
    return { kind: "answered", promise: this.#playAwaitingMetadata(element, realPlay) };
  }

  #audibilityOf(element: object): ElementAudibility {
    return this.#environment.dom.isSilent(element) ? "silent" : "audible";
  }

  /** Настоящий `play()` проходит, странице — наше обещание: его заберёт тень (см. шапку). */
  #playAwaitingMetadata(element: object, realPlay: () => unknown): unknown {
    const realPromise = realPlay();
    if (!isThenable(realPromise)) return realPromise;
    const { PagePromise } = this.#environment.seam;
    return new PagePromise<void>((resolve, reject) => {
      const pending: PendingPlay = { resolve, reject };
      const list = this.#awaitingMetadata.get(element) ?? [];
      list.push(pending);
      this.#awaitingMetadata.set(element, list);
      // Обещание тени настоящему больше не следует: `removePending` вернёт `false`.
      const removePending = (): boolean => {
        const index = list.indexOf(pending);
        if (index < 0) return false;
        list.splice(index, 1);
        return true;
      };
      realPromise.then(
        () => {
          if (removePending()) resolve();
        },
        (reason: unknown) => {
          if (removePending()) reject(reason);
        },
      );
    });
  }

  /**
   * `play()` при взводе возврата (см. шапку): узнанное видео на месте — тень сразу; не наше — браузеру; иначе
   * странице наше обещание, настоящий `play()` отложен, а сторож ждёт метаданных или конца своей перемотки.
   */
  #interceptPlayForReturn(element: object, realPlay: () => unknown, target: PageReturnTarget): PlayWatchAnswer {
    if (this.#environment.isProtected(element)) return NOT_MINE;
    const step = this.#returnStepOf(element, target);
    if (step.kind === "not_this_video") return NOT_MINE;
    if (step.kind === "hold") return this.#hold(element, "as_real_element", "next_video") ? { kind: "now_shadowed" } : NOT_MINE;
    this.#listenOnElement(element);
    const promise = this.#deferPlay(element, realPlay);
    this.#advanceReturn(element, target);
    return { kind: "answered", promise };
  }

  /** Странице — обещание её мира; настоящий `play()` не зовётся, а запоминается (последний). */
  #deferPlay(element: object, realPlay: () => unknown): unknown {
    this.#deferredRealPlays.set(element, realPlay);
    const { PagePromise } = this.#environment.seam;
    return new PagePromise<void>((resolve, reject) => {
      const list = this.#awaitingMetadata.get(element) ?? [];
      list.push({ resolve, reject });
      this.#awaitingMetadata.set(element, list);
    });
  }

  /**
   * Отложенный `play()` уходит браузеру: видео не наше или взвод кончился. Обещания страницы следуют за
   * настоящим. Отложенного нет — ничего.
   */
  #releaseDeferredPlay(element: object): void {
    const realPlay = this.#deferredRealPlays.get(element);
    if (realPlay === undefined) return;
    this.#deferredRealPlays.delete(element);
    const pending = this.#awaitingMetadata.get(element)?.splice(0) ?? [];
    const realPromise = realPlay();
    if (!isThenable(realPromise)) {
      for (const play of pending) play.resolve();
      return;
    }
    realPromise.then(
      () => pending.forEach((play) => play.resolve()),
      (reason: unknown) => pending.forEach((play) => play.reject(reason)),
    );
  }

  /** Шаг взвода возврата для элемента, который хочет играть (`play()` страницы или уже играет). */
  #advanceReturn(element: object, target: PageReturnTarget): void {
    const { dom, seam } = this.#environment;
    const pageAskedToPlay = (this.#awaitingMetadata.get(element)?.length ?? 0) > 0;
    if (dom.isPaused(element) && !pageAskedToPlay) return;
    const step = this.#returnStepOf(element, target);
    switch (step.kind) {
      case "wait":
        return;
      case "not_this_video":
        this.#releaseDeferredPlay(element);
        return;
      case "seek_to":
        // Конец перемотки придёт доверенным `seeked`: слушаем и сам элемент (теневой DOM окну его не донесёт).
        this.#listenOnElement(element);
        this.#returnSeeks.set(element, (this.#returnSeeks.get(element) ?? 0) + 1);
        seam.seekRealElement(element, step.seconds);
        return;
      case "hold":
        this.#hold(element, pageAskedToPlay ? "page_asked_to_play" : "as_real_element", "next_video");
        return;
    }
  }

  #returnStepOf(element: object, target: PageReturnTarget): ReturnType<typeof pageReturnStep> {
    const { dom, seam } = this.#environment;
    return pageReturnStep(
      {
        readyState: dom.readyStateOf(element),
        durationS: dom.durationSecondsOf(element),
        currentTimeS: dom.currentTimeSeconds(element),
        seeking: seam.isSeekingReal(element),
        seeksIssued: this.#returnSeeks.get(element) ?? 0,
      },
      target,
    );
  }

  #listenOnElement(element: object): void {
    if (this.#listenedElements.has(element)) return;
    this.#listenedElements.add(element);
    this.#environment.seam.onRealMediaEvent(element, (target, eventType) => this.#considerRunningVideo(target, eventType));
  }

  /** Элемент уже хочет играть (`play`, `loadedmetadata`, `seeked`; без вида события — обход при взводе). */
  #considerRunningVideo(element: object, eventType?: RealMediaEventType): void {
    try {
      const { dom } = this.#environment;
      if (!dom.isVideoElement(element) || dom.isEnded(element)) return;
      if (!this.#armed()) {
        this.#releaseDeferredPlay(element);
        if (eventType !== undefined) this.#considerViewerStartedVideo(element, eventType);
        return;
      }
      if (this.#pageReturn !== undefined) {
        if (!this.#environment.isProtected(element)) this.#advanceReturn(element, this.#pageReturn);
        return;
      }
      if (dom.isPaused(element) || dom.readyStateOf(element) < HAVE_METADATA) return;
      this.#hold(element, "as_real_element", "next_video");
    } catch {
      this.#environment.reportOddity("next video guard failed on a media event");
    }
  }

  /**
   * Режим 2: доверенное событие элемента вне взвода фона. `play` (родные кнопки, `autoplay`) судится по
   * обстановке сейчас, `loadedmetadata` — по метке засчитанного `play()`; `seeked` зрителя ничего не начинает.
   */
  #considerViewerStartedVideo(element: object, eventType: RealMediaEventType): void {
    const { viewerPlay, dom } = this.#environment;
    if (eventType === "seeked" || !viewerPlay.isWatchOn() || this.#environment.isProtected(element)) return;
    if (dom.isPaused(element) || dom.readyStateOf(element) < HAVE_METADATA) return;
    const audibility = this.#audibilityOf(element);
    const verdict: ViewerPlayVerdict =
      eventType === "play" ? viewerPlay.judgeTrustedPlayEvent(element, audibility) : viewerPlay.judgeLoadedMetadata(element, audibility);
    if (verdict === "viewer_started") this.#hold(element, "as_real_element", "viewer_play");
  }

  /** Форма источника для тени; `null` — передавать нечего. Чтения могут бросить. */
  #kindOf(element: object): ShadowSourceKind | null {
    const source: ElementSource = this.#environment.candidates.sourceOf(element as Element);
    switch (source.kind) {
      case "file":
        return "file";
      case "hls": {
        // Запись HLS под тенью — как файл (решения 186/187), эфир — своя тень (10a3). Правило одно с
        // контроллером передачи (hls-stream-kind.ts); метаданные уже есть (сторож ждёт HAVE_METADATA).
        const stream = hlsStreamOfDuration(this.#environment.dom.durationSecondsOf(element));
        return stream === null ? null : shadowSourceOfHls(stream);
      }
      case "media_source":
      case "worker_media_source":
        return "media_source";
      default:
        return null;
    }
  }

  /**
   * Поставить тень и спросить фон. `false` — видео задерживать нельзя. `initialPlayback` — с чего начинает
   * модель тени: при возврате страницы страница уже просила играть, а настоящий элемент стоит. `trigger` —
   * почему задержали (уйдёт фону в донесении).
   */
  #hold(element: object, initialPlayback: HeldShadowPlayback, trigger: HeldVideoTrigger): boolean {
    const environment = this.#environment;
    const { dom, seam, registry } = environment;
    if (seam.isShadowed(element) || this.#held.has(element) || this.#held.size >= MAX_HELD_VIDEOS) return false;
    if (environment.isProtected(element) || dom.isLooping(element)) return false;
    const kind = this.#kindOf(element);
    if (kind === null) return false;
    const video = element as HTMLMediaElement;
    const declined = this.#declined[trigger].get(element);
    if (declined !== undefined && declined === this.#fingerprintOf(video)) return false;
    const key = registry.keyOf(video);
    // Описание — до тени: она заглушит настоящий элемент, а фону нужно, как его видела страница.
    const candidates = withHeldPlaying(environment.candidates.describe(), key);
    let forward: (event: PageEvent) => void = (event) => {
      // До решения фона наружу не идёт ничего; тень могла сняться сама: страница сменила источник.
      if (event.kind === "handoff_ended") this.#held.delete(element);
    };
    const session = environment.startHeldShadow(video, (event) => forward(event), {
      isMediaSourceObjectUrl: (url) => registry.isMediaSourceObjectUrl(url),
      source: kind,
      initialPlayback,
    });
    const entry: HeldEntry = {
      key,
      trigger,
      session,
      connect: (emit) => {
        forward = emit;
      },
    };
    this.#held.set(element, entry);
    // Отложенный `play()` больше не нужен: обещания страницы исполняет тень.
    this.#deferredRealPlays.delete(element);
    session.takeOverPendingPlays(this.#awaitingMetadata.get(element)?.splice(0) ?? []);
    environment.send({ kind: "next_video_held", element: key, candidates, trigger });
    return true;
  }

  #remember(element: object, trigger: HeldVideoTrigger): void {
    this.#declined[trigger].set(element, this.#fingerprintOf(element));
  }

  /** Что должно измениться, чтобы отпущенное видео задержали снова: источник или видимость. */
  #fingerprintOf(element: object): string {
    const candidate = this.#environment.candidates
      .describe()
      .find((video) => video.element === this.#environment.registry.keyOf(element as Element));
    return `${this.#environment.dom.currentSrcOf(element)}|${candidate !== undefined && candidate.visible_area_px > 0}`;
  }
}

/** Страница видит задержанное видео играющим, хотя настоящий элемент уже стоит. */
function withHeldPlaying(candidates: readonly VideoCandidate[], key: ElementKey): VideoCandidate[] {
  return candidates.map((candidate) => (candidate.element === key ? { ...candidate, playing: true } : candidate));
}

function isThenable(value: unknown): value is PromiseLike<unknown> {
  return typeof value === "object" && value !== null && typeof (value as { then?: unknown }).then === "function";
}
