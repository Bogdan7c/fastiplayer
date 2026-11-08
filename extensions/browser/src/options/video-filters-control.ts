// Страница настроек: раздел «Video filters» режима 2 (решения 260–261).
//
// Три фильтра: «без звука», «короткие», «зацикленные». У каждого — галочка «On every site» и свой список
// сайтов, где фильтр действует, даже когда галочка снята. У фильтра длины ещё и порог в секундах (он действует
// и для сайтов из списка). По умолчанию все фильтры выключены: человек настраивает их сам.
//
// Стиль `auto-handoff-lists-control.ts`: createElement, правка сохраняется сразу, статус «Saved.» /
// «Could not save: …». Любая запись: прочитать свежую настройку -> чистая правка
// (`auto-handoff-settings-edits.ts`) -> писать только при `changed`. Раздел перерисовывается, когда настройку
// меняют в другом окне.

import { readAutoHandoffSettings, writeAutoHandoffSettings, MAX_AUTO_HANDOFF_MIN_DURATION_S, type AutoHandoffSettings } from "../shared/auto-handoff-settings.ts";
import {
  withMinDuration,
  withVideoFilterEnabled,
  withVideoFilterSiteAdded,
  withVideoFilterSiteRemoved,
  type SettingsEdit,
} from "../shared/auto-handoff-settings-edits.ts";
import type { KeyValueStore } from "../shared/codec-storage.ts";
import { VIDEO_FILTER_NAMES, type VideoFilterName } from "../shared/video-filter-settings.ts";
import { describeEditRejection } from "./settings-edit-text.ts";

/** Заголовки фильтров (английский, пользовательский текст). */
export const VIDEO_FILTER_TITLES: Record<VideoFilterName, string> = {
  sound: "Skip videos without sound",
  duration: "Skip short videos",
  loop: "Skip looping videos",
};

/** Подсказка, когда список сайтов фильтра пуст. */
export const NO_FILTER_SITES_TEXT = "No sites.";

/** Что нужно разделу снаружи (в тестах подменяется). */
export interface VideoFiltersControlDependencies {
  readonly store: KeyValueStore;
  /** Подписаться на новые значения настройки. */
  readonly onSettingsChanged: (listener: (settings: AutoHandoffSettings) => void) => void;
}

/**
 * Порог из текста поля: целые секунды от 0 до предела. Пусто, дробь, минус, буквы, "1e3", слишком большое —
 * `undefined` (такое значение не записывается).
 */
export function parseMinDurationInput(text: string): number | undefined {
  const trimmed = text.trim();
  if (!/^\d{1,5}$/.test(trimmed)) return undefined;
  const seconds = Number(trimmed);
  return seconds <= MAX_AUTO_HANDOFF_MIN_DURATION_S ? seconds : undefined;
}

function textElement<Tag extends keyof HTMLElementTagNameMap>(tag: Tag, text: string): HTMLElementTagNameMap[Tag] {
  const element = document.createElement(tag);
  element.textContent = text;
  return element;
}

function buttonElement(text: string, id?: string): HTMLButtonElement {
  const button = textElement("button", text);
  button.type = "button";
  if (id !== undefined) button.id = id;
  return button;
}

/** Органы одного фильтра, которые перерисовываются по настройке. */
interface FilterControls {
  readonly enabledBox: HTMLInputElement;
  readonly sitesList: HTMLUListElement;
}

/** Нарисовать раздел в `container` и связать его с хранилищем. */
export async function mountVideoFiltersControl(container: HTMLElement, dependencies: VideoFiltersControlDependencies): Promise<void> {
  const status = document.createElement("p");
  status.id = "video-filters-status";

  /** Применить правку: читаем свежую настройку, пишем только при `changed`. `true` — правка принята. */
  async function applyEdit(edit: (current: AutoHandoffSettings) => SettingsEdit, rejectedEntry: string): Promise<boolean> {
    try {
      const result = edit(await readAutoHandoffSettings(dependencies.store));
      if (result.kind === "rejected") {
        status.textContent = describeEditRejection(result.reason, rejectedEntry);
        return false;
      }
      if (result.kind === "changed") await writeAutoHandoffSettings(dependencies.store, result.settings);
      status.textContent = "Saved.";
      return true;
    } catch (error) {
      status.textContent = `Could not save: ${String(error)}`;
      return false;
    }
  }

  // Порог длины: показывается в блоке фильтра длины.
  const durationInput = document.createElement("input");
  durationInput.type = "number";
  durationInput.id = "video-filter-duration-seconds";
  durationInput.min = "0";
  durationInput.max = String(MAX_AUTO_HANDOFF_MIN_DURATION_S);

  /** Один блок фильтра: заголовок, галочка, (порог), список сайтов с полем добавления. */
  function filterBlock(name: VideoFilterName): { readonly block: HTMLElement; readonly controls: FilterControls } {
    const enabledBox = document.createElement("input");
    enabledBox.type = "checkbox";
    enabledBox.id = `video-filter-${name}-enabled`;
    const enabledRow = document.createElement("label");
    enabledRow.className = "setting-row";
    enabledRow.append(enabledBox, textElement("span", " On every site"));
    enabledBox.addEventListener("change", () => {
      const enabled = enabledBox.checked;
      void applyEdit((current) => withVideoFilterEnabled(current, name, enabled), name);
    });

    const siteInput = document.createElement("input");
    siteInput.type = "text";
    siteInput.id = `video-filter-${name}-site`;
    siteInput.placeholder = "example.com";
    const siteAdd = buttonElement("Add", `video-filter-${name}-add`);
    siteAdd.addEventListener("click", () => {
      const site = siteInput.value;
      void applyEdit((current) => withVideoFilterSiteAdded(current, name, site), site).then((accepted) => {
        if (accepted) siteInput.value = "";
      });
    });
    const addRow = document.createElement("div");
    addRow.className = "setting-row";
    addRow.append(siteInput, siteAdd);

    const sitesList = document.createElement("ul");
    sitesList.id = `video-filter-${name}-sites`;

    const block = document.createElement("section");
    block.id = `video-filter-${name}`;
    block.append(textElement("h3", VIDEO_FILTER_TITLES[name]), enabledRow);
    if (name === "duration") {
      const durationRow = document.createElement("label");
      durationRow.className = "setting-row";
      durationRow.append(textElement("span", "Shorter than "), durationInput, textElement("span", " seconds"));
      block.append(durationRow);
    }
    block.append(textElement("p", "Also on these sites, even when it is off for every site:"), addRow, sitesList);
    return { block, controls: { enabledBox, sitesList } };
  }

  const controls = new Map<VideoFilterName, FilterControls>();
  const blocks = VIDEO_FILTER_NAMES.map((name) => {
    const made = filterBlock(name);
    controls.set(name, made.controls);
    return made.block;
  });

  container.append(
    textElement("h2", "Video filters"),
    textElement("p", "Filters keep some videos in the browser. All are off by default. A site is the address in the tab's address bar; a listed site covers its subdomains."),
    ...blocks,
    status,
  );

  /** Показать в полях то, что лежит в хранилище. */
  function render(settings: AutoHandoffSettings): void {
    durationInput.value = String(settings.min_duration_s);
    for (const name of VIDEO_FILTER_NAMES) {
      const filter = settings.video_filters[name];
      const filterControls = controls.get(name);
      if (filterControls === undefined) continue;
      filterControls.enabledBox.checked = filter.enabled;
      const items = filter.sites.map((site) => {
        const remove = buttonElement("Remove");
        remove.addEventListener("click", () => void applyEdit((current) => withVideoFilterSiteRemoved(current, name, site), site));
        const item = document.createElement("li");
        item.append(textElement("span", `${site} `), remove);
        return item;
      });
      filterControls.sitesList.replaceChildren(...(items.length > 0 ? items : [textElement("li", NO_FILTER_SITES_TEXT)]));
    }
  }

  /** Перечитать хранилище и показать (после неверного ввода поле возвращается к хранимому). */
  async function reload(): Promise<void> {
    try {
      render(await readAutoHandoffSettings(dependencies.store));
    } catch (error) {
      status.textContent = `Could not read the settings: ${String(error)}`;
    }
  }

  durationInput.addEventListener("change", () => {
    const text = durationInput.value;
    const seconds = parseMinDurationInput(text);
    if (seconds === undefined) {
      // Неверное число не пишем; поле возвращаем к тому, что хранится, чтобы не показывать несохранённое.
      void reload().then(() => (status.textContent = describeEditRejection("bad_duration", text)));
      return;
    }
    void applyEdit((current) => withMinDuration(current, seconds), text);
  });

  await reload();
  dependencies.onSettingsChanged(render);
}
