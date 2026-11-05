// Сетевой заслон локальных адресов (решение 199, находка ревью 09c). Проверка адреса в
// file-address-guard.ts видит только то, что назвала страница, и ответ после перенаправлений. Но сам
// перенаправленный запрос уходит раньше: публичный адрес с 302 на `http://192.168.1.1/reboot` выполнил бы
// GET с куками и `Range`, и его побочный эффект никто бы не отменил. `fetch` тут бессилен: `redirect:
// "manual"` даёт непрозрачный ответ. Закрыть сам запрос может только браузер, поэтому здесь постоянные
// сессионные правила `block` на локальные адреса и правило `allow` на один запрос там, где разрешено.
//
// Границы правила:
// - `tabIds: [-1]` — только запросы не из вкладок;
// - `initiatorDomains` — только запросы самого экстеншена. Без него под `tabIds: [-1]` попали бы и
//   service worker сайтов: NAS с офлайн-кэшем потерял бы доступ к самому себе;
// - `resourceTypes: ["xmlhttprequest"]` — только `fetch`. WebSocket к плееру на 127.0.0.1 — другой вид
//   запроса и остаётся нетронутым (инвариант «единственный канал к плееру»).
//
// Фильтры и предел Chrome. Регулярка `regexFilter` должна укладываться в 2 КБ после компиляции (RE2), а
// одна слишком большая отвергает всё обновление целиком, и заслона нет вообще. Отсюда форма фильтров:
// - только перечисленные символы хоста: отрицание вроде `[^/?#]` в UTF-8 разворачивается в десятки
//   диапазонов и не влезает (проверено библиотекой RE2 с параметрами Chrome: 133 символа уже отказ);
// - регистр не сворачивается (`isUrlFilterCaseSensitive`): схема и хост в разобранном адресе всегда строчные;
// - диапазоны разнесены по нескольким правилам с запасом;
// - логина в адресе нет: перенаправление на адрес с учётными данными `fetch` отвергает сам.
//
// Фильтры шире `isLocalHost` у IPv6: разбирать в регулярке встроенный IPv4 слишком дорого. Поэтому заслон
// закрывает все адреса с нулями впереди, NAT64 и 6to4 целиком. Цена — литеральный IPv6-адрес с публичным
// IPv4 внутри, а такой файл на деле не встречается. Точную проверку по-прежнему делает `ruleAllowsFileUrl`.
//
// Разрешение на один запрос (`allow` с приоритетом выше `block`):
// - страница сама локальная: разрешено всё, что разрешает решение 199;
// - человек согласился: только его хост.
// Разрешение общее на фон. Если в этот же миг вторая вкладка с публичной страницы ведёт запрос на
// локальный адрес, заслон её не остановит, и останется проверка конечного адреса в загрузчике. Окно —
// время до ответа сервера при одновременном просмотре файла из локальной сети.
//
// Сбой установки заслона закачку не останавливает: остаётся проверка конечного адреса (ни байта плееру),
// сбой виден в журнале. Сбой разрешения безопасен: запрос будет заблокирован, и плеер получит честный отказ.

import { localNetworkAccessOf, LOCAL_NAME_SUFFIXES, type FileAddressRule, type LocalNetworkAccess } from "./file-address-guard.ts";
import type { RangeFetch } from "./range-fetcher.ts";
import { NO_TAB_ID, SESSION_RULE_PRIORITY, underRequestRule, type RequestRules, type SessionRule, type SessionRuleCondition, type SessionRuleDraft } from "./session-rules.ts";

/** Запросы `fetch` фона; WebSocket сюда не входит намеренно (см. шапку). */
const BACKGROUND_FETCH_ONLY: readonly string[] = ["xmlhttprequest"];

/** Начало адреса до хоста. */
const URL_BEFORE_HOST = "^https?://";

/** Конец хоста: порт и начало пути, запроса или фрагмента. */
const URL_AFTER_HOST = "(:[0-9]+)?([/?#]|$)";

/** Спецсимволы регулярного выражения: имя хоста вставляется в него как есть. */
const REGEX_SPECIAL_CHARACTERS = /[.*+?^${}()|[\]\\]/g;

function escapeForRegex(text: string): string {
  return text.replace(REGEX_SPECIAL_CHARACTERS, "\\$&");
}

function urlFilterForHostPattern(hostPattern: string): string {
  return `${URL_BEFORE_HOST}(${hostPattern})${URL_AFTER_HOST}`;
}

/** Имена: из одного слова или с локальным окончанием (список — у `isLocalHost`). */
const LOCAL_NAMES_PATTERN = `[a-z0-9_-]+\\.?|[a-z0-9_.-]+(${LOCAL_NAME_SUFFIXES.map(escapeForRegex).join("|")})\\.?`;

/** Группа IPv4 после указанных старших чисел адреса. */
const ANY_OCTET = "[0-9]+";

/** Диапазоны IPv4 из `LOCAL_IPV4_RANGES`; совпадение закреплено тестом на общей таблице адресов. */
const LOCAL_IPV4_PATTERNS: readonly string[] = [
  `(0|10|127)\\.${ANY_OCTET}\\.${ANY_OCTET}\\.${ANY_OCTET}`,
  `100\\.(6[4-9]|[7-9][0-9]|1[01][0-9]|12[0-7])\\.${ANY_OCTET}\\.${ANY_OCTET}`,
  `(169\\.254|192\\.168|198\\.1[89]|172\\.(1[6-9]|2[0-9]|3[01]))\\.${ANY_OCTET}\\.${ANY_OCTET}`,
];

/** IPv6 в скобках: нули впереди (`::`), NAT64, 6to4, `fc00::/7` и `fe80::/9`. */
const LOCAL_IPV6_PATTERN = "\\[(::|64:ff9b:|2002:|f[cd]|fe[89a-f])[0-9a-f:.]*\\]";

/** Фильтры заслона: отдельными правилами, чтобы каждое укладывалось в предел Chrome с запасом. */
export const LOCAL_ADDRESS_URL_FILTERS: readonly string[] = [LOCAL_NAMES_PATTERN, ...LOCAL_IPV4_PATTERNS, LOCAL_IPV6_PATTERN].map(urlFilterForHostPattern);

/** Первый номер постоянных правил заслона. */
const FIRST_BLOCK_RULE_ID = 1;

/** Запросы самого экстеншена (см. шапку). `extensionHost` — хост адреса страниц экстеншена. */
function ownFetches(extensionHost: string): SessionRuleCondition {
  return { tabIds: [NO_TAB_ID], initiatorDomains: [extensionHost], resourceTypes: BACKGROUND_FETCH_ONLY };
}

/** Постоянные правила заслона (ставит владелец набора, session-rules.ts). */
export function localAddressBlockRules(extensionHost: string): readonly SessionRule[] {
  return LOCAL_ADDRESS_URL_FILTERS.map((regexFilter, index) => ({
    id: FIRST_BLOCK_RULE_ID + index,
    priority: SESSION_RULE_PRIORITY.block,
    action: { type: "block" },
    condition: { ...ownFetches(extensionHost), regexFilter, isUrlFilterCaseSensitive: true },
  }));
}

/** Правило `allow` на один запрос; `undefined` — разрешать нечего. */
function allowRule(access: LocalNetworkAccess, extensionHost: string): SessionRuleDraft | undefined {
  switch (access.kind) {
    case "none":
      return undefined;
    case "any":
      return { priority: SESSION_RULE_PRIORITY.allow, action: { type: "allow" }, condition: ownFetches(extensionHost) };
    case "host":
      return {
        priority: SESSION_RULE_PRIORITY.allow,
        action: { type: "allow" },
        condition: { ...ownFetches(extensionHost), regexFilter: urlFilterForHostPattern(escapeForRegex(access.host)), isUrlFilterCaseSensitive: true },
      };
  }
}

export interface LocalAddressBlock {
  /** Обернуть сеть фона: запросы сеанса с правилом `rule` идут с разрешённым ему локальным доступом. */
  around(fetchRange: RangeFetch, rule: FileAddressRule): RangeFetch;
}

export function createLocalAddressBlock(rules: RequestRules, extensionHost: string): LocalAddressBlock {
  return {
    around(fetchRange, rule) {
      const allow = allowRule(localNetworkAccessOf(rule), extensionHost);
      if (allow === undefined) {
        return async (url, init) => {
          // Разбуженный фон ставит заслон параллельно с первым запросом: запрос идёт только за ним.
          await rules.whenPrepared(init.signal);
          init.signal.throwIfAborted();
          return fetchRange(url, init);
        };
      }
      return (url, init) => underRequestRule(rules, allow, init.signal, () => fetchRange(url, init));
    },
  };
}
