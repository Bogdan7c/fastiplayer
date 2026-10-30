#!/usr/bin/env bash
# Генерация крошечных медиа для тестов источника «сегменты извне» (crate
# pushed-media-source, сессия 03a плана user/browser-bridge/).
#
# Запуск: crates/pushed-media-source/fixtures/generate.sh
# Нужен ffmpeg с libx264, libvpx-vp9 и aac. Источники синтетические (testsrc2 и
# синус): ничего не скачивается, чужого контента нет.
#
# Все видео — 0,8 с при 25 к/с, ключевой кадр каждые 5 кадров, фрагмент fMP4 на
# каждый ключевой кадр: четыре фрагмента с tfdt 0; 0,2; 0,4; 0,6 с. Тесты режут
# файлы на init и фрагменты сами и собирают из них потоки «как от MSE»:
# - video-h264-64.mp4 + video-h264-96.mp4 — смена качества (разрешение 64 → 96);
# - video-h264-64.mp4 + video-vp9-96.mp4 — changeType (H.264 → VP9);
# - audio-aac.mp4 — звук на всю длину (0,8 с, синус 440 Гц);
# - muxed-h264-aac.ts — MPEG-TS со звуком и видео (0,4 с);
# - video-h264-64-4s.mp4 — длинный ролик для полки и обратного давления (сессия
#   03b): 4 с, ключевой кадр каждые 10 кадров, десять фрагментов по 0,4 с;
# - audio-aac-4s.mp4 — звук к нему (4 с, фрагменты ~0,4 с): со звуком старт ведёт
#   звук, и поток с обратным давлением не упирается в «забор» старта только-видео.
# - video-h264-64-midgop.mp4 — перемотка (пакет F 03b): 0,8 с, ключевой кадр каждые
#   10 кадров, а фрагменты по 0,2 с — фрагменты 1 и 3 начинаются НЕ с ключевого
#   кадра (так бывает у страниц, которые режут по времени, а не по GOP).
# - video-vp9-64-4fps.webm — открытый конец кластера (пакет G 03b): WebM VP9 4 к/с,
#   4 с, кластер на каждый ключевой кадр (раз в секунду, 4 блока по 250 мс).
#   ffmpeg пишет `DefaultDuration`; тест заменяет его на `Void` того же размера —
#   так выглядят WebM плееров страниц без длительностей кадров.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
common=(-hide_banner -loglevel error -y)
fmp4=(-fflags +bitexact -map_metadata -1 -movflags empty_moov+default_base_moof+frag_keyframe+skip_trailer)
gop=(-g 5 -keyint_min 5 -sc_threshold 0 -pix_fmt yuv420p)

h264() {
    ffmpeg "${common[@]}" -f lavfi -i "testsrc2=size=$1:rate=25:duration=0.8" \
        -c:v libx264 -preset ultrafast -profile:v baseline -bf 0 "${gop[@]}" \
        "${fmp4[@]}" "$here/$2"
}

h264 64x64 video-h264-64.mp4
h264 96x96 video-h264-96.mp4

ffmpeg "${common[@]}" -f lavfi -i "testsrc2=size=96x96:rate=25:duration=0.8" \
    -c:v libvpx-vp9 -deadline realtime -cpu-used 8 -b:v 50k "${gop[@]}" \
    "${fmp4[@]}" "$here/video-vp9-96.mp4"

ffmpeg "${common[@]}" -f lavfi -i "sine=frequency=440:sample_rate=48000:duration=0.8" \
    -ac 1 -c:a aac -b:a 32k -frag_duration 200000 "${fmp4[@]}" "$here/audio-aac.mp4"

ffmpeg "${common[@]}" -f lavfi -i "testsrc2=size=64x64:rate=25:duration=0.4" \
    -f lavfi -i "sine=frequency=440:sample_rate=48000:duration=0.4" \
    -c:v libx264 -preset ultrafast -profile:v baseline -bf 0 "${gop[@]}" \
    -c:a aac -b:a 32k -ac 1 -fflags +bitexact -map_metadata -1 -f mpegts "$here/muxed-h264-aac.ts"

ffmpeg "${common[@]}" -f lavfi -i "testsrc2=size=64x64:rate=25:duration=4" \
    -c:v libx264 -preset ultrafast -profile:v baseline -bf 0 \
    -g 10 -keyint_min 10 -sc_threshold 0 -pix_fmt yuv420p \
    "${fmp4[@]}" "$here/video-h264-64-4s.mp4"

ffmpeg "${common[@]}" -f lavfi -i "sine=frequency=440:sample_rate=48000:duration=4" \
    -ac 1 -c:a aac -b:a 32k -frag_duration 400000 "${fmp4[@]}" "$here/audio-aac-4s.mp4"

ffmpeg "${common[@]}" -f lavfi -i "testsrc2=size=64x64:rate=25:duration=0.8" \
    -c:v libx264 -preset ultrafast -profile:v baseline -bf 0 \
    -g 10 -keyint_min 10 -sc_threshold 0 -pix_fmt yuv420p \
    -fflags +bitexact -map_metadata -1 -frag_duration 200000 \
    -movflags empty_moov+default_base_moof+skip_trailer "$here/video-h264-64-midgop.mp4"

ffmpeg "${common[@]}" -f lavfi -i "testsrc2=size=64x64:rate=4:duration=4" \
    -c:v libvpx-vp9 -deadline realtime -cpu-used 8 -b:v 50k \
    -g 4 -keyint_min 4 -pix_fmt yuv420p -fflags +bitexact -map_metadata -1 \
    -cluster_time_limit 900 -f webm "$here/video-vp9-64-4fps.webm"

# --- Файл по запросам байтов (сессия 03c моста): обычные, НЕ фрагментированные файлы.
# - file-h264-aac-moov-end.mp4 — оглавление `moov` в конце файла (так пишет ffmpeg по
#   умолчанию; так лежат многие `<video src>`): 4 с, 25 к/с, ключевой кадр раз в 10
#   кадров (0,4 с), звук AAC;
# - file-h264-aac-faststart.mp4 — тот же ролик, `moov` в начале (+faststart);
# - file-vp9-opus.webm — WebM VP9 + Opus (те же 4 с, ключевой раз в 10 кадров);
# - file-h264-cenc.mp4 — тот же ролик, зашифрованный CENC (для отказа «защищённое»).
plain=(-fflags +bitexact -map_metadata -1)
file_video=(-f lavfi -i "testsrc2=size=64x64:rate=25:duration=4")
file_audio=(-f lavfi -i "sine=frequency=440:sample_rate=48000:duration=4")
h264_file=(-c:v libx264 -preset ultrafast -profile:v baseline -bf 0 -g 10 -keyint_min 10
    -sc_threshold 0 -pix_fmt yuv420p -c:a aac -b:a 32k -ac 1)

ffmpeg "${common[@]}" "${file_video[@]}" "${file_audio[@]}" "${h264_file[@]}" \
    "${plain[@]}" "$here/file-h264-aac-moov-end.mp4"

ffmpeg "${common[@]}" "${file_video[@]}" "${file_audio[@]}" "${h264_file[@]}" \
    "${plain[@]}" -movflags +faststart "$here/file-h264-aac-faststart.mp4"

ffmpeg "${common[@]}" "${file_video[@]}" "${file_audio[@]}" \
    -c:v libvpx-vp9 -deadline realtime -cpu-used 8 -b:v 80k -g 10 -keyint_min 10 \
    -pix_fmt yuv420p -c:a libopus -b:a 24k -ac 1 "${plain[@]}" -f webm \
    "$here/file-vp9-opus.webm"

# Ключ и идентификатор — любые 16 байт: ролик только проверяет отказ, не расшифровку.
ffmpeg "${common[@]}" "${file_video[@]}" "${file_audio[@]}" "${h264_file[@]}" \
    "${plain[@]}" -encryption_scheme cenc-aes-ctr \
    -encryption_key 00112233445566778899aabbccddeeff \
    -encryption_kid 112233445566778899aabbccddeeff00 \
    -movflags +faststart "$here/file-h264-cenc.mp4"
