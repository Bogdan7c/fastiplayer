// Типизированные причины отказа. Зеркало error.rs (MessageErrorKind) и data_frame.rs (DataFrameError).
//
// Имена видов — общий словарь с Rust и с общими фикстурами (fixtures/manifest.json).
// В ошибке нет содержимого сообщения: всё, что пришло со страницы или из сети,
// считается данными врага (инвариант 1) и в журналы не попадает. `detail` хранит
// только путь к полю по СХЕМЕ протокола (например `hints.duration.duration_us`).

/** Все виды отказа JSON-сообщения; порядок и имена как в MessageErrorKind. */
export const MESSAGE_ERROR_KINDS = [
  "too_large",
  "not_utf8",
  "syntax",
  "not_an_object",
  "missing_type",
  "unknown_type",
  "unknown_field",
  "missing_field",
  "duplicate_field",
  "unknown_value",
  "wrong_value_type",
  "text_empty",
  "text_too_long",
  "text_too_short",
  "text_forbidden_character",
  "number_out_of_range",
  "too_many_items",
  "duplicate_id",
  "conflicting_selection",
  "empty_range",
] as const;

/** Вид отказа JSON-сообщения. */
export type MessageErrorKind = (typeof MESSAGE_ERROR_KINDS)[number];

/** Все виды отказа двоичной рамки; имена как в DataFrameError. */
export const DATA_FRAME_ERROR_KINDS = [
  "frame_too_short",
  "unknown_frame_kind",
  "reserved_bits_set",
  "payload_empty",
  "payload_too_large",
  "number_out_of_range",
] as const;

/** Вид отказа двоичной рамки. */
export type DataFrameErrorKind = (typeof DATA_FRAME_ERROR_KINDS)[number];

/** Причина отказа: вид (общий с Rust) и путь к полю по схеме. */
export interface ProtocolError {
  readonly kind: MessageErrorKind | DataFrameErrorKind;
  readonly detail: string;
}

/** Итог разбора или записи: либо значение, либо точная причина отказа. */
export type DecodeResult<T> =
  | { readonly ok: true; readonly value: T }
  | { readonly ok: false; readonly error: ProtocolError };

/** Итог записи: те же две ветки, что и у разбора. */
export type EncodeResult<T> = DecodeResult<T>;

/**
 * Внутреннее исключение разбора: так вложенные проверки значений прерывают разбор
 * на первой же ошибке (как serde в Rust). Наружу не выходит: публичные функции
 * ловят его и возвращают `DecodeResult`.
 */
export class ProtocolFailure extends Error {
  readonly kind: MessageErrorKind | DataFrameErrorKind;
  /** Путь к полю от корня, собирается по мере всплытия исключения. */
  readonly path: string[] = [];

  constructor(kind: MessageErrorKind | DataFrameErrorKind) {
    super(kind);
    this.kind = kind;
  }

  /** Добавляет в начало пути имя поля (или индекс элемента списка). */
  prependPath(segment: string): this {
    this.path.unshift(segment);
    return this;
  }
}

/** Превращает пойманное исключение в ошибку протокола; чужие исключения пробрасывает дальше. */
export function failureToError(failure: unknown): ProtocolError {
  if (failure instanceof ProtocolFailure) {
    return { kind: failure.kind, detail: failure.path.length > 0 ? failure.path.join(".") : "message root" };
  }
  throw failure;
}

/** Успешный итог. */
export function succeed<T>(value: T): DecodeResult<T> {
  return { ok: true, value };
}

/** Итог-отказ без пути. */
export function reject<T>(kind: MessageErrorKind | DataFrameErrorKind, detail: string): DecodeResult<T> {
  return { ok: false, error: { kind, detail } };
}
