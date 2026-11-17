"""BDF から、決めた範囲の字だけを残す（zcode のコードのフォント。tools/fetch-fonts.sh から）。

使い方: python3 subset_bdf.py <元.bdf> <出力.bdf>
zcode は起動時に BDF を全部読むので、エディタで使わない字（絵文字・Nerd Fonts の
アイコンなど）を落として、読む量を減らす。残さなかった字は DejaVu Sans Mono で描かれる。
"""
import sys

KEEP = [
    (0x20, 0x7E),      # ASCII
    (0xA0, 0x17F),     # Latin-1・Latin Extended-A
    (0x370, 0x3FF),    # ギリシャ文字
    (0x2000, 0x206F),  # 一般の句読点（…・‘’“” など）
    (0x20A0, 0x20BF),  # 通貨
    (0x2100, 0x214F),  # 文字のような記号
    (0x2190, 0x21FF),  # 矢印
    (0x2200, 0x22FF),  # 数学の記号（⊗ など）
    (0x2300, 0x23FF),  # その他の技術用の記号
    (0x2500, 0x25FF),  # 罫線・ブロック・幾何学の図形（▽▼ など）
    (0x2600, 0x26FF),  # その他の記号
    (0x2700, 0x27BF),  # 装飾記号（✓✗ など）
    (0xFFFD, 0xFFFD),  # 置換文字
]


def keep(cp):
    return any(a <= cp <= b for a, b in KEEP)


def main(src, dst):
    lines = open(src, encoding='latin-1').read().split('\n')
    head, glyphs, cur, i = [], [], None, 0
    for line in lines:
        if line.startswith('STARTCHAR'):
            cur = [line]
        elif cur is not None:
            cur.append(line)
            if line.startswith('ENDCHAR'):
                glyphs.append(cur)
                cur = None
        elif line.startswith('CHARS '):
            head.append(None)  # 後で数を入れる
        elif line.startswith('ENDFONT'):
            break
        else:
            head.append(line)
    kept = []
    for g in glyphs:
        enc = next((int(l.split()[1]) for l in g if l.startswith('ENCODING ')), -1)
        if enc >= 0 and keep(enc):
            kept.append(g)
    out = []
    for line in head:
        out.append(f'CHARS {len(kept)}' if line is None else line)
    for g in kept:
        out.extend(g)
    out.append('ENDFONT')
    open(dst, 'w', encoding='latin-1', newline='\n').write('\n'.join(out) + '\n')
    print(f'{dst}: {len(kept)} / {len(glyphs)} 字')


if __name__ == '__main__':
    main(sys.argv[1], sys.argv[2])
