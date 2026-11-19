// Прокрутка ленты назад на фоне целиком (сессия 16b3b, решение 372).
//
// Наблюдение живых лент 16b3: Instagram, если элемент ещё жив (назад и сразу вперёд), возвращает тот же
// `<video>` с тем же MediaSource и продолжает с той секунды, где поставил его на паузу. Отсюда два правила:
// - при шаге ленты прежний ролик остаётся странице на своей секунде, на паузе (реклама — по-прежнему на 0);
// - возврат окна к уже объявленному MediaSource — новое поколение, а не «повтор объявления» (до 16b3b сеанс
//   закрывался 4003 `source_buffer_announced_twice`).
// Проверяется то, что видят плеер (провод: поколения, коды закрытия) и страница (команды тени).
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { FEED_SECTION_LEARN_MS } from "../../src/background/handoff/feed/session-feed.ts";
import { HELD_VIDEO_REPORT_KIND, type HeldVideoReport, type PageCommand, type VideoCandidate } from "../../src/shared/handoff-contract.ts";
import { DEFAULT_CANDIDATE_FACTS } from "../support/video-candidate-defaults.ts";
import { MP4, setup, startHandoff, TAB, type World } from "./handoff-world.ts";

const REELS = "https://www.instagram.com/reels/C7xYz1AbCdE/";

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

const report = (candidate: VideoCandidate): HeldVideoReport => ({ kind: HELD_VIDEO_REPORT_KIND, element: candidate.element, candidates: [candidate], trigger: "next_video" });

type BeginCommand = Extract<PageCommand, { kind: "begin_handoff" }>;
type EndCommand = Extract<PageCommand, { kind: "end_handoff" }>;

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

function lastBeginFor(world: World, element: number): BeginCommand {
  const begin = posted(world).findLast((command): command is BeginCommand => command.kind === "begin_handoff" && command.element === element);
  assert.ok(begin !== undefined, `begin_handoff для ролика ${element} не отправлен`);
  return begin;
}

/** Команда, которой сеанс снял тень прежнего ролика перед `begin_handoff` ролика `element`. */
function leaveBefore(world: World, element: number): EndCommand {
  const commands = posted(world);
  const beginIndex = commands.lastIndexOf(lastBeginFor(world, element));
  const leave = commands[beginIndex - 1];
  assert.ok(leave?.kind === "end_handoff", `перед begin_handoff ролика ${element} нет end_handoff: ${JSON.stringify(leave)}`);
  return leave;
}

/** Страница: тень встала на ролик `element` (ответ на его последний `begin_handoff`). */
function shadowStood(world: World, element: number): void {
  world.framePorts.last().deliver({ kind: "handoff_began", request_id: lastBeginFor(world, element).request_id, outcome: "started" });
}

/**
 * Поток ролика: его MediaSource (`ms-<element>`), SourceBuffer и один кусок. Повторный вызов для того же ролика —
 * то, что шлёт страница при новой передаче живого элемента: журнал повторяется с теми же локальными ключами.
 */
function streamOf(world: World, element: number): void {
  const port = world.framePorts.last();
  const mediaSource = `ms-${element}`;
  port.deliver({ kind: "mse", event: { kind: "source_buffer_added", media_source: mediaSource, source_buffer: 0, mime_type: MP4 } });
  port.deliver({ kind: "bytes", media_source: mediaSource, source_buffer: 0, bytes: new Uint8Array([element, element, element]) });
}

/** Страница поставила наш ролик на паузу, тень донесла уход с экрана прокруткой. */
function scrolledAway(world: World): void {
  const port = world.framePorts.last();
  port.deliver({ kind: "page_playback", request: { kind: "pause" } });
  port.deliver({ kind: "video_left_screen", exit: "scrolled_off_screen" });
}

/** Шаг ленты: ушли с экрана, ролик `element` принят, его тень встала, его поток пошёл. */
function feedStepTo(world: World, element: number): void {
  scrolledAway(world);
  assert.equal(world.handoffs.heldVideo(TAB, 0, report(reel(element))), "adopt");
  shadowStood(world, element);
  streamOf(world, element);
  liveFor(world, FEED_SECTION_LEARN_MS + 1);
}

/** Время идёт, тень жива: пульс раз в секунду (иначе сторож тишины честно кончил бы сеанс, решение 180). */
function liveFor(world: World, ms: number): void {
  for (let elapsed = 0; elapsed < ms; elapsed += 1_000) {
    world.framePorts.last().deliver({ kind: "shadow_alive" });
    world.clock.advance(Math.min(1_000, ms - elapsed));
  }
}

/** Номера поколений и SourceBuffer, объявленные плееру по порядку. */
function announcedOnWire(world: World): string[] {
  return world.sockets
    .last()
    .sentMessages()
    .flatMap((message) => {
      if (message.type === "generation_started") return [`generation ${message.generation}`];
      if (message.type === "source_buffer_added") return [`buffer ${message.generation}:${message.source_buffer}`];
      return [];
    });
}

/** Рамки байтов, ушедшие плееру, в краткой записи провода: «frame поколение/SourceBuffer#номер». */
function framesOnWire(world: World): string[] {
  return world.sockets
    .last()
    .wireSummary()
    .filter((line) => line.startsWith("frame"));
}

/** Ролик сеанса в окне, как его видит попап. */
async function videoInPlayer(world: World): Promise<number | null> {
  const survey = await world.handoffs.surveyTab(TAB);
  assert.equal(survey.kind, "surveyed");
  const presence = survey.kind === "surveyed" ? survey.in_player : { kind: "no_session" as const };
  return presence.kind === "in_player" ? (presence.video?.element ?? null) : null;
}

const AT_ITS_SECOND_PAUSED: EndCommand = { kind: "end_handoff", position: { kind: "shadow_position" }, resume: { kind: "exactly", resume: "paused" } };
const AT_START_PAUSED: EndCommand = { kind: "end_handoff", position: { kind: "at", position_us: 0 }, resume: { kind: "exactly", resume: "paused" } };

describe("назад к живому ролику: тот же MediaSource — новое поколение", () => {
  it("ролик 1 → ролик 2 → снова ролик 1 тем же MediaSource: окно не закрыто, поколения 1, 2, 3, в окне ролик 1", async () => {
    const world = await feedSession();
    feedStepTo(world, 2);
    // Прокрутка назад: Instagram возвращает тот же элемент, страница повторяет его журнал (те же ключи `ms-1`).
    feedStepTo(world, 1);
    assert.equal(world.sockets.last().closedWithCode, undefined, "сеанс не закрыт как нарушение протокола");
    assert.deepEqual(world.ended, []);
    assert.deepEqual(announcedOnWire(world), ["generation 1", "buffer 1:0", "generation 2", "buffer 2:0", "generation 3", "buffer 3:0"]);
    assert.deepEqual(framesOnWire(world), ["frame 1/0#0", "frame 2/0#0", "frame 3/0#0"], "у нового поколения рамки с нуля");
    assert.equal(await videoInPlayer(world), 1);
    assert.equal(world.door.requests.length, 1, "окно одно");
  });

  it("вперёд-назад несколько раз: каждый возврат — своё поколение, номера не повторяются", async () => {
    const world = await feedSession();
    for (const element of [2, 1, 2, 1]) feedStepTo(world, element);
    assert.equal(world.sockets.last().closedWithCode, undefined);
    const generations = announcedOnWire(world).filter((line) => line.startsWith("generation"));
    assert.deepEqual(generations, ["generation 1", "generation 2", "generation 3", "generation 4", "generation 5"]);
  });

  it("новый MediaSource того же переданного элемента без переезда — по-прежнему поколение того же видео", async () => {
    // Смена качества или Shorts: тень та же, `handoff_began` нет — забывать ключи нельзя и не нужно.
    const world = await feedSession();
    const port = world.framePorts.last();
    port.deliver({ kind: "mse", event: { kind: "source_buffer_added", media_source: "ms-9", source_buffer: 0, mime_type: MP4 } });
    port.deliver({ kind: "bytes", media_source: "ms-1", source_buffer: 0, bytes: new Uint8Array([1]) });
    assert.equal(world.sockets.last().closedWithCode, undefined, "старый MediaSource того же видео ещё принимается");
    assert.deepEqual(announcedOnWire(world), ["generation 1", "buffer 1:0", "generation 2", "buffer 2:0"]);
    assert.deepEqual(framesOnWire(world), ["frame 1/0#0", "frame 1/0#1"]);
  });

  it("повтор объявления внутри одного видео (без переезда) — по-прежнему нарушение протокола", async () => {
    const world = await feedSession();
    streamOf(world, 1);
    assert.notEqual(world.sockets.last().closedWithCode, undefined, "страница, объявившая тот же SourceBuffer дважды, — враг");
  });
});

describe("шаг ленты: прежний ролик остаётся странице на своей секунде", () => {
  it("ролик ушёл прокруткой — тень снимается на позиции тени, на паузе, раньше begin_handoff следующего", async () => {
    const world = await feedSession();
    feedStepTo(world, 2);
    assert.deepEqual(leaveBefore(world, 2), AT_ITS_SECOND_PAUSED);
  });

  it("ролик доиграл до конца, потом лента листнулась — на начало: второго `ended` страница не получит", async () => {
    const world = await feedSession();
    feedStepTo(world, 2);
    world.sockets.last().receive({ type: "clock", seq: 1, position_us: 12_000_000, state: "ended", rate: 1000 });
    scrolledAway(world);
    assert.equal(world.handoffs.heldVideo(TAB, 0, report(reel(3))), "adopt");
    assert.deepEqual(leaveBefore(world, 3), AT_START_PAUSED);
  });
});
