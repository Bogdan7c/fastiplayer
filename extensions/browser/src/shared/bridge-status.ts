// Состояние связи экстеншена с плеером. Контракт общий для фона (источник) и попапа/значка (потребители).
export type BridgeStatus =
  | { readonly state: "unknown" } // ещё не проверяли
  | { readonly state: "checking" } // идёт проверка
  | { readonly state: "host_missing" } // connectNative не нашёл посредника
  | {
      readonly state: "player_unavailable";
      readonly reason: "player_unavailable" | "player_not_responding" | "player_shutting_down" | "no_reply";
    }
  | { readonly state: "incompatible"; readonly update: "extension" | "player" }
  | {
      readonly state: "connected";
      readonly playerVersion: string;
      readonly agreedProtocol: { readonly major: number; readonly minor: number };
    };

export type BridgeStatusState = BridgeStatus["state"];
