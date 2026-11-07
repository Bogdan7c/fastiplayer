// Переписчик плейлистов HLS (сессия 10a2, решение 228). НЕ второй движок HLS - только подстановка
// строк по БЕЛОМУ СПИСКУ: плееру уходит лишь то, что проверено и пересобрано (tag-allowlist.ts), а
// каждый адрес становится адресом-заглушкой номерка. Плеер настоящих адресов не видит (приватность).
//
// Что делает:
// - принимает текст, чья первая строка (после BOM и пробелов) - ровно `#EXTM3U`;
// - оставляет теги из белого списка (tag-allowlist.ts), прочее - комментарии, DATERANGE,
//   SESSION-DATA, CONTENT-STEERING, неизвестные и кривые теги - выбрасывает;
// - переписывает строки-адреса и `URI="..."` в тегах с атрибутами; относительные адреса считает от
//   адреса плейлиста ПОСЛЕ перенаправлений;
// - адрес, который выдать нельзя (чужая схема, локальный адрес, заглушка от страницы), заменяет
//   номерком REFUSED_RESOURCE_HANDLE - плеер получит на него `refused`;
// - подставляет переменные EXT-X-DEFINE (NAME/VALUE и QUERYPARAM) ТОЛЬКО в адреса (строки-адреса и
//   URI): в другие значения переменная (например, токен) не попадёт; сами DEFINE убирает;
// - приводит концы строк к `\n`.
//
// Ограничения (честный отказ `ok:false`, а не догадка): EXT-X-DEFINE IMPORT (нужны переменные
// мастера), необъявленная переменная, неверный DEFINE, и нарушение в тегах KEY / SESSION-KEY / MAP:
// выбросить их молча значило бы сменить смысл потока (зашифрованное играло бы как открытое).
// В остальных тегах нарушение - выброс строки.

import type { ResourceHandle } from "../../../protocol/index.ts";
import { formatResourceAddress } from "../../../protocol/index.ts";
import { parseAttributeList, type Attribute } from "./attribute-list.ts";
import type { ResourceKind } from "./handle-desk.ts";
import { PLAYLIST_SIGNATURE, REFUSED_RESOURCE_HANDLE } from "./limits.ts";
import {
  ATTRIBUTE_TAGS,
  checkAttributes,
  formatAttributeTag,
  isNoValueTag,
  isScalarTag,
  MEANING_CRITICAL_TAGS,
  rebuildScalarTag,
  TAG_NAME,
  URI_ATTRIBUTE,
} from "./tag-allowlist.ts";
import { uriKindOfTag } from "./tag-uri-kinds.ts";

/** Выдача номерка: тут же проверка допуска адреса и запись в стойку. `undefined` - адрес не выдать. */
export type IssueResource = (absoluteUrl: string, kind: ResourceKind) => ResourceHandle | undefined;

export type PlaylistRewriteFailure =
  /** Текст не начинается с #EXTM3U: это не плейлист, решает вызывающий. */
  | "not_a_playlist"
  /** EXT-X-DEFINE IMPORT: нужны переменные мастер-плейлиста. */
  | "define_import_unsupported"
  /** Неверный EXT-X-DEFINE (нет полей, повтор имени, плохое имя, QUERYPARAM без значения в адресе). */
  | "invalid_define"
  /** Ссылка `{$имя}` на необъявленную переменную. */
  | "undefined_variable"
  /** Нарушение в KEY / SESSION-KEY / MAP: молчаливый выброс сменил бы смысл потока. */
  | "malformed_attributes"
  /** Битый EXTINF или EXT-X-BYTERANGE: без них сегмент остался бы без длительности или без диапазона. */
  | "malformed_segment_tag";

export type PlaylistRewrite = { readonly ok: true; readonly text: string } | { readonly ok: false; readonly reason: PlaylistRewriteFailure };

/** Результат разбора одной строки. */
type LineResult = { readonly kind: "keep"; readonly line: string } | { readonly kind: "drop" } | { readonly kind: "fail"; readonly reason: PlaylistRewriteFailure };

/** Всё, что нужно разбору строк плейлиста. */
interface RewriteContext {
  readonly playlistUrl: string;
  readonly issue: IssueResource;
  /** Мастер-плейлист или медиа-плейлист: от этого зависит вид ресурса (плейлист/медиа). */
  readonly isMaster: boolean;
  /** Переменные EXT-X-DEFINE, объявленные выше по тексту. */
  readonly variables: Map<string, string>;
}

const BOM = "\uFEFF";
const LINE_BREAK = /\r\n|\r|\n/;
/** Ссылка на переменную `{$name}` (RFC 8216bis, 4.4.2.3). */
const VARIABLE_REFERENCE = /\{\$([A-Za-z0-9_-]+)\}/g;
/** Допустимое имя переменной в EXT-X-DEFINE. */
const VARIABLE_NAME = /^[A-Za-z0-9_-]+$/;

/** Теги, по которым плейлист признаётся мастер-плейлистом. */
const MASTER_ONLY_TAGS: ReadonlySet<string> = new Set(["#EXT-X-STREAM-INF", "#EXT-X-MEDIA", "#EXT-X-I-FRAME-STREAM-INF"]);
const DEFINE_TAG = "#EXT-X-DEFINE";

/**
 * Скалярные теги, которые задают сам сегмент: без длительности (EXTINF) или диапазона (BYTERANGE)
 * плеер играл бы не то, что написано. Их порча — отказ плейлисту, а не выброс строки.
 */
const SEGMENT_DEFINING_TAGS: ReadonlySet<string> = new Set(["#EXTINF", "#EXT-X-BYTERANGE"]);
const MALFORMED_SEGMENT_TAG: LineResult = { kind: "fail", reason: "malformed_segment_tag" };

/** Заменить плейлист на вариант с номерками. `playlistUrl` - конечный адрес после перенаправлений. */
export function rewritePlaylist(text: string, playlistUrl: string, issue: IssueResource): PlaylistRewrite {
  const withoutBom = text.startsWith(BOM) ? text.slice(BOM.length) : text;
  const lines = withoutBom.trimStart().split(LINE_BREAK).map((line) => line.trim());
  // Первая строка - ровно метка: `#EXTM3U что-то` плейлистом не считается.
  if (lines[0] !== PLAYLIST_SIGNATURE) return { ok: false, reason: "not_a_playlist" };

  const context: RewriteContext = { playlistUrl, issue, isMaster: lines.some(isMasterOnlyTag), variables: new Map() };
  const output: string[] = [PLAYLIST_SIGNATURE];
  for (const line of lines.slice(1)) {
    const result = rewriteLine(line, context);
    if (result.kind === "fail") return { ok: false, reason: result.reason };
    if (result.kind === "keep") output.push(result.line);
  }
  return { ok: true, text: `${output.join("\n")}\n` };
}

/** Имя тега строки `#EXT-X-NAME:...` -> `#EXT-X-NAME` (без нормализации: пробел перед `:` портит имя). */
function tagNameOf(line: string): string {
  const colon = line.indexOf(":");
  return colon === -1 ? line : line.slice(0, colon);
}

function isMasterOnlyTag(line: string): boolean {
  return MASTER_ONLY_TAGS.has(tagNameOf(line));
}

function rewriteLine(line: string, context: RewriteContext): LineResult {
  if (line.length === 0) return { kind: "drop" };
  // Всё, что начинается с `#`, - тег или комментарий: адресом быть не может.
  if (line.startsWith("#")) return rewriteTagLine(line, context);
  return rewriteUriLine(line, context);
}

/** Строка-адрес (сегмент или вариант мастера). */
function rewriteUriLine(line: string, context: RewriteContext): LineResult {
  const substituted = substituteVariables(line, context.variables);
  if (substituted === undefined) return { kind: "fail", reason: "undefined_variable" };
  const kind: ResourceKind = context.isMaster ? "playlist" : "media";
  return { kind: "keep", line: addressFor(substituted, kind, context) };
}

/** Тег: только по белому списку, остальное выбрасывается. */
function rewriteTagLine(line: string, context: RewriteContext): LineResult {
  const colon = line.indexOf(":");
  const name = tagNameOf(line);
  // Комментарии, имена с пробелами и регистром, неизвестные оформления - вон.
  if (!TAG_NAME.test(name)) return { kind: "drop" };
  if (name === DEFINE_TAG) return colon === -1 ? { kind: "fail", reason: "invalid_define" } : applyDefine(line.slice(colon + 1), context);
  if (colon === -1) {
    if (SEGMENT_DEFINING_TAGS.has(name)) return MALFORMED_SEGMENT_TAG;
    return isNoValueTag(name) ? { kind: "keep", line } : { kind: "drop" };
  }
  const value = line.slice(colon + 1);
  if (isScalarTag(name)) {
    const rebuilt = rebuildScalarTag(name, value);
    // Выброс тега, задающего сегмент, сменил бы смысл плейлиста — честный отказ, как у KEY и MAP.
    return rebuilt.kind === "drop" && SEGMENT_DEFINING_TAGS.has(name) ? MALFORMED_SEGMENT_TAG : rebuilt;
  }
  if (ATTRIBUTE_TAGS.has(name)) return rewriteAttributeTag(name, value, context);
  return { kind: "drop" };
}

/** Тег со списком атрибутов: проверка, подстановка в URI, пересборка. */
function rewriteAttributeTag(name: string, attributeText: string, context: RewriteContext): LineResult {
  const checked = checkAttributes(attributeText);
  if (checked.kind === "invalid") {
    return MEANING_CRITICAL_TAGS.has(name) ? { kind: "fail", reason: "malformed_attributes" } : { kind: "drop" };
  }
  const kind = uriKindOfTag(name, context.isMaster);
  const rewritten: Attribute[] = [];
  for (const attribute of checked.attributes) {
    if (attribute.name !== URI_ATTRIBUTE) {
      rewritten.push(attribute);
      continue;
    }
    // Переменные подставляются ТОЛЬКО в адреса.
    const substituted = substituteVariables(attribute.value, context.variables);
    if (substituted === undefined) return { kind: "fail", reason: "undefined_variable" };
    rewritten.push({ ...attribute, value: addressFor(substituted, kind, context) });
  }
  return { kind: "keep", line: formatAttributeTag(name, rewritten) };
}

/**
 * Адрес-заглушка для ссылки: относительная ссылка считается от плейлиста, номерок выдаёт `issue`.
 * Не http(s), порча адреса и отказ выдачи дают номерок «отказ».
 */
function addressFor(reference: string, kind: ResourceKind, context: RewriteContext): string {
  const refused = formatResourceAddress(REFUSED_RESOURCE_HANDLE);
  if (reference.length === 0) return refused;
  let resolved: URL;
  try {
    resolved = new URL(reference, context.playlistUrl);
  } catch {
    return refused;
  }
  if (resolved.protocol !== "http:" && resolved.protocol !== "https:") return refused;
  const handle = context.issue(resolved.href, kind);
  return handle === undefined ? refused : formatResourceAddress(handle);
}

/** Подставить `{$имя}`; `undefined` - ссылка на необъявленную переменную. */
function substituteVariables(text: string, variables: ReadonlyMap<string, string>): string | undefined {
  let undefinedVariable = false;
  const result = text.replace(VARIABLE_REFERENCE, (reference, variableName: string) => {
    const value = variables.get(variableName);
    if (value === undefined) {
      undefinedVariable = true;
      return reference;
    }
    return value;
  });
  return undefinedVariable ? undefined : result;
}

/** EXT-X-DEFINE: запомнить переменную и убрать строку. */
function applyDefine(attributeText: string, context: RewriteContext): LineResult {
  const parsed = parseAttributeList(attributeText);
  if (!parsed.ok) return { kind: "fail", reason: "invalid_define" };
  const attributeValue = (name: string): string | undefined => parsed.attributes.find((attribute) => attribute.name === name)?.value;
  if (attributeValue("IMPORT") !== undefined) return { kind: "fail", reason: "define_import_unsupported" };

  const queryParameter = attributeValue("QUERYPARAM");
  const variableName = queryParameter ?? attributeValue("NAME");
  const variableValue = queryParameter === undefined ? attributeValue("VALUE") : queryValue(context.playlistUrl, queryParameter);
  if (variableName === undefined || variableValue === undefined || !VARIABLE_NAME.test(variableName) || context.variables.has(variableName)) {
    return { kind: "fail", reason: "invalid_define" };
  }
  context.variables.set(variableName, variableValue);
  return { kind: "drop" };
}

/** Значение параметра запроса адреса плейлиста; `undefined` - параметра нет. */
function queryValue(playlistUrl: string, parameterName: string): string | undefined {
  try {
    return new URL(playlistUrl).searchParams.get(parameterName) ?? undefined;
  } catch {
    return undefined;
  }
}
