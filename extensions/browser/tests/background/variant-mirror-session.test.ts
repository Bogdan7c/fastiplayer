// Меню вариантов страницы на уровне сеанса (14b): аренда наблюдения, `variant_menu_changed` и `page_navigation`
// на проводе, команды плеера `select_variant` / `next_episode` во фрейм, приславший меню. Фон целиком на
// фейковых часах, плеере, порте и браузере.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { VARIANT_WATCH_LEASE_MS, VARIANT_WATCH_RENEW_MS } from "../../src/background/handoff/variant-mirror.ts";
import { VARIANT_REPORT_SILENCE_MS } from "../../src/background/handoff/variant-frame-summary.ts";
import { createVariantWatchReportListener } from "../../src/background/handoff/variant-watch-listener.ts";
import { translatePlayerMessage } from "../../src/background/handoff/player-message-translator.ts";
import { protocolOutcomeOf } from "../../src/background/handoff/variant-outcome-report.ts";
import { PROTOCOL_VERSION_1_6, PROTOCOL_VERSION_1_7, type PageSessionMessage, type VariantMenu } from "../../src/protocol/index.ts";
import { VARIANT_WATCH_REPORT_KIND, type PageVariantDescription, type VariantActionOutcome, type VariantWatchReport } from "../../src/shared/variant-watch-contract.ts";
import { PLAYING_VIDEO, setup, startHandoff, TAB, type World } from "./handoff-world.ts";
import { flushPromises, grantFor } from "./support.ts";

const SERIES: PageVariantDescription = {
  seriesKey: "bench-series",
  episodeKey: "1",
  audio: [
    { id: "dub-a", label: "Dub A", selected: true },
    { id: "dub-b", label: "Dub B", selected: false },
  ],
  quality: [
    { id: "video-360", label: "360p", selected: true },
    { id: "video-720", label: "720p", selected: false },
  ],
  subtitles: [],
  nextEpisode: true,
};

const report = (description: PageVariantDescription | null, adapter = "bench-series@1"): VariantWatchReport => ({
  kind: VARIANT_WATCH_REPORT_KIND,
  adapter,
  description,
});

/** Сообщения плееру этого вида. */
function sent<Type extends PageSessionMessage["type"]>(world: World, type: Type): Extract<PageSessionMessage, { type: Type }>[] {
  return world.sockets
    .last()
    .sentMessages()
    .filter((message): message is Extract<PageSessionMessage, { type: Type }> => message.type === type);
}

const menus = (world: World): VariantMenu[] => sent(world, "variant_menu_changed").map((message) => message.menu);
const labelsOf = (menu: VariantMenu | undefined): string[] => menu?.options.map((option) => `${option.label}:${option.selection}`) ?? [];

describe("аренда наблюдения за вариантами", () => {
  it("до тени — ни аренды, ни меню; с тенью аренда уходит сразу и продлевается раз в секунду (решение 198)", async () => {
    const world = setup();
    world.handoffs.variantWatchReport(TAB, 0, report(SERIES));
    assert.deepEqual(world.variantMirror.armings, []);
    await startHandoff(world);
    assert.deepEqual(world.variantMirror.armings, [{ tabId: TAB, leaseMs: VARIANT_WATCH_LEASE_MS }]);
    world.clock.advance(VARIANT_WATCH_RENEW_MS * 4);
    assert.equal(world.variantMirror.armings.length, 5);
    // Донесение до тени не запомнилось: меню плеер не получил.
    assert.deepEqual(menus(world), []);
  });

  it("конец сеанса — продления прекращаются, донесения и команды больше ничего не делают", async () => {
    const world = setup();
    await startHandoff(world);
    world.handoffs.variantWatchReport(TAB, 0, report(SERIES));
    const socket = world.sockets.last();
    socket.receive({ type: "return_to_page", position_us: 0, resume: "paused" });
    world.framePorts.last().deliver({ kind: "handoff_ended", cause: "returned" });
    const armingsAtEnd = world.variantMirror.armings.length;
    world.clock.advance(VARIANT_WATCH_RENEW_MS * 10);
    assert.equal(world.variantMirror.armings.length, armingsAtEnd);
    world.handoffs.variantWatchReport(TAB, 0, report({ ...SERIES, audio: [] }));
    assert.equal(menus(world).length, 2, "после конца меню не обновляется");
    assert.equal(world.clock.liveTimers(), 0);
  });
});

describe("меню на проводе", () => {
  it("донесение фрейма — два меню и «следующая серия есть»; повтор того же описания молчит", async () => {
    const world = setup();
    await startHandoff(world);
    world.handoffs.variantWatchReport(TAB, 0, report(SERIES));
    assert.deepEqual(labelsOf(menus(world).find((menu) => menu.kind === "audio")), ["Dub A:selected", "Dub B:available"]);
    assert.deepEqual(labelsOf(menus(world).find((menu) => menu.kind === "quality")), ["360p:selected", "720p:available"]);
    assert.deepEqual(sent(world, "page_navigation"), [{ type: "page_navigation", next_episode: "available" }]);
    world.handoffs.variantWatchReport(TAB, 0, report(SERIES));
    world.clock.advance(VARIANT_WATCH_RENEW_MS);
    assert.equal(menus(world).length, 2);
    assert.equal(sent(world, "page_navigation").length, 1);
  });

  it("сменилась только озвучка — уходит только меню звука; меню качества не трогается", async () => {
    const world = setup();
    await startHandoff(world);
    world.handoffs.variantWatchReport(TAB, 0, report(SERIES));
    const dubB = { ...SERIES, audio: [{ ...SERIES.audio[0]!, selected: false }, { ...SERIES.audio[1]!, selected: true }] };
    world.handoffs.variantWatchReport(TAB, 0, report(dubB));
    const all = menus(world);
    assert.equal(all.length, 3);
    assert.equal(all[2]?.kind, "audio");
    assert.deepEqual(labelsOf(all[2]), ["Dub A:available", "Dub B:selected"]);
  });

  it("меню ушло (адаптер упал) — плеер получает пустые меню и «следующей серии нет»", async () => {
    const world = setup();
    await startHandoff(world);
    world.handoffs.variantWatchReport(TAB, 0, report(SERIES));
    world.handoffs.variantWatchReport(TAB, 0, report(null));
    const all = menus(world);
    assert.deepEqual(
      all.slice(2).map((menu) => [menu.kind, menu.options.length]),
      [
        ["audio", 0],
        ["quality", 0],
      ],
    );
    assert.deepEqual(sent(world, "page_navigation").at(-1), { type: "page_navigation", next_episode: "unavailable" });
  });

  it("фрейм замолчал дольше срока — его меню снимается на продлении", async () => {
    const world = setup();
    await startHandoff(world);
    world.handoffs.variantWatchReport(TAB, 0, report(SERIES));
    world.clock.advance(VARIANT_REPORT_SILENCE_MS + VARIANT_WATCH_RENEW_MS);
    assert.deepEqual(
      menus(world).slice(2).map((menu) => menu.options.length),
      [0, 0],
    );
  });

  it("адаптер без меню (ни одного годного пункта) ничего не шлёт: пустое меню до непустого не нужно", async () => {
    const world = setup();
    await startHandoff(world);
    world.handoffs.variantWatchReport(TAB, 0, report({ ...SERIES, audio: [], quality: [{ id: "bad id with spaces", label: "720p", selected: true }], nextEpisode: false }));
    assert.deepEqual(menus(world), []);
    assert.deepEqual(sent(world, "page_navigation"), []);
  });

  it("плеер старше 1.7: меню вариантов уходит (оно из 1.0), page_navigation — нет", async () => {
    const world = setup();
    world.door.nextOutcome = (request) => grantFor(request, "started", PROTOCOL_VERSION_1_6);
    await startHandoff(world);
    world.handoffs.variantWatchReport(TAB, 0, report(SERIES));
    assert.equal(menus(world).length, 2);
    assert.deepEqual(sent(world, "page_navigation"), []);
    // И Next такому плееру не предлагался — его просьбу (если бы пришла) фон всё равно исполнит по меню страницы.
  });

  it("главнее фрейм с видео сеанса, а не фрейм с меньшим номером; команды — ему же", async () => {
    // Видео сеанса во фрейме 3; меню приходит и от него, и от фрейма 1 (второй плеер на странице).
    const world = setup([{ frameId: 3, result: [PLAYING_VIDEO] }]);
    await startHandoff(world);
    world.handoffs.variantWatchReport(TAB, 1, report({ ...SERIES, audio: [{ id: "other", label: "Other player", selected: true }] }));
    world.handoffs.variantWatchReport(TAB, 3, report(SERIES));
    assert.deepEqual(labelsOf(menus(world).filter((menu) => menu.kind === "audio").at(-1)), ["Dub A:selected", "Dub B:available"]);
    world.sockets.last().receive({ type: "select_variant", kind: "audio", variant_id: "dub-b" });
    await flushPromises();
    assert.deepEqual(world.variantMirror.commands.map((sentCommand) => sentCommand.frameId), [3]);
  });

  it("фрейма видео в сводке нет — меню фрейма с наименьшим номером", async () => {
    const world = setup([{ frameId: 3, result: [PLAYING_VIDEO] }]);
    await startHandoff(world);
    world.handoffs.variantWatchReport(TAB, 7, report({ ...SERIES, audio: [{ id: "seven", label: "Seven", selected: true }] }));
    world.handoffs.variantWatchReport(TAB, 5, report({ ...SERIES, audio: [{ id: "five", label: "Five", selected: true }] }));
    assert.deepEqual(labelsOf(menus(world).filter((menu) => menu.kind === "audio").at(-1)), ["Five:selected"]);
  });
});

const WITH_SUBTITLES: PageVariantDescription = {
  ...SERIES,
  subtitles: [
    { id: "t0_abc", label: "English", selected: false },
    { id: "x-de", label: "German (auto-translated)", selected: false },
  ],
};

describe("субтитры страницы (15a2)", () => {
  it("меню субтитров уходит плееру третьим меню; без субтитров его нет", async () => {
    const world = setup();
    await startHandoff(world);
    world.handoffs.variantWatchReport(TAB, 0, report(SERIES));
    assert.equal(menus(world).some((menu) => menu.kind === "subtitles"), false, "пустое меню субтитров не шлётся");
    world.handoffs.variantWatchReport(TAB, 0, report(WITH_SUBTITLES));
    assert.deepEqual(labelsOf(menus(world).find((menu) => menu.kind === "subtitles")), ["English:available", "German (auto-translated):available"]);
  });

  it("select_variant субтитров доходит до фрейма меню с ключом пункта и не отвергается как «дорожки страницы»", async () => {
    const world = setup();
    await startHandoff(world);
    world.handoffs.variantWatchReport(TAB, 0, report(WITH_SUBTITLES));
    world.sockets.last().receive({ type: "select_variant", kind: "subtitles", variant_id: "x-de" });
    await flushPromises();
    assert.deepEqual(world.variantMirror.commands, [{ tabId: TAB, frameId: 0, command: { kind: "select", variant_kind: "subtitles", variant_id: "x-de" } }]);
    assert.ok(world.adMirror.notes.every((note) => !/subtitles are page tracks|ignored/u.test(note)));
  });

  it("выбор пункта, которого нет в меню субтитров, ничего не нажимает", async () => {
    const world = setup();
    await startHandoff(world);
    world.handoffs.variantWatchReport(TAB, 0, report(WITH_SUBTITLES));
    world.sockets.last().receive({ type: "select_variant", kind: "subtitles", variant_id: "x-fr" });
    await flushPromises();
    assert.deepEqual(world.variantMirror.commands, []);
    assert.ok(world.adMirror.notes.some((note) => /no such subtitles option/u.test(note)));
  });
});

describe("команды плеера", () => {
  it("select_variant уходит во фрейм, приславший меню этого вида, с ключом пункта", async () => {
    const world = setup();
    await startHandoff(world);
    world.handoffs.variantWatchReport(TAB, 0, report(SERIES));
    world.sockets.last().receive({ type: "select_variant", kind: "audio", variant_id: "dub-b" });
    world.sockets.last().receive({ type: "select_variant", kind: "quality", variant_id: "video-720" });
    await flushPromises();
    assert.deepEqual(world.variantMirror.commands, [
      { tabId: TAB, frameId: 0, command: { kind: "select", variant_kind: "audio", variant_id: "dub-b" } },
      { tabId: TAB, frameId: 0, command: { kind: "select", variant_kind: "quality", variant_id: "video-720" } },
    ]);
    assert.ok(world.adMirror.notes.every((note) => !/not done|could not reach/u.test(note)));
  });

  it("пункта нет в последнем меню (устаревший выбор) или меню нет — ничего не нажимается, отметка в журнале", async () => {
    const world = setup();
    await startHandoff(world);
    world.sockets.last().receive({ type: "select_variant", kind: "audio", variant_id: "dub-b" });
    world.handoffs.variantWatchReport(TAB, 0, report(SERIES));
    world.sockets.last().receive({ type: "select_variant", kind: "audio", variant_id: "dub-z" });
    world.sockets.last().receive({ type: "select_variant", kind: "subtitles", variant_id: "en" });
    await flushPromises();
    assert.deepEqual(world.variantMirror.commands, []);
    assert.equal(world.adMirror.notes.filter((note) => note.startsWith("select_variant ignored")).length, 3);
  });

  it("next_episode — во фрейм, сказавший «следующая серия есть»; без неё — никуда", async () => {
    const world = setup();
    await startHandoff(world);
    world.sockets.last().receive({ type: "next_episode" });
    await flushPromises();
    assert.deepEqual(world.variantMirror.commands, []);
    world.handoffs.variantWatchReport(TAB, 0, report(SERIES));
    world.sockets.last().receive({ type: "next_episode" });
    await flushPromises();
    assert.deepEqual(world.variantMirror.commands, [{ tabId: TAB, frameId: 0, command: { kind: "next" } }]);
  });

  it("исход person_only, отказ доставки — строки в журнал разработчика, сеанс живёт", async () => {
    const world = setup();
    await startHandoff(world);
    world.handoffs.variantWatchReport(TAB, 0, report(SERIES));
    world.variantMirror.behavior.outcome = "person_only";
    world.sockets.last().receive({ type: "select_variant", kind: "quality", variant_id: "video-720" });
    await flushPromises();
    world.variantMirror.behavior.outcome = new Error("frame is gone");
    world.sockets.last().receive({ type: "next_episode" });
    await flushPromises();
    assert.ok(world.adMirror.notes.some((note) => /select_variant quality not done on the page: person_only/u.test(note)));
    assert.ok(world.adMirror.notes.some((note) => /next_episode could not reach the frame: Error: frame is gone/u.test(note)));
    assert.deepEqual(world.ended, []);
  });

  it("версия адаптера — в журнале разработчика один раз (решение 283)", async () => {
    const world = setup();
    await startHandoff(world);
    world.handoffs.variantWatchReport(TAB, 0, report(SERIES));
    world.clock.advance(VARIANT_WATCH_RENEW_MS * 3);
    world.handoffs.variantWatchReport(TAB, 0, report(SERIES));
    assert.deepEqual(world.adMirror.notes.filter((note) => note.includes("bench-series@1")), ["variant adapter bench-series@1 describes the page"]);
  });

  it("переводчик: select_variant и next_episode — команды адаптеру, а не «пропустить»", () => {
    assert.deepEqual(translatePlayerMessage({ type: "select_variant", kind: "audio", variant_id: "dub-b" }, undefined), {
      kind: "site_adapter",
      command: { kind: "select_variant", variantKind: "audio", variantId: "dub-b" },
    });
    assert.deepEqual(translatePlayerMessage({ type: "next_episode" }, undefined), { kind: "site_adapter", command: { kind: "next_episode" } });
  });
});

const outcomes = (world: World) => sent(world, "variant_outcome").map((message) => ({ request: message.request, outcome: message.outcome }));
const selectAudioDubB = { kind: "select_variant", menu: "audio", variant_id: "dub-b" } as const;

describe("исход выбора зрителя плееру (протокол 1.8, 15c2)", () => {
  it("исход адаптера -> слово протокола: no_such_variant уходит как failed, остальные как есть", () => {
    const expected: Record<VariantActionOutcome, string> = {
      done: "done",
      no_such_episode: "no_such_episode",
      person_only: "person_only",
      no_such_variant: "failed",
      failed: "failed",
    };
    for (const [adapterOutcome, protocolOutcome] of Object.entries(expected)) {
      assert.equal(protocolOutcomeOf(adapterOutcome as VariantActionOutcome), protocolOutcome, adapterOutcome);
    }
  });

  it("каждый исход выбора доходит до плеера с точным запросом; no_such_variant приходит как failed", async () => {
    const world = setup();
    await startHandoff(world);
    world.handoffs.variantWatchReport(TAB, 0, report(SERIES));
    const cases: [VariantActionOutcome, string][] = [
      ["done", "done"],
      ["no_such_episode", "no_such_episode"],
      ["person_only", "person_only"],
      ["failed", "failed"],
      ["no_such_variant", "failed"],
    ];
    for (const [adapterOutcome, expected] of cases) {
      world.variantMirror.behavior.outcome = adapterOutcome;
      world.sockets.last().receive({ type: "select_variant", kind: "audio", variant_id: "dub-b" });
      await flushPromises();
      assert.deepEqual(outcomes(world).at(-1), { request: selectAudioDubB, outcome: expected }, adapterOutcome);
    }
    assert.equal(outcomes(world).length, cases.length);
  });

  it("выбор качества и субтитров тоже отвечает: запрос несёт своё меню", async () => {
    const world = setup();
    await startHandoff(world);
    world.handoffs.variantWatchReport(TAB, 0, report(WITH_SUBTITLES));
    world.sockets.last().receive({ type: "select_variant", kind: "quality", variant_id: "video-720" });
    world.sockets.last().receive({ type: "select_variant", kind: "subtitles", variant_id: "x-de" });
    await flushPromises();
    assert.deepEqual(outcomes(world), [
      { request: { kind: "select_variant", menu: "quality", variant_id: "video-720" }, outcome: "done" },
      { request: { kind: "select_variant", menu: "subtitles", variant_id: "x-de" }, outcome: "done" },
    ]);
  });

  it("отказ доставки во фрейм — failed; сеанс живёт", async () => {
    const world = setup();
    await startHandoff(world);
    world.handoffs.variantWatchReport(TAB, 0, report(SERIES));
    world.variantMirror.behavior.outcome = new Error("frame is gone");
    world.sockets.last().receive({ type: "select_variant", kind: "audio", variant_id: "dub-b" });
    await flushPromises();
    assert.deepEqual(outcomes(world), [{ request: selectAudioDubB, outcome: "failed" }]);
    assert.deepEqual(world.ended, []);
  });

  it("пункта нет в последнем меню (устаревший выбор) — плеер получает failed, ничего не нажато", async () => {
    const world = setup();
    await startHandoff(world);
    world.handoffs.variantWatchReport(TAB, 0, report(SERIES));
    world.sockets.last().receive({ type: "select_variant", kind: "audio", variant_id: "dub-z" });
    await flushPromises();
    assert.deepEqual(world.variantMirror.commands, []);
    assert.deepEqual(outcomes(world), [{ request: { kind: "select_variant", menu: "audio", variant_id: "dub-z" }, outcome: "failed" }]);
    assert.ok(world.adMirror.notes.some((note) => /select_variant ignored/u.test(note)), "заметка в журнале осталась");
  });

  it("Next: исход страницы уходит плееру; нет следующей серии — failed без обращения к странице", async () => {
    const world = setup();
    await startHandoff(world);
    world.sockets.last().receive({ type: "next_episode" });
    await flushPromises();
    assert.deepEqual(world.variantMirror.commands, []);
    assert.deepEqual(outcomes(world), [{ request: { kind: "next_episode" }, outcome: "failed" }]);
    world.handoffs.variantWatchReport(TAB, 0, report(SERIES));
    world.variantMirror.behavior.outcome = "person_only";
    world.sockets.last().receive({ type: "next_episode" });
    await flushPromises();
    assert.deepEqual(outcomes(world).at(-1), { request: { kind: "next_episode" }, outcome: "person_only" });
    world.variantMirror.behavior.outcome = "done";
    world.sockets.last().receive({ type: "next_episode" });
    await flushPromises();
    assert.deepEqual(outcomes(world).at(-1), { request: { kind: "next_episode" }, outcome: "done" });
    assert.equal(outcomes(world).length, 3);
  });

  it("общая версия 1.7: исход не шлётся (ни успех, ни отказ), всё прочее как раньше", async () => {
    const world = setup();
    world.door.nextOutcome = (request) => grantFor(request, "started", PROTOCOL_VERSION_1_7);
    await startHandoff(world);
    world.handoffs.variantWatchReport(TAB, 0, report(SERIES));
    assert.equal(menus(world).length, 2, "меню по-прежнему уходит");
    assert.deepEqual(sent(world, "page_navigation"), [{ type: "page_navigation", next_episode: "available" }]);
    world.sockets.last().receive({ type: "select_variant", kind: "audio", variant_id: "dub-b" });
    world.sockets.last().receive({ type: "select_variant", kind: "audio", variant_id: "dub-z" });
    world.sockets.last().receive({ type: "next_episode" });
    await flushPromises();
    assert.equal(world.variantMirror.commands.length, 2, "страница нажата как раньше");
    assert.deepEqual(outcomes(world), []);
    assert.deepEqual(world.ended, []);
  });

  it("сеанс кончился, пока фрейм работал — ответ фрейма плееру не шлётся", async () => {
    const world = setup();
    await startHandoff(world);
    world.handoffs.variantWatchReport(TAB, 0, report(SERIES));
    const socket = world.sockets.last();
    socket.receive({ type: "select_variant", kind: "audio", variant_id: "dub-b" });
    socket.receive({ type: "return_to_page", position_us: 0, resume: "paused" });
    world.framePorts.last().deliver({ kind: "handoff_ended", cause: "returned" });
    await flushPromises();
    assert.equal(world.variantMirror.commands.length, 1, "команда фрейму уйти успела");
    assert.deepEqual(outcomes(world), []);
  });
});

describe("приём донесений", () => {
  const EXTENSION_ID = "our-extension";
  const frameSender = { id: EXTENSION_ID, tab: { id: TAB }, frameId: 3 };

  it("свой фрейм с годной формой — дальше с вкладкой и фреймом от браузера", () => {
    const received: [number, number, VariantWatchReport][] = [];
    const listener = createVariantWatchReportListener(EXTENSION_ID, (tabId, frameId, accepted) => void received.push([tabId, frameId, accepted]));
    assert.equal(listener(report(SERIES), frameSender), false);
    assert.deepEqual(received, [[TAB, 3, report(SERIES)]]);
  });

  it("чужой экстеншен, не фрейм, лишнее поле, слишком много пунктов — отброшены", () => {
    const received: unknown[] = [];
    const listener = createVariantWatchReportListener(EXTENSION_ID, (...args) => void received.push(args));
    listener(report(SERIES), { ...frameSender, id: "someone-else" });
    listener(report(SERIES), { id: EXTENSION_ID });
    listener({ ...report(SERIES), extra: 1 }, frameSender);
    const tooMany = Array.from({ length: 257 }, (_, index) => ({ id: `v${index}`, label: `${index}`, selected: false }));
    listener(report({ ...SERIES, quality: tooMany }), frameSender);
    listener(report({ ...SERIES, seriesKey: "" }), frameSender);
    // Пометка «Auto» (14b2) — только булева.
    listener({ ...report(SERIES), description: { ...SERIES, quality: [{ id: "auto", label: "Auto", selected: true, automatic: "yes" }] } }, frameSender);
    assert.deepEqual(received, []);
    const withAuto = report({ ...SERIES, quality: [{ id: "auto", label: "Auto", selected: true, automatic: true }] });
    listener(withAuto, frameSender);
    assert.deepEqual(received, [[TAB, 3, withAuto]]);
  });
});
