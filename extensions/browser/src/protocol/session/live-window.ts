// Окно эфира live HLS (протокол 1.2, сессия 10a3, решение 232). Зеркало session/live_window.rs.
//
// Плеер сам ведёт live HLS и знает честное окно перемотки; странице он сообщает его
// сообщением `live_window{generation, edge_us, seekable}`. Тень показывает по нему
// `seekable` и прижимает перемотку страницы к краям окна.
//
// Все моменты - во времени плеера (то же, что `clock.position_us`), целые микросекунды.
// Договор: `start_us <= end_us <= edge_us`; нарушение - ошибка разбора, как в Rust.
// Частоту окна задаёт плеер (`MIN_LIVE_WINDOW_INTERVAL_MS`); экстеншен её не проверяет.

import { ProtocolFailure } from "../errors.ts";
import { type Codec, taggedUnion } from "../schema.ts";
import { type MediaTimeMicros, mediaTimeCodec } from "../time.ts";

/**
 * Куда в эфире можно перемотать. Не необязательное поле, а метка: пропуск поля -
 * `missing_field`, `null` - `wrong_value_type`, как в Rust.
 * - `none` - эфир без DVR, перемотки нет;
 * - `range` - окно `[start_us, end_us]`; окно в одну точку (`start_us === end_us`) допустимо.
 */
export type LiveSeekable =
  | { readonly kind: "none" }
  | { readonly kind: "range"; readonly start_us: MediaTimeMicros; readonly end_us: MediaTimeMicros };

export const liveSeekableCodec: Codec<LiveSeekable> = taggedUnion<LiveSeekable, "kind">("kind", {
  none: {},
  range: { start_us: mediaTimeCodec, end_us: mediaTimeCodec },
});

/**
 * Проверяет окно относительно края эфира. Виды ошибок - как в Rust
 * (`LiveSeekable::check_within_edge`):
 * - начало позже конца (окно вывернуто) - `empty_range` у `seekable.end_us`;
 * - конец дальше края - `number_out_of_range` у `seekable.end_us`.
 */
export function checkLiveWindowWithinEdge(edgeUs: MediaTimeMicros, seekable: LiveSeekable): void {
  // Без окна сверять нечего: край сам по себе любой момент шкалы.
  if (seekable.kind === "none") return;
  if (seekable.start_us > seekable.end_us) {
    throw new ProtocolFailure("empty_range").prependPath("end_us").prependPath("seekable");
  }
  if (seekable.end_us > edgeUs) {
    throw new ProtocolFailure("number_out_of_range").prependPath("end_us").prependPath("seekable");
  }
}
