// Текст blob-скриптов, которыми обёртка `Worker` заменяет скрипт сайта (сессия 09b, решения 63 и
// 192). Чистые функции без состояния: на входе текст хука, литерал настройки и литерал адреса.
//
// Оборачиваются только воркеры из blob-адреса страницы (решение 192): `<адрес скрипта сайта>`
// ниже — всегда blob-адрес того же происхождения.
//
// Классический воркер (так делают Twitch/IVS и hls.js) — один скрипт:
//   (function () {
//     var fastiplayerWorkerHookLinks = { siteScriptLoaded: function () {} };
//     (function (fastiplayerWorkerHookConfig, fastiplayerWorkerHookLinks) { <хук> })(<настройка>, fastiplayerWorkerHookLinks);
//     try { importScripts(<адрес скрипта сайта>); } finally { fastiplayerWorkerHookLinks.siteScriptLoaded(); }
//   })();
// `importScripts` синхронный: код сайта выполняется ровно после хука, в той же задаче, и в
// глобальной области воркера (обёртка-функция на него не влияет). Объект-ссылка живёт только в
// замыкании внешней функции: код сайта его не видит. `finally` не меняет исход скрипта сайта:
// исключение `importScripts` уходит дальше как было, донесение «загружен» лишь не бросает.
//
// Модульный воркер (`{ type: "module" }`) — два модуля:
//   хук:  const fastiplayerWorkerHookLinks = {…}; (function (…) { <хук> })(<настройка>, fastiplayerWorkerHookLinks);
//         export function fastiplayerSiteScriptLoaded() { fastiplayerWorkerHookLinks.siteScriptLoaded(); }
//   вход: import { fastiplayerSiteScriptLoaded } from <адрес хука>; import <адрес скрипта сайта>;
//         fastiplayerSiteScriptLoaded();
// Статические импорты выполняются в порядке их записи (модуль целиком, со своими зависимостями,
// раньше следующего), а тело входа — после всех импортов. Поэтому хук нельзя вставить прямо во
// вход (импорты поднимаются и выполнились бы РАНЬШЕ него), а донесение «скрипт сайта загружен» —
// как раз тело входа. `import.meta.url` у модуля сайта остаётся его собственным адресом.
//
// Адреса и настройку подставляет вызывающий уже литералами (JSON-строки, `JSON.stringify` снят на
// document_start): в текст не попадает ни одного символа, который мог бы разорвать строку.

import { WORKER_HOOK_CONFIG_PARAMETER, WORKER_HOOK_LINKS_PARAMETER, WORKER_HOOK_SITE_LOADED_FIELD } from "../../shared/worker-hook-config.ts";

/** Хук для подстановки: текст бандла и литерал его настройки. */
export interface HookScript {
  /** Текст IIFE-бандла `src/worker/main.ts`. */
  readonly source: string;
  /** Литерал JavaScript настройки (`encodeWorkerHookConfig`). */
  readonly configLiteral: string;
}

/** Имя экспорта модуля хука: донесение «скрипт сайта загружен» для входного модуля. */
const MODULE_SITE_LOADED_EXPORT = "fastiplayerSiteScriptLoaded";

/** Объявление объекта-ссылки с безопасной заглушкой (хук не встал — донесения просто нет). */
function linksDeclaration(keyword: "var" | "const"): string {
  return `${keyword} ${WORKER_HOOK_LINKS_PARAMETER} = { ${WORKER_HOOK_SITE_LOADED_FIELD}: function () {} };\n`;
}

/** Вызов хука с настройкой и объектом-ссылкой: функция-обёртка с двумя параметрами, без глобалов. */
export function hookInvocation(hook: HookScript): string {
  return (
    `(function (${WORKER_HOOK_CONFIG_PARAMETER}, ${WORKER_HOOK_LINKS_PARAMETER}) {\n${hook.source}\n})` +
    `(${hook.configLiteral}, ${WORKER_HOOK_LINKS_PARAMETER});\n`
  );
}

/** Вызов донесения «скрипт сайта загружен» (объект-ссылка — в той же области видимости). */
const SITE_LOADED_CALL = `${WORKER_HOOK_LINKS_PARAMETER}.${WORKER_HOOK_SITE_LOADED_FIELD}();\n`;

/** Скрипт классического воркера: хук, затем скрипт сайта через `importScripts`, затем донесение. */
export function classicWorkerScript(hook: HookScript, scriptUrlLiteral: string): string {
  return (
    `(function () {\n${linksDeclaration("var")}${hookInvocation(hook)}` +
    `try {\nimportScripts(${scriptUrlLiteral});\n} finally {\n${SITE_LOADED_CALL}}\n})();\n`
  );
}

/** Модуль хука для модульного воркера (тот же вызов; модуль выполняется в строгом режиме). */
export function moduleHookScript(hook: HookScript): string {
  return `${linksDeclaration("const")}${hookInvocation(hook)}export function ${MODULE_SITE_LOADED_EXPORT}() {\n${SITE_LOADED_CALL}}\n`;
}

/** Входной модуль модульного воркера: модуль хука, модуль сайта, затем донесение «загружен». */
export function moduleEntryScript(hookUrlLiteral: string, scriptUrlLiteral: string): string {
  return `import { ${MODULE_SITE_LOADED_EXPORT} } from ${hookUrlLiteral};\nimport ${scriptUrlLiteral};\n${MODULE_SITE_LOADED_EXPORT}();\n`;
}
