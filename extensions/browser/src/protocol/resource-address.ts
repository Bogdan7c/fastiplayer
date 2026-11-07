// Адрес-заглушка номерка ресурса HLS (протокол 1.1, решение 228). Зеркало resource_address.rs.
//
// Фон переписывает каждый адрес в плейлисте HLS на `https://resource.fastiplayer-bridge.invalid/<N>`,
// где `<N>` - номерок десятичным числом. Домен `.invalid` зарезервирован (RFC 2606) и никуда не ведёт.
//
// Разбор строгий и НЕ через `URL`: тот нормализует (регистр хоста, `:443`, `%`-коды, точки пути),
// а у одного номерка должен быть ровно один адрес. Ровно схема `https`, ровно этот хост, без порта,
// имени пользователя, запроса и фрагмента, путь - `/` и номерок без ведущих нулей (кроме `0`).
// Общий список верных и неверных адресов - fixtures/resource-addresses.json.

import type { ResourceHandle } from "./stream.ts";

/** Хост адреса-заглушки. */
export const RESOURCE_ADDRESS_HOST = "resource.fastiplayer-bridge.invalid";

/** Начало адреса-заглушки до номерка. */
const RESOURCE_ADDRESS_PREFIX = `https://${RESOURCE_ADDRESS_HOST}/`;

/** Наибольший номерок (u32). */
const MAX_RESOURCE_HANDLE = 0xffff_ffff;

/** Десятичное число из ASCII-цифр без ведущих нулей (кроме самого «0»). */
const CANONICAL_DECIMAL = /^(?:0|[1-9][0-9]*)$/;

/** Адрес-заглушка номерка. */
export function formatResourceAddress(handle: ResourceHandle): string {
  return `${RESOURCE_ADDRESS_PREFIX}${handle}`;
}

/** Номерок из адреса-заглушки; `undefined` - адрес не того вида. */
export function parseResourceAddress(address: string): ResourceHandle | undefined {
  if (!address.startsWith(RESOURCE_ADDRESS_PREFIX)) return undefined;
  const digits = address.slice(RESOURCE_ADDRESS_PREFIX.length);
  // `$` в JS-регулярке без флага `m` не пропускает перевод строки в конце, в отличие от многих других.
  if (!CANONICAL_DECIMAL.test(digits)) return undefined;
  // Длинная строка цифр может не влезть в u32: сравниваем через BigInt, без потери точности.
  const handle = BigInt(digits);
  return handle <= BigInt(MAX_RESOURCE_HANDLE) ? Number(handle) : undefined;
}
