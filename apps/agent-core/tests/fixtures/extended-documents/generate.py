"""Generate small, original document fixtures without user data or Office automation.
无需用户数据或 Office 自动化，生成原创的小型文档夹具。
"""

from pathlib import Path
from zipfile import ZIP_DEFLATED, ZipFile
import base64
import struct

ROOT = Path(__file__).resolve().parent


def package(name, items):
    with ZipFile(ROOT / name, "w", ZIP_DEFLATED) as output:
        for key, value in items.items():
            output.writestr(key, value)


def relationships(rows):
    return (
        '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
        + "".join(f'<Relationship Id="{a}" Type="{b}" Target="{c}"/>' for a, b, c in rows)
        + "</Relationships>"
    )


package("guide.pptx", {
    "ppt/presentation.xml": '<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="257" r:id="r2"/><p:sldId id="256" r:id="r1"/></p:sldIdLst></p:presentation>',
    "ppt/_rels/presentation.xml.rels": relationships([("r1", "slide", "slides/slide1.xml"), ("r2", "slide", "slides/slide2.xml")]),
    "ppt/slides/slide1.xml": '<p:sld xmlns:p="p" xmlns:a="a"><a:p><a:r><a:t>Second 35 kg</a:t></a:r></a:p></p:sld>',
    "ppt/slides/slide2.xml": '<p:sld xmlns:p="p" xmlns:a="a"><a:p><a:r><a:t>第一张 First 25 kg &amp; balance</a:t></a:r></a:p></p:sld>',
    "ppt/slides/_rels/slide2.xml.rels": relationships([("n1", "notesSlide", "../notesSlides/notesSlide1.xml")]),
    "ppt/notesSlides/notesSlide1.xml": '<p:notes xmlns:p="p" xmlns:a="a"><a:p><a:r><a:t>备注：只读验证 Speaker notes</a:t></a:r></a:p></p:notes>',
})

package("guide.xlsx", {
    "[Content_Types].xml": '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/></Types>',
    "xl/workbook.xml": '<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="数据 kg" sheetId="1" r:id="r1"/></sheets></workbook>',
    "xl/_rels/workbook.xml.rels": relationships([("r1", "http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet", "worksheets/sheet1.xml")]),
    "xl/worksheets/sheet1.xml": '<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><dimension ref="A1:C3"/><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>质量 kg</t></is></c><c r="B1" t="inlineStr"><is><t>Quantity</t></is></c></row><row r="2"><c r="A2"><v>25.5</v></c><c r="B2"><v>2</v></c><c r="C2"><f>A2*B2</f><v>51</v></c></row><row r="3"><c r="C3" t="b"><v>1</v></c></row></sheetData></worksheet>',
})


def record(kind, data=b""):
    header = bytes([kind]) if kind < 128 else bytes([(kind & 127) | 128, (kind >> 7) & 127])
    length = len(data)
    while length >= 128:
        header += bytes([(length & 127) | 128])
        length >>= 7
    return header + bytes([length]) + data


def wide(text):
    data = text.encode("utf-16-le")
    return struct.pack("<I", len(data) // 2) + data


package("guide.xlsb", {
    "xl/_rels/workbook.bin.rels": relationships([("r1", "worksheet", "worksheets/sheet1.bin")]),
    "xl/workbook.bin": record(0x83) + record(0x99, b"\0") + record(0x8F) + record(0x9C, struct.pack("<II", 0, 1) + wide("r1") + wide("资料")) + record(0x90) + record(0x84),
    "xl/worksheets/sheet1.bin": record(0x81) + record(0x94, struct.pack("<IIII", 0, 1, 0, 1)) + record(0x91) + record(0, struct.pack("<I", 0)) + record(6, struct.pack("<II", 0, 0) + wide("质量 kg")) + record(0, struct.pack("<I", 1)) + record(5, struct.pack("<IId", 0, 0, 25.5)) + record(0x92) + record(0x82),
})

CONTENT = '<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0"><office:body><office:text><text:h>资料 Report</text:h><text:p>账户密码 reset password</text:p><table:table table:name="Measurements"><table:table-row><table:table-cell><text:p>质量 kg</text:p></table:table-cell><table:table-cell office:value-type="float" office:value="25.5" table:formula="of:=SUM([.A1:.A2])"><text:p>25.5</text:p></table:table-cell></table:table-row></table:table></office:text></office:body></office:document-content>'
for extension, mime in [("odt", "text"), ("odp", "presentation"), ("ods", "spreadsheet")]:
    package(f"guide.{extension}", {"mimetype": f"application/vnd.oasis.opendocument.{mime}", "content.xml": CONTENT})
for extension in ["fodt", "fodp", "fods"]:
    (ROOT / f"guide.{extension}").write_text(CONTENT, encoding="utf-8")

package("guide.epub", {
    "mimetype": "application/epub+zip",
    "META-INF/container.xml": '<container><rootfiles><rootfile full-path="OPS/package.opf"/></rootfiles></container>',
    "OPS/package.opf": '<package><manifest><item id="c1" href="chapter1.xhtml" media-type="application/xhtml+xml"/><item id="c2" href="chapter2.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="c2"/><itemref idref="c1"/></spine></package>',
    "OPS/chapter1.xhtml": '<html><body><h1>Second chapter</h1><p>账户 password</p></body></html>',
    "OPS/chapter2.xhtml": '<html><body><h1>First chapter</h1><p>第一章 25 kg</p><script>do_not_index_script()</script></body></html>',
})
(ROOT / "guide.html").write_text('<!DOCTYPE html><html><head><meta charset="UTF-8"/><style>secret_css</style><title>资料</title></head><body><h1>账户 reset</h1><table><tr><td>质量 kg</td><td>25.5</td></tr></table><script>do_not_index_script()</script></body></html>', encoding="utf-8")
(ROOT / "guide-gbk.html").write_bytes('<html><head><meta charset="gbk"/></head><body>账户密码 Password</body></html>'.encode("gbk"))
(ROOT / "guide.rtf").write_bytes(br"{\rtf1\ansi\ansicpg1252{\fonttbl{\f0 Arial;}}\f0 Account 25 kg\par \u36134?\u25143? \'e9\tab value}")
(ROOT / "guide-gbk.rtf").write_bytes(br"{\rtf1\ansi\ansicpg936 Account \'d5\'cb\'bb\'a7 Password}")
body = "账户密码 reset password 25 kg\n".encode()
subject = base64.b64encode("账户确认".encode()).decode()
(ROOT / "guide.eml").write_bytes((f"From: Example <nobody@example.invalid>\r\nSubject: =?UTF-8?B?{subject}?=\r\nDate: Mon, 12 Oct 2026 12:00:00 +0800\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n\r\n" + base64.b64encode(body).decode() + "\r\n").encode())
(ROOT / "guide.mhtml").write_bytes(b"MIME-Version: 1.0\r\nContent-Type: multipart/related; boundary=fixture\r\n\r\n--fixture\r\nContent-Type: text/html; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n\r\n" + base64.b64encode('<html><body>账户 reset 25 kg<script>secret_script</script></body></html>'.encode()) + b"\r\n--fixture--\r\n")
(ROOT / "guide.mbox").write_bytes(b"From nobody@example.invalid Mon Oct 12 12:00:00 2026\n" + (ROOT / "guide.eml").read_bytes() + b"\nFrom nobody@example.invalid Tue Oct 13 12:00:00 2026\n" + (ROOT / "guide.eml").read_bytes())
(ROOT / "guide.fb2").write_text('<FictionBook><description><title-info><book-title>资料</book-title></title-info></description><body><section><title><p>账户</p></title><p>Reset password 25 kg</p></section></body><binary>excluded_binary</binary></FictionBook>', encoding="utf-8")
(ROOT / "guide.svg").write_text('<svg xmlns="http://www.w3.org/2000/svg"><text>账户 reset 25 kg</text><script>excluded_script</script></svg>', encoding="utf-8")
print("Generated original fixtures.")
