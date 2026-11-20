// YouTube Shorts — лента одного элемента по списку адресов (сессия 16b3c, решения 374, 381, 382) на фоне целиком.
// Shorts листается новым поколением ТОГО ЖЕ элемента (новый MediaSource), адрес меняется на каждом ролике, паузы на
// шаге нет (наблюдение 16b3, S1–S4). Проверяется то, что видит плеер (поколения, пауза, `tab_gone`, коды закрытия),
// страница (снятие тени) и что сериал с номером серии в адресе по-прежнему судится правилами 16a и 306.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { FEED_SECTION_LEAVE_GRACE_MS } from "../../src/background/handoff/feed/session-feed.ts";
import { HELD_VIDEO_REPORT_KIND, type HeldVideoReport, type VideoCandidate } from "../../src/shared/handoff-contract.ts";
import { DEFAULT_CANDIDATE_FACTS } from "../support/video-candidate-defaults.ts";
import { flushPromises } from "./support.ts";
import { MP4, setup, startHandoff, TAB, type World } from "./handoff-world.ts";

/** Адреса YouTube: ролик Shorts номер `n` и главная. */
const shortAt = (n: number): string => `https://www.youtube.com/shorts/Sh${n}aBcDeFgHiJ`;
const YOUTUBE_HOME = "https://www.youtube.com/";
/** Стенд: страница открыта ВНЕ ленты, первый шаг — `pushState('/shorts/Sh2…')` (форма `moved`, feed-edge-shapes.js). */
const BENCH_PAGE = "http://127.0.0.1:41234/pages/feed-edges.html?run=r-1&mode=handoff&shape=moved";
const benchShortAt = (n: number): string => `http://127.0.0.1:41234/shorts/Sh${n}aBcDeFgHiJ`;
const BENCH_HOME = "http://127.0.0.1:41234/";
/** Сериал с номером серии в адресе: новое поколение + смена адреса, как у Shorts, но не в списке. */
const serialAt = (episode: number): string => `https://serial.example/serial/123/${episode}`;

/** Единственный элемент ленты Shorts. */
const SHORTS_ELEMENT = 1;

function shortsVideo(overrides: Partial<VideoCandidate> = {}): VideoCandidate {
  return {
    ...DEFAULT_CANDIDATE_FACTS,
    document: "doc-shorts",
    element: SHORTS_ELEMENT,
    media: { kind: "media_source", in_worker: false },
    playing: true,
    visible_area_px: 180_000,
    duration_s: 30,
    ...overrides,
  };
}

/** Тихое превью главной на ДРУГОМ элементе (S4: через 1,9 с после ухода). */
const HOME_PREVIEW: VideoCandidate = shortsVideo({ element: 70, muted: true, duration_s: 8 });
/** MediaSource превью главной на ТОМ ЖЕ элементе Shorts (новая форма ухода стенда). */
const HOME_PREVIEW_MEDIA_SOURCE = 900;

const report = (candidate: VideoCandidate): HeldVideoReport => ({ kind: HELD_VIDEO_REPORT_KIND, element: candidate.element, candidates: [candidate], trigger: "next_video", start_data: "available" });

/** Страница дала новый MediaSource на элементе Shorts (`ms-<номер>`, номер — по форме ключа страницы): SourceBuffer и кусок. */
function newMediaSource(world: World, key: number): void {
  const port = world.framePorts.last();
  const mediaSource = `ms-${key}`;
  port.deliver({ kind: "mse", event: { kind: "source_buffer_added", media_source: mediaSource, source_buffer: 0, mime_type: MP4 } });
  port.deliver({ kind: "bytes", media_source: mediaSource, source_buffer: 0, bytes: new Uint8Array([7, 7, 7]) });
}

/** Время идёт, тень жива: пульс раз в секунду (иначе сторож тишины честно кончил бы сеанс, решение 180). */
function liveFor(world: World, ms: number): void {
  for (let elapsed = 0; elapsed < ms; elapsed += 1_000) {
    world.framePorts.last().deliver({ kind: "shadow_alive" });
    world.clock.advance(Math.min(1_000, ms - elapsed));
  }
}

/** Порядок новостей на шаге: адрес раньше поколения или поколение раньше адреса (новость браузера идёт своим путём). */
type StepOrder = "address_first" | "generation_first";

/** Шаг Shorts: новый адрес ролика `n` и новое поколение того же элемента, в заданном порядке. */
function shortsStep(world: World, url: string, n: number, order: StepOrder = "address_first"): void {
  if (order === "address_first") world.updateTab({ url });
  newMediaSource(world, n * 10);
  if (order === "generation_first") world.updateTab({ url });
}

/** Сеанс Shorts: ролик 1 передан на `openedAt`, его поток пошёл. */
async function shortsSession(openedAt: string): Promise<World> {
  const world = setup([{ frameId: 0, result: [shortsVideo()] }]);
  world.navigateTab(openedAt);
  await startHandoff(world);
  newMediaSource(world, 10);
  return world;
}

function generationsOnWire(world: World): number[] {
  return world.sockets
    .last()
    .sentMessages()
    .flatMap((message) => (message.type === "generation_started" ? [message.generation] : []));
}

/** Что ушло плееру: виды сообщений по порядку (`page_playback` — с видом просьбы). */
function wireTypes(world: World): string[] {
  return world.sockets
    .last()
    .sentMessages()
    .map((message) => (message.type === "page_playback" ? `page_playback:${message.request.kind}` : message.type));
}

function shadowLifted(world: World): boolean {
  return world.framePorts.last().posted.some((command) => command.kind === "end_handoff");
}

/** Конец уходом из ленты (решение 370): окно на паузе, затем «Source tab closed», тень снята, сокет закрыт. */
function assertEndedLeavingFeed(world: World): void {
  assert.deepEqual(wireTypes(world).slice(-2), ["page_playback:pause", "tab_gone"]);
  assert.deepEqual(world.ended, [{ kind: "page_gone", reason: "navigated_away" }]);
  assert.ok(shadowLifted(world), "тень снята: страница снова владеет элементом");
  assert.notEqual(world.sockets.last().closedWithCode, undefined, "сокет плеера закрыт");
}

describe("381: уход из Shorts — по адресу, окно на паузе через отсрочку", () => {
  for (const [name, openedAt, step, home] of [
    ["YouTube, открыт прямо в Shorts", shortAt(1), shortAt(2), YOUTUBE_HOME],
    ["стенд, открыт вне ленты, вошёл первым шагом", BENCH_PAGE, benchShortAt(2), BENCH_HOME],
  ] as const) {
    for (const order of ["address_first", "generation_first"] as const) {
      it(`${name} (шаг: ${order}): главная — пауза и tab_gone через ${FEED_SECTION_LEAVE_GRACE_MS} мс, без element_released`, async () => {
        const world = await shortsSession(openedAt);
        shortsStep(world, step, 2, order);
        assert.deepEqual(generationsOnWire(world), [1, 2], "шаг Shorts — новое поколение в том же окне");
        world.updateTab({ url: home });
        liveFor(world, FEED_SECTION_LEAVE_GRACE_MS - 1);
        assert.deepEqual(world.ended, [], "сразу не кончаем: отсрочка");
        assert.equal(wireTypes(world).includes("tab_gone"), false);
        world.clock.advance(1);
        // `handoff_ended{element_released}` (5 с тени после снятого источника) не приходил вовсе.
        assertEndedLeavingFeed(world);
      });
    }
  }

  it("страница уже поставила ролик на паузу — второй паузы плееру нет", async () => {
    const world = await shortsSession(shortAt(1));
    world.updateTab({ url: YOUTUBE_HOME });
    world.framePorts.last().deliver({ kind: "page_playback", request: { kind: "pause" } });
    world.clock.advance(FEED_SECTION_LEAVE_GRACE_MS);
    assert.deepEqual(wireTypes(world).filter((type) => type === "page_playback:pause").length, 1);
    assert.deepEqual(world.ended, [{ kind: "page_gone", reason: "navigated_away" }]);
  });

  it("сайт на миг переписал адрес и вернул ленту — сеанс жив, шаги дальше идут", async () => {
    const world = await shortsSession(shortAt(1));
    world.updateTab({ url: YOUTUBE_HOME });
    world.clock.advance(FEED_SECTION_LEAVE_GRACE_MS / 2);
    world.updateTab({ url: shortAt(1) });
    liveFor(world, FEED_SECTION_LEAVE_GRACE_MS * 5);
    assert.deepEqual(world.ended, []);
    shortsStep(world, shortAt(2), 2);
    assert.deepEqual(generationsOnWire(world), [1, 2]);
  });
});

describe("381: превью главной окно не берёт", () => {
  it("новый MediaSource на ТОМ ЖЕ элементе внутри отсрочки — конец сразу, поколение превью плееру не ушло", async () => {
    const world = await shortsSession(BENCH_PAGE);
    shortsStep(world, benchShortAt(2), 2);
    world.updateTab({ url: BENCH_HOME });
    world.clock.advance(400);
    newMediaSource(world, HOME_PREVIEW_MEDIA_SOURCE);
    assert.deepEqual(generationsOnWire(world), [1, 2], "превью не стало поколением окна");
    assertEndedLeavingFeed(world);
  });

  it("новый MediaSource на том же элементе ПОСЛЕ отсрочки — сеанс уже кончен, плееру ничего не уходит", async () => {
    const world = await shortsSession(shortAt(1));
    world.updateTab({ url: YOUTUBE_HOME });
    world.clock.advance(FEED_SECTION_LEAVE_GRACE_MS);
    const sentBefore = world.sockets.last().sentMessages().length;
    newMediaSource(world, HOME_PREVIEW_MEDIA_SOURCE);
    assert.equal(world.sockets.last().sentMessages().length, sentBefore);
    assert.deepEqual(world.ended, [{ kind: "page_gone", reason: "navigated_away" }]);
  });

  it("превью на другом элементе, задержанное внутри отсрочки, — отпущено, переезда нет", async () => {
    const world = await shortsSession(shortAt(1));
    world.updateTab({ url: YOUTUBE_HOME });
    world.clock.advance(300);
    assert.equal(world.handoffs.heldVideo(TAB, 0, report(HOME_PREVIEW)), "release");
    world.clock.advance(FEED_SECTION_LEAVE_GRACE_MS);
    assertEndedLeavingFeed(world);
  });
});

describe("382: страница убрала ролик Shorts, адрес в ленте — окно ждёт следующий без предела", () => {
  for (const [name, removal] of [
    ["снят источник (элемент на месте)", "source_dropped"],
    ["элемент вынут из документа", "element_removed"],
  ] as const) {
    it(`стенд, после шага: ${name} — не 4009 и не «Source tab closed»; следующий ролик Shorts уходит в окно`, async () => {
      const world = await shortsSession(BENCH_PAGE);
      shortsStep(world, benchShortAt(2), 2);
      const port = world.framePorts.last();
      if (removal === "element_removed") port.deliver({ kind: "video_left_screen", exit: "removed_from_document" });
      port.deliver({ kind: "handoff_ended", cause: "element_released" });
      world.clock.advance(5 * 60_000);
      assert.deepEqual(world.ended, [], "лента продолжается: ожидание без предела");
      assert.equal(world.sockets.last().closedWithCode, undefined);
      assert.equal(wireTypes(world).includes("tab_gone"), false);
      assert.equal(world.handoffs.heldVideo(TAB, 0, report(shortsVideo({ element: 2 }))), "adopt", "следующий ролик ленты — в окно");
    });
  }

  it("открыт прямо в Shorts, шагов ещё не было — то же: ожидание, не 4009", async () => {
    const world = await shortsSession(shortAt(1));
    world.framePorts.last().deliver({ kind: "handoff_ended", cause: "element_released" });
    world.clock.advance(5 * 60_000);
    assert.deepEqual(world.ended, []);
  });

  it("пока ждём, вкладка ушла на главную — честный конец через отсрочку (окно на паузе)", async () => {
    const world = await shortsSession(shortAt(1));
    world.framePorts.last().deliver({ kind: "handoff_ended", cause: "element_released" });
    world.clock.advance(30_000);
    world.updateTab({ url: YOUTUBE_HOME });
    assert.equal(world.handoffs.heldVideo(TAB, 0, report(HOME_PREVIEW)), "release");
    world.clock.advance(FEED_SECTION_LEAVE_GRACE_MS);
    assert.deepEqual(world.ended, [{ kind: "page_gone", reason: "navigated_away" }]);
    assert.equal(wireTypes(world).at(-1), "tab_gone");
  });
});

describe("шаги Shorts внутри /shorts/ сеанс не кончают", () => {
  it("пять шагов в обоих порядках новостей, по два MediaSource на шаг — поколения подряд, сеанс жив", async () => {
    const world = await shortsSession(BENCH_PAGE);
    for (let n = 2; n <= 6; n += 1) {
      const order: StepOrder = n % 2 === 0 ? "address_first" : "generation_first";
      newMediaSource(world, n * 10 + 1); // S1: сперва MediaSource без запуска
      shortsStep(world, benchShortAt(n), n, order);
      liveFor(world, FEED_SECTION_LEAVE_GRACE_MS * 2);
    }
    assert.deepEqual(world.ended, []);
    assert.equal(world.sockets.last().closedWithCode, undefined);
    assert.deepEqual(generationsOnWire(world), [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]);
  });
});

describe("сериал со сменой серии в адресе лентой не стал (16a, 306)", () => {
  it("новое поколение + смена адреса, затем уход на другую страницу — отсрочки нет; потеря — прежний уход 306, без паузы", async () => {
    const world = await shortsSession(serialAt(4));
    shortsStep(world, serialAt(5), 5);
    world.updateTab({ url: "https://serial.example/" });
    liveFor(world, FEED_SECTION_LEAVE_GRACE_MS * 5);
    assert.deepEqual(world.ended, [], "сериал не лента: уход по адресу (381) его не кончает");
    world.framePorts.last().deliver({ kind: "handoff_ended", cause: "element_released" });
    assert.deepEqual(world.ended, [{ kind: "page_gone", reason: "navigated_away" }], "одностраничник ушёл (решения 42, 306)");
    assert.equal(wireTypes(world).at(-1), "tab_gone");
    assert.equal(wireTypes(world).includes("page_playback:pause"), false, "306: окно доигрывает скачанное, паузы сеанса нет");
  });

  it("серия сменилась (поколение + адрес), страница забрала видео без ухода — 4009, а не ожидание ленты", async () => {
    const world = await shortsSession(serialAt(4));
    shortsStep(world, serialAt(5), 5);
    world.framePorts.last().deliver({ kind: "handoff_ended", cause: "element_released" });
    assert.deepEqual(world.ended, [{ kind: "page_took_video_back" }]);
  });
});

describe("«Назад» в истории после ухода из Shorts (решение 375)", () => {
  for (const [name, openedAt, step, home, back] of [
    ["стенд: открыт вне ленты", BENCH_PAGE, benchShortAt(2), BENCH_HOME, benchShortAt(2)],
    ["YouTube", shortAt(1), shortAt(2), YOUTUBE_HOME, shortAt(2)],
  ] as const) {
    it(`${name}: вкладка вернулась в Shorts, ролик снова играет — новый сеанс в то же окно`, async () => {
      const world = await shortsSession(openedAt);
      shortsStep(world, step, 2);
      world.updateTab({ url: home });
      world.clock.advance(FEED_SECTION_LEAVE_GRACE_MS);
      assert.deepEqual(world.ended, [{ kind: "page_gone", reason: "navigated_away" }]);
      await flushPromises();
      assert.equal(world.handoffs.heldVideo(TAB, 0, report(HOME_PREVIEW)), "release", "на главной превью не подхватывается");
      world.updateTab({ url: back });
      assert.equal(world.handoffs.heldVideo(TAB, 0, report(shortsVideo())), "adopt_new_session", "раздел ленты — Shorts, а не адрес открытия");
    });
  }
});
