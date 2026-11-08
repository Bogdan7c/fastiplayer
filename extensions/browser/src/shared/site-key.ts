// Ключ сайта для правил режима 2: один на правила сайтов, стоп-лист и списки фильтров видео.
// Отдельный модуль, чтобы настройки и фильтры видео брали его, не завися друг от друга по кругу.

/** Хост — только буквы, цифры, дефис и точки (IDN хранится в punycode, как его отдаёт `URL`). */
const HOST_PATTERN = /^[a-z0-9-]+(\.[a-z0-9-]+)*$/;

/**
 * Ключ сайта для правил: хост строчными, без завершающей точки и без ведущего `www.`. `null` — это не хост
 * (пусто, пробелы, порт, путь): такой ключ не записывается и не совпадает ни с чем.
 */
export function siteKeyOfHost(host: string): string | null {
  let normalized = host.trim().toLowerCase();
  if (normalized.endsWith(".")) normalized = normalized.slice(0, -1);
  if (normalized.startsWith("www.")) normalized = normalized.slice("www.".length);
  return normalized.length > 0 && HOST_PATTERN.test(normalized) ? normalized : null;
}
