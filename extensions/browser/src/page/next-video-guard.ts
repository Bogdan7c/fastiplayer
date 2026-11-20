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
// Отказ переезда относится к шагу ленты, на котором видео задержали (16b2, находка живой проверки Instagram).
// Лента запускает следующий ролик той же задачей, что останавливает наш, и хвост прежнего взвода успевает
// задержать его раньше, чем фон узнает, что наш ролик ушёл с экрана: фон отвечает «не сейчас», и без поправки
// этот ролик остался бы отпущенным навсегда — играл бы в браузере со звуком, без заглушки. Поэтому, когда
// переданный ролик уходит с экрана (`noteHandedOffVideoLeftScreen`), прежние отказы переезда больше не в счёт.
// Обратный порядок (16b3, правило F6): уход донесён раньше, чем видео задержано, но донесение сторожа обогнало
// `video_left_screen` в пути к фону (разные каналы), и фон ответил «не сейчас» на ТОМ ЖЕ шаге. Пока после ухода
// не началась новая передача (фон ещё ничего не принял), такой отказ мог быть сказан без знания об уходе: сторож
// спрашивает фон о том же видео ещё раз — один раз на видео и шаг, второй отказ уже окончательный.
//
// Режим 2 «автозапуск» (сессия 12a): вне взвода фона сторож тоже задерживает видео, но только если охрана
// включена настройкой, а запустил видео со звуком САМ человек (`ViewerPlayWatch`: свежий жест или кредит
// клика). Механика та же (тень в вызове `play()`, на `loadedmetadata`, по доверенному `play`), донесение
// фону несёт `trigger: "viewer_play"`. Охрана выключена — `play()` строго прозрачен, лишних слушателей нет.
// Политика «кто зритель» лежит в `auto-handoff/viewer-play-watch.ts`, правил сайтов здесь нет.
//
// Предложение «Ask» (сессия 12b, решения 255–257): фон ответил «спроси», и изолированный мир просит держать
// задержанное видео (`holdForViewerOffer`). Модель тени показывает странице паузу (плеер сайта спокойно стоит,
// пока человек выбирает), а сигнал предложения говорит изолированному миру, над каким элементом рисовать панель.
// Срока у такого удержания нет: выбор за человеком. Дальше три исхода, и каждый возвращает модели ту игру,
// которую страница видела до предложения:
// - «в плеер» — обычный `take` контроллера передачи (`begin_handoff`): плеер начинает с игры, как в 12a;
// - «здесь» — обычное отпускание: видео играет в браузере и, как любое отпущенное, по этой причине больше не
//   задерживается, пока не сменит источник или видимость (решение 257: «не спрашивать до новой страницы»);
// - «снято» (на той же вкладке встало новое предложение) — тень снимается, видео остаётся на паузе, отказ
//   не запоминается: новое нажатие play по нему — новый вопрос.
//
// Взвод возврата страницы (сессия 11b2, решения 240–241): фон открыл ушедшую страницу заново и ждёт ТО
// САМОЕ видео, что играет в окне плеера (`PageReturnTarget`). Отличия от переезда (шаги — page-return-step.ts):
// - настоящий `play()` не зовётся, пока видео не узнано: открытая фоном вкладка не имеет жеста зрителя, и
//   браузер отверг бы звучащий запуск (разведка 11b2: `NotAllowedError` в Chromium 156 и Firefox 158, даже у
//   заглушённой вкладки). Страница получает своё обещание, его заберёт тень;
// - видео другой длины (реклама, превью) не наше: отложенный `play()` уходит браузеру, ролик играет сам;
// - запись сначала честно перематывается на секунду плеера (настоящий `currentTime`, до тени), тень встаёт
//   на `seeked`: плеер сайта видит обычную перемотку и сам качает данные с этой секунды.

import type { AutoHandoffWatch } from "../shared/auto-handoff-contract.ts";
import type { ElementKey, HeldVideoStartData, HeldVideoTrigger, PageEvent, PageReturnTarget, VideoCandidate } from "../shared/handoff-contract.ts";
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
import type { ViewerOfferSignals } from "./viewer-offer-anchor.ts";

/** `HTMLMediaElement.HAVE_METADATA`: длительность и размер кадра известны. */
const HAVE_METADATA = 1;
/**
 * `HTMLMediaElement.HAVE_FUTURE_DATA`: данных хватает, чтобы играть. Элемент не на паузе дошёл до неё — браузер
 * уже забрал ждущие обещания `play()` на исполнение («notify about playing» стандарта HTML), и `pause()` их больше
 * не отвергнет.
 */
const HAVE_FUTURE_DATA = 3;

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
  /** Сигнал «предложение «Ask» встало/ушло» для изолированного мира (12b). */
  readonly viewerOffer: ViewerOfferSignals;
  /**
   * Есть ли у моста данные под секундой, с которой видео пошло бы в окне (решение 378, held-video-start-data.ts).
   * Уходит фону в донесении; не бросает.
   */
  readonly startDataOf: (element: object) => HeldVideoStartData;
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
  /** Видео держится под предложением «Ask» (12b); `null` — обычное удержание до решения фона. */
  offer: ViewerOfferHold | null;
  /** На каком шаге ленты видео задержали: отказ фона говорит о том шаге, а не о нынешнем (см. шапку). */
  feedStepWhenHeld: number;
}

/** Удержание под предложением: какую игру страница видела до него (её модель вернёт при любом исходе, кроме «снято»). */
interface ViewerOfferHold {
  readonly pageWasPlaying: boolean;
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
  /**
   * Элементы, чей `play()` страницы ушёл браузеру мимо сторожа (сторож не взведён, видео ему не подошло): обещание
   * у страницы НАСТОЯЩЕЕ, и пока у элемента нет данных, оно ждёт. Тень поставила бы элемент на паузу, и браузер
   * отверг бы его `AbortError` (стенд 16b3b `feed-back-recreated-pool`, см. `#pagePlayMayBePending`). Слабое
   * множество: страница этого не видит, `play()` остаётся прозрачным.
   */
  readonly #pagePlaysPastGuard = new WeakSet<object>();
  /** Шаг ленты: сколько раз переданный ролик этого документа уходил с экрана (см. шапку). */
  #feedStep = 0;
  /** Переданный ролик ушёл с экрана, а новой передачи с тех пор не было: фон мог отказать, не зная об уходе (F6). */
  #screenExitUnanswered = false;
  /** На каком шаге ленты видео уже спрашивали повторно (F6): второй раз на том же шаге не спрашиваем. */
  readonly #reaskedAtStep = new WeakMap<object, number>();
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
  setViewerPlayWatch(watch: AutoHandoffWatch): void {
    this.#environment.viewerPlay.setWatch(watch);
    if (watch.state === "on") this.#listenOnDocument();
  }

  /** Фон выдал «кредит клика»: пока он действует, запуск видео со звуком считается запуском человека. */
  grantViewerPlayCredit(leaseMs: number): void {
    this.#environment.viewerPlay.grantCredit(leaseMs);
  }

  /** Передача элемента кончилась (Return, окно закрыто, замена): повторный запуск засчитается после нового ввода. */
  noteHandoffEnded(element: object): void {
    this.#environment.viewerPlay.noteHandoffEnded(element);
  }

  /**
   * Переданный ролик этого документа уходит с экрана, и страница его остановила: лента листнулась (16b2).
   * Отказы переезда, полученные до этого, больше не в счёт — следующий ролик можно задержать и спросить
   * фон заново. Отказы режима 2 (нажатие play человеком) шагом ленты не меряются.
   */
  noteHandedOffVideoLeftScreen(): void {
    this.#feedStep += 1;
    this.#screenExitUnanswered = true;
  }

  /** Забрать задержанное видео для передачи (`null` — такого нет). */
  take(element: object): HeldVideo | null {
    const entry = this.#held.get(element);
    if (entry === undefined) return null;
    this.#held.delete(element);
    // Началась новая передача: фон ответил на уход переданного ролика, отказы дальше — уже с этим знанием (F6).
    this.#screenExitUnanswered = false;
    // Человек выбрал «в плеер»: плеер начинает с той игры, что страница видела до предложения.
    this.#finishOffer(entry);
    return entry;
  }

  /**
   * Фон ответил «спроси» (12b): держать задержанное зрителем видео под предложением. Чужой ключ, видео не
   * задержано зрителем или изолированный мир о предложении не узнал — отпустить: держать видео на паузе без
   * панели нельзя, человеку нечем было бы его запустить.
   */
  holdForViewerOffer(key: ElementKey): void {
    const element = this.#environment.registry.elementByKey(key);
    const entry = element === null ? undefined : this.#held.get(element);
    if (element === null || entry === undefined || entry.trigger !== "viewer_play" || entry.offer !== null) return;
    // Одно предложение на документ: прежнее снимается (фон делает то же между фреймами вкладки).
    for (const [otherElement, other] of this.#held) if (other.offer !== null && otherElement !== element) this.#withdrawEntry(otherElement, other);
    entry.offer = { pageWasPlaying: entry.session.pageSawPlaying() };
    entry.session.showHeldPagePlayback("pause");
    if (!this.#environment.viewerOffer.offerStood(element)) {
      this.#environment.reportOddity("viewer offer could not be shown, the video plays here");
      this.releaseElement(element);
    }
  }

  /** Фон снял предложение (на вкладке встало новое): тень снимается, видео стоит на паузе. Чужой ключ — ничего. */
  withdrawViewerOffer(key: ElementKey): void {
    const element = this.#environment.registry.elementByKey(key);
    const entry = element === null ? undefined : this.#held.get(element);
    if (element !== null && entry?.offer != null) this.#withdrawEntry(element, entry);
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
    // Фон отказал, не зная, что лента уже листнулась: его ответ и донесение тени разминулись в пути (в любом порядке).
    if (this.#refusalMayPredateScreenExit(element, entry)) {
      this.#reaskWhileHeld(element, entry);
      return;
    }
    this.#held.delete(element);
    this.#remember(element, entry);
    // «Play here»: страница снова видит игру, и снятая тень запускает настоящий элемент (`as_shadow`).
    this.#finishOffer(entry);
    try {
      entry.session.end({ kind: "shadow_position" }, { kind: "as_shadow" });
    } catch {
      this.#environment.reportOddity("could not release a held video");
    }
  }

  /**
   * Спросить фон о задержанном видео ещё раз, НЕ снимая тень. Снять и тут же задержать снова нельзя: настоящий
   * `play()` после снятия ещё не исполнился, а новая тень ставит элемент на паузу — обещание страницы отверглось бы
   * `AbortError`, которого страница не вызывала (стенд 16b3: пул TikTok в быстрой серии, превью при уходе из ленты).
   * Под тенью страница по-прежнему видит игру; донесение — с нынешним описанием видео и на нынешнем шаге ленты.
   */
  #reaskWhileHeld(element: object, entry: HeldEntry): void {
    this.#reaskedAtStep.set(element, this.#feedStep);
    entry.feedStepWhenHeld = this.#feedStep;
    const candidates = withHeldPlaying(this.#environment.candidates.describe(), entry.key);
    const startData = this.#environment.startDataOf(element);
    this.#environment.send({ kind: "next_video_held", element: entry.key, candidates, trigger: entry.trigger, start_data: startData });
  }

  /**
   * Мог ли фон отказать в переезде, ещё не зная об уходе переданного ролика с экрана (шапка). Два порядка:
   * - видео задержано на прежнем шаге, а уход донесён после (16b2): отказ устарел;
   * - уход донесён раньше задержания, но новой передачи с тех пор не было (16b3, F6): ответ фона мог обогнать
   *   `video_left_screen` в пути.
   * Повтор один на видео и шаг: второй отказ на том же шаге окончательный. Отказы режима 2 сюда не относятся.
   */
  #refusalMayPredateScreenExit(element: object, entry: HeldEntry): boolean {
    if (entry.trigger !== "next_video" || this.#reaskedAtStep.get(element) === this.#feedStep) return false;
    if (entry.feedStepWhenHeld !== this.#feedStep) return true;
    return this.#screenExitUnanswered;
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

  /** `play()` страницы у элемента не в тени. Ушёл браузеру мимо сторожа — запоминается (`#pagePlaysPastGuard`). */
  #interceptPlay(element: object, realPlay: () => unknown): PlayWatchAnswer {
    this.#forgetSettledPagePlay(element);
    const answer = this.#answerPlay(element, realPlay);
    if (answer.kind === "not_mine") this.#notePagePlayPastGuard(element);
    return answer;
  }

  /**
   * Запомнить `play()`, ушедший браузеру мимо сторожа. Чужой объект (`play.call(1)`) и наш сбой до страницы не долетают.
   * Метка живёт, пока обещание может ждать: её снимает доверенное `playing` с данными (`#forgetPagePlayIfExecuted`),
   * поэтому сторож слушает сам элемент — слушателя документа до первого взвода нет.
   */
  #notePagePlayPastGuard(element: object): void {
    try {
      if (!this.#environment.dom.isVideoElement(element)) return;
      this.#pagePlaysPastGuard.add(element);
      this.#listenOnElement(element);
    } catch {
      this.#environment.reportOddity("next video guard could not note a page play() call");
    }
  }

  /**
   * Новый `play()` застал элемент на паузе: прежнее настоящее обещание уже отвергнуто или исполнено. Его отвергают
   * `pause()`, новый источник и конец ролика, и все они ставят паузу. Ждать обещание может, только пока элемент
   * не вставал на паузу с того `play()`. Метка снимается; если и этот `play()` уйдёт мимо сторожа, она встанет снова.
   */
  #forgetSettledPagePlay(element: object): void {
    try {
      if (this.#pagePlaysPastGuard.has(element) && this.#environment.dom.isPaused(element)) this.#pagePlaysPastGuard.delete(element);
    } catch {
      this.#environment.reportOddity("next video guard could not read a paused state on play()");
    }
  }

  /**
   * Браузер исполнил обещание `play()` страницы: доверенное `playing`, элемент не на паузе и с данными
   * (HAVE_FUTURE_DATA). Метка больше ничего не бережёт и снимается. Без этого она переживала ролик: реклама или
   * серия 1, запущенная до передачи, оставляла метку элементу, и фильм (серия 2) в том же `<video>` не задерживался
   * на своих метаданных (стенд `ad-film-same-element`, `episodes delayed-next`).
   */
  #forgetPagePlayIfExecuted(element: object, eventType: RealMediaEventType | undefined): void {
    if (eventType !== "playing" || !this.#pagePlaysPastGuard.has(element)) return;
    const { dom } = this.#environment;
    if (dom.isPaused(element) || dom.readyStateOf(element) < HAVE_FUTURE_DATA) return;
    this.#pagePlaysPastGuard.delete(element);
  }

  /**
   * Может ли у элемента ждать обещание `play()`, которое страница получила от браузера (не от нас): `play()` ушёл
   * мимо сторожа, а данных для игры ещё нет. Задерживать такой элемент сейчас нельзя: тень ставит настоящий элемент
   * на паузу, и браузер отверг бы обещание страницы `AbortError`, которого она не вызывала (как правило F6 / решение
   * 371, только путь другой: `play()` сказан до взвода, тень встала бы позже — на метаданных или обходом при взводе).
   * Задержание приходит, когда элемент дошёл до HAVE_FUTURE_DATA: доверенное `playing` — ровно тот миг, когда
   * браузер уже забрал обещание на исполнение. Плата — до первого кадра ролик успевает начать играть в браузере.
   * Метку снимает исполненное обещание (`#forgetPagePlayIfExecuted`) и новый `play()` на паузе
   * (`#forgetSettledPagePlay`). Если обещание отвергнуто паузой или новым источником, а нового `play()` нет, метка
   * откладывает задержание до `playing`. Обещания нашего мира тень тогда исполняет сразу (`takeOverPendingPlays`).
   */
  #pagePlayMayBePending(element: object): boolean {
    return this.#pagePlaysPastGuard.has(element) && this.#environment.dom.readyStateOf(element) < HAVE_FUTURE_DATA;
  }

  /** Ответ сторожа на `play()` страницы. Всё под try/catch: наш сбой не долетает до страницы. */
  #answerPlay(element: object, realPlay: () => unknown): PlayWatchAnswer {
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

  /**
   * Зацикленное видео (баннер, превью) сторож не трогает. Исключение — режим 2 при выключенном общем фильтре
   * цикла (решение 261): человек сам запустил ролик ленты, а включён ли фильтр на этом сайте, решит фон.
   */
  #loopKeepsAway(element: object, trigger: HeldVideoTrigger): boolean {
    if (!this.#environment.dom.isLooping(element)) return false;
    return trigger !== "viewer_play" || this.#environment.viewerPlay.skipsLooped();
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
      if (!dom.isVideoElement(element)) return;
      this.#forgetPagePlayIfExecuted(element, eventType);
      if (dom.isEnded(element)) return;
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
    // `seeked` зрителя и `playing` (данные пришли) ничего не начинают: запуск судится по `play` и `loadedmetadata`.
    if (eventType === "seeked" || eventType === "playing" || !viewerPlay.isWatchOn() || this.#environment.isProtected(element)) return;
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
    const { seam, registry } = environment;
    if (seam.isShadowed(element) || this.#held.has(element) || this.#held.size >= MAX_HELD_VIDEOS) return false;
    if (environment.isProtected(element) || this.#loopKeepsAway(element, trigger)) return false;
    // Переезд: обещание play() страницы ещё ждёт данных — тень встанет на `playing` (см. `#pagePlayMayBePending`).
    // Режим 2 судит запуск по своим событиям (`play`, `loadedmetadata`) и сюда не относится.
    if (trigger === "next_video" && initialPlayback === "as_real_element" && this.#pagePlayMayBePending(element)) return false;
    const kind = this.#kindOf(element);
    if (kind === null) return false;
    const video = element as HTMLMediaElement;
    const declined = this.#declined[trigger].get(element);
    if (declined !== undefined && declined === this.#declineMarkOf(video, trigger, this.#feedStep)) return false;
    const key = registry.keyOf(video);
    // Описание — до тени: она заглушит настоящий элемент, а фону нужно, как его видела страница.
    const candidates = withHeldPlaying(environment.candidates.describe(), key);
    const startData = environment.startDataOf(element);
    let forward: (event: PageEvent) => void = (event) => {
      // До решения фона наружу не идёт ничего; тень могла сняться сама: страница сменила источник.
      if (event.kind !== "handoff_ended") return;
      const ended = this.#held.get(element);
      this.#held.delete(element);
      // Висело предложение — его панель уходит вместе с видео.
      if (ended?.offer != null) environment.viewerOffer.offerEnded();
    };
    const session = environment.startHeldShadow(video, (event) => forward(event), {
      isMediaSourceObjectUrl: (url) => registry.isMediaSourceObjectUrl(url),
      visibleShareOf: (element) => environment.candidates.visibleShareOf(element),
      source: kind,
      initialPlayback,
    });
    const entry: HeldEntry = {
      key,
      trigger,
      offer: null,
      feedStepWhenHeld: this.#feedStep,
      session,
      connect: (emit) => {
        forward = emit;
      },
    };
    this.#held.set(element, entry);
    // Отложенный `play()` больше не нужен: обещания страницы исполняет тень.
    this.#deferredRealPlays.delete(element);
    session.takeOverPendingPlays(this.#awaitingMetadata.get(element)?.splice(0) ?? []);
    environment.send({ kind: "next_video_held", element: key, candidates, trigger, start_data: startData });
    return true;
  }

  /** Конец предложения выбором человека: модели — прежняя игра, изолированному миру — «ушло». Без предложения — ничего. */
  #finishOffer(entry: HeldEntry): void {
    const offer = entry.offer;
    if (offer === null) return;
    entry.offer = null;
    if (offer.pageWasPlaying) entry.session.showHeldPagePlayback("play");
    this.#environment.viewerOffer.offerEnded();
  }

  /** Снять предложение без выбора: тень уходит, видео остаётся на паузе, отказ не запоминается. */
  #withdrawEntry(element: object, entry: HeldEntry): void {
    this.#held.delete(element);
    entry.offer = null;
    this.#environment.viewerOffer.offerEnded();
    try {
      entry.session.end({ kind: "shadow_position" }, { kind: "exactly", resume: "paused" });
    } catch {
      this.#environment.reportOddity("could not withdraw a viewer offer");
    }
  }

  #remember(element: object, entry: HeldEntry): void {
    this.#declined[entry.trigger].set(element, this.#declineMarkOf(element, entry.trigger, entry.feedStepWhenHeld));
  }

  /**
   * Метка отказа: чем было видео и, для переезда, на каком шаге ленты его задержали. Совпала с нынешней —
   * видео уже отпускали в тех же обстоятельствах, снова не задерживаем.
   */
  #declineMarkOf(element: object, trigger: HeldVideoTrigger, feedStep: number): string {
    const fingerprint = this.#fingerprintOf(element);
    return trigger === "next_video" ? `${fingerprint}|feed-step-${feedStep}` : fingerprint;
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
