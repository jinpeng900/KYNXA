# 正式 Shell UI 迁移隔离验收

## 原生对话框主题、圆角及连接占位

```powershell
pwsh -NoProfile -File tests/shell-migration-ui-smoke/run-checks.ps1 -SingleProcessor -Scenario dialogs
```

`App.DialogChecks.cs` 点击生产定时任务与连接入口，以实际 F1 打开键盘操作帮助，并核对浅色、深色及自定义强调色在同一个已打开原生 ContentDialog 上即时生效。真实 SDK 模板中的外层 `BackgroundElement` 与内层 `DialogSpace` 均需为 16 DIP 圆角；表面、边框及文字引用应用共享画刷。逐状态读取真实按钮呈现层，核对默认操作和普通取消／拒绝操作的 Normal、PointerOver、Pressed 背景、前景、至少 4.5:1 文字对比度和 8 DIP 按钮圆角。状态检查调用原生 VisualStateManager，不声称对话框按钮使用了真实鼠标按下。

单操作对话框还比较实际按钮与 `BackgroundElement` 的水平中心，误差不超过 1 DIP，覆盖中英文定时／帮助／连接及项目错误提示。短按钮为 130×36 DIP、左右内边距各 24 DIP；中文“知道了”原文保持不变，文字本地字距为 180，并核对实际排版宽度增加，避免只读取继承属性而误计生效。同一生产定时弹窗临时使用较长标签，验证宽度在 202 DIP 上限内自然增长，随后恢复原文和尺寸；英语及其他标签恢复原生内容模板和字距。多操作的重命名、审批、检索窗口保留分列与间隔。实际文字／按钮边界、模板和间距写入 `shell-dialog-action-layout.json`，英文及长标签另保存截图。

场景还核对实际 Escape 关闭、关闭后的主题订阅生命周期、中文及英文连接入口只显示“功能暂未开发”、重命名的空名称禁用及取消结果、工具审批无默认批准及明确拒绝。生产检索窗口中的添加链接弹窗保留自己的 XamlRoot，空链接验证仍取消提交，切换主题和取消导入仍保持原操作生命周期。草稿、会话 ID、消息对象及模拟网关零写入必须保持；不访问正式数据或调用模型。

截图为 `shell-dialog-schedule-light.png`、`shell-dialog-schedule-dark.png`、`shell-dialog-keyboard-custom-light.png`、`shell-dialog-keyboard-custom-dark.png`、`shell-dialog-schedule-long-action-zh.png`、`shell-dialog-connections-en.png` 和 `shell-dialog-rename-dark.png`。集中入口成功标记为 `PASS: ... production Shell native dialog checks.`；默认完整入口也包含这组。文档列出验收条件，不替代本轮原生运行结果。

## 功能窗口首次呈现与原生关闭背景

```powershell
pwsh -NoProfile -File tests/shell-migration-ui-smoke/run-checks.ps1 -SingleProcessor -Scenario window-presentation
```

`App.WindowPresentationChecks.cs` 在浅色与深色模式中点击实际模型管理、设置和知识库入口，再向本场景窗口发送原生关闭消息。记录首次可见事件与稳定尺寸，检查设置窗口没有显示后再扩大的尺寸变化。显示采样核验精确 HWND 和屏幕点归属，只读取自有窗口；关闭阶段的后台探针仅访问原生句柄，不在后台读取 WinUI 对象。隐藏／销毁后停止读取附属窗口，改在其原位置读取所属窗口。实际时间戳、采样间隔和窗口事件写入 `window-presentation-frames.json`，不把目标采样间隔称作覆盖每个显示刷新帧。

同一文件的 `EraseContracts` 单列真实 `WM_DESTROY` 阶段的离屏背景擦除验证：使用自有 8×8 内存 DC／bitmap 向原生 subclass 链下方调用 `WM_ERASEBKGND`，比较实际绘制颜色与关闭前的当前配色。它验证清除 XAML 内容后的原生背景合同，不冒充屏幕上捕获的自然白帧。回调异常隔离，GDI 对象及测试钩子结束时释放；保留模拟网关零写入、原草稿和会话。场景覆盖这三个实际入口，其他共享窗口仍需按其功能验收，不据此声称所有页面或系统动画都已目视检查。

## 设置窗口布局、下拉滚轮与正文查找

```powershell
pwsh -NoProfile -File tests/shell-migration-ui-smoke/run-checks.ps1 -SingleProcessor -Scenario settings-interactions
pwsh -NoProfile -File tests/shell-migration-ui-smoke/run-checks.ps1 -SingleProcessor -Scenario shortcut-tooltip
pwsh -NoProfile -File tests/shell-migration-ui-smoke/run-checks.ps1 -SingleProcessor -Scenario body-search
```

`App.SettingsInteractionChecks.cs` 在真实设置窗口检查 660／1000 DIP、最大化及 420／360 DIP 窄窗，内容随可用宽度铺满且两侧各留 24 DIP；受控对照上一版居中限宽布局，确认宽窗口确实利用新增空间，并检查配色和下方设置可达。以原生普通 ComboBox 和 `ScrollSafeComboBox` 对比真实滚轮行为。收起时不改选项且所属页面实际滚动；方向键／Home 及展开列表的滚轮保留原生操作。正式设置三个下拉框还核对本机偏好文件没有因滚轮而改变。测试使用自有窗口、临时数据和模拟网关，实际通过／失败以本轮结果为准。

快捷键专项使用实际键盘输入检查 `Ctrl+F` 正文查找、`Ctrl+Shift+K` 会话标题／项目搜索、空会话搜索、关闭后的焦点恢复，以及原生模态／输入法保护；WebView 焦点下不出现浏览器自带查找框或重复搜索请求。 `body-search` 专项集中检查点击查找框或主输入区后的 Enter／方向键、F3／Shift+F3、Ctrl+F 与 Escape，并验证原生和网页处理不会重复导航。

本夹具直接链接生产 MainWindow、ShellPage 的完整 C#、XAML 和资源，不复制搜索、模式选择、草稿、抽屉或导出业务实现。唯一替代的是应用启动器与外部 HTTP 网关；生产 StoragePaths、模型客户端、正式聊天客户端及 Transcript WebView 保持原实现。

采用每轮唯一的独立 MSIX 调试身份 `KYNXA.ShellMigrationUiSmoke.<短ID>`，以支持生产代码所用的 Windows.Storage.ApplicationData。入口通过已还原的 Windows App SDK winapp 工具注册本轮身份，直接启动程序继承隔离环境，退出后精确注销；不使用正式 KYNXA 身份。构建与运行请串行执行，避免与正式桌面或其他 WinUI 夹具竞争编译内存和窗口。

```powershell
dotnet build tests/shell-migration-ui-smoke/ShellMigrationUiSmoke.csproj -p:Platform=x64
dotnet run --project tests/shell-migration-ui-smoke/ShellMigrationUiSmoke.csproj -p:Platform=x64 --no-build
```

脚本将本轮 `%TEMP%/kynxa-shell-migration-latest.txt`、`result.txt`、原生截图、构建与身份工具日志收集到 `artifacts/shell-migration-ui-smoke/vs-framework-csc-x64/runs/<本轮名称>/`。先检查结果含 `PASS: ... production Shell UI migration checks.`，不能只凭运行进程创建判定通过。

`App.StartupChecks.cs` 在正式页面加载前挂起模拟网关的首次目录响应，核对目录尚未返回时的工作提示、灰色禁用发送按钮、禁用箭头和44 DIP工作页脚。检查跨实际渲染帧的位置稳定，再在加载期间切换聊天并输入虚构草稿，释放响应后核对模式、草稿、发送可用状态及原生矩形坐标；恢复工作模式后的位置也应一致。挂起只作用于首次目录GET，模型列表和健康检查独立，失败和退出都会释放或取消门闩。截图为 `shell-startup-before-catalog.png`、`shell-startup-chat-after-catalog.png`，坐标保存在 `shell-startup-geometry.json`；不等待真实网关，也不读取日常数据。

覆盖范围：正式页面初始化、已配置/失败/恢复模型状态，真实模型菜单按连接 ID 和模型 ID 搜索、无匹配提示、固定底部配置入口、失败后的缓存列表及重试，同名模型按连接和模型 ID 对选中，项目操作前后发送按钮恢复；会话标题搜索、聊天和工作模式间草稿恢复、稳定聊天及消息 ID、800 DIP 临时抽屉、放宽后的已保存侧栏宽度、生产工具审批弹窗对新快捷键和导航的阻止、整段会话的可见 Markdown 投影及快照。快捷键模态守卫和项目操作门控调用真实处理函数；项目操作只挂起本地测试委托，文件选择器交互不在本夹具范围。

截图包含 `shell-migration-model-menu.png`、`shell-migration-drawer-search-800.png` 和 `shell-migration-wide.png`。捕获前核对搜索行的真实文本，等待弹出动画、XAML Rendered 事件和桌面合成刷新；失败时另保存 `shell-migration-failure.png`。模型选择仅保存临时 Desktop 下的 UI 偏好，模拟网关仍禁止 POST 等写入。

页面细节回归覆盖：真实发送按钮模板的禁用背景与语义色原生箭头、非空输入的配套强调色前景、内存生成状态下的配套停止图标、附件不可用状态、104 DIP 默认输入高度、620 DIP 英文窄窗；模型菜单按内容确定高度、筛选后稳定高度、重复 ID 行隐藏与当前模型勾选。

`App.PanelPolishChecks.cs` 使用两个虚构挂载项目和真实生产 Open/Close、关闭标签与 Reopen 菜单，检查空右栏默认释放、模式/项目/会话作用域、草稿及 432 DIP 用户宽度保留。窄窗注入正式形状的截图回执，再恢复宽窗，通过原 `AgentApiClient` 读取单条模拟归档并显示；显式关闭及关闭最后标签不会被刷新或缩放撤销。图片为 WinRT 编码的 48×24 合成 PNG，引用中的长度与 SHA256 由归档 JSON 的 UTF-8 字节计算，未知会话和资源请求仍使夹具失败。

新增截图：`shell-polish-empty-wide.png`、`shell-polish-stop-wide.png`、`shell-polish-empty-narrow-en.png`、`shell-polish-screenshot-wide.png` 和 `shell-polish-wide-after-preview.png`。空闲页面截图在夹具内以临时透明度变化请求两帧，恢复原值并等待 Rendered、DwmFlush 后捕获；该措施不进入生产页面。生成状态检查只插入夹具拥有的内存 pending 对象，结束后释放，不启动真实流式请求。

在创建生产页面之前写入临时 `KYNXA_DATA_HOME`（另兼设 `KYNXA_DATA_ROOT`）、模型与扩展路径、布局/模型选择种子，回环网关只提供固定虚构 GET 响应，非 GET 请求直接使测试失败。`WEBVIEW2_USER_DATA_FOLDER` 覆盖生产 Transcript 的用户数据目录，运行后核验 WebView 实际目录。未打包网关脚本，模拟网关失效时不能启动其他真实服务。所有窗口、数据、浏览器缓存和截图均属于夹具。

WebView 环境变量覆盖参数行为依据 [Microsoft WebView2 官方参考](https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/webview2-idl)。

`App.ComprehensiveChecks.cs` 继续检查 480 DIP 英文输入工具栏：完整权限说明、极长模型名与发送按钮不重叠，窄窗只隐藏权限文字而保留可访问名称和完整提示；临时侧栏的 Escape 收起入口恢复触发按钮焦点。设置窗口按实际 DPI 和工作区定位，400×360 DIP 时存储路径移到第二行且可选择复制，放宽后恢复行内显示。所有检查保留当前草稿与会话，模拟网关写入计数为零。

`App.RetrievalNavigationChecks.cs` 点击真实知识库按钮，复用生产检索设置窗口和客户端，从模拟网关读取 settings、status、providers、global sources 四个 GET。检查重复打开复用同一窗口且不重复读取，关闭释放持有引用，并保留草稿、消息对象、正文和目录版本；不执行真实检索、模型调用或写入。缩窗与恢复宽窗先等待真实 Shell 宽度达到目标区间，再断言布局；弹窗及尺寸检查需在测试窗口未被手动操作时串行运行。

## Ctrl+F 悬浮提示集中回归

```powershell
pwsh -NoProfile -File tests/shell-migration-ui-smoke/run-checks.ps1 -SingleProcessor -Scenario shortcut-tooltip
pwsh -NoProfile -File tests/shell-migration-ui-smoke/run-checks.ps1 -SingleProcessor -Scenario body-search
```

脚本根据 `-Scenario` 将对应专项参数直接交给已注册本轮调试身份的应用。`App.ShortcutTooltipChecks.cs` 承载真实生产页面，将指针移到页面原生区域及搜索按钮，观察实际打开的 Popup 与可见文本；搜索按钮的中文及英文完整提示必须仍能真实打开。工具提示的逻辑所有者不一定在 Popup.Child 的可视后代中，因此页面附加 ToolTip 仅作诊断，不以其空值证明没有自动提示。向夹具自己的前台窗口发送 Ctrl+F、Escape、Ctrl+L 和鼠标滚轮，再核对搜索、输入焦点及长正文的实际滚动距离；仅视图中的虚构长文在 finally 恢复。审批 ContentDialog 打开时真实快捷键必须保留模态焦点、会话和草稿。检查始终使用虚构网关，无模型请求和正式写入。此组也追加到原完整入口，不替换原有检查。

`body-search` 仅执行初始化、空会话查找与正文键盘专项：真实 Enter／Shift+Enter 导航、鼠标点击原生输入框后 F3／Shift+F3 导航、Ctrl+F 返回搜索框，以及原生输入框中的 Escape。通过匹配索引核对每次按键只移动一处，保留草稿与会话，受控原生输入法组合状态仍阻止快捷键。后台启动的自有窗口若被桌面置顶窗口遮挡，夹具仅在激活期间临时提高自己的置顶层级，核验标题栏命中后点击，并在 finally 恢复原设置；不操作其他应用窗口。成功标记为 `PASS: ... production Shell body-search keyboard checks.`。

所有原生输入先校验前台窗口；发送快捷键前还检查修饰键状态，用户持有 Ctrl/Shift/Alt/Win 时停止快捷键输入并报告失败。鼠标目标必须真实位于屏幕内且命中夹具窗口，使用实际 `SendInput` 移动触发 XAML 悬停。运行期间请勿操作测试窗口、持有修饰键或切换前台。观察保存在 `shell-shortcut-tooltip-observation.json`，实际滚动距离保存在 `shell-shortcut-tooltip-scroll.json`，滚动观察在失败断言前也写出，指针与弹出层诊断分别为 `shell-shortcut-tooltip-pointer.json` 和 `shell-shortcut-tooltip-popups.json`；截图为 `shell-shortcut-tooltip-hover.png` 和 `shell-shortcut-tooltip-after-scroll.png`。旧版首次断言失败前也会保留页面附加 ToolTip 的类型、内容、启用状态和所观察到的裸提示弹出层。集中入口成功标记为 `PASS: ... production Shell shortcut tooltip checks.`。

## 主题与阅读设置集中回归

```powershell
pwsh -NoProfile -File tests/shell-migration-ui-smoke/run-checks.ps1 -SingleProcessor -Scenario appearance
pwsh -NoProfile -File tests/shell-migration-ui-smoke/run-checks.ps1 -SingleProcessor -Scenario appearance-transfer
```

新的脚本入口使用 VS Full MSBuild / Framework C# 编译器，独立 obj、bin、临时数据和 WebView2 目录；构建期可选单逻辑处理器兼容参数，运行前恢复进程调度范围。`-Scenario all` 包含完整主窗口回归；专项目录的原始 obj/bin 显式排除，不能因自定义中间目录将旧生成 XAML 再次作为源码编译。

在既有五套配色回归上，`App.AppearanceReadingChecks.cs` 验证浅色／深色／跟随系统、13～22 px 半像素步进、HEX 与真实 ColorPicker。选色只更新待应用 HEX，点击应用后才保存。重新加载临时布局文件、迁移守卫和真实原子保存失败分别验证持久化与回滚；打开的设置页、主窗口及 Transcript 同步，选区、草稿、正式消息和模拟网关写入保持不变。

`appearance-transfer` 只运行本批外观文件功能的定向检查：生产按钮和本地提示、版本 1 的六字段 JSON、16 KiB 大小限制、重复或额外键及无效值拒绝、真实 StorageFile 导入导出、尚未应用的 HEX 不导出、取消、真实布局原子保存失败回滚、语言切换、400 DIP 窄窗和窗口关闭后的晚到选择。文件选择由本夹具的委托提供临时文件，随后使用生产读取、校验、保存与应用链路；不声称自动化了 Windows 文件选择器内部交互，也不读取日常用户数据。成功标记为 `PASS: ... production Shell appearance import/export checks.`。

新增 `App.ThemeIconChecks.cs` 验证实际 Shell 图标的共享语义画刷、原始尺寸与固定路径、权限切换、26 个图标键及品牌位图保留。极端自定义强调色验证主要背景上的文字对比度；系统变化采用当前值读取及自有 HWND 通知路径，不修改系统主题。每项以当前结果文件为准。

配色及主题集中入口：

```powershell
pwsh -NoProfile -File tests/shell-migration-ui-smoke/run-checks.ps1 -SingleProcessor -Scenario appearance
```

`App.AppearanceChecks.cs` 打开生产设置窗口，通过真实原生按钮逐个选择经典灰、雾蓝、青绿、柔紫和暖砂，重新读取本次临时 `layout.json` 核对保存，并检查已存在的原生发送按钮、侧栏、模式选中背景画刷即时变化。正式 Transcript 的 CSS、实际用户消息气泡和 WebView 背景须同步；虚构长文切换前后的消息与正文 DOM 引用、精确选区、滚动位置、输入草稿、正式消息对象和模拟目录须保持一致。

夹具仅将长文临时附加到视图，结束时恢复生产消息来源。迁移守卫通过临时 `StoragePaths.IsMigrating` 检查；保存失败通过在夹具临时 Desktop 下建立空 `layout.json.tmp` 目录阻止真实原子保存，核对可见错误及配色回滚后立即删除。全部操作使用隔离数据和模拟 GET 网关，不消耗模型 API、不写正式会话。还检查 400 DIP 英文设置卡片布局及恢复默认配色，结果追加到完整入口。

实际文字检查在强制 GC 后双向切换语言，直接核对已实现的字号标题和五套配色名称，并记录 `shell-appearance-settings-400-language.json`；仅检查自动化名称或窗口几何不能证明英文已经显示。另释放一个真实原生绑定控件，确认翻译值源可被回收，防止全局本地化注册强持有窗口。

还核对旧版偏好缺少配色字段及未知预设值的默认回退，以及真实输入框获得和离开焦点时外框画刷的切换。证据文件为 `shell-appearance-observations.json`、`shell-appearance-settings-400-en.png`、`shell-appearance-settings-wide.png` 和 `shell-appearance-mist-blue-wide.png`。集中入口成功标记为 `PASS: ... production Shell appearance checks.`。这些是需要实际运行的验收条件，文档描述本身不代表本轮已经通过。

原生输入框的选中文字为白色，逐套读取真实 `Prompt.SelectionHighlightColor`，要求不透明、与该套强调色一致且对白字对比度至少 4.5:1。另实际聚焦主输入框并全选原草稿，生成 `shell-appearance-native-selection.png/json` 核对显示效果；WebView 继续验证浅色选区与原文字选择保留，不把两种控件的选区背景混为一谈。

## 侧栏展开与收起集中回归

```powershell
pwsh -NoProfile -File tests/shell-migration-ui-smoke/run-checks.ps1 -SingleProcessor -Scenario sidebar-controls
```

`App.SidebarControlChecks.cs` 使用真实生产页与原生 `NativeUi.Invoke` 按钮自动化，在中文、英文的默认 240、最小 200、最大 360 DIP 侧栏中测量实际模式按钮、文本、图标和收起按钮边界，核对没有截断或交叠。展开和收起按钮互斥出现，点击隐藏原按钮后焦点应进入当前可见的对应按钮；宽窗的宽度及折叠偏好通过真实临时 `layout.json` 读取验证。

五个侧栏导航入口还在浅色、深色、自定义浅色和自定义深色四组配色下接受实际 `SendInput` 鼠标悬停，核对当前主题强调色的浅色反馈背景。原生按下视觉状态及键盘焦点使用同一组真实控件，检查文字、圆角和焦点画刷，并在保持上次悬停时切换配色。截图为 `shell-sidebar-navigation-light.png`、`shell-sidebar-navigation-dark.png`、`shell-sidebar-navigation-custom-light.png` 和 `shell-sidebar-navigation-custom-dark.png`。

移入、移出与再次移入还即时读取按钮空白处的实际合成屏幕像素，以约 8 ms 的异步目标间隔观察至少 200 ms，直至至少八个样本及末尾三次稳定，最长 600 ms。每次只读取一个像素以减少 GDI 同步阻塞，JSON 保留实际采样时间；不声称采集了显示器每一个刷新帧。全部颜色必须处于实际空闲／悬停端点的 RGB 包络及亮度范围内，容差为 3，防止只检查最终画刷而漏掉亮色闪帧。采样前后及每次读取均验证前台和准确的自有窗口命中，证据为 `shell-sidebar-navigation-pixels.json`。

收起按钮使用现有 `IconSidebarToggle` 与共享主题画刷，原生按钮继续拥有提示、命中与键盘焦点。浅色／深色宽窗、紧凑窗及返回宽窗后，检查展开与设置图标中心仍距左侧24 DIP，正文左缘为48 DIP，两侧留白相等且没有闲置拖动列；再展开时恢复原8 DIP拖动区域。深色展开与收起截图供实际视觉复核。

展开状态只预留 4 DIP 拖动列，底部图标原坐标保留，比较最左设置与最右状态图标到可见侧栏边缘的实际空白均为 20 DIP。完整 8 DIP `SidebarGrip` 跨越较窄预留列，两侧各覆盖 2 DIP；实际 MOVE 进入两处覆盖范围并等待 `PointerEntered`，核对坐标、层级及未开始拖动。原拖动、保存、取消、复位的控件接线检查继续执行。

800 DIP 紧凑模式分别以新收起按钮、原遮罩按钮和生产 `TryDismissCompactSidebar` 的 Escape 处理路径关闭临时抽屉，核对焦点及保存偏好，再恢复宽窗。测试同时覆盖原本展开和原本折叠两种宽窗偏好。Escape 在本场景调用生产处理方法，不发送系统按键；真实按钮使用原生 Invoke，而非直接调用点击方法，因此不要求键盘输入获得前台焦点。

透明分隔线检查读取真实 `ResizeGrip` 的悬停状态画刷、八 DIP 命中区域及 `HitTarget`，并触发该控件实例已接线的拖动、完成、取消和重置事件，验证生产宽度逻辑和保存仍可用。还核对 `ShellGrid` 与侧栏实际背景画刷颜色相同且不透明，使透明拖动列连续显示侧栏颜色，主内容区保留独立背景；宽窗与最小宽度截图供实际边界复核。这是原生控件与接线回归，不冒充实际鼠标拖动验收。整个场景保留草稿、活动聊天 ID、原消息对象和正文，模拟网关写入始终为零，结束时恢复夹具原布局及语言。

证据文件：`shell-sidebar-control-geometry.json`、`shell-sidebar-controls-wide.png`、`shell-sidebar-controls-minimum-200-en.png`、`shell-sidebar-controls-compact.png`、`shell-sidebar-controls-closed.png`、`shell-sidebar-controls-wide-dark.png` 和 `shell-sidebar-controls-closed-dark.png`。集中入口成功标记为 `PASS: ... production Shell sidebar control checks.`；这组也纳入默认完整入口，文档不代替实际运行结果。

## 侧栏行圆角与滚动边界集中回归

```powershell
pwsh -NoProfile -File tests/shell-migration-ui-smoke/run-checks.ps1 -SingleProcessor -Scenario sidebar-roundness
```

`App.SidebarRoundnessChecks.cs` 仅向工作最近、任务、聊天历史和项目树的视图绑定集合放入夹具拥有的长标题合成条目，结束后按原对象恢复。它不把条目加入正式项目或聊天目录、不点击合成会话、不调用模型。为依次呈现模板，夹具临时切换工作/聊天侧栏内容的可见性，保留业务模式和当前聊天 ID。工作最近、任务和聊天历史各有 30 行，并验证真实 ScrollViewer 发生滚动；318 和最小 200 DIP 侧栏分别读取已实现的原生容器、ListViewItemPresenter、活动背景、文本及操作按钮边界。

检查要求 presenter、容器及 SDK 插入的实际背景 Border 均为 8 DIP 圆角；悬停、按下、选中及复合状态画刷相同且不透明；滚动条与整行背景之间保留外侧空间。读取原生背景本身的内缩与边界，工作活动背景不得使用负边距，必须与实际原生背景同框，不能拿包含留白的 presenter 外框代替。长标题允许省略，但完整标题保留，键盘聚焦操作按钮后标题与按钮不能交叠。项目树的父项、子项也比较悬停与活动背景的真实边界。整个过程保持草稿、消息对象、正式项目/聊天 JSON 和模拟网关零写入。

悬停使用已有前台窗口守卫与 `SendInput` 鼠标移动，目标必须真实位于显示器工作区并命中夹具窗口。先移开指针、移开键盘焦点并确认原生背景透明，再移入列表行，等待实际背景颜色和行内操作按钮出现；项目树还核对实际 `CommonStates.CurrentState`。移出后再次确认悬停消失，再切换视图条目的活动标记或原生 SelectedItem，避免把叠加中的悬停背景误当成活动背景。按下及复合状态核对实际原生画刷属性，不发送鼠标按下或点击合成会话。运行时请勿操作测试窗口或切换前台，结束后恢复夹具原位置与指针位置。

截图包括 `shell-sidebar-roundness-work-318.png`、`shell-sidebar-roundness-work-200.png`，以及各列表、各宽度的 `*-hover.png` / `*-active-selected.png`、项目树的 `shell-sidebar-roundness-tree-*.png`。实际背景、内缩、文字和按钮边界保存于 `shell-sidebar-roundness.json`，指针命中诊断沿用已有 `shell-shortcut-tooltip-pointer.json`。集中成功标记为 `PASS: ... production Shell sidebar roundness checks.`，默认完整入口也包含该组。文档列出的是验收条件，不代表未执行的测试已经通过。

## 中英文字体集中回归

```powershell
pwsh -NoProfile -File tests/shell-migration-ui-smoke/run-checks.ps1 -SingleProcessor -Scenario typography
```

`App.TypographyChecks.cs` 在生产 MainWindow/ShellPage 内检查动态主导航菜单、侧栏、输入框和设置弹窗的实际字体、已实现文字与边界，保存真实窗口截图和字体诊断。普通文字应使用 Segoe UI Variable Text / Segoe UI / Microsoft YaHei UI / Microsoft YaHei 的统一回退顺序，专用图标与代码字体保留。夹具使用临时数据和模拟网关，不请求模型；默认完整入口也包含本组。以本次 result.txt 的 PASS 或 FAIL 为准。
原生菜单使用已加载的 MenuFlyoutItem／模板 TextBlock 属性和实际边界验收。PrintWindow 只捕获所属主窗口，`shell-typography-window-*.png` 不用于声称菜单弹出层已被拍入。`shell-typography-settings.png` 与 `shell-typography-modal-markdown.png` 保存真实设置／模态窗口；后者提供共享字体栈、显式微软雅黑 UI 和宋体的同字号中文对照，以及实际代码和图标。WebView 真实字形使用另一个 transcript 专项检查中的 CDP 字体记录验证。
