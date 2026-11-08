// Слежение за настройкой режима 2 в изолированном мире (сессия 12a): охрана включается и выключается на лету,
// а фону не уходит ничего.
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { watchAutoHandoffSettings } from "../../src/content/auto-handoff-watch.ts";
import { AUTO_HANDOFF_SETTINGS_KEY, DEFAULT_AUTO_HANDOFF_SETTINGS, type AutoHandoffSettings } from "../../src/shared/auto-handoff-settings.ts";
import type { AutoHandoffWatchState } from "../../src/shared/auto-handoff-contract.ts";

type ChangeListener = (changes: Readonly<Record<string, { readonly newValue?: unknown }>>) => void;

function setup(read: () => Promise<AutoHandoffSettings>) {
  const applied: AutoHandoffWatchState[] = [];
  const notes: string[] = [];
  let changeListener: ChangeListener = () => {};
  let stopped = false;
  watchAutoHandoffSettings({
    readSettings: read,
    onLocalStorageChanged: (listener) => {
      changeListener = listener;
      return () => void (stopped = true);
    },
    applyWatch: (state) => void applied.push(state),
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
    assert.deepEqual(enabledStart.applied, ["on"]);
    const perSite = setup(() => Promise.resolve({ ...DEFAULT_AUTO_HANDOFF_SETTINGS, site_rules: { "tube.example": "always" } }));
    await settled();
    assert.deepEqual(perSite.applied, ["on"]);
  });

  it("фильтр звука снят (решение 260): охрана `on_including_muted`, снова включён — `on`", async () => {
    const { applied, change } = setup(() => Promise.resolve({ ...enabled, require_sound: false }));
    await settled();
    change({ ...enabled });
    change({ ...DEFAULT_AUTO_HANDOFF_SETTINGS, require_sound: false });
    assert.deepEqual(applied, ["on_including_muted", "on", "off"]);
  });

  it("человек переключил настройку: охрана меняется на лету", async () => {
    const { applied, change } = setup(() => Promise.resolve(DEFAULT_AUTO_HANDOFF_SETTINGS));
    await settled();
    change({ ...enabled });
    change({ ...DEFAULT_AUTO_HANDOFF_SETTINGS });
    assert.deepEqual(applied, ["off", "on", "off"]);
  });

  it("изменение пришло раньше ответа чтения: старый ответ выбрасывается", async () => {
    let answerRead: (settings: AutoHandoffSettings) => void = () => {};
    const { applied, change } = setup(() => new Promise((resolve) => void (answerRead = resolve)));
    change({ ...enabled });
    answerRead(DEFAULT_AUTO_HANDOFF_SETTINGS);
    await settled();
    assert.deepEqual(applied, ["on"]);
  });

  it("чужой ключ хранилища охрану не трогает", async () => {
    const { applied, rawChange } = setup(() => Promise.resolve(enabled));
    await settled();
    rawChange({ someOtherSetting: { newValue: false } });
    assert.deepEqual(applied, ["on"]);
  });

  it("чтение не удалось — охрана остаётся выключенной (ничего не применено), след для разработчика оставлен", async () => {
    const { applied, notes } = setup(() => Promise.reject(new Error("storage is gone")));
    await settled();
    assert.deepEqual(applied, []);
    assert.deepEqual(notes, ["could not read the auto handoff settings"]);
  });
});
