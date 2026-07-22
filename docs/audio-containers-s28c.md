# S28C: current audio-container proof

## Граница реализации

S28C не добавляет новые codec families и не создаёт второй parser. Existing
`symphonia-demux` остаётся владельцем sniff/open, track metadata, codec-private,
packet timing и seek. `demux-api` по-прежнему владеет input shape, lossless sniff
replay, cancellation и progressive worker. S20/S21C выполняют capability
intersection до transport/demux I/O и не создают decoder.

Для CAF используется локальная replacement-копия exact
`symphonia-format-caf 0.6.0`. Patch меняет только format-level scan:

- seekable source сохраняет прежний полный chunk scan и seek назад к `data`;
- forward-only source останавливает initial scan перед `data` payload;
- declared data читается exact, поэтому truncation остаётся `UnexpectedEof`, а не
  коротким packet-ом или clean EOS;
- общий `MediaSourceStream` и EOF policy остальных formats не меняются.

Forward-only CAF требует stream-friendly layout: `desc` и все обязательные для
codec-а configuration chunks должны предшествовать `data`. Metadata после `data`
невозможно прочитать без буферизации всего media и поэтому не входит в этот
конечный progressive contract.

## Hermetic proof matrix

| Container row | Representative codec | Codec private | Packet/duration evidence | Local/Range | Non-Range |
|---|---|---|---|---|---|
| Ogg | Opus | exact `OpusHead` | один 20 ms packet, chained stream даёт `TracksChanged` | seekable, known duration | `NotSeekable`, typed seek reject, playback/EOS |
| CAF | PCM S16LE | `None` | fixed packets, 32 frames | seekable, known duration | patched forward scan, playback/EOS |
| WAVE | PCM S16LE | `None` | RIFF data packet, 32 frames | seekable, known duration | playback/EOS |
| AIFF | PCM S16BE | `None` | SSND packet, 32 frames | seekable, known duration | playback/EOS |
| native FLAC | FLAC | exact 34-byte STREAMINFO | two constant-subframe packets | seekable, known duration | playback/EOS |
| MPEG audio | Layer I | `None` | distinct legal frame, 384 samples | seekable, unknown raw duration | playback/EOS |
| MPEG audio | Layer II | `None` | distinct legal frame, 1152 samples | seekable, unknown raw duration | playback/EOS |
| MPEG audio | Layer III | `None` | distinct legal frame, 1152 samples | seekable, unknown raw duration | playback/EOS |

На non-Range input duration остаётся known у CAF/WAVE/AIFF/FLAC из header
authority. Ogg без tail scan-а и raw MP1/2/3 публикуют `None`; тесты закрепляют
это как корректную unknown duration, а не угадывают значение.

Все fixtures строятся в памяти. Required CI не читает `test-assets`, не включает
real media через `include_bytes!` и не вызывает внешний encoder/parser.

## Failure и capability evidence

- Каждая signature распознаётся без extension; conflicting ISO BMFF hint
  проигрывает content signature.
- Cancellation до sniff/open возвращает typed cancellation для каждой row.
- Узнанные truncated headers дают probe/backend failure, а clean finite streams —
  отдельный EOS.
- MPEG sniff принимает distinct Layer I/II/III и отклоняет reserved version/layer.
- S20/S21C отдельно проверяют Ogg Opus, CAF/WAV/AIFF PCM, FLAC и MP1/MP2/MP3:
  exact available family playable, отсутствующая family даёт typed
  `AudioUnavailable` до I/O.
- Реальный loopback S22 provider проводит каждую row через HTTP Range `206` и
  non-Range `200`; non-Range seek reject не повреждает последующее чтение.

## Fixture identities

- `symphonia/s28c-ogg-opus`
- `symphonia/s28c-caf-pcm`
- `symphonia/s28c-wave-pcm`
- `symphonia/s28c-aiff-pcm`
- `symphonia/s28c-native-flac`
- `symphonia/s28c-mpeg-layer-1`
- `symphonia/s28c-mpeg-layer-2`
- `symphonia/s28c-mpeg-layer-3`
