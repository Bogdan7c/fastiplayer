// Разбор списка атрибутов тега HLS (RFC 8216, 4.2): `NAME=value,NAME2="quoted, with comma"`.
// Строгий: на неверный вход - отказ, а не догадка (в тегах с адресами догадка могла бы пропустить
// настоящий адрес плееру).

/** Один атрибут: имя, значение без кавычек и признак «было в кавычках». */
export interface Attribute {
  readonly name: string;
  readonly value: string;
  readonly quoted: boolean;
}

export type AttributeListParse = { readonly ok: true; readonly attributes: readonly Attribute[] } | { readonly ok: false };

/** Символ имени атрибута: буквы, цифры и дефис (по RFC - заглавные; строчные принимаем и не путаем с URI). */
const ATTRIBUTE_NAME_CHARACTER = /[A-Za-z0-9-]/;

const QUOTE = '"';
const COMMA = ",";
const EQUALS = "=";

/** Разобрать список. Пустая строка - пустой список. */
export function parseAttributeList(text: string): AttributeListParse {
  const attributes: Attribute[] = [];
  let position = 0;
  while (position < text.length) {
    const nameStart = position;
    while (position < text.length && ATTRIBUTE_NAME_CHARACTER.test(text.charAt(position))) position += 1;
    if (position === nameStart || text.charAt(position) !== EQUALS) return { ok: false };
    const name = text.slice(nameStart, position);
    position += 1;
    if (text.charAt(position) === QUOTE) {
      // Строка в кавычках: запятая внутри - часть значения; закрывающая кавычка обязательна.
      const closing = text.indexOf(QUOTE, position + 1);
      if (closing === -1) return { ok: false };
      attributes.push({ name, value: text.slice(position + 1, closing), quoted: true });
      position = closing + 1;
    } else {
      // Значение без кавычек (число, перечисление): до запятой, без пробелов и кавычек, не пустое.
      const valueStart = position;
      while (position < text.length && text.charAt(position) !== COMMA) position += 1;
      const value = text.slice(valueStart, position);
      if (value.length === 0 || /[\s"]/.test(value)) return { ok: false };
      attributes.push({ name, value, quoted: false });
    }
    if (position === text.length) break;
    // После атрибута - только запятая и обязательно ещё один атрибут (висячая запятая - порча).
    if (text.charAt(position) !== COMMA) return { ok: false };
    position += 1;
    if (position === text.length) return { ok: false };
  }
  return { ok: true, attributes };
}

/** Собрать список обратно: порядок и вид значений (в кавычках или нет) сохраняются. */
export function formatAttributeList(attributes: readonly Attribute[]): string {
  return attributes.map((attribute) => `${attribute.name}=${attribute.quoted ? `"${attribute.value}"` : attribute.value}`).join(COMMA);
}
