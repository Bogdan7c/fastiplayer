// Устройство страницы Twitch — данные в одном месте, логика адаптера их не знает (живая разведка 10.11.2026).
//
// Плеер Twitch — не глобальный объект окна, а свойство `mediaPlayerInstance` React-компонента, который стоит
// над DOM-элементом `.video-player`. Из MAIN-мира его достают по цепочке React fiber.

export const TWITCH_MARKUP = {
  /** Хост и его поддомены (`www.`, `m.`, `player.`); сравнение по суффиксу с точкой — `eviltwitch.tv` не наш. */
  hostSuffixes: ["twitch.tv"],
  /** Элемент, над которым в дереве React живёт компонент плеера; по нему же опознаём страницу. */
  playerElementSelector: ".video-player",
  /** Собственный ключ элемента, за которым React прячет fiber: `__reactFiber$<случайный хвост>`. */
  fiberKeyPrefix: "__reactFiber",
  /** Свойство компонента с плеером и функция, по которой плеер узнаётся среди прочих свойств. */
  playerPropertyName: "mediaPlayerInstance",
  playerSignatureFunction: "getQualities",
  /** Сколько узлов цепочки проверяем (в разведке плеер нашёлся на 24-м). */
  maxFiberNodes: 40,
  /** Функции плеера (методы прототипа). */
  playerFunctions: {
    qualities: "getQualities",
    currentQuality: "getQuality",
    setQuality: "setQuality",
    isAutoMode: "isAutoQualityMode",
    setAutoMode: "setAutoQualityMode",
    channelLogin: "getLoadedChannelLogin",
  },
  /** Наш ключ и подпись пункта «Auto» (подпись совпадает с подписью самого Twitch). */
  autoQuality: { id: "auto", label: "Auto" },
  /** Ключ пункта протокола: `A-Za-z0-9-_`, 1–64 символа. */
  optionIdPattern: /^[A-Za-z0-9_-]{1,64}$/u,
} as const;

/** Имена действий руки-функции этого адаптера: ключи белого списка в MAIN-мире. */
export const TWITCH_HAND_ACTIONS = {
  describe: "describe",
  selectQuality: "select_quality",
} as const;
