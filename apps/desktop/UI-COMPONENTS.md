# 前端 UI 维护入口

挂载目录：项目标题悬停显示名称和“文件夹图标＋目录名”；项目三点菜单可更换、取消关联。选择已挂载项目时，`MountedWorkspaceHeader` 在右侧最上方显示紧凑一行，悬停显示完整路径。控件只发送带项目/路径身份的事件，由页面核验当前选择并通过原目录 API 保存；取消关联不删除文件、聊天或记忆。旧应用自动管理目录不显示为外部挂载。右侧截图模块排在目录行下方，继续使用正式工具结果中的 PNG、原有缩略图、切换、缩放和查看大图能力。

- `Controls/ComposerSurface.xaml`：输入框外壳与可选底栏。`Body` 放编辑器和发送工具，`Footer` 放项目选择等操作；`EditorHeight` 只表示编辑器高度，`SurfaceHeight` 包含底栏。两部分共用组件宽度，底栏通过贯穿两行的背景与输入框相连。
- `Controls/PickerMenu.cs`：模型、项目和权限菜单共用的创建入口。`CreateList` 统一列表滚动和选中指示；`WithFixedFooter` 将滚动列表与固定底部操作分开。选择、保存和打开窗口仍由对应的 `ShellPage.*.cs` 处理。
- `Styles/Dimensions.xaml`：字号、行高、按钮尺寸、菜单行高、底栏高度和圆角。`Styles/Controls.xaml` 通过基础按钮样式派生图标、选择器、侧栏操作和发送按钮。
- `Layout/ShellLayoutMetrics.cs`：侧栏、输入框、聊天正文和右栏的默认尺寸及约束。持久化布局的默认值也来自这里；窗口临时变窄时只约束显示宽度，不覆盖用户保存的宽度。
- `Layout/WorkSidebarLayout.cs`：不依赖 WinUI 的最近/项目行高度分配和两组分隔拖动快照。页面只传测量值、转换 `GridLength`、接线和保存偏好；折叠、取消、复位及内部拖动不改变任务边界的规则集中在此，纯布局回归由 `ui-layout-smoke` 验证。

增加 UI 时优先使用现有组件与样式。新的数据保存、协议解析和复杂业务放在负责对应流程的服务中，页面负责展示与事件接线，避免写入通用控件。现有 `ShellPage.*` 仍共享页面状态，不因拆成 partial 就等同于完整 MVVM。

桌面通信：`Services/GatewayResponseReader.cs` 共用 JSON 响应读取及错误解码，保留调用方的 JSON 选项、空响应提示和目录 409 恢复提示。`GatewayApiException` 继承现有 `InvalidOperationException` 并携带 HTTP 状态及可用错误码。模型/会话客户端继续拥有请求、超时和响应释放，SSE 的成功响应不经过 JSON 读取；正式存储仍归网关。`dotnet run --project tests/gateway-response-smoke/GatewayResponseSmoke.csproj` 检查格式兼容、错误降级、取消与响应所有权。

流式回复：`Views/ShellPage.StreamReplies.cs` 按会话 ID 维护生成任务，40ms 合并刷新当前消息；`ModelApiClient.StreamReplyAsync` 与 `ChatStreamReader` 读取网关 SSE，区分正文、公开思考和终止事件。主聊天呈现正文阶段与紧凑工具活动，不为各轮创建独立思考折叠区；发送按钮在生成期间变成停止。消息保存 `Reasoning`、`ReasoningDurationMs`、`Status`、`Error` 和原模型信息，旧记录兼容；启动时把未完成生成恢复为中断。生成开始、定期快照和完成时由网关保存到统一会话日志，页面不再每 1.5 秒重写整个聊天目录；重试复用请求 ID 与用户消息 ID，避免重复上下文。

会话展示：`Controls/ConversationTranscript.cs` 用一个 WebView2 承载整段会话，页面资源在 `Resources/Transcript`，所有用户消息、可见正文阶段、工具活动和表格处于同一份 DOM。输入框、侧栏和模型菜单继续使用 WinUI。主聊天的公式直接由 KaTeX 排版，不经过截图、Skia 或原生文字上方的图片图层；浏览器负责文字、公式与表格的尺寸和换行。

`ConversationTranscript` 按可见正文、阶段内容和生成状态缓存 HTML，40ms 合并刷新；Markdig 解析和代码着色在后台任务中执行。后台仅使用跨越 `await` 前捕获的不可变消息和缓存引用，不读取 UI 拥有的可变字典；关闭控件使解析代次失效并结束尚未完成的 Ready 等待。切换聊天时清除前一会话，异步结果通过会话代际检查后才能发送给页面，避免旧任务覆盖新会话。聊天记录仍保存原始 Markdown，历史消息走相同的展示路径。

`Services/TranscriptMarkdown.cs` 复用 Markdig 和 `MathMarkdown.Normalize`，输出顶层 HTML 块，支持标题、强调、列表、任务清单、引用、代码、链接与真实表格。单元格保留行内节点，公式不再经过纯文本投影。`Resources/Transcript/transcript.css` 统一管理视觉样式：正文 15px、行高 1.7，公式 1.08em，围栏代码 13px。代码长行按可用宽度软换行，保留原始换行和缩进供复制；普通表格随回复区调整宽度，长单词可断行，无法换行的长公式仍可横向滚动。会话视口填满聊天列，正文不再受固定最大宽度限制，右侧栏缩小或收起时同步扩展；滚动条位于聊天列右缘。

`Resources/Transcript/transcript.js` 按顶层块的源 HTML 比较并保留未变化前缀，追加内容只替换变化尾部；已排版公式缓存 DOM 字符串，不重复生成图片。有活动选区时保留现有 DOM 并暂存最新快照，清除选区后再应用最终内容；切换会话会立即清除旧选区并显示目标会话。页面独立维护跟随底部状态：打开聊天到底部，从底部发送后跟随增长，主动向上滚动或选择文字时暂停。主控件保留最多 1,024 条消息、8M 字符的 Markdown/高亮 HTML 缓存，页面保留最近 4 个聊天的 DOM（合计最多 40,000 节点、2M 字符，单个最多 400 条消息）；再次打开复用公式和消息节点，缓存命中不等流式批处理定时器。后台解析在消息边界检查会话代次，快速切换时丢弃过期结果，避免旧聊天覆盖当前聊天。

跨消息选择使用浏览器原生 Range，用户消息按纯文本保留空白。复制时按选区的文档顺序序列化正文、表格和代码，过滤按钮、状态信息及 KaTeX 的辅助 MathML；完整选中的公式复制为带分隔符的原始 TeX，公式内部的部分选择只复制所选可视字符。旧消息的整条复制取原始 `Content`；分段回复按顺序连接各段原始 Markdown，不包含可见思考和工具回执。输入框仍使用原生控件，其复制不经过会话页面。

工具回复支持 `toolStreamProtocol:3` 的 `AssistantSegments`，每轮保存稳定 `id`、`round`、全局 `order`、`phase`、`status`、正文及接口提供的公开思考。桌面接受 `assistant_segment` 开始/完成快照和带 `segmentId` 的增量；每轮完成后根据是否继续调用工具确认 `commentary` 或 `final_answer`，不能根据文字内容猜测最终回答。工具回执带相同轮次和自己的排序位置。`ChatStreamReader` 核对阶段身份、顺序、终态及工具归属，未知未来协议拒绝；旧 v2 事件和只有 `Content`、`Reasoning`、`ToolActivities` 的历史继续兼容，按保存的正文与真实状态应用当前展示投影。

主聊天采用运行中与终态分别投影。`TranscriptPresentation` 与浏览器 `message-presentation.js` 保留全部已有正文阶段，最终回答成功完成并渲染后才移除过程；失败、取消或没有有效最终答案时也保留已显示正文。阶段正文沿用最终正文相同的字体、字号、颜色与 Markdown 样式。可见阶段总计最多八个普通工具，待批准动作及所属阶段额外保留。连续同类工具合并为紧凑行，每组最多显示四个真实网页链接，余数仅作静态计数，完整来源保留于正式记录。动作、状态、命令及网站链接保持 12px 灰色文字，不再为每轮添加思考折叠标题和灰色整行卡。模型只在有实际发现的关键节点给简短阶段回复，不额外生成内部思维或伪造进度。

整条消息成功完成且存在有效最终正文后，只展示最终 Markdown 和顶部的灰色用时；最终正文与公式先完成渲染，再移除主聊天 DOM 中的工具、思考和阶段播报。`DurationMs` 是网关保存的整次模型与工具执行耗时，HTTP/SSE 使用 `durationMs`；不是思考时间，也不在重开时重新计算。旧无段落边界消息沿用保存的 `Content`，有段却没有最终答案的异常消息不按成功展示。失败、取消和截断保留已有正文及简短状态。整条复制和重开使用同一投影，过程记录继续完整保存在正式日志中，后续模型配对历史不变，独立轨迹界面留待后续实现。

截图放在挂载目录行下方的右侧 `ConversationScreenshotsPanel` 内容页，聊天正文和运行活动都不添加「查看截图」按钮。图库取当前聊天完成的 `computer.screenshot` 或明确的浏览器 MCP 截图正式回执，支持 PNG/JPEG，通过顶部标签选择图片，点击图片可放大。`ConversationScreenshotSources` 负责来源与身份检查，`ConversationScreenshotLoader` 读取并验证归档；面板拥有取消、过期结果和有界预览缓存，`ToolResultImageDecoder` 与原有结果查看器共用有界解码。只读取正式归档，不根据工具返回的路径或 Markdown 在界面访问磁盘和网络。侧栏隐藏时不读图片，切换聊天清除旧图；流式正文刷新不重复拉取相同回执。标题、说明和按钮复用既有灰色资源及紧凑样式，语言切换保留当前图片。

挂载行下方使用可扩展的公共内容区；图库按剩余宽高等比增大，略向下居中，不占固定 230 高度。180ms 合并尺寸变化，按显示尺寸和 DPI 升级预览，复用原归档；缓存最多三张及 16M 解码像素。`Views/ScreenshotViewerWindow.cs` 是独立全屏原图查看器，提供适应窗口、100%、滚轮缩放、拖动和 Esc 关闭；100% 按物理像素映射，不放大缩略图冒充原图。长网页适应窗口只缩放布局，原归档像素不改变。当前聊天、消息和回执身份继续校验，聊天切换取消查看器，生产接线也支持自定义名称的浏览器 MCP 截图。

终端不在正式右侧栏渲染，也不创建终端标签。聊天继续显示紧凑的真实工具活动，命令输出和退出状态由网关正式工具回执保存；独立可见控制台仅在 `visible:true` 时打开，不依赖 Python。实时输出事件与 `hostTerminalProtocol:3` 保留兼容，原生助手能力协议保持版本 2，但协议支持不等于当前界面展示。`ConversationTerminalPanel`、`TerminalOutputState` 及其独立验收保留，当前未挂接到正式 Shell。

`ConversationWorkTabs` 是共用的横向标签栏，`ConversationWorkTabState` 保存各聊天的选择与关闭状态，`ShellPage.WorkTabs.cs` 将稳定的聊天／消息／调用身份映射到现有截图查看器。正式右侧栏目前只接入截图标签，仅选中页启用预览；隐藏页取消归档读取和解码。新截图自动添加为后台标签，已有选择不改变；没有打开内容时才自动显示第一项。标签可单独关闭，通过右端打开列表恢复，横向滚动位置在后台更新时保留，内容随右侧栏缩放。用户收起右侧栏后新截图不强制打开它。标签键不包含可变化的归档版本，避免更新后重新打开已关闭项。附件后续沿用这一入口，目前未新增附件查看器。

`ToolManagementWindow.Browser.cs` 只协调浏览器设置展示与草稿；纯参数转换由 `BrowserConnectionSettings` 负责，兼容布尔值的等号与独立参数写法，直接可执行文件的配置也保留原选中配置目录。暂含无效 JSON 成员的编辑草稿不使窗口崩溃，正式保存仍拒绝无效成员。三种模式为独立本机、现有登录浏览器和远程地址，显示窗口选项只适用于独立模式。自定义配置、环境连接不被简单模式覆盖；切换 HTTP 服务隐藏本机参数。语言即时切换会刷新选中项但保留模式、地址及未保存状态，保存不自动启动 MCP。Chrome 现有浏览器提供打开浏览器连接设置入口，初次调试允许仍在浏览器完成。

`McpConfigurationInput` 集中处理环境变量、HTTP 请求头引用与 Bearer/OAuth 编辑输入，只校验引用名和配置结构，不读取凭据值。工具管理窗口仅释放自己创建的 API 客户端；注入客户端的生命周期属于调用方，关闭窗口仍取消自己的列表与预览请求。右侧标签 UI 状态最多保留 16 个聊天，并保留每个聊天最近 256 个暂时缺席资源的关闭标记；空/部分历史投影不会直接撤销这些关闭选择，也不会显示已缺席资源，正式记录不受 UI 状态缓存淘汰影响。

`tool-presentation.js` 提供本地化友好动作，未知工具显示「执行操作」。聊天不创建参数/结果 JSON、调用 ID、哈希、沙箱元信息和完整结果按钮；审批窗口与独立结果查看器继续保留。C# 只为可见阶段解析 Markdown/公式并复用未变 HTML 和 DOM，隐藏的思考不再解析，增量仍以 40ms 批处理。收束过程沿用选区冻结，用户上滚时补偿稳定阅读锚点，底部跟随只对原本正在跟随的用户生效。当前记录尚不能准确重放同轮正文与公开思考的任意多次交错，不根据文字猜额外阶段。验证入口为 `chat-stream-smoke`、`agent-transcript-smoke/run.ps1` 和真实 C# DTO→WebView2 的 `transcript-ui-smoke`。

查询网站的单项直接显示工具状态与可点击网址，不再提供参数、原始结果 JSON 或引用详情的二次展开。`tool-web-links.js` 只识别明确的网站 MCP 工具，从业务 URL、搜索的 `URL:` 行/结构化结果和浏览器当前页面地址提取 HTTP(S) 链接；不从审批理由、私有 `_meta`、脚本或任意文件内容搜网址。浏览器优先返回的最终/当前页，排除后台标签；相同网址去重，每项最多 32 个。尚未返回网址的搜索显示简短原查询，未知 MCP 保持普通展示。链接沿用宿主打开机制，不在聊天 WebView 中访问网络；小号灰色文字可软换行和完整选择复制。该模块只影响展示，不授予执行权限，也不把返回地址当成已验证事实。

聊天页面只加载打包的本地 HTML、脚本、样式和字体。Markdown 原始 HTML 被转义，图片显示为文本链接；只有 `http`、`https`、`mailto` 地址可以作为链接交给宿主打开。WebView2 拦截其他页面导航、资源请求、下载和权限请求，不在排版时获取远程图片或脚本。

代码高亮：`Services/CodeSyntaxHighlighter.cs` 使用 [ColorCode.Core](https://github.com/CommunityToolkit/ColorCode-Universal) 解析围栏语言，`TranscriptMarkdown` 将文字片段输出为彩色 `span`；复制仍使用完整代码文本。支持 Python、JS/TS、C#/C++、Java、JSON、SQL、HTML/CSS、XML/XAML、PowerShell 等及常见缩写。关键字紫色、字符串深蓝、注释灰绿、数字蓝色；不改动缩进和换行。未标语言、未知语言以及超过 40,000 字符或 4,096 个文字片段的代码保留等宽纯文本。

解析与协议检查：`dotnet run --project tests/transcript-markdown-smoke/TranscriptMarkdownSmoke.csproj` 验证 HTML 结构、表格内公式、列对齐、代码保真、安全链接和流式转最终结果；`dotnet run --project tests/code-highlighting-smoke/CodeHighlightingSmoke.csproj` 验证代码语言匹配与降级；`dotnet run --project tests/chat-stream-smoke/ChatStreamSmoke.csproj` 验证 SSE 分包、真实 HTTP 增量、取消与记录兼容。`node tests/streaming-ui-fixture.mjs <空临时目录>` 提供独立模拟上游与真实网关，不含密钥，不应接到日常使用的模型配置中。

主聊天 UI 检查：`dotnet run --project tests/transcript-ui-smoke/TranscriptUiSmoke.csproj` 使用独立 WebView2 会话，检查多公式表格、跨消息选择、完整与部分公式复制、选区期间的最终更新以及打开和跟随底部；`--keep-open` 保留测试窗口。结果写入 `%TEMP%/kynxa-transcript-smoke.txt`，耗时与指标写入同目录的 `kynxa-transcript-smoke.json`，不读写用户聊天。

保留的原生控件：`MarkdownReply.*`、`TextSelectionAutoScroll`、`ConversationAutoFollow` 仍供原生控件与旧测试使用，其 `RichTextBlock`、原生表格、图片公式和鼠标捕获逻辑不再控制主聊天。`tests/markdown-ui-smoke`、`tests/scroll-follow-smoke` 及跨消息选择脚本只验证这些原生路径；它们通过不代表新的浏览器会话展示已经通过 UI 检查。

`Services/StoragePaths.cs` 统一解析桌面数据位置，与网关共享 `~/.kynxa/storage.json`（`dataRoot`）或 `KYNXA_DATA_HOME`。模型选择、布局和界面错误日志使用该目录下的 `Desktop`；未配置时兼容原 LocalState。`ProjectStore` 现在是异步会话 API 客户端，项目元信息与聊天正文的唯一正式来源在网关 `ConversationStore`；正文按项目/会话目录保存到 Data 下的 JSONL，项目关联文件夹独立。目录保存只上传元信息和新用户消息，不回写助手快照；版本冲突要求重新加载。移动已有数据使用网关目录下 `migrate-storage.mjs`，切勿只改路径导致旧记录不可见。

模型管理：`ModelManagementWindow.xaml` 使用固定底部操作区、独立滚动表单和折叠的高级设置；服务地址直接显示在主表单，连接 ID 位于高级设置。`Services/ModelPresets.cs` 集中维护服务地址、默认模型 ID、名称识别及本机/局域网地址判定。「本地 API（直接连接）」不依赖 Ollama，直接连接兼容接口，密钥提示随地址更新。新连接自动避开已有 ID，编辑连接固定 ID；切换服务商建立新配置，不修改已有连接。测试只读取模型列表，保存后通过聊天模型菜单选择。

上下文窗口按连接保存，可选择 8K、32K、128K、256K、1M（1000000）或自定义 2048–2000000；旧连接缺少字段时默认 8192。独立最大输出默认 256K，提供 4K、8K、16K、32K、64K、128K、256K 和自定义 1024–262144，已有 2K 等值保留；同一灰色 TokenLimitRowStyle、字号与选择/校验助手覆盖两行。输出是上限，小上下文会降低实际预留。这些配置不改变服务端能力，本地服务须匹配启动窗口。保存/测试先校验，两项均整数往返；切语言刷新选中标签并保留草稿，忙碌禁用全部预算控件。`tests/model-context-smoke` 验证通信，`tests/model-ui-smoke` 用临时 Data、模拟 HTTP 和真实 WinUI 验证表单、窄窗、关闭与晚到响应。聊天/工作/用户记忆由网关分层管理，详见 [记忆架构](../../docs/architecture/chat-work-memory.md)。

设置中的「记忆管理 → 打开」创建独立原生窗口，可最小化、最大化和关闭。`MemoryManagementWindow` 负责展示与确认，`MemoryManagementViewModel` 管理编辑、版本冲突和取消，`MemoryApiClient` 负责范围 HTTP 接口，`apps/shared/Memory/MemoryApiContracts.cs` 定义共享 DTO 与内容校验。窗口分别选择聊天、工作、全局记忆，支持内容筛选、手动新增、编辑、单条删除，显示来源与有效状态；加载失败明确提示，写入和 409 后重新 GET，保留冲突编辑。未发送草稿不纳入聊天目标，全局/工作管理不创建聊天。中英界面即时切换，用户内容原样保留，窄窗口上下排列列表与编辑器。更改 Data 位置前须完成编辑，迁移关闭干净的记忆窗口。

验证入口：`tests/memory-management-smoke` 覆盖模拟 HTTP 与状态，`tests/memory-ui-smoke` 用隔离临时目录、模拟 API 和真实 WinUI 控件检查窗口。输出配置已实现，SSE 终态可选 contextUsage 诊断尚未显示为桌面用量控件。聊天展示保持现有 Markdown DOM、稳定消息 ID 与跨消息选择行为，验收安排见 [B：桌面前端](../../docs/team/B-桌面前端.md)。

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

## 存储设置

工具管理窗口使用 MCP、技能两页。常用操作和启停保留在主视图，服务 ID、JSON、认证和工具目录折叠到高级/详情区域；技能来源、内容、依赖诊断及外部目录同样按需展开。收起区域保留原编辑控件及草稿，不清除配置；灰色焦点和字体沿用现有资源。原生 UI fixture 验证折叠、切页、尺寸及原有保存/冲突/诊断流程。

`Controls/StorageLocationRow.cs` 为数据存储、用户工具共用的三列灰色行：名称、截断的路径及完整 tooltip、更改位置按钮。`ShellPage.StorageSettings.cs` 创建设置窗口，`ShellPage.ExtensionStorageSettings.cs` 处理文件夹选择、窗口生命周期、草稿检查、维护等待与成功后 health 核对；UI 不直接保存 MCP 配置。两行在迁移时一起停用，共用维护锁，先验证扩展存储协议与当前实际根，再调用 `ExtensionStorageMigrationService`。复制后指针原子激活，网关安全关闭旧客户端再加载新位置；状态只在操作后显示，中英文即时切换。

`ExtensionPaths` 的纯解析器可独立测试；未设置独立扩展指针时使用 Data 的旧位置，Data 迁移与技能 ID/缓存路径同步，已独立配置时保持扩展根。`tests/extension-storage-smoke` 覆盖复制、校验、配置重写、并发、取消、链接和 Data 兼容；`tests/agent-ui-smoke` 用真实存储行和模拟文件夹选择检查灰色样式、路径与状态，未弹出系统文件夹选择器。

## 数学公式与工作排序

`MathMarkdown` 将 `\(...\)`（含同一段落内的跨行内容）、`\[...\]` 转为 Markdown 数学分隔符，也支持 `$...$`、同一行或跨行的 `$$...$$`。行内公式不能跨越空段落或代码边界。单独方括号包围且包含数学运算符的段落可兼容恢复；普通括号、代码块和行内代码不猜测转换。显式标记为 `math` 的围栏代码块在生成完成后显示为公式；`tex`、`latex` 及其他语言仍按代码展示。

主聊天由 `TranscriptMarkdown` 输出带 `data-latex`、`data-display` 的数学节点，页面直接调用固定版本 [KaTeX 0.16.47](https://github.com/KaTeX/KaTeX/tree/v0.16.47) 渲染为 DOM。原始 TeX 保留在节点数据中，供复制使用；未闭合的流式块公式先显示源码，`math` 围栏到生成完成后再转换。资源、字体和 MIT 许可保存在 `Resources/Math` 并随软件发布，主聊天不进行公式截图，也不依赖图片缓存。

`KatexFormulaRenderer`、`MarkdownReply.Math` 的图片路径及 `MathFormulaRenderer` 的 CSharpMath/SkiaSharp 降级仍保留给原生控件。它们的公式长度、图像尺寸限制和 TeX 兼容改写属于原生渲染实现，不应套用到浏览器主聊天。浏览器渲染始终接收原始 TeX，避免为较小的解析器改写公式含义。

工作侧栏分为最近、项目、任务。点击项目名称选择工作区，旁边的原生箭头独立展开聊天；选择项目、打开聊天、发送消息和三点操作均不自动展开项目。`ShellPage.WorkSidebar.cs` 按项目 ID 维护工作区选择，空白输入页也能直接向该项目创建会话。任务显示选中项目全部已提交且未归档的聊天，并可临时显示该项目当前活动的未发送草稿；未选择项目时不显示占位内容。任务区使用剩余高度，超过底部固定栏上缘才滚动，不设像素高度上限。

项目行右侧的新增聊天按钮与任务标题旁的新增按钮共用 `StartNewProjectChat`，创建初始标题为“新聊天”（重名时编号）的内存草稿，立即选中并显示在任务列表中；任务标题按钮只在选中有效项目且鼠标或键盘焦点进入标题时显示。全界面只保留一个活动项目空稿，再次新增替换前一空稿；重新选择同一项目保留草稿及输入，切换到其他项目丢弃未发送草稿。`WorkSidebarState.ProjectChats(project, activeDraftId)` 只为当前真实、未归档项目开放这一个临时任务行，最近、项目树与正式存储仍仅收录已提交聊天，folderless 或归档项目不显示临时任务行。首条消息提交后才正式保存并更新最近及未置顶顺序；创建、重选或打开草稿不更新 MRU。

最近收录全部已提交且未归档的工作聊天，包括项目内和 `IsFolderlessWorkspace` 下的会话；普通聊天模式独立。最近在项目上方，默认收起，展开选择写入 `LayoutState.WorkRecentExpanded`，刷新不改变偏好。打开项目内的最近聊天自动选中对应工作区与任务聊天，保留项目的手动展开状态。`LayoutState.RecentWorkChatIds` 保存跨项目的最近发送顺序，过滤无效 ID，置顶始终在前。项目树和最近各自滚动、各自收起，共享随窗口变化的高度预算，保证任务区始终可用并保留列表虚拟化。最近与任务复用 `WorkChatRowTemplate`，由行数据控制灰色选中背景，原生 ListView 选择关闭以避免蓝色标记；三点菜单与项目内聊天相同，鼠标或键盘焦点进入时显示。

`Services/WorkSidebarState.cs` 集中处理聊天筛选、置顶排序与稳定行更新。仅在消息提交成功时，将未置顶聊天、工作移到各自置顶项之后并更新最近顺序；打开项目或聊天保持顺序，回复的开始/结束仅更新状态标识，流式 token 不触发重排。拖动工作行可在相同置顶分组内调整顺序并保存，普通刷新不覆盖该顺序。切换工作聊天仅生成一次会话展示，不重复渲染，不写入排序和最近记录；主模式未变化时也不重复保存布局。项目树与任务区共享聊天 ID 和操作，不创建第二份记录。`dotnet run --project tests/work-sidebar-smoke/WorkSidebarSmoke.csproj` 检查归属、过滤、排序和稳定刷新。

项目树刷新由 `ShellPage.ProjectRendering.cs` 在当前输入事件结束后合并执行，`ProjectTreeReconciler` 按项目与聊天 ID 保留行对象并增量更新。不要在点击回调中清空整棵树，也不要重新加入 `TreeView.SelectedItem` 赋值；当前聊天的灰色背景由 `IsActive` 控制。重排使用移除/插入相同行对象，避免 WinRT 集合对 `ObservableCollection.Move` 的显示不同步。行卸载和窗口关闭后丢弃悬停、焦点及排队刷新操作。`dotnet run --project tests/project-tree-smoke/ProjectTreeSmoke.csproj` 使用独立原生窗口检查节点更新与生命周期，不访问用户数据。

原生兼容检查：`dotnet run --project tests/math-project-smoke/MathProjectSmoke.csproj` 检查 CSharpMath 降级；`dotnet run --project tests/markdown-ui-smoke/MarkdownUiSmoke.csproj -- --math` 预览原生公式。旧的 `--delimiter-math`、`--physics-math`、`--table-math` 与选择脚本继续用于原生控件回归，主聊天公式、表格和复制应在新的 Transcript 页面检查。

记忆管理已从设置打开独立窗口，具有记忆 HTTP 客户端和聊天/工作/全局范围的查看、来源状态及单条新增/编辑/删除。实现遵循 [接口与验收切分](../../docs/architecture/chat-work-memory.md#下一轮实施切分)，使用范围文档版本检测冲突、写入后重新获取来源状态。未发送空稿不因打开记忆入口而正式保存；快速切换聊天时，晚到列表和写入结果只能更新其所属会话。独立输出配置可并行开发，批量清除及预算诊断等待对应后端/协议实现。

工具管理窗口使用 API 提供的官方/用户来源和只读根目录，可筛选来源、保存官方覆盖或恢复默认；用户项保留增删改。官方文件在本体 `model-gateway/official-tools/`，配置路径只迁移用户扩展；官方技能稳定 ID 与用户路径 ID 分开。语言切换保留选择和草稿。
