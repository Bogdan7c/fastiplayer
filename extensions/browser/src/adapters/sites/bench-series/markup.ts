// Устройство страницы «сериал» стенда — данные в одном месте, логика адаптера их не знает.
//
// Адаптер стенда нужен, чтобы проверить каркас целиком на своей странице, а не на живом сайте (решение 33):
// озвучки он меняет КЛИКОМ (рука DOM), качество и следующую серию — ВЫЗОВОМ функций плеера страницы
// (рука-функция, решение 282). Живым сайтам он не отвечает: хост только локальный, и нужна метка стенда.

export const BENCH_SERIES_MARKUP = {
  /** Стенд слушает только локальные имена (`crates/browser-bridge-bench/src/site.rs`). */
  hosts: ["127.0.0.1", "localhost"],
  /** Корень плеера страницы с меткой стенда; на нём же ключи сериала и серии. */
  rootSelector: "[data-fastiplayer-bench-series]",
  seriesKeyAttribute: "data-series-key",
  episodeKeyAttribute: "data-episode-key",
  /** Кнопки озвучек: ключ озвучки в атрибуте, выбранная — `aria-pressed="true"`. */
  dubButtonSelector: "[data-dub]",
  dubKeyAttribute: "data-dub",
  pressedAttribute: "aria-pressed",
  /** Объект плеера страницы в её окне и его функции (MAIN-мир). */
  playerObject: "fastiplayerBenchSeries",
  playerFunctions: {
    qualities: "qualities",
    selectQuality: "selectQuality",
    hasNextEpisode: "hasNextEpisode",
    nextEpisode: "nextEpisode",
  },
} as const;

/** Имена действий руки-функции этого адаптера: ключи белого списка в MAIN-мире. */
export const BENCH_SERIES_HAND_ACTIONS = {
  qualities: "qualities",
  selectQuality: "select_quality",
  hasNextEpisode: "has_next_episode",
  nextEpisode: "next_episode",
} as const;
