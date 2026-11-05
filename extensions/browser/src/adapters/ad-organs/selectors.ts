// Селекторы и слова органов рекламы: данные в одном месте, логика читателей их не знает.
// Все факты — из живой разведки 5.11.2026 (бриф сессии 09d); сайт сменил разметку — правим только этот файл.

/** YouTube и встроенный плеер YouTube. */
export const YOUTUBE_ORGANS = {
  /** Хосты, где живёт плеер (сам сайт и встраиваемый). */
  hostSuffixes: ["youtube.com", "youtube-nocookie.com"],
  /** Плеер; пока идёт реклама, он получает класс `adShowingClass`. */
  playerSelector: "#movie_player",
  adShowingClass: "ad-showing",
  /** Счётчик «1 из 2»: язык интерфейса любой, берём только числа. */
  podCounterSelector: ".ytp-ad-pod-index",
  /** Кнопка пропуска: лежит в DOM с начала рекламы скрытой, показывается примерно на 5-й секунде. */
  skipButtonSelector: ".ytp-skip-ad-button",
} as const;

/** Google IMA SDK: весь интерфейс рекламы — в iframe `imasdk.googleapis.com`. */
export const IMA_ORGANS = {
  hostSuffixes: ["imasdk.googleapis.com"],
  /** Контейнер интерфейса рекламы: есть и показан — идёт реклама. */
  containerSelector: ".videoAdUi",
  /** Текст-число отсчёта «5»…«1». */
  countdownSelector: ".videoAdUiPreSkipText",
  /** Кнопка пропуска (текст зависит от языка, поэтому по классу). */
  skipButtonSelector: "button.videoAdUiSkipButton",
} as const;

/**
 * PlayerJS (Ashdi, Tortuga и другие): всё в `<pjsdiv>` без классов и id, различать можно только по тексту.
 * Слова трёх языков: украинский, русский, английский. Сравнение без регистра, пробелы сжаты.
 */
export const PLAYERJS_ORGANS = {
  rootSelector: "pjsdiv",
  /** «Реклама 1/2»: слово, потом «номер/размер блока». */
  counterWords: ["реклама", "реклами", "ad", "ads", "advertisement"],
  /** «Пропустити через 15»: отсчёт до кнопки. */
  countdownPhrases: ["пропустити через", "пропустить через", "skip in", "skip ad in"],
  /** Единицы после числа отсчёта; пусто тоже можно. */
  countdownUnits: ["с", "c", "сек", "s", "sec"],
  /** Сама кнопка: лист с этим текстом целиком. */
  skipPhrases: ["пропустити", "пропустить", "skip", "skip ad"],
} as const;
