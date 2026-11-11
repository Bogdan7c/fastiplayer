// Номер поколения в тике часов (протокол 1.10, сессия 16a2, решение 311). Зеркало session/clock_generation.rs.
//
// Тик `clock` до 1.10 говорил «где плеер и что делает», но не «про какой ролик». Пока плеер переходит с одного
// поколения потока на другое (реклама -> фильм, серия -> серия, новый источник на том же или другом элементе), он
// ещё какое-то время шлёт часы ПРОШЛОГО поколения, и тень принимала их за часы нового видео: `ended` баннера,
// кончившегося на 12,0 с, подтверждал старт фильма с 11,1 с и навсегда защёлкивал у фильма `ended`.
// С 1.10 тик называет поколение, которое описывает:
//
//   {"type":"clock","seq":42,"position_us":11100000,"state":"playing","rate":1000,"generation":2}
//
// - `generation` - необязательное поле, всегда ПОСЛЕДНЕЕ (тик 1.9 - точный префикс тика 1.10); значение - номер
//   поколения 1..2^32-1, тот же, что в `generation_started` / `file_source_added` / `hls_source_added`.
// - Плеер шлёт поле только при общей версии не ниже CLOCK_GENERATION_INTRODUCED_IN; со стороной 1.9 тик прежний.
// - Нет ключа - «номер неизвестен» (сторона старше 1.10 или у плеера ещё нет поколения). `null` -
//   `wrong_value_type`: одна форма «номера нет» на проводе (`optionalField` в schema.ts).
// Тик с полем требует 1.10 (`playerSessionRequiredVersion`); тип `clock` по-прежнему из 1.0. Общая таблица версий
// полей - fixtures/field-versions.json.

import { PROTOCOL_VERSION_1_10, type ProtocolVersion } from "../version.ts";

/**
 * Версия протокола, в которой у `clock` появилось необязательное поле `generation`. Номеру в тике можно верить
 * (и ждать его) только при `versionIncludes(agreed, CLOCK_GENERATION_INTRODUCED_IN)`. Зеркало
 * `CLOCK_GENERATION_INTRODUCED_IN` (clock_generation.rs).
 */
export const CLOCK_GENERATION_INTRODUCED_IN: ProtocolVersion = PROTOCOL_VERSION_1_10;
