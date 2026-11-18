#!/usr/bin/env python3
"""Переписывает «паспорт» HE-AAC в M4A от fdkaac на явную иерархическую форму.

fdkaac пишет в MP4 обратно совместимую сигнализацию: AudioSpecificConfig
говорит «AAC-LC» и добавляет расширение синхронизации 0x2B7 с SBR (и 0x548 с PS).
Instagram и TikTok пишут иерархическую: тип объекта 5 (SBR) или 29 (PS), частота
ядра, каналы ядра, выходная частота, затем тип ядра 2 (AAC-LC).

Сами кадры AAC при этом одинаковы — меняется только DecoderSpecificInfo в
`esds`. Скрипт строит новый ASC из полей старого и переписывает дерево
дескрипторов и размеры коробок. Коробка `mdat` у fdkaac стоит перед `moov`,
поэтому смещения `stco` не меняются (скрипт это проверяет).

Использование: rewrite_asc_hierarchical.py <вход.m4a> <выход.m4a> <ожидаемый ASC hex>
"""

import struct
import sys

SAMPLE_RATES = [96000, 88200, 64000, 48000, 44100, 32000, 24000,
                22050, 16000, 12000, 11025, 8000, 7350]
AOT_AAC_LC = 2
AOT_SBR = 5
AOT_PS = 29
SYNC_EXTENSION_SBR = 0x2B7
SYNC_EXTENSION_PS = 0x548


class BitReader:
    def __init__(self, data):
        self.data = data
        self.position = 0

    def read(self, count):
        value = 0
        for _ in range(count):
            byte = self.data[self.position // 8]
            value = (value << 1) | ((byte >> (7 - self.position % 8)) & 1)
            self.position += 1
        return value

    def remaining(self):
        return len(self.data) * 8 - self.position


class BitWriter:
    def __init__(self):
        self.bits = []

    def write(self, value, count):
        self.bits.extend((value >> (count - 1 - index)) & 1 for index in range(count))

    def to_bytes(self):
        padded = self.bits + [0] * (-len(self.bits) % 8)
        return bytes(
            int("".join(map(str, padded[index:index + 8])), 2)
            for index in range(0, len(padded), 8)
        )


def hierarchical_asc(backward_compatible_asc):
    """Обратно совместимый ASC от fdkaac -> иерархический."""
    reader = BitReader(backward_compatible_asc)
    if reader.read(5) != AOT_AAC_LC:
        sys.exit("expected AAC-LC core in fdkaac ASC")
    core_rate_index = reader.read(4)
    core_channels = reader.read(4)
    # GASpecificConfig: frameLengthFlag, dependsOnCoreCoder, extensionFlag.
    if reader.read(3) != 0:
        sys.exit("unexpected GASpecificConfig flags")
    if reader.read(11) != SYNC_EXTENSION_SBR or reader.read(5) != AOT_SBR:
        sys.exit("no SBR sync extension")
    if reader.read(1) != 1:
        sys.exit("sbrPresentFlag is 0")
    output_rate_index = reader.read(4)
    has_ps = reader.remaining() >= 12 and reader.read(11) == SYNC_EXTENSION_PS and reader.read(1) == 1

    writer = BitWriter()
    writer.write(AOT_PS if has_ps else AOT_SBR, 5)
    writer.write(core_rate_index, 4)
    writer.write(core_channels, 4)
    writer.write(output_rate_index, 4)
    writer.write(AOT_AAC_LC, 5)
    writer.write(0, 3)  # GASpecificConfig без флагов
    print(
        f"core {SAMPLE_RATES[core_rate_index]} Hz x{core_channels} -> "
        f"{SAMPLE_RATES[output_rate_index]} Hz, PS={has_ps}"
    )
    return writer.to_bytes()


def read_descriptor(data, offset):
    tag = data[offset]
    offset += 1
    length = 0
    while True:
        byte = data[offset]
        offset += 1
        length = (length << 7) | (byte & 0x7F)
        if not byte & 0x80:
            break
    return tag, data[offset:offset + length], offset + length


def write_descriptor(tag, payload):
    length = len(payload)
    encoded_length = bytes([
        0x80 | ((length >> 21) & 0x7F), 0x80 | ((length >> 14) & 0x7F),
        0x80 | ((length >> 7) & 0x7F), length & 0x7F,
    ])
    return bytes([tag]) + encoded_length + payload


def rewrite_esds_payload(esds_payload, new_asc):
    """esds = версия/флаги + ES_Descriptor(3) { DecoderConfig(4) { DSI(5) }, SL(6) }."""
    version_flags = esds_payload[:4]
    tag, es_body, _ = read_descriptor(esds_payload, 4)
    if tag != 3 or es_body[2] != 0:
        sys.exit("unexpected ES_Descriptor")
    es_fixed, es_children = es_body[:3], es_body[3:]
    rebuilt_children = b""
    offset = 0
    while offset < len(es_children):
        child_tag, child_body, offset = read_descriptor(es_children, offset)
        if child_tag == 4:
            config_fixed, config_children = child_body[:13], child_body[13:]
            dsi_tag, _, _ = read_descriptor(config_children, 0)
            if dsi_tag != 5:
                sys.exit("DecoderSpecificInfo expected")
            child_body = config_fixed + write_descriptor(5, new_asc)
        rebuilt_children += write_descriptor(child_tag, child_body)
    return version_flags + write_descriptor(3, es_fixed + rebuilt_children)


CONTAINER_BOXES = {b"moov", b"trak", b"mdia", b"minf", b"stbl"}
MP4A_HEADER_BYTES = 28


def rewrite_boxes(data, new_asc_bytes, found):
    """Рекурсивно переписывает коробки; возвращает новые байты уровня."""
    output = b""
    offset = 0
    while offset < len(data):
        size, kind = struct.unpack(">I4s", data[offset:offset + 8])
        body = data[offset + 8:offset + size]
        if kind in CONTAINER_BOXES:
            body = rewrite_boxes(body, new_asc_bytes, found)
        elif kind == b"stsd":
            entries = rewrite_boxes(body[8:], new_asc_bytes, found)
            body = body[:8] + entries
        elif kind == b"mp4a":
            children = rewrite_boxes(body[MP4A_HEADER_BYTES:], new_asc_bytes, found)
            body = body[:MP4A_HEADER_BYTES] + children
        elif kind == b"esds":
            old_asc = descriptor_asc(body)
            found["old"] = old_asc
            found["new"] = hierarchical_asc(old_asc)
            body = rewrite_esds_payload(body, found["new"])
        output += struct.pack(">I4s", len(body) + 8, kind) + body
        offset += size
    return output


def descriptor_asc(esds_payload):
    _, es_body, _ = read_descriptor(esds_payload, 4)
    _, config_body, _ = read_descriptor(es_body, 3)
    _, dsi_body, _ = read_descriptor(config_body, 13)
    return dsi_body


def top_level_order(data):
    order = []
    offset = 0
    while offset < len(data):
        size, kind = struct.unpack(">I4s", data[offset:offset + 8])
        order.append(kind)
        offset += size
    return order


def main():
    source_path, output_path, expected_hex = sys.argv[1:4]
    with open(source_path, "rb") as source:
        data = source.read()
    order = top_level_order(data)
    if order.index(b"mdat") > order.index(b"moov"):
        sys.exit("moov before mdat: stco offsets would shift")
    found = {}
    rewritten = rewrite_boxes(data, None, found)
    if found.get("new", b"").hex() != expected_hex:
        sys.exit(f"ASC {found.get('new', b'').hex()} != expected {expected_hex}")
    with open(output_path, "wb") as output:
        output.write(rewritten)
    print(f"{output_path}: ASC {found['old'].hex()} -> {found['new'].hex()}")


if __name__ == "__main__":
    main()
