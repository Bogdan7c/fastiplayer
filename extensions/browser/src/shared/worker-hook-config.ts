// Настройка хука воркера, которую MAIN-мир «запекает» в текст blob-скрипта при создании воркера
// (сессия 09b, пакет C).
//
// Зачем запекать: ответы про кодеки воркер обязан давать с первой строки кода сайта, а спросить
// MAIN-мир асинхронно он не успеет. Поэтому обёртка `Worker` читает действующую политику документа
// синхронно (`PolicyCell.current()`) и подставляет её литералом:
//
//   (function (fastiplayerWorkerHookConfig) { <бандл src/worker/main.ts> })("av1,vp9");
//
// Глобалов не появляется: значение — параметр функции, бандл хука видит его лексически.
// Литерал — только строка из алфавита `encodeRefusedFamilies` (буквы, цифры, запятые): её
// безопасно вставить в код без экранирования, и это проверяется при кодировании.

import {
  decodeRefusedFamilies,
  encodeRefusedFamilies,
  refuse,
  TRANSPARENT_POLICY,
  type CodecPolicy,
} from "./codec-policy.ts";

/** Имя параметра обёртки, под которым бандл хука видит настройку (бандл ссылается на него как на свободное имя). */
export const WORKER_HOOK_CONFIG_PARAMETER = "fastiplayerWorkerHookConfig";

/**
 * Имя второго параметра обёртки: объект-«ссылка» между текстом blob-скрипта и бандлом хука
 * (решение 192). Бандл кладёт в его поле `WORKER_HOOK_SITE_LOADED_FIELD` функцию донесения
 * «скрипт сайта загружен», текст blob-скрипта зовёт её после скрипта сайта. Объект создаёт сам
 * текст blob-скрипта в своей функции (классический) или модуле (модульный) — коду сайта он не виден.
 */
export const WORKER_HOOK_LINKS_PARAMETER = "fastiplayerWorkerHookLinks";

/** Поле объекта-ссылки с функцией донесения «скрипт сайта загружен». */
export const WORKER_HOOK_SITE_LOADED_FIELD = "siteScriptLoaded";

/** Что хук воркера получает от MAIN-мира. */
export interface WorkerHookConfig {
  /** Политика кодеков документа на миг создания воркера. */
  readonly codecPolicy: CodecPolicy;
}

/**
 * Литерал JavaScript для подстановки: строка отказываемых семейств (`""` — прозрачная политика).
 * Бросает, если политика не кодируется в безопасный алфавит (так быть не может; лучше честный сбой
 * сборки текста, чем код с чужими символами).
 */
export function encodeWorkerHookConfig(config: WorkerHookConfig): string {
  const payload = config.codecPolicy.kind === "refuse" ? encodeRefusedFamilies(config.codecPolicy.families) : "";
  if (payload !== "" && decodeRefusedFamilies(payload) === undefined) throw new TypeError("codec policy is not encodable");
  return `"${payload}"`;
}

/**
 * Настройка из значения параметра. Мусор или пусто — прозрачная политика (HLS хук воркера всё равно
 * отвергает: этот отказ от политики не зависит).
 */
export function decodeWorkerHookConfig(value: unknown): WorkerHookConfig {
  if (value === "") return { codecPolicy: TRANSPARENT_POLICY };
  const families = decodeRefusedFamilies(value);
  return { codecPolicy: families === undefined ? TRANSPARENT_POLICY : refuse(families) };
}
