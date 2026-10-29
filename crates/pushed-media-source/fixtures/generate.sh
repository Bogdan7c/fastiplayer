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
# - muxed-h264-aac.ts — MPEG-TS со звуком и видео (0,4 с).
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
