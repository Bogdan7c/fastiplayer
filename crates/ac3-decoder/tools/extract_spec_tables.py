"""Сверка таблиц битового распределения и длин кадра двух спецификаций.

Извлекает Таблицы 7.12–7.16, 5.18 и 7.34 из текстовых дампов ATSC A/52:2012
и соответствующие Таблицы 6.12–6.16, 4.13, 6.34 из ETSI TS 102 366 v1.4.1,
сравнивает их поэлементно и печатает контрольные суммы (их же проверяют
тесты `src/tables/*`). Дампы получаются `pdftotext -layout` из PDF
спецификаций; сами спецификации в репозиторий не входят.

Запуск: python3 -I extract_spec_tables.py A52-2012.txt ts_102366.txt [print]
(«print» дополнительно печатает массивы). Ожидаемые расхождения — только
у frame size / 5/8 таблиц: ETSI пишет тысячи с пробелом («1 152»).
"""
import re, sys

def lines_after(text, marker, count):
    rows = text.splitlines()
    idx = [i for i, l in enumerate(rows) if marker in l]
    assert idx, marker
    return rows[idx[-1]:idx[-1] + count]

def norm(s):
    return s.replace('×', 'x')

def grid_table(text, marker, count, total, hexvals):
    """Таблица вида A=n: 10 значений в строке (masktab, latab)."""
    vals = []
    for l in lines_after(text, marker, count):
        l = norm(l)
        m = re.match(r'^\s*A\s*=\s*(\d+)\s+(.*)$', l)
        if not m:
            continue
        a = int(m.group(1))
        items = m.group(2).split()
        nums = [int(x, 16) if hexvals else int(x) for x in items]
        assert len(vals) == a * 10, (marker, a, len(vals))
        vals.extend(nums)
    return vals[:total]

def two_col(text, marker, count, ncols):
    """Таблица в две колонки: idx v1..vn idx v1..vn."""
    out = {}
    for l in lines_after(text, marker, count):
        l = norm(l)
        toks = l.split()
        if len(toks) == 2 * (ncols + 1) and all(re.match(r'^(0x[0-9a-fA-F]+|\d+)$', t) for t in toks):
            for half in (toks[:ncols + 1], toks[ncols + 1:]):
                out[int(half[0])] = [int(t, 0) for t in half[1:]]
    return out

def frame_table(text, marker, count):
    out = []
    for l in lines_after(text, marker, count):
        l = norm(l)
        m = re.match(r"^\s*['‘’]?([01]{6})['‘’]?\s*\((\d+)\)\s+(\d+)\s*(?:kbps)?\s+(\d+)\s+(\d+)\s+(\d+)", l)
        if m:
            out.append((int(m.group(1), 2), int(m.group(3)), int(m.group(4)), int(m.group(5)), int(m.group(6))))
    return out

atsc = open(sys.argv[1], encoding='utf-8').read()
etsi = open(sys.argv[2], encoding='utf-8').read()

tables = {}
tables['masktab'] = (grid_table(atsc, 'Table 7.13 Bin Number', 40, 256, False),
                     grid_table(etsi, 'Table 6.13: Bin number', 45, 256, False))
tables['latab'] = (grid_table(atsc, 'Table 7.14 Log-Addition', 40, 256, True),
                   grid_table(etsi, 'Table 6.14: Log-addition', 45, 256, True))
b_a = two_col(atsc, 'Table 7.12 Banding', 40, 2)
b_e = two_col(etsi, 'Table 6.12: Banding', 40, 2)
tables['bndtab'] = ([b_a[i][0] for i in range(50)], [b_e[i][0] for i in range(50)])
tables['bndsz'] = ([b_a[i][1] for i in range(50)], [b_e[i][1] for i in range(50)])
h_a = two_col(atsc, 'Table 7.15 Hearing', 40, 3)
h_e = two_col(etsi, 'Table 6.15: Hearing', 40, 3)
for f in range(3):
    tables[f'hth{f}'] = ([h_a[b][f] for b in range(50)], [h_e[b][f] for b in range(50)])
p_a = two_col(atsc, 'Table 7.16 Bit Allocation Pointer', 45, 1)
p_e = two_col(etsi, 'Table 6.16: Bit allocation pointer', 60, 1)
tables['baptab'] = ([p_a[i][0] for i in range(64)], [p_e[i][0] for i in range(64)])
tables['framesize'] = (frame_table(atsc, 'Table 5.18 Frame Size', 60), frame_table(etsi, 'Table 4.13: Frame size', 70))
tables['fiveeighths'] = (frame_table(atsc, 'Table 7.34 5/8_frame', 60), frame_table(etsi, 'Table 6.34: 5/8_framesize', 70))

ok = True
for name, (a, e) in tables.items():
    same = a == e
    ok &= same
    print(f'{name}: atsc={len(a)} etsi={len(e)} equal={same} sum={sum(x if isinstance(x,int) else sum(x) for x in a)}')
    if not same:
        for i, (x, y) in enumerate(zip(a, e)):
            if x != y:
                print('   diff', i, x, y)
if len(sys.argv) > 3:
    for name, (a, _) in tables.items():
        print(f'// {name}')
        print(a)
sys.exit(0 if ok else 1)
