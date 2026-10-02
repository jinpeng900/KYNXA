# 前端 UI 维护入口

- `Controls/ComposerSurface.xaml`：输入框外壳与可选底栏。`Body` 放编辑器和发送工具，`Footer` 放项目选择等操作；`EditorHeight` 只表示编辑器高度，`SurfaceHeight` 包含底栏。两部分共用组件宽度，底栏通过贯穿两行的背景与输入框相连。
- `Controls/PickerMenu.cs`：模型、项目和权限菜单共用的创建入口。`CreateList` 统一列表滚动和选中指示；`WithFixedFooter` 将滚动列表与固定底部操作分开。选择、保存和打开窗口仍由对应的 `ShellPage.*.cs` 处理。
- `Styles/Dimensions.xaml`：字号、行高、按钮尺寸、菜单行高、底栏高度和圆角。`Styles/Controls.xaml` 通过基础按钮样式派生图标、选择器、侧栏操作和发送按钮。
- `Layout/ShellLayoutMetrics.cs`：侧栏、输入框、聊天正文和右栏的默认尺寸及约束。持久化布局的默认值也来自这里；窗口临时变窄时只约束显示宽度，不覆盖用户保存的宽度。

增加 UI 时优先使用现有组件与样式。新的数据保存和业务行为放在页面对应功能文件或服务中，避免写入通用控件。

流式回复：`Views/ShellPage.StreamReplies.cs` 按会话 ID 维护生成任务，40ms 合并刷新当前消息；`ModelApiClient.StreamReplyAsync` 与 `ChatStreamReader` 读取网关 SSE，区分正文、思考和终止事件。思考默认收起，可展开渲染 Markdown；发送按钮在生成期间变成停止。消息保存 `Reasoning`、`ReasoningDurationMs`、`Status`、`Error` 和原模型信息，旧记录兼容；启动时把未完成生成恢复为中断。生成开始、定期快照和完成时由网关保存到统一会话日志，页面不再每 1.5 秒重写整个聊天目录；重试复用请求 ID 与用户消息 ID，避免重复上下文。

会话展示：`Controls/ConversationTranscript.cs` 用一个 WebView2 承载整段会话，页面资源在 `Resources/Transcript`，所有用户消息、正文、思考和表格处于同一份 DOM。输入框、侧栏和模型菜单继续使用 WinUI。主聊天的公式直接由 KaTeX 排版，不经过截图、Skia 或原生文字上方的图片图层；浏览器负责文字、公式与表格的尺寸和换行。

`ConversationTranscript` 按消息内容、思考内容和生成状态缓存 HTML，40ms 合并刷新；Markdig 解析和代码着色在后台任务中执行。切换聊天时清除前一会话，异步结果通过会话代际检查后才能发送给页面，避免旧任务覆盖新会话。聊天记录仍保存原始 Markdown，历史消息走相同的展示路径。

`Services/TranscriptMarkdown.cs` 复用 Markdig 和 `MathMarkdown.Normalize`，输出顶层 HTML 块，支持标题、强调、列表、任务清单、引用、代码、链接与真实表格。单元格保留行内节点，公式不再经过纯文本投影。`Resources/Transcript/transcript.css` 统一管理视觉样式：正文 15px、行高 1.7，公式 1.08em，围栏代码 13px。代码长行按可用宽度软换行，保留原始换行和缩进供复制；普通表格随回复区调整宽度，长单词可断行，无法换行的长公式仍可横向滚动。会话视口填满聊天列，正文不再受固定最大宽度限制，右侧栏缩小或收起时同步扩展；滚动条位于聊天列右缘。

`Resources/Transcript/transcript.js` 按顶层块的源 HTML 比较并保留未变化前缀，追加内容只替换变化尾部；已排版公式缓存 DOM 字符串，不重复生成图片。有活动选区时保留现有 DOM 并暂存最新快照，清除选区后再应用最终内容；切换会话会立即清除旧选区并显示目标会话。页面独立维护跟随底部状态：打开聊天到底部，从底部发送后跟随增长，主动向上滚动或选择文字时暂停。主控件保留最多 1,024 条消息、8M 字符的 Markdown/高亮 HTML 缓存，页面保留最近 4 个聊天的 DOM（合计最多 40,000 节点、2M 字符，单个最多 400 条消息）；再次打开复用公式和消息节点，缓存命中不等流式批处理定时器。后台解析在消息边界检查会话代次，快速切换时丢弃过期结果，避免旧聊天覆盖当前聊天。

跨消息选择使用浏览器原生 Range，用户消息按纯文本保留空白。复制时按选区的文档顺序序列化正文、表格和代码，过滤按钮、状态信息及 KaTeX 的辅助 MathML；完整选中的公式复制为带分隔符的原始 TeX，公式内部的部分选择只复制所选可视字符。整条消息的复制按钮直接复制原始内容。输入框仍使用原生控件，其复制不经过会话页面。

聊天页面只加载打包的本地 HTML、脚本、样式和字体。Markdown 原始 HTML 被转义，图片显示为文本链接；只有 `http`、`https`、`mailto` 地址可以作为链接交给宿主打开。WebView2 拦截其他页面导航、资源请求、下载和权限请求，不在排版时获取远程图片或脚本。

代码高亮：`Services/CodeSyntaxHighlighter.cs` 使用 [ColorCode.Core](https://github.com/CommunityToolkit/ColorCode-Universal) 解析围栏语言，`TranscriptMarkdown` 将文字片段输出为彩色 `span`；复制仍使用完整代码文本。支持 Python、JS/TS、C#/C++、Java、JSON、SQL、HTML/CSS、XML/XAML、PowerShell 等及常见缩写。关键字紫色、字符串深蓝、注释灰绿、数字蓝色；不改动缩进和换行。未标语言、未知语言以及超过 40,000 字符或 4,096 个文字片段的代码保留等宽纯文本。

解析与协议检查：`dotnet run --project tests/transcript-markdown-smoke/TranscriptMarkdownSmoke.csproj` 验证 HTML 结构、表格内公式、列对齐、代码保真、安全链接和流式转最终结果；`dotnet run --project tests/code-highlighting-smoke/CodeHighlightingSmoke.csproj` 验证代码语言匹配与降级；`dotnet run --project tests/chat-stream-smoke/ChatStreamSmoke.csproj` 验证 SSE 分包、真实 HTTP 增量、取消与记录兼容。`node tests/streaming-ui-fixture.mjs <空临时目录>` 提供独立模拟上游与真实网关，不含密钥，不应接到日常使用的模型配置中。

主聊天 UI 检查：`dotnet run --project tests/transcript-ui-smoke/TranscriptUiSmoke.csproj` 使用独立 WebView2 会话，检查多公式表格、跨消息选择、完整与部分公式复制、选区期间的最终更新以及打开和跟随底部；`--keep-open` 保留测试窗口。结果写入 `%TEMP%/kynxa-transcript-smoke.txt`，耗时与指标写入同目录的 `kynxa-transcript-smoke.json`，不读写用户聊天。

保留的原生控件：`MarkdownReply.*`、`TextSelectionAutoScroll`、`ConversationAutoFollow` 仍供原生控件与旧测试使用，其 `RichTextBlock`、原生表格、图片公式和鼠标捕获逻辑不再控制主聊天。`tests/markdown-ui-smoke`、`tests/scroll-follow-smoke` 及跨消息选择脚本只验证这些原生路径；它们通过不代表新的浏览器会话展示已经通过 UI 检查。

`Services/StoragePaths.cs` 统一解析桌面数据位置，与网关共享 `~/.kynxa/storage.json`（`dataRoot`）或 `KYNXA_DATA_HOME`。模型选择、布局和界面错误日志使用该目录下的 `Desktop`；未配置时兼容原 LocalState。`ProjectStore` 现在是异步会话 API 客户端，项目元信息与聊天正文的唯一正式来源在网关 `ConversationStore`；正文按项目/会话目录保存到 Data 下的 JSONL，项目关联文件夹独立。目录保存只上传元信息和新用户消息，不回写助手快照；版本冲突要求重新加载。移动已有数据使用网关目录下 `migrate-storage.mjs`，切勿只改路径导致旧记录不可见。

模型管理：`ModelManagementWindow.xaml` 使用固定底部操作区、独立滚动表单和折叠的高级设置；服务地址直接显示在主表单，连接 ID 位于高级设置。`Services/ModelPresets.cs` 集中维护服务地址、默认模型 ID、名称识别及本机/局域网地址判定。「本地 API（直接连接）」不依赖 Ollama，直接连接兼容接口，密钥提示随地址更新。新连接自动避开已有 ID，编辑连接固定 ID；切换服务商建立新配置，不修改已有连接。测试只读取模型列表，保存后通过聊天模型菜单选择。

预设来源（2026-10-01 核对）：共 151 个云端聊天模型 ID。数字是本应用纳入的预设数，包含可输出文本的视觉模型；不重复计算兼容别名，不代表账号已开通全部模型。精确 ID 和显示名称仅维护在 `Services/ModelCatalog.cs`，`ModelPresets` 从同一目录生成预设。

| 服务商 | 预设数 | 官方依据 |
| --- | ---: | --- |
| DeepSeek | 2 | [当前模型](https://api-docs.deepseek.com/quick_start/pricing/)；旧 Flash 名称为兼容别名 |
| Kimi | 4 | [模型列表](https://platform.kimi.com/docs/models)；包括 Code Highspeed，已下线的 K2.5 / Moonshot V1 不再新增 |
| OpenAI | 28 | [官方完整目录](https://developers.openai.com/api/docs/models/all)；通用文本、推理、代码和 Chat Latest，排除专用音视频、嵌入、受限研究模型和弃用型号 |
| Claude | 13 | [生命周期](https://platform.claude.com/docs/en/about-claude/model-deprecations)；公开 Active 型号，排除邀请制 Mythos |
| Gemini | 8 | [模型目录](https://ai.google.dev/gemini-api/docs/models)、[OpenAI 接口](https://ai.google.dev/gemini-api/docs/openai)；旧账户限定的 2.5 不作为新连接预设 |
| Qwen | 55 | [北京区域清单](https://help.aliyun.com/zh/model-studio/rate-limit)、[文本接口](https://help.aliyun.com/zh/model-studio/qwen-api-via-openai-chat-completions)；含 Coder、VL 及支持文本输出的 Qwen3.8 Omni Flash |
| GLM | 25 | [模型总览](https://docs.bigmodel.cn/cn/guide/start/model-overview)、[对话 API](https://docs.bigmodel.cn/api-reference/模型-api/对话补全)、[Long](https://docs.bigmodel.cn/cn/guide/models/text/glm-4-long)、[Plus](https://docs.bigmodel.cn/cn/guide/models/text/glm-4) |
| MiniMax | 8 | [对话 API](https://platform.minimax.io/docs/api-reference/text-chat-openai)、[模型介绍](https://platform.minimax.io/docs/guides/models-intro)；M3.1 Flash Preview 限 M Plan / MiniMax Code，留给账号实际获取 |
| Grok | 8 | [型号表](https://docs.x.ai/developers/models)、[Multi-agent 普通文本调用](https://docs.x.ai/developers/model-capabilities/text/multi-agent) |

本地服务仍从服务端获取实际安装模型：[Ollama](https://docs.ollama.com/api/openai-compatibility)、[LM Studio](https://lmstudio.ai/docs/developer/openai-compat)。配置页仅展示模型名称与 ID，删除用途、特征介绍；名称与 ID 相同只显示一行。搜索按名称/ID，数量显示全部候选数和已选数。未知模型保留原始 ID，不猜测能力或本地/云端部署位置。已有连接的勾选项保持原配置，补充预设会作为可勾选项出现，保存后才加入聊天菜单。

“获取模型”依据账号接口返回计数，去重并读取 Anthropic 的全部分页；不再截断前 100 个，超过明确上限或分页失败则报告错误而不返回残缺成功列表。预设数、接口返回数和用户勾选数含义不同。高级设置继续支持三种协议。官方 [GPT-5.5 Pro](https://developers.openai.com/api/docs/models/gpt-5.5-pro) 和 [o3-pro](https://developers.openai.com/api/docs/models/o3-pro) 不支持上游流式请求，网关使用完整 JSON 响应，经现有事件协议交给聊天界面，完成后一次显示结果。

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

工作侧栏分为最近、项目、任务。点击项目名称选择工作区，旁边的原生箭头独立展开聊天；选择项目、打开聊天、发送消息和三点操作均不自动展开项目。`ShellPage.WorkSidebar.cs` 按项目 ID 维护工作区选择，空白输入页也能直接向该项目创建会话。任务显示选中项目全部已提交且未归档的聊天，未选择项目时不显示占位内容；任务区使用剩余高度，超过底部固定栏上缘才滚动，不设像素高度上限。

最近收录全部已提交且未归档的工作聊天，包括项目内和 `IsFolderlessWorkspace` 下的会话；普通聊天模式独立。最近在项目上方，默认收起，展开选择写入 `LayoutState.WorkRecentExpanded`，刷新不改变偏好。打开项目内的最近聊天自动选中对应工作区与任务聊天，保留项目的手动展开状态。`LayoutState.RecentWorkChatIds` 保存跨项目的最近发送顺序，过滤无效 ID，置顶始终在前。项目树和最近各自滚动、各自收起，共享随窗口变化的高度预算，保证任务区始终可用并保留列表虚拟化。最近与任务复用 `WorkChatRowTemplate`，由行数据控制灰色选中背景，原生 ListView 选择关闭以避免蓝色标记；三点菜单与项目内聊天相同，鼠标或键盘焦点进入时显示。

`Services/WorkSidebarState.cs` 集中处理聊天筛选、置顶排序与稳定行更新。仅在消息提交成功时，将未置顶聊天、工作移到各自置顶项之后并更新最近顺序；打开项目或聊天保持顺序，回复的开始/结束仅更新状态标识，流式 token 不触发重排。拖动工作行可在相同置顶分组内调整顺序并保存，普通刷新不覆盖该顺序。切换工作聊天仅生成一次会话展示，不重复渲染，不写入排序和最近记录；主模式未变化时也不重复保存布局。项目树与任务区共享聊天 ID 和操作，不创建第二份记录。`dotnet run --project tests/work-sidebar-smoke/WorkSidebarSmoke.csproj` 检查归属、过滤、排序和稳定刷新。

项目树刷新由 `ShellPage.ProjectRendering.cs` 在当前输入事件结束后合并执行，`ProjectTreeReconciler` 按项目与聊天 ID 保留行对象并增量更新。不要在点击回调中清空整棵树，也不要重新加入 `TreeView.SelectedItem` 赋值；当前聊天的灰色背景由 `IsActive` 控制。重排使用移除/插入相同行对象，避免 WinRT 集合对 `ObservableCollection.Move` 的显示不同步。行卸载和窗口关闭后丢弃悬停、焦点及排队刷新操作。`dotnet run --project tests/project-tree-smoke/ProjectTreeSmoke.csproj` 使用独立原生窗口检查节点更新与生命周期，不访问用户数据。

原生兼容检查：`dotnet run --project tests/math-project-smoke/MathProjectSmoke.csproj` 检查 CSharpMath 降级；`dotnet run --project tests/markdown-ui-smoke/MarkdownUiSmoke.csproj -- --math` 预览原生公式。旧的 `--delimiter-math`、`--physics-math`、`--table-math` 与选择脚本继续用于原生控件回归，主聊天公式、表格和复制应在新的 Transcript 页面检查。
