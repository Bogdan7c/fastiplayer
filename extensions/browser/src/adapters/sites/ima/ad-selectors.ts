// Селекторы органов рекламы Google IMA SDK: данные в одном месте, логика читателя их не знает.
// Факты — из живой разведки 5.11.2026 (бриф сессии 09d); сайт сменил разметку — правим только этот файл.

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
