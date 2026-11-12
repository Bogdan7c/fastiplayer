// «Основной домен» адреса вкладки (сессия 16c, решение 304): тот ли это сайт после перезагрузки страницы.
//
// Правило решения 304: сравнивается основной домен адреса САМОЙ вкладки (uaserials.pro → uaserials.pro,
// www.uaserials.pro → uaserials.pro, cdn.player.uaserials.pro → uaserials.pro). Поддомен и домен фрейма с
// плеером не важны: сайт может сменить поддомен от серии к серии.
//
// Как считается основной домен (без полного списка публичных суффиксов — его в экстеншене нет, а тащить
// большую чужую таблицу ради одного сравнения незачем):
// - хост строчными, без завершающей точки;
// - IP-адрес (v4 или v6), `localhost` и хост из одного слова — сам хост целиком;
// - иначе последние две метки (`example.com`), а если последние две метки — известный многосоставной суффикс
//   из короткого списка ниже (`co.uk`, `com.ua`, `github.io`…), то последние три (`bbc.co.uk`).
//
// Предел правила (честно): суффикс, которого нет в списке, считается обычным доменом. Тогда «сайтом» станет
// весь суффикс (например, `foo.example.xx` и `bar.example.xx` сольются, если `example.xx` на деле публичный
// суффикс) — сайт выйдет ШИРЕ настоящего. Цена такой ошибки — окно плеера подождёт видео соседнего сайта на
// том же суффиксе, а не закроется сразу; чужое видео всё равно передаётся только после проверки сторожем.
// Список намеренно без российских суффиксов (правило владельца).

/**
 * Многосоставные публичные суффиксы, под которыми каждый следующий уровень — отдельный сайт. Подобраны
 * под сайты, где реально смотрят видео (украинские сериальные сайты, крупные страны), и бесплатные хостинги
 * страниц, где у каждого проекта свой поддомен.
 */
const MULTI_LABEL_PUBLIC_SUFFIXES: ReadonlySet<string> = new Set([
  // Великобритания.
  "co.uk",
  "org.uk",
  "ac.uk",
  "gov.uk",
  // Украина: общие и городские суффиксы.
  "com.ua",
  "org.ua",
  "net.ua",
  "in.ua",
  "kiev.ua",
  "kyiv.ua",
  "od.ua",
  "lviv.ua",
  "edu.ua",
  "gov.ua",
  // Другие страны.
  "com.au",
  "net.au",
  "org.au",
  "co.jp",
  "co.nz",
  "co.za",
  "com.br",
  "com.pl",
  "com.tr",
  "com.cn",
  "com.tw",
  "co.kr",
  "co.in",
  "com.mx",
  "com.ar",
  // Хостинги страниц: каждый проект — свой поддомен.
  "github.io",
  "gitlab.io",
  "netlify.app",
  "vercel.app",
  "pages.dev",
  "web.app",
  "firebaseapp.com",
  "herokuapp.com",
  "blogspot.com",
  "appspot.com",
  "workers.dev",
]);

/** IPv4 вида `192.168.0.1`: четыре числа через точку (сам `URL` уже привёл запись к этому виду). */
const IPV4_PATTERN = /^\d{1,3}(\.\d{1,3}){3}$/;

/** Хост — IP-литерал: `URL` отдаёт IPv6 в квадратных скобках (`[::1]`). */
function isIpLiteral(host: string): boolean {
  return host.startsWith("[") || IPV4_PATTERN.test(host);
}

/** Основной домен хоста по правилу шапки; пустой хост — `undefined`. */
export function baseSiteOfHost(host: string): string | undefined {
  let normalized = host.toLowerCase();
  if (normalized.endsWith(".")) normalized = normalized.slice(0, -1);
  if (normalized.length === 0) return undefined;
  // IP, `localhost` и хост из одного слова делить не на что: сайт — весь хост.
  if (isIpLiteral(normalized) || normalized === "localhost") return normalized;
  const labels = normalized.split(".");
  if (labels.length <= 2) return normalized;
  const lastTwo = labels.slice(-2).join(".");
  // Под многосоставным суффиксом сайт — на уровень глубже: `bbc.co.uk`, `user.github.io`.
  return MULTI_LABEL_PUBLIC_SUFFIXES.has(lastTwo) ? labels.slice(-3).join(".") : lastTwo;
}

/**
 * Основной домен адреса вкладки. Только `http:` и `https:`: служебные страницы браузера, `file:`,
 * `about:blank` и битый адрес сайта не имеют — `undefined`.
 */
export function baseSiteOfUrl(url: string | undefined): string | undefined {
  if (url === undefined) return undefined;
  let parsed: URL;
  try {
    parsed = new URL(url);
  } catch {
    // Битый адрес — не сайт; это ответ, а не сбой: вызывающий получит «сайт неизвестен».
    return undefined;
  }
  if (parsed.protocol !== "http:" && parsed.protocol !== "https:") return undefined;
  return baseSiteOfHost(parsed.hostname);
}

/**
 * Тот ли это сайт (решение 304). Неизвестный сайт у любого из адресов — НЕ тот же: без сайта сравнивать
 * нечего, и честнее закончить сеанс, чем ждать видео неизвестно какой страницы.
 */
export function isSameBaseSite(firstUrl: string | undefined, secondUrl: string | undefined): boolean {
  const first = baseSiteOfUrl(firstUrl);
  return first !== undefined && first === baseSiteOfUrl(secondUrl);
}
