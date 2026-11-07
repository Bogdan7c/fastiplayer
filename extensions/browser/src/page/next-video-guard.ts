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
// зеркальный плеер (решение 68) ходил бы по кругу «задержан — отпущен» на каждый взвод.

import type { ElementKey, PageEvent, VideoCandidate } from "../shared/handoff-contract.ts";
import type { DomReaders } from "./dom-readers.ts";
import type { ElementRegistry } from "./element-registry.ts";
import type { ElementSource } from "./element-source.ts";
import type { HeldShadowHandle, HeldShadowStarter, PendingPlay, PlayWatchAnswer, PlayWatchSeam, ShadowSourceKind } from "./handoff-seam.ts";
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
}

/** Ответ «не наше дело»: один объект на все вызовы. */
const NOT_MINE: PlayWatchAnswer = { kind: "not_mine" };

export class NextVideoGuard {
  readonly #environment: NextVideoGuardEnvironment;
  readonly #held = new Map<object, HeldEntry>();
  /** Отпущенные: чем было видео в момент отказа (см. шапку). */
  readonly #declined = new WeakMap<object, string>();
  /** Обещания, выданные странице до метаданных. */
  readonly #awaitingMetadata = new WeakMap<object, PendingPlay[]>();
  /** Элементы, на которых уже слушаем события напрямую (теневой DOM). */
  readonly #listenedElements = new WeakSet<object>();
  #armedUntilMs = Number.NEGATIVE_INFINITY;
  #documentListened = false;

  constructor(environment: NextVideoGuardEnvironment) {
    this.#environment = environment;
    environment.seam.watchPlayCalls((element, realPlay) => this.#interceptPlay(element, realPlay));
  }

  /**
   * Взвести на `leaseMs`. Каждый взвод обходит видео документа: фрейм мог появиться уже после запуска
   * своего ролика, а `autoplay` в теневом DOM события до окна не доносит.
   */
  arm(leaseMs: number): void {
    this.#armedUntilMs = this.#environment.now() + leaseMs;
    if (!this.#documentListened) {
      this.#documentListened = true;
      this.#environment.seam.onRealMediaEvent(this.#environment.documentEvents, (element) => this.#considerRunningVideo(element));
    }
    for (const element of this.#environment.registry.findVideoElements()) this.#considerRunningVideo(element);
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
    this.#remember(element);
    try {
      entry.session.end({ kind: "shadow_position" }, { kind: "as_shadow" });
    } catch {
      this.#environment.reportOddity("could not release a held video");
    }
  }

  #armed(): boolean {
    return this.#environment.now() < this.#armedUntilMs;
  }

  /** `play()` страницы у элемента не в тени. Всё под try/catch: наш сбой не долетает до страницы. */
  #interceptPlay(element: object, realPlay: () => unknown): PlayWatchAnswer {
    try {
      if (!this.#armed() || !this.#environment.dom.isVideoElement(element)) return NOT_MINE;
      if (this.#environment.dom.readyStateOf(element) >= HAVE_METADATA) return this.#hold(element) ? { kind: "now_shadowed" } : NOT_MINE;
      // Источника может ещё не быть: Ashdi зовёт `play()` у пустого элемента, а `src` ставит следующей строкой.
      if (this.#environment.isProtected(element)) return NOT_MINE;
      this.#listenOnElement(element);
      return { kind: "answered", promise: this.#playAwaitingMetadata(element, realPlay) };
    } catch {
      this.#environment.reportOddity("next video guard failed on play()");
      return NOT_MINE;
    }
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

  #listenOnElement(element: object): void {
    if (this.#listenedElements.has(element)) return;
    this.#listenedElements.add(element);
    this.#environment.seam.onRealMediaEvent(element, (target) => this.#considerRunningVideo(target));
  }

  /** Элемент уже хочет играть (`play` или `loadedmetadata`, обход при взводе). */
  #considerRunningVideo(element: object): void {
    try {
      const { dom } = this.#environment;
      if (!this.#armed() || !dom.isVideoElement(element) || dom.isPaused(element) || dom.isEnded(element)) return;
      if (dom.readyStateOf(element) < HAVE_METADATA) return;
      this.#hold(element);
    } catch {
      this.#environment.reportOddity("next video guard failed on a media event");
    }
  }

  /** Форма источника для тени; `null` — передавать нечего. Чтения могут бросить. */
  #kindOf(element: object): ShadowSourceKind | null {
    const source: ElementSource = this.#environment.candidates.sourceOf(element as Element);
    switch (source.kind) {
      case "file":
      case "hls":
        // Запись HLS под тенью — как файл (решения 186/187).
        return "file";
      case "media_source":
      case "worker_media_source":
        return "media_source";
      default:
        return null;
    }
  }

  /** Поставить тень и спросить фон. `false` — видео задерживать нельзя. */
  #hold(element: object): boolean {
    const environment = this.#environment;
    const { dom, seam, registry } = environment;
    if (seam.isShadowed(element) || this.#held.has(element) || this.#held.size >= MAX_HELD_VIDEOS) return false;
    if (environment.isProtected(element) || dom.isLooping(element)) return false;
    const kind = this.#kindOf(element);
    if (kind === null) return false;
    const video = element as HTMLMediaElement;
    const declined = this.#declined.get(element);
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
    });
    const entry: HeldEntry = {
      key,
      session,
      connect: (emit) => {
        forward = emit;
      },
    };
    this.#held.set(element, entry);
    session.takeOverPendingPlays(this.#awaitingMetadata.get(element)?.splice(0) ?? []);
    environment.send({ kind: "next_video_held", element: key, candidates });
    return true;
  }

  #remember(element: object): void {
    this.#declined.set(element, this.#fingerprintOf(element));
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
