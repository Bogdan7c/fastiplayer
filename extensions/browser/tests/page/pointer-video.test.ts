// «Какое видео под указателем» (сессия 10c). Итог каждого теста — то, что получит фон: какое видео названо
// и назван ли вообще кто-то. Часть 1: поиск на фейковом дереве. Часть 2: снятие оригиналов (страница
// их позже подменит). Часть 3: настоящий мост на фейковой странице, ответ идёт по каналу участка 1.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import type { ViewportRect } from "../../src/page/dom-readers.ts";
import { capturePointerReaders, PointerVideo, type PointerEventReaders, type PointerTreeReaders } from "../../src/page/pointer-video.ts";
import { parseRightClickedReply, parseVideoAtPointReply } from "../../src/shared/entry-validation.ts";
import { VIDEO_MP4, AUDIO_MP4 } from "./support/media-samples.ts";
import { createFakePage, type FakePage } from "./support/fake-page.ts";
import { handshake, startHarness, type BridgeHarness } from "./support/bridge-harness.ts";

// ------------------------------------------------------------------ часть 1: поиск на фейковом дереве

/** Узел фейкового дерева: родитель в составном дереве (хозяин — через `parent` корня shadow). */
interface TestNode {
  readonly name: string;
  parent: TestNode | null;
  connected: boolean;
  rect: ViewportRect;
}

const rect = (left: number, top: number, right: number, bottom: number): ViewportRect => ({ left, top, right, bottom });
const node = (name: string, box: ViewportRect = rect(0, 0, 0, 0), parent: TestNode | null = null): TestNode => ({ name, parent, connected: true, rect: box });

/** Событие мыши теста: путь, координаты, «настоящесть». */
interface TestEvent {
  readonly trusted: boolean;
  readonly path: readonly unknown[];
  readonly x: number;
  readonly y: number;
}

function setupSearch(videos: readonly TestNode[], options: { stack?: readonly unknown[] } = {}) {
  const inPlayer = new Set<unknown>();
  let stack: readonly unknown[] = options.stack ?? [];
  let readFailure = false;
  const events: PointerEventReaders = {
    isTrusted: (event) => (event as TestEvent).trusted,
    composedPath: (event) => {
      if (readFailure) throw new TypeError("not an event");
      return (event as TestEvent).path;
    },
    position: (event) => ({ x: (event as TestEvent).x, y: (event as TestEvent).y }),
  };
  const tree: PointerTreeReaders = {
    elementsFromPoint: () => stack,
    composedParentOf: (target) => (target as TestNode).parent,
  };
  const keys = new Map<unknown, number>();
  const search = new PointerVideo({
    documentTag: "doc-tag",
    events,
    tree,
    videoElements: () => videos as unknown as Element[],
    keyOf: (element) => {
      if (!keys.has(element)) keys.set(element, keys.size + 1);
      return keys.get(element) as number;
    },
    isVideoElement: (candidate) => videos.includes(candidate as TestNode),
    isConnected: (target) => (target as TestNode).connected,
    rectOf: (element) => (element as unknown as TestNode).rect,
    isInPlayer: (element) => inPlayer.has(element),
  });
  const rightClick = (event: Partial<TestEvent> = {}): void =>
    search.noteContextMenu({ trusted: true, path: [], x: 10, y: 10, ...event } satisfies TestEvent);
  return {
    search,
    inPlayer,
    rightClick,
    setStack: (next: readonly unknown[]) => void (stack = next),
    breakEventReads: () => void (readFailure = true),
    keyOf: (video: TestNode) => keys.get(video),
  };
}

describe("видео последнего правого клика", () => {
  it("видео есть в composedPath события — оно и называется (метка документа и стабильный ключ)", () => {
    const video = node("video", rect(0, 0, 100, 100));
    const world = setupSearch([video]);
    world.rightClick({ path: [video, node("div"), node("body")] });
    const answered = world.search.describeRightClicked();
    assert.deepEqual(answered, { document: "doc-tag", element: 1 });
    assert.deepEqual(world.search.describeRightClicked(), answered, "тот же ключ при повторном вопросе");
  });

  it("слой сайта над видео: в пути видео нет, видео находится по точке клика", () => {
    const video = node("video", rect(0, 0, 400, 300));
    const layer = node("site-layer", rect(0, 0, 400, 300));
    const world = setupSearch([video], { stack: [layer, video] });
    world.rightClick({ path: [layer, node("body")], x: 200, y: 150 });
    assert.deepEqual(world.search.describeRightClicked(), { document: "doc-tag", element: 1 });
  });

  it("закрытый shadow root: путь кончается хозяином, стопка отдаёт хозяина вместо видео; верхнее из двух видео — то, чей хозяин выше", () => {
    const hostBelow = node("host-below");
    const hostAbove = node("host-above");
    const rootBelow = node("root-below", rect(0, 0, 0, 0), hostBelow);
    const rootAbove = node("root-above", rect(0, 0, 0, 0), hostAbove);
    // Видео под в документе первое, и оно крупнее: порядок и размер не должны решать.
    const videoBelow = node("video-below", rect(0, 0, 800, 600), rootBelow);
    const videoAbove = node("video-above", rect(100, 100, 300, 250), rootAbove);
    const world = setupSearch([videoBelow, videoAbove], { stack: [hostAbove, hostBelow] });
    world.rightClick({ path: [hostAbove, node("body")], x: 150, y: 150 });
    const answered = world.search.describeRightClicked();
    assert.ok(answered !== null);
    assert.equal(answered.element, world.keyOf(videoAbove));
  });

  it("ранг не определился (видео не попало в стопку): выбирается наибольшее по площади", () => {
    const small = node("small", rect(0, 0, 100, 100));
    const large = node("large", rect(0, 0, 500, 500));
    const world = setupSearch([small, large], { stack: [node("unrelated")] });
    world.rightClick({ x: 50, y: 50 });
    const answered = world.search.describeRightClicked();
    assert.ok(answered !== null);
    assert.equal(answered.element, world.keyOf(large));
  });

  it("определившийся ранг выигрывает у неопределившегося, даже если тот крупнее", () => {
    const ranked = node("ranked", rect(0, 0, 100, 100));
    const unranked = node("unranked", rect(0, 0, 900, 900));
    const world = setupSearch([unranked, ranked], { stack: [ranked] });
    world.rightClick({ x: 50, y: 50 });
    assert.equal(world.search.describeRightClicked()?.element, world.keyOf(ranked));
  });

  it("клик вне прямоугольников видео — «ничего», и это перезаписывает прошлое видео", () => {
    const video = node("video", rect(0, 0, 100, 100));
    const world = setupSearch([video]);
    world.rightClick({ path: [video] });
    assert.notEqual(world.search.describeRightClicked(), null);
    world.rightClick({ path: [node("p")], x: 900, y: 900 });
    assert.equal(world.search.describeRightClicked(), null, "старое видео не должно уехать в плеер после клика мимо");
  });

  it("событие, созданное скриптом страницы (isTrusted=false), память не трогает — ни писать, ни стирать", () => {
    const video = node("video", rect(0, 0, 100, 100));
    const other = node("other", rect(200, 200, 300, 300));
    const world = setupSearch([video, other]);
    world.rightClick({ path: [video] });
    world.rightClick({ trusted: false, path: [other], x: 250, y: 250 });
    assert.equal(world.search.describeRightClicked()?.element, world.keyOf(video), "подделка не подменила видео");
    world.rightClick({ trusted: false, path: [], x: 900, y: 900 });
    assert.equal(world.search.describeRightClicked()?.element, world.keyOf(video), "подделка не стёрла память");
  });

  it("устаревшая память: видео убрали из документа после клика — «ничего», а не отсоединённый элемент", () => {
    const video = node("video", rect(0, 0, 100, 100));
    const world = setupSearch([video]);
    world.rightClick({ path: [video] });
    video.connected = false;
    assert.equal(world.search.describeRightClicked(), null);
  });

  it("до первого правого клика отвечает «ничего»", () => {
    assert.equal(setupSearch([node("video", rect(0, 0, 10, 10))]).search.describeRightClicked(), null);
  });

  it("событие нечитаемо (сбой чтения): прошлое видео забывается, исключение не выходит на страницу", () => {
    const video = node("video", rect(0, 0, 100, 100));
    const world = setupSearch([video]);
    world.rightClick({ path: [video] });
    world.breakEventReads();
    assert.doesNotThrow(() => world.rightClick({ path: [video] }));
    assert.equal(world.search.describeRightClicked(), null);
  });
});

describe("видео под точкой (наведение)", () => {
  it("называет видео, его прямоугольник и in_player; точка мимо — «ничего»", () => {
    const video = node("video", rect(10, 20, 410, 320));
    const world = setupSearch([video], { stack: [video] });
    const found = world.search.videoAtPoint(100, 100);
    assert.deepEqual(found, { document: "doc-tag", element: 1, rect: { left: 10, top: 20, width: 400, height: 300 }, in_player: false });
    assert.equal(world.search.videoAtPoint(5, 5), null);
  });

  it("in_player берётся у владельца передачи через намерение, а не из полей: видео под тенью — true", () => {
    const video = node("video", rect(0, 0, 100, 100));
    const world = setupSearch([video], { stack: [video] });
    world.inPlayer.add(video);
    assert.equal(world.search.videoAtPoint(50, 50)?.in_player, true);
    world.inPlayer.delete(video);
    assert.equal(world.search.videoAtPoint(50, 50)?.in_player, false);
  });

  it("скрытое видео (нулевая площадь) и отсоединённое не находятся", () => {
    const empty = node("empty", rect(50, 50, 50, 50));
    const detached = node("detached", rect(0, 0, 100, 100));
    detached.connected = false;
    const world = setupSearch([empty, detached]);
    assert.equal(world.search.videoAtPoint(50, 50), null);
  });

  it("не пишет в память правого клика и не читает её", () => {
    const video = node("video", rect(0, 0, 100, 100));
    const world = setupSearch([video], { stack: [video] });
    world.search.videoAtPoint(50, 50);
    assert.equal(world.search.describeRightClicked(), null);
  });
});

// ------------------------------------------------------------------ часть 2: снятие оригиналов

/** Окно, у которого есть всё для оригиналов указателя, на классах Node. */
function pointerWindow(page: FakePage) {
  const stack: unknown[] = [];
  class MouseLike extends Event {
    get clientX(): number {
      return 12;
    }
    get clientY(): number {
      return 34;
    }
    override composedPath(): EventTarget[] {
      return [page.document];
    }
  }
  Object.defineProperty(page.document.constructor.prototype, "elementsFromPoint", { configurable: true, writable: true, value: () => [...stack] });
  return { win: Object.assign(page.win, { MouseEvent: MouseLike, Event: MouseLike }), stack, MouseLike };
}

describe("снятие оригиналов указателя", () => {
  it("окружение без elementsFromPoint (старый браузер, фейк) — null: вход по указателю не включается", () => {
    assert.equal(capturePointerReaders(createFakePage().win), null);
  });

  it("читает путь, координаты и стопку; isTrusted — собственное свойство события", () => {
    const page = createFakePage();
    const { win, stack, MouseLike } = pointerWindow(page);
    const readers = capturePointerReaders(win);
    assert.ok(readers !== null);
    const event = new MouseLike("contextmenu");
    assert.deepEqual(readers.events.composedPath(event), [page.document]);
    assert.deepEqual(readers.events.position(event), { x: 12, y: 34 });
    assert.equal(readers.events.isTrusted(event), false);
    assert.equal(readers.events.isTrusted(page.browserEvent("contextmenu")), true);
    const video = page.addVideo();
    stack.push(video);
    assert.deepEqual(readers.tree.elementsFromPoint(1, 1), [video]);
  });

  it("составной родитель: над корнем shadow root — хозяин; страница позже подменила parentNode — чтение не меняется", () => {
    const page = createFakePage();
    const { win } = pointerWindow(page);
    const readers = capturePointerReaders(win);
    assert.ok(readers !== null);
    const host = new page.FakeElement("div");
    const root = host.attachShadow({ mode: "closed" });
    const video = page.addVideoInShadowRoot(root);
    Object.defineProperty(video, "parentNode", { value: null, configurable: true });
    Object.defineProperty(page.win.Node.prototype, "parentNode", { get: () => null, configurable: true });
    assert.equal(readers.tree.composedParentOf(video), host, "от видео в закрытом root подъём доходит до хозяина");
  });
});

// ------------------------------------------------------------------ часть 3: настоящий мост

const VIDEO_MIME = 'video/mp4; codecs="avc1.42E01E"';
const AUDIO_MIME = 'audio/mp4; codecs="mp4a.40.2"';

/** Страница с играющим MSE-видео (его можно отдать плееру). */
function pageWithPlayingVideo(page: FakePage) {
  const video = page.addVideo();
  video.rect = { left: 0, top: 0, right: 640, bottom: 360 };
  const mediaSource = page.attachMediaSource(video);
  mediaSource.duration = 0.4;
  mediaSource.addSourceBuffer(VIDEO_MIME).appendBuffer(VIDEO_MP4.bytes().slice());
  mediaSource.addSourceBuffer(AUDIO_MIME).appendBuffer(AUDIO_MP4.bytes().slice());
  video.testSetState({ paused: false, currentTime: 0.25 });
  return video;
}

/** Страница с оригиналами указателя: стопка элементов задаётся тестом. */
function startPointerHarness(): { harness: BridgeHarness; stack: unknown[]; contextMenu(options: { path: unknown[]; trusted: boolean }): void } {
  const page = createFakePage();
  const stack: unknown[] = [];
  class PathEvent extends Event {
    testPath: unknown[] = [];
    // Хук DRM читает `target` с собственного прототипа события.
    override get target(): EventTarget | null {
      return super.target;
    }
    get clientX(): number {
      return 5;
    }
    get clientY(): number {
      return 5;
    }
    override composedPath(): EventTarget[] {
      return this.testPath as EventTarget[];
    }
  }
  Object.defineProperty(page.document.constructor.prototype, "elementsFromPoint", { configurable: true, writable: true, value: () => [...stack] });
  Object.assign(page.win, { Event: PathEvent, MouseEvent: PathEvent });
  const harness = startHarness({ page });
  return {
    harness,
    stack,
    contextMenu: ({ path, trusted }) => {
      const event = new PathEvent("contextmenu");
      event.testPath = path;
      if (trusted) Object.defineProperty(event, "isTrusted", { value: true });
      page.win.dispatchEvent(event);
    },
  };
}

describe("мост: вопросы про указатель по каналу участка 1", () => {
  it("правый клик по видео -> describe_right_clicked отвечает тем же видео, что и опрос кандидатов; ответ проходит проверку пограничника", async () => {
    const { harness, contextMenu } = startPointerHarness();
    const video = pageWithPlayingVideo(harness.page);
    const isolated = handshake(harness.page);
    try {
      contextMenu({ path: [video], trusted: true });
      isolated.send({ kind: "describe_candidates", request_id: 1 });
      isolated.send({ kind: "describe_right_clicked", request_id: 2 });
      await isolated.waitFor(2);
      const candidates = isolated.received.find((message) => message.kind === "candidates");
      const reply = isolated.received.find((message) => (message as { kind: string }).kind === "right_clicked");
      assert.ok(candidates?.kind === "candidates" && reply !== undefined);
      const parsed = parseRightClickedReply(reply);
      assert.ok(parsed !== undefined, "пограничник принимает ответ");
      assert.equal(parsed.request_id, 2);
      assert.deepEqual(parsed.video, { document: candidates.candidates[0]?.document, element: candidates.candidates[0]?.element });
    } finally {
      isolated.close();
    }
  });

  it("клик, созданный страницей, не считается: ответ «ничего»", async () => {
    const { harness, contextMenu } = startPointerHarness();
    const video = pageWithPlayingVideo(harness.page);
    const isolated = handshake(harness.page);
    try {
      contextMenu({ path: [video], trusted: false });
      isolated.send({ kind: "describe_right_clicked", request_id: 3 });
      await isolated.waitFor(1);
      assert.deepEqual(parseRightClickedReply(isolated.received[0]), { kind: "right_clicked", request_id: 3, video: null });
    } finally {
      isolated.close();
    }
  });

  it("video_at_point: рамка видео, in_player=false до передачи и true под тенью", async () => {
    const { harness, stack } = startPointerHarness();
    const video = pageWithPlayingVideo(harness.page);
    stack.push(video);
    const isolated = handshake(harness.page);
    try {
      isolated.send({ kind: "video_at_point", request_id: 4, x: 100, y: 100 });
      await isolated.waitFor(1);
      const before = parseVideoAtPointReply(isolated.received[0]);
      assert.ok(before?.video != null);
      assert.deepEqual(before.video.rect, { left: 0, top: 0, width: 640, height: 360 });
      assert.equal(before.video.in_player, false);

      isolated.received.length = 0;
      isolated.send({ kind: "begin_handoff", request_id: 5, document: before.video.document, element: before.video.element });
      await isolated.waitFor(1);
      isolated.send({ kind: "video_at_point", request_id: 6, x: 100, y: 100 });
      await isolated.waitFor(isolated.received.length + 1);
      const during = isolated.received.map(parseVideoAtPointReply).find((reply) => reply?.request_id === 6);
      assert.equal(during?.video?.in_player, true, "видео под тенью называется «в плеере»");
      assert.equal(harness.shadow.started.length, 1);
    } finally {
      isolated.close();
    }
  });

  it("окружение без оригиналов указателя: вопросы получают честное «ничего», мост работает", async () => {
    const harness = startHarness();
    const isolated = handshake(harness.page);
    try {
      isolated.send({ kind: "describe_right_clicked", request_id: 1 });
      isolated.send({ kind: "video_at_point", request_id: 2, x: 1, y: 1 });
      await isolated.waitFor(2);
      assert.deepEqual(isolated.received.map((reply) => (reply as { video?: unknown }).video), [null, null]);
      assert.equal(harness.bridge.pointerVideo, null);
    } finally {
      isolated.close();
    }
  });

  it("кривой вопрос (дробные координаты — допустимы, строка вместо числа — нет) без ответа и без сбоя", async () => {
    const harness = startHarness();
    const isolated = handshake(harness.page);
    try {
      isolated.send({ kind: "video_at_point", request_id: 1, x: "1", y: 2 });
      isolated.send({ kind: "describe_right_clicked", request_id: 2, extra: true });
      isolated.send({ kind: "describe_right_clicked", request_id: 7 });
      await isolated.waitFor(1);
      assert.equal(isolated.received.length, 1);
      assert.equal((isolated.received[0] as { request_id: number }).request_id, 7);
    } finally {
      isolated.close();
    }
  });
});
