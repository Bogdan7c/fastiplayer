// Доступ к API браузера. В Chrome и Firefox MV3 глобал `chrome` есть в обоих, поэтому используем его.
// `typeof browser` признаком Firefox НЕ служит (в Chrome 156 глобал `browser` тоже есть, решение 00c):
// настоящий признак — наличие runtime.getBrowserInfo (есть только в Firefox).
export const browserApi: typeof chrome = chrome;

export function isFirefox(): boolean {
  // getBrowserInfo нет в типах @types/chrome — это API только Firefox, поэтому проверяем через неизвестный тип.
  const runtime: unknown = browserApi.runtime;
  return typeof (runtime as { getBrowserInfo?: unknown }).getBrowserInfo === "function";
}
