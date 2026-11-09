// Наблюдение за `<track>` и `TextTrack` переданного видео (сессия 13b2, решения 271, 272, 275).
//
// Что делает для каждой дорожки видео (`kind` — только `subtitles` и `captions`; главы, метаданные и
// описания — не субтитры):
//  - `<track src>` с адресом http(s) — событие `subtitle_track_source`: документ качает ФОН через сеть
//    браузера (куки, заслон локальных адресов, Referer правилом). Один раз на элемент;
//  - `<track>` без http(s) адреса (`blob:`, `data:`) и `TextTrack` без `<track>` (hls.js, dash.js:
//    `addTextTrack`) с режимом не `disabled` — реплики превращаются в WebVTT (`web-vtt-builder.ts`)
//    и уходят документом `text_track_cues`. Выключенная дорожка (`disabled`) не читается: браузер не
//    отдаёт её реплики. Растущая дорожка пересылается целиком под тем же ключом, только если реплик
//    стало больше;
//  - на `display` смотрим один раз, в момент отправки: дальше «скачала = включено» (271) — дело склада,
//    а смену режима `<track src>` плееру не сообщаем (известное ограничение, `display` в протоколе 1.5 не обновляется).
//
// Когда смотрим: сразу при старте, на `addtrack`/`change` списка `textTracks` и по тикам, которые
// уже приходят на страницу от часов плеера, не чаще раза в `TRACK_GROWTH_CHECK_INTERVAL_MS`. Таймеры
// не используются: скрытая вкладка Chromium будит их раз в минуту, а тики часов идут всегда.

import { nameSubtitleDocumentByAddress } from "../../adapters/registry/page-world.ts";
import { labelOrNull, languageOrNull } from "../../adapters/capabilities/subtitle-naming/language-tag.ts";
import type { PageSubtitleKey, PageSubtitleNaming } from "../../shared/page-subtitle-contract.ts";
import type { ReferrerPolicyName } from "../../shared/referrer-policy.ts";
import type { PageSubtitleDisplay } from "../../protocol/session/page-subtitles.ts";
import type { ChannelOriginals } from "../page-channel.ts";
import type { OddityReporter } from "../prototype-wrap.ts";
import type { ByteTools } from "./byte-tools.ts";
import { MAX_SUBTITLE_RESPONSE_BYTES } from "./response-filter.ts";
import type { SubtitleKeyAllocator } from "./subtitle-keys.ts";
import type { SubtitleSender } from "./subtitle-sender.ts";
import type { TextTrackMode, TrackReaders } from "./track-readers.ts";
import { buildWebVttFromCues } from "./web-vtt-builder.ts";

/**
 * Как часто проверяем рост реплик дорожек, мс. Часы плеера тикают ~4 раза в секунду; реплики hls.js
 * приходят вместе с сегментами (раз в несколько секунд), так что 5 с — это «почти сразу» для зрителя и
 * одна пересборка документа на 20 тиков вместо пересборки на каждый.
 */
export const TRACK_GROWTH_CHECK_INTERVAL_MS = 5_000;

/** Больше реплик одной дорожки не читаем: фильм на три часа — до 5 000 реплик, 50 000 — запас на лекции и ТВ. */
const MAX_CUES_PER_TRACK = 50_000;

/** Виды дорожек, которые показывают речь: остальное (chapters, metadata, descriptions) — не субтитры. */
const SUBTITLE_TRACK_KINDS: readonly string[] = ["subtitles", "captions"];

/** Факты для Referer закачки `<track src>` — те же, что у `file_source` (решение 202). */
export interface ReferrerFacts {
  readonly referrer_origin: string | null;
  readonly referrer_policy: ReferrerPolicyName;
}

export interface TrackWatchTools {
  readonly readers: TrackReaders;
  readonly keys: SubtitleKeyAllocator;
  readonly bytes: ByteTools;
  readonly channel: Pick<ChannelOriginals, "addListener" | "removeListener">;
  readonly now: () => number;
  readonly reportOddity: OddityReporter;
}

/** Что известно о дорожке с файлом, уже объявленной фону. */
interface CueTrackState {
  readonly key: PageSubtitleKey;
  /** Сколько реплик было в дорожке при последней отправке (сырое число, не число годных). */
  readonly sentRawCueCount: number;
}

function isHttpAddress(address: string): boolean {
  return /^https?:\/\//i.test(address);
}

function displayOf(mode: TextTrackMode): PageSubtitleDisplay {
  return mode === "showing" ? "showing" : "not_showing";
}

export class TrackSubtitleWatch {
  readonly #tools: TrackWatchTools;
  readonly #video: object;
  readonly #sender: SubtitleSender;
  readonly #referrerFacts: () => ReferrerFacts;
  /** Ключи `<track src>` (по элементу), уже объявленных фону: повторно не объявляем. */
  readonly #announcedSources = new WeakMap<object, PageSubtitleKey>();
  /** Дорожки с репликами, уже отправленные хотя бы раз (по `TextTrack`). */
  readonly #cueTracks = new WeakMap<object, CueTrackState>();
  #lastCheckMs = Number.NEGATIVE_INFINITY;
  #trackList: object | null = null;
  #stopped = false;

  /** Обработчик событий списка дорожек: один объект на жизнь наблюдения, чтобы его можно было снять. */
  readonly #onTrackListEvent = (): void => this.scan();

  constructor(tools: TrackWatchTools, video: object, sender: SubtitleSender, referrerFacts: () => ReferrerFacts) {
    this.#tools = tools;
    this.#video = video;
    this.#sender = sender;
    this.#referrerFacts = referrerFacts;
  }

  /** Начать: слушатели списка дорожек и первый осмотр. */
  start(): void {
    try {
      this.#trackList = this.#tools.readers.textTrackListOf(this.#video);
      this.#tools.channel.addListener(this.#trackList, "addtrack", this.#onTrackListEvent, false);
      this.#tools.channel.addListener(this.#trackList, "change", this.#onTrackListEvent, false);
    } catch {
      this.#tools.reportOddity("could not listen to text tracks");
    }
    this.scan();
  }

  /** Тик часов плеера: осмотр не чаще раза в `TRACK_GROWTH_CHECK_INTERVAL_MS`. */
  tick(): void {
    if (this.#stopped) return;
    const now = this.#tools.now();
    if (Number.isFinite(this.#lastCheckMs) && !(now - this.#lastCheckMs >= TRACK_GROWTH_CHECK_INTERVAL_MS)) return;
    this.scan();
  }

  /** Передача кончилась: слушатели сняты, больше ничего не отправляется. */
  stop(): void {
    this.#stopped = true;
    const list = this.#trackList;
    this.#trackList = null;
    if (list === null) return;
    try {
      this.#tools.channel.removeListener(list, "addtrack", this.#onTrackListEvent, false);
      this.#tools.channel.removeListener(list, "change", this.#onTrackListEvent, false);
    } catch {
      this.#tools.reportOddity("could not stop listening to text tracks");
    }
  }

  /** Осмотреть все дорожки видео. Не бросает: сбой одной дорожки не мешает остальным. */
  scan(): void {
    if (this.#stopped) return;
    this.#lastCheckMs = this.#tools.now();
    const { readers } = this.#tools;
    let elementTracks = new Set<object>();
    try {
      for (const trackElement of readers.trackElementsOf(this.#video)) {
        const track = readers.textTrackOfElement(trackElement);
        if (track !== null) elementTracks.add(track);
        this.#guarded(() => this.#processTrackElement(trackElement, track));
      }
    } catch {
      this.#tools.reportOddity("reading track elements failed");
      elementTracks = new Set<object>();
    }
    try {
      for (const track of readers.textTracksIn(readers.textTrackListOf(this.#video))) {
        if (!elementTracks.has(track)) this.#guarded(() => this.#processCueTrack(track));
      }
    } catch {
      this.#tools.reportOddity("reading text tracks failed");
    }
  }

  #guarded(step: () => void): void {
    try {
      step();
    } catch {
      this.#tools.reportOddity("processing a subtitle track failed");
    }
  }

  /** `<track>`: http(s) адрес — фону (он качает сам); иначе — реплики его `TextTrack`. */
  #processTrackElement(trackElement: object, track: object | null): void {
    const { readers } = this.#tools;
    if (track !== null && !SUBTITLE_TRACK_KINDS.includes(readers.kindOf(track))) return;
    const facts = readers.trackElementFacts(trackElement);
    if (!isHttpAddress(facts.src)) {
      if (track !== null) this.#processCueTrack(track);
      return;
    }
    if (this.#announcedSources.has(trackElement)) return;
    const key = this.#tools.keys.next();
    if (key === null) return;
    const byAddress = nameSubtitleDocumentByAddress(facts.src);
    const naming: PageSubtitleNaming = {
      language: languageOrNull(facts.srclang) ?? byAddress.language,
      label: labelOrNull(facts.label),
      auto_generated: byAddress.auto_generated,
    };
    const mode = track === null ? "disabled" : readers.modeOf(track);
    const outcome = this.#sender.sendTrackSource({ key, naming, display: displayOf(mode), url: facts.src, ...this.#referrerFacts() });
    // Объявляем один раз, даже если отправка не удалась: повтор того же события сеанс не лечит.
    if (outcome !== "refused") this.#announcedSources.set(trackElement, key);
  }

  /** Дорожка с репликами в браузере (без файла или с `blob:`/`data:`): собрать WebVTT, если реплик стало больше. */
  #processCueTrack(track: object): void {
    const { readers, bytes } = this.#tools;
    if (!SUBTITLE_TRACK_KINDS.includes(readers.kindOf(track))) return;
    const mode = readers.modeOf(track);
    if (mode === "disabled") return;
    const rawCueCount = readers.cueCountOf(track);
    if (rawCueCount === null || rawCueCount === 0) return;
    const previous = this.#cueTracks.get(track);
    if (previous !== undefined && rawCueCount <= previous.sentRawCueCount) return;
    const cues = readers.cuesOf(track, MAX_CUES_PER_TRACK);
    if (cues === null || bytes.encodeUtf8 === null) return;
    const built = buildWebVttFromCues(cues);
    if (built.cueCount === 0) return;
    const document = bytes.encodeUtf8(built.text);
    if (bytes.lengthOf(document) > MAX_SUBTITLE_RESPONSE_BYTES) {
      this.#tools.reportOddity("text track cues exceed the subtitle document limit");
      return;
    }
    const key = previous?.key ?? this.#tools.keys.next();
    if (key === null) return;
    const outcome = this.#sender.sendDocument({
      key,
      naming: { language: languageOrNull(readers.languageOf(track)), label: labelOrNull(readers.labelOf(track)), auto_generated: false },
      format_hint: "web_vtt",
      origin: "text_track_cues",
      display: displayOf(mode),
      bytes: document,
    });
    // Отказ счётчика копий не запоминаем как отправленное: следующий осмотр попробует снова.
    if (outcome === "sent") this.#cueTracks.set(track, { key, sentRawCueCount: rawCueCount });
  }
}
