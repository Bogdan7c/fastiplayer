#!/usr/bin/env bash
# Генерация фикстур для тестов своего `get_buffer2` (cpu-parity 08b):
# `crates/video-ffmpeg/src/ffi/direct_frame_buffers/tests.rs`.
#
# Запуск: crates/video-ffmpeg/tests/fixtures/direct-frame-buffers/generate.sh
# Требуется ffmpeg с libx264, libx265 (с 10/12 бит), libvpx (VP8 и VP9 с high bit depth) и libaom-av1. Тесты ffmpeg не запускают: они читают
# закоммиченные файлы. Источник синтетический (testsrc2), ничего не скачивается.
#
# Размер 176x100 выбран нарочно: ширина не кратна 256, поэтому наш `get_buffer2`
# расширяет stride (176 -> 512 / 256), а высота даёт нецелую chroma-сетку кодека.
# Каждый файл — 12 кадров с ключевым кадром каждые 6: есть и intra-, и inter-кадры,
# B-кадры H.264/HEVC держат ссылки (frame threading декодера делит их между потоками).
#
# Форматы выбраны так, чтобы тест резал поток на пакеты без демуксера:
# - VP9 — IVF (32-байтный заголовок файла, 12-байтный заголовок кадра);
# - H.264/HEVC — Annex B с AUD перед каждым access unit (`*_metadata=aud=insert`):
#   пакет = байты от одного AUD до следующего.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
source=(-f lavfi -i "testsrc2=duration=2.4:size=176x100:rate=5" -vf format=yuv420p)

ffmpeg -hide_banner -loglevel error -y "${source[@]}" \
    -c:v libvpx-vp9 -deadline realtime -cpu-used 8 -crf 40 -b:v 0 -g 6 \
    -f ivf "$here/vp9-176x100-12frames.ivf"

ffmpeg -hide_banner -loglevel error -y "${source[@]}" \
    -c:v libx264 -preset veryfast -crf 35 -g 6 -bf 2 \
    -bsf:v h264_metadata=aud=insert \
    -f h264 "$here/h264-176x100-12frames.h264"

ffmpeg -hide_banner -loglevel error -y "${source[@]}" \
    -c:v libx265 -preset veryfast -crf 35 -x265-params "keyint=6:bframes=2:log-level=error" \
    -bsf:v hevc_metadata=aud=insert \
    -f hevc "$here/hevc-176x100-12frames.hevc"

# VP9 profile 2, 10 бит (yuv420p10le): тот же источник, размер, число кадров и IVF, что у 8-битного.
# Нужен сквозному тесту `render-gles2-shell/tests/ffmpeg_decoder_to_shell.rs` (10-битный кадр
# в memfd-блоках доходит до экрана GLES2 без CPU-копии и совпадает с путём копии).
ffmpeg -hide_banner -loglevel error -y -f lavfi -i "testsrc2=duration=2.4:size=176x100:rate=5" \
    -vf format=yuv420p10le \
    -c:v libvpx-vp9 -profile:v 2 -pix_fmt yuv420p10le -deadline realtime -cpu-used 8 -crf 40 -b:v 0 -g 6 \
    -f ivf "$here/vp9-10bit-176x100-12frames.ivf"

# AV1 (libaom, IVF: кадр IVF = один temporal unit) — сессия 13 плана gles2-renderer: патч FFmpeg
# даёт обёртке libdav1d `AV_CODEC_CAP_DR1`, и dav1d пишет кадры в наши блоки.
# - 8 бит 4:2:0 (AV1 Main);
# - 10 бит 4:2:0 (yuv420p10le) — dav1d выделяет 16-битные плоскости;
# - 8 бит с зерном плёнки (`film-grain-test=1`: тестовая таблица libaom кладёт параметры зерна в
#   каждый кадр). dav1d накладывает зерно во ВТОРУЮ картинку, тоже из нашего get_buffer2.
# 176 не кратно 128: dav1d просит память под ширину/высоту, дополненные до 128.
av1_source=(-f lavfi -i "testsrc2=duration=2.4:size=176x100:rate=5")
av1_encoder=(-c:v libaom-av1 -cpu-used 8 -crf 40 -g 6 -still-picture 0)

ffmpeg -hide_banner -loglevel error -y "${av1_source[@]}" -vf format=yuv420p \
    "${av1_encoder[@]}" -f ivf "$here/av1-176x100-12frames.ivf"

ffmpeg -hide_banner -loglevel error -y "${av1_source[@]}" -vf format=yuv420p10le \
    "${av1_encoder[@]}" -pix_fmt yuv420p10le -f ivf "$here/av1-10bit-176x100-12frames.ivf"

ffmpeg -hide_banner -loglevel error -y "${av1_source[@]}" -vf format=yuv420p \
    "${av1_encoder[@]}" -aom-params film-grain-test=1 -f ivf "$here/av1-filmgrain-176x100-12frames.ivf"

# Матрица сессии 13a плана gles2-renderer: каждая раскладка, которую играет программный путь
# плеера, — сквозной тест `tests/direct_frame_upload_wgpu/` (кадр доходит до экрана wgpu
# из памяти декодера без CPU-копии). Тот же источник, размер и число кадров.
matrix_source=(-f lavfi -i "testsrc2=duration=2.4:size=176x100:rate=5")

# VP8 (IVF `VP80`): только 8 бит 4:2:0.
ffmpeg -hide_banner -loglevel error -y "${matrix_source[@]}" -vf format=yuv420p \
    -c:v libvpx -deadline realtime -cpu-used 8 -crf 40 -b:v 1M -g 6 \
    -f ivf "$here/vp8-176x100-12frames.ivf"

# VP9 профили 1 (8 бит 4:2:2/4:4:4), 2 (12 бит 4:2:0) и 3 (10/12 бит 4:2:2, 10 бит 4:4:4).
vp9_matrix_encoder=(-c:v libvpx-vp9 -deadline realtime -cpu-used 8 -crf 40 -b:v 0 -g 6)
for spec in "1:yuv422p:422" "1:yuv444p:444" "2:yuv420p12le:12bit" \
    "3:yuv422p10le:422-10bit" "3:yuv422p12le:422-12bit" "3:yuv444p10le:444-10bit"; do
    IFS=: read -r profile pixel_format name <<< "$spec"
    ffmpeg -hide_banner -loglevel error -y "${matrix_source[@]}" -vf "format=$pixel_format" \
        "${vp9_matrix_encoder[@]}" -profile:v "$profile" -pix_fmt "$pixel_format" \
        -f ivf "$here/vp9-$name-176x100-12frames.ivf"
done

# HEVC Main10, Main12 и RExt (4:2:2 10/12 бит, 4:4:4 8/10 бит): Annex B с AUD, как 8-битный.
for spec in "main10:yuv420p10le:10bit" "main12:yuv420p12le:12bit" \
    "main422-10:yuv422p10le:422-10bit" "main422-12:yuv422p12le:422-12bit" \
    "main444-8:yuv444p:444" "main444-10:yuv444p10le:444-10bit"; do
    IFS=: read -r profile pixel_format name <<< "$spec"
    ffmpeg -hide_banner -loglevel error -y "${matrix_source[@]}" -vf "format=$pixel_format" \
        -c:v libx265 -preset veryfast -crf 35 -pix_fmt "$pixel_format" -profile:v "$profile" \
        -x265-params "keyint=6:bframes=2:log-level=error" \
        -bsf:v hevc_metadata=aud=insert \
        -f hevc "$here/hevc-$name-176x100-12frames.hevc"
done

# AV1 High: 4:4:4 8 и 10 бит.
for spec in "yuv444p:444" "yuv444p10le:444-10bit"; do
    IFS=: read -r pixel_format name <<< "$spec"
    ffmpeg -hide_banner -loglevel error -y "${matrix_source[@]}" -vf "format=$pixel_format" \
        "${av1_encoder[@]}" -pix_fmt "$pixel_format" -f ivf "$here/av1-$name-176x100-12frames.ivf"
done
