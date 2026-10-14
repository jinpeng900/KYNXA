"""Author small MS-DOC/CFB fixtures from public format structures, without Office.
根据公开 MS-DOC/CFB 结构生成原创小夹具，不使用 Office 或第三方文档。
"""

from pathlib import Path
import hashlib
import json
import struct

DIRECTORY = Path(__file__).resolve().parent
FREE = 0xFFFFFFFF
END = 0xFFFFFFFE
FAT = 0xFFFFFFFD


def compound(streams):
    """Use regular 4096-byte streams so no mini-FAT is necessary.
    流统一填充至 4096 字节，使用正常 FAT，无需微型流分配表。
    """
    header = bytearray(512)
    header[:8] = bytes.fromhex("d0cf11e0a1b11ae1")
    struct.pack_into("<HHHH", header, 24, 0x3E, 3, 0xFFFE, 9)
    struct.pack_into("<H", header, 32, 6)
    struct.pack_into("<I", header, 44, 1)
    struct.pack_into("<I", header, 48, 1)
    struct.pack_into("<I", header, 56, 4096)
    struct.pack_into("<IIII", header, 60, END, 0, END, 0)
    struct.pack_into("<109I", header, 76, 0, *([FREE] * 108))
    entries = bytearray(512)

    def entry(index, name, kind, left, right, child, sector, length):
        start = index * 128
        encoded = (name + "\0").encode("utf-16le")
        entries[start:start + len(encoded)] = encoded
        struct.pack_into("<HBBIII", entries, start + 64, len(encoded), kind, 1, left, right, child)
        struct.pack_into("<IQ", entries, start + 116, sector, length)

    entry(0, "Root Entry", 5, FREE, FREE, 1, END, 0)
    # Stream names are sorted using the CFB length-then-uppercase ordering.
    # 流名按 CFB 先长度、再大写字典序排序，目录树对应同一顺序。
    streams = sorted(streams.items(), key=lambda item: (len(item[0]), item[0].upper()))
    sectors = []
    fat = [FREE] * 128
    fat[0] = FAT
    fat[1] = END
    sector = 2
    for index, (name, content) in enumerate(streams, 1):
        content = content.ljust(max(4096, ((len(content) + 511) // 512) * 512), b"\0")
        count = len(content) // 512
        entry(index, name, 2, FREE, index + 1 if index < len(streams) else FREE, FREE, sector, len(content))
        for position in range(count):
            fat[sector + position] = sector + position + 1 if position + 1 < count else END
        sectors.append(content)
        sector += count
    return bytes(header) + struct.pack("<128I", *fat) + bytes(entries) + b"".join(sectors)


def word_document(encrypted=False, version=0xC1, descending=False, malformed_surrogate=False):
    stories = [
        "Report 2026: mass\x07 12.5 kg\x07\r",
        "中文正文：力学与量子；😀；\x13 HYPERLINK https://invalid.example\x14官方网站\x15\r",
        "Footnote: measured uncertainty 0.2 kg.\r",
        "页眉：实验记录\r页脚：2026\r",
        "Comment: independently verified.\r",
        "尾注：所有单位为国际单位制。\r",
        "文本框：注意温度 25 °C。\r",
        "Header textbox: lab A.\r",
        "\r",
    ]
    units = [text.encode("utf-16le") for text in stories]
    if malformed_surrogate:
        units[1] = b"\x00\xd8" + units[1][2:]
    counts = [len(raw) // 2 for raw in units]
    story_counts = [counts[0] + counts[1], *counts[2:8]]
    word = bytearray(4096)
    struct.pack_into("<HH", word, 0, 0xA5EC, version)
    struct.pack_into("<H", word, 6, 0x0804)
    struct.pack_into("<H", word, 10, 0x1204 | (0x0100 if encrypted else 0))
    struct.pack_into("<H", word, 12, 0x00BF)
    struct.pack_into("<H", word, 32, 14)
    struct.pack_into("<H", word, 62, 22)
    struct.pack_into("<H", word, 152, 93)
    struct.pack_into("<H", word, 898, 0)
    for index, count in zip((3, 4, 5, 7, 8, 9, 10), story_counts):
        struct.pack_into("<I", word, 64 + index * 4, count)
    # First logical English piece is physically after the Chinese piece.
    # 首个逻辑英文片段在物理上位于中文片段之后，以验证不能顺序扫描全文。
    offsets = [2200, 1024, 2350, 2460, 2530, 2630, 2720, 2800, 2860]
    descriptors = []
    for index, raw in enumerate(units):
        compressed = index == 0
        encoded = stories[index].encode("latin-1") if compressed else raw
        word[offsets[index]:offsets[index] + len(encoded)] = encoded
        location = (0x40000000 | offsets[index] * 2) if compressed else offsets[index]
        descriptors.append(struct.pack("<HIH", 0, location, 0))
    stale = "DELETED OLD BODY / 已被替换的历史正文".encode("utf-16le")
    word[3200:3200 + len(stale)] = stale
    struct.pack_into("<I", word, 64, 2862)
    cps = [0]
    for count in counts:
        cps.append(cps[-1] + count)
    if descending:
        cps[2] = cps[1] - 1
    plc = struct.pack(f"<{len(cps)}I", *cps) + b"".join(descriptors)
    # Include a valid Prc before Pcdt to exercise CLX navigation.
    # Pcdt 前放入合法 Prc，验证 CLX 变长属性区的定位。
    clx = b"\x01\x02\x00\x00\x00" + b"\x02" + struct.pack("<I", len(plc)) + plc
    struct.pack_into("<II", word, 154 + 33 * 8, 0, len(clx))
    return compound({"WordDocument": bytes(word), "1Table": clx})


def main():
    files = {
        "bilingual-stories.doc": word_document(),
        "encrypted.doc": word_document(encrypted=True),
        "word95.doc": word_document(version=0xA5),
        "bad-piece-order.doc": word_document(descending=True),
        "bad-surrogate.doc": word_document(malformed_surrogate=True),
    }
    for name, content in files.items():
        (DIRECTORY / name).write_bytes(content)
    (DIRECTORY / "manifest.json").write_text(json.dumps({
        "provenance": "Original generated MS-DOC/CFB fixtures; no third-party user document",
        "files": {name: {"bytes": len(content), "sha256": hashlib.sha256(content).hexdigest()} for name, content in files.items()},
    }, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
