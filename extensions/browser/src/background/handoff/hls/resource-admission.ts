// Допуск адреса в стойку номерков (сессия 10a2, решение 199). Адреса в плейлисте выбирает ЧУЖОЙ
// сервер (или страница), а качает фон - с куками пользователя и правами экстеншена. Поэтому в
// стойку попадает только то, что прошло эту проверку. Функция чистая: правило сеанса приходит
// параметром.

import { RESOURCE_ADDRESS_HOST } from "../../../protocol/index.ts";
import { MAX_FILE_SOURCE_URL_LENGTH } from "../../../shared/page-message-validation.ts";
import { ruleAllowsFileUrl, type FileAddressRule } from "../file-address-guard.ts";

/** Схемы, которые фон вообще качает. */
const ADMITTED_PROTOCOLS: readonly string[] = ["http:", "https:"];

/**
 * Можно ли выдать номерок на `url`: абсолютный http(s), без имени и пароля, не длиннее
 * MAX_FILE_SOURCE_URL_LENGTH, не адрес-заглушка (страница не протащит «чужой номерок») и разрешён
 * правилом локальной сети сеанса.
 */
export function admitsResourceUrl(url: string, rule: FileAddressRule): boolean {
  if (url.length === 0 || url.length > MAX_FILE_SOURCE_URL_LENGTH) return false;
  let parsed: URL;
  try {
    parsed = new URL(url);
  } catch {
    // Не абсолютный адрес (или мусор): выдавать нечего.
    return false;
  }
  if (!ADMITTED_PROTOCOLS.includes(parsed.protocol)) return false;
  if (parsed.username !== "" || parsed.password !== "") return false;
  if (parsed.hostname === RESOURCE_ADDRESS_HOST) return false;
  return ruleAllowsFileUrl(rule, url);
}
