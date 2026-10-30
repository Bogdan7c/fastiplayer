#!/usr/bin/env bash
# Генерация крошечных медиа для записанных потоков протокола моста
# (crates/browser-bridge-protocol/fixtures/streams/).
#
# Запуск: crates/browser-bridge-protocol/fixtures/media/generate.sh
# Требуется ffmpeg с libx264, aac, libvpx-vp9 и libopus. Источники синтетические
# (testsrc2 и синус), ничего не скачивается и чужого контента нет.
#
# Что получается — то же, что плееры страниц кладут в SourceBuffer (results/00.md):
# - video.mp4 — fMP4 H.264, только видео (как видео-SourceBuffer YouTube/Tortuga);
# - audio.mp4 — fMP4 AAC, только звук (звуковой SourceBuffer);
# - muxed.webm — WebM VP9 + Opus в одном файле (один SourceBuffer на звук и видео,
#   как у PeerTube и Twitch live).
# После генерации пересоберите потоки: fixtures/generate_fixtures.py.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
common=(-hide_banner -loglevel error -y)
fmp4=(-fflags +bitexact -map_metadata -1 -movflags empty_moov+default_base_moof+frag_keyframe+skip_trailer)

# Видео: 10 кадров 64×64 @ 25 fps, ключевой кадр каждые 5 → два фрагмента moof.
ffmpeg "${common[@]}" -f lavfi -i "testsrc2=size=64x64:rate=25:duration=0.4" \
    -c:v libx264 -preset ultrafast -profile:v baseline -bf 0 -g 5 -keyint_min 5 \
    -sc_threshold 0 -pix_fmt yuv420p "${fmp4[@]}" "$here/video.mp4"

# Звук: AAC-LC моно 48 кГц, 0,4 с.
ffmpeg "${common[@]}" -f lavfi -i "sine=frequency=440:sample_rate=48000:duration=0.4" \
    -ac 1 -c:a aac -b:a 32k -frag_duration 200000 "${fmp4[@]}" "$here/audio.mp4"

# Звук и видео вместе: WebM VP9 + Opus, 0,4 с.
ffmpeg "${common[@]}" -f lavfi -i "testsrc2=size=64x64:rate=25:duration=0.4" \
    -f lavfi -i "sine=frequency=660:sample_rate=48000:duration=0.4" \
    -c:v libvpx-vp9 -deadline realtime -cpu-used 8 -b:v 50k -g 5 -pix_fmt yuv420p \
    -c:a libopus -b:a 24k -ac 1 -fflags +bitexact -map_metadata -1 -f webm "$here/muxed.webm"

# Смена качества новым MediaSource (сессия 03e, поток streams/quality-switch-new-media-source):
# один и тот же 6-секундный ролик в двух качествах (64 и 96 px) и звук к нему. Ключевой
# кадр каждые 1,2 с (длинный GOP, как у YouTube) → фрагмент moof на каждый (5 фрагментов),
# звук — фрагменты по ~1 с.
# Так страница после смены качества кладёт коробки с ближайшего ключевого кадра ДО своей
# позиции, а звук — с ещё более раннего фрагмента (как YouTube, results/03e.md).
for size in 64 96; do
    ffmpeg "${common[@]}" -f lavfi -i "testsrc2=size=${size}x${size}:rate=25:duration=6" \
        -c:v libx264 -preset ultrafast -profile:v baseline -bf 0 -g 30 -keyint_min 30 \
        -sc_threshold 0 -crf 40 -pix_fmt yuv420p "${fmp4[@]}" "$here/video-${size}-6s.mp4"
done
ffmpeg "${common[@]}" -f lavfi -i "sine=frequency=440:sample_rate=48000:duration=6" \
    -ac 1 -c:a aac -b:a 24k -frag_duration 1000000 "${fmp4[@]}" "$here/audio-6s.mp4"
