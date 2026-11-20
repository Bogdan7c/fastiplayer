// Ленты одного элемента по списку адресов (сессия 16b3c, решение 374).
//
// Зачем. Обычную ленту (Instagram, TikTok) фон узнаёт по переезду: на каждом ролике новый `<video>`, сеанс
// переезжает на него, и после первого шага ленты раздел выучен (session-feed.ts). YouTube Shorts устроен иначе:
// ОДИН `<video>` на всю ленту, следующий ролик — новое поколение (новый MediaSource) того же элемента, а адрес
// меняется на каждом ролике. По поведению это неотличимо от сериала с номером серии в адресе (`/serial/123/4`:
// новое поколение + смена адреса), а сериал лентой быть не должен никогда (правила 16a, 306).
//
// Поэтому — список (выбор владельца, решение 374): фон знает ленту одного элемента ТОЛЬКО по известному адресу,
// никакой догадки. Плата — новую ленту такого устройства добавляют сюда руками: дописать запись в группу.
//
// Форма записи — как в стоп-листе режима 2 (`host` или `host/начало-пути`, shared/web-address-match.ts): хост
// совпадает вместе с поддоменами (`www.`, `m.`), путь — на границе сегмента.

import { feedSectionOf, type FeedSection } from "./feed-section.ts";
import { addressMatchesSiteEntry, parseWebAddress } from "../../../shared/web-address-match.ts";

/** Группа списка: кто это и какие записи. */
export interface SingleElementFeedGroup {
  readonly id: "youtube_shorts" | "bench_shorts";
  /** Для людей: что за лента. */
  readonly title: string;
  readonly entries: readonly string[];
}

/** Встроенный список лент одного элемента (решение 374). */
export const SINGLE_ELEMENT_FEED_GROUPS: readonly SingleElementFeedGroup[] = [
  {
    id: "youtube_shorts",
    title: "YouTube Shorts",
    // `youtube.com/shorts` покрывает и `www.youtube.com`, и мобильный `m.youtube.com` (поддомены).
    entries: ["youtube.com/shorts"],
  },
  {
    id: "bench_shorts",
    title: "Bench Shorts emulation",
    // Стенд моста (`crates/browser-bridge-bench`, страница `feed-edges`, форма `moved`) изображает Shorts путём
    // `/shorts/<id>` на локальных именах. Прецедент — адаптер стенда (`adapters/sites/bench-series/markup.ts`):
    // локальные хосты в продуктовом списке. Только С ПУТЁМ: голый `127.0.0.1` сделал бы лентой все страницы
    // стенда, в том числе «сериал» (строка `feed-leave-section-serial`).
    entries: ["127.0.0.1/shorts", "localhost/shorts"],
  },
];

/** Все записи встроенного списка одним списком (порядок групп сохраняется). */
export function singleElementFeedEntries(): readonly string[] {
  return SINGLE_ELEMENT_FEED_GROUPS.flatMap((group) => group.entries);
}

/**
 * Где адрес вкладки относительно лент одного элемента:
 * - `single_element_feed` — адрес из списка; `section` — раздел ленты (feed-section.ts), по нему «Назад» в истории
 *   узнаёт возврат в ту же ленту (решение 375);
 * - `elsewhere` — веб-адрес не из списка (главная, другой раздел, другой сайт);
 * - `unknown_address` — адрес не разобрать или это не веб-страница: о ленте судить нельзя, правила ухода молчат.
 */
export type SingleElementFeedPlace =
  | { readonly kind: "single_element_feed"; readonly section: FeedSection }
  | { readonly kind: "elsewhere" }
  | { readonly kind: "unknown_address" };

const ELSEWHERE: SingleElementFeedPlace = { kind: "elsewhere" };
const UNKNOWN_ADDRESS: SingleElementFeedPlace = { kind: "unknown_address" };

/** Лежит ли адрес вкладки в ленте одного элемента из списка `entries` (по умолчанию — встроенного). */
export function singleElementFeedPlaceOf(url: string, entries: readonly string[] = singleElementFeedEntries()): SingleElementFeedPlace {
  const address = parseWebAddress(url);
  if (address === undefined) return UNKNOWN_ADDRESS;
  if (!entries.some((entry) => addressMatchesSiteEntry(address, entry))) return ELSEWHERE;
  const section = feedSectionOf(url);
  // Разобранный веб-адрес раздел имеет всегда; ветка — на случай расхождения двух разборщиков URL.
  return section === undefined ? UNKNOWN_ADDRESS : { kind: "single_element_feed", section };
}

/**
 * Где вкладка сеанса относительно ленты одного элемента:
 * - `never_in_feed` — адреса из списка вкладка за сеанс не показывала: прежние правила (сериал, обычная лента);
 * - `in_feed` — последний разобранный адрес вкладки — из списка;
 * - `left_feed` — вкладка была в ленте из списка, а последний разобранный адрес — уже не из списка (решение 381).
 */
export type SingleElementFeedState = "never_in_feed" | "in_feed" | "left_feed";

/** Память сеанса о ленте одного элемента: что видели в адресах вкладки. */
export interface SingleElementFeedWatch {
  /** Браузер назвал адрес вкладки (адрес открытия — при создании). Неразборный адрес ничего не меняет. */
  addressSeen(url: string): void;
  state(): SingleElementFeedState;
  /** Раздел ленты из списка, в которой вкладка была последней (`undefined` — не была). */
  lastFeedSection(): FeedSection | undefined;
}

export function createSingleElementFeedWatch(openedUrl: string | undefined): SingleElementFeedWatch {
  let lastFeedSection: FeedSection | undefined;
  let state: SingleElementFeedState = "never_in_feed";

  function addressSeen(url: string): void {
    const place = singleElementFeedPlaceOf(url);
    if (place.kind === "single_element_feed") {
      lastFeedSection = place.section;
      state = "in_feed";
    } else if (place.kind === "elsewhere" && state !== "never_in_feed") {
      state = "left_feed";
    }
    // `unknown_address`: о ленте судить нельзя — состояние прежнее.
  }

  // Страница может открыться прямо в ленте (Shorts по ссылке), а может войти в неё позже (стенд: первый
  // `pushState` на `/shorts/…`) — поэтому учитываются и адрес открытия, и каждая новость об адресе.
  if (openedUrl !== undefined) addressSeen(openedUrl);
  return { addressSeen, state: () => state, lastFeedSection: () => lastFeedSection };
}
