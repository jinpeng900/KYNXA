# 原生文档与图像入库

2026-10-13。入口沿用资料导入和工作目录后台扫描，使用同一 `retrieval.sqlite`、作用域、来源版本与 E5 向量空间；未增加第二资料库或生成模型调用。

| 文件 | 实际提取内容 | 依赖与边界 |
| --- | --- | --- |
| MD、TXT、代码与已支持结构文本 | 原文，Markdown 标题／段落／表格／代码结构 | Rust 稳定读取与既有分块；大文本使用窗口 |
| PDF | 可提取正文；无正文的页面使用离线 OCR | Rust `lopdf` 与 Windows OCR；页窗口保留范围及原始字节哈希 |
| DOCX | 主文档段落和表格文字 | Rust ZIP／XML；不宣称还原排版、分页、嵌入图片或全部附属内容 |
| DOC／DOT（Word 97 及已识别后续二进制代际） | 活动片段的正文、脚注、页眉页脚、批注、尾注与文本框；保留已保存的域结果 | Rust `cfb` 与 MS-DOC FIB／CLX；不扫描残留旧正文，不执行域、宏或链接；不恢复版式及格式相关的隐藏／修订显示策略 |
| PPTX／PPTM／PPSX／POTX | 按幻灯片顺序提取标题、文本与备注 | Rust ZIP／XML；保存页和部件位置，不执行宏，不宣称图形及动画已理解 |
| PPT／PPS／POT（PowerPoint 97–2003 二进制） | 当前幻灯片顺序、活动文本形状与备注 | Rust `cfb` 与 MS-PPT Current User／UserEdit／PersistDirectory；排除被替换版本、删除幻灯片与删除形状；不恢复动画、图像与母版版式 |
| XLSX／XLSM／XLSB／XLS、ODS | 工作表、实际单元格、公式与已有值 | Rust `calamine` 和流式 XML；保留单元格位置，不重新计算公式、不展开巨大稀疏网格 |
| ODT／ODP、FODT／FODP／FODS | 正文、幻灯片或表格文字 | ZIP／XML，保留结构位置；加密内容明确拒绝 |
| EPUB、RTF、HTML／XHTML、FB2、SVG | 书籍章节、富文本或可见文字 | 按正文结构解码；不执行脚本、宏或嵌入程序；SVG 提取文字不等于理解图形 |
| EML、MHTML、MBOX | 邮件正文、网页邮件快照与消息顺序 | MIME 字符编码解码；附件与遗漏分别报告，不把二进制当正文 |
| ZIP、TAR、GZIP、BZIP2、7Z、RAR 等 | 有界成员展开后复用同一格式解析器 | [压缩与网址合同](archive-and-url-ingestion.md)；不落盘解压，不执行成员 |
| 明确 HTTP(S) 网址 | 网页正文或文档／压缩包直链快照 | 复用公共地址校验和下载传输，记录原始字节身份，不自动爬整站 |
| PNG、JPEG、BMP、GIF、TIFF、WebP | 离线 OCR 识别文字 | 已安装的 Windows 图像解码器和 OCR 语言；多帧图像明确报告暂不支持 |

格式注册统一位于 `document_formats.rs`，扫描、解析、回读和持久解析版本共用它。旧 Word 6／95、未知二进制代际、加密与损坏结构明确拒绝，不把 `strings` 提取当成解析。音视频转写、CAD、磁盘映像和未知专有格式没有通用解析保证。扩展名命中仅选择解析器，真实魔数、结构、加密与额度仍需验证。现代 Office 的含宏后缀仅提取文本，不执行宏。

旧二进制格式的提取身份分别为 `rust-msdoc-text-v1` 与 `rust-msppt-text-v1`，正文使用真实故事／幻灯片位置而非伪造页码。CFB 容器、流量、记录数、CP 与输出有界，取消后不发布半截正文。`documentComplete` 表示声明的文本解析结束；`omittedContent` 中的版式、图像、自动编号与修订显示等能力不能据此视为已完成。格式原件和独立 Apache POI 对照、许可证与哈希见 `tests/fixtures/legacy-word/` 和 `tests/fixtures/legacy-powerpoint/`。

图片 OCR 是文字识别，不是图像语义理解。原图格式、哈希、尺寸、解码后尺寸、缩放状态、帧覆盖及 OCR 语言／后端版本随来源保存。无文字、缺语言、无解码器或额度不足时保留失败原因，健康文件继续处理；不能把未识别内容说成不存在。

图片输入最多 32 MiB、原始尺寸最多 64M 像素，解码时缩放到每边最多 4000 像素及 Windows OCR 实际上限的较小值。资源租约覆盖解码和进程生命周期，取消或超时后先确认进程退出再释放。OCR 正文最多 2 MiB，JSON 转义的传输帧独立有界；扫描和回源采用一致的实际额度。PDF／DOCX 原有归档展开及正文限额继续生效，未提供无限文件大小保证。

工作文件回读和向量发布同时核对文字与提取身份。即使图像字节变化后 OCR 文字相同，旧工作文件引用也失效。导入资料仍是已授权的文字快照，原件变更不会撤销仍存在的快照；重新导入、撤销和目录增量维护沿用已有合同。

本轮 Windows 证据：原生 HTTP 导入原生 PDF、扫描 PDF、DOCX、MD、PNG 五个夹具，实际产生 10 个 E5 CPU 向量；工作重建产生 20 个向量并复用 10 个缓存。中英文检索、OCR 回读、原图字节改变后的旧引用拒绝、资料快照保留及单个坏 PNG 的部分完成回执均通过。独立 Windows OCR 检查实际通过 PNG、扫描 PDF、JPEG、BMP、静态 GIF、TIFF、WebP；不将本机解码器结果外推到所有安装环境，不将这些功能夹具当成 OCR 准确率基准。

定向验证入口为 `retrieval::gateway_documents::tests`、`storage::document_coverage::tests` 和显式运行的 `apps/agent-core/tests/document_ingestion.rs`。后者要求已准备的原生载荷与 ToolHost，使用独立临时 Data，无用户凭据；夹具来源见 `apps/agent-core/tests/fixtures/documents/README.md`。

扩展格式的真实夹具位于 `tests/fixtures/extended-documents/`；Office 四组定向检查通过。压缩与网址的真实 HTTP/E5 验证见 [本轮结果](archive-and-url-ingestion.md#本轮实际结果)，不把解析夹具通过当作所有格式的准确率或吞吐基准。

旧 DOC／PPT 本轮专项通过 7 项，其中原件来自固定 Apache POI 标签的 2 份 DOC、4 份 PPT；另有原创二进制结构负例验证活动版本、中文代理项、旧版本与删除内容、加密、取消和限额。现代 Office 原有 4 项与网址 MIME／CFB 识别 2 项同时通过，未重跑大题集。

新增 Windows 原生 HTTP 验收只导入 DOC、PPT 两份资料，实际生成 8 个 384 维 E5 CPU 向量；工作重建发布 16 个向量、复用 8 个缓存，来源覆盖与语义状态完成。中英文查询、持久化结构位置与提取版本、原文回读、原件非正文处字节变化后拒绝旧引用均通过；12.55 秒为这一独立功能测试总耗时，不能当成一般建库吞吐或模型回答准确率。入口为 `document_ingestion::legacy_office_imports_persist_parts_generate_vectors_and_reject_stale_live_sources`，诊断写入 `KYNXA_LEGACY_DOCUMENT_RESULT_PATH`，每次使用独立临时 Data。
