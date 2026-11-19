// Края ленты на фоне целиком (сессия 16b3, правила F1–F5): быстрое листание, удаление ролика сеанса-ленты, уход
// из ленты в другой раздел. Формы — из наблюдения живых лент 19 ноября 2026 (Instagram, TikTok, Shorts).
// Проверяется то, что видят плеер (провод: одно окно, поколения подряд, `tab_gone`, коды закрытия) и страница
// (команды тени), и какой ролик у сеанса в окне.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { FEED_EXIT_WINDOW_MS, FEED_SECTION_LEARN_MS, FEED_SECTION_LEAVE_GRACE_MS } from "../../src/background/handoff/feed/session-feed.ts";
import { NEXT_VIDEO_WAIT_MS } from "../../src/background/handoff/video-relocation.ts";
import { NEXT_EPISODE_WAIT_MS } from "../../src/shared/next-episode-wait.ts";
import { SESSION_CLOSE_CODES } from "../../src/protocol/index.ts";
import {
  HELD_VIDEO_REPORT_KIND,
  type HeldVideoReport,
  type PageCommand,
  type ScreenExitKind,
  type VideoCandidate,
} from "../../src/shared/handoff-contract.ts";
import { DEFAULT_CANDIDATE_FACTS } from "../support/video-candidate-defaults.ts";
import { MP4, setup, startHandoff, TAB, type World } from "./handoff-world.ts";

const REELS = "https://www.instagram.com/reels/C7xYz1AbCdE/";
const NEXT_REEL_ADDRESS = "https://www.instagram.com/reels/DBq-9_kLmNo/";
const EXPLORE = "https://www.instagram.com/explore/";

/** Площадь ролика ленты на экране. */
const REEL_AREA = 164_157;

/** Ролик ленты номер `element`: MSE, короткий, видимый, играет. */
function reel(element: number, overrides: Partial<VideoCandidate> = {}): VideoCandidate {
  return {
    ...DEFAULT_CANDIDATE_FACTS,
    document: "doc-feed",
    element,
    media: { kind: "media_source", in_worker: false },
    playing: true,
    visible_area_px: REEL_AREA,
    duration_s: 10 + element,
    ...overrides,
  };
}

/** Тихое превью нового раздела (Instagram «Интересное», TikTok «Смотреть»): виден, играет, без звука. */
const SILENT_PREVIEW: VideoCandidate = reel(50, { media: { kind: "file" }, file_host: "www.instagram.com", muted: true, duration_s: 8 });

const report = (candidate: VideoCandidate): HeldVideoReport => ({ kind: HELD_VIDEO_REPORT_KIND, element: candidate.element, candidates: [candidate], trigger: "next_video", start_data: "available" });

type BeginCommand = Extract<PageCommand, { kind: "begin_handoff" }>;

/** Лента Instagram: ролик 1 передан, вкладка на `/reels/<id>/`. */
async function feedSession(): Promise<World> {
  const world = setup([{ frameId: 0, result: [reel(1), reel(2, { playing: false, visible_area_px: 0 })] }]);
  world.navigateTab(REELS);
  await startHandoff(world);
  streamOf(world, 1);
  return world;
}

function posted(world: World): PageCommand[] {
  return world.framePorts.last().posted;
}

function begins(world: World): BeginCommand[] {
  return posted(world).filter((command): command is BeginCommand => command.kind === "begin_handoff");
}

function beginFor(world: World, element: number): BeginCommand {
  const begin = begins(world).findLast((command) => command.element === element);
  assert.ok(begin !== undefined, `begin_handoff для ролика ${element} не отправлен`);
  return begin;
}

/** Страница: тень встала на ролик `element` (ответ на его `begin_handoff`). */
function shadowStood(world: World, element: number): void {
  world.framePorts.last().deliver({ kind: "handoff_began", request_id: beginFor(world, element).request_id, outcome: "started" });
}

/** Поток ролика: его MediaSource (`ms-<element>`), SourceBuffer и один кусок. */
function streamOf(world: World, element: number): void {
  const port = world.framePorts.last();
  const mediaSource = `ms-${element}`;
  port.deliver({ kind: "mse", event: { kind: "source_buffer_added", media_source: mediaSource, source_buffer: 0, mime_type: MP4 } });
  port.deliver({ kind: "bytes", media_source: mediaSource, source_buffer: 0, bytes: new Uint8Array([element, element, element]) });
}

/** Страница поставила наш ролик на паузу, тень донесла уход с экрана. */
function leftScreen(world: World, exit: ScreenExitKind = "scrolled_off_screen"): void {
  const port = world.framePorts.last();
  port.deliver({ kind: "page_playback", request: { kind: "pause" } });
  port.deliver({ kind: "video_left_screen", exit });
}

/** Шаг ленты целиком: ушли с экрана, следующий ролик принят, его тень встала, его поток пошёл. */
function feedStep(world: World, element: number): void {
  leftScreen(world);
  assert.equal(world.handoffs.heldVideo(TAB, 0, report(reel(element))), "adopt");
  shadowStood(world, element);
  streamOf(world, element);
  // Раздел ленты выучен на этом шаге; окно уточнения адресом шага прошло.
  liveFor(world, FEED_SECTION_LEARN_MS + 1);
}

/** Время идёт, тень жива: пульс раз в секунду (иначе сторож тишины честно кончил бы сеанс, решение 180). */
function liveFor(world: World, ms: number): void {
  for (let elapsed = 0; elapsed < ms; elapsed += 1_000) {
    world.framePorts.last().deliver({ kind: "shadow_alive" });
    world.clock.advance(Math.min(1_000, ms - elapsed));
  }
}

function generationsOnWire(world: World): number[] {
  return world.sockets
    .last()
    .sentMessages()
    .flatMap((message) => (message.type === "generation_started" ? [message.generation] : []));
}

function tabGoneReasons(world: World): string[] {
  return world.sockets
    .last()
    .sentMessages()
    .flatMap((message) => (message.type === "tab_gone" ? [message.reason] : []));
}

/** Ролик сеанса в окне, как его видит попап (опрос вкладки): номер элемента или `null` между роликами. */
async function videoInPlayer(world: World): Promise<number | null> {
  const survey = await world.handoffs.surveyTab(TAB);
  assert.equal(survey.kind, "surveyed");
  const presence = survey.kind === "surveyed" ? survey.in_player : { kind: "no_session" as const };
  return presence.kind === "in_player" ? (presence.video?.element ?? null) : null;
}

describe("быстрое листание: ролик, задержанный посреди переезда (F1 отменено — поведение HEAD)", () => {
  it("ролик пришёл, пока тень промежуточного встаёт, — отпущен; после ухода промежуточного с экрана окно едет к нему", async () => {
    // Сайты сами ставят промежуточный ролик на паузу (I4: через 40–120 мс; пул TikTok вынимает его на следующем
    // шаге), и сторож переспрашивает о ролике, отпущенном до этого ухода (16b2). Отмена переезда на промежуточный
    // (F1) не нужна, а её пауза отвергала обещание `play()` страницы, которого страница не прерывала (стенд
    // `feed-burst-pool`).
    const world = await feedSession();
    leftScreen(world);
    assert.equal(world.handoffs.heldVideo(TAB, 0, report(reel(2))), "adopt");
    assert.equal(world.handoffs.heldVideo(TAB, 0, report(reel(3))), "release", "посреди переезда цель не меняется");
    assert.equal(posted(world).filter((command) => command.kind === "begin_handoff").length, 2, "только ролики 1 и 2");
    shadowStood(world, 2);
    streamOf(world, 2);
    leftScreen(world);
    assert.equal(world.handoffs.heldVideo(TAB, 0, report(reel(3))), "adopt");
    shadowStood(world, 3);
    streamOf(world, 3);
    assert.equal(await videoInPlayer(world), 3, "в окне — последний ролик");
    assert.deepEqual(generationsOnWire(world), [1, 2, 3]);
    assert.equal(world.door.requests.length, 1, "окно одно");
    assert.deepEqual(world.ended, []);
  });
});

describe("F2: сеанс-лента потерял ролик без смены адреса — ждать следующий", () => {
  it("после шага ленты страница удалила ролик посреди переезда — не 4009, окно ждёт следующий без предела", async () => {
    const world = await feedSession();
    feedStep(world, 2);
    world.framePorts.last().deliver({ kind: "handoff_ended", cause: "element_released" });
    world.clock.advance(Math.max(NEXT_VIDEO_WAIT_MS, NEXT_EPISODE_WAIT_MS) * 10);
    assert.equal(world.sockets.last().closedWithCode, undefined);
    assert.deepEqual(world.ended, []);
    assert.equal(world.handoffs.heldVideo(TAB, 0, report(reel(3))), "adopt", "следующий ролик ленты принят");
    shadowStood(world, 3);
    assert.equal(await videoInPlayer(world), 3);
  });

  it("до первого шага ленты — прежнее «The page took the video back» (4009)", async () => {
    const world = await feedSession();
    world.framePorts.last().deliver({ kind: "handoff_ended", cause: "element_released" });
    assert.equal(world.sockets.last().closedWithCode, SESSION_CLOSE_CODES.page_took_video_back);
    assert.deepEqual(world.ended, [{ kind: "page_took_video_back" }]);
  });
});

describe("F3: уход из ленты — адрес сменился и ролик потерян, не уйдя прокруткой", () => {
  it("Instagram: адрес «Интересного», через полсекунды ролик удалён и встал на паузу вне документа — «Source tab closed»", async () => {
    const world = await feedSession();
    feedStep(world, 2);
    world.updateTab({ url: EXPLORE });
    world.clock.advance(500);
    leftScreen(world, "removed_from_document");
    assert.equal(world.handoffs.heldVideo(TAB, 0, report(SILENT_PREVIEW)), "release", "тихое превью нового раздела окно не уводит");
    world.clock.advance(1_000);
    world.framePorts.last().deliver({ kind: "handoff_ended", cause: "element_released" });
    assert.deepEqual(tabGoneReasons(world), ["navigated_away"]);
    assert.deepEqual(world.ended, [{ kind: "page_gone", reason: "navigated_away" }]);
  });

  it("порядок «удаление → адрес»: ролик удалён, адрес пришёл в окне — тот же честный конец", async () => {
    const world = await feedSession();
    feedStep(world, 2);
    world.framePorts.last().deliver({ kind: "handoff_ended", cause: "element_released" });
    assert.deepEqual(world.ended, [], "пока адрес не сменился — лента продолжается (F2)");
    world.clock.advance(FEED_EXIT_WINDOW_MS - 100);
    world.updateTab({ url: EXPLORE });
    assert.deepEqual(tabGoneReasons(world), ["navigated_away"], "сразу, без отсрочки F5");
    assert.deepEqual(world.ended, [{ kind: "page_gone", reason: "navigated_away" }]);
  });

  it("правило окна само по себе: страница не звала pause(), уход с экрана не донесён — конец сразу на потере", async () => {
    // Пауза удалённого элемента — внутренняя пауза браузера, а не вызов страницы: тень её не видит, ожидания ленты
    // нет, и отсрочка F5 не идёт. Решает только окно F3.
    const world = await feedSession();
    feedStep(world, 2);
    world.clock.advance(5_000);
    world.updateTab({ url: EXPLORE });
    world.clock.advance(1_500);
    world.framePorts.last().deliver({ kind: "handoff_ended", cause: "element_released" });
    assert.deepEqual(tabGoneReasons(world), ["navigated_away"], "сразу на потере, без ожидания");
    // Решение 370: страница паузы не ставила — окно ставит на паузу сам сеанс, перед `tab_gone`.
    const lastTwo = world.sockets.last().sentMessages().slice(-2);
    assert.deepEqual(lastTwo.map((message) => message.type), ["page_playback", "tab_gone"]);
    assert.ok(lastTwo[0]?.type === "page_playback" && lastTwo[0].request.kind === "pause");
  });

  it("форма TikTok (T5): адрес другого раздела, через 6 мс ролик вынут и встал на паузу, потеря раньше отсрочки — конец сразу на потере", async () => {
    const world = await feedSession();
    feedStep(world, 2);
    world.updateTab({ url: EXPLORE });
    world.clock.advance(6);
    leftScreen(world, "removed_from_document");
    world.clock.advance(FEED_SECTION_LEAVE_GRACE_MS / 2);
    world.framePorts.last().deliver({ kind: "handoff_ended", cause: "element_released" });
    assert.deepEqual(tabGoneReasons(world), ["navigated_away"], "не ждём ни ролика, ни отсрочки");
  });

  for (const exit of ["не донесён", "донесён после удаления"] as const) {
    it(`быстрая серия Instagram: адрес ушёл на следующий ролик, промежуточный удалён (уход ${exit}) — ждём, не конец`, async () => {
      const world = await feedSession();
      feedStep(world, 2);
      world.clock.advance(80);
      world.updateTab({ url: NEXT_REEL_ADDRESS });
      if (exit === "донесён после удаления") leftScreen(world, "removed_from_document");
      world.clock.advance(1_200);
      world.framePorts.last().deliver({ kind: "handoff_ended", cause: "element_released" });
      assert.deepEqual(world.ended, [], "промежуточный ролик пропал — лента продолжается (F2), это не уход");
      assert.equal(world.handoffs.heldVideo(TAB, 0, report(reel(3))), "adopt");
      shadowStood(world, 3);
      assert.equal(await videoInPlayer(world), 3);
      world.clock.advance(FEED_SECTION_LEAVE_GRACE_MS * 5);
      assert.deepEqual(world.ended, []);
    });
  }

  it("шаг Instagram: ролик ушёл прокруткой, адрес сменился на следующий ролик, старый удалён — это не уход", async () => {
    const world = await feedSession();
    feedStep(world, 2);
    leftScreen(world);
    world.updateTab({ url: NEXT_REEL_ADDRESS });
    world.framePorts.last().deliver({ kind: "handoff_ended", cause: "element_released" });
    world.clock.advance(FEED_SECTION_LEAVE_GRACE_MS * 10);
    assert.deepEqual(world.ended, []);
    assert.equal(world.handoffs.heldVideo(TAB, 0, report(reel(3))), "adopt");
  });

  it("Shorts без переездов: адрес главной, источник снят, тень сдалась через 5 с — прежний уход по адресу (S4)", async () => {
    const world = setup([{ frameId: 0, result: [reel(1)] }]);
    world.navigateTab("https://www.youtube.com/shorts/dQw4w9WgXcQ");
    await startHandoff(world);
    streamOf(world, 1);
    world.updateTab({ url: "https://www.youtube.com/" });
    world.clock.advance(6_450);
    world.framePorts.last().deliver({ kind: "handoff_ended", cause: "element_released" });
    assert.deepEqual(tabGoneReasons(world), ["navigated_away"]);
  });
});

describe("F5: пока ждём ролик ленты, вкладка ушла в другой раздел", () => {
  it("ожидание ленты, адрес «Интересного» — честный конец после отсрочки; превью в это время не принимается", async () => {
    const world = await feedSession();
    feedStep(world, 2);
    leftScreen(world);
    world.framePorts.last().deliver({ kind: "handoff_ended", cause: "element_released" });
    world.clock.advance(FEED_EXIT_WINDOW_MS * 2);
    world.updateTab({ url: EXPLORE });
    assert.equal(world.handoffs.heldVideo(TAB, 0, report(SILENT_PREVIEW)), "release");
    assert.deepEqual(world.ended, [], "сразу не кончаем: отсрочка");
    world.clock.advance(FEED_SECTION_LEAVE_GRACE_MS);
    assert.deepEqual(tabGoneReasons(world), ["navigated_away"]);
    assert.deepEqual(world.ended, [{ kind: "page_gone", reason: "navigated_away" }]);
  });

  it("тень ещё стоит на пролистанном ролике (`active`) — тень снимается, конец тот же", async () => {
    const world = await feedSession();
    feedStep(world, 2);
    leftScreen(world);
    world.updateTab({ url: EXPLORE });
    world.clock.advance(FEED_SECTION_LEAVE_GRACE_MS);
    assert.ok(posted(world).some((command) => command.kind === "end_handoff"), "тень снята");
    assert.deepEqual(world.ended, [{ kind: "page_gone", reason: "navigated_away" }]);
  });

  it("адрес того же раздела (следующий ролик) — окно ждёт дальше", async () => {
    const world = await feedSession();
    leftScreen(world);
    world.updateTab({ url: NEXT_REEL_ADDRESS });
    liveFor(world, FEED_SECTION_LEAVE_GRACE_MS * 5);
    assert.deepEqual(world.ended, []);
    assert.equal(world.handoffs.heldVideo(TAB, 0, report(reel(2))), "adopt");
  });

  it("ролик не листали (он на экране и играет) — смена раздела этим правилом не судится", async () => {
    const world = await feedSession();
    world.updateTab({ url: EXPLORE });
    liveFor(world, FEED_SECTION_LEAVE_GRACE_MS * 5);
    assert.deepEqual(world.ended, []);
    assert.equal(world.sockets.last().closedWithCode, undefined);
  });
});

describe("раздел ленты выучивается на шаге ленты (неверный конец хуже ожидания)", () => {
  /** Страница стенда: открыта не на адресе ленты, первый шаг — `pushState('/reels/2/')`. */
  const BENCH_PAGE = "http://127.0.0.1:8080/pages/feed.html?shape=many";
  const benchReel = (number: number): string => `http://127.0.0.1:8080/reels/${number}/`;

  async function benchSession(): Promise<World> {
    const world = setup([{ frameId: 0, result: [reel(1), reel(2, { playing: false, visible_area_px: 0 })] }]);
    world.navigateTab(BENCH_PAGE);
    await startHandoff(world);
    streamOf(world, 1);
    return world;
  }

  it("первый шаг уводит адрес со страницы на /reels/2/ — переезд принят, сеанс жив, дальше лента идёт", async () => {
    const world = await benchSession();
    leftScreen(world);
    world.updateTab({ url: benchReel(2) });
    assert.equal(world.handoffs.heldVideo(TAB, 0, report(reel(2))), "adopt", "раздел ещё не выучен — не «другой»");
    shadowStood(world, 2);
    liveFor(world, FEED_SECTION_LEAVE_GRACE_MS * 3);
    assert.deepEqual(world.ended, []);
    leftScreen(world);
    world.updateTab({ url: benchReel(3) });
    liveFor(world, FEED_SECTION_LEAVE_GRACE_MS * 3);
    assert.equal(world.handoffs.heldVideo(TAB, 0, report(reel(3))), "adopt");
    assert.deepEqual(world.ended, []);
  });

  it("новость об адресе шага пришла уже после того, как тень встала, — это раздел ленты, а не уход", async () => {
    const world = await benchSession();
    leftScreen(world);
    world.handoffs.heldVideo(TAB, 0, report(reel(2)));
    shadowStood(world, 2);
    world.clock.advance(200);
    world.updateTab({ url: benchReel(2) });
    liveFor(world, 2_000);
    leftScreen(world);
    world.updateTab({ url: benchReel(3) });
    liveFor(world, FEED_SECTION_LEAVE_GRACE_MS * 3);
    assert.deepEqual(world.ended, []);
    assert.equal(world.handoffs.heldVideo(TAB, 0, report(reel(3))), "adopt");
  });

  it("до первого шага ленты уход в другой раздел сеанс не кончает (прежние правила)", async () => {
    const world = await feedSession();
    leftScreen(world);
    world.updateTab({ url: EXPLORE });
    liveFor(world, FEED_SECTION_LEAVE_GRACE_MS * 5);
    assert.deepEqual(world.ended, []);
    assert.equal(world.sockets.last().closedWithCode, undefined);
  });
});

describe("уход из ленты: окно встаёт на паузу с «Source tab closed» (решение 370)", () => {
  /** Что ушло плееру: виды сообщений по порядку (`page_playback` — с видом просьбы). */
  function wireTypes(world: World): string[] {
    return world.sockets.last().sentMessages().map((message) => (message.type === "page_playback" ? `page_playback:${message.request.kind}` : message.type));
  }

  /** Последнее слово об игре перед `tab_gone` и сам `tab_gone` в конце провода. */
  function lastPlaybackBeforeTabGone(world: World): string | undefined {
    const types = wireTypes(world);
    assert.equal(types.at(-1), "tab_gone");
    return types.filter((type) => type.startsWith("page_playback:")).at(-1);
  }

  it("Instagram: страница поставила уходящий ролик на паузу — окно на паузе, «играть» не шлётся (конец на потере, F3)", async () => {
    const world = await feedSession();
    feedStep(world, 2);
    world.updateTab({ url: EXPLORE });
    world.clock.advance(500);
    leftScreen(world, "removed_from_document");
    // Потеря раньше отсрочки F5: конец решает окно F3.
    world.clock.advance(FEED_SECTION_LEAVE_GRACE_MS / 2);
    world.framePorts.last().deliver({ kind: "handoff_ended", cause: "element_released" });
    assert.equal(lastPlaybackBeforeTabGone(world), "page_playback:pause");
    assert.equal(wireTypes(world).filter((type) => type === "page_playback:pause").length, 2, "пауза шага 1 и пауза ухода; второй своей нет");
  });

  it("окно ждало следующий ролик (сеанс сам поставил паузу) — после смены раздела пауза остаётся", async () => {
    const world = await feedSession();
    feedStep(world, 2);
    leftScreen(world);
    world.framePorts.last().deliver({ kind: "handoff_ended", cause: "element_released" });
    world.clock.advance(FEED_EXIT_WINDOW_MS * 2);
    world.updateTab({ url: EXPLORE });
    world.clock.advance(FEED_SECTION_LEAVE_GRACE_MS);
    assert.equal(lastPlaybackBeforeTabGone(world), "page_playback:pause");
  });

  it("страница паузы не ставила (окно играет) — сеанс сам ставит окно на паузу перед `tab_gone`", async () => {
    const world = await feedSession();
    feedStep(world, 2);
    world.updateTab({ url: EXPLORE });
    world.clock.advance(500);
    world.framePorts.last().deliver({ kind: "video_left_screen", exit: "removed_from_document" });
    world.clock.advance(1_000);
    world.framePorts.last().deliver({ kind: "handoff_ended", cause: "element_released" });
    const types = wireTypes(world);
    assert.deepEqual(types.slice(-2), ["page_playback:pause", "tab_gone"]);
    assert.equal(types.includes("page_playback:play"), false, "скачанное само не доигрывается");
  });
});

describe("уход до первого шага ленты: раздел не выучен, а уход сайта виден", () => {
  it("Instagram (I6): адрес «Интересного», ролик вынут и на паузе — честный конец, превью не берётся", async () => {
    const world = await feedSession();
    world.updateTab({ url: EXPLORE });
    world.clock.advance(500);
    leftScreen(world, "removed_from_document");
    assert.deepEqual(tabGoneReasons(world), ["navigated_away"]);
    assert.equal(world.handoffs.heldVideo(TAB, 0, report(SILENT_PREVIEW)), "release");
    assert.deepEqual(generationsOnWire(world), [1], "превью не стало поколением 2");
  });

  it("порядок «удаление → адрес» (T5 наоборот): ролик вынут, адрес пришёл в окне — тот же конец", async () => {
    const world = await feedSession();
    leftScreen(world, "removed_from_document");
    assert.deepEqual(world.ended, []);
    world.clock.advance(FEED_EXIT_WINDOW_MS - 100);
    world.updateTab({ url: "https://www.instagram.com/explore/" });
    assert.deepEqual(tabGoneReasons(world), ["navigated_away"]);
  });

  it("шаг TikTok до первого шага: ролик вынут, адрес тот же — не уход, окно едет к следующему", async () => {
    const world = await feedSession();
    leftScreen(world, "removed_from_document");
    world.clock.advance(FEED_EXIT_WINDOW_MS * 2);
    assert.deepEqual(world.ended, []);
    assert.equal(world.handoffs.heldVideo(TAB, 0, report(reel(2))), "adopt");
  });

  it("шаг Instagram до первого шага: уход прокруткой и новый адрес ролика — не уход", async () => {
    const world = await feedSession();
    leftScreen(world);
    world.updateTab({ url: NEXT_REEL_ADDRESS });
    assert.deepEqual(world.ended, []);
    assert.equal(world.handoffs.heldVideo(TAB, 0, report(reel(2))), "adopt");
  });
});
