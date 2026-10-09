# 前端 UI 维护入口

## 代码与纯文本框的独立复制

普通 Markdown 围栏（包括未指定语言、`text`、`plaintext`、JSON、命令、配置及未知语言）和缩进代码块共用 `Resources/Transcript/content-blocks.js/css`。每框顶部左侧显示语言／文本类型，右侧常显独立复制按钮；操作栏位于源码上方，使用共享字体、配色及圆角。长类型名显示省略号，不挤出复制按钮。只有模型原文已标记为代码块的内容会成框，普通段落、引用、列表和行内代码不自动转成框。

按钮点击时读取该框 `code.textContent`，保留缩进、制表符、空行和换行结构，不带 Markdown 围栏、语言标签或高亮 HTML。浏览器文本使用 LF 换行；整条消息复制仍保留原消息的 LF／CRLF。流式输出及被中断的未闭合围栏提示“复制当前内容”；已闭合框即使后文仍生成，也使用普通复制提示。选区冻结期间复制当前可见 DOM，不取尚未显示的后续消息内容。

复制复用现有原生剪贴板桥接，按会话、消息和独立请求 ID 核验，只有成功回执才显示对号与“已复制”，约2.2秒恢复；失败保留复制图标，并在同一位置显示“复制失败，请重试。”。成功和失败都不再触发页面悬空提示，反馈期间的按钮与小字不设置重复 title，保留 aria-label 与状态公告。普通操作说明在反馈结束后恢复。按钮可用 Tab、Enter、Space 操作，鼠标点击保留正文选区。操作栏有 `data-copy-ignore`，跨框／跨消息选区复制排除控件文字；整条消息复制仍返回原始 Markdown。

一对一包装原 `pre` 保留增量渲染块数量、原 `code` 及未变化前缀实例；包装在正常快照应用后、恢复阅读锚点之前完成，有选区时延后。切换会话清理待处理回执和反馈计时器，缓存恢复复用原控件与监听。语言／配色只更新展示，正式消息字段和网关接口不变。

Mermaid 继续使用专用“复制源码”，公式源码不重复添加按钮；表格 TSV／CSV、公式 TeX 按钮、图片／图表导出、长框收起不在此批范围。测试入口为 `transcript-markdown-smoke` 及 `transcript-ui-smoke --block-copy-only`，后者使用真实 WinUI/WebView2 和剪贴板。

## 模型回复中的 Mermaid 图表

助手 Markdown 中的完整 `mermaid` 围栏可展示流程图（`flowchart`/`graph`）、时序图、状态图、类图、实体关系图、甘特图和思维导图。`TranscriptMarkdown` 通过 Markdig 的实际闭合围栏计数标记就绪；未闭合时无论流式或最终状态都显示源码与等待提示。完整块可在回复尚未结束时绘制，普通代码、公式和原始 Markdown 不变。

`Resources/Transcript/diagrams.js` 管理串行队列、源码回退、折叠源码、复制回执、缩放和平移及模态全屏查看；`diagrams.css` 负责当前配色及宽度适配。空白处拖动平移，Ctrl＋滚轮或加减按钮缩放，0/适应按钮复位，Esc 关闭查看器并返回原按钮焦点。在聊天正文中完整选中图表时复制一次原始源码；只选中部分图中文字、从正文跨入局部图表，或在全屏查看器内选中文字时，保留实际选中的可见文字，排除 SVG 样式及操作控件。复制源码使用现有原生剪贴板接口，成功回执后显示对号约2.2秒。

`diagram-renderer.html/js` 在 `sandbox="allow-scripts"` 且无同源权限的本地 iframe 中加载固定的 Mermaid 12.1.0 完整浏览器包，禁止联网、下载、弹窗、表单和顶层导航。源码预检查拒绝前置配置、初始化指令、已识别的危险 HTML 标签或属性、外部图片/图标，以及非 mindmap 中明确的 click/link 动作；比较表达式和普通 links、click 等节点文字保持可用。预检查用于提前提示不支持的输入；实际安全边界由固定的 strict 与 htmlLabels:false、沙箱、CSP、结果来源校验及 SVG 白名单重建共同保证，不调用交互绑定，也不将任意 HTML 插入页面。重建移除脚本、事件、外链、foreignObject 和动画。内置文件出处、校验和与原许可证保留在 `Resources/Diagrams/Mermaid`，无需 CDN。

单块源码上限50,000字符、边数上限800；另有限制 SVG 大小、节点数及异步等待时间。语法或安全检查失败仅显示短提示和完整源码，大小限制不截断原文。超时可恢复异步失败，不保证硬中止浏览器同一线程上的任意同步复杂布局，过于复杂的图应拆分。Graphviz、PlantUML 和远程图标不在当前范围。

聊天接线在 `transcript.js`：图表结果在正文或全屏图表选择期间延迟应用，释放后合并；上滚阅读时保留滚动锚点，切换会话丢弃过期渲染结果，全屏图不跨会话保留。主题变化仅重绘图表，保留正文和查看位置；缓存会话恢复后重接尺寸监听并刷新当前语言的控件文字。原生控制只增加本地化文案与静态资源打包，不新增模型提示、共享 DTO、后端接口或持久化字段。

验收入口为 `transcript-markdown-smoke` 和 `transcript-ui-smoke --diagrams-only`，完整 Transcript 套件也覆盖图表。真实 WebView2 检查类型、中文与英文、换行与长标签、未闭合等待、失败回退、安全输入、原生复制对号、480/980布局、配色、缩放平移、全屏Esc与缓存/迟到结果；运行方法见测试 README。

## 浅色配色与本机外观设置

设置的“外观”区提供经典灰、雾蓝、青绿、柔紫、暖砂五套浅色预设，默认雾蓝。卡片按宽度排列为三、二或一列，显示预览、名称与选中勾号；点击立即应用，恢复默认只重置配色。名称与状态支持中英文及键盘访问。

`Models/UI/AppearancePalette.cs` 定义语义颜色，`Services/Presentation/AppearanceService.cs` 统一预设、共享画刷与窗口标题栏。保留已有资源键并修改画刷颜色，使静态资源使用者即时更新；原生主界面、模型/记忆/工具/检索设置、审批与结果弹窗使用相同焦点和选中资源。原生输入框使用强调色选区以适配白色选中文字；WebView 使用浅色选区与深色文字，分别验证对比度。成功、错误、权限语义色及禁用发送的可读灰色保持独立。输入区外框随实际焦点切换，不修改发送条件。

`Views/ShellPage.Appearance.cs` 负责卡片展示与接线，`LayoutState.AppearancePaletteId` 通过现有 `layout.json` 保存。未知或缺失值回退雾蓝；先完成原子保存，再应用外观，保存失败保留原配色并提示。迁移期间拒绝更改。启动在创建主窗口前恢复配色，窗口关闭时释放外观订阅。

WebView 使用内部 `setAppearance` 展示消息更新 CSS 变量及默认背景，不重建正文、缓存、选区、复制反馈或滚动状态。配色不进入模型请求、共享 DTO 或正式聊天存储。当前仅开放浅色预设；后续显示模式应独立于预设 ID 扩展，并补齐深色资源和可读性验收。

实际验收入口为 `shell-migration-ui-smoke --appearance-only`，覆盖五款切换与偏好重载、真实原生和 WebView 颜色、草稿/正文/选区/滚动保留、保存失败、迁移守卫、窄窗英文布局与焦点状态。启动命令及证据见该套件 README。

## 单条消息复制与行内时间

消息底部操作行常显，顺序为复制图标、时间、适用时的重试按钮；时间使用浅灰12px文字、等宽数字及完整本地日期和秒数，窄窗允许自然换行。用户显示发送时间，成功助手显示回复完成时间，中断或失败显示回复结束时间，生成中显示“正在生成”。消息不再设置 `article.title` 或时间悬停说明；语义化 `time` 元素直接提供可读文本，只有已知时刻才设置ISO `datetime`。操作行继续标记 `data-copy-ignore`，时间不写入正文、导出或复制内容。

消息复制仍由原生剪贴板回执确认后显示对号，约2.2秒恢复，图标与时间的位置在回执前后不变。成功／失败在操作行显示局部小字，反馈节点位于原复制、时间、重试节点之后；空闲时不占空间，不生成重复 title 或页面全局提醒。Mermaid 复制也在自己的操作栏显示局部状态。原生只回传 copyResult，已移除复制全局反馈事件及 Shell 订阅；导出、缓存等其它功能继续使用原有页面提醒。顶部原有用时展示独立保留。

`Services/Presentation/MessageTimePresentation.cs` 保留本机首次观察到的终态时刻，通过 `MessageEndTimeCache` 将显示元数据异步保存到当前 `Desktop/MessageTimes/<聊天ID>/<消息ID>.json`。文件仅含版本、稳定ID、状态、最终正文的SHA256摘要和UTC结束时刻，不含聊天正文、思考或凭据，不修改 `ChatMessageState`、共享DTO、模型请求及网关正式日志。用户发送时间继续来自既有 `CreatedAt`；助手时间表示本机接收结束的观测，未升级为网关正式结束字段。

重新打开聊天时按聊天ID、消息ID、终态及最终正文摘要匹配缓存，读取分批且异步，旧数据根、已切换会话或已改变的回复不能收到迟到的显示更新；不依赖客户端和网关创建时刻一致。旧历史没有缓存则显示“未记录”，不能用 `CreatedAt + DurationMs` 推算补填。重试开始前等待旧写入、清除旧缓存并保留新尝试标记；相同ID和相同正文的新尝试仍可保存新时间。删除聊天在正式操作成功后清除该聊天的缓存，删除前读取已有观测，撤销可重写原时间，其他聊天不受影响。

写入采用有时限的跨进程锁、同目录唯一临时文件及原子替换；正常关闭和数据迁移前等待已发起的缓存写入。迁移仍使用现有Desktop完整复制，切换目录后重新绑定缓存，不新增后端迁移逻辑。损坏、超大和未知版本的缓存不覆盖，读写失败不改变聊天状态，写失败以简短本地化反馈提示；当前运行中的时间仍可显示。尚未收到终态时强制退出不会伪造完成时刻。缓存未设时间过期，删除对应聊天或移除缓存文件后不保证恢复；它是本机辅助显示记录，不提供跨设备时间一致性。

验证入口：纯.NET `tests/message-time-cache-smoke` 覆盖独立进程退出后读取、并发、失败保护、重试及撤销；原生 `transcript-ui-smoke --message-time-cache-only` 清空进程内观测并从原始JSON新建对象，确认实际DOM恢复正确时间。后者不宣称重新启动了整个桌面程序。

## 全页面细节补充：短窗、状态与结果阅读

- 主输入工具栏根据已有控件的实际宽度分配模型菜单；窄窗将权限入口收为图标，完整名称和说明仍保留在可访问名称及提示中。权限和项目菜单受当前窗口宽高限制。临时侧栏支持 Escape 收起和焦点返回，已有模态与打开菜单优先处理。
- 展开侧栏顶部在“工作／聊天”右侧提供收起按钮，折叠后由原展开按钮恢复；两者复用同一切换入口并交接键盘焦点。标题栏更多菜单的原入口继续可用。窄窗关闭的是临时抽屉，不改宽窗折叠偏好或保存宽度。侧栏边缘的8 DIP拖动区域与侧栏同色，SidebarGrip仅隐藏视觉提示线，拖动、取消和复位逻辑保留；其他分隔条不受影响。
- 最近、任务、普通聊天列表与项目树的行统一使用8 DIP圆角及右侧滚动条留白。共享侧栏样式沿用当前SDK的原生ListViewItemPresenter模板属性，将圆角绑定到容器，并统一悬停、按下与选中背景；工作聊天与项目树的活动背景使用相同正内缩，对齐原生背景的左右4 DIP、上下2 DIP边界，避免点击后背景扩大。整行命中高度仍为40 DIP，文字和操作按钮使用内层边距，不再通过负边距扩展背景。圆角、行外边距与背景内缩集中来自Styles/Dimensions.xaml，保留原生容器、键盘焦点、选择与虚拟化。
- 模型窗口用真实编辑状态显示“尚未保存／未保存的修改／已保存”。长连接名和模型 ID 只在展示时省略，完整提示和保存原文保留；原校验失败时定位输入框，并展开其所在高级区域。
- MCP 与技能窗口按短窗高度约束列表；窄窗把已有连接操作和目录设置移动到现有滚动区域，放宽后恢复。清除筛选只恢复展示，隐藏的草稿仍保留。忙碌、未保存、取消及保存按钮共用原状态判断。
- 设置窗口按 DPI 和显示器工作区定位，`StorageLocationRow` 在窄行把可选择复制的路径放到下一行。记忆范围入口采用等宽列，英文文字可换行，数据与作用域仍由原接口处理。
- 工具审批和结果弹窗对长名称、目录路径及短窗使用换行和滚动。结果复制显示成功或失败反馈，复制数据取已加载分页原文；原生文本框的显示换行不能充当复制来源。图片只限制弹窗缩略图解码，完整归档保持不变。
- 超长行内公式在当前回复宽度内独立横向滚动；短公式仍位于原文字行内，选择复制继续使用原 TeX。截图查看器读取失败后可重新读取同一会话的同一归档，不执行新截图；关闭或取消后丢弃迟到读取。

相关原生验证入口为 `shell-migration-ui-smoke`、`model-ui-smoke`、`agent-ui-smoke`、`memory-ui-smoke`、`transcript-ui-smoke` 和 `screenshot-panel-ui-smoke`。网关、DTO、工具宿主、模型和数据客户端无新增协议；具体证据及限制见实施记录第 9 节。

## 页面细节优化：按钮、菜单与空预览

- 发送按钮的空输入状态使用 `KynxaSendDisabledBrush` 和独立深灰箭头 SVG；有内容时使用当前配色的主题色背景、白色箭头，生成期间停止图标明确为白色。启用与停止的原判定不变。附件上传尚未实现，入口停用并说明可用的粘贴文本、关联项目目录操作。
- `ComposerSurface.xaml` 减轻原生阴影并使用较浅底栏；默认编辑器高度为 104 DIP，已有用户高度偏好不重写。空白页减小品牌图及波纹透明度。
- 模型和会话搜索菜单按首次打开的内容确定高度，超出可用高度后滚动；输入搜索词时保持菜单高度稳定。`PickerMenu.SetContentWidth` 为这两类菜单设置局部外壳宽度，其它菜单沿用原样式。模型行仅在名称与 ID 不同时显示第三行，当前模型以勾选标记显示；行包装只用于呈现，选择和保存仍传原 `ModelChoice`。
- `ShellPage.WorkPanel.cs` 没有打开的预览标签时默认释放右栏，已有文件夹仍可通过原按钮展开。手动空面板请求按模式、项目和会话限定；自动收起不保存布局、不改变用户宽度或显式关闭偏好。截图正式回执的发现继续独立于栏位显示，宽窗且用户允许展开时可显示首张截图，关闭标签的原状态仍保留。

上述行为由 `shell-migration-ui-smoke` 的真实生产页面检查覆盖，包括按钮模板实际配色、内容菜单尺寸、空项目切换、窄窗发现截图及真实客户端归档读取。测试使用独立包身份与虚构数据。

## 本轮迁移：导航、草稿保护与窄窗适配

- 页面级快捷键使用 `KeyboardAcceleratorPlacementMode.Hidden`，避免鼠标停在聊天正文时 WinUI 自动弹出首个 `Ctrl+F` 提示。快捷键仍正常响应，搜索按钮自己的中英文说明提示、菜单文字和 F1 帮助保留。
- `Views/ShellPage.PresentationActions.cs`：更多功能菜单、会话标题/项目搜索、整段会话复制及 Markdown 导出、短暂操作反馈和快捷键。搜索重新核验聊天/项目 ID，复用原选择入口恢复草稿；导出在文件选择器等待前捕获可见正文快照。审批、设置等同一 XamlRoot 的弹窗打开时，快捷键和导航先返回。
- `Views/ShellPage.ModelPicker.cs`：模型/连接搜索、读取中/空列表/失败/已选择状态。失败保留上次成功列表和选择，只有成功读取权威列表后才清除失效选择。状态提示明确区分已选择模型与已验证模型调用，菜单底部配置/重试入口保持固定。
- `ModelManagementWindow.EditorState.cs`、`.Layout.cs`：连接切换、服务商切换、新增及原生关闭前保护未保存字段，包括仅修改密钥和 token 原始草稿。密钥不进入比较快照。窄窗移动已有控件，保留草稿、焦点和滚动；保存/测试仍调用原 ModelApiClient。
- `ToolManagementWindow`：MCP/技能搜索和来源筛选仅改变显示，隐藏的当前编辑项保留身份、草稿及预览。`ShowSection(bool skills)` 复用同一窗口切页。窄窗上下排列列表与编辑器，原 dirty、busy、revision 冲突和迟到预览保护保持生效。
- `Controls/ConversationTranscript.cs` 与 `Resources/Transcript`：复制按钮等原生剪贴板确认后再显示结果，按聊天/消息/请求身份丢弃过期反馈；离开底部时提供跳转最新消息。选区冻结及手动阅读保持有效。首次打开聊天的底部滚动帧核验导航代次和聊天 ID，避免迟到滚动通知取消导航或污染另一会话。
- `Services/Presentation/ConversationExport.cs`：使用原 `TranscriptPresentation` 投影生成可见正文，不改写网关归档；保留 Markdown、TeX 和代码缩进。成功最终回复不导出隐藏过程，取消/失败仍按原展示规则保留已显示正文。
- `Layout/ShellLayoutMetrics.cs`：主页面小于 980 DIP 时用临时抽屉；改变窗口尺寸不保存抽屉状态或覆盖用户侧栏宽度。输入区增加 Enter/Shift+Enter 提示，空白或准备数据期间停用发送；项目操作完成后恢复发送状态。

界面入口中“连接”和“技能”打开已实现的管理窗口。“知识库”复用新版全局检索设置窗口，保留目录就绪、存储迁移、未保存配置和窗口复用保护；项目级检索设置仍从项目菜单进入。“定时任务”说明当前开放范围；记忆管理仍使用原设置入口。

以上实现位于桌面展示与交互代码。网关、共享 DTO、工具宿主和 `Services/Data`、`Services/Models`、`Services/Tools` 的接口/保存逻辑不因本轮迁移修改。窗口文件位于 `apps/desktop` 根目录，是现有 WinUI 页面结构的一部分；不应为目录整齐另行重写后台模块。

相关隔离检查：`model-ui-smoke`、`agent-ui-smoke`、`transcript-ui-smoke`、`shell-migration-ui-smoke`、`conversation-export-smoke`。正式 Shell 夹具链接生产页面源码和资源，使用独立 MSIX 身份、临时数据、模拟网关和独立 WebView 目录。测试结果以实际日志为准，实施记录见 [UI 迁移实施与验证记录](../../docs/ui/UI迁移实施与验证记录.md)。

挂载目录：项目标题悬停显示名称和“文件夹图标＋目录名”；项目三点菜单可更换、取消关联。选择已挂载项目时，`MountedWorkspaceHeader` 在右侧最上方显示紧凑一行，悬停显示完整路径。控件只发送带项目/路径身份的事件，由页面核验当前选择并通过原目录 API 保存；取消关联不删除文件、聊天或记忆。旧应用自动管理目录不显示为外部挂载。右侧截图模块排在目录行下方，继续使用正式工具结果中的 PNG、原有缩略图、切换、缩放和查看大图能力。

- `Controls/ComposerSurface.xaml`：输入框外壳与可选底栏。`Body` 放编辑器和发送工具，`Footer` 放项目选择等操作；`EditorHeight` 只表示编辑器高度，`SurfaceHeight` 包含底栏。两部分共用组件宽度，底栏通过贯穿两行的背景与输入框相连。
- `Controls/PickerMenu.cs`：模型、项目和权限菜单共用的创建入口。`CreateList` 统一列表滚动和选中指示；`WithFixedFooter` 将滚动列表与固定底部操作分开。选择、保存和打开窗口仍由对应的 `ShellPage.*.cs` 处理。
- `Styles/Dimensions.xaml`：字号、行高、按钮尺寸、菜单行高、底栏高度和圆角。`Styles/Controls.xaml` 通过基础按钮样式派生图标、选择器、侧栏操作和发送按钮。
- 普通界面文字统一使用 `Segoe UI Variable Text, Segoe UI, Microsoft YaHei UI, Microsoft YaHei`：英文优先 Segoe UI Variable Text，中文回退到微软雅黑 UI。`KynxaUIFont`、普通控件的隐式字体样式及动态弹出层共享该规则；标题栏也使用同一资源。聊天 CSS 与 Mermaid 渲染配置同步同一字体顺序，不加载网络字体。代码和终端保留等宽字体，KaTeX 数学字形及 FontIcon/SymbolIcon 保留专用字体；不要给所有后代强制套用普通字体。字号、字重和行高由各自现有层级管理。
- `Layout/ShellLayoutMetrics.cs`：侧栏、输入框、聊天正文和右栏的默认尺寸及约束。持久化布局的默认值也来自这里；窗口临时变窄时只约束显示宽度，不覆盖用户保存的宽度。
- `Layout/WorkSidebarLayout.cs`：不依赖 WinUI 的最近/项目行高度分配和两组分隔拖动快照。页面只传测量值、转换 `GridLength`、接线和保存偏好；折叠、取消、复位及内部拖动不改变任务边界的规则集中在此，纯布局回归由 `ui-layout-smoke` 验证。

增加 UI 时优先使用现有组件与样式。新的数据保存、协议解析和复杂业务放在负责对应流程的服务中，页面负责展示与事件接线，避免写入通用控件。现有 `ShellPage.*` 仍共享页面状态，不因拆成 partial 就等同于完整 MVVM。

桌面通信：`Services/GatewayResponseReader.cs` 共用 JSON 响应读取及错误解码，保留调用方的 JSON 选项、空响应提示和目录 409 恢复提示。`GatewayApiException` 继承现有 `InvalidOperationException` 并携带 HTTP 状态及可用错误码。模型/会话客户端继续拥有请求、超时和响应释放，SSE 的成功响应不经过 JSON 读取；正式存储仍归网关。`dotnet run --project tests/gateway-response-smoke/GatewayResponseSmoke.csproj` 检查格式兼容、错误降级、取消与响应所有权。

流式回复：`Views/ShellPage.StreamReplies.cs` 按会话 ID 维护生成任务，40ms 合并刷新当前消息；`ModelApiClient.StreamReplyAsync` 与 `ChatStreamReader` 读取网关 SSE，区分正文、公开思考和终止事件。主聊天呈现正文阶段与紧凑工具活动，不为各轮创建独立思考折叠区；发送按钮在生成期间变成停止。消息保存 `Reasoning`、`ReasoningDurationMs`、`Status`、`Error` 和原模型信息，旧记录兼容；启动时把未完成生成恢复为中断。生成开始、定期快照和完成时由网关保存到统一会话日志，页面不再每 1.5 秒重写整个聊天目录；重试复用请求 ID 与用户消息 ID，避免重复上下文。

会话展示：`Controls/ConversationTranscript.cs` 用一个 WebView2 承载整段会话，页面资源在 `Resources/Transcript`，所有用户消息、可见正文阶段、工具活动和表格处于同一份 DOM。输入框、侧栏和模型菜单继续使用 WinUI。主聊天的公式直接由 KaTeX 排版，不经过截图、Skia 或原生文字上方的图片图层；浏览器负责文字、公式与表格的尺寸和换行。

`ConversationTranscript` 按可见正文、阶段内容和生成状态缓存 HTML，40ms 合并刷新；Markdig 解析和代码着色在后台任务中执行。后台仅使用跨越 `await` 前捕获的不可变消息和缓存引用，不读取 UI 拥有的可变字典；关闭控件使解析代次失效并结束尚未完成的 Ready 等待。切换聊天时清除前一会话，异步结果通过会话代际检查后才能发送给页面，避免旧任务覆盖新会话。聊天记录仍保存原始 Markdown，历史消息走相同的展示路径。

`Services/TranscriptMarkdown.cs` 复用 Markdig 和 `MathMarkdown.Normalize`，输出顶层 HTML 块，支持标题、强调、列表、任务清单、引用、代码、链接与真实表格。单元格保留行内节点，公式不再经过纯文本投影。`Resources/Transcript/transcript.css` 统一管理视觉样式：正文 15px、行高 1.7，公式 1.08em，围栏代码 13px。`content-blocks.css` 让普通内容框保留源码行边界，长行在当前框内横向滚动，原始换行、连续空行和缩进供显示与复制；普通正文和表格仍随回复区调整宽度，长单词可断行，无法换行的长公式仍可横向滚动。Mermaid 源码和未完成公式保留各自原有显示规则。会话视口填满聊天列，正文不再受固定最大宽度限制，右侧栏缩小或收起时同步扩展；滚动条位于聊天列右缘。

`Resources/Transcript/transcript.js` 按顶层块的源 HTML 比较并保留未变化前缀，追加内容只替换变化尾部；已排版公式缓存 DOM 字符串，不重复生成图片。有活动选区时保留现有 DOM 并暂存最新快照，清除选区后再应用最终内容；切换会话会立即清除旧选区并显示目标会话。页面独立维护跟随底部状态：打开聊天到底部，从底部发送后跟随增长，主动向上滚动或选择文字时暂停。主控件保留最多 1,024 条消息、8M 字符的 Markdown/高亮 HTML 缓存，页面保留最近 4 个聊天的 DOM（合计最多 40,000 节点、2M 字符，单个最多 400 条消息）；再次打开复用公式和消息节点，缓存命中不等流式批处理定时器。后台解析在消息边界检查会话代次，快速切换时丢弃过期结果，避免旧聊天覆盖当前聊天。

跨消息选择使用浏览器原生 Range，用户消息按纯文本保留空白。复制时按选区的文档顺序序列化正文、表格和代码，过滤按钮、状态信息及 KaTeX 的辅助 MathML；完整选中的公式复制为带分隔符的原始 TeX，公式内部的部分选择只复制所选可视字符。旧消息的整条复制取原始 `Content`；分段回复按顺序连接各段原始 Markdown，不包含可见思考和工具回执。输入框仍使用原生控件，其复制不经过会话页面。

工具回复支持 `toolStreamProtocol:3` 的 `AssistantSegments`，每轮保存稳定 `id`、`round`、全局 `order`、`phase`、`status`、正文及接口提供的公开思考。桌面接受 `assistant_segment` 开始/完成快照和带 `segmentId` 的增量；每轮完成后根据是否继续调用工具确认 `commentary` 或 `final_answer`，不能根据文字内容猜测最终回答。工具回执带相同轮次和自己的排序位置。`ChatStreamReader` 核对阶段身份、顺序、终态及工具归属，未知未来协议拒绝；旧 v2 事件和只有 `Content`、`Reasoning`、`ToolActivities` 的历史继续兼容，按保存的正文与真实状态应用当前展示投影。

主聊天采用运行中与终态分别投影。`TranscriptPresentation` 与浏览器 `message-presentation.js` 保留全部已有正文阶段，最终回答成功完成并渲染后才移除过程；失败、取消或没有有效最终答案时也保留已显示正文。阶段正文沿用最终正文相同的字体、字号、颜色与 Markdown 样式。可见阶段总计最多八个普通工具，待批准动作及所属阶段额外保留。连续同类工具合并为紧凑行，每组最多显示四个真实网页链接，余数仅作静态计数，完整来源保留于正式记录。动作、状态、命令及网站链接保持 12px 灰色文字，不再为每轮添加思考折叠标题和灰色整行卡。模型只在有实际发现的关键节点给简短阶段回复，不额外生成内部思维或伪造进度。

模型和上下文准备完成后，首个 `assistant_segment` 开始快照触发回复顶部的灰色实时用时；不在发送、排队或传输 `started` 时提前计时。旧流没有消息段时以首个正文或思考增量兼容启动。运行中使用单调时钟，每秒只更新用时文字，不重绘正文；后续工具调用、模型轮次、语言切换和切换聊天不重置起点。本机单调时间戳仅属于当前流，不写入正式日志。

整条消息成功完成且存在有效最终正文后，只展示最终 Markdown 和顶部的灰色总用时；最终正文与公式先完成渲染，再移除主聊天 DOM 中的工具、思考和阶段播报。`DurationMs` 是网关保存的本次准备完成后整次模型与工具执行耗时，HTTP/SSE 使用 `durationMs`；不是思考时间，不包含排队或上下文、工具目录准备，也不在重开时重新计算。完成、失败或取消均停止实时刷新。旧无段落边界消息沿用保存的 `Content`，有段却没有最终答案的异常消息不按成功展示。失败、取消和截断保留已有正文及简短状态。整条复制和重开使用同一投影，过程记录继续完整保存在正式日志中，后续模型配对历史不变，独立轨迹界面留待后续实现。

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
