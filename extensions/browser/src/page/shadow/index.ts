// «Тень» переданного видеоэлемента (решение 81, устройство — results/00c.md): шов A1↔A2.
//
// Настоящий `<video>` остаётся со своим MediaSource (браузер раскладывает куски, `buffered`,
// `duration`, `videoWidth` — настоящие), но стоит на паузе и заглушен: браузер не декодирует.
// Странице же тень показывает время, паузу, перемотку, readyState, события и счётчики кадров
// по часам Fastiplayer. Плеер сайта думает, что играет сам, и качает дальше.
//
// Как пользоваться (page/shadow-assembly.ts):
//   const originals = captureElementOriginals(window);        // document_start, один раз
//   installShadowOverrides(originals);                        // document_start, прозрачны вне передачи
//   const mediaSession = installMediaSessionGuard(window);    // document_start, один раз
//   const session = startShadow(video, emit, { win: window, originals, mediaSession, now, ... });
//   session.apply(command);  // команды плеера
//   session.end(position, resume); // возврат; наружу уйдёт handoff_ended{returned}
// Пока тень жива, на тиках часов она шлёт пульс `shadow_alive` (решение 180). Снятый источник —
// пауза решения (до EMPTY_SOURCE_GRACE_MS); если страница честно забрала видео (чужой источник,
// пусто дольше допуска, элемент убран из документа), тень снимется сама и пошлёт
// handoff_ended{element_released}.

export { captureElementOriginals, type ElementOriginals } from "./originals.ts";
export type { ShadowEnvironment } from "./shadow-session.ts";
export { startHeldShadow, startShadow, type HeldShadowSession, type ShadowSession } from "./shadow-start.ts";
export { installShadowOverrides } from "./prototype-overrides.ts";
