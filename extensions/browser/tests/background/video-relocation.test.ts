// Переезд сеанса на следующее видео (сессия 09c2, решения 201, 204): что видит плеер на проводе и что
// получает страница, когда реклама кончилась, её пропустили или сеанс начинался не на рекламе.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { HANDOFF_BEGIN_REPLY_TIMEOUT_MS } from "../../src/background/handoff/constants.ts";
import { judgeHeldVideo } from "../../src/background/handoff/held-video-choice.ts";
import { NEXT_VIDEO_ARM_LEASE_MS, NEXT_VIDEO_ARM_REFRESH_MS, NEXT_VIDEO_WAIT_MS } from "../../src/background/handoff/video-relocation.ts";
import { SESSION_CLOSE_CODES } from "../../src/protocol/index.ts";
import { HELD_VIDEO_REPORT_KIND, SHADOW_SILENCE_LIMIT_MS, type HeldVideoReport, type PageCommand, type VideoCandidate } from "../../src/shared/handoff-contract.ts";
import { DEFAULT_CANDIDATE_FACTS } from "../support/video-candidate-defaults.ts";
import { MP4, setup, startHandoff, TAB, type World } from "./handoff-world.ts";
import { flushPromises } from "./support.ts";

const AREA = 296_000;
const WITH_NATIVE_HLS = { nativeHls: true, hlsLive: false } as const;

/** Реклама: файл с неизвестного домена, рядом ждёт фильм (форма Ashdi и Tortuga). */
const AD: VideoCandidate = {
  ...DEFAULT_CANDIDATE_FACTS,
  document: "doc-ad",
  element: 3,
  media: { kind: "file" },
  file_host: "video.unknown-ad-cdn.example",
  playing: true,
  visible_area_px: AREA,
  duration_s: 24,
};
const FILM: VideoCandidate = { ...DEFAULT_CANDIDATE_FACTS, document: "doc-ad", element: 1, media: { kind: "media_source", in_worker: false }, playing: false, visible_area_px: AREA, duration_s: 6381 };
/** Обычный фильм без рекламы: длинный MSE. */
const PLAIN_FILM: VideoCandidate = { ...FILM, playing: true };

const report = (element: number, candidates: readonly VideoCandidate[]): HeldVideoReport => ({ kind: HELD_VIDEO_REPORT_KIND, element, candidates });
/** Сторож задержал фильм: страница видит его играющим. */
const heldFilm = (overrides: Partial<VideoCandidate> = {}): HeldVideoReport => report(FILM.element, [{ ...FILM, playing: true, ...overrides }]);

function adWorld(): World {
  return setup([{ frameId: 0, result: [FILM, AD] }]);
}

function lastBegin(world: World): Extract<PageCommand, { kind: "begin_handoff" }> {
  const begin = world.framePorts.last().posted.filter((command) => command.kind === "begin_handoff").at(-1);
  assert.ok(begin?.kind === "begin_handoff", "begin_handoff не отправлен");
  return begin;
}

/** Страница убрала рекламу: тень снялась сама. */
function adElementGone(world: World): void {
  world.framePorts.last().deliver({ kind: "handoff_ended", cause: "element_released" });
}

function clock(world: World, state: "playing" | "ended" | "waiting", seq = 1): void {
  world.sockets.last().receive({ type: "clock", seq, position_us: 24_000_000, state, rate: 1000 });
}

/** Новый документ подтвердил тень после переезда. */
function confirmMove(world: World): void {
  const port = world.framePorts.last();
  port.deliver({ kind: "handoff_began", request_id: lastBegin(world).request_id, outcome: "started" });
}

describe("сеанс начинается на рекламе", () => {
  it("уходит реклама, сторож взведён всем фреймам и продлевается, признак «реклама на экране» виден", async () => {
    const world = adWorld();
    await startHandoff(world);
    assert.equal(lastBegin(world).element, AD.element);
    assert.equal(world.handoffs.isAdOnScreen(TAB), true);
    assert.deepEqual(world.armings, [{ tabId: TAB, leaseMs: NEXT_VIDEO_ARM_LEASE_MS }]);
    world.clock.advance(NEXT_VIDEO_ARM_REFRESH_MS * 3);
    assert.equal(world.armings.length, 4, "срок взвода продлён на каждом шаге");
  });

  it("у вкладки без сеанса рекламы на экране нет, а задержанное видео отпускается", () => {
    const world = adWorld();
    assert.equal(world.handoffs.isAdOnScreen(TAB), false);
    assert.equal(world.handoffs.heldVideo(TAB, 0, heldFilm()), "release");
  });

  it("обычный длинный фильм сторож не взводит: страница живёт как раньше", async () => {
    const world = setup([{ frameId: 0, result: [PLAIN_FILM] }]);
    await startHandoff(world);
    assert.deepEqual(world.armings, []);
    assert.equal(world.handoffs.isAdOnScreen(TAB), false);
  });
});

describe("реклама кончилась: фильм в том же документе", () => {
  it("страница убрала рекламу и запустила фильм: один сокет, новый begin_handoff на том же порте", async () => {
    const world = adWorld();
    await startHandoff(world);
    const firstRequest = lastBegin(world).request_id;
    adElementGone(world);
    assert.equal(world.sockets.last().closedWithCode, undefined, "сеанс не закрыт");
    assert.deepEqual(world.ended, []);
    assert.equal(world.handoffs.heldVideo(TAB, 0, heldFilm()), "adopt");
    const begin = lastBegin(world);
    assert.equal(begin.element, FILM.element);
    assert.notEqual(begin.request_id, firstRequest, "у переезда свой номер запроса");
    assert.equal(world.framePorts.ports.length, 1, "документ тот же, порт тот же");
    confirmMove(world);
    assert.equal(world.handoffs.isAdOnScreen(TAB), false, "признак рекламы снят");
    assert.equal(world.door.requests.length, 1, "окно одно: второго сеанса нет");
  });

  it("фильм идёт дальше по общей нумерации: поколение рекламы и поколение фильма разные", async () => {
    const world = adWorld();
    await startHandoff(world);
    const port = world.framePorts.last();
    port.deliver({ kind: "file_source", url: "https://video.unknown-ad-cdn.example/ad.mp4", referrer_origin: null, referrer_policy: "no-referrer" });
    adElementGone(world);
    world.handoffs.heldVideo(TAB, 0, heldFilm());
    confirmMove(world);
    port.deliver({ kind: "mse", event: { kind: "source_buffer_added", media_source: "ms-1", source_buffer: 0, mime_type: MP4 } });
    const summary = world.sockets.last().wireSummary();
    assert.deepEqual(
      summary.filter((line) => line === "generation_started" || line === "file_source_added" || line === "source_buffer_added"),
      ["generation_started", "file_source_added", "generation_started", "source_buffer_added"],
      "файл сменяется на MSE новым поколением в том же сеансе",
    );
  });

  it("плеер получает паузу, пока следующего ролика нет: хвост оборванной рекламы не звучит", async () => {
    const world = adWorld();
    await startHandoff(world);
    adElementGone(world);
    assert.ok(world.sockets.last().wireSummary().includes("page_playback"), "слово «пауза» ушло плееру");
  });

  it("между роликами тени нет: часы странице не идут, а тишина пульса сеанс не кончает", async () => {
    const world = adWorld();
    await startHandoff(world);
    adElementGone(world);
    const port = world.framePorts.last();
    const postedBefore = port.posted.length;
    clock(world, "ended");
    assert.equal(port.posted.length, postedBefore, "показывать часы некому");
    world.clock.advance(SHADOW_SILENCE_LIMIT_MS + 1);
    assert.equal(world.handoffs.heldVideo(TAB, 0, heldFilm()), "adopt", "предел ожидания (15 с) ещё не вышел, а предел пульса (10 с) не действует");
    assert.deepEqual(world.ended, []);
  });
});

describe("фильм в другом фрейме", () => {
  it("новый порт к его фрейму, старый закрыт, поколения идут дальше", async () => {
    const world = setup([{ frameId: 5, result: [AD, FILM] }]);
    await startHandoff(world);
    const adPort = world.framePorts.last();
    adElementGone(world);
    assert.equal(world.handoffs.heldVideo(TAB, 9, heldFilm({ document: "doc-film" })), "adopt");
    const filmPort = world.framePorts.last();
    assert.notEqual(filmPort, adPort);
    assert.equal(filmPort.frameId, 9);
    assert.equal(adPort.disconnectedByBackground, true);
    assert.equal(lastBegin(world).document, "doc-film");
    confirmMove(world);
    filmPort.deliver({ kind: "mse", event: { kind: "source_buffer_added", media_source: "ms-1", source_buffer: 0, mime_type: MP4 } });
    assert.ok(world.sockets.last().wireSummary().includes("source_buffer_added"));
    assert.deepEqual(world.ended, [], "закрытие старого порта не кончает сеанс");
  });

  it("рекламный фрейм убрали вместе с роликом: порт умер, сеанс ждёт фильм", async () => {
    const world = setup([{ frameId: 5, result: [AD, FILM] }]);
    await startHandoff(world);
    world.framePorts.last().drop();
    await flushPromises();
    assert.deepEqual(world.ended, []);
    assert.equal(world.handoffs.heldVideo(TAB, 0, heldFilm()), "adopt");
  });

  it("главный документ ушёл — это уход со страницы, а не конец ролика", async () => {
    const world = setup([{ frameId: 0, result: [AD, FILM] }]);
    await startHandoff(world);
    world.framePorts.last().drop();
    await flushPromises();
    assert.deepEqual(world.ended, [{ kind: "page_gone", reason: "navigated_away" }]);
  });
});

describe("цепочка и пропуск", () => {
  it("две рекламы подряд, потом фильм: признак рекламы переживает переезд", async () => {
    const world = adWorld();
    await startHandoff(world);
    clock(world, "ended");
    adElementGone(world);
    const secondAd = report(7, [FILM, { ...AD, element: 7 }]);
    assert.equal(world.handoffs.heldVideo(TAB, 0, secondAd), "adopt");
    confirmMove(world);
    assert.equal(world.handoffs.isAdOnScreen(TAB), true, "второй ролик — тоже реклама");
    adElementGone(world);
    assert.equal(world.handoffs.heldVideo(TAB, 0, heldFilm()), "adopt");
    confirmMove(world);
    assert.equal(world.handoffs.isAdOnScreen(TAB), false);
    assert.equal(world.door.requests.length, 1);
  });

  it("пропуск посреди ролика: страница запустила фильм, прежняя тень снимается на начало и на паузе", async () => {
    const world = adWorld();
    await startHandoff(world);
    clock(world, "playing");
    assert.equal(world.handoffs.heldVideo(TAB, 0, heldFilm()), "adopt");
    const posted = world.framePorts.last().posted;
    const leave = posted.at(-2);
    assert.deepEqual(leave, { kind: "end_handoff", position: { kind: "at", position_us: 0 }, resume: { kind: "exactly", resume: "paused" } });
    assert.equal(posted.at(-1)?.kind, "begin_handoff", "сначала снята старая тень, потом новая принята");
    // Подтверждение снятия старой тени приходит раньше ответа новой и сеанс не кончает.
    world.framePorts.last().deliver({ kind: "handoff_ended", cause: "returned" });
    assert.deepEqual(world.ended, []);
    confirmMove(world);
    assert.equal(world.handoffs.isAdOnScreen(TAB), false);
  });
});

describe("концы", () => {
  it("реклама кончилась, фильма нет: честный конец 4009 по пределу ожидания, страница уже сама", async () => {
    const world = adWorld();
    await startHandoff(world);
    adElementGone(world);
    world.clock.advance(NEXT_VIDEO_WAIT_MS - 1);
    assert.deepEqual(world.ended, []);
    world.clock.advance(1);
    assert.equal(world.sockets.last().closedWithCode, SESSION_CLOSE_CODES.page_took_video_back);
    assert.deepEqual(world.ended, [{ kind: "page_took_video_back" }]);
    const armingsAtEnd = world.armings.length;
    world.clock.advance(NEXT_VIDEO_ARM_REFRESH_MS * 5);
    assert.equal(world.armings.length, armingsAtEnd, "после конца сторож больше не продлевается");
  });

  it("реклама доиграла, а страница её не убрала: тот же предел, тень снимается", async () => {
    const world = adWorld();
    await startHandoff(world);
    clock(world, "ended");
    // Тень жива и подаёт пульс: сеанс кончает предел ожидания, а не сторож тишины.
    for (let waited = 0; waited < NEXT_VIDEO_WAIT_MS; waited += SHADOW_SILENCE_LIMIT_MS / 2) {
      world.framePorts.last().deliver({ kind: "shadow_alive" });
      world.clock.advance(SHADOW_SILENCE_LIMIT_MS / 2);
    }
    assert.deepEqual(world.ended, [{ kind: "page_took_video_back" }]);
    assert.deepEqual(world.framePorts.last().disconnectedByBackground, true);
  });

  it("зритель закрыл окно между роликами: подтверждения не ждём, тени нет", async () => {
    const world = adWorld();
    await startHandoff(world);
    adElementGone(world);
    world.sockets.last().receive({ type: "return_to_page", position_us: 24_000_000, resume: "paused" });
    assert.deepEqual(world.ended, [{ kind: "returned_to_page" }]);
  });

  it("вкладку закрыли между роликами: tab_gone{closed}", async () => {
    const world = adWorld();
    await startHandoff(world);
    adElementGone(world);
    world.handoffs.tabRemoved(TAB);
    assert.deepEqual(world.ended, [{ kind: "page_gone", reason: "closed" }]);
  });

  it("новый документ не принял тень: сеанс снова ждёт в пределах того же срока", async () => {
    const world = adWorld();
    await startHandoff(world);
    adElementGone(world);
    world.handoffs.heldVideo(TAB, 0, heldFilm());
    world.framePorts.last().deliver({ kind: "handoff_began", request_id: lastBegin(world).request_id, outcome: "no_media" });
    assert.deepEqual(world.ended, []);
    assert.equal(world.handoffs.heldVideo(TAB, 0, heldFilm()), "adopt", "следующая попытка принята");
  });
});

describe("сбои переезда (ревью 09c2)", () => {
  it("новый документ молчит дольше предела: тень снимается, поздний ответ страницы её не оживит", async () => {
    const world = adWorld();
    await startHandoff(world);
    adElementGone(world);
    world.handoffs.heldVideo(TAB, 0, heldFilm());
    const port = world.framePorts.last();
    world.clock.advance(HANDOFF_BEGIN_REPLY_TIMEOUT_MS);
    assert.deepEqual(port.posted.at(-1), { kind: "end_handoff", position: { kind: "shadow_position" }, resume: { kind: "as_shadow" } });
    assert.deepEqual(world.ended, [], "сеанс ждёт следующее видео дальше");
  });

  it("страница, которая подсовывает видео и не отвечает, не продлевает ожидание: 15 с общие", async () => {
    const world = adWorld();
    await startHandoff(world);
    adElementGone(world);
    world.clock.advance(NEXT_VIDEO_WAIT_MS - 1);
    world.handoffs.heldVideo(TAB, 0, heldFilm());
    world.clock.advance(HANDOFF_BEGIN_REPLY_TIMEOUT_MS);
    assert.deepEqual(world.ended, [{ kind: "page_took_video_back" }], "после неудавшегося переезда срока больше нет");
  });

  it("человек ушёл на другой адрес, пока сеанс в рекламном фрейме: это уход со страницы, а не конец ролика", async () => {
    const world = setup([{ frameId: 5, result: [AD, FILM] }]);
    await startHandoff(world);
    world.navigateTab("https://other.example/");
    world.framePorts.last().drop();
    await flushPromises();
    assert.deepEqual(world.ended, [{ kind: "page_gone", reason: "navigated_away" }]);
  });

  it("вкладку закрыли: фрейм умер вместе с ней", async () => {
    const world = setup([{ frameId: 5, result: [AD, FILM] }]);
    await startHandoff(world);
    world.closeTab();
    world.framePorts.last().drop();
    await flushPromises();
    assert.deepEqual(world.ended, [{ kind: "page_gone", reason: "closed" }]);
  });
});

describe("что не переезжает", () => {
  it("фильм идёт, страница запустила другое видео: превью не уводит сеанс", async () => {
    const world = setup([{ frameId: 0, result: [PLAIN_FILM] }]);
    await startHandoff(world);
    clock(world, "playing");
    assert.equal(world.handoffs.heldVideo(TAB, 0, report(8, [{ ...FILM, element: 8, playing: true }])), "release");
  });

  it("решение 179 в силе: фильм посреди просмотра забрали — 4009 сразу", async () => {
    const world = setup([{ frameId: 0, result: [PLAIN_FILM] }]);
    await startHandoff(world);
    adElementGone(world);
    assert.equal(world.sockets.last().closedWithCode, SESSION_CLOSE_CODES.page_took_video_back);
  });

  it("баннер, скрытое видео и элемент без вердикта отпускаются, сеанс на рекламе", async () => {
    const world = adWorld();
    await startHandoff(world);
    const banner: VideoCandidate = { ...AD, element: 8, muted: true, loop: true, visible_area_px: 14_400 };
    assert.equal(world.handoffs.heldVideo(TAB, 0, report(8, [banner])), "release");
    assert.equal(world.handoffs.heldVideo(TAB, 0, heldFilm({ visible_area_px: 0 })), "release");
    assert.equal(world.handoffs.heldVideo(TAB, 0, report(99, [FILM])), "release");
    assert.equal(lastBegin(world).element, AD.element, "новых команд странице нет");
  });

  it("согласие на локальный адрес, данное странице, чужому фрейму не достаётся", () => {
    const consented = { pageHost: "site.example", setting: "ask", consentedHost: "192.168.1.5" } as const;
    const local: VideoCandidate = { ...AD, frame_host: "ads.example", file_host: "192.168.1.5", duration_s: 600 };
    assert.deepEqual(judgeHeldVideo(9, report(AD.element, [local]), consented, WITH_NATIVE_HLS), { kind: "release" });
    const samePage = judgeHeldVideo(9, report(AD.element, [{ ...local, frame_host: "site.example" }]), consented, WITH_NATIVE_HLS);
    assert.equal(samePage.kind, "adopt", "та же страница пользуется своим согласием");
  });

  it("локальный адрес со страницы из интернета сторож не открывает (решение 199)", async () => {
    const world = adWorld();
    await startHandoff(world);
    adElementGone(world);
    const local: VideoCandidate = { ...AD, element: 8, file_host: "192.168.1.5", duration_s: 600 };
    assert.equal(world.handoffs.heldVideo(TAB, 0, report(8, [local])), "release");
  });
});

describe("неузнанная реклама (свой фрейм, незнакомый домен)", () => {
  /** Короткий файл без соседей: вердикт «фильм». */
  const UNRECOGNISED: VideoCandidate = { ...AD, document: "doc-frame" };

  it("сторож взведён заранее, но пока ролик идёт, переезд закрыт", async () => {
    const world = setup([{ frameId: 5, result: [UNRECOGNISED] }]);
    await startHandoff(world);
    assert.equal(world.armings.length, 1);
    assert.equal(world.handoffs.isAdOnScreen(TAB), false);
    clock(world, "playing");
    assert.equal(world.handoffs.heldVideo(TAB, 0, heldFilm()), "release");
  });

  it("ролик дошёл до конца: фильм принимается, а окно не закрывается", async () => {
    const world = setup([{ frameId: 5, result: [UNRECOGNISED] }]);
    await startHandoff(world);
    clock(world, "ended");
    assert.equal(world.handoffs.heldVideo(TAB, 0, heldFilm()), "adopt");
    assert.deepEqual(world.ended, []);
  });

  it("пропуск посреди неузнанного ролика: страница его убрала, сеанс ждёт следующее видео и принимает его", async () => {
    const world = setup([{ frameId: 5, result: [UNRECOGNISED] }]);
    await startHandoff(world);
    clock(world, "playing");
    adElementGone(world);
    assert.deepEqual(world.ended, [], "короткий файл убрали — это не «страница забрала видео»");
    assert.equal(world.sockets.last().closedWithCode, undefined);
    assert.equal(world.handoffs.heldVideo(TAB, 5, heldFilm()), "adopt");
  });

  it("запоздалый `ended` старого поколения после переезда не открывает переезд снова", async () => {
    const world = setup([{ frameId: 5, result: [UNRECOGNISED] }]);
    await startHandoff(world);
    clock(world, "ended");
    world.handoffs.heldVideo(TAB, 0, heldFilm());
    confirmMove(world);
    clock(world, "ended", 2);
    assert.equal(world.handoffs.heldVideo(TAB, 0, report(8, [{ ...FILM, element: 8, playing: true }])), "release");
  });
});
