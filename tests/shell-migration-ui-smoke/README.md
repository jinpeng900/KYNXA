# 正式 Shell UI 迁移隔离验收

本夹具直接链接生产 MainWindow、ShellPage 的完整 C#、XAML 和资源，不复制搜索、模式选择、草稿、抽屉或导出业务实现。唯一替代的是应用启动器与外部 HTTP 网关；生产 StoragePaths、模型客户端、正式聊天客户端及 Transcript WebView 保持原实现。

采用独立 MSIX 身份 `KYNXA.ShellMigrationUiSmoke`，以支持生产代码所用的 Windows.Storage.ApplicationData。构建与运行请串行执行，避免与正式桌面或其他 WinUI 夹具竞争编译内存和窗口。

```powershell
dotnet build tests/shell-migration-ui-smoke/ShellMigrationUiSmoke.csproj -p:Platform=x64
dotnet run --project tests/shell-migration-ui-smoke/ShellMigrationUiSmoke.csproj -p:Platform=x64 --no-build
```

运行结果通过 `%TEMP%/kynxa-shell-migration-latest.txt` 指向独立结果目录，包含 `result.txt` 和原生截图。先检查结果含 `PASS: ... production Shell UI migration checks.`，不能只凭运行进程创建判定通过。

`App.StartupChecks.cs` 在正式页面加载前挂起模拟网关的首次目录响应，核对目录尚未返回时的工作提示、灰色禁用发送按钮、禁用箭头和44 DIP工作页脚。检查跨实际渲染帧的位置稳定，再在加载期间切换聊天并输入虚构草稿，释放响应后核对模式、草稿、发送可用状态及原生矩形坐标；恢复工作模式后的位置也应一致。挂起只作用于首次目录GET，模型列表和健康检查独立，失败和退出都会释放或取消门闩。截图为 `shell-startup-before-catalog.png`、`shell-startup-chat-after-catalog.png`，坐标保存在 `shell-startup-geometry.json`；不等待真实网关，也不读取日常数据。

覆盖范围：正式页面初始化、已配置/失败/恢复模型状态，真实模型菜单按连接 ID 和模型 ID 搜索、无匹配提示、固定底部配置入口、失败后的缓存列表及重试，同名模型按连接和模型 ID 对选中，项目操作前后发送按钮恢复；会话标题搜索、聊天和工作模式间草稿恢复、稳定聊天及消息 ID、800 DIP 临时抽屉、放宽后的已保存侧栏宽度、生产工具审批弹窗对新快捷键和导航的阻止、整段会话的可见 Markdown 投影及快照。快捷键模态守卫和项目操作门控调用真实处理函数；项目操作只挂起本地测试委托，文件选择器交互不在本夹具范围。

截图包含 `shell-migration-model-menu.png`、`shell-migration-drawer-search-800.png` 和 `shell-migration-wide.png`。捕获前核对搜索行的真实文本，等待弹出动画、XAML Rendered 事件和桌面合成刷新；失败时另保存 `shell-migration-failure.png`。模型选择仅保存临时 Desktop 下的 UI 偏好，模拟网关仍禁止 POST 等写入。

页面细节回归覆盖：真实发送按钮模板的灰色禁用背景与深灰 SVG、非空输入的原白色箭头、内存生成状态下的白色停止图标、附件不可用状态、104 DIP 默认输入高度、620 DIP 英文窄窗；模型菜单按内容确定高度、筛选后稳定高度、重复 ID 行隐藏与当前模型勾选。

`App.PanelPolishChecks.cs` 使用两个虚构挂载项目和真实生产 Open/Close、关闭标签与 Reopen 菜单，检查空右栏默认释放、模式/项目/会话作用域、草稿及 432 DIP 用户宽度保留。窄窗注入正式形状的截图回执，再恢复宽窗，通过原 `AgentApiClient` 读取单条模拟归档并显示；显式关闭及关闭最后标签不会被刷新或缩放撤销。图片为 WinRT 编码的 48×24 合成 PNG，引用中的长度与 SHA256 由归档 JSON 的 UTF-8 字节计算，未知会话和资源请求仍使夹具失败。

新增截图：`shell-polish-empty-wide.png`、`shell-polish-stop-wide.png`、`shell-polish-empty-narrow-en.png`、`shell-polish-screenshot-wide.png` 和 `shell-polish-wide-after-preview.png`。空闲页面截图在夹具内以临时透明度变化请求两帧，恢复原值并等待 Rendered、DwmFlush 后捕获；该措施不进入生产页面。生成状态检查只插入夹具拥有的内存 pending 对象，结束后释放，不启动真实流式请求。

在创建生产页面之前写入临时 `KYNXA_DATA_HOME`（另兼设 `KYNXA_DATA_ROOT`）、模型与扩展路径、布局/模型选择种子，回环网关只提供固定虚构 GET 响应，非 GET 请求直接使测试失败。`WEBVIEW2_USER_DATA_FOLDER` 覆盖生产 Transcript 的用户数据目录，运行后核验 WebView 实际目录。未打包网关脚本，模拟网关失效时不能启动其他真实服务。所有窗口、数据、浏览器缓存和截图均属于夹具。

WebView 环境变量覆盖参数行为依据 [Microsoft WebView2 官方参考](https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/webview2-idl)。

`App.ComprehensiveChecks.cs` 继续检查 480 DIP 英文输入工具栏：完整权限说明、极长模型名与发送按钮不重叠，窄窗只隐藏权限文字而保留可访问名称和完整提示；临时侧栏的 Escape 收起入口恢复触发按钮焦点。设置窗口按实际 DPI 和工作区定位，400×360 DIP 时存储路径移到第二行且可选择复制，放宽后恢复行内显示。所有检查保留当前草稿与会话，模拟网关写入计数为零。

`App.RetrievalNavigationChecks.cs` 点击真实知识库按钮，复用生产检索设置窗口和客户端，从模拟网关读取 settings、status、providers、global sources 四个 GET。检查重复打开复用同一窗口且不重复读取，关闭释放持有引用，并保留草稿、消息对象、正文和目录版本；不执行真实检索、模型调用或写入。缩窗与恢复宽窗先等待真实 Shell 宽度达到目标区间，再断言布局；弹窗及尺寸检查需在测试窗口未被手动操作时串行运行。
