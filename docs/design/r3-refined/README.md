# R3 精炼与工程细化版

主文档：KYNXA_R3_精炼与工程细化版.docx；可维护文本：同名 .md。

原始 139 页压缩包保持不变。本版保留 18 章和 5 个附录，将重复的通用规则集中，并补充可实现的接口、状态机、恢复和验收建议。建议内容不自动覆盖原冻结决策。

engineering-additions.md 是新增内容源，build.ps1 从原压缩包提取章节并生成主文档。运行方式：在 PowerShell 中执行 .\build.ps1；也可传 -SourceArchive 指定原压缩包。

content-audit.json 记录原始包摘要、内容规模、章节覆盖和 OOXML 结构校验。源文档的独特表格记录按内容保留检查；不把模板去重等同于删除需求。

排版采用 compact_reference_guide，Letter / 1 英寸页边距，Calibri 11 pt + Microsoft YaHei 中文，正文 1.25 倍行距；表格 9.5 pt、代码 9 pt 为具名密集参考样式。表格固定 DXA 宽度、重复表头、无固定行高，列表使用 Word 编号。页眉为简洁技术手册标识。

此环境缺少文档技能要求的配套 Python/LibreOffice，尚未完成 render_docx.py 的逐页图片视觉校验；页数不可由源文本量推断。结构校验不等同于视觉校验。