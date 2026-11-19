// Свидетель просмотра: что зритель сделал с видео каждого MediaSource документа (решение 364).
//
// Зачем: потолок «памяти без копий» отдаёт сначала уже просмотренное (journal/memory-budget.ts), а
// сам про элементы страницы ничего не знает. Этот модуль отвечает ему на языке journal-viewing.ts:
// видео пройдено, идёт на странице (и на какой секунде), уже отдано плееру, ещё не играло.
//
// Откуда знание:
//  - какой элемент несёт MediaSource — по blob-адресу, как везде в мосте (`src`/`currentSrc` элемента =
//    адрес MediaSource). Найденный элемент запоминается слабой ссылкой: его состояние читается и после
//    того, как страница убрала его из документа;
//  - «видео смотрели» — положительное свидетельство, и оно не забывается: элемент играет, у него есть
//    сыгранное (`played`), его видео идёт в окне плеера или журнал его MediaSource уже повторяли для
//    плеера. Одной паузы или позиции мало: заранее загруженный ролик тоже стоит на паузе;
//  - «уже отдано плееру» — только пока идёт передача этого элемента (повтор журнала плеер получил в её
//    начале). Элемент под тенью БЕЗ передачи задержан для плеера: повтор ему ещё предстоит, и это самое
//    ценное из ещё не игравшего — даже если тот же ролик когда-то уже побывал в плеере;
//  - «пройдено» — видео смотрели, и оно больше не текущее: элемент убран из документа либо стоит и
//    ушёл с экрана. Видео на паузе НА экране — не пройдено: зритель сам поставил паузу (или поверх идёт
//    реклама) и вернётся к этой секунде. «На экране» — тем же порогом, что у сторожа ухода ролика
//    (`REEL_ON_SCREEN_SHARE`): живой Instagram оставляет от пролистанного ролика видимую полоску, и
//    полоска — это уже не просмотр.
//
// Чего свидетель не знает и не выдумывает. Элемент ни разу не найден — отвечает «неизвестно», а не
// «пройдено» и не «ещё не играло»: он может лежать вне документа (страница готовит ролик заранее), а
// может играть внутри shadow root, куда дешёвый поиск не заглядывает. С «неизвестно» потолок ведёт себя
// как до решения 364 (самый старый MediaSource первым), то есть для таких страниц ничего не меняется.
//
// Цена чтений. Свидетеля спрашивают внутри `appendBuffer` страницы, пока документ выше потолка (на
// ленте Instagram — почти всегда), поэтому всё здесь обязано быть дешёвым:
//  - элементы ищутся одним встроенным поиском `<video>` в документе (`videoElementsInDocument`), без
//    shadow root. Полный обход всех элементов (так ищет опрос кандидатов) здесь не годится: замер в
//    Chromium 19 ноября 2026 — 8,7 мс на документе в 8000 узлов против 0,02 мс у поиска по тегу;
//  - найденный элемент запоминается; ненайденные ищутся заново не чаще раза в
//    `ELEMENT_SEARCH_INTERVAL_MS` и сразу после появления нового MediaSource (его элемента прошлый
//    поиск видеть не мог);
//  - раскладка страницы меряется только для видео на паузе и только пока оно не признано пройденным.

import type { JournalViewing } from "./journal/journal-viewing.ts";
import { VIEWING_UNKNOWN } from "./journal/journal-viewing.ts";
import type { DomReaders } from "./dom-readers.ts";
import type { MediaSourceRecord } from "./mse-tracker.ts";
import { REEL_ON_SCREEN_SHARE } from "./shadow/screen-exit-watch.ts";

/**
 * Как часто можно заново искать в документе элементы ещё не найденных MediaSource: раз в секунду.
 * Страница вставляет заранее подготовленный ролик в документ задолго до того, как до него долистают,
 * а ролик ленты живёт десятки секунд: секунда опоздания порядок выкидывания не меняет. Реже искать
 * незачем — поиск по тегу стоит сотые доли миллисекунды.
 */
export const ELEMENT_SEARCH_INTERVAL_MS = 1000;

/** Что свидетелю нужно от страницы и моста. */
export interface MediaSourceViewingEnvironment {
  readonly dom: Pick<
    DomReaders,
    "isConnected" | "isPaused" | "isEnded" | "currentTimeSeconds" | "hasPlayed" | "mediaSourceUrlsOf" | "rectOf" | "viewportSize" | "videoElementsInDocument"
  >;
  /** Какая доля площади элемента на экране, 0..1 (0 — за окном, вне документа или скрыт стилями). */
  readonly visibleShareOf: (element: Element) => number;
  /** Идёт передача этого элемента: его видео играет в окне плеера, повтор журнала плеер уже получил. */
  readonly isHandedOff: (element: Element) => boolean;
  /** На элементе стоит тень (он передан либо задержан для плеера до решения о передаче). */
  readonly isUnderShadow: (element: Element) => boolean;
  /** Часы (мс) — только чтобы не обходить документ слишком часто. */
  readonly now: () => number;
}

/** Что свидетель помнит об одном MediaSource. */
interface Sighting {
  /** Элемент, который несёт этот MediaSource (`null` — ещё не найден). */
  element: WeakRef<Element> | null;
  /** Видео уже смотрели (см. шапку файла). Назад не сбрасывается. */
  watched: boolean;
  /** Видео стояло и ушло с экрана: пройдено, пока снова не заиграет. */
  leftScreen: boolean;
}

/** Итог поиска `<video>` в документе: когда был и что нашёл (blob-адрес → элемент). */
interface DocumentSearch {
  readonly atMs: number;
  readonly elementByUrl: ReadonlyMap<string, WeakRef<Element>>;
}

const PASSED: JournalViewing = { kind: "passed" };
const IN_PLAYER: JournalViewing = { kind: "in_player" };
/**
 * Элемент под тенью, а передача ещё не началась: видео задержано для плеера и уйдёт ему через мгновение,
 * повтором журнала. Это «ещё не игравшее», ближе которого к показу нет.
 */
const NEXT_FOR_PLAYER: JournalViewing = { kind: "not_started", place: { kind: "on_page", distanceFromView: () => 0 } };
/** Элемент известен, но страница убрала его из документа, так и не запустив: показать его она сейчас не может. */
const NOT_STARTED_OFF_PAGE: JournalViewing = { kind: "not_started", place: { kind: "off_page" } };

export class MediaSourceViewing {
  readonly #environment: MediaSourceViewingEnvironment;
  readonly #sightings = new WeakMap<MediaSourceRecord, Sighting>();
  #lastSearch: DocumentSearch | null = null;
  /** После последнего поиска страница завела новый MediaSource: его элемента в том поиске быть не могло. */
  #mediaSourceStartedSinceSearch = false;

  constructor(environment: MediaSourceViewingEnvironment) {
    this.#environment = environment;
  }

  /** Страница завела новый MediaSource (у него появился первый SourceBuffer): прошлый поиск элементов устарел. */
  noteMediaSourceStarted(): void {
    this.#mediaSourceStartedSinceSearch = true;
  }

  /** Что известно о просмотре видео этого MediaSource прямо сейчас. Чтения страницы могут бросить — ловит вызывающий. */
  viewingOf(record: MediaSourceRecord): JournalViewing {
    // Без blob-адреса элемент не найти (MediaSource подключён иначе): честное «не знаю».
    if (record.objectUrl === null) return VIEWING_UNKNOWN;
    const sighting = this.#sightingOf(record);
    // Повтор журнала запрашивают только для плеера: видео уже уходило в его окно. Настоящий элемент при
    // этом стоит и «сыгранного» не копит, так что после конца передачи это единственное свидетельство.
    if (record.journal.hasBeenReplayed) sighting.watched = true;
    const element = this.#elementOf(record.objectUrl, sighting);
    // Элемент не найден: смотрели — значит, пройдено; иначе честное «не знаю» (почему — в шапке файла).
    if (element === null) return sighting.watched ? PASSED : VIEWING_UNKNOWN;
    if (this.#environment.isHandedOff(element)) {
      sighting.watched = true;
      sighting.leftScreen = false;
      return IN_PLAYER;
    }
    if (this.#environment.isUnderShadow(element)) {
      sighting.leftScreen = false;
      return NEXT_FOR_PLAYER;
    }
    return this.#viewingOfElement(element, sighting);
  }

  /** Просмотр видео, которое показывает (или не показывает) сама страница: элемент не под тенью. */
  #viewingOfElement(element: Element, sighting: Sighting): JournalViewing {
    const { dom } = this.#environment;
    const playing = !dom.isPaused(element) && !dom.isEnded(element);
    if (playing || dom.hasPlayed(element)) sighting.watched = true;
    const inDocument = dom.isConnected(element);
    if (!sighting.watched) {
      if (!inDocument) return NOT_STARTED_OFF_PAGE;
      return { kind: "not_started", place: { kind: "on_page", distanceFromView: () => this.#distanceFromView(element) } };
    }
    if (playing) {
      sighting.leftScreen = false;
      return { kind: "watching_on_page", playheadSeconds: this.#playheadSecondsOf(element), playback: "playing" };
    }
    // Стоит. Вне документа или уже уходил с экрана — пройдено (и останется пройденным, пока не заиграет:
    // раскладку страницы ради него больше не меряем).
    if (!inDocument || sighting.leftScreen) return PASSED;
    if (this.#environment.visibleShareOf(element) >= REEL_ON_SCREEN_SHARE) {
      // На паузе, но на экране: зритель вернётся к этой секунде.
      return { kind: "watching_on_page", playheadSeconds: this.#playheadSecondsOf(element), playback: "paused" };
    }
    sighting.leftScreen = true;
    return PASSED;
  }

  /** Позиция страницы, секунды (у элемента без метаданных — 0). */
  #playheadSecondsOf(element: Element): number {
    const seconds = this.#environment.dom.currentTimeSeconds(element);
    return Number.isFinite(seconds) ? seconds : 0;
  }

  /** Расстояние от центра окна до центра элемента, пиксели. Элемент без прямоугольника — бесконечно далеко. */
  #distanceFromView(element: Element): number {
    const { dom } = this.#environment;
    const rect = dom.rectOf(element);
    if (rect.right <= rect.left || rect.bottom <= rect.top) return Number.POSITIVE_INFINITY;
    const viewport = dom.viewportSize();
    return Math.hypot((rect.left + rect.right) / 2 - viewport.width / 2, (rect.top + rect.bottom) / 2 - viewport.height / 2);
  }

  #sightingOf(record: MediaSourceRecord): Sighting {
    const known = this.#sightings.get(record);
    if (known !== undefined) return known;
    const sighting: Sighting = { element: null, watched: false, leftScreen: false };
    this.#sightings.set(record, sighting);
    return sighting;
  }

  /** Элемент, который несёт MediaSource с этим blob-адресом (`null` — не найден). */
  #elementOf(objectUrl: string, sighting: Sighting): Element | null {
    // Элемент мог получить другой источник (один `<video>` на всю ленту): тогда он уже не наш. То же
    // касается найденного прошлым поиском: с тех пор страница могла сменить источник.
    const stillCarries = (element: Element): boolean => this.#environment.dom.mediaSourceUrlsOf(element).includes(objectUrl);
    const remembered = sighting.element?.deref();
    if (remembered !== undefined && stillCarries(remembered)) return remembered;
    sighting.element = null;
    const found = this.#currentSearch().elementByUrl.get(objectUrl)?.deref();
    if (found === undefined || !stillCarries(found)) return null;
    sighting.element = new WeakRef(found);
    return found;
  }

  /** Последний поиск `<video>` в документе, если он ещё годится, иначе новый (когда можно искать — в шапке файла). */
  #currentSearch(): DocumentSearch {
    const now = this.#environment.now();
    const last = this.#lastSearch;
    // `now < last.atMs` — часы пошли назад (перевод времени): прошлому поиску верить нечем.
    const lastIsFresh = last !== null && now >= last.atMs && now - last.atMs < ELEMENT_SEARCH_INTERVAL_MS;
    if (last !== null && lastIsFresh && !this.#mediaSourceStartedSinceSearch) return last;
    const elementByUrl = new Map<string, WeakRef<Element>>();
    for (const element of this.#environment.dom.videoElementsInDocument()) {
      for (const url of this.#environment.dom.mediaSourceUrlsOf(element)) elementByUrl.set(url, new WeakRef(element));
    }
    const search: DocumentSearch = { atMs: now, elementByUrl };
    this.#lastSearch = search;
    this.#mediaSourceStartedSinceSearch = false;
    return search;
  }
}
