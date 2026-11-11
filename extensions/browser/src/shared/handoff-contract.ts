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
import type { LiveSeekable } from "../protocol/session/live-window.ts";
import type { PagePlaybackRequest, RemovalEnd } from "../protocol/session/page.ts";
import type { ReferrerPolicyName } from "./referrer-policy.ts";
import type { PageSubtitleEvent } from "./page-subtitle-contract.ts";

/** Имя порта участка 2: его открывает фон к выбранному фрейму (`tabs.connect`). */
export const HANDOFF_PORT_NAME = "fastiplayer-handoff";

/**
 * Локальный ключ MediaSource внутри одного документа:
 * - главный поток — «ms-1», «ms-2», … (счётчик документа);
 * - выделенный воркер (сессия 09b) — «w<номер воркера>/ms-N»: у каждого воркера свой счётчик
 *   «ms-N», а номер воркера документа (`w1`, `w2`, …) ставит MAIN-мир. Так ключи воркера никогда
 *   не совпадают с ключами главного потока и других воркеров того же документа, а фон по-прежнему
 *   нумерует поколения по паре «подключение + локальный ключ».
 */
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

/**
 * Форма видео, которую экстеншен пока честно не передаёт (сессия 09b). Видео остаётся в браузере,
 * попап (сессия 10) говорит «not supported yet» с этой формой.
 * - `native_hls` — m3u8 прямо в `src`/`<source>`: браузер играет HLS сам (Chromium 156 умеет).
 *   С 10a2 запись (VOD) передаётся формой `hls`; здесь остаётся ЖИВОЙ эфир (длительность Infinity,
 *   решение 231) и старый плеер протокола 1.0 (решение 185);
 * - `blob_file` — blob-адрес обычного `Blob`/`File` (не MediaSource): байты живут только в
 *   памяти страницы, фону их не скачать;
 * - `data_url` — видео целиком в адресе `data:`;
 * - `media_stream` — `srcObject` = MediaStream (камера, захват экрана, WebRTC): живой поток, не файл;
 * - `other_scheme` — адрес не http(s) (например `file:`, `filesystem:`), фон его не качает;
 * - `untracked_worker_media_source` — `srcObject` = MediaSourceHandle, но его MediaSource живёт в
 *   воркере, куда наш хук не попал: обёртку не пустил CSP (`worker-src` без `blob:`) или Trusted
 *   Types, handle пришёл из вложенного воркера или через чужой MessagePort (сессия 09b);
 * - `started_before_script` — blob-адрес, созданный раньше нашего хука: Firefox вставляет скрипт в уже
 *   открытую вкладку поздно (сессия 10d), начало потока упущено. Лечится только перезагрузкой вкладки.
 */
export type UnsupportedMediaForm =
  | "native_hls"
  | "blob_file"
  | "data_url"
  | "media_stream"
  | "other_scheme"
  | "untracked_worker_media_source"
  | "started_before_script";

/**
 * Почему видео вкладки не передать без перезагрузки (сессия 10d, решение 210). Вкладка открыта раньше,
 * чем встал наш скрипт, а разведка показала, что Chrome и Firefox ведут себя по-разному:
 * - `script_missing` — на странице нет живого скрипта. Chrome: и после установки, и после обновления,
 *   потому что новый фон не видит изолированный мир старого скрипта;
 * - `script_outdated` — скрипт ответил, но его версия не совпадает с версией фона (страховка: живые
 *   браузеры такого не показали, но разговор разных версий запрещён тем же правилом, что в протоколе);
 * - `stream_started_before_script` — Firefox сам вставил скрипт, но поток начался раньше него.
 */
export type ReloadReason = "script_missing" | "script_outdated" | "stream_started_before_script";

/**
 * Что сейчас играет элемент — с точки зрения «можно ли его передать» (сессия 09b).
 * - `media_source` — живой MediaSource (хотя бы один SourceBuffer, не `closed`);
 * - `file` — обычный файл по абсолютному http(s)-адресу: байты качает фон по `fetch_range`;
 * - `unsupported` — видео есть, но в форме, которую пока не передаём;
 * - `none` — источника нет (или MediaSource ещё без SourceBuffer).
 */
export type CandidateMedia =
  /**
   * `in_worker` — MediaSource живёт в выделенном воркере (форма `worker_media_source`), а не в
   * главном потоке. Для выбора видео это то же MSE; признак нужен модели вкладки и попапу.
   */
  | { readonly kind: "media_source"; readonly in_worker: boolean }
  | { readonly kind: "file" }
  /** Плейлист нативного HLS (сессия 10a2). Запись или эфир — по `duration_s` кандидата. */
  | { readonly kind: "hls" }
  | { readonly kind: "unsupported"; readonly form: UnsupportedMediaForm }
  | { readonly kind: "none" };

/** Описание одного видеоэлемента документа для правила «какое видео отдать» (пункт 9 09a). */
export interface VideoCandidate {
  readonly document: DocumentTag;
  readonly element: ElementKey;
  /** Чем элемент играет: MSE, файл, неподдержанная форма или ничего. */
  readonly media: CandidateMedia;
  /** Настоящий элемент играет (не на паузе и не кончился). */
  readonly playing: boolean;
  /**
   * Видимая площадь элемента в CSS-пикселях. 0 — не виден: за окном, нулевого размера или скрыт
   * (`display:none`, `visibility:hidden`, `opacity:0` у самого элемента или любого предка,
   * решение 68).
   */
  readonly visible_area_px: number;
  /** Длительность настоящего элемента, секунды; `null` — неизвестна, бесконечна (эфир) или NaN. */
  readonly duration_s: number | null;
  /** Настоящий элемент без звука: `muted` ИЛИ громкость 0. */
  readonly muted: boolean;
  /** У настоящего элемента включён `loop`. */
  readonly loop: boolean;
  /** `videoWidth` настоящего элемента (0 — размер кадра ещё неизвестен). */
  readonly video_width_px: number;
  /** `videoHeight` настоящего элемента (0 — размер кадра ещё неизвестен). */
  readonly video_height_px: number;
  /**
   * Элемент под DRM (инвариант 4, решение 65): у него есть ключи (`mediaKeys !== null`), на нём
   * было событие `encrypted` или страница успешно вызвала `setMediaKeys` с ключами. Один опрос
   * `requestMediaKeySystemAccess` признаком не считается.
   */
  readonly drm: boolean;
  /** Имя хоста документа, в котором лежит элемент (`location.hostname`; "" — нет). */
  readonly frame_host: string;
  /** Имя хоста адреса медиа для `media.kind === "file"` и `"hls"`, иначе `null`. */
  readonly file_host: string | null;
  /** MIME всех отслеживаемых SourceBuffer-ов элемента (не больше `MAX_SOURCE_MIME_TYPES`); пусто — неизвестно. */
  readonly source_mime_types: readonly string[];
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
  /**
   * У элемента к моменту команды нет того, что мы передаём: ни живого MediaSource, ни файла по
   * http(s) (источник успели снять или сменить на неподдержанную форму между опросом и командой).
   */
  | "no_media"
  /**
   * Элемент под DRM (09c, инвариант 4): передачу не начинаем, тень не встаёт, видео остаётся
   * играть в браузере. Фон показывает исход `protected`.
   */
  | "protected";

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
  | "element_released"
  /**
   * Счётчик копий (page/forward-byte-meter.ts, решение 200) отказал: страница льёт байты сверх
   * предела. Тень снята нами, но это вина страницы, и причина `page_flood` доходит до модели вкладки.
   */
  | "page_flood"
  /**
   * Сломался собственный плеер страницы (решение 237, сессия 11a): у переданного MSE-элемента ошибка
   * (`error`), которая держится дольше допуска, или страница вскоре после ошибки бросила источник.
   * Фон кончает сеанс как «вкладка ушла» (`tab_gone{page_player_failed}`): плеер доигрывает скачанное.
   * Владелец правила — `page/page-player-failure-watch.ts`.
   */
  | "page_player_failed"
  /**
   * Серия доиграла у нас до конца (часы плеера сказали `ended`), страница сняла источник и за
   * `NEXT_EPISODE_WAIT_MS` не поставила новый (сессия 16a, решение 307). Это не «страница забрала видео»:
   * видео доиграно. Фон закрывает связь `closeReasonForUnstartedNextEpisode` (4011 с плеером 1.9+, иначе
   * прежний 4009), а если адрес вкладки за это время сменился — как уход одностраничника (решение 42).
   */
  | "next_episode_not_started";

/**
 * Вид нативного HLS по элементу в момент передачи (10a3):
 * - `vod` — запись: `duration` конечна (`#EXT-X-ENDLIST`);
 * - `live` — эфир: `duration === Infinity` (плейлист без конца, MDN `HTMLMediaElement.duration`).
 */
export type HlsStreamKind = "vod" | "live";

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
   * Передаётся файл `<video src>` (сессия 09b): идёт сразу после `handoff_began{started}`, до
   * `page_seek`. Адрес — абсолютный http(s) выбранного источника настоящего элемента (`currentSrc`).
   * Он остаётся ВНУТРИ экстеншена: фон качает по нему диапазоны для `fetch_range`, а плееру уходит
   * только `file_source_added{generation}` — протокол адресов не несёт.
   *
   * `referrer_origin` — сайт-источник документа для заголовка Referer закачки (решение 202): только
   * происхождение, без пути страницы; `null` — Referer не нужен (политика `no-referrer` или непрозрачное
   * происхождение документа). `referrer_policy` — политика, которую видно из MAIN-мира (атрибут элемента,
   * иначе `<meta name=referrer>`): фон считает по ней Referer на каждый адрес, в том числе конечный после
   * перенаправления. Заголовок ответа `Referrer-Policy` страница не видит (известное ограничение).
   */
  | { readonly kind: "file_source"; readonly url: string; readonly referrer_origin: string | null; readonly referrer_policy: ReferrerPolicyName }
  /**
   * Передаётся плейлист нативного HLS (сессия 10a2): по месту и форме тот же, что `file_source`
   * (адрес `.m3u8` выбранного источника, происхождение и политика Referer по решению 202), но фон открывает
   * не поколение-файл, а поколение HLS: плейлист и сегменты качает он сам.
   *
   * `stream` (10a3, решение 232) — запись или эфир, как его видит элемент в момент передачи
   * (`duration` конечна или `Infinity`). Фон по нему решает, может ли плеер сеанса такое играть:
   * эфир — только плеер, договорившийся на протокол 1.2 (`live_window`).
   */
  | {
      readonly kind: "hls_source";
      readonly url: string;
      readonly referrer_origin: string | null;
      readonly referrer_policy: ReferrerPolicyName;
      readonly stream: HlsStreamKind;
    }
  /**
   * Пульс «тень жива» (решение владельца 180, Д-2). Шлёт сама тень — владелец знания «элемент
   * под тенью» — не чаще раза в `SHADOW_PULSE_INTERVAL_MS`, на тиках часов плеера (не таймером:
   * таймеры скрытой вкладки Chromium будит раз в минуту). Фон, не слышавший пульса дольше
   * `SHADOW_SILENCE_LIMIT_MS`, честно кончает сеанс: потерянный `handoff_ended`, зависшая страница
   * и сломанный канал больше не оставляют плеер «живым на паузе».
   */
  | { readonly kind: "shadow_alive" }
  /**
   * Серия доиграла у нас (часы плеера сказали `ended`), а страница сняла источник: тень начала ждать
   * следующую серию `NEXT_EPISODE_WAIT_MS` (сессия 16a, решения 303, 306). Шлёт тень один раз на ожидание;
   * фон передаёт плееру `awaiting_next_episode`, только если его собственные часы тоже видели конец серии
   * (страница — чужой код) и не чаще раза на видео или поколение.
   */
  | { readonly kind: "next_episode_wait_began" }
  /** Тень снята (после этого MAIN-мир больше ничего не шлёт по этой передаче). */
  | { readonly kind: "handoff_ended"; readonly cause: HandoffEndCause }
  /**
   * Сторож следующего видео (сессия 09c2) поставил тень на видео, которое страница запустила, пока
   * сторож взведён. Фон ещё ничего не решил: `candidates` — все видео документа, включая задержанное
   * `element`. Событие проходит только пока сторож взведён, поэтому страница не может будить фон сама.
   * `trigger` — почему сторож задержал видео (см. `HeldVideoTrigger`).
   */
  | { readonly kind: "next_video_held"; readonly element: ElementKey; readonly candidates: readonly VideoCandidate[]; readonly trigger: HeldVideoTrigger }
  /**
   * Субтитры страницы (сессия 13b2): готовый документ или `<track src>`, который качает фон.
   * Договор и смысл полей — `page-subtitle-contract.ts`. Идут только после `handoff_began{started}`.
   */
  | PageSubtitleEvent;

/**
 * Почему сторож задержал видео:
 * - `next_video` — сторож взведён фоном на срок: переезд сеанса (09c2) или возврат страницы (11b2);
 * - `viewer_play` — режим 2 (сессия 12a): охрана включена настройкой, а человек сам запустил видео со звуком
 *   (свежий жест или «наследованный клик», решения 246–247). Решать, передавать ли, будет фон по правилам сайта.
 */
export const HELD_VIDEO_TRIGGERS = ["next_video", "viewer_play"] as const;
export type HeldVideoTrigger = (typeof HELD_VIDEO_TRIGGERS)[number];

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
// Изолированный мир → фон: своё донесение пограничника, из MAIN-мира его подделать нельзя.
// ---------------------------------------------------------------------------------------------

/**
 * Вид события, который считает бюджет страницы (content/page-event-budget.ts, решение 200).
 * `malformed` — сообщения не договорной формы: страница, сыплющая мусором, тратит тот же бюджет.
 * Байты бюджет не считает: их держит потолок очереди сеанса.
 */
export type PageEventBudgetKind =
  | "mse"
  | "page_seek"
  | "player_seek_applied"
  | "page_playback"
  | "file_source"
  | "hls_source"
  | "shadow_alive"
  /** «Ждём следующую серию» (16a): раз на ожидание, свой вид — чтобы флуд им не тратил пульс. */
  | "next_episode_wait"
  /** Оба события субтитров страницы (13b2): документы и `<track src>` считаются вместе. */
  | "subtitle"
  | "malformed";

/** Вид донесения «страница превысила бюджет»: он НЕ входит в `PageEvent`. */
export const PAGE_FLOOD_NOTICE_KIND = "page_flood";

/**
 * Страница превысила бюджет событий (решение 200). Шлёт ТОЛЬКО изолированный мир по порту участка 2.
 * MAIN-мир подделать его не может: всё, что он шлёт, проходит `parsePageEvent`, а там этого вида нет.
 * `over_budget` — только для отладочного журнала.
 */
export interface PageFloodNotice {
  readonly kind: typeof PAGE_FLOOD_NOTICE_KIND;
  readonly over_budget: PageEventBudgetKind;
}

/** Вид просьбы зрителя «Return» с заглушки (сессия 11a, решение 236): она НЕ входит в `PageEvent`. */
export const VIEWER_RETURN_REQUEST_KIND = "viewer_return_requested";

/**
 * Зритель нажал «Return» на заглушке поверх переданного видео (решение 236): вернуть видео странице на
 * позиции тени, страница играет дальше, окно плеера закрывается молча (4010). Шлёт ТОЛЬКО изолированный
 * мир по порту участка 2 той передачи, на которой стоит заглушка, и только на настоящий клик человека
 * (`isTrusted`). MAIN-мир подделать её не может: всё, что он шлёт, проходит `parsePageEvent`, а там
 * такого вида нет. Груза нет: порт сам называет передачу.
 */
export interface ViewerReturnRequest {
  readonly kind: typeof VIEWER_RETURN_REQUEST_KIND;
}

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
  | { readonly kind: "seek"; readonly seek_id: number; readonly target_us: MediaTimeMicros }
  /**
   * Окно эфира live HLS (протокол 1.2, 10a3, решение 232): край и окно перемотки ВО ВРЕМЕНИ ПЛЕЕРА
   * (как `clock.position_us`). Поколение сверил фон: сюда доходит окно только последнего поколения
   * сеанса. Перевод во время страницы — дело тени (`shadow/live/`). `start_us <= end_us <= edge_us`.
   */
  | { readonly kind: "live_window"; readonly edge_us: MediaTimeMicros; readonly seekable: LiveSeekable };

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

/**
 * Команды сторожа следующего видео. Их рождает изолированный мир: фон зовёт его широковещательным
 * `tabs.sendMessage`, а не портом сеанса, поэтому в `PageCommand` их нет.
 * - `arm_next_video` — взвести сторож на `lease_ms`. Срок вместо команды «снять»: фон может умереть в
 *   любой момент, а страница не должна остаться со взведённым сторожем навсегда;
 * - `release_held_video` — отпустить задержанное видео: фон сказал «не берём» или не ответил вовремя.
 */
export type NextVideoCommand =
  | { readonly kind: "arm_next_video"; readonly lease_ms: number; readonly page_return?: PageReturnTarget }
  | { readonly kind: "release_held_video"; readonly element: ElementKey };

/**
 * Взвод возврата страницы (сессия 11b2, решения 240–241): вкладку открыли заново, и сторож ждёт не «любое
 * следующее видео», а то самое, что играет в окне плеера.
 * - `recorded` — запись: длина ролика (сверка ± `PAGE_RETURN_DURATION_TOLERANCE_S`, как у позднего входа) и
 *   секунда, на которую страница должна встать до тени (последняя секунда часов плеера);
 * - `live` — эфир: длины нет, сверять нечем, страница сама входит на живой край.
 */
export type PageReturnTarget =
  | { readonly kind: "recorded"; readonly duration_s: number; readonly start_s: number }
  | { readonly kind: "live" };

/** Допуск сверки длины вернувшегося ролика, секунды (тот же, что у позднего входа, сессия 10d). */
export const PAGE_RETURN_DURATION_TOLERANCE_S = 2;

// ---------------------------------------------------------------------------------------------
// Переезд сеанса на следующее видео (сессия 09c2, решения 201, 204): фон ⇄ изолированный мир.
// ---------------------------------------------------------------------------------------------

/** Вид сообщения фона всем фреймам вкладки: «взведи сторож». */
export const ARM_NEXT_VIDEO_KIND = "arm_next_video";

/** Широковещательная просьба фона; приходит в изолированный мир каждого фрейма вкладки. */
export interface ArmNextVideoRequest {
  readonly kind: typeof ARM_NEXT_VIDEO_KIND;
  readonly lease_ms: number;
  /** Есть — взвод возврата страницы (11b2); нет — обычный переезд на следующее видео (09c2). */
  readonly page_return?: PageReturnTarget;
}

/**
 * Ответ изолированного мира на взвод: «сторож этого документа есть». Фону возврата он говорит, что страница
 * уже загрузила наш скрипт (иначе `tabs.sendMessage` отвергается — получателя нет).
 */
export const ARM_NEXT_VIDEO_ACCEPTED = "armed";

/** Вид донесения изолированного мира фону о задержанном видео. */
export const HELD_VIDEO_REPORT_KIND = "held_video_report";

/** Донесение фону: `runtime.sendMessage`, вкладку и фрейм фон берёт из отправителя, а не из сообщения. */
export interface HeldVideoReport {
  readonly kind: typeof HELD_VIDEO_REPORT_KIND;
  readonly element: ElementKey;
  readonly candidates: readonly VideoCandidate[];
  readonly trigger: HeldVideoTrigger;
}

/**
 * Ответ фона:
 * - `adopt` — сеанс переезжает, следом за миллисекунды придёт `begin_handoff`;
 * - `adopt_new_session` — режим 2: фон открывает НОВЫЙ сеанс, `begin_handoff` придёт после запуска плеера
 *   (до десятков секунд, см. `NEW_SESSION_ADOPT_INSURANCE_MS`). Не вышло — фон отпустит видео сам
 *   (`release_held_video_request`), а страховка изолированного мира — если фон умер;
 * - `release` — отпустить;
 * - `ask_viewer` — режим 2 на сайте `ask` (сессия 12b): держать видео на паузе и спросить человека на
 *   предложении поверх видео (viewer-offer-contract.ts). Срока у такого удержания нет: выбор за человеком.
 */
export const HELD_VIDEO_REPLIES = ["adopt", "adopt_new_session", "release", "ask_viewer"] as const;
export type HeldVideoReply = (typeof HELD_VIDEO_REPLIES)[number];

/**
 * Страховка изолированного мира после `adopt_new_session`: столько видео ждёт `begin_handoff`, потом
 * отпускается само. Худший случай старта сеанса — сумма сроков фона: `hello` 8 с + `open_session` 12 с +
 * `attached` 10 с + опрос фреймов 5 с ≈ 35 с; 45 с — с запасом. Обычный исход наступает раньше: фон
 * отпускает видео сам, как только сеанс не состоялся. Сюда страховка доходит, только если фон умер.
 */
export const NEW_SESSION_ADOPT_INSURANCE_MS = 45_000;

// ---------------------------------------------------------------------------------------------
// Внутренняя команда «отдать вкладку N» (решение 175). Шлют только страницы самого экстеншена
// (сейчас — робот стенда со служебной страницы, в 10 — кнопка попапа). Фон проверяет, что
// отправитель — страница экстеншена, а не контент-скрипт.
// ---------------------------------------------------------------------------------------------

export const HAND_OFF_TAB_KIND = "hand_off_tab";

/**
 * Ссылка на видео вкладки: фрейм, метка документа и ключ элемента (как в `begin_handoff`). Её выдаёт
 * опрос вкладки (`survey_tab`), человек выбирает по ней строку в попапе, а `hand_off_tab` сверяет её
 * со СВЕЖИМ опросом: ссылка устарела — исход `video_gone`, подмены другим видео нет.
 */
export interface VideoRef {
  readonly frame_id: number;
  readonly document: DocumentTag;
  readonly element: ElementKey;
}

export interface HandOffTabRequest {
  readonly kind: typeof HAND_OFF_TAB_KIND;
  readonly tab_id: number;
  /**
   * Видео, которое человек выбрал в списке попапа (сессия 10b). Без поля фон сам берёт главное видео
   * вкладки. С полем и живым сеансом вкладки — замена видео в том же окне (решение 215).
   */
  readonly video?: VideoRef;
  /**
   * Хост, на который человек согласился (решение 199): значение `host` из исхода
   * `local_address_needs_consent`. Ставит только страница самого экстеншена (попап 10) после вопроса
   * «спрашивать»; без поля действует настройка. Согласие на хост, а не вообще: пока человек думал,
   * страница могла сменить файл. Другой хост получит новый вопрос, а не молчаливое разрешение.
   */
  readonly consented_local_host?: string;
}

/**
 * Окно плеера при удаче (решение 216): `new` — плеер открыл новое окно и забрал фокус; `already_open` — окно
 * уже было (то же видео или замена), оно остаётся за браузером, и зрителю надо сказать «переключись на него».
 * Источник — ответ плеера на `open_session` (`started` -> new; `reused_existing`, `replaced_previous` -> already_open).
 */
export type PlayerWindow = "new" | "already_open";

export type HandOffTabOutcome =
  | { readonly kind: "started"; readonly player_window: PlayerWindow }
  /** Во вкладке нет видео, которое можно передать (ни живого MediaSource, ни файла). */
  | { readonly kind: "no_video" }
  /**
   * Видео есть, но в форме, которую экстеншен пока честно не передаёт (решение 185 для HLS):
   * оно остаётся играть в браузере, сеанс с плеером даже не открывается.
   */
  | { readonly kind: "not_supported_yet"; readonly form: UnsupportedMediaForm }
  /** Лучшее видео вкладки под DRM (инвариант 4): не трогаем, оно играет в браузере. */
  | { readonly kind: "protected" }
  /**
   * Вкладка открыта раньше, чем встал наш скрипт (сессия 10d). Попап предлагает «Reload and open»;
   * сама передача страницу не перезагружает никогда: человек мог что-то вводить (решение 210).
   */
  | { readonly kind: "reload_needed"; readonly reason: ReloadReason }
  /**
   * Выбранного человеком видео (`video` в команде) нет в свежем опросе: страница успела его убрать или
   * перейти на другой документ. Другое видео вместо него НЕ отдаётся; попап обновляет список.
   */
  | { readonly kind: "video_gone" }
  /** Файл с локального адреса, настройка «блокировать» (решение 199). */
  | { readonly kind: "local_address_refused" }
  /** Файл с локального адреса, настройка «спрашивать»: попап 10 спросит человека про `host`. */
  | { readonly kind: "local_address_needs_consent"; readonly host: string }
  /** Связь с плеером не поднялась или плеер отказал (`reason` — для журнала и попапа 10). */
  | { readonly kind: "player_unavailable"; readonly reason: string }
  /** Внутренняя ошибка экстеншена (`reason` — для журнала). */
  | { readonly kind: "failed"; readonly reason: string };
