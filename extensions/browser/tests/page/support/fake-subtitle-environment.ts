// Фейковое окружение субтитров для тестов MAIN-мира (13b2): `fetch`, XMLHttpRequest, Response, `<track>`,
// `TextTrack`, VTTCue. Добавляется в фейковую страницу ДО `startHarness` (хуки встают на старте).
//
// Всё ведёт себя как браузер в тех местах, где от этого зависит наблюдатель: промис `fetch`, клон
// ответа с учётом отмены потока, события `load` XHR, события `addtrack` списка дорожек.

import { getEventListeners } from "node:events";
import type { FakePage } from "./fake-page.ts";

/** Что известно о ответе, который «пришёл по сети». */
export interface FakeNetworkReply {
  readonly url: string;
  readonly status?: number;
  readonly type?: string;
  readonly contentType?: string | null;
  readonly contentLength?: string | null;
  /** Тело одним куском. */
  readonly body?: Uint8Array;
  /** Или лениво порождаемые куски (для проверки потолка): сколько кусков по `chunkBytes` байт; бесконечно, пока не отменят. */
  readonly endlessChunkBytes?: number;
}

/** Учёт того, что случилось с клонами и потоками. */
export interface FakeResponseLedger {
  clones: number;
  cancelledStreams: number;
  chunksPulled: number;
}

export function addSubtitleEnvironment(page: FakePage) {
  const ledger: FakeResponseLedger = { clones: 0, cancelledStreams: 0, chunksPulled: 0 };

  class FakeResponse {
    readonly #reply: FakeNetworkReply;
    readonly #headers: Headers;
    constructor(reply: FakeNetworkReply) {
      this.#reply = reply;
      const headers = new Headers();
      if (reply.contentType !== null && reply.contentType !== undefined) headers.set("content-type", reply.contentType);
      if (reply.contentLength !== null && reply.contentLength !== undefined) headers.set("content-length", reply.contentLength);
      this.#headers = headers;
    }
    get url(): string {
      return this.#reply.url;
    }
    get status(): number {
      return this.#reply.status ?? 200;
    }
    get type(): string {
      return this.#reply.type ?? "basic";
    }
    get headers(): Headers {
      return this.#headers;
    }
    get body(): ReadableStream<Uint8Array> {
      const reply = this.#reply;
      if (reply.endlessChunkBytes !== undefined) {
        const chunkBytes = reply.endlessChunkBytes;
        return new ReadableStream<Uint8Array>({
          pull(controller) {
            ledger.chunksPulled += 1;
            controller.enqueue(new Uint8Array(chunkBytes).fill(0x61));
          },
          cancel() {
            ledger.cancelledStreams += 1;
          },
        });
      }
      const body = reply.body ?? new Uint8Array(0);
      return new ReadableStream<Uint8Array>({
        start(controller) {
          controller.enqueue(body.slice());
          controller.close();
        },
        cancel() {
          ledger.cancelledStreams += 1;
        },
      });
    }
    clone(): FakeResponse {
      if (lastFetch.cloneFailure !== null) throw lastFetch.cloneFailure;
      ledger.clones += 1;
      return new FakeResponse(this.#reply);
    }
  }

  /** Браузерный `fetch` фейка: отвечает заранее заданным ответом, ровно как промис. */
  const fetchCalls: string[] = [];
  const replies = new Map<string, FakeNetworkReply>();
  /** Промис, который «браузерный» `fetch` отдал последним: страница обязана получить именно его. */
  const lastFetch: { promise: Promise<FakeResponse> | null; cloneFailure: Error | null } = { promise: null, cloneFailure: null };
  function fakeFetch(input: string): Promise<FakeResponse> {
    fetchCalls.push(String(input));
    const reply = replies.get(String(input));
    const promise = reply === undefined ? Promise.reject(new TypeError("network error")) : Promise.resolve(new FakeResponse(reply));
    lastFetch.promise = promise;
    return promise;
  }

  class FakeXhr extends EventTarget {
    #responseType = "";
    #reply: FakeNetworkReply | null = null;
    #responseValue: unknown = null;
    #text = "";
    opened = "";
    open(_method: string, url: string): void {
      this.opened = String(url);
    }
    send(): void {}
    get status(): number {
      return this.#reply === null ? 0 : (this.#reply.status ?? 200);
    }
    get responseURL(): string {
      return this.#reply?.url ?? "";
    }
    get responseType(): string {
      return this.#responseType;
    }
    set responseType(value: string) {
      this.#responseType = value;
    }
    get responseText(): string {
      if (this.#responseType !== "" && this.#responseType !== "text") throw new DOMException("not text", "InvalidStateError");
      return this.#text;
    }
    get response(): unknown {
      return this.#responseValue;
    }
    getResponseHeader(name: string): string | null {
      const key = name.toLowerCase();
      if (key === "content-type") return this.#reply?.contentType ?? null;
      if (key === "content-length") return this.#reply?.contentLength ?? null;
      return null;
    }
    /** Тестовый помощник: сеть ответила — выставить поля по `responseType` и разослать `load`. */
    testRespond(reply: FakeNetworkReply, value: { text?: string; json?: unknown; buffer?: ArrayBuffer }): void {
      this.#reply = reply;
      this.#text = value.text ?? "";
      this.#responseValue = this.#responseType === "json" ? value.json : this.#responseType === "arraybuffer" ? value.buffer : this.#text;
      this.dispatchEvent(new Event("load"));
    }
  }

  // ---------------------------------------------------------------- дорожки

  /** Свойства на ПРОТОТИПЕ (как у браузера): читатели снимают именно прототипные геттеры. */
  function defineBackedAccessors(prototype: object, defaults: Record<string, unknown>): void {
    const stored = new WeakMap<object, Record<string, unknown>>();
    for (const [name, fallback] of Object.entries(defaults)) {
      Object.defineProperty(prototype, name, {
        configurable: true,
        get(this: object): unknown {
          const values = stored.get(this);
          return values !== undefined && name in values ? values[name] : fallback;
        },
        set(this: object, value: unknown) {
          const values = stored.get(this) ?? {};
          values[name] = value;
          stored.set(this, values);
        },
      });
    }
  }

  class FakeVTTCue {
    declare id: string;
    declare startTime: number;
    declare endTime: number;
    declare text: string;
    declare align: string;
    declare line: number | string;
    declare snapToLines: boolean;
    declare position: number | string;
    declare size: number;
    declare vertical: string;
  }
  defineBackedAccessors(FakeVTTCue.prototype, { id: "", startTime: 0, endTime: 0, text: "", align: "center", line: "auto", snapToLines: true, position: "auto", size: 100, vertical: "" });

  class FakeCueList {
    readonly items: FakeVTTCue[] = [];
    get length(): number {
      return this.items.length;
    }
  }
  // Индексный доступ `list[i]` как у настоящего списка реплик.
  const cueListProxy = (list: FakeCueList): FakeCueList =>
    new Proxy(list, { get: (target, key) => (typeof key === "string" && /^\d+$/.test(key) ? target.items[Number(key)] : Reflect.get(target, key)) });

  class FakeTextTrack extends EventTarget {
    declare mode: string;
    declare kind: string;
    declare language: string;
    declare label: string;
    readonly cueList = new FakeCueList();
    readonly cueListView = cueListProxy(this.cueList);
    get cues(): FakeCueList | null {
      return this.mode === "disabled" ? null : this.cueListView;
    }
  }

  defineBackedAccessors(FakeTextTrack.prototype, { mode: "disabled", kind: "subtitles", language: "", label: "" });

  class FakeTextTrackList extends EventTarget {
    readonly items: FakeTextTrack[] = [];
    get length(): number {
      return this.items.length;
    }
  }
  const trackListProxy = (list: FakeTextTrackList): FakeTextTrackList =>
    new Proxy(list, { get: (target, key) => (typeof key === "string" && /^\d+$/.test(key) ? target.items[Number(key)] : Reflect.get(target, key)) });

  class FakeTrackElement extends page.FakeElement {
    readonly textTrack = new FakeTextTrack();
    resolvedSrc = "";
    srclangValue = "";
    labelValue = "";
    constructor() {
      super("track");
    }
    get src(): string {
      return this.resolvedSrc;
    }
    get srclang(): string {
      return this.srclangValue;
    }
    get label(): string {
      return this.labelValue;
    }
    get track(): FakeTextTrack {
      return this.textTrack;
    }
  }

  // `video.textTracks` и `video.querySelectorAll("track")` — на прототипах, как у браузера.
  const textTrackLists = new WeakMap<object, FakeTextTrackList>();
  const textTrackListViews = new WeakMap<object, FakeTextTrackList>();
  const trackElements = new WeakMap<object, FakeTrackElement[]>();
  const mediaPrototype = Object.getPrototypeOf(page.FakeVideoElement.prototype) as object;
  Object.defineProperty(mediaPrototype, "textTracks", {
    configurable: true,
    get(this: object): FakeTextTrackList {
      let view = textTrackListViews.get(this);
      if (view === undefined) {
        const list = new FakeTextTrackList();
        textTrackLists.set(this, list);
        view = trackListProxy(list);
        textTrackListViews.set(this, view);
      }
      return view;
    },
  });
  Object.defineProperty(page.FakeElement.prototype, "querySelectorAll", {
    configurable: true,
    value(this: object, selector: string): FakeTrackElement[] {
      return selector === "track" ? [...(trackElements.get(this) ?? [])] : [];
    },
  });

  Object.assign(page.win, {
    fetch: fakeFetch,
    Response: FakeResponse,
    Headers,
    ReadableStream,
    ReadableStreamDefaultReader,
    XMLHttpRequest: FakeXhr,
    TextEncoder,
    JSON,
    HTMLTrackElement: FakeTrackElement,
    TextTrack: FakeTextTrack,
    TextTrackCue: FakeVTTCue,
    VTTCue: FakeVTTCue,
  });

  function listOf(video: object): FakeTextTrackList {
    void (video as { textTracks: unknown }).textTracks;
    const list = textTrackLists.get(video);
    if (list === undefined) throw new Error("track list is missing");
    return list;
  }

  return {
    ledger,
    fetchCalls,
    lastFetch,
    /** Заранее заданный ответ сети на адрес. */
    serve(reply: FakeNetworkReply): void {
      replies.set(reply.url, reply);
    },
    FakeXhr,
    /** `<track>` у видео; `mode` — режим его дорожки. */
    addTrackElement(video: object, init: { src: string; srclang?: string; label?: string; kind?: string; mode?: string }): FakeTrackElement {
      const element = new FakeTrackElement();
      element.resolvedSrc = init.src;
      element.srclangValue = init.srclang ?? "";
      element.labelValue = init.label ?? "";
      element.textTrack.kind = init.kind ?? "subtitles";
      element.textTrack.mode = init.mode ?? "disabled";
      element.textTrack.language = init.srclang ?? "";
      element.textTrack.label = init.label ?? "";
      trackElements.set(video, [...(trackElements.get(video) ?? []), element]);
      const list = listOf(video);
      list.items.push(element.textTrack);
      list.dispatchEvent(new Event("addtrack"));
      return element;
    },
    /** Дорожка без файла (`video.addTextTrack`, как у hls.js). */
    addTextTrack(video: object, init: { kind?: string; label?: string; language?: string; mode?: string }): FakeTextTrack {
      const track = new FakeTextTrack();
      track.kind = init.kind ?? "subtitles";
      track.label = init.label ?? "";
      track.language = init.language ?? "";
      track.mode = init.mode ?? "hidden";
      const list = listOf(video);
      list.items.push(track);
      list.dispatchEvent(new Event("addtrack"));
      return track;
    },
    addCue(track: FakeTextTrack, cue: Partial<FakeVTTCue> & { startTime: number; endTime: number; text: string }): void {
      const made = new FakeVTTCue();
      Object.assign(made, cue);
      track.cueList.items.push(made);
    },
    /** Сколько слушателей сейчас стоит на списке дорожек видео (наши `addtrack` и `change`). */
    trackListListeners(video: object): number {
      const list = listOf(video);
      return getEventListeners(list, "addtrack").length + getEventListeners(list, "change").length;
    },
    /** Событие `change` списка дорожек (зритель включил/выключил субтитры). */
    changeTracks(video: object): void {
      listOf(video).dispatchEvent(new Event("change"));
    },
  };
}

export type FakeSubtitleEnvironment = ReturnType<typeof addSubtitleEnvironment>;
