// Потолок памяти «памяти без копий» на один документ (решение 176).
//
// Все журналы MediaSource документа делят один потолок: страница с рекламой и сменой качества
// создаёт несколько MediaSource, и считать надо то, что держит документ целиком.
//
// Кто отдаёт память первым (решение 364, «сначала просмотренное»). Когда документ выше потолка,
// память берётся по очереди, и следующая ступень трогается, только когда предыдущей нечего отдать:
//
//  0. закрытые MediaSource (страница сняла их с элементов): их журналы забываются целиком;
//  1. пройденные видео — их уже смотрели, и зритель с них ушёл (ролик ленты, пролистанный дальше).
//     Отдают всё, от начала к концу, самый старый MediaSource первым;
//  2. видео, которое уже идёт в окне плеера: повтор журнала плеер получил, дальше берёт живой поток.
//     Журнал пригодится, только если зритель вернёт видео странице и отдаст его снова;
//  3. просмотренная часть видео, которое идёт на странице: записи позади его текущей секунды. Журнал
//     выкидывает запись, только если повтор после этого всё ещё начнётся не позже чем за
//     `WATCHED_KEEP_BEHIND_SECONDS` до текущей секунды (media-journal.ts, `evictOldestIfWatchedBefore`);
//  4. видео, о просмотре которых ничего не известно (воркер; элемент не найден — он вне документа
//     или внутри shadow root; сбой чтения страницы): самый старый MediaSource первым — как было до
//     решения 364;
//  5. видео, которые ещё ни разу не играли, КРОМЕ ближайшего к показу. Первым — то, до которого
//     зрителю дальше всего: сначала элементы, убранные из документа, потом самый далёкий от окна; при равенстве —
//     самый новый MediaSource (страница заводит их по ходу ленты, новый дальше по ленте). Выбранное
//     видео отдаёт память, пока у него есть что отдать: начало у него уже потеряно, и портить второе незачем;
//  6. место вокруг текущей секунды видео, которое идёт на странице (с начала журнала к концу):
//     сначала у играющего, потом у стоящего на паузе на экране;
//  7. начало ближайшего ещё не игравшего видео — следующего ролика. Самое последнее.
//
// Почему в конце именно такой порядок. Потеря вокруг текущей секунды играющего видео проходит сама:
// страница играет дальше, и через сегмент-другой под её позицией снова лежат данные. У видео на паузе
// (фильм под рекламой) она останется до возврата зрителя. А потерянное начало следующего ролика не
// вернётся вовсе: страница держит его у себя в SourceBuffer и второй раз не пришлёт, окно после
// прокрутки будет ждать.
//
// Внутри журнала память всегда уходит с начала и по границе сегмента, init не выкидывается никогда:
// повтор остаётся цельным потоком с границы. Поэтому «начало ролика» и «место вокруг текущей секунды»
// можно сберечь только одним способом — не трогать этот журнал, пока есть что взять у других.
//
// Почему не возраст. Лента коротких видео (Instagram, замер во вкладке владельца 19 ноября 2026) держит
// живыми около 20 MediaSource — начала роликов далеко вперёд — и упирается в потолок через пару секунд
// после открытия. Дальше всё решает порядок выкидывания. Правило «самая старая запись документа»
// выкидывало начала следующих роликов (их страница качает первыми). Правило «самый старый MediaSource»
// (решение 363) работало, пока в документе жили пройденные ролики; но страница сбрасывает их пачками,
// и тогда самым старым оказывается текущий ролик, а за ним следующие по ленте — ровно то, что нужно
// сберечь. Окно плеера после прокрутки 10 с ждало данных у начала ролика. Возраст остался только
// развязкой внутри ступени.
//
// На странице с одним видео (фильм, эфир) выбирать не из чего: журнал отдаёт своё начало, как и раньше.
//
// Цена. Если документ переполнен, а пройденного, отданного плееру и просмотренного не хватает, память
// отдают начала самых дальних роликов: когда зритель до такого ролика долистает, окно будет ждать его
// данных. Журнал видео, идущего в плеере, уходит рано: «вернуть странице и тут же отдать снова» тогда
// ждёт данных. Просмотренная часть ролика на странице тоже уходит: после передачи его перезапуск с нуля
// (`loop`) данных не найдёт.

import type { JournalViewing, NotStartedPlace } from "./journal-viewing.ts";
import { MediaJournal } from "./media-journal.ts";
import { RetainedMemory } from "./retained-memory.ts";
import type { LocalMediaSourceKey } from "../../shared/handoff-contract.ts";

/**
 * Потолок памяти документа: 64 МиБ.
 *
 * Откуда число: посреди просмотра страница держит в буфере 30–120 с вперёд (YouTube — до ~2 мин).
 * 1080p VP9/AV1 — около 2,5–4 Мбит/с, звук — ~0,13 Мбит/с; 120 с × 4,1 Мбит/с ≈ 62 МБ. Значит,
 * 64 МиБ хватает, чтобы при «отдать» плеер получил всю уже скачанную полку обычного ролика. Это
 * ССЫЛКИ на память, которую страница и так выделила: пока она сама держит те же куски, лишней
 * памяти нет; потолок ограничивает только то, что мы не даём ей освободить.
 */
export const DOCUMENT_MEMORY_BUDGET_BYTES = 64 * 1024 * 1024;

/**
 * Сколько секунд позади текущей секунды страницы журнал бережёт, отдавая «просмотренное»: 10 с.
 *
 * Откуда число. Передача начинается с текущей секунды страницы, и плееру нужен ключевой кадр не позже
 * неё: сегменты сайтов длятся 2–6 с (сетка Instagram — 5 с), а время сегмента в заголовке fMP4 — это
 * время декодирования, которое опережает показ не больше чем на долю секунды. 10 с покрывают самый
 * длинный обычный сегмент с запасом и заодно привычный шаг перемотки назад (← на YouTube — 5 с,
 * двойной тап — 10 с). Цена — около 1–5 МБ на видео, которое смотрят.
 */
export const WATCHED_KEEP_BEHIND_SECONDS = 10;

/** Чем живёт журнал снаружи: что владелец знает о своём MediaSource. */
export interface JournalOwnerProbe {
  /** `true` — MediaSource собран сборщиком мусора или отсоединён от элемента (`closed`). */
  readonly isGone: () => boolean;
  /**
   * Что известно о просмотре видео этого MediaSource прямо сейчас (решение 364). Зовётся внутри
   * `appendBuffer` страницы, поэтому обязан быть дешёвым и не бросать: не знаешь — отвечай «unknown».
   * То же касается ленивого чтения в ответе (`distanceFromView`).
   */
  readonly viewing: () => JournalViewing;
}

/** Просмотр журналов на время одного наведения порядка: владельца спрашиваем один раз на журнал. */
type ViewingReader = (journal: MediaJournal) => JournalViewing;

export class DocumentMemoryBudget {
  readonly memory = new RetainedMemory();
  readonly #capBytes: number;
  /** Журналы живых MediaSource в порядке их создания (старший — первый). */
  readonly #journals = new Map<MediaJournal, JournalOwnerProbe>();
  /**
   * Ещё не игравшее видео, которое сейчас отдаёт память (ступень 5). Помним его между вызовами: начало
   * у него уже потеряно, а новый выбор каждый раз мерил бы раскладку страницы внутри её `appendBuffer`.
   */
  #notStartedGiver: MediaJournal | null = null;

  constructor(capBytes: number = DOCUMENT_MEMORY_BUDGET_BYTES) {
    this.#capBytes = capBytes;
  }

  /**
   * Новый журнал для MediaSource. Заодно забываем журналы ушедших MediaSource (их байты и init
   * держали бы память страницы зря).
   */
  createJournal(key: LocalMediaSourceKey, owner: JournalOwnerProbe): MediaJournal {
    this.#forgetGoneJournals();
    const journal = new MediaJournal(key, { memory: this.memory });
    this.#journals.set(journal, owner);
    return journal;
  }

  /** Привести память под потолок: сначала отдаётся просмотренное (порядок ступеней — в шапке файла). */
  enforce(): void {
    if (!this.#isOverCap()) return;
    // Ступень 0: закрытый MediaSource страница уже бросила, повтор его журнала никому не понадобится.
    this.#forgetGoneJournals();
    // Владельцев спрашиваем лениво и один раз за вызов: это чтения страницы внутри её `appendBuffer`.
    const viewings = new Map<MediaJournal, JournalViewing>();
    const viewingOf: ViewingReader = (journal) => {
      const known = viewings.get(journal);
      if (known !== undefined) return known;
      const viewing = this.#journals.get(journal)?.viewing() ?? { kind: "unknown" };
      viewings.set(journal, viewing);
      return viewing;
    };
    while (this.#isOverCap()) {
      // Нечего отдавать: держат только init-сегменты (их не выкидываем — без них нет повтора).
      if (!this.#evictLeastNeeded(viewingOf)) return;
    }
  }

  #isOverCap(): boolean {
    return this.memory.totalBytes > this.#capBytes;
  }

  /** Выкинуть одну запись у того, кому она нужна меньше всех. `false` — отдавать больше нечего. */
  #evictLeastNeeded(viewingOf: ViewingReader): boolean {
    const givers = [...this.#journals.keys()].filter((journal) => journal.hasEntriesToEvict());
    const [first, ...others] = givers;
    if (first === undefined) return false;
    // Один журнал (фильм, эфир): выбирать не из чего, и спрашивать страницу незачем.
    if (others.length === 0) {
      first.evictOldest();
      return true;
    }
    const evictFromEldest = (isNext: (viewing: JournalViewing) => boolean): boolean => {
      const eldest = givers.find((journal) => isNext(viewingOf(journal)));
      eldest?.evictOldest();
      return eldest !== undefined;
    };
    return (
      evictFromEldest((viewing) => viewing.kind === "passed") ||
      evictFromEldest((viewing) => viewing.kind === "in_player") ||
      this.#evictWatchedPart(givers, viewingOf) ||
      evictFromEldest((viewing) => viewing.kind === "unknown") ||
      this.#evictFromFarNotStarted(givers, viewingOf) ||
      evictFromEldest((viewing) => viewing.kind === "watching_on_page" && viewing.playback === "playing") ||
      evictFromEldest((viewing) => viewing.kind === "watching_on_page") ||
      // Сюда доходит только ближайшее ещё не игравшее видео: остальных забрала ступень 5.
      evictFromEldest((viewing) => viewing.kind === "not_started")
    );
  }

  /** Ступень 3: запись позади текущей секунды видео, которое идёт на странице. */
  #evictWatchedPart(givers: readonly MediaJournal[], viewingOf: ViewingReader): boolean {
    for (const journal of givers) {
      const viewing = viewingOf(journal);
      if (viewing.kind !== "watching_on_page") continue;
      if (journal.evictOldestIfWatchedBefore(viewing.playheadSeconds - WATCHED_KEEP_BEHIND_SECONDS)) return true;
    }
    return false;
  }

  /**
   * Ступень 5: ещё не игравшее видео, до которого зрителю дальше всего. Ближайшее к показу не трогается:
   * пока ещё не игравшее видео одно, оно и есть ближайшее.
   */
  #evictFromFarNotStarted(givers: readonly MediaJournal[], viewingOf: ViewingReader): boolean {
    const notStarted = givers.filter((journal) => viewingOf(journal).kind === "not_started");
    if (notStarted.length < 2) return false;
    if (this.#notStartedGiver === null || !notStarted.includes(this.#notStartedGiver)) {
      this.#notStartedGiver = farthestFromView(notStarted, viewingOf);
    }
    if (this.#notStartedGiver === null) return false;
    this.#notStartedGiver.evictOldest();
    return true;
  }

  #forgetGoneJournals(): void {
    for (const [journal, owner] of this.#journals) {
      if (!owner.isGone()) continue;
      journal.discard();
      this.#journals.delete(journal);
      if (this.#notStartedGiver === journal) this.#notStartedGiver = null;
    }
  }
}

/** Место ещё не игравшего видео (`null` — журнал не из таких). */
function placeOf(journal: MediaJournal, viewingOf: ViewingReader): NotStartedPlace | null {
  const viewing = viewingOf(journal);
  return viewing.kind === "not_started" ? viewing.place : null;
}

/**
 * Ещё не игравшее видео, до которого зрителю дальше всего, не считая ближайшего к показу. `notStarted` —
 * в порядке создания MediaSource, в нём не меньше двух журналов. `null` — выбрать не из чего.
 */
function farthestFromView(notStarted: readonly MediaJournal[], viewingOf: ViewingReader): MediaJournal | null {
  const newestFirst = [...notStarted].reverse();
  const onPage = newestFirst.filter((journal) => placeOf(journal, viewingOf)?.kind === "on_page");
  const newestOffPage = newestFirst.find((journal) => placeOf(journal, viewingOf)?.kind === "off_page");
  // Элемента в документе нет: дальше некуда, и раскладку страницы мерить не нужно. Ближайшее при этом —
  // видео на странице, а если на странице нет ни одного — самый старый MediaSource (он первый по ленте).
  if (newestOffPage !== undefined) return onPage.length === 0 && newestOffPage === notStarted[0] ? null : newestOffPage;

  const measured = onPage.map((journal) => ({ journal, distance: distanceOf(journal, viewingOf) }));
  // Ближайшее: наименьшее расстояние, при равенстве — самый старый MediaSource (список идёт от нового к старому).
  let nearest = measured[0];
  for (const candidate of measured) {
    if (nearest === undefined || candidate.distance <= nearest.distance) nearest = candidate;
  }
  // Самое далёкое из остальных: при равенстве — самый новый MediaSource (первый в списке).
  let farthest: { readonly journal: MediaJournal; readonly distance: number } | null = null;
  for (const candidate of measured) {
    if (candidate === nearest) continue;
    if (farthest === null || candidate.distance > farthest.distance) farthest = candidate;
  }
  return farthest?.journal ?? null;
}

/** Расстояние видео на странице от окна; неизмеримое (скрытый элемент без прямоугольника) — самое большое. */
function distanceOf(journal: MediaJournal, viewingOf: ViewingReader): number {
  const place = placeOf(journal, viewingOf);
  if (place === null || place.kind !== "on_page") return Number.POSITIVE_INFINITY;
  const measured = place.distanceFromView();
  return Number.isNaN(measured) ? Number.POSITIVE_INFINITY : measured;
}
