// DOM плашки: рисуется в закрытой тени общего слоя. Только вид; решения о показе — в `banner-controller.ts`.
import type { EntryReport } from "../../shared/entry-contract.ts";
import type { BannerView } from "./banner-controller.ts";
import { applyImportantStyles, type UiDocument, type UiElement } from "./ui-dom.ts";
import type { UiLayer } from "./ui-layer.ts";

/** Оформление по тону. Подробная доводка внешнего вида отложена на финальную полировку интерфейса. */
const TONE_COLORS: Readonly<Record<EntryReport["tone"], { readonly background: string; readonly accent: string }>> = {
  success: { background: "#12301f", accent: "#3ecf7a" },
  refusal: { background: "#3a1a14", accent: "#ff8a5c" },
};

const CLOSE_LABEL = "Close";

export function createBannerView(document: UiDocument, layer: UiLayer): BannerView {
  let panel: UiElement | null = null;
  let closeListener: (() => void) | null = null;
  let closeButton: UiElement | null = null;

  function detach(): void {
    if (closeButton !== null && closeListener !== null) closeButton.removeEventListener("click", closeListener);
    closeButton = null;
    closeListener = null;
    panel?.remove();
    panel = null;
  }

  return {
    show(report, onDismissClicked) {
      const shadow = layer.shadowRoot();
      if (shadow === null) return; // документ ещё без корня: показать нечем, сообщение пропадёт тихо, но не навсегда: новое придёт
      detach();
      const colors = TONE_COLORS[report.tone];
      const created = document.createElement("div");
      created.setAttribute("role", report.tone === "refusal" ? "alert" : "status");
      applyImportantStyles(created, {
        all: "initial",
        position: "fixed",
        top: "16px",
        right: "16px",
        "max-width": "360px",
        "box-sizing": "border-box",
        padding: "12px 40px 12px 14px",
        background: colors.background,
        color: "#f4f4f4",
        "border-left": `4px solid ${colors.accent}`,
        "border-radius": "6px",
        font: "14px/1.4 system-ui, sans-serif",
        "pointer-events": "auto",
        display: "block",
      });
      const headline = document.createElement("div");
      headline.textContent = report.headline;
      applyImportantStyles(headline, { display: "block", "font-weight": "600", color: colors.accent });
      created.append(headline);
      for (const line of report.details) {
        const detail = document.createElement("div");
        detail.textContent = line;
        applyImportantStyles(detail, { display: "block", "margin-top": "4px" });
        created.append(detail);
      }
      const close = document.createElement("button");
      close.textContent = "×";
      close.setAttribute("aria-label", CLOSE_LABEL);
      applyImportantStyles(close, {
        all: "initial",
        position: "absolute",
        top: "6px",
        right: "8px",
        font: "20px/1 system-ui, sans-serif",
        color: "#f4f4f4",
        cursor: "pointer",
        display: "block",
      });
      created.append(close);
      closeListener = onDismissClicked;
      closeButton = close;
      close.addEventListener("click", closeListener);
      shadow.append(created);
      panel = created;
      layer.raise();
    },
    hide: detach,
  };
}
