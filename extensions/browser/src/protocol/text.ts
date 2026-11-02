// Строки протокола с правилами допуска. Зеркало text.rs.
//
// Всё, что пришло со страницы, — данные врага (инвариант 1). Поэтому каждая
// строка проверяется по правилу своего вида. Порядок проверок фиксирован
// (пустота -> длина в байтах UTF-8 -> нижняя граница -> символы), чтобы причина
// отказа была однозначной для общих фикстур.
//
// Два вида правил:
// - текст для человека (заголовок, подписи): любой Unicode, кроме управляющих
//   символов Cc, разделителей строки/абзаца U+2028/2029 и символов переворота
//   направления текста (U+202A-202E, U+2066-2069) - ими подделывают надписи;
// - машинные строки (домен, MIME, идентификаторы, токен): узкий набор ASCII.

import { ProtocolFailure } from "./errors.ts";
import {
  MAX_CAPABILITY_TOKEN_BYTES,
  MAX_MIME_TYPE_BYTES,
  MAX_OPAQUE_ID_BYTES,
  MAX_PAGE_TITLE_BYTES,
  MAX_SESSION_TOKEN_BYTES,
  MAX_SITE_DOMAIN_BYTES,
  MAX_SOFTWARE_VERSION_BYTES,
  MAX_VARIANT_LABEL_BYTES,
  MIN_SESSION_TOKEN_BYTES,
} from "./limits.ts";
import type { Codec } from "./schema.ts";

/** Правило допуска одного вида строк. */
interface TextRule {
  readonly emptyAllowed: boolean;
  readonly minBytes: number;
  readonly maxBytes: number;
  /** Допустим ли символ (кодовая точка Unicode). */
  readonly allowedCodePoint: (codePoint: number) => boolean;
}

/** Длина строки в байтах UTF-8; одиночный суррогат считается как U+FFFD (3 байта). */
export function utf8ByteLength(text: string): number {
  let bytes = 0;
  for (const character of text) {
    const codePoint = character.codePointAt(0) ?? 0;
    if (codePoint < 0x80) bytes += 1;
    else if (codePoint < 0x800) bytes += 2;
    else if (codePoint < 0x10000) bytes += 3;
    else bytes += 4;
  }
  return bytes;
}

/** Управляющий символ категории Cc: C0, DEL и C1 (как `char::is_control` в Rust). */
function isControlCodePoint(codePoint: number): boolean {
  return codePoint <= 0x1f || (codePoint >= 0x7f && codePoint <= 0x9f);
}

/**
 * Символ допустим в тексте для человека. Метки направления U+200E/200F и
 * соединитель U+200D (эмодзи-семьи) разрешены: они нужны настоящему тексту.
 */
export function isAllowedDisplayCodePoint(codePoint: number): boolean {
  return !(
    isControlCodePoint(codePoint) ||
    codePoint === 0x2028 ||
    codePoint === 0x2029 ||
    (codePoint >= 0x202a && codePoint <= 0x202e) ||
    (codePoint >= 0x2066 && codePoint <= 0x2069)
  );
}

function isAsciiLowercase(codePoint: number): boolean {
  return codePoint >= 0x61 && codePoint <= 0x7a;
}

function isAsciiDigit(codePoint: number): boolean {
  return codePoint >= 0x30 && codePoint <= 0x39;
}

function isAsciiAlphanumeric(codePoint: number): boolean {
  return isAsciiLowercase(codePoint) || isAsciiDigit(codePoint) || (codePoint >= 0x41 && codePoint <= 0x5a);
}

/** Домен: нижний регистр, цифры, `-`, `.`, а также `[`, `]`, `:` для IPv6. */
function isAllowedDomainCodePoint(codePoint: number): boolean {
  return (
    isAsciiLowercase(codePoint) ||
    isAsciiDigit(codePoint) ||
    "-.[]:".includes(String.fromCodePoint(codePoint))
  );
}

/** MIME-тип: печатный ASCII с пробелом (U+0020-U+007E). */
function isAllowedMimeCodePoint(codePoint: number): boolean {
  return codePoint >= 0x20 && codePoint <= 0x7e;
}

/** Идентификатор и токен: алфавит base64url. */
function isAllowedOpaqueCodePoint(codePoint: number): boolean {
  return isAsciiAlphanumeric(codePoint) || codePoint === 0x2d || codePoint === 0x5f;
}

/** Метка возможности: `a-z`, `0-9`, `_`, `.`, `-`. */
function isAllowedCapabilityCodePoint(codePoint: number): boolean {
  return isAsciiLowercase(codePoint) || isAsciiDigit(codePoint) || codePoint === 0x5f || codePoint === 0x2e || codePoint === 0x2d;
}

/** Версия программы: буквы, цифры, `.`, `-`, `+`. */
function isAllowedVersionCodePoint(codePoint: number): boolean {
  return isAsciiAlphanumeric(codePoint) || codePoint === 0x2e || codePoint === 0x2d || codePoint === 0x2b;
}

/** Проверяет строку целиком; нарушение — исключение с видом причины. */
function checkText(rule: TextRule, text: string): void {
  if (text.length === 0) {
    if (rule.emptyAllowed) return;
    throw new ProtocolFailure("text_empty");
  }
  const byteLength = utf8ByteLength(text);
  if (byteLength > rule.maxBytes) {
    throw new ProtocolFailure("text_too_long");
  }
  // Нижняя граница есть только у токена: короткий легко подобрать.
  if (byteLength < rule.minBytes) {
    throw new ProtocolFailure("text_too_short");
  }
  for (const character of text) {
    const codePoint = character.codePointAt(0) ?? 0;
    // Одиночный суррогат в Rust-строке невозможен; в JS его надо отвергнуть явно.
    const isLoneSurrogate = codePoint >= 0xd800 && codePoint <= 0xdfff;
    if (isLoneSurrogate || !rule.allowedCodePoint(codePoint)) {
      throw new ProtocolFailure("text_forbidden_character");
    }
  }
}

function textCodec(rule: TextRule): Codec<string> {
  return {
    decode(json) {
      if (json.kind !== "string") {
        throw new ProtocolFailure("wrong_value_type");
      }
      checkText(rule, json.text);
      return json.text;
    },
    encode: (value) => value,
  };
}

/** Заголовок вкладки для подписи окна; может быть пустым (страница без `<title>`). */
const PAGE_TITLE_RULE: TextRule = {
  emptyAllowed: true,
  minBytes: 0,
  maxBytes: MAX_PAGE_TITLE_BYTES,
  allowedCodePoint: isAllowedDisplayCodePoint,
};

/** Заголовок вкладки. */
export const pageTitleCodec: Codec<string> = textCodec(PAGE_TITLE_RULE);

/** Домен страницы (`www.youtube.com`): единственное, что плеер знает об адресе. */
export const siteDomainCodec: Codec<string> = textCodec({
  emptyAllowed: false,
  minBytes: 1,
  maxBytes: MAX_SITE_DOMAIN_BYTES,
  allowedCodePoint: isAllowedDomainCodePoint,
});

/** MIME-тип с кодеками из `addSourceBuffer`/`changeType`. */
export const mimeTypeCodec: Codec<string> = textCodec({
  emptyAllowed: false,
  minBytes: 1,
  maxBytes: MAX_MIME_TYPE_BYTES,
  allowedCodePoint: isAllowedMimeCodePoint,
});

/** Ключ сеанса: экстеншен даёт его одному видеоэлементу в одной вкладке. */
export const sessionKeyCodec: Codec<string> = textCodec({
  emptyAllowed: false,
  minBytes: 1,
  maxBytes: MAX_OPAQUE_ID_BYTES,
  allowedCodePoint: isAllowedOpaqueCodePoint,
});

/** Идентификатор варианта в меню (озвучка, качество, субтитры). */
export const variantIdCodec: Codec<string> = sessionKeyCodec;

/** Подпись варианта в меню плеера. */
export const variantLabelCodec: Codec<string> = textCodec({
  emptyAllowed: false,
  minBytes: 1,
  maxBytes: MAX_VARIANT_LABEL_BYTES,
  allowedCodePoint: isAllowedDisplayCodePoint,
});

/** Метка возможности стороны в `hello` (`cheap_codec.h264`). */
export const capabilityTokenCodec: Codec<string> = textCodec({
  emptyAllowed: false,
  minBytes: 1,
  maxBytes: MAX_CAPABILITY_TOKEN_BYTES,
  allowedCodePoint: isAllowedCapabilityCodePoint,
});

/** Версия программы для диагностики (`0.2.0`). */
export const softwareVersionCodec: Codec<string> = textCodec({
  emptyAllowed: false,
  minBytes: 1,
  maxBytes: MAX_SOFTWARE_VERSION_BYTES,
  allowedCodePoint: isAllowedVersionCodePoint,
});

/** Одноразовый пропуск в сеанс по WebSocket: 32-128 символов base64url. */
export const sessionTokenCodec: Codec<string> = textCodec({
  emptyAllowed: false,
  minBytes: MIN_SESSION_TOKEN_BYTES,
  maxBytes: MAX_SESSION_TOKEN_BYTES,
  allowedCodePoint: isAllowedOpaqueCodePoint,
});
