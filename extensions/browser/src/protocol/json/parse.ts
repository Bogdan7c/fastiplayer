// Маленький строгий JSON-разборщик (RFC 8259) под правила протокола.
//
// Почему не `JSON.parse`: он молча берёт ПОСЛЕДНИЙ из повторных ключей, теряет
// порядок «дубликат или нет» и округляет целые больше 2^53. Протокол строгий
// (решение 86): повтор поля — ошибка `duplicate_field`, а число читается из
// исходного текста. Любое нарушение синтаксиса — вид `syntax` (как у serde_json:
// мусор после значения, оборванный текст, ведущие нули, `NaN`, комментарии,
// хвостовые запятые, лишние управляющие символы в строках).

import { ProtocolFailure } from "../errors.ts";
import type { JsonValue, ParsedJson } from "./value.ts";

/**
 * Наибольшая вложенность массивов и объектов. У serde_json предел 128 уровней
 * (128-й уже ошибка); держим тот же, чтобы глубокие входы отвергались одинаково.
 */
const MAX_NESTING_DEPTH = 127;

/** Пробельные символы JSON: только эти четыре. */
function isJsonWhitespace(code: number): boolean {
  return code === 0x20 || code === 0x0a || code === 0x0d || code === 0x09;
}

function isDigit(code: number): boolean {
  return code >= 0x30 && code <= 0x39;
}

/** Состояние разбора одного текста. */
class StrictJsonParser {
  private readonly source: string;
  private position = 0;
  /** Встретился ли помеченный узел (одиночный суррогат, число вне f64). */
  private deferredProblem = false;
  /** Сколько строк с одиночным суррогатом уже встретилось (по разнице видно, malformed ли очередная). */
  private malformedStrings = 0;

  constructor(source: string) {
    this.source = source;
  }

  /** Разбирает значение целиком: после него допустимы только пробелы. */
  parseDocument(): ParsedJson {
    this.skipWhitespace();
    const root = this.parseValue(0);
    this.skipWhitespace();
    if (this.position < this.source.length) {
      throw syntaxError();
    }
    return { root, hasDeferredProblem: this.deferredProblem };
  }

  private skipWhitespace(): void {
    while (this.position < this.source.length && isJsonWhitespace(this.source.charCodeAt(this.position))) {
      this.position += 1;
    }
  }

  /** Код текущего символа или -1 в конце текста. */
  private peekCode(): number {
    return this.position < this.source.length ? this.source.charCodeAt(this.position) : -1;
  }

  private parseValue(nesting: number): JsonValue {
    switch (this.peekCode()) {
      case 0x7b: // {
        return this.parseObject(nesting);
      case 0x5b: // [
        return this.parseArray(nesting);
      case 0x22: // "
        return this.parseStringValue();
      case 0x74: // t
        return this.parseLiteral("true");
      case 0x66: // f
        return this.parseLiteral("false");
      case 0x6e: // n
        return this.parseLiteral("null");
      default:
        return this.parseNumber();
    }
  }

  private parseStringValue(): JsonValue {
    const problemsBefore = this.malformedStrings;
    const text = this.parseString();
    return { kind: "string", text, malformed: this.malformedStrings !== problemsBefore };
  }

  private parseLiteral(word: "true" | "false" | "null"): JsonValue {
    if (!this.source.startsWith(word, this.position)) {
      throw syntaxError();
    }
    this.position += word.length;
    return { kind: "literal", text: word };
  }

  private parseObject(nesting: number): JsonValue {
    if (nesting >= MAX_NESTING_DEPTH) {
      throw syntaxError();
    }
    this.position += 1; // {
    const entries: (readonly [string, JsonValue])[] = [];
    const malformedKeys = new Set<number>();
    this.skipWhitespace();
    if (this.peekCode() === 0x7d) {
      this.position += 1;
      return { kind: "object", entries, malformedKeys };
    }
    for (;;) {
      this.skipWhitespace();
      if (this.peekCode() !== 0x22) {
        throw syntaxError();
      }
      const malformedBefore = this.malformedStrings;
      const key = this.parseString();
      if (this.malformedStrings !== malformedBefore) {
        malformedKeys.add(entries.length);
      }
      this.skipWhitespace();
      if (this.peekCode() !== 0x3a) {
        throw syntaxError();
      }
      this.position += 1; // :
      this.skipWhitespace();
      entries.push([key, this.parseValue(nesting + 1)]);
      this.skipWhitespace();
      const separator = this.peekCode();
      this.position += 1;
      if (separator === 0x7d) {
        return { kind: "object", entries, malformedKeys };
      }
      if (separator !== 0x2c) {
        throw syntaxError();
      }
    }
  }

  private parseArray(nesting: number): JsonValue {
    if (nesting >= MAX_NESTING_DEPTH) {
      throw syntaxError();
    }
    this.position += 1; // [
    const items: JsonValue[] = [];
    this.skipWhitespace();
    if (this.peekCode() === 0x5d) {
      this.position += 1;
      return { kind: "array", items };
    }
    for (;;) {
      this.skipWhitespace();
      items.push(this.parseValue(nesting + 1));
      this.skipWhitespace();
      const separator = this.peekCode();
      this.position += 1;
      if (separator === 0x5d) {
        return { kind: "array", items };
      }
      if (separator !== 0x2c) {
        throw syntaxError();
      }
    }
  }

  /** Строка в кавычках; позиция стоит на открывающей кавычке. */
  private parseString(): string {
    this.position += 1; // "
    let text = "";
    let runStart = this.position;
    for (;;) {
      const code = this.peekCode();
      if (code === -1 || code < 0x20) {
        // Конец текста внутри строки или неэкранированный управляющий символ.
        throw syntaxError();
      }
      if (code === 0x22) {
        text += this.source.slice(runStart, this.position);
        this.position += 1;
        return text;
      }
      if (code === 0x5c) {
        text += this.source.slice(runStart, this.position);
        this.position += 1;
        text += this.parseEscape();
        runStart = this.position;
        continue;
      }
      this.position += 1;
    }
  }

  /** Экранирование после обратной косой черты (позиция уже за ней). */
  private parseEscape(): string {
    const code = this.peekCode();
    this.position += 1;
    switch (code) {
      case 0x22:
        return '"';
      case 0x5c:
        return "\\";
      case 0x2f:
        return "/";
      case 0x62:
        return "\b";
      case 0x66:
        return "\f";
      case 0x6e:
        return "\n";
      case 0x72:
        return "\r";
      case 0x74:
        return "\t";
      case 0x75:
        return this.parseUnicodeEscape();
      default:
        throw syntaxError();
    }
  }

  /** `\uXXXX` и пара суррогатов `🎬`; одиночный суррогат — ошибка (как у serde_json). */
  private parseUnicodeEscape(): string {
    const first = this.readHexQuad();
    if (first >= 0xdc00 && first <= 0xdfff) {
      return this.markMalformedString(); // одиночный «младший» суррогат
    }
    if (first < 0xd800 || first > 0xdbff) {
      return String.fromCharCode(first);
    }
    // «Старший» суррогат обязан сразу сопровождаться `\u` с «младшим».
    if (this.peekCode() !== 0x5c || this.source.charCodeAt(this.position + 1) !== 0x75) {
      return this.markMalformedString();
    }
    this.position += 2;
    const second = this.readHexQuad();
    if (second < 0xdc00 || second > 0xdfff) {
      return this.markMalformedString();
    }
    return String.fromCharCode(first, second);
  }

  /**
   * Одиночный суррогат: как значение строку прочитать нельзя, но при пропуске
   * (`IgnoredAny` в serde_json) это не ошибка. Помечаем и идём дальше.
   */
  private markMalformedString(): string {
    this.malformedStrings += 1;
    this.deferredProblem = true;
    return "\ufffd";
  }

  private readHexQuad(): number {
    let value = 0;
    for (let digitIndex = 0; digitIndex < 4; digitIndex += 1) {
      const digit = hexDigitValue(this.peekCode());
      if (digit < 0) {
        throw syntaxError();
      }
      value = value * 16 + digit;
      this.position += 1;
    }
    return value;
  }

  /** Число по грамматике JSON: `-? (0 | [1-9][0-9]*) (. [0-9]+)? ([eE] [+-]? [0-9]+)?`. */
  private parseNumber(): JsonValue {
    const start = this.position;
    if (this.peekCode() === 0x2d) {
      this.position += 1;
    }
    const firstDigit = this.peekCode();
    if (!isDigit(firstDigit)) {
      throw syntaxError();
    }
    this.position += 1;
    if (firstDigit !== 0x30) {
      this.skipDigits();
    } else if (isDigit(this.peekCode())) {
      throw syntaxError(); // ведущий ноль: 01
    }
    if (this.peekCode() === 0x2e) {
      this.position += 1;
      this.requireDigits();
    }
    const exponentMark = this.peekCode();
    if (exponentMark === 0x65 || exponentMark === 0x45) {
      this.position += 1;
      const sign = this.peekCode();
      if (sign === 0x2b || sign === 0x2d) {
        this.position += 1;
      }
      this.requireDigits();
    }
    const text = this.source.slice(start, this.position);
    // Число вне f64 (1e999): как значение прочитать нельзя, при пропуске - не ошибка.
    const overflow = !Number.isFinite(Number(text));
    if (overflow) {
      this.deferredProblem = true;
    }
    return { kind: "number", text, overflow };
  }

  private skipDigits(): void {
    while (isDigit(this.peekCode())) {
      this.position += 1;
    }
  }

  private requireDigits(): void {
    if (!isDigit(this.peekCode())) {
      throw syntaxError();
    }
    this.skipDigits();
  }
}

function hexDigitValue(code: number): number {
  if (code >= 0x30 && code <= 0x39) return code - 0x30;
  if (code >= 0x61 && code <= 0x66) return code - 0x61 + 10;
  if (code >= 0x41 && code <= 0x46) return code - 0x41 + 10;
  return -1;
}

function syntaxError(): ProtocolFailure {
  return new ProtocolFailure("syntax");
}

/**
 * Строго разбирает JSON-текст в дерево.
 *
 * @throws ProtocolFailure вида `syntax`, если текст не является ровно одним JSON-значением.
 *   Одиночные суррогаты и числа вне f64 не прерывают разбор, а помечаются (см. `ParsedJson`).
 */
export function parseStrictJson(source: string): ParsedJson {
  return new StrictJsonParser(source).parseDocument();
}
