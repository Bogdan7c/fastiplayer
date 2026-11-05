// Факты кандидата, которые отдаёт ФЕЙКОВАЯ страница по умолчанию (`fake-page.ts`): видео 640x360,
// метаданных нет (`duration` = NaN), со звуком, без цикла и DRM, документ на `site.example`.
// Тест, которому важна одна пара фактов, берёт эту основу и перекрывает нужные поля.
import type { VideoCandidate } from "../../../src/shared/handoff-contract.ts";

export const PAGE_FACTS_OF_UNKNOWN_DURATION_VIDEO: Omit<VideoCandidate, "document" | "element" | "media" | "playing" | "visible_area_px"> = {
  duration_s: null,
  muted: false,
  loop: false,
  video_width_px: 640,
  video_height_px: 360,
  drm: false,
  frame_host: "site.example",
  file_host: null,
  source_mime_types: [],
};
