// Раздел «Remembered choices» на странице настроек (сессия 14b2, решения 281, 285): сколько сериалов помнить,
// сколько запомнено сейчас и кнопка «Forget remembered choices».
//
// Объём страница пишет сама (это обычная настройка, `shared/choice-memory-settings.ts`). Память же целиком
// принадлежит фону: число записей и «Forget» страница просит у фона сообщением (`choice-memory-commands.ts`),
// поэтому «Forget» не разминётся с записью, которая идёт в этот момент.
import {
  COUNT_REMEMBERED_CHOICES_KIND,
  FORGET_REMEMBERED_CHOICES_KIND,
  parseChoiceMemoryReply,
  type ChoiceMemoryReply,
  type ChoiceMemoryRequest,
} from "../shared/choice-memory-contract.ts";
import {
  MAX_CHOICE_MEMORY_CAPACITY,
  MIN_CHOICE_MEMORY_CAPACITY,
  parseChoiceMemoryCapacityInput,
  readChoiceMemorySettings,
  writeChoiceMemorySettings,
} from "../shared/choice-memory-settings.ts";
import type { KeyValueStore } from "../shared/codec-storage.ts";

/** Пояснение раздела: что помнится, как хранится и что с приватными окнами (честный текст, решение 281). */
export const CHOICE_MEMORY_EXPLANATION =
  "For each series, the extension remembers the dub, quality and subtitles you pick, and sets them again on the next episode. " +
  "A series is stored only as an irreversible fingerprint of the site and the series, never as its name or address. " +
  "Private windows use remembered choices but never save new ones.";

export const CAPACITY_HINT = `Enter a whole number from ${MIN_CHOICE_MEMORY_CAPACITY} to ${MAX_CHOICE_MEMORY_CAPACITY}.`;

export interface ChoiceMemoryControlDependencies {
  readonly store: KeyValueStore;
  /** Спросить фон (`runtime.sendMessage`); ответ разбирается здесь. */
  readonly askBackground: (request: ChoiceMemoryRequest) => Promise<unknown>;
}

/** Текст числа запомненных сериалов. */
export function describeRememberedCount(series: number): string {
  return series === 1 ? "1 series remembered." : `${series} series remembered.`;
}

/** Спросить фон и разобрать ответ; чужой ответ — `failed`, а не тишина. */
async function ask(dependencies: ChoiceMemoryControlDependencies, request: ChoiceMemoryRequest): Promise<ChoiceMemoryReply> {
  try {
    return parseChoiceMemoryReply(await dependencies.askBackground(request)) ?? { kind: "failed", reason: "the extension gave no answer" };
  } catch (error) {
    return { kind: "failed", reason: String(error) };
  }
}

/** Нарисовать раздел в `container` и связать его с хранилищем и фоном. Ошибки показываем текстом, не глотаем. */
export async function mountChoiceMemoryControl(container: HTMLElement, dependencies: ChoiceMemoryControlDependencies): Promise<void> {
  const heading = document.createElement("h2");
  heading.textContent = "Remembered choices";
  const explanation = document.createElement("p");
  explanation.textContent = CHOICE_MEMORY_EXPLANATION;
  const capacityLabel = document.createElement("label");
  const capacityInput = document.createElement("input");
  capacityInput.type = "number";
  capacityInput.id = "choice-memory-capacity";
  capacityInput.min = String(MIN_CHOICE_MEMORY_CAPACITY);
  capacityInput.max = String(MAX_CHOICE_MEMORY_CAPACITY);
  const capacityText = document.createElement("span");
  capacityText.textContent = "Series to remember: ";
  capacityLabel.append(capacityText, capacityInput);
  const count = document.createElement("p");
  count.id = "choice-memory-count";
  const forgetButton = document.createElement("button");
  forgetButton.type = "button";
  forgetButton.id = "choice-memory-forget";
  forgetButton.textContent = "Forget remembered choices";
  const status = document.createElement("p");
  status.id = "choice-memory-status";
  container.append(heading, explanation, capacityLabel, count, forgetButton, status);

  async function showCount(): Promise<void> {
    const reply = await ask(dependencies, { kind: COUNT_REMEMBERED_CHOICES_KIND });
    count.textContent = reply.kind === "count" ? describeRememberedCount(reply.series) : `Could not count the remembered series: ${failureOf(reply)}`;
  }

  try {
    capacityInput.value = String((await readChoiceMemorySettings(dependencies.store)).capacity);
  } catch (error) {
    status.textContent = `Could not read the settings: ${String(error)}`;
  }
  await showCount();

  capacityInput.addEventListener("change", () => {
    const capacity = parseChoiceMemoryCapacityInput(capacityInput.value);
    if (capacity === undefined) {
      status.textContent = CAPACITY_HINT;
      return;
    }
    writeChoiceMemorySettings(dependencies.store, { capacity }).then(
      () => (status.textContent = "Saved. A smaller limit applies from the next remembered choice."),
      (error: unknown) => (status.textContent = `Could not save: ${String(error)}`),
    );
  });

  forgetButton.addEventListener("click", () => {
    void ask(dependencies, { kind: FORGET_REMEMBERED_CHOICES_KIND }).then(async (reply) => {
      status.textContent = reply.kind === "forgotten" ? "Remembered choices are forgotten." : `Could not forget: ${failureOf(reply)}`;
      await showCount();
    });
  });
}

/** Причина отказа фона для текста; неожиданный вид ответа — тоже отказ. */
function failureOf(reply: ChoiceMemoryReply): string {
  return reply.kind === "failed" ? reply.reason : `unexpected answer: ${reply.kind}`;
}
