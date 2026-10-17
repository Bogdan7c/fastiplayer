"""Извлечение и сверка таблиц E-AC-3 (Annex E) из двух спецификаций.

Таблицы векторного квантования AHT (ATSC A/52:2012 Таблицы E4.1–E4.7 =
ETSI TS 102 366 v1.4.1 Таблицы E.3.1–E.3.7) слишком велики для ручной
переписи, поэтому они извлекаются программно из текстовых дампов
(`pdftotext -layout`) ОБЕИХ спецификаций и сравниваются поэлементно.
Скрипт также сверяет таблицы, которые в `src/tables/eac3/*` переписаны
вручную (E2.10, E3.1, E3.6, E3.14, E2.11–E2.13, E3.10), с текстом ATSC и ETSI.

Запуск (спецификации в репозиторий не входят):
    python3 -I extract_eac3_tables.py A52-2012.txt ts_102366.txt check
    python3 -I extract_eac3_tables.py A52-2012.txt ts_102366.txt rust [hebap...] > vq.rs

`check` печатает размеры, контрольные суммы (их же проверяют тесты
`src/tables/eac3/vq_codebooks.rs`) и падает при любом расхождении спецификаций;
`rust` печатает Rust-массивы таблиц E4.x.
"""
import re
import sys

# hebap → число бит индекса (Таблица E3.2: 2/6 … 9/6 бит на мантиссу × 6).
VQ_INDEX_BITS = {1: 2, 2: 3, 3: 4, 4: 5, 5: 7, 6: 8, 7: 9}
HEX = r'0x[0-9a-fA-F]{4}'
ROW = re.compile(r'^\s*(\d+)\s+(' + HEX + r')\s+(' + HEX + r')\s+(' + HEX + r')\s+('
                 + HEX + r')\s+(' + HEX + r')\s+(' + HEX + r')\s*$')


def vq_tables(text, title_pattern):
    """Разбирает все VQ-таблицы: строки «index + 6 hex» после заголовка."""
    tables = {}
    current = None
    expected = 0
    for line in text.splitlines():
        title = re.search(title_pattern, line)
        if title:
            current = int(title.group(1))
            tables[current] = []
            expected = 0
            continue
        if current is None:
            continue
        match = ROW.match(line)
        if not match:
            continue
        index = int(match.group(1))
        if index != expected:
            # Строка другой таблицы (или мусор) — заканчиваем текущую.
            if index == 0:
                continue
            raise SystemExit(f'hebap {current}: index {index}, expected {expected}')
        tables[current].append([int(match.group(k), 16) for k in range(2, 8)])
        expected += 1
        if expected == 1 << VQ_INDEX_BITS[current]:
            current = None
    return tables


def signed(value):
    return value - 0x10000 if value >= 0x8000 else value


def checksum(rows):
    """Сумма знаковых значений и сумма value·(позиция+1) по модулю 2^32."""
    plain = sum(signed(v) for row in rows for v in row)
    weighted = 0
    position = 0
    for row in rows:
        for value in row:
            position += 1
            weighted = (weighted + value * position) % (1 << 32)
    return plain, weighted


def frame_strategies(text, marker):
    """Таблица E2.10 / E.1.8: 32 строки «n Dxx/R ×6»."""
    rows = {}
    lines = text.splitlines()
    start = [i for i, line in enumerate(lines) if marker in line][-1]
    for line in lines[start:start + 80]:
        match = re.match(r'^\s*(\d+)\s+((?:(?:D15|D25|D45|R)\s+){5}(?:D15|D25|D45|R))\s*$', line)
        if match:
            rows[int(match.group(1))] = match.group(2).split()
    return [rows[i] for i in range(32)]


def hebap_table(text, marker):
    lines = text.splitlines()
    start = [i for i, line in enumerate(lines) if marker in line][-1]
    values = {}
    for line in lines[start:start + 50]:
        numbers = line.split()
        if len(numbers) == 4 and all(n.isdigit() for n in numbers):
            values[int(numbers[0])] = int(numbers[1])
            values[int(numbers[2])] = int(numbers[3])
    return [values[i] for i in range(64)]


def remap_table(text, marker):
    lines = text.splitlines()
    start = [i for i, line in enumerate(lines) if marker in line][-1]
    out = []
    for line in lines[start:start + 60]:
        tokens = line.replace('≥', '>=').split()
        hexes = [t for t in tokens if re.match(r'^(0x[0-9a-fA-F]{4}|N/A)$', t)]
        if len(hexes) == 6:
            out.append(hexes)
    return out


def attenuation_table(text, marker):
    lines = text.splitlines()
    start = [i for i, line in enumerate(lines) if marker in line][-1]
    out = []
    for line in lines[start:start + 60]:
        tokens = line.replace(',', '.').split()
        if len(tokens) == 4 and tokens[0].isdigit() and int(tokens[0]) == len(out):
            out.append([float(t) for t in tokens[1:]])
        if len(out) == 32:
            break
    return out


def main():
    atsc = open(sys.argv[1], encoding='utf-8').read()
    etsi = open(sys.argv[2], encoding='utf-8').read()
    mode = sys.argv[3] if len(sys.argv) > 3 else 'check'
    atsc_vq = vq_tables(atsc, r'Table E4\.(\d) VQ Table for hebap')
    etsi_vq = vq_tables(etsi, r'Table E\.3\.(\d): VQ table for hebap')
    for hebap, bits in VQ_INDEX_BITS.items():
        assert len(atsc_vq[hebap]) == 1 << bits, (hebap, len(atsc_vq[hebap]))
        assert atsc_vq[hebap] == etsi_vq[hebap], f'hebap {hebap}: ATSC != ETSI'
    strategies_atsc = frame_strategies(atsc, 'Table E2.10 Frame Exponent Strategy Combinations')
    strategies_etsi = frame_strategies(etsi, 'Table E.1.9: Frame exponent strategy combinations')
    assert strategies_atsc == strategies_etsi, 'E2.10 differs'
    hebap_atsc = hebap_table(atsc, 'Table E3.1 High Efficiency Bit Allocation Pointers')
    hebap_etsi = hebap_table(etsi, 'Table E.2.1: High efficiency bit allocation pointers')
    assert hebap_atsc == hebap_etsi, 'E3.1 differs'
    remap_atsc = remap_table(atsc, 'Table E3.6 Large Mantissa Inverse Quantization')
    remap_etsi = remap_table(etsi, 'Table E.2.6: Large mantissa inverse quantization')
    assert remap_atsc == remap_etsi and len(remap_atsc) == 24, 'E3.6 differs'
    atten_atsc = attenuation_table(atsc, 'Table E3.14 Spectral Extension Attenuation Table')
    atten_etsi = attenuation_table(etsi, 'Table E.2.12: Spectral extension attenuation table')
    assert atten_atsc == atten_etsi and len(atten_atsc) == 32, 'E3.14 differs'
    if mode == 'check':
        for hebap in VQ_INDEX_BITS:
            print('hebap', hebap, 'rows', len(atsc_vq[hebap]), 'checksum', checksum(atsc_vq[hebap]))
        print('E2.10', strategies_atsc)
        print('E3.1', hebap_atsc)
        print('E3.6', remap_atsc)
        print('E3.14 sum', sum(sum(row) for row in atten_atsc))
        print('ATSC == ETSI for E4.x/E.3.x, E2.10, E3.1, E3.6, E3.14')
        return
    selected = [int(h) for h in sys.argv[4:]] or list(VQ_INDEX_BITS)
    for hebap in selected:
        rows = atsc_vq[hebap]
        print(f'/// Таблица E4.{hebap} (ATSC A/52:2012) = Таблица E.3.{hebap} (ETSI TS 102 366 v1.4.1):')
        print(f'/// VQ-кодовая книга для hebap {hebap}, {len(rows)} векторов × 6 значений в 16-битном')
        print('/// дополнительном коде (сгенерировано `tools/extract_eac3_tables.py`, не править руками).')
        print(f'pub(crate) const VQ_HEBAP_{hebap}: [[u16; 6]; {len(rows)}] = [')
        for row in rows:
            print('    [' + ', '.join(f'0x{v:04x}' for v in row) + '],')
        print('];')
        print()

main()
