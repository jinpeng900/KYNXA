# 前端 UI 维护入口

- `Controls/ComposerSurface.xaml`：输入框外壳与可选底栏。`Body` 放编辑器和发送工具，`Footer` 放项目选择等操作；`EditorHeight` 只表示编辑器高度，`SurfaceHeight` 包含底栏。两部分共用组件宽度，底栏通过贯穿两行的背景与输入框相连。
- `Controls/PickerMenu.cs`：模型、项目和权限菜单共用的创建入口。`CreateList` 统一列表滚动和选中指示；`WithFixedFooter` 将滚动列表与固定底部操作分开。选择、保存和打开窗口仍由对应的 `ShellPage.*.cs` 处理。
- `Styles/Dimensions.xaml`：字号、行高、按钮尺寸、菜单行高、底栏高度和圆角。`Styles/Controls.xaml` 通过基础按钮样式派生图标、选择器、侧栏操作和发送按钮。
- `Layout/ShellLayoutMetrics.cs`：侧栏、输入框、聊天正文和右栏的默认尺寸及约束。持久化布局的默认值也来自这里；窗口临时变窄时只约束显示宽度，不覆盖用户保存的宽度。

增加 UI 时优先使用现有组件与样式。新的数据保存和业务行为放在页面对应功能文件或服务中，避免写入通用控件。

## 首页、会话状态与工作详情

`Views/ShellPage.Presentation.cs` 集中处理首页引导、模型状态、能力说明、工作详情、复制和 Markdown 导出。首页提供聊天和工作入口，示例按钮只填入提示词。首次使用不会写入示例项目或聊天，已有记录沿用原存储。

模型状态区区分加载、连接失败、未配置、未选择和已选择。状态灯只表示模型网关可用，实际调用能力需要在模型管理中测试。未就绪时发送会保留草稿；刷新失败保留上次的模型列表和选择，并提供重试。模型能力入口显示“文本对话”；附件、知识库、连接、定时任务和技能均显示规划说明。文件页的手动读取用于本地预览，不授予模型操作文件或工具的权限。

工作详情分为“任务 / 文件 / 结果”。任务页显示实际消息数量和请求状态；文件页显示关联路径、手动刷新一级目录和预览小型文本；结果页展示最近保存的回复，支持复制和会话导出。这里尚未接入任务执行器、目录递归浏览或产物预览。导出使用 Windows 保存选择器，由用户选择 `.md` 保存位置。

`Views/ShellPage.WorkFiles.cs` 在用户点击后调用 `Services/WorkspaceFileReader.cs`，不在切换工作时自动读盘。一级目录最多显示 200 项，文本最多 256 KiB，仅支持限定扩展名、UTF-8 和带 BOM 的 UTF-16。过滤隐藏、系统和重解析项；目录祖先链、文件父目录及 Windows 打开后的真实路径再次验证。内容只在应用中只读显示，不执行或自动发送给模型。切换工作或关闭页面会取消旧读取，旧结果不会回填。

`Services/ConversationSearch.cs` 过滤当前本地历史的标题、项目名称和消息原文，排除归档记录并保留既有顺序。`Views/ShellPage.History.cs` 管理按模式分别保存的搜索词、清除和空状态；新建对话或项目后清除当前查询，使新内容可见。Ctrl+F 聚焦搜索，Ctrl+L 聚焦输入，Ctrl+N 新建当前模式的空对话；项目选择器仍可携带原输入草稿。弹出对话框和中文输入法选词期间，全局快捷键让位。

`Views/ShellPage.ConversationScroll.cs` 保存各会话的阅读位置。回复更新保留已有消息 VM 和文本控件，只更新变化的尾部；阅读旧内容时显示“回到最新”，主动发送或点击按钮后才滚到消息末尾。已有请求按会话管理，等待某个会话回复时不会禁用其他会话的输入。原生滚动、选区和文件对话框需结合实际桌面窗口验收。

`Views/ShellPage.MockReplies.cs` 的回复来自模型 API。停止按钮取消本地等待，模型网关将取消传递到上游 HTTP 请求；模型服务端能否立即停止生成取决于服务实现，重新请求可能产生额外调用。回复以会话 ID 归属原会话，停止后的迟到结果不会写入聊天，重试也不会再次插入用户消息。网关会检查排队、响应正文读取和历史写入前的取消状态，已取消的请求不会按正常完成保存。

窄窗口下侧栏临时收起，点击顶部入口以覆盖层展开；恢复宽窗口后沿用用户保存的侧栏宽度和收起偏好。工作详情在主区域至少 900 × 440 DIP 时并排显示，否则以右侧覆盖层打开。工作侧栏的项目与任务区域使用有限视口的外层滚动容器，矮窗口仍保留完整列表行并可滚动访问；内部项目与任务列表各自限制高度。短窗口减少首页装饰和间距，输入区高度留出状态和底栏空间；主窗口设置约 480 × 360 DIP 的最小尺寸，并按 XAML 缩放换算。模型状态区尺寸变化时重新计算消息区底部留白。相关边界检查见 `tests/ui-layout-smoke`；真实 WinUI 缩放与视觉效果还需在桌面窗口中检查。

回复展示：`Controls/MarkdownReply.cs` 使用 Markdig 解析 Markdown，在同一个 WinUI `RichTextBlock` 中渲染标题、粗体、斜体、删除线、列表、任务清单、引用、代码和链接，正文 14px / 23px 行高，代码 13px 等宽字体。表格暂以等宽文字排版，图片显示为链接；不执行 HTML，也不在渲染时请求外部图片。聊天记录仍保存原始 Markdown，旧消息打开时同样渲染。

`Controls/MarkdownReply.Backgrounds.cs` 在同一文本表面背后绘制不参与鼠标命中的背景，只有代码块、行内代码、引用和表格有浅灰底，普通正文、标题和列表保持白底。按实际文字排版定位背景，换行和窗口宽度变化时重新定位；不要给整条助手回复套灰色 Border。

`Controls/TextSelectionAutoScroll.cs` 按聊天 ScrollViewer 共享一个选择会话，用户消息与模型回复都注册其中；用户消息通过 `MarkdownReply.IsPlainText` 保留原样，仍使用 16px 字号。聊天 ListView 使用 StackPanel 保留屏幕外的文本控件，才能跨消息保持选区，超长会话需关注首次排版成本。选区按视觉顺序连续覆盖首尾的部分文字和中间完整消息，Ctrl+C、右键复制按同一顺序连接内容，输入框的复制保持独立。

拖选时启用 32ms 定时器，在聊天视口上下 40px 边缘区加速滚动。根元素接管鼠标捕获，并清理原生控件的按下状态；松手事件直接终止拖选，按钮状态轮询处理窗口外松手，取消/失焦/卸载也会停止。松手后的选区保存起止位置，拦截原生迟到的悬停更新；各文本控件使用 TextHighlighter 保持连续高亮，避免只有焦点所在消息显示选中。文字指针和纯文本索引分别转换，复制内容与显示选区一致。拖选期间拦截自动 BringIntoView；DPI 换算、经过头像、反向拖选和 Markdown 代码高亮继续保留。

代码高亮：`Services/CodeSyntaxHighlighter.cs` 使用 [ColorCode.Core](https://github.com/CommunityToolkit/ColorCode-Universal) 解析代码围栏的语言，返回文字片段与浅色主题配色，由 `MarkdownReply` 在原有段落内创建彩色 `Run`。支持 Python、JS/TS、C#/C++、Java、JSON、SQL、HTML/CSS、XML/XAML、PowerShell 等及常见缩写。关键字紫色、字符串深蓝、注释灰绿、数字蓝色；不改动缩进和代码文字。未标语言、未知语言以及超过 40,000 字符或 4,096 个文字片段的代码保留等宽纯文本。运行 `dotnet run --project tests/code-highlighting-smoke/CodeHighlightingSmoke.csproj` 检查语言匹配、字符串/注释上下文、换行/缩进保真和降级行为。UI 测试还覆盖带颜色的长代码拖选。

独立 UI 检查：`dotnet run --project tests/markdown-ui-smoke/MarkdownUiSmoke.csproj`，结果写入 `%TEMP%/kynxa-markdown-smoke.txt`，不读写用户聊天。测试使用带头像的 ListView 聊天布局；窗口打开后运行 `powershell -NoProfile -ExecutionPolicy Bypass -File tests/markdown-ui-smoke/check-selection.ps1`，验证持续上下拖选时滚动、选区单调增长、起点不漂移、经过实际头像、不松手反转方向和松手停止（会短暂操作鼠标）。解析器参考：https://github.com/xoofx/markdig ，原生文字控件：https://learn.microsoft.com/windows/windows-app-sdk/api/winrt/microsoft.ui.xaml.controls.richtextblock 。

跨消息检查：测试程序使用 `--conversation` 参数启动，随后运行 `tests/markdown-ui-smoke/check-conversation-selection.ps1`。包含用户/模型交替消息的双向拖选、松手后悬停不变、屏幕外消息选择及复制顺序检查。可加 `-CheckClipboard` 实测 Ctrl+C 和右键复制，脚本会暂存并恢复原剪贴板。

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

`MathMarkdown` 将 `\(...\)`、`\[...\]` 转为 Markdown 数学分隔符，也支持 `$...$` 与独占行的 `$$` 公式块。单独方括号包围且包含数学运算符的段落可兼容恢复；普通括号、代码块和行内代码不猜测转换。

`MathFormulaRenderer` 使用 [CSharpMath](https://github.com/verybadcat/CSharpMath) 与 SkiaSharp 在本机排版，不加载远程脚本。支持常见分数、根号、上下标、集合符号、积分、求和和矩阵；不支持或超限的公式回退源码。`MarkdownReply.Math` 的图层不参与鼠标命中，原生文档仍保留可选中的 LaTeX，跨消息复制包含公式源码。公式块使用现有浅灰背景，普通正文不变灰。每条消息限制渲染数量，单公式限制长度、嵌套和图像尺寸。

打开工作内聊天时展开该工作，并将未置顶工作移到置顶项之后；用户显式收起后，普通刷新不会再强制展开。拖动工作行可在相同置顶分组内调整顺序，写回项目列表；不会改变聊天所属工作或创建嵌套工作。之后再次打开工作内聊天会重新按最近使用规则提升该工作。

验证：`dotnet run --project tests/math-project-smoke/MathProjectSmoke.csproj`；原生公式预览使用 `dotnet run --project tests/markdown-ui-smoke/MarkdownUiSmoke.csproj -- --math`。公式变更同时回归已有跨消息选择脚本。
