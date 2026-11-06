// Чтение и постановка позиции из изолированного мира (сессия 10d). Внедряемая функция пересобирается из
// текста, как это делает браузер: замыкание на модуль здесь упало бы.
import assert from "node:assert/strict";
import { afterEach, describe, it } from "node:test";
import { createLateVideoReader, LATE_POSITION_TOLERANCE_S, parseVideoFacts, visitFrameVideos, type LateSeekTarget } from "../../src/background/entries/late-video-reader.ts";

interface FakeElement {
  localName: string;
  currentTime?: number;
  duration?: number;
  paused?: boolean;
  ended?: boolean;
  shadowRoot: unknown;
  getBoundingClientRect?: () => { left: number; top: number; right: number; bottom: number };
}

const fakeVideo = (currentTime: number, duration: number, paused = false): FakeElement => ({
  localName: "video",
  currentTime,
  duration,
  paused,
  ended: false,
  shadowRoot: null,
  getBoundingClientRect: () => ({ left: 0, top: 0, right: 100, bottom: 50 }),
});

const fakeRoot = (...elements: FakeElement[]) => ({ querySelectorAll: () => elements });
const visit = new Function(`return (${visitFrameVideos.toString()})`)() as (target: LateSeekTarget | null, tolerance: number) => Record<string, unknown>[];

function installPage(root: ReturnType<typeof fakeRoot>, extensionApi?: unknown): void {
  const scope = globalThis as Record<string, unknown>;
  scope["document"] = root;
  scope["window"] = { innerWidth: 800, innerHeight: 600 };
  if (extensionApi !== undefined) scope["chrome"] = extensionApi;
}

describe("видео страницы из изолированного мира", () => {
  afterEach(() => {
    const scope = globalThis as Record<string, unknown>;
    delete scope["document"];
    delete scope["window"];
    delete scope["chrome"];
  });

  it("читает позицию, длительность, игру и видимую площадь", () => {
    installPage(fakeRoot(fakeVideo(12, 300), { localName: "div", shadowRoot: null }));
    assert.deepEqual(visit(null, LATE_POSITION_TOLERANCE_S), [{ position_s: 12, duration_s: 300, playing: true, visible_area_px: 5_000 }]);
  });

  it("эфир отдаёт неизвестную длительность", () => {
    installPage(fakeRoot(fakeVideo(12, Number.POSITIVE_INFINITY)));
    assert.equal(visit(null, LATE_POSITION_TOLERANCE_S)[0]?.["duration_s"], null);
  });

  it("находит видео в закрытом shadow root через API экстеншена", () => {
    const host: FakeElement = { localName: "div", shadowRoot: null };
    const hidden = fakeRoot(fakeVideo(7, 90));
    installPage(fakeRoot(host), { dom: { openOrClosedShadowRoot: (element: unknown) => (element === host ? hidden : null) } });
    assert.deepEqual(visit(null, LATE_POSITION_TOLERANCE_S).map((video) => video["position_s"]), [7]);
  });

  it("API бросил на элементе: открытый shadow root всё равно обойдён", () => {
    const host: FakeElement = { localName: "svg", shadowRoot: fakeRoot(fakeVideo(3, 90)) };
    const throwing = { dom: { openOrClosedShadowRoot: () => { throw new Error("not an HTMLElement"); } } };
    installPage(fakeRoot(host), throwing);
    assert.deepEqual(visit(null, LATE_POSITION_TOLERANCE_S).map((video) => video["position_s"]), [3]);
  });

  it("секунду получает только видео нужной длительности", () => {
    const film = fakeVideo(0, 3_000);
    const banner = fakeVideo(0, 15);
    installPage(fakeRoot(banner, film));
    visit({ duration_s: 3_001, position_s: 600 }, LATE_POSITION_TOLERANCE_S);
    assert.equal(film.currentTime, 600);
    assert.equal(banner.currentTime, 0, "рекламе секунду фильма не ставим");
  });

  it("видео без метаданных не трогается: NaN не совпадает ни с какой длительностью", () => {
    const empty = fakeVideo(0, Number.NaN);
    installPage(fakeRoot(empty));
    visit({ duration_s: 3_000, position_s: 600 }, LATE_POSITION_TOLERANCE_S);
    assert.equal(empty.currentTime, 0);
  });
});

describe("разбор ответов фреймов", () => {
  const good = { position_s: 1, duration_s: 2, playing: true, visible_area_px: 3 };

  it("собирает факты всех фреймов, пустые и кривые отбрасывает", () => {
    const facts = parseVideoFacts([
      { result: [good] },
      undefined,
      { result: "мусор" },
      { result: [{ ...good, position_s: Number.NaN }, { ...good, playing: "да" }, { ...good, duration_s: null }] },
    ]);
    assert.deepEqual(facts, [good, { ...good, duration_s: null }]);
  });

  it("чтение и постановка идут через один шов: цель доходит до внедрения", async () => {
    const targets: unknown[] = [];
    const reader = createLateVideoReader((_tabId, target) => {
      targets.push(target);
      return Promise.resolve([{ result: [good] }]);
    });
    assert.deepEqual(await reader.read(1), [good]);
    await reader.seek(1, { duration_s: 10, position_s: 5 });
    assert.deepEqual(targets, [null, { duration_s: 10, position_s: 5 }]);
  });
});
