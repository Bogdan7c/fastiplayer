// Referer закачки файла `<video src>` (решение 202). Фон качает файл с куками, но браузер не даёт
// `fetch` экстеншена поставить Referer, и сайт, который его проверяет, отказывает. Сессионное правило
// `declarativeNetRequest` ставит заголовок на один запрос самого фона.
//
// Факты опыта (Chromium 156, Firefox 158): правило с `tabIds: [-1]` и точным адресом даёт Referer на
// `fetch` фона, куки сохраняются, запросы вкладки не меняются. БЕЗ `tabIds` правило протекает на
// запросы страницы, поэтому `tabIds` обязателен и закреплён тестом.
//
// Как устроено:
// - правило живёт ровно один запрос, номера и уборку ведёт владелец набора (session-rules.ts);
// - значение считает `refererValueFor` по политике документа: только сайт-источник плюс `/`, без пути
//   страницы. Считается на каждый адрес: после перенаправления сайт может быть уже другой;
// - сбой API никогда не мешает передаче: закачка идёт без Referer, сбой пишется в журнал один раз.
//
// Ограничения:
// - правило покрывает только запрошенный адрес. Промежуточный шаг перенаправления идёт без Referer;
//   загрузчик сам переходит на конечный адрес, и следующий запрос идёт на него со своим правилом;
// - две одновременные закачки ОДНОГО адреса с разных сайтов: правила совпадут по адресу, браузер
//   применит одно из них. Окно — время до ответа сервера, такого совпадения на деле не ждём.

import { refererValueFor, type FileReferrer } from "../../shared/referrer-policy.ts";
import type { RangeFetch } from "./range-fetcher.ts";
import { NO_TAB_ID, SESSION_RULE_PRIORITY, underRequestRule, type RequestRules, type SessionRuleDraft } from "./session-rules.ts";

/** Заголовок, который ставит правило. */
const REFERER_HEADER = "Referer";

/** Виды запросов, которыми браузер помечает `fetch` фона. */
const BACKGROUND_FETCH_RESOURCE_TYPES: readonly string[] = ["xmlhttprequest", "media", "other"];

export interface FileReferrerRules {
  /** Обернуть сеть фона: каждый запрос через обёртку идёт с Referer по политике `referrer`. */
  around(fetchRange: RangeFetch, referrer: FileReferrer): RangeFetch;
}

/** Фильтр на точный адрес: фрагмент `#…` в запрос не уходит, и с ним правило бы не совпало. */
export function exactUrlFilter(url: string): string {
  const fragmentStart = url.indexOf("#");
  return `|${fragmentStart === -1 ? url : url.slice(0, fragmentStart)}|`;
}

function refererRule(url: string, value: string): SessionRuleDraft {
  return {
    priority: SESSION_RULE_PRIORITY.header,
    action: { type: "modifyHeaders", requestHeaders: [{ header: REFERER_HEADER, operation: "set", value }] },
    condition: { tabIds: [NO_TAB_ID], urlFilter: exactUrlFilter(url), resourceTypes: BACKGROUND_FETCH_RESOURCE_TYPES },
  };
}

export function createFileReferrerRules(rules: RequestRules): FileReferrerRules {
  return {
    around(fetchRange, referrer) {
      return (url, init) => {
        const value = refererValueFor(url, referrer);
        if (value === null) return fetchRange(url, init);
        return underRequestRule(rules, refererRule(url, value), init.signal, () => fetchRange(url, init));
      };
    },
  };
}
