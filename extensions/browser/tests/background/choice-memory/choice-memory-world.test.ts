// Память выбора через весь фон (14b2): сокет плеера, порт фрейма, зеркало вариантов, реле субтитров и хранилище
// памяти вместе. Проверяется то, что увидел бы человек: что легло в хранилище, что нажалось на странице, что
// ушло плееру.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { createChoiceMemoryCommandListener } from "../../../src/background/choice-memory/choice-memory-commands.ts";
import { createChoiceMemoryStore } from "../../../src/background/choice-memory/choice-memory-store.ts";
import { seriesFingerprintOf } from "../../../src/background/choice-memory/series-fingerprint.ts";
import { CHOICE_MEMORY_POLICY } from "../../../src/background/choice-memory/session-choice-memory.ts";
import { translatePlayerMessage } from "../../../src/background/handoff/player-message-translator.ts";
import { PROTOCOL_VERSION_1_6, type PageSessionMessage } from "../../../src/protocol/index.ts";
import { REMEMBERED_CHOICES_KEY, type ChoiceMemoryReply, type RememberedChoices } from "../../../src/shared/choice-memory-contract.ts";
import { VARIANT_WATCH_REPORT_KIND, type PageVariantDescription } from "../../../src/shared/variant-watch-contract.ts";
import { memoryStore } from "../auto-handoff-lists-fake-dom.ts";
import { setup, startHandoff, TAB, type World, type WorldOptions } from "../handoff-world.ts";
import { documentEvent } from "../page-subtitles/subtitle-support.ts";
import { flushPromises, grantFor } from "../support.ts";

// Вкладка мира — https://www.example.com/…, сериал — ключ адаптера стенда.
const FINGERPRINT = await seriesFingerprintOf("example.com", "bench-series");

function episode(episodeKey: string, dub: "dub-a" | "dub-b"): PageVariantDescription {
  return {
    seriesKey: "bench-series",
    episodeKey,
    audio: [
      { id: "dub-a", label: "Dub A", selected: dub === "dub-a" },
      { id: "dub-b", label: "Dub B", selected: dub === "dub-b" },
    ],
    quality: [
      { id: "video-360", label: "360p", selected: true },
      { id: "video-720", label: "720p", selected: false },
    ],
    subtitles: [],
    nextEpisode: true,
  };
}

const report = (description: PageVariantDescription) => ({ kind: VARIANT_WATCH_REPORT_KIND, adapter: "bench-series@1", description }) as const;

const storedSeries = (world: World): RememberedChoices | undefined =>
  (world.choiceStorage.values.get(REMEMBERED_CHOICES_KEY) as { series: Record<string, RememberedChoices> } | undefined)?.series[FINGERPRINT];

const clockAt = (seconds: number, state: "playing" | "waiting" = "playing") =>
  ({ type: "clock", seq: 1, position_us: seconds * 1_000_000, state, rate: 1000 }) as const;

async function started(options: WorldOptions = {}, remembered?: Omit<RememberedChoices, "used_at">): Promise<World> {
  const world = setup(undefined, undefined, undefined, options);
  if (remembered !== undefined) {
    world.choiceStorage.values.set(REMEMBERED_CHOICES_KEY, { version: 1, series: { [FINGERPRINT]: { ...remembered, used_at: 3 } } });
  }
  await startHandoff(world);
  return world;
}

/**
 * Дать памяти доработать: отпечаток считает `crypto.subtle` в пуле потоков Node, это не микрозадача — нужны
 * круги настоящего цикла событий. Под нагрузкой параллельного прогона их число непредсказуемо, поэтому
 * `eventually` ждёт условия (с потолком), а `settle` — фиксированные круги для проверок «ничего не случилось».
 */
async function settle(): Promise<void> {
  for (let round = 0; round < 10; round += 1) {
    await new Promise((resolve) => setTimeout(resolve, 1));
    await flushPromises();
  }
}

const EVENTUALLY_LIMIT_MS = 5_000;

async function eventually(condition: () => boolean, what: string): Promise<void> {
  const deadline = Date.now() + EVENTUALLY_LIMIT_MS;
  while (!condition()) {
    if (Date.now() > deadline) assert.fail(`не дождались: ${what}`);
    await new Promise((resolve) => setTimeout(resolve, 2));
    await flushPromises();
  }
}

const sentOfType = <Type extends PageSessionMessage["type"]>(world: World, type: Type) =>
  world.sockets
    .last()
    .sentMessages()
    .filter((message): message is Extract<PageSessionMessage, { type: Type }> => message.type === type);

describe("запись через весь фон", () => {
  it("выбор в меню плеера → кнопка на странице нажата → в хранилище «Dub B» под отпечатком сайта и сериала", async () => {
    const world = await started();
    world.handoffs.variantWatchReport(TAB, 0, report(episode("1", "dub-a")));
    world.sockets.last().receive({ type: "select_variant", kind: "audio", variant_id: "dub-b" });
    await eventually(() => storedSeries(world) !== undefined, "запись в хранилище");
    assert.deepEqual(world.variantMirror.commands.map((entry) => entry.command), [{ kind: "select", variant_kind: "audio", variant_id: "dub-b" }]);
    assert.equal(storedSeries(world)?.audio, "Dub B");
    // В хранилище нет ни сайта, ни сериала открытым текстом.
    assert.ok(!JSON.stringify(world.choiceStorage.values.get(REMEMBERED_CHOICES_KEY)).includes("example"));
  });

  it("приватное окно: тот же выбор нажат на странице, но в хранилище ничего не записано (решение 285)", async () => {
    const world = await started({ incognito: true });
    world.handoffs.variantWatchReport(TAB, 0, report(episode("1", "dub-a")));
    world.sockets.last().receive({ type: "select_variant", kind: "audio", variant_id: "dub-b" });
    await settle();
    assert.equal(world.variantMirror.commands.length, 1);
    assert.equal(world.choiceStorage.state.writes, 0);
  });

  it("subtitle_choice плеера → в хранилище язык и подпись дорожки страницы", async () => {
    const world = await started();
    world.handoffs.variantWatchReport(TAB, 0, report(episode("1", "dub-a")));
    world.framePorts.last().deliver(documentEvent({ key: 1 }));
    const offered = sentOfType(world, "subtitle_track_offered");
    assert.equal(offered.length, 1);
    world.sockets.last().receive({ type: "subtitle_choice", choice: { kind: "page_track", track: offered[0]?.track ?? 0 } });
    await eventually(() => storedSeries(world)?.subtitles !== undefined, "субтитры в хранилище");
    assert.deepEqual(storedSeries(world)?.subtitles, { kind: "track", language: "en", label: "English" });
  });

  it("переводчик: subtitle_choice — выбор для памяти, странице не уходит", () => {
    assert.deepEqual(translatePlayerMessage({ type: "subtitle_choice", choice: { kind: "off" } }, undefined), {
      kind: "site_adapter",
      command: { kind: "subtitle_choice", choice: { kind: "off" } },
    });
  });
});

describe("применение через весь фон", () => {
  it("запомнена «Dub B»: у начала серии страница сама получает команду выбрать её", async () => {
    const world = await started({}, { audio: "Dub B" });
    world.sockets.last().receive(clockAt(1));
    world.handoffs.variantWatchReport(TAB, 0, report(episode("1", "dub-a")));
    await eventually(() => world.variantMirror.commands.length > 0, "команда памяти странице");
    assert.deepEqual(world.variantMirror.commands, [{ tabId: TAB, frameId: 0, command: { kind: "select", variant_kind: "audio", variant_id: "dub-b" } }]);
  });

  it("часы «0 / waiting» до показа кадра — не начало серии; кадр на 20-й минуте — только качество (решение 287)", async () => {
    const world = await started({}, { audio: "Dub B", quality: "720p" });
    world.sockets.last().receive(clockAt(0, "waiting"));
    world.handoffs.variantWatchReport(TAB, 0, report(episode("1", "dub-a")));
    await settle();
    assert.equal(world.variantMirror.commands.length, 0, "позиция неизвестна — ждать");
    world.sockets.last().receive(clockAt(20 * 60, "playing"));
    await eventually(() => world.variantMirror.commands.length > 0, "команда памяти странице");
    assert.deepEqual(
      world.variantMirror.commands.map((entry) => entry.command),
      [{ kind: "select", variant_kind: "quality", variant_id: "video-720" }],
    );
  });

  it("запомненные субтитры: на новой серии плееру уходит subtitle_preference с дорожкой ЭТОЙ серии", async () => {
    const world = await started({}, { subtitles: { kind: "track", language: "en", label: "English" } });
    world.handoffs.variantWatchReport(TAB, 0, report(episode("1", "dub-a")));
    world.framePorts.last().deliver(documentEvent({ key: 1 }));
    await eventually(() => sentOfType(world, "subtitle_preference").length > 0, "подсказка субтитров первой серии");
    assert.equal(sentOfType(world, "subtitle_preference").length, 1, "первая серия: её дорожка");
    // Дальше запаса границы серии, но меньше 10 с тишины тени (иначе сеанс честно кончится без пульса).
    world.clock.advance(CHOICE_MEMORY_POLICY.episodeTrackGraceMs + 1_000);
    world.handoffs.variantWatchReport(TAB, 0, report(episode("2", "dub-a")));
    await settle();
    assert.equal(sentOfType(world, "subtitle_preference").length, 1, "дорожка прошлой серии не годится");
    world.framePorts.last().deliver(documentEvent({ key: 2 }));
    const [, second] = sentOfType(world, "subtitle_track_offered");
    assert.ok(second !== undefined && second.track !== 1);
    assert.equal(sentOfType(world, "subtitle_preference").length, 2);
    assert.deepEqual(sentOfType(world, "subtitle_preference").at(-1)?.choice, { kind: "page_track", track: second?.track });
  });

  it("плеер старше 1.7: подсказка субтитров не уходит (он рвал бы сеанс), сеанс живёт", async () => {
    const world = setup();
    world.choiceStorage.values.set(REMEMBERED_CHOICES_KEY, { version: 1, series: { [FINGERPRINT]: { subtitles: { kind: "off" }, used_at: 3 } } });
    world.door.nextOutcome = (request) => grantFor(request, "started", PROTOCOL_VERSION_1_6);
    await startHandoff(world);
    world.handoffs.variantWatchReport(TAB, 0, report(episode("1", "dub-a")));
    await eventually(() => world.adMirror.notes.some((note) => note.startsWith("remembered subtitles suggested")), "решение памяти о субтитрах");
    assert.deepEqual(sentOfType(world, "subtitle_preference"), []);
    assert.ok(world.adMirror.notes.some((note) => /remembered subtitles suggested to the player: player_too_old/u.test(note)));
    assert.deepEqual(world.ended, []);
  });
});

describe("просьбы страницы настроек", () => {
  const IDENTITY = { extensionId: "our-extension", extensionBaseUrl: "chrome-extension://our-extension/" };
  const ownPage = { id: "our-extension", url: "chrome-extension://our-extension/options.html" };

  async function askListener(listener: ReturnType<typeof createChoiceMemoryCommandListener>, message: unknown, sender: object): Promise<ChoiceMemoryReply | "no_answer"> {
    let reply: ChoiceMemoryReply | "no_answer" = "no_answer";
    const asynchronous = listener(message, sender, (answer) => void (reply = answer));
    if (asynchronous) await settle();
    return reply;
  }

  it("count и forget от своей страницы; после forget память пуста", async () => {
    const storage = memoryStore();
    const memory = createChoiceMemoryStore({ store: storage.store, now: () => 1 });
    await memory.remember(FINGERPRINT, { audio: "Dub B" });
    const listener = createChoiceMemoryCommandListener(IDENTITY, memory);
    assert.deepEqual(await askListener(listener, { kind: "count_remembered_choices" }, ownPage), { kind: "count", series: 1 });
    assert.deepEqual(await askListener(listener, { kind: "forget_remembered_choices" }, ownPage), { kind: "forgotten" });
    assert.equal(await memory.count(), 0);
  });

  it("страница сайта или чужой экстеншен не могут стереть память; сбой хранилища — честный failed", async () => {
    const storage = memoryStore();
    const memory = createChoiceMemoryStore({ store: storage.store, now: () => 1 });
    await memory.remember(FINGERPRINT, { audio: "Dub B" });
    const listener = createChoiceMemoryCommandListener(IDENTITY, memory);
    assert.equal(await askListener(listener, { kind: "forget_remembered_choices" }, { id: "our-extension", url: "https://example.com/" }), "no_answer");
    assert.equal(await askListener(listener, { kind: "forget_remembered_choices" }, { id: "someone-else", url: ownPage.url }), "no_answer");
    assert.equal(await memory.count(), 1);
    storage.state.failWrites = true;
    const failed = await askListener(listener, { kind: "forget_remembered_choices" }, ownPage);
    assert.equal(typeof failed === "object" && failed.kind, "failed");
  });
});
