# R3 精炼与工程细化版

主文档：KYNXA_R3_精炼与工程细化版.docx；可维护文本：同名 .md。

原始 139 页压缩包保持不变。本版保留 18 章和 5 个附录，将重复的通用规则集中，并补充可实现的接口、状态机、恢复和验收建议。建议内容不自动覆盖原冻结决策。

实现现状以 [聊天与工作记忆架构](../../architecture/chat-work-memory.md)、[团队现状与下一步](../../team/README.md) 和仓库中英文 README 为准。2026-10-02 基线 `3226527` 已有统一聊天、分层确认记忆与 1M 配置，Host、Authority、任务图及执行闭环仍未实现。此处 `.md` / `.docx` 是更完整的设计参考，本次现状更新未重新生成 Word 文档或改变设计冻结状态。

当前修订采用扁平 Work：每个 Work 都是不可嵌套且相互独立的 Scope；复杂任务层级继续由 Work 内部的 TaskGraph、TaskNode 与 Subagent 表达。标签、置顶、归档和展示分组不产生权限、知识或状态继承。

engineering-additions.md 是新增内容源，build.ps1 从原压缩包提取章节并生成主文档。运行方式：在 PowerShell 中执行 .\build.ps1；也可传 -SourceArchive 指定原压缩包。

content-audit.json 记录原始包摘要、内容规模、章节覆盖和 OOXML 结构校验。源文档的独特表格记录按内容保留检查；不把模板去重等同于删除需求。

排版采用 compact_reference_guide，Letter / 1 英寸页边距，Calibri 11 pt + Microsoft YaHei 中文，正文 1.25 倍行距；表格 9.5 pt、代码 9 pt 为具名密集参考样式。表格固定 DXA 宽度、重复表头、无固定行高，列表使用 Word 编号。页眉为简洁技术手册标识。

此环境缺少文档技能要求的配套 Python/LibreOffice，尚未完成 render_docx.py 的逐页图片视觉校验；页数不可由源文本量推断。结构校验不等同于视觉校验。
