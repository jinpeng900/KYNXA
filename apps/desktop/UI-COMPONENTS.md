# 前端 UI 维护入口

- `Controls/ComposerSurface.xaml`：输入框外壳与可选底栏。`Body` 放编辑器和发送工具，`Footer` 放项目选择等操作；`EditorHeight` 只表示编辑器高度，`SurfaceHeight` 包含底栏。两部分共用组件宽度，底栏通过贯穿两行的背景与输入框相连。
- `Controls/PickerMenu.cs`：模型、项目和权限菜单共用的创建入口。`CreateList` 统一列表滚动和选中指示；`WithFixedFooter` 将滚动列表与固定底部操作分开。选择、保存和打开窗口仍由对应的 `ShellPage.*.cs` 处理。
- `Styles/Dimensions.xaml`：字号、行高、按钮尺寸、菜单行高、底栏高度和圆角。`Styles/Controls.xaml` 通过基础按钮样式派生图标、选择器、侧栏操作和发送按钮。
- `Layout/ShellLayoutMetrics.cs`：侧栏、输入框、聊天正文和右栏的默认尺寸及约束。持久化布局的默认值也来自这里；窗口临时变窄时只约束显示宽度，不覆盖用户保存的宽度。

增加 UI 时优先使用现有组件与样式。新的数据保存和业务行为放在页面对应功能文件或服务中，避免写入通用控件。

流式回复：`Views/ShellPage.StreamReplies.cs` 按会话 ID 维护生成任务，40ms 合并刷新当前消息；`ModelApiClient.StreamReplyAsync` 与 `ChatStreamReader` 读取网关 SSE，区分正文、思考和终止事件。思考默认收起，可展开渲染 Markdown；发送按钮在生成期间变成停止。消息保存 `Reasoning`、`ReasoningDurationMs`、`Status`、`Error` 和原模型信息，旧记录兼容；启动时把未完成生成恢复为中断。生成开始、定期快照和完成时保存，重试复用请求 ID，避免网络末包丢失导致重复上下文。

会话展示：`Controls/ConversationTranscript.cs` 用一个 WebView2 承载整段会话，页面资源在 `Resources/Transcript`，所有用户消息、正文、思考和表格处于同一份 DOM。输入框、侧栏和模型菜单继续使用 WinUI。主聊天的公式直接由 KaTeX 排版，不经过截图、Skia 或原生文字上方的图片图层；浏览器负责文字、公式与表格的尺寸和换行。

`ConversationTranscript` 按消息内容、思考内容和生成状态缓存 HTML，40ms 合并刷新；Markdig 解析和代码着色在后台任务中执行。切换聊天时清除前一会话，异步结果通过会话代际检查后才能发送给页面，避免旧任务覆盖新会话。聊天记录仍保存原始 Markdown，历史消息走相同的展示路径。

`Services/TranscriptMarkdown.cs` 复用 Markdig 和 `MathMarkdown.Normalize`，输出顶层 HTML 块，支持标题、强调、列表、任务清单、引用、代码、链接与真实表格。单元格保留行内节点，公式不再经过纯文本投影。`Resources/Transcript/transcript.css` 统一管理视觉样式：正文 15px、行高 1.7，公式 1.08em，围栏代码 13px。代码长行按可用宽度软换行，保留原始换行和缩进供复制；普通表格随回复区调整宽度，长单词可断行，无法换行的长公式仍可横向滚动。会话视口填满聊天列，正文不再受固定最大宽度限制，右侧栏缩小或收起时同步扩展；滚动条位于聊天列右缘。

`Resources/Transcript/transcript.js` 按顶层块的源 HTML 比较并保留未变化前缀，追加内容只替换变化尾部；已排版公式缓存 DOM 字符串，不重复生成图片。有活动选区时保留现有 DOM 并暂存最新快照，清除选区后再应用最终内容；切换会话会立即清除旧选区和消息。页面独立维护跟随底部状态：打开聊天到底部，从底部发送后跟随增长，主动向上滚动或选择文字时暂停。

跨消息选择使用浏览器原生 Range，用户消息按纯文本保留空白。复制时按选区的文档顺序序列化正文、表格和代码，过滤按钮、状态信息及 KaTeX 的辅助 MathML；完整选中的公式复制为带分隔符的原始 TeX，公式内部的部分选择只复制所选可视字符。整条消息的复制按钮直接复制原始内容。输入框仍使用原生控件，其复制不经过会话页面。

聊天页面只加载打包的本地 HTML、脚本、样式和字体。Markdown 原始 HTML 被转义，图片显示为文本链接；只有 `http`、`https`、`mailto` 地址可以作为链接交给宿主打开。WebView2 拦截其他页面导航、资源请求、下载和权限请求，不在排版时获取远程图片或脚本。

代码高亮：`Services/CodeSyntaxHighlighter.cs` 使用 [ColorCode.Core](https://github.com/CommunityToolkit/ColorCode-Universal) 解析围栏语言，`TranscriptMarkdown` 将文字片段输出为彩色 `span`；复制仍使用完整代码文本。支持 Python、JS/TS、C#/C++、Java、JSON、SQL、HTML/CSS、XML/XAML、PowerShell 等及常见缩写。关键字紫色、字符串深蓝、注释灰绿、数字蓝色；不改动缩进和换行。未标语言、未知语言以及超过 40,000 字符或 4,096 个文字片段的代码保留等宽纯文本。

解析与协议检查：`dotnet run --project tests/transcript-markdown-smoke/TranscriptMarkdownSmoke.csproj` 验证 HTML 结构、表格内公式、列对齐、代码保真、安全链接和流式转最终结果；`dotnet run --project tests/code-highlighting-smoke/CodeHighlightingSmoke.csproj` 验证代码语言匹配与降级；`dotnet run --project tests/chat-stream-smoke/ChatStreamSmoke.csproj` 验证 SSE 分包、真实 HTTP 增量、取消与记录兼容。`node tests/streaming-ui-fixture.mjs <空临时目录>` 提供独立模拟上游与真实网关，不含密钥，不应接到日常使用的模型配置中。

主聊天 UI 检查：`dotnet run --project tests/transcript-ui-smoke/TranscriptUiSmoke.csproj` 使用独立 WebView2 会话，检查多公式表格、跨消息选择、完整与部分公式复制、选区期间的最终更新以及打开和跟随底部；`--keep-open` 保留测试窗口。结果写入 `%TEMP%/kynxa-transcript-smoke.txt`，耗时与指标写入同目录的 `kynxa-transcript-smoke.json`，不读写用户聊天。

保留的原生控件：`MarkdownReply.*`、`TextSelectionAutoScroll`、`ConversationAutoFollow` 仍供原生控件与旧测试使用，其 `RichTextBlock`、原生表格、图片公式和鼠标捕获逻辑不再控制主聊天。`tests/markdown-ui-smoke`、`tests/scroll-follow-smoke` 及跨消息选择脚本只验证这些原生路径；它们通过不代表新的浏览器会话展示已经通过 UI 检查。

`Services/StoragePaths.cs` 统一解析桌面数据位置，与网关共享 `~/.kynxa/storage.json`（`dataRoot`）或 `KYNXA_DATA_HOME`。项目、普通聊天、模型选择、布局和界面错误日志均使用该目录下的 `Desktop`；未配置时兼容原 LocalState。移动已有数据使用网关目录下 `migrate-storage.mjs`，切勿只改路径导致旧记录不可见。

模型管理：`ModelManagementWindow.xaml` 使用固定底部操作区、独立滚动表单和折叠的高级设置；服务地址直接显示在主表单，连接 ID 位于高级设置。`Services/ModelPresets.cs` 集中维护服务地址、默认模型 ID、名称识别及本机/局域网地址判定。「本地 API（直接连接）」不依赖 Ollama，直接连接兼容接口，密钥提示随地址更新。新连接自动避开已有 ID，编辑连接固定 ID；切换服务商建立新配置，不修改已有连接。测试只读取模型列表，保存后通过聊天模型菜单选择。

预设来源（2026-09-28 核对，模型 ID 仍可编辑或通过接口刷新）：
- DeepSeek：https://api-docs.deepseek.com/ （`deepseek-flash`、`deepseek-v4-pro`）
- Kimi：https://platform.kimi.com/docs/get-api-key （国内地址、`kimi-k3`、`kimi-k2.7-code`、`kimi-k2.6`）
- Ollama：https://docs.ollama.com/api/openai-compatibility
- LM Studio：https://lmstudio.ai/docs/developer/openai-compat
- OpenAI：https://developers.openai.com/api/docs/models 、https://developers.openai.com/api/docs/guides/text （Responses）
- Claude：https://platform.claude.com/docs/en/models/overview 、https://platform.claude.com/docs/en/api/messages/create （Messages）
- Gemini：https://ai.google.dev/gemini-api/docs/openai
- Qwen：https://help.aliyun.com/zh/model-studio/compatibility-of-openai-with-dashscope
- GLM：https://docs.bigmodel.cn/cn/guide/start/model-overview
- MiniMax：https://platform.minimax.cn/docs/api-reference/text-openai-api
- Grok：https://docs.x.ai/developers/models

`ModelCatalog` 维护显示名称与简短用途说明，API 始终发送原始 ID。配置页可搜索/勾选模型、手动填写 ID；新接口返回的未知 ID 使用通用说明，不推断未验证的能力。聊天模型菜单显示模型名称、连接名称和 ID。高级设置支持切换三种协议，不能用显示名代替实际协议适配。

验证命令（在仓库根目录执行）：

```powershell
dotnet build apps/desktop/KYNXA.Desktop.csproj -p:Platform=x64 --no-restore
dotnet run --project tests/ui-layout-smoke/KYNXA.UiLayoutSmoke.csproj
```

界面检查：工作首页底栏与输入框等宽且相连；拖动输入框、缩放窗口后继续对齐；进入项目或切换普通聊天时底栏隐藏且没有额外空白；模型和项目菜单只滚动列表，底部操作保持固定。

## 数学公式与工作排序

`MathMarkdown` 将 `\(...\)`（含同一段落内的跨行内容）、`\[...\]` 转为 Markdown 数学分隔符，也支持 `$...$`、同一行或跨行的 `$$...$$`。行内公式不能跨越空段落或代码边界。单独方括号包围且包含数学运算符的段落可兼容恢复；普通括号、代码块和行内代码不猜测转换。显式标记为 `math` 的围栏代码块在生成完成后显示为公式；`tex`、`latex` 及其他语言仍按代码展示。

主聊天由 `TranscriptMarkdown` 输出带 `data-latex`、`data-display` 的数学节点，页面直接调用固定版本 [KaTeX 0.16.47](https://github.com/KaTeX/KaTeX/tree/v0.16.47) 渲染为 DOM。原始 TeX 保留在节点数据中，供复制使用；未闭合的流式块公式先显示源码，`math` 围栏到生成完成后再转换。资源、字体和 MIT 许可保存在 `Resources/Math` 并随软件发布，主聊天不进行公式截图，也不依赖图片缓存。

`KatexFormulaRenderer`、`MarkdownReply.Math` 的图片路径及 `MathFormulaRenderer` 的 CSharpMath/SkiaSharp 降级仍保留给原生控件。它们的公式长度、图像尺寸限制和 TeX 兼容改写属于原生渲染实现，不应套用到浏览器主聊天。浏览器渲染始终接收原始 TeX，避免为较小的解析器改写公式含义。

打开工作内聊天时展开该工作，并将未置顶工作移到置顶项之后；用户显式收起后，普通刷新不会再强制展开。拖动工作行可在相同置顶分组内调整顺序，写回项目列表；不会改变聊天所属工作或创建嵌套工作。之后再次打开工作内聊天会重新按最近使用规则提升该工作。

原生兼容检查：`dotnet run --project tests/math-project-smoke/MathProjectSmoke.csproj` 检查 CSharpMath 降级；`dotnet run --project tests/markdown-ui-smoke/MarkdownUiSmoke.csproj -- --math` 预览原生公式。旧的 `--delimiter-math`、`--physics-math`、`--table-math` 与选择脚本继续用于原生控件回归，主聊天公式、表格和复制应在新的 Transcript 页面检查。
