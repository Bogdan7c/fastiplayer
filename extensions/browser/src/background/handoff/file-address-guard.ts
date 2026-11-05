// Проверка адреса файла `<video src>` (решение 199): страница из интернета не может послать фон в
// локальную сеть. Адрес выбирает страница, а качает фон с куками и без CORS (см. шапку
// range-fetcher.ts), поэтому роутер, принтер и `localhost` закрыты, пока человек не разрешил иное.
// Страница, которая сама живёт в локальной сети (Jellyfin, NAS), ходит туда как обычно.
//
// Чистые функции без браузера. Имена хостов — в том виде, какой даёт разбор `URL`: IPv4 приведён к
// четырём десятичным числам (`http://2130706433/` → `127.0.0.1`), IPv6 — в квадратных скобках и
// шестнадцатеричной записи (`[::ffff:127.0.0.1]` → `[::ffff:7f00:1]`).
//
// Честное ограничение (решение 199): домен, чей DNS ведёт на домашний адрес, здесь не поймать:
// имя выглядит публичным, а во что оно разрешится, фон до запроса узнать не может.

import type { LocalAddressSetting } from "../../shared/local-address-setting.ts";

/**
 * Согласие человека передать файл с локального адреса (ставит только страница экстеншена). Согласие
 * всегда про конкретный хост: человек ответил на вопрос о нём, а страница могла сменить файл.
 */
export type LocalAddressConsent = { readonly kind: "absent" } | { readonly kind: "given_for"; readonly host: string };

/** Согласия нет. */
export const NO_LOCAL_ADDRESS_CONSENT: LocalAddressConsent = { kind: "absent" };

/** Итог проверки адреса: закрытый список, а не `bool` (попапу важно отличить запрет от вопроса). */
export type FileAddressDecision =
  | { readonly kind: "allowed" }
  /** Настройка «блокировать»: файл не передаётся. */
  | { readonly kind: "refused" }
  /** Настройка «спрашивать», согласия нет: попап 10 спросит человека. */
  | { readonly kind: "needs_consent" };

/** Что сеанс помнит для повторных проверок (адрес от страницы, адрес после перенаправления). */
export interface FileAddressRule {
  /** Хост документа с элементом; прочитан изолированным миром, страница его не подделает. */
  readonly pageHost: string;
  readonly setting: LocalAddressSetting;
  /** Локальный хост, про который человек сказал «да»; `null` — согласия не было. */
  readonly consentedHost: string | null;
}

/** Окончания имён, которые по определению не ведут в интернет. */
export const LOCAL_NAME_SUFFIXES: readonly string[] = [".localhost", ".localdomain", ".local", ".internal", ".lan", ".home.arpa"];

/** Имя компьютера. */
const LOCALHOST_NAME = "localhost";

/** Десятичный IPv4 (разбор `URL` приводит к нему все формы записи). */
const IPV4_PATTERN = /^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/;

/** Диапазон IPv4: сеть и длина маски. */
interface Ipv4Range {
  readonly network: readonly [number, number, number, number];
  readonly prefixBits: number;
}

/** «Это устройство», частные сети, loopback, link-local, CGNAT, сеть замеров 198.18.0.0/15 (RFC 2544). */
const LOCAL_IPV4_RANGES: readonly Ipv4Range[] = [
  { network: [0, 0, 0, 0], prefixBits: 8 },
  { network: [10, 0, 0, 0], prefixBits: 8 },
  { network: [100, 64, 0, 0], prefixBits: 10 },
  { network: [127, 0, 0, 0], prefixBits: 8 },
  { network: [169, 254, 0, 0], prefixBits: 16 },
  { network: [172, 16, 0, 0], prefixBits: 12 },
  { network: [192, 168, 0, 0], prefixBits: 16 },
  { network: [198, 18, 0, 0], prefixBits: 15 },
];

/** Групп по 16 бит в адресе IPv6. */
const IPV6_GROUPS = 8;

/** Диапазон IPv6 по первой группе: маска и значение. */
interface Ipv6Range {
  readonly firstGroupMask: number;
  readonly firstGroupValue: number;
}

/** Уникальные локальные fc00::/7, link-local fe80::/10 и устаревшие site-local fec0::/10. */
const LOCAL_IPV6_RANGES: readonly Ipv6Range[] = [
  { firstGroupMask: 0xfe00, firstGroupValue: 0xfc00 },
  { firstGroupMask: 0xffc0, firstGroupValue: 0xfe80 },
  { firstGroupMask: 0xffc0, firstGroupValue: 0xfec0 },
];

/** Признак «внутри IPv6 лежит IPv4»: группа 5 у IPv4-mapped (`::ffff:a.b.c.d`), группа 4 у IPv4-translated (`::ffff:0:a.b.c.d`). */
const IPV4_MAPPED_MARKER = 0xffff;

/** NAT64 `64:ff9b::/96`: ведёт на встроенный IPv4. */
const NAT64_FIRST_GROUPS: readonly [number, number] = [0x64, 0xff9b];

/** Третья группа NAT64 для локального использования `64:ff9b:1::/48` (RFC 8215): вся сеть не публичная. */
const LOCAL_NAT64_THIRD_GROUP = 1;

/** Первая группа 6to4 `2002::/16`: IPv4 лежит в группах 1 и 2. */
const SIX_TO_FOUR_FIRST_GROUP = 0x2002;

function ipv4ToNumber(octets: readonly number[]): number {
  return octets.reduce((value, octet) => value * 256 + octet, 0);
}

function isInIpv4Range(address: number, range: Ipv4Range): boolean {
  const hostSpan = 2 ** (32 - range.prefixBits);
  const networkStart = ipv4ToNumber(range.network);
  return address >= networkStart && address < networkStart + hostSpan;
}

function isLocalIpv4(octets: readonly number[]): boolean {
  const address = ipv4ToNumber(octets);
  return LOCAL_IPV4_RANGES.some((range) => isInIpv4Range(address, range));
}

/** Группы IPv6 из записи без скобок; `undefined` — запись не разобрана. */
function parseIpv6Groups(text: string): number[] | undefined {
  const compressed = text.split("::");
  if (compressed.length > 2) return undefined;
  const groupsOf = (part: string | undefined): string[] => (part === undefined || part === "" ? [] : part.split(":"));
  const head = groupsOf(compressed[0]);
  const tail = groupsOf(compressed[1]);
  const missing = IPV6_GROUPS - head.length - tail.length;
  if (compressed.length === 1 ? missing !== 0 : missing < 1) return undefined;
  const zeros = compressed.length === 1 ? [] : new Array<string>(missing).fill("0");
  const groups = [...head, ...zeros, ...tail].map((group) => (/^[0-9a-f]{1,4}$/.test(group) ? Number.parseInt(group, 16) : Number.NaN));
  return groups.some(Number.isNaN) ? undefined : groups;
}

/** Четыре числа IPv4 из двух групп IPv6. */
function octetsOf(high: number, low: number): number[] {
  return [high >> 8, high & 0xff, low >> 8, low & 0xff];
}

/** IPv4, спрятанный в IPv6: `::a.b.c.d`, `::ffff:a.b.c.d`, `::ffff:0:a.b.c.d`, NAT64, 6to4. `undefined` — такого нет. */
function embeddedIpv4(groups: readonly number[]): number[] | undefined {
  const leadingZero = (count: number): boolean => groups.slice(0, count).every((group) => group === 0);
  const isCompatible = leadingZero(6);
  const isMapped = leadingZero(5) && groups[5] === IPV4_MAPPED_MARKER;
  const isTranslated = leadingZero(4) && groups[4] === IPV4_MAPPED_MARKER && groups[5] === 0;
  const isNat64 = groups[0] === NAT64_FIRST_GROUPS[0] && groups[1] === NAT64_FIRST_GROUPS[1] && groups.slice(2, 6).every((group) => group === 0);
  if (groups[0] === SIX_TO_FOUR_FIRST_GROUP) return octetsOf(groups[1] ?? 0, groups[2] ?? 0);
  if (!isCompatible && !isMapped && !isTranslated && !isNat64) return undefined;
  return octetsOf(groups[6] ?? 0, groups[7] ?? 0);
}

function isLocalNat64(groups: readonly number[]): boolean {
  return groups[0] === NAT64_FIRST_GROUPS[0] && groups[1] === NAT64_FIRST_GROUPS[1] && groups[2] === LOCAL_NAT64_THIRD_GROUP;
}

function isLocalIpv6(text: string): boolean {
  const groups = parseIpv6Groups(text);
  // Скобки есть, а адрес не разобран: разбор `URL` такого не выпускает. В сомнении — считаем локальным.
  if (groups === undefined) return true;
  if (isLocalNat64(groups)) return true;
  const embedded = embeddedIpv4(groups);
  // `::` и `::1` попадают сюда же как 0.0.0.0/8: «это устройство» и loopback.
  if (embedded !== undefined) return isLocalIpv4(embedded);
  const firstGroup = groups[0] ?? 0;
  return LOCAL_IPV6_RANGES.some((range) => (firstGroup & range.firstGroupMask) === range.firstGroupValue);
}

/** Ведёт ли имя хоста в локальную сеть (форма — как у разбора `URL`, см. шапку файла). */
export function isLocalHost(hostname: string): boolean {
  // Точка в конце — та же зона DNS: «localhost.» и «nas.local.» ничем не отличаются от имён без неё.
  const name = hostname.toLowerCase().replace(/\.$/, "");
  if (name === "") return false;
  if (name.startsWith("[") && name.endsWith("]")) return isLocalIpv6(name.slice(1, -1));
  const ipv4 = IPV4_PATTERN.exec(name);
  if (ipv4 !== null) return isLocalIpv4(ipv4.slice(1).map(Number));
  if (name === LOCALHOST_NAME || LOCAL_NAME_SUFFIXES.some((suffix) => name.endsWith(suffix))) return true;
  // Имя из одного слова («router», «nas») в интернете не встречается: это домашняя сеть.
  return !name.includes(".");
}

/**
 * Можно ли странице `pageHost` передать файл с `fileHost`. Правило решения 199: публичный файл —
 * всегда; локальный — странице, которая сама локальна; публичной странице — только при настройке
 * «спрашивать» и согласии человека.
 */
export function decideFileAddress(pageHost: string, fileHost: string, setting: LocalAddressSetting, consent: LocalAddressConsent): FileAddressDecision {
  if (!isLocalHost(fileHost) || isLocalHost(pageHost)) return { kind: "allowed" };
  if (setting === "block") return { kind: "refused" };
  // Согласие на другой хост — не согласие: человек спрашивался не про этот адрес, и вопрос будет заново.
  return consent.kind === "given_for" && consent.host === fileHost ? { kind: "allowed" } : { kind: "needs_consent" };
}

/**
 * Можно ли сеансу ходить по адресу `url`. Согласие человека относится только к тому хосту, про который
 * он сказал «да»: страница не сменит его на другой локальный адрес после вопроса.
 */
export function ruleAllowsFileUrl(rule: FileAddressRule, url: string): boolean {
  let host: string;
  try {
    host = new URL(url).hostname;
  } catch {
    return false;
  }
  const consent: LocalAddressConsent = host === rule.consentedHost ? { kind: "given_for", host } : NO_LOCAL_ADDRESS_CONSENT;
  return decideFileAddress(rule.pageHost, host, rule.setting, consent).kind === "allowed";
}

/** Какие локальные адреса сеанс вправе открыть сетевому заслону браузера (local-address-block-rules.ts). */
export type LocalNetworkAccess =
  /** Никаких: заслон стоит на всём локальном. */
  | { readonly kind: "none" }
  /** Страница сама локальная (Jellyfin, NAS): правило решения 199 разрешает ей любой адрес. */
  | { readonly kind: "any" }
  /** Только хост, на который согласился человек. */
  | { readonly kind: "host"; readonly host: string };

/** Доступ сеанса к локальной сети по правилу адреса. Владелец смысла правила — этот модуль. */
export function localNetworkAccessOf(rule: FileAddressRule): LocalNetworkAccess {
  if (isLocalHost(rule.pageHost)) return { kind: "any" };
  return rule.consentedHost !== null && isLocalHost(rule.consentedHost) ? { kind: "host", host: rule.consentedHost } : { kind: "none" };
}
