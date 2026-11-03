// Внутренний договор передачи видео (сессия 09a): кто кому что говорит внутри экстеншена.
//
// Три участника и два участка связи:
//
//   MAIN-мир страницы (src/page)  ⇄  изолированный мир (src/content)  ⇄  фон (src/background)
//          участок 1: личный MessagePort          участок 2: порт runtime (tabs.connect)
//
// - MAIN-мир видит MSE страницы и держит «тень» элемента. Он НЕ знает номеров поколений
//   протокола: MediaSource у него называется локальным ключом документа (`LocalMediaSourceKey`).
// - Изолированный мир — пограничник: проверяет форму КАЖДОГО сообщения из MAIN-мира (страница
//   может подделать что угодно в своём мире) и пересылает фону. Сам ничего не решает.
// - Фон — единственный хозяин сеанса: нумерует поколения (решение 01: номер получает новый
//   MediaSource, SourceBuffer метится поколением своего MediaSource), ведёт порядковые номера
//   рамок, WebSocket в плеер, переводит сообщения плеера в команды тени.
//
// Почему так: номера поколений должны быть едины на весь сеанс (несколько фреймов, позже воркер
// в 09b), а документ страницы может перезагрузиться и начать счёт заново (находка 03b: MediaSource
// различаются по документу, а не по счётчику страницы). Документ = одно подключение порта
// участка 2, поэтому фон ключует поколения парой «подключение + локальный ключ».
//
// Все сообщения — простые объекты с полем `kind`. Байты — `Uint8Array` (структурное клонирование:
// Firefox — всегда, Chrome — с ключом манифеста `message_serialization: structured_clone`, 148+).

import type { MediaDuration, MediaTimeMicros, MediaTimeOffsetMicros, PlaybackRatePermille } from "../protocol/time.ts";
import type { ClockState, ResumeState } from "../protocol/session/player.ts";
import type { PagePlaybackRequest, RemovalEnd } from "../protocol/session/page.ts";

/** Имя порта участка 2: его открывает фон к выбранному фрейму (`tabs.connect`). */
export const HANDOFF_PORT_NAME = "fastiplayer-handoff";

/** Локальный ключ MediaSource внутри одного документа: «ms-1», «ms-2», … (счётчик документа). */
export type LocalMediaSourceKey = string;

/** Ключ видеоэлемента внутри одного документа (выдаёт MAIN-мир при описании кандидатов). */
export type ElementKey = number;

// ---------------------------------------------------------------------------------------------
// Выбор видео (до передачи). Фон спрашивает все фреймы вкладки через scripting.executeScript,
// изолированный мир каждого фрейма спрашивает свой MAIN-мир.
// ---------------------------------------------------------------------------------------------

/**
 * Метка документа: случайная строка, которую MAIN-мир выбирает один раз на старте документа.
 * Зачем: между опросом кандидатов и `begin_handoff` фон ждёт плеер (секунды), и за это время фрейм
 * может перейти на другой документ, где ключи элементов снова начинаются с 1. Команда несёт метку
 * документа, из которого выбран элемент; чужой документ отвечает `element_not_found` (ревью 09a).
 */
export type DocumentTag = string;

/** Описание одного видеоэлемента документа для правила «какое видео отдать» (пункт 9 09a). */
export interface VideoCandidate {
  readonly document: DocumentTag;
  readonly element: ElementKey;
  /** К элементу подключён MediaSource, у которого есть хотя бы один SourceBuffer. */
  readonly has_live_media: boolean;
  /** Настоящий элемент играет (не на паузе и не кончился). */
  readonly playing: boolean;
  /** Видимая площадь элемента в CSS-пикселях (0 — не виден). */
  readonly visible_area_px: number;
}

/** Только участок 1: изолированный мир просит MAIN-мир описать видео документа. */
export interface DescribeCandidatesQuery {
  readonly kind: "describe_candidates";
  readonly request_id: number;
}

/** Только участок 1: ответ MAIN-мира на `describe_candidates`. */
export interface CandidatesReply {
  readonly kind: "candidates";
  readonly request_id: number;
  readonly candidates: readonly VideoCandidate[];
}

// ---------------------------------------------------------------------------------------------
// События MSE с локальными ключами (MAIN → фон). Фон превращает их в сообщения протокола,
// подставляя номер поколения. Индекс SourceBuffer — 0..7 по порядку addSourceBuffer в MediaSource.
// ---------------------------------------------------------------------------------------------

export type LocalMseEvent =
  | {
      readonly kind: "source_buffer_added";
      readonly media_source: LocalMediaSourceKey;
      readonly source_buffer: number;
      readonly mime_type: string;
    }
  | {
      readonly kind: "source_buffer_type_changed";
      readonly media_source: LocalMediaSourceKey;
      readonly source_buffer: number;
      readonly mime_type: string;
    }
  | {
      readonly kind: "timestamp_offset_changed";
      readonly media_source: LocalMediaSourceKey;
      readonly source_buffer: number;
      readonly offset_us: MediaTimeOffsetMicros;
    }
  | {
      readonly kind: "source_buffer_removed";
      readonly media_source: LocalMediaSourceKey;
      readonly source_buffer: number;
      readonly start_us: MediaTimeMicros;
      readonly end: RemovalEnd;
    }
  | { readonly kind: "source_buffer_aborted"; readonly media_source: LocalMediaSourceKey; readonly source_buffer: number }
  | { readonly kind: "duration_changed"; readonly media_source: LocalMediaSourceKey; readonly duration: MediaDuration }
  | { readonly kind: "end_of_stream"; readonly media_source: LocalMediaSourceKey };

// ---------------------------------------------------------------------------------------------
// MAIN → изолированный мир → фон.
// ---------------------------------------------------------------------------------------------

/** Чем кончилась команда «начни передачу этого элемента». */
export type HandoffStartOutcome =
  | "started"
  /** Элемент с таким ключом в документе больше не найден (или документ уже другой). */
  | "element_not_found"
  /** У элемента нет живого MediaSource (09a передаёт только MSE). */
  | "no_live_media";

/** Почему тень снята по инициативе страницы, а не плеера. */
export type HandoffEndCause =
  /** Снята по команде `end_handoff` (возврат, отказ поколения, обрыв связи). */
  | "returned"
  /**
   * Страница честно забрала видео (решение 179, сессия 09a2): поставила элементу не-MSE источник
   * (обычный файл, HLS-адрес), оставила его без источника дольше допуска или убрала элемент из
   * документа дольше допуска. Пустой источник сам по себе — пауза решения, а не конец (Д-1):
   * перед новым MediaSource сайты (YouTube) сначала снимают старый (`srcObject = null`).
   */
  | "element_released";

export type PageEvent =
  | { readonly kind: "handoff_began"; readonly request_id: number; readonly outcome: HandoffStartOutcome }
  | { readonly kind: "mse"; readonly event: LocalMseEvent }
  /**
   * Байты одного `appendBuffer` (или их часть) SourceBuffer-а. Порядок событий `mse` и `bytes`
   * — ровно порядок вызовов страницы. Пустой груз не шлётся. Режет на рамки ≤ 1 МиБ фон.
   * ТОЧНАЯ копия куска (byteOffset 0, длина = buffer.byteLength), отправляется с transfer её буфера:
   * клонирование вида тащит за собой ВЕСЬ буфер страницы (замер 09a: вид 1 МиБ на буфере 16 МиБ
   * приходил с 16 МиБ; 64 куска — 346/605 мс против 18/47 мс у копии с transfer, Chromium/Firefox).
   */
  | {
      readonly kind: "bytes";
      readonly media_source: LocalMediaSourceKey;
      readonly source_buffer: number;
      readonly bytes: Uint8Array;
    }
  /** Страница сама поставила `currentTime` (или `fastSeek`) переданного элемента. */
  | { readonly kind: "page_seek"; readonly target_us: MediaTimeMicros }
  /** Тень выполнила перемотку, которую попросил плеер (`seek{seek_id}`). */
  | { readonly kind: "player_seek_applied"; readonly seek_id: number; readonly target_us: MediaTimeMicros }
  /** Страница сама позвала `play()` / `pause()` / сменила `playbackRate`. */
  | { readonly kind: "page_playback"; readonly request: PagePlaybackRequest }
  /**
   * Пульс «тень жива» (решение владельца 180, Д-2). Шлёт сама тень — владелец знания «элемент
   * под тенью» — не чаще раза в `SHADOW_PULSE_INTERVAL_MS`, на тиках часов плеера (не таймером:
   * таймеры скрытой вкладки Chromium будит раз в минуту). Фон, не слышавший пульса дольше
   * `SHADOW_SILENCE_LIMIT_MS`, честно кончает сеанс: потерянный `handoff_ended`, зависшая страница
   * и сломанный канал больше не оставляют плеер «живым на паузе».
   */
  | { readonly kind: "shadow_alive" }
  /** Тень снята (после этого MAIN-мир больше ничего не шлёт по этой передаче). */
  | { readonly kind: "handoff_ended"; readonly cause: HandoffEndCause };

/**
 * Как часто тень подаёт пульс. Часы плеера приходят раз в ~250 мс и идут и на паузе (09a),
 * поэтому пульс раз в секунду не зависит от таймеров страницы.
 */
export const SHADOW_PULSE_INTERVAL_MS = 1_000;

/**
 * Сколько фон терпит тишину пульса до честного конца сеанса. С запасом в 10 пульсов: длинная
 * сборка мусора или тяжёлый скрипт страницы (сотни миллисекунд) сеанс не рвут, а настоящая
 * потеря видна зрителю за секунды, а не никогда.
 */
export const SHADOW_SILENCE_LIMIT_MS = 10_000;

// ---------------------------------------------------------------------------------------------
// Фон → изолированный мир → MAIN.
// ---------------------------------------------------------------------------------------------

/** Команды тени, переведённые из сообщений плеера. */
export type ShadowCommand =
  | {
      readonly kind: "clock";
      readonly position_us: MediaTimeMicros;
      readonly state: ClockState;
      readonly rate: PlaybackRatePermille;
    }
  | { readonly kind: "play" }
  | { readonly kind: "pause" }
  | { readonly kind: "set_rate"; readonly rate: PlaybackRatePermille }
  | { readonly kind: "seek"; readonly seek_id: number; readonly target_us: MediaTimeMicros };

/** Куда поставить настоящий элемент при снятии тени. */
export type HandoffEndPosition =
  | { readonly kind: "at"; readonly position_us: MediaTimeMicros }
  /** Плеер позицию не сообщил (обрыв связи): остаться на позиции тени. */
  | { readonly kind: "shadow_position" };

/** Играть ли странице после снятия тени. */
export type HandoffEndResume =
  | { readonly kind: "exactly"; readonly resume: ResumeState }
  /** Как было у тени в момент снятия (обрыв связи, отказ поколения). */
  | { readonly kind: "as_shadow" };

export type PageCommand =
  | {
      readonly kind: "begin_handoff";
      readonly request_id: number;
      readonly document: DocumentTag;
      readonly element: ElementKey;
    }
  | { readonly kind: "shadow"; readonly command: ShadowCommand }
  | { readonly kind: "end_handoff"; readonly position: HandoffEndPosition; readonly resume: HandoffEndResume };

// ---------------------------------------------------------------------------------------------
// Внутренняя команда «отдать вкладку N» (решение 175). Шлют только страницы самого экстеншена
// (сейчас — робот стенда со служебной страницы, в 10 — кнопка попапа). Фон проверяет, что
// отправитель — страница экстеншена, а не контент-скрипт.
// ---------------------------------------------------------------------------------------------

export const HAND_OFF_TAB_KIND = "hand_off_tab";

export interface HandOffTabRequest {
  readonly kind: typeof HAND_OFF_TAB_KIND;
  readonly tab_id: number;
}

export type HandOffTabOutcome =
  | { readonly kind: "started" }
  /** Во вкладке нет видео с живым MediaSource. */
  | { readonly kind: "no_video" }
  /** Связь с плеером не поднялась или плеер отказал (`reason` — для журнала и попапа 10). */
  | { readonly kind: "player_unavailable"; readonly reason: string }
  /** Внутренняя ошибка экстеншена (`reason` — для журнала). */
  | { readonly kind: "failed"; readonly reason: string };
