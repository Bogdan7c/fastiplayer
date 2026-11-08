// Слежение за настройкой режима 2 в изолированном мире (сессия 12a): охрана включается и выключается на лету,
// а фону не уходит ничего.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { watchAutoHandoffSettings } from "../../src/content/auto-handoff-watch.ts";
import { AUTO_HANDOFF_SETTINGS_KEY, DEFAULT_AUTO_HANDOFF_SETTINGS, type AutoHandoffSettings } from "../../src/shared/auto-handoff-settings.ts";
import type { AutoHandoffWatch } from "../../src/shared/auto-handoff-contract.ts";

function labelOf(watch: AutoHandoffWatch): string {
  return watch.state === "off" ? "off" : `on muted=${watch.muted} looped=${watch.looped}`;
}

/** Охрана включена при фильтрах по умолчанию (все выключены, решение 261): задерживает всё, судит фон. */
const ON = "on muted=hold looped=hold";

type ChangeListener = (changes: Readonly<Record<string, { readonly newValue?: unknown }>>) => void;

function setup(read: () => Promise<AutoHandoffSettings>) {
  /** Что ушло MAIN-миру, словами: `off` или `on muted=… looped=…`. */
  const applied: string[] = [];
  const notes: string[] = [];
  let changeListener: ChangeListener = () => {};
  let stopped = false;
  watchAutoHandoffSettings({
    readSettings: read,
    onLocalStorageChanged: (listener) => {
      changeListener = listener;
      return () => void (stopped = true);
    },
    applyWatch: (watch) => void applied.push(labelOf(watch)),
    note: (message) => void notes.push(message),
  });
  return { applied, notes, rawChange: (changes: Parameters<ChangeListener>[0]) => changeListener(changes), change: (newValue: unknown) => changeListener({ [AUTO_HANDOFF_SETTINGS_KEY]: { newValue } }), isStopped: () => stopped };
}

const settled = (): Promise<void> => new Promise((resolve) => setImmediate(resolve));
const enabled: AutoHandoffSettings = { ...DEFAULT_AUTO_HANDOFF_SETTINGS, enabled: true };

describe("слежение за настройкой режима 2", () => {
  it("настройка выключена по умолчанию: охрана `off`", async () => {
    const { applied } = setup(() => Promise.resolve(DEFAULT_AUTO_HANDOFF_SETTINGS));
    await settled();
    assert.deepEqual(applied, ["off"]);
  });

  it("включена при старте — `on`; сайт с правилом `always` при выключенном режиме тоже включает охрану", async () => {
    const enabledStart = setup(() => Promise.resolve(enabled));
    await settled();
    assert.deepEqual(enabledStart.applied, [ON]);
    const perSite = setup(() => Promise.resolve({ ...DEFAULT_AUTO_HANDOFF_SETTINGS, site_rules: { "tube.example": "always" } }));
    await settled();
    assert.deepEqual(perSite.applied, [ON]);
  });

  it("общие включатели фильтров звука и цикла доходят до охраны; списки сайтов — нет (их судит фон)", async () => {
    const filters = DEFAULT_AUTO_HANDOFF_SETTINGS.video_filters;
    const { applied, change } = setup(() => Promise.resolve({ ...enabled, video_filters: { ...filters, sound: { enabled: true, sites: [] } } }));
    await settled();
    change({ ...enabled, video_filters: { ...filters, loop: { enabled: true, sites: [] } } });
    change({ ...enabled, video_filters: { ...filters, sound: { enabled: false, sites: ["a.example"] }, duration: { enabled: true, sites: [] } } });
    assert.deepEqual(applied, ["on muted=skip looped=hold", "on muted=hold looped=skip", ON]);
  });

  it("человек переключил настройку: охрана меняется на лету", async () => {
    const { applied, change } = setup(() => Promise.resolve(DEFAULT_AUTO_HANDOFF_SETTINGS));
    await settled();
    change({ ...enabled });
    change({ ...DEFAULT_AUTO_HANDOFF_SETTINGS });
    assert.deepEqual(applied, ["off", ON, "off"]);
  });

  it("изменение пришло раньше ответа чтения: старый ответ выбрасывается", async () => {
    let answerRead: (settings: AutoHandoffSettings) => void = () => {};
    const { applied, change } = setup(() => new Promise((resolve) => void (answerRead = resolve)));
    change({ ...enabled });
    answerRead(DEFAULT_AUTO_HANDOFF_SETTINGS);
    await settled();
    assert.deepEqual(applied, [ON]);
  });

  it("чужой ключ хранилища охрану не трогает", async () => {
    const { applied, rawChange } = setup(() => Promise.resolve(enabled));
    await settled();
    rawChange({ someOtherSetting: { newValue: false } });
    assert.deepEqual(applied, [ON]);
  });

  it("чтение не удалось — охрана остаётся выключенной (ничего не применено), след для разработчика оставлен", async () => {
    const { applied, notes } = setup(() => Promise.reject(new Error("storage is gone")));
    await settled();
    assert.deepEqual(applied, []);
    assert.deepEqual(notes, ["could not read the auto handoff settings"]);
  });
});
