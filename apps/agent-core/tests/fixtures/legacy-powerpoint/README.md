# Legacy PowerPoint fixtures

These four files are unmodified Apache POI test fixtures from the fixed
`REL_5_4_1` release tag. They exercise real compound storage, presentation
records and independent expected content. They are test data only; KYNXA does
not embed or invoke Apache POI, Java or Microsoft Office to parse documents.

这四份文件来自固定 Apache POI 发布标签，保留原始字节；它们只用于验证真实
CFB/PPT 结构，不是 KYNXA 解析器自行生成的正例，也不引入 Java 运行时。

Base source:
[Apache POI REL_5_4_1](https://svn.apache.org/repos/asf/poi/tags/REL_5_4_1/test-data/slideshow/).
The upstream LICENSE and NOTICE are included verbatim in this directory.

| File | Bytes | SHA-256 | Independent expectation |
| --- | ---: | --- | --- |
| basic_test_ppt_file.ppt | 15360 | e8fd4c38ad72ce3735acbca0b60dd5870fb60f819c169e3947d166f4f1bcc79d | Two slides, title/subtitle/body, notes for both slides; several saved edits |
| 54880_chinese.ppt | 103936 | c6a7769355bd678958fcd74eada9cd556a2d863014729e69b270e8679b6f484d | Single byte, Mix, 表, ﾊﾝｶｸ |
| with_textbox.ppt | 10752 | 56a900d5784d9cc245c6580ea26b00ec7fd430d39c26ad5b5e2fc071a45a629d | Hello, World!!!; I am just a poor boy; Times New Roman; Plain Text |
| incorrect_slide_order.ppt | 9728 | c9fcede64db56c68d7a2c34667800ee96eae6e4f36a07290c4b47793c7a8bb40 | First slide I added → Third slide I added → Second slide I added |

Content assertions are independently documented in upstream
[TestExtractor.java](https://svn.apache.org/repos/asf/poi/tags/REL_5_4_1/poi-scratchpad/src/test/java/org/apache/poi/hslf/extractor/TestExtractor.java)
and the records themselves. Expected presentation order comes from the active
slide list, not the order of objects on disk.

KYNXA also creates a separate authored CFB fixture in the focused Rust tests.
It appends a later save, replaces one slide and its notes, removes another slide
while retaining its persist-directory entry, and retains a deleted drawing
shape. Old/deleted marker text must never appear in extraction. This synthetic
boundary fixture is reported separately from the four independent files.

Parser basis: Microsoft [MS-PPT stream/liveness rules](https://learn.microsoft.com/en-us/openspecs/office_file_formats/ms-ppt/1fc22d56-28f9-4818-bd45-67c2bf721ccf),
[TextBytesAtom](https://learn.microsoft.com/en-us/openspecs/office_file_formats/ms-ppt/80aae34b-2699-43fa-9e6a-c560ae790cd7)
and [MS-ODRAW deleted shape flags](https://learn.microsoft.com/en-us/openspecs/office_file_formats/ms-odraw/8a7e7be3-0582-4461-9400-29d7eda8497d).
Text extraction does not claim faithful rendering, inherited master text,
chart/image OCR or macro/embedded-object execution.
