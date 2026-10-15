/*
 * Генератор эталонов для порта растеризатора субтитров (crates/subtitle-raster).
 *
 * Рендерит один кадр .ass системным libass и пишет в stdout бинарный список ASS_Image.
 * Инструмент не входит в сборку проекта и CI: его запускает scripts/subtitle-reference/generate.py,
 * а результат коммитится как фикстура.
 *
 * Использование:
 *   render_reference <track.ass> <fonts_dir> <width> <height> <time_ms>
 *
 * Формат stdout (все числа little-endian, как на хосте x86-64/aarch64):
 *   u32 версия libass (ass_library_version)
 *   i32 число картинок
 *   для каждой картинки:
 *     i32 type, i32 dst_x, i32 dst_y, i32 w, i32 h, i32 stride, u32 color (0xRRGGBBAA,
 *     где AA = 255 - alpha, как в libass), затем h строк по w байт маски (без хвоста stride)
 */
#include <ass/ass.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

/* Подавляем сообщения libass: детерминированный stdout важнее диагностики. */
static void silent_log(int level, const char *fmt, va_list args, void *data)
{
    (void) level; (void) fmt; (void) args; (void) data;
}

static void write_i32(int32_t value)
{
    fwrite(&value, sizeof(value), 1, stdout);
}

int main(int argc, char **argv)
{
    if (argc != 6) {
        fprintf(stderr, "usage: %s track.ass fonts_dir width height time_ms\n", argv[0]);
        return 2;
    }
    const char *track_path = argv[1];
    const char *fonts_dir = argv[2];
    int width = atoi(argv[3]);
    int height = atoi(argv[4]);
    long long time_ms = atoll(argv[5]);

    ASS_Library *library = ass_library_init();
    if (!library) {
        fprintf(stderr, "ass_library_init failed\n");
        return 1;
    }
    ass_set_message_cb(library, silent_log, NULL);
    /* Каталог с вложенными шрифтами: провайдер NONE берёт шрифты только оттуда
       (ass_fontselect.c: ass_embedded_fonts_add_provider -> load_fonts_from_dir). */
    ass_set_fonts_dir(library, fonts_dir);

    ASS_Renderer *renderer = ass_renderer_init(library);
    if (!renderer) {
        fprintf(stderr, "ass_renderer_init failed\n");
        return 1;
    }
    ass_set_frame_size(renderer, width, height);
    ass_set_storage_size(renderer, width, height);
    ass_set_hinting(renderer, ASS_HINTING_NONE);
    ass_set_shaper(renderer, ASS_SHAPING_COMPLEX);
    /* Никаких системных шрифтов: детерминизм между машинами. */
    ass_set_fonts(renderer, NULL, "Noto Sans", ASS_FONTPROVIDER_NONE, NULL, 0);

    ASS_Track *track = ass_read_file(library, (char *) track_path, NULL);
    if (!track) {
        fprintf(stderr, "cannot read track %s\n", track_path);
        return 1;
    }

    int changed = 0;
    ASS_Image *image = ass_render_frame(renderer, track, time_ms, &changed);

    int32_t count = 0;
    for (ASS_Image *it = image; it; it = it->next)
        count++;

    uint32_t version = (uint32_t) ass_library_version();
    fwrite(&version, sizeof(version), 1, stdout);
    write_i32(count);
    for (ASS_Image *it = image; it; it = it->next) {
        write_i32(it->type);
        write_i32(it->dst_x);
        write_i32(it->dst_y);
        write_i32(it->w);
        write_i32(it->h);
        write_i32(it->stride);
        uint32_t color = it->color;
        fwrite(&color, sizeof(color), 1, stdout);
        for (int row = 0; row < it->h; row++)
            fwrite(it->bitmap + (size_t) row * it->stride, 1, (size_t) it->w, stdout);
    }

    ass_free_track(track);
    ass_renderer_done(renderer);
    ass_library_done(library);
    return 0;
}
