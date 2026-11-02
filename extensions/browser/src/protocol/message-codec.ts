// Строгий разбор и запись JSON-сообщений протокола. Зеркало codec.rs.
//
// Все JSON-сообщения - объекты с полем `type`, остальные поля - содержимое.
// Запись компактная, поля в порядке объявления: одно и то же сообщение всегда
// даёт одни и те же байты, поэтому общие фикстуры сверяются байт в байт.
//
// Порядок проверок при разборе (как в Rust, от него зависит вид причины):
// размер -> UTF-8 -> синтаксис всего текста -> корень - объект -> поле `type`
// (нет / не строка / повтор) -> известный `type` -> поля сообщения -> проверки
// между полями.

import {
  type DecodeResult,
  type EncodeResult,
  ProtocolFailure,
  failureToError,
  reject,
  succeed,
} from "./errors.ts";
import { parseStrictJson } from "./json/parse.ts";
import type { ParsedJson } from "./json/value.ts";
import { MAX_JSON_MESSAGE_BYTES } from "./limits.ts";
import type { TaggedUnionCodec } from "./schema.ts";

/** Вид JSON-сообщений одного направления одного участка моста. */
export interface MessageKind<Message> {
  /** Кодек с известными значениями `type`. */
  readonly codec: TaggedUnionCodec<Message>;
  /** Проверки между полями, которые не выразить кодеком одного поля. */
  readonly validate: (message: Message) => void;
}

/** Входной текст сообщения: строка (текстовая рамка WebSocket) или байты (native messaging, фикстуры). */
export type MessageInput = string | Uint8Array;

const TYPE_FIELD = "type";

// `fatal: true` - невалидный UTF-8 даёт исключение (вид `not_utf8`);
// `ignoreBOM: true` - BOM не отбрасывается молча: в Rust он остаётся символом и ломает JSON.
const strictUtf8Decoder = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true });
const utf8Encoder = new TextEncoder();

/**
 * Проверяет поле `type` как «зонд» Rust: нет / не строка / повтор. Остальные поля
 * зонд пропускает, не читая значений (одиночный суррогат и число вне f64 в них
 * пока не ошибка), но ключи читает по-настоящему.
 */
function probeMessageType(parsed: ParsedJson): string {
  const root = parsed.root;
  if (root.kind !== "object") {
    throw new ProtocolFailure("not_an_object");
  }
  let messageType: string | undefined;
  for (const [index, [key, value]] of root.entries.entries()) {
    if (root.malformedKeys.has(index)) {
      throw new ProtocolFailure("syntax");
    }
    if (key !== TYPE_FIELD) continue;
    if (messageType !== undefined) {
      throw new ProtocolFailure("duplicate_field").prependPath(TYPE_FIELD);
    }
    if (value.kind === "number" && value.overflow) {
      throw new ProtocolFailure("syntax");
    }
    if (value.kind !== "string") {
      throw new ProtocolFailure("wrong_value_type").prependPath(TYPE_FIELD);
    }
    if (value.malformed) {
      throw new ProtocolFailure("syntax");
    }
    messageType = value.text;
  }
  if (messageType === undefined) {
    throw new ProtocolFailure("missing_type");
  }
  return messageType;
}

function decodeOrThrow<Message>(kind: MessageKind<Message>, input: MessageInput): Message {
  const bytes = typeof input === "string" ? utf8Encoder.encode(input) : input;
  if (bytes.length > MAX_JSON_MESSAGE_BYTES) {
    throw new ProtocolFailure("too_large");
  }
  let text: string;
  try {
    text = strictUtf8Decoder.decode(bytes);
  } catch {
    throw new ProtocolFailure("not_utf8");
  }
  const parsed = parseStrictJson(text);
  const messageType = probeMessageType(parsed);
  if (!kind.codec.knownTags.includes(messageType)) {
    throw new ProtocolFailure("unknown_type");
  }
  // Настоящее чтение значений отвергает одиночные суррогаты и числа вне f64 раньше
  // любых ошибок полей: serde сначала целиком читает содержимое сообщения.
  if (parsed.hasDeferredProblem) {
    throw new ProtocolFailure("syntax");
  }
  const message = kind.codec.decode(parsed.root);
  kind.validate(message);
  return message;
}

/**
 * Разбирает одно JSON-сообщение вида `Message` и проверяет его целиком.
 * Вид причины отказа - точный (размер, не UTF-8, не JSON, не объект, нет `type`,
 * незнакомый `type`, лишнее/недостающее/повторное поле, значение вне правил).
 */
export function decodeMessage<Message>(kind: MessageKind<Message>, input: MessageInput): DecodeResult<Message> {
  try {
    return succeed(decodeOrThrow(kind, input));
  } catch (failure) {
    return { ok: false, error: failureToError(failure) };
  }
}

/**
 * Записывает сообщение в компактный JSON и сразу проверяет записанное тем же
 * разбором: отправитель не должен слать то, что получатель отвергнет, лучше узнать
 * об этом у себя, с точной причиной (в том числе `too_large`).
 */
export function encodeMessage<Message>(kind: MessageKind<Message>, message: Message): EncodeResult<string> {
  let text: string | undefined;
  try {
    const written = kind.codec.encode(message);
    // JSON.stringify записал бы одиночный суррогат как `\ud800`, а такой текст получатель
    // отверг бы как синтаксическую ошибку. Честная причина - запрещённый символ в тексте.
    const loneSurrogatePath = findLoneSurrogate(written);
    if (loneSurrogatePath !== undefined) {
      return reject("text_forbidden_character", loneSurrogatePath);
    }
    text = JSON.stringify(written);
  } catch (failure) {
    // JSON.stringify не умеет BigInt и циклы: для протокола это значение не того типа.
    if (failure instanceof TypeError) {
      return reject("wrong_value_type", "message root");
    }
    throw failure;
  }
  if (text === undefined) {
    return reject("wrong_value_type", "message root");
  }
  const checked = decodeMessage(kind, text);
  return checked.ok ? succeed(text) : checked;
}

/** Путь к первой строке с одиночным суррогатом внутри записанного значения или `undefined`. */
function findLoneSurrogate(value: unknown, path = "message root"): string | undefined {
  if (typeof value === "string") {
    for (const character of value) {
      const codePoint = character.codePointAt(0) ?? 0;
      if (codePoint >= 0xd800 && codePoint <= 0xdfff) return path;
    }
    return undefined;
  }
  if (typeof value !== "object" || value === null) {
    return undefined;
  }
  for (const [key, child] of Object.entries(value)) {
    const found = findLoneSurrogate(child, path === "message root" ? key : `${path}.${key}`);
    if (found !== undefined) return found;
  }
  return undefined;
}
