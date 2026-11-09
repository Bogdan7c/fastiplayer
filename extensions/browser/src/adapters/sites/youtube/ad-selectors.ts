// Селекторы органов рекламы YouTube: данные в одном месте, логика читателя их не знает.
// Факты — из живой разведки 5.11.2026 (бриф сессии 09d); сайт сменил разметку — правим только этот файл.

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
