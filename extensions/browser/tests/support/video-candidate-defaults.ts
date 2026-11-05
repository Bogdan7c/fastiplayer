// Значения по умолчанию для фактов кандидата видео (09c), чтобы тесты, которым важны только
// форма источника, «играет» и площадь, не перечисляли все остальные поля.
import type { VideoCandidate } from "../../src/shared/handoff-contract.ts";

/** Факты обычного фильма: со звуком, без цикла, без DRM, хост документа и размер кадра известны. */
export const DEFAULT_CANDIDATE_FACTS: Omit<VideoCandidate, "document" | "element" | "media" | "playing" | "visible_area_px"> = {
  duration_s: 5400,
  muted: false,
  loop: false,
  video_width_px: 1280,
  video_height_px: 720,
  drm: false,
  frame_host: "site.example",
  file_host: null,
  source_mime_types: [],
};
