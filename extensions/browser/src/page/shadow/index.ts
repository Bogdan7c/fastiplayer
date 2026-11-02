// «Тень» переданного видеоэлемента (решение 81, устройство — results/00c.md): шов A1↔A2.
//
// Настоящий `<video>` остаётся со своим MediaSource (браузер раскладывает куски, `buffered`,
// `duration`, `videoWidth` — настоящие), но стоит на паузе и заглушен: браузер не декодирует.
// Странице же тень показывает время, паузу, перемотку, readyState, события и счётчики кадров
// по часам Fastiplayer. Плеер сайта думает, что играет сам, и качает дальше.
//
// Как пользоваться (page/main.ts):
//   const originals = captureElementOriginals(window);        // document_start, один раз
//   const mediaSession = installMediaSessionGuard(window);    // document_start, один раз
//   const session = startShadow(video, emit, { win: window, originals, mediaSession, now, ... });
//   session.apply(command);  // команды плеера
//   session.end(position, resume); // возврат; наружу уйдёт handoff_ended{returned}
// Если страница сама бросит элемент (уберёт источник), тень снимется сама и пошлёт
// handoff_ended{element_released}.

export { captureElementOriginals, type ElementOriginals } from "./originals.ts";
export { startShadow, type ShadowEnvironment, type ShadowSession } from "./shadow-session.ts";
