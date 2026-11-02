#!/usr/bin/env bash
# Генерация тестовых роликов стенда моста (сессия 08). Использование: generate.sh <папка-вывода>
#
# Всё делает ffmpeg; питон (стандартная библиотека) нужен только для нарезки/проверки/каталога.
# Результат: <папка>/catalog.json (контракт «версия 1») + файлы роликов рядом.
# Файлы не лежат в git: Rust-модуль `media` вызывает этот скрипт и кэширует результат по sha256
# САМОГО скрипта, поэтому любое изменение скрипта = пересборка медиа.
#
# Инварианты, на которых держится стенд:
#  * GOP = 2 с (25 к/с, -g 50, без детекта смены сцены) — границы сегментов всех качеств совпадают;
#  * сегмент начинается с ключевого кадра, init-сегмент содержит только заголовки (ftyp+moov / EBML+Tracks);
#  * детерминизм: -bitexact, вырезанные метаданные, один поток кодера там, где это не убивает скорость.
set -euo pipefail

OUT_DIR="${1:?usage: generate.sh <output-dir>}"
FRAME_RATE=25
GOP_FRAMES=50          # 2 секунды при 25 к/с
SEGMENT_SECONDS=2
# Фиксированный ключ ClearKey: тестовый, не секрет (лежит и в каталоге, и на странице).
CLEARKEY_KID_HEX="0123456789abcdef0123456789abcdef"
CLEARKEY_KEY_HEX="00112233445566778899aabbccddeeff"

fail() { echo "generate.sh: $*" >&2; exit 1; }

# --- проверка окружения: падаем сразу и понятно, а не на середине ---
command -v ffmpeg >/dev/null || fail "ffmpeg not found in PATH"
command -v ffprobe >/dev/null || fail "ffprobe not found in PATH"
command -v python3 >/dev/null || fail "python3 not found in PATH (needed to split/validate segments)"
ENCODERS="$(ffmpeg -hide_banner -encoders 2>/dev/null)"
for encoder in libx264 aac libvpx-vp9 libopus libsvtav1; do
  grep -qw "$encoder" <<<"$ENCODERS" || fail "ffmpeg encoder '$encoder' is missing"
done
MUXERS="$(ffmpeg -hide_banner -muxers 2>/dev/null)"
for muxer in dash hls mp4 webm; do
  grep -qw "$muxer" <<<"$MUXERS" || fail "ffmpeg muxer '$muxer' is missing"
done

# Шрифт для выжженного номера кадра; без шрифта/drawtext ролики идут без текста (это допустимо).
FONT_FILE="$(fc-match -f '%{file}' monospace 2>/dev/null || true)"
HAVE_DRAWTEXT=0
if [ -n "$FONT_FILE" ] && [ -f "$FONT_FILE" ] && ffmpeg -hide_banner -filters 2>/dev/null | grep -qw drawtext; then
  HAVE_DRAWTEXT=1
else
  echo "generate.sh: warning: no font/drawtext, clips will have no burnt-in text" >&2
fi

mkdir -p "$OUT_DIR"
OUT_DIR="$(cd "$OUT_DIR" && pwd)"
# Если папка уже заполнена — начинаем с чистого листа, чтобы не смешать старые сегменты с новыми.
find "$OUT_DIR" -mindepth 1 -delete

GLOBAL_FLAGS=(-hide_banner -loglevel error -y)
# Опции ВЫХОДА (после -i): детерминизм.
BITEXACT_FLAGS=(-fflags +bitexact -flags:v +bitexact -flags:a +bitexact -map_metadata -1)
# DASH-мультиплексор: init + нумерованные сегменты, без таймлайна (длительности считаем ffprobe-ом).
DASH_COMMON=(-f dash -seg_duration "$SEGMENT_SECONDS" -use_template 1 -use_timeline 0 -remove_at_exit 0)
# Ключевые кадры: закрытый GOP фиксированной длины.
X264_GOP=(-g "$GOP_FRAMES" -keyint_min "$GOP_FRAMES" -sc_threshold 0)

# Файл-реестр дорожек для питона: asset<TAB>track<TAB>kind<TAB>container<TAB>width<TAB>height<TAB>mime_hint
TRACKS_TSV="$OUT_DIR/.tracks.tsv"
: > "$TRACKS_TSV"
# Реестр прочих ассетов: id<TAB>kind<TAB>url<TAB>mime<TAB>duration
ASSETS_TSV="$OUT_DIR/.assets.tsv"
: > "$ASSETS_TSV"
# Реестр длительностей segmented/clearkey ассетов: id<TAB>kind<TAB>duration
register_track() { printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$@" >> "$TRACKS_TSV"; }
register_asset() { printf '%s\t%s\t%s\t%s\t%s\n' "$@" >> "$ASSETS_TSV"; }

# Источник картинки: testsrc2 с выжженным номером кадра и временем.
# $1 размер WxH, $2 длительность, $3 размер шрифта, $4 подпись (необязательно)
video_filter_graph() {
  local size="$1" duration="$2" font_size="$3" label="${4:-}"
  local source="testsrc2=size=${size}:rate=${FRAME_RATE}:duration=${duration}"
  if [ "$HAVE_DRAWTEXT" = 1 ]; then
    local text="${label}%{eif\\:n\\:d\\:5}  %{pts\\:hms}"
    echo "$source,drawtext=fontfile='${FONT_FILE}':text='${text}':fontsize=${font_size}:fontcolor=white:box=1:boxcolor=black@0.6:x=(w-text_w)/2:y=h-(text_h*2)"
  else
    echo "$source"
  fi
}

# Дорожка H.264 в DASH-сегменты. $1 asset, $2 track, $3 WxH, $4 длительность, $5 битрейт, $6 уровень, $7 шрифт
make_h264_track() {
  local asset="$1" track="$2" size="$3" duration="$4" bitrate="$5" level="$6" font="$7"
  local dir="$OUT_DIR/$asset/$track"; mkdir -p "$dir"
  ffmpeg "${GLOBAL_FLAGS[@]}" -f lavfi -i "$(video_filter_graph "$size" "$duration" "$font")" \
    "${BITEXACT_FLAGS[@]}" -an -c:v libx264 -profile:v high -level "$level" -pix_fmt yuv420p \
    -preset veryfast -b:v "$bitrate" -maxrate "$bitrate" -bufsize "$bitrate" -threads 1 "${X264_GOP[@]}" \
    "${DASH_COMMON[@]}" -init_seg_name init.mp4 -media_seg_name 'seg-$Number%05d$.m4s' "$dir/manifest.mpd"
  rm -f "$dir/manifest.mpd"
  register_track "$asset" "$track" video mp4 "${size%x*}" "${size#*x}"
}

# Дорожка AAC в DASH-сегменты. $1 asset, $2 track, $3 частота синуса, $4 длительность
make_aac_track() {
  local asset="$1" track="$2" frequency="$3" duration="$4"
  local dir="$OUT_DIR/$asset/$track"; mkdir -p "$dir"
  ffmpeg "${GLOBAL_FLAGS[@]}" -f lavfi -i "sine=frequency=${frequency}:sample_rate=48000:duration=${duration}" \
    "${BITEXACT_FLAGS[@]}" -vn -ac 2 -c:a aac -b:a 96k -threads 1 \
    "${DASH_COMMON[@]}" -init_seg_name init.mp4 -media_seg_name 'seg-$Number%05d$.m4s' "$dir/manifest.mpd"
  rm -f "$dir/manifest.mpd"
  register_track "$asset" "$track" audio mp4 0 0
}

# Склеенный фрагментированный MP4 (видео+звук или шифрованное видео): режется питоном на init + сегменты.
# Один moof+mdat = один фрагмент = один GOP (frag_keyframe).
FRAGMENT_FLAGS=(-movflags frag_keyframe+empty_moov+default_base_moof)

echo "generate.sh: h264-aac (video 360p/720p + audio, 30 s)"
make_h264_track h264-aac video-360 640x360 30 500k 3.0 36
make_h264_track h264-aac video-720 1280x720 30 1500k 3.1 72
make_aac_track h264-aac audio 440 30

echo "generate.sh: dub-a / dub-b (audio only variants for h264-aac)"
make_aac_track dub-a audio 330 30
make_aac_track dub-b audio 660 30

echo "generate.sh: vp9-opus (WebM, 20 s)"
make_vp9_opus() {
  local asset=vp9-opus dir
  dir="$OUT_DIR/$asset/video-360"; mkdir -p "$dir"
  ffmpeg "${GLOBAL_FLAGS[@]}" -f lavfi -i "$(video_filter_graph 640x360 20 36)" \
    "${BITEXACT_FLAGS[@]}" -an -c:v libvpx-vp9 -pix_fmt yuv420p -b:v 400k -deadline realtime -cpu-used 8 \
    -row-mt 1 -g "$GOP_FRAMES" -keyint_min "$GOP_FRAMES" -auto-alt-ref 0 -lag-in-frames 0 \
    "${DASH_COMMON[@]}" -dash_segment_type webm -init_seg_name init.webm -media_seg_name 'seg-$Number%05d$.chk' "$dir/manifest.mpd"
  rm -f "$dir/manifest.mpd"
  register_track "$asset" video-360 video webm 640 360
  dir="$OUT_DIR/$asset/audio"; mkdir -p "$dir"
  ffmpeg "${GLOBAL_FLAGS[@]}" -f lavfi -i "sine=frequency=523:sample_rate=48000:duration=20" \
    "${BITEXACT_FLAGS[@]}" -vn -ac 2 -c:a libopus -b:a 64k \
    "${DASH_COMMON[@]}" -dash_segment_type webm -init_seg_name init.webm -media_seg_name 'seg-$Number%05d$.chk' "$dir/manifest.mpd"
  rm -f "$dir/manifest.mpd"
  register_track "$asset" audio audio webm 0 0
}
make_vp9_opus

echo "generate.sh: av1-aac (fMP4, 12 s)"
make_av1_aac() {
  local asset=av1-aac dir="$OUT_DIR/av1-aac/video-360"
  mkdir -p "$dir"
  # irefresh-type=2 — закрытый GOP (IDR), чтобы каждый сегмент декодировался сам по себе.
  ffmpeg "${GLOBAL_FLAGS[@]}" -f lavfi -i "$(video_filter_graph 640x360 12 36)" \
    "${BITEXACT_FLAGS[@]}" -an -c:v libsvtav1 -pix_fmt yuv420p -preset 10 -b:v 400k \
    -g "$GOP_FRAMES" -svtav1-params "irefresh-type=2:scd=0" \
    "${DASH_COMMON[@]}" -init_seg_name init.mp4 -media_seg_name 'seg-$Number%05d$.m4s' "$dir/manifest.mpd"
  rm -f "$dir/manifest.mpd"
  register_track "$asset" video-360 video mp4 640 360
  make_aac_track "$asset" audio 392 12
}
make_av1_aac

# Режем один склеенный фрагментированный MP4 в одну дорожку «muxed». $1 asset, $2 вход, $3 kind (muxed|clearkey)
register_muxed_track() {
  local asset="$1" whole="$2"
  local dir="$OUT_DIR/$asset/muxed"; mkdir -p "$dir"
  mv "$whole" "$dir/whole.mp4"
  register_track "$asset" muxed muxed mp4 640 360
}

echo "generate.sh: muxed-h264-aac (one track with video+audio, 20 s)"
mkdir -p "$OUT_DIR/muxed-h264-aac/muxed"
ffmpeg "${GLOBAL_FLAGS[@]}" -f lavfi -i "$(video_filter_graph 640x360 20 36)" \
  -f lavfi -i "sine=frequency=494:sample_rate=48000:duration=20" \
  "${BITEXACT_FLAGS[@]}" -c:v libx264 -profile:v high -level 3.0 -pix_fmt yuv420p -preset veryfast -b:v 500k \
  -threads 1 "${X264_GOP[@]}" -bf 0 -c:a aac -b:a 96k -ac 2 "${FRAGMENT_FLAGS[@]}" -f mp4 "$OUT_DIR/muxed-h264-aac/whole.mp4"
register_muxed_track muxed-h264-aac "$OUT_DIR/muxed-h264-aac/whole.mp4"

echo "generate.sh: clearkey-h264 (cenc-aes-ctr, 6 s)"
mkdir -p "$OUT_DIR/clearkey-h264/muxed"
ffmpeg "${GLOBAL_FLAGS[@]}" -f lavfi -i "$(video_filter_graph 640x360 6 36 'CK ')" \
  "${BITEXACT_FLAGS[@]}" -an -c:v libx264 -profile:v high -level 3.0 -pix_fmt yuv420p -preset veryfast -b:v 500k \
  -threads 1 "${X264_GOP[@]}" -bf 0 -encryption_scheme cenc-aes-ctr -encryption_key "$CLEARKEY_KEY_HEX" \
  -encryption_kid "$CLEARKEY_KID_HEX" "${FRAGMENT_FLAGS[@]}" -f mp4 "$OUT_DIR/clearkey-h264/whole.mp4"
register_muxed_track clearkey-h264 "$OUT_DIR/clearkey-h264/whole.mp4"

echo "generate.sh: progressive-h264-aac (faststart MP4, 12 s)"
mkdir -p "$OUT_DIR/progressive"
ffmpeg "${GLOBAL_FLAGS[@]}" -f lavfi -i "$(video_filter_graph 640x360 12 36)" \
  -f lavfi -i "sine=frequency=440:sample_rate=48000:duration=12" \
  "${BITEXACT_FLAGS[@]}" -c:v libx264 -profile:v high -level 3.0 -pix_fmt yuv420p -preset veryfast -b:v 500k \
  -threads 1 "${X264_GOP[@]}" -c:a aac -b:a 96k -ac 2 -movflags +faststart "$OUT_DIR/progressive/h264-aac.mp4"
register_asset progressive-h264-aac file progressive/h264-aac.mp4 video/mp4 12.0

echo "generate.sh: ad-preroll (5 s, red picture with AD label)"
AD_TEXT=""
if [ "$HAVE_DRAWTEXT" = 1 ]; then
  AD_TEXT=",drawtext=fontfile='${FONT_FILE}':text='AD  %{pts\\:hms}':fontsize=80:fontcolor=white:x=(w-text_w)/2:y=(h-text_h)/2"
fi
ffmpeg "${GLOBAL_FLAGS[@]}" -f lavfi -i "color=c=0xcc2200:size=640x360:rate=${FRAME_RATE}:duration=5${AD_TEXT}" \
  -f lavfi -i "sine=frequency=1000:sample_rate=48000:duration=5" \
  "${BITEXACT_FLAGS[@]}" -c:v libx264 -profile:v high -level 3.0 -pix_fmt yuv420p -preset veryfast -b:v 300k \
  -threads 1 "${X264_GOP[@]}" -c:a aac -b:a 64k -ac 2 -movflags +faststart "$OUT_DIR/progressive/ad.mp4"
register_asset ad-preroll file progressive/ad.mp4 video/mp4 5.0

echo "generate.sh: hls-h264-aac (MPEG-TS segments, 12 s)"
mkdir -p "$OUT_DIR/hls"
ffmpeg "${GLOBAL_FLAGS[@]}" -f lavfi -i "$(video_filter_graph 640x360 12 36 'HLS ')" \
  -f lavfi -i "sine=frequency=440:sample_rate=48000:duration=12" \
  "${BITEXACT_FLAGS[@]}" -map 0:v -map 1:a -c:v libx264 -profile:v high -level 3.0 -pix_fmt yuv420p -preset veryfast \
  -b:v 500k -threads 1 "${X264_GOP[@]}" -c:a aac -b:a 96k -ac 2 \
  -f hls -hls_time "$SEGMENT_SECONDS" -hls_playlist_type vod -hls_flags independent_segments \
  -var_stream_map "v:0,a:0" -master_pl_name master.m3u8 \
  -hls_segment_filename "$OUT_DIR/hls/seg-%05d.ts" "$OUT_DIR/hls/stream.m3u8"
# При var_stream_map ffmpeg подставляет номер потока в имя плейлиста — проверяем, что master на месте.
[ -f "$OUT_DIR/hls/master.m3u8" ] || fail "hls master playlist was not produced"
register_asset hls-h264-aac hls hls/master.m3u8 "" 12.0

echo "generate.sh: subtitles (WebVTT)"
mkdir -p "$OUT_DIR/subtitles"
cat > "$OUT_DIR/subtitles/en.vtt" <<'VTT'
WEBVTT

00:00:01.000 --> 00:00:03.000
Hello from the bench

00:00:04.000 --> 00:00:06.000
Second line of text

00:00:07.000 --> 00:00:09.000
Third line of text

00:00:10.000 --> 00:00:12.000
Last line of the English track
VTT
cat > "$OUT_DIR/subtitles/uk.vtt" <<'VTT'
WEBVTT

00:00:01.000 --> 00:00:03.000
Привіт зі стенда

00:00:04.000 --> 00:00:06.000
Другий рядок

00:00:07.000 --> 00:00:09.000
Останній рядок українською
VTT
register_asset subtitles-en text subtitles/en.vtt text/vtt 0
register_asset subtitles-uk text subtitles/uk.vtt text/vtt 0

echo "generate.sh: splitting, validating and writing catalog.json"
python3 - "$OUT_DIR" "$CLEARKEY_KID_HEX" "$CLEARKEY_KEY_HEX" <<'PY'
import glob, json, os, struct, subprocess, sys, tempfile

out_dir, clearkey_kid, clearkey_key = sys.argv[1:4]


def die(message):
    sys.exit("generate.sh: " + message)


def read_boxes(raw):
    """Верхнеуровневые боксы ISO BMFF: список (тип, начало, конец)."""
    boxes, position = [], 0
    while position < len(raw):
        size, kind = struct.unpack(">I4s", raw[position:position + 8])
        if size == 1:
            size = struct.unpack(">Q", raw[position + 8:position + 16])[0]
        if size < 8 or position + size > len(raw):
            die("broken MP4 box structure")
        boxes.append((kind, position, position + size))
        position += size
    return boxes


def split_fragmented_mp4(track_dir):
    """whole.mp4 -> init.mp4 (ftyp+moov) и seg-NNNNN.m4s (каждый moof+mdat — один GOP)."""
    whole_path = os.path.join(track_dir, "whole.mp4")
    raw = open(whole_path, "rb").read()
    boxes = read_boxes(raw)
    init_end = next((start for kind, start, _ in boxes if kind == b"moof"), None)
    if init_end is None:
        die("no fragments in " + whole_path)
    open(os.path.join(track_dir, "init.mp4"), "wb").write(raw[:init_end])
    fragments = [(s, e) for kind, s, e in boxes if kind in (b"moof", b"mdat")]
    if len(fragments) % 2:
        die("moof/mdat boxes do not pair up in " + whole_path)
    for index in range(0, len(fragments), 2):
        start, end = fragments[index][0], fragments[index + 1][1]
        name = "seg-%05d.m4s" % (index // 2 + 1)
        open(os.path.join(track_dir, name), "wb").write(raw[start:end])
    os.remove(whole_path)

# SystemID ClearKey / W3C Common PSSH (формат инициализационных данных «cenc», W3C EME).
CLEARKEY_SYSTEM_ID = bytes.fromhex("1077efec c0b24d02 ace33c1e 52e2fb4b".replace(" ", ""))


def build_clearkey_pssh(kid_hex):
    """pssh версии 1: size, 'pssh', версия+флаги, SystemID, число KID, сами KID, DataSize=0."""
    body = struct.pack(">I", 0x01000000)  # версия 1, флаги 0
    body += CLEARKEY_SYSTEM_ID
    body += struct.pack(">I", 1) + bytes.fromhex(kid_hex)  # список из одного KID
    body += struct.pack(">I", 0)  # DataSize = 0: данных системы нет
    return struct.pack(">I4s", 8 + len(body), b"pssh") + body


def inject_clearkey_pssh(init_path, kid_hex):
    """Дописывает pssh последним ребёнком moov init-сегмента и поправляет размер moov.

    Без pssh браузеры не шлют событие `encrypted` (в init только tenc), а реальные DRM-сайты шлют.
    Результат детерминирован: те же входные байты дают те же выходные.
    """
    raw = open(init_path, "rb").read()
    moov = next(((s, e) for kind, s, e in read_boxes(raw) if kind == b"moov"), None)
    if moov is None:
        die("no moov in " + init_path)
    start, end = moov
    if struct.unpack(">I", raw[start:start + 4])[0] != end - start:
        die("moov uses 64-bit size, unsupported")
    pssh = build_clearkey_pssh(kid_hex)
    patched = raw[:start] + struct.pack(">I", end - start + len(pssh)) + raw[start + 4:end] + pssh + raw[end:]
    open(init_path, "wb").write(patched)
    # Самопроверка: pssh действительно лежит внутри moov.
    inner = find_box(patched, [b"moov", b"pssh"])
    if inner is None:
        die("pssh was not injected into moov")


def find_box(raw, path):
    """Ищет вложенный бокс по пути типов (moov/trak/mdia/...)."""
    scope = (0, len(raw))
    for wanted in path:
        position, found = scope[0], None
        while position < scope[1]:
            size, kind = struct.unpack(">I4s", raw[position:position + 8])
            if kind == wanted:
                found = (position + 8, position + size)
                break
            position += size
        if found is None:
            return None
        scope = found
    return scope


def sample_entry_codec(init):
    """Строка codecs из sample entry init-сегмента (avc1/av01/mp4a/vp09/opus) + проверка заголовков."""
    codecs = []
    if b"avcC" in init:
        at = init.index(b"avcC") + 4
        codecs.append("avc1.%02x%02x%02x" % (init[at + 1], init[at + 2], init[at + 3]))
    if b"av1C" in init:
        at = init.index(b"av1C") + 4
        profile, level_byte, flags = init[at + 1] >> 5, init[at + 1] & 0x1F, init[at + 2]
        tier = "H" if flags & 0x80 else "M"
        high_bitdepth, twelve = bool(flags & 0x40), bool(flags & 0x20)
        depth = 12 if (high_bitdepth and twelve) else 10 if high_bitdepth else 8
        codecs.append("av01.%d.%02d%s.%02d" % (profile, level_byte, tier, depth))
    if b"mp4a" in init:
        codecs.append("mp4a.40.2")
    return ", ".join(codecs)


def ebml_ids_present(init):
    return init.startswith(b"\x1a\x45\xdf\xa3") and b"\x18\x53\x80\x67" in init and b"\x16\x54\xae\x6b" in init


def probe_time(init_path, seg_path):
    """start/duration сегмента: склеиваем init+сегмент и спрашиваем ffprobe (сегмент один не разбирается)."""
    with tempfile.NamedTemporaryFile(suffix=os.path.splitext(init_path)[1], dir=out_dir) as joined:
        joined.write(open(init_path, "rb").read() + open(seg_path, "rb").read())
        joined.flush()
        result = subprocess.run(
            ["ffprobe", "-v", "error", "-show_entries", "packet=pts_time,duration_time", "-of", "json", joined.name],
            capture_output=True, text=True)
    if result.returncode != 0:
        die("ffprobe failed on %s: %s" % (seg_path, result.stderr.strip()))
    # Контейнер fMP4 с empty_moov не знает общей длительности — берём по пакетам: от первого pts до конца последнего.
    packets = [p for p in json.loads(result.stdout)["packets"] if "pts_time" in p]
    if not packets:
        die("no packets in " + seg_path)
    start = min(float(p["pts_time"]) for p in packets)
    end = max(float(p["pts_time"]) + float(p.get("duration_time", 0)) for p in packets)
    # Первый сегмент AAC начинается с «разгонного» отрицательного времени (-21 мс) — в каталоге честно ставим 0.
    start = max(start, 0.0)
    return start, end - start


def validate_mse_shape(container, init, segments):
    """Проверка формы под MSE byte stream: init без медиа-данных, сегмент начинается с «медиа-единицы»."""
    if container == "mp4":
        kinds = [k for k, _, _ in read_boxes(init)]
        if b"moov" not in kinds or b"moof" in kinds or b"mdat" in kinds:
            die("fMP4 init must contain ftyp/moov and no fragments, got %r" % kinds)
        for path in segments:
            first = read_boxes(open(path, "rb").read())[0][0]
            if first not in (b"moof", b"styp", b"sidx"):
                die("fMP4 segment %s must start with moof/styp, got %r" % (path, first))
    else:
        if not ebml_ids_present(init) or b"\x1f\x43\xb6\x75" in init:
            die("WebM init must be EBML+Segment+Tracks without Cluster")
        for path in segments:
            if not open(path, "rb").read(4) == b"\x1f\x43\xb6\x75":
                die("WebM segment %s must start with a Cluster" % path)


def mime_for(kind, container, init, hint_width):
    if container == "webm":
        return 'video/webm; codecs="vp09.00.21.08"' if kind == "video" else 'audio/webm; codecs="opus"'
    codecs = sample_entry_codec(init)
    if not codecs:
        die("cannot determine codecs of an mp4 init segment")
    prefix = "audio" if kind == "audio" else "video"
    return '%s/mp4; codecs="%s"' % (prefix, codecs)


def build_track(asset, row):
    asset_id, track_id, kind, container, width, height = row
    track_dir = os.path.join(out_dir, asset_id, track_id)
    if track_id == "muxed":
        split_fragmented_mp4(track_dir)
        if asset_id == "clearkey-h264":
            inject_clearkey_pssh(os.path.join(track_dir, "init.mp4"), clearkey_kid)
    init_name = "init." + container
    init_path = os.path.join(track_dir, init_name)
    init = open(init_path, "rb").read()
    segment_paths = sorted(glob.glob(os.path.join(track_dir, "seg-*")))
    if not segment_paths:
        die("no segments for %s/%s" % (asset_id, track_id))
    validate_mse_shape(container, init, segment_paths)
    segments = []
    for path in segment_paths:
        start, duration = probe_time(init_path, path)
        segments.append({"url": "%s/%s/%s" % (asset_id, track_id, os.path.basename(path)),
                         "start_s": round(start, 3), "duration_s": round(duration, 3)})
    mime = mime_for(kind, container, init, width)
    if asset_id == "h264-aac" and kind == "video" and "avc1.6400" not in mime:
        die("unexpected H.264 profile in %s: %s" % (track_id, mime))
    track = {"id": track_id, "kind": kind, "mime": mime}
    if kind != "audio":
        track["width"], track["height"] = int(width), int(height)
    track["init"] = "%s/%s/%s" % (asset_id, track_id, init_name)
    track["segments"] = segments
    return track


durations = {"h264-aac": 30.0, "dub-a": 30.0, "dub-b": 30.0, "vp9-opus": 20.0, "av1-aac": 12.0,
             "muxed-h264-aac": 20.0, "clearkey-h264": 6.0}
track_rows = [line.rstrip("\n").split("\t") for line in open(os.path.join(out_dir, ".tracks.tsv"))]
assets = {}
for row in track_rows:
    asset_id = row[0]
    entry = assets.setdefault(asset_id, {"kind": "clearkey" if asset_id == "clearkey-h264" else "segmented",
                                         "duration_s": durations[asset_id]})
    if asset_id == "clearkey-h264":
        entry["key_id_hex"], entry["key_hex"] = clearkey_kid, clearkey_key
    entry.setdefault("tracks", []).append(build_track(asset_id, row))

# Проверяем заявленную длительность: последний сегмент не может кончаться раньше/позже больше чем на 0.5 с.
for asset_id, entry in assets.items():
    for track in entry["tracks"]:
        last = track["segments"][-1]
        end = last["start_s"] + last["duration_s"]
        if abs(end - entry["duration_s"]) > 0.5:
            die("%s/%s ends at %.3f s, expected about %.1f s" % (asset_id, track["id"], end, entry["duration_s"]))
# Границы сегментов видео разных качеств обязаны совпадать (иначе ABR склеится с разрывом).
v360 = [s["start_s"] for s in next(t for t in assets["h264-aac"]["tracks"] if t["id"] == "video-360")["segments"]]
v720 = [s["start_s"] for s in next(t for t in assets["h264-aac"]["tracks"] if t["id"] == "video-720")["segments"]]
if v360 != v720:
    die("video-360 and video-720 segment boundaries differ: %r vs %r" % (v360, v720))

for line in open(os.path.join(out_dir, ".assets.tsv")):
    asset_id, kind, url, mime, duration = line.rstrip("\n").split("\t")
    if not os.path.isfile(os.path.join(out_dir, url)):
        die("missing file " + url)
    entry = {"kind": kind, "url": url}
    if kind == "file":
        entry.update({"mime": mime, "duration_s": float(duration)})
    elif kind == "hls":
        entry["duration_s"] = float(duration)
        playlist = open(os.path.join(out_dir, url)).read()
        if "#EXT-X-STREAM-INF" not in playlist:
            die("hls master playlist has no variants")
        for line_text in playlist.splitlines():
            if line_text and not line_text.startswith("#") and not os.path.isfile(os.path.join(out_dir, "hls", line_text)):
                die("hls master references missing playlist " + line_text)
    else:
        entry["mime"] = mime
    assets[asset_id] = entry

for hidden in (".tracks.tsv", ".assets.tsv"):
    os.remove(os.path.join(out_dir, hidden))
with open(os.path.join(out_dir, "catalog.json"), "w", encoding="utf-8") as handle:
    json.dump({"version": 1, "assets": dict(sorted(assets.items()))}, handle, indent=1, ensure_ascii=False)
    handle.write("\n")
print("generate.sh: wrote %s/catalog.json (%d assets)" % (out_dir, len(assets)))
PY
