# 验证与协作

验证目标是本次用户行为及受影响约定。下列命令从仓库根目录执行，按改动选择；不要求每次全量运行。

## 最低必要验证与推进节奏

- 优先让用户的主流程可用。先核对实际改动和调用关系，只选能回答本次问题的最低必要检查；已有检查能够覆盖时直接复用，不另建同义用例。
- 约七成工程把握即可推进，是基于源码、现有证据和改动影响的判断，不是测得的 70% 成功率，也不能写成量化验收成绩。已知的主流程阻断、越权、正式数据损坏或重复副作用仍应处理，不能据此宣称已完成。
- 复用仍适用于当前逻辑、数据和环境的有效结果，只补变化与真实使用缺口。报告区分本轮执行、已有结果复用和未验证部分，不将历史记录冒充本轮实测。
- 小改动不自动增加冒烟套件、诊断探针、开发快照、逐文件哈希对照、完整基准或全套构建测试。注释、格式和规范修改通常以差异及相关引用核对收尾；行为修改选择最接近主流程的现有检查。只有改动影响、已出现的故障或明确用户要求能解释新增成本时才扩测。
- 必要检查通过后停止，继续集成或交付。不追求完美覆盖，不为尚无证据的边界情况堆叠防御、反复调用模型或延长验收；新变更、失败或影响主流程的疑点才扩大对应范围。
- 精简的是开发过程中的额外验证，不删除运行时的权限、作用域、正式数据一致性、来源版本与未知副作用保护。任务暂停的业务断点继续保留；开发用副本或快照不是业务断点，不要求每次修改都制作一份。模型资产完整性和来源引用所需的哈希仍按正式合同执行。

## 选择检查

下面列出可复用的检查入口，不是全部必跑项。按上述规则选择最低必要范围，已经有效覆盖的部分直接复用。

| 改动 | 优先验证 |
|---|---|
| 文案、样式、按钮等局部 UI | 桌面编译及相关实际界面状态；无需编写镜像实现的测试 |
| 侧栏布局、展开、排序、草稿 | `ui-layout-smoke`、`work-sidebar-smoke`；涉及 TreeView 增量更新再选 `project-tree-smoke` |
| 界面语言与即时切换 | `ui-language-smoke`，并检查活动窗口、表单/草稿和聊天正文 |
| Markdown、公式、代码、选择/复制/滚动 | `transcript-markdown-smoke` 和受影响的 `transcript-ui-smoke` 检查；高亮逻辑再选 `code-highlighting-smoke` |
| 流式 API、状态、取消/重试 | `chat-stream-smoke` 与 Rust models/gateway、stream、orchestration 测试及本机 HTTP/SSE 夹具；改变主聊天表现再查 Transcript UI |
| 桌面 HTTP 响应、错误与资源所有权 | `gateway-response-smoke` 和受影响的模型/会话客户端 smoke，确认 JSON 默认值、409、取消及 SSE 仍兼容 |
| 原生网关启动、随包文件与桌面 SSE 合同 | `native-runtime-smoke` 和 `native-gateway-contract-smoke`；后者读取本轮原生 HTTP 测试实际生成的 SSE，不使用旧 Node 夹具 |
| 模型预设、上下文配置 | `model-presets-smoke` 或 `model-context-smoke`，与 Rust models 连接/发现/能力/预算测试 |
| 会话/记忆/归属/迁移/初始化 | 受影响 Rust storage/retrieval 测试及对应 C# 客户端 smoke，包含分页、失败、恢复与并发边界 |
| 资源准入、观测与扩展进程 | agent-core resources/tools 对应用例及独立 `resource-service` crate 测试；验证实际租约、前后台优先级和进程退出后释放 |
| C# ToolHost、原生 ABI 与 Windows 操作 | ToolHost 独立编译及受影响原生/沙箱夹具，保留真实输出、权限、取消与进程树清理 |
| ORT/向量空间/模型资产/设备 | 固定少量 tokenizer/向量/分数对照、实际 DLL/资产哈希与设备资格检查；CPU 通过不替代 DirectML 验收 |
| 共享接口、构建文件、多模块核心改动 | 原生编译、桌面/ToolHost 编译、相关 HTTP/SSE/客户端和架构检查；影响面广时扩大 Rust 套件 |
| 开发 Skill/代理规则 | 两份 Skill 的 frontmatter、`agents/openai.yaml`、相对链接与当前入口；必要时用只读独立任务验证选路，不启动产品或修改真实数据 |

## 常用命令

当前产品目标为 Windows x64。桌面构建要求项目当前声明的 .NET/Windows SDK 与 WinUI 环境；原生 Rust 依赖按 `Cargo.toml`/`Cargo.lock` 固定版本，随包资产由 `apps/desktop/Build/prepare-native-runtime.ps1` 校验准备，桌面构建目标调用它。核心不依赖 Node，不安装旧网关 npm 依赖或构建 NAPI；私有 Node 与固定 MCP 依赖仅作为扩展兼容载荷随包。用 `native-runtime-smoke/check-mcp-runtime.ps1` 与 `check-skill-runtime.ps1` 验证不依赖系统 PATH 的实际运行。首次构建允许正常 restore，只有确认资产已恢复时才用 `--no-restore`。

```powershell
dotnet build apps/desktop/KYNXA.Desktop.csproj -p:Platform=x64
dotnet build apps/tool-host/KYNXA.ToolHost.csproj
dotnet run --project tests/ui-layout-smoke/KYNXA.UiLayoutSmoke.csproj
dotnet run --project tests/work-sidebar-smoke/WorkSidebarSmoke.csproj
dotnet run --project tests/project-tree-smoke/ProjectTreeSmoke.csproj
dotnet run --project tests/ui-language-smoke/UiLanguageSmoke.csproj
dotnet run --project tests/transcript-markdown-smoke/TranscriptMarkdownSmoke.csproj
dotnet run --project tests/transcript-ui-smoke/TranscriptUiSmoke.csproj
dotnet run --project tests/code-highlighting-smoke/CodeHighlightingSmoke.csproj
dotnet run --project tests/chat-stream-smoke/ChatStreamSmoke.csproj
dotnet run --project tests/gateway-response-smoke/GatewayResponseSmoke.csproj
dotnet run --project tests/model-context-smoke/ModelContextSmoke.csproj
dotnet run --project tests/model-presets-smoke/KYNXA.ModelPresetsSmoke.csproj
dotnet run --project tests/conversation-store-smoke/ConversationStoreSmoke.csproj
dotnet run --project tests/storage-migration-smoke/StorageMigrationSmoke.csproj
node tools/development/check-architecture.mjs
cargo check --manifest-path apps/agent-core/Cargo.toml --locked --all-targets
cargo check --manifest-path apps/resource-service/Cargo.toml --locked --all-targets
cargo test --manifest-path apps/agent-core/Cargo.toml --locked
cargo test --manifest-path apps/resource-service/Cargo.toml --locked
```

上述是独立命令示例，不是每次依次执行的清单。开发机可用其已配置的 Node 执行架构守卫，也可使用本轮载荷中已校验的私有 Node 绝对路径；不能为此恢复旧网关依赖。Transcript 原生检查按当前 [夹具说明](../../../../tests/transcript-ui-smoke/README.md) 选择 `run-checks.ps1 -Scenario`，该入口要求 PowerShell 7，不能交给 Windows PowerShell 5.1；独立 Skill 文档更新只做格式、引用和只读选路验证。

原生随包验证先构建 Windows x64 桌面，再指定本轮真实输出目录，不把历史构建路径当作当前产物：

```powershell
powershell.exe -NoProfile -File tests/native-runtime-smoke/check-pri.ps1
powershell.exe -NoProfile -File tests/native-runtime-smoke/check-payload-cleanup.ps1
powershell.exe -NoProfile -File tests/native-runtime-smoke/run.ps1 -BundleRoot '<本轮桌面 Windows x64 输出目录>' -ResultPath artifacts/development/native-package-check.json
powershell.exe -NoProfile -File tests/native-runtime-smoke/check-mcp-runtime.ps1 -BundleRoot '<本轮桌面 Windows x64 输出目录>' -ResultPath artifacts/development/private-mcp-check.json
powershell.exe -NoProfile -File tests/native-runtime-smoke/check-skill-runtime.ps1 -BundleRoot '<本轮桌面 Windows x64 输出目录>' -ToolHostDirectory '<本轮 ToolHost 输出目录>'
```

桌面 SSE 合同先由独立临时存储、本机模拟供应商和真实文件读取生成实际消息流，再交给未修改的桌面解析器：

```powershell
$nativeSseFixture = Join-Path (Get-Location).Path 'artifacts/development/native-gateway-sse.txt'
$previousNativeSseFixture = $env:KYNXA_GATEWAY_SSE_FIXTURE
try {
    $env:KYNXA_GATEWAY_SSE_FIXTURE = $nativeSseFixture
    cargo test --manifest-path apps/agent-core/Cargo.toml --locked --test gateway_http tool_cycle_streams_real_read_receipt_into_provider_and_formal_log -- --exact
    if ($LASTEXITCODE -ne 0) { throw 'Native SSE fixture generation failed.' }
    dotnet run --project tests/native-gateway-contract-smoke/NativeGatewayContractSmoke.csproj -- $nativeSseFixture
} finally {
    $env:KYNXA_GATEWAY_SSE_FIXTURE = $previousNativeSseFixture
}
```

`native-runtime-smoke/run.ps1` 会启动并关闭自有临时网关、资源服务与 ToolHost，验证便携载荷；`native-gateway-contract-smoke` 只读取所传 SSE 文件，不能单独证明原生启动、完整 UI 或检索质量。`gateway-response-smoke` 是独立桌面响应读取器检查，仍适用于原生 HTTP。

单项原生检查使用 `cargo test --manifest-path apps/agent-core/Cargo.toml --locked 实际测试过滤名`，先用 `rg -n '#\[(tokio::)?test\]|fn 实际关键字' apps/agent-core/src` 选择真实测试，不猜名称。ignored 的固定 ORT 对照仅在本轮需要且资产准备好时显式运行，报告实际结果。多人协作先协调编译/资产准备，避免重复 Cargo 构建；不要并行启动竞争相同端口、数据位置、结果文件或窗口的 UI/集成测试。

Rust 控制改动优先复用受影响的现有用例；需要核对传输或实际执行时，再选择本机 HTTP/SSE/ToolHost 定向检查并准备正式原生随包文件，不要求每次同时运行全部入口。正式请求不能在服务失败后退回空恢复账本或 Node；权限和正式写入必须经过既有工具/数据服务。执行回执区分未派发、已提交、取消与未知结果，消息传输终态和任务核验状态分别断言；模拟供应商测试不消耗真实凭据。旧 Node 测试或隔离旧 SDK 对照只作为迁移前证据，不能直接计入当前原生测试成绩。

历史入口 `tests/gateway-startup-smoke`、`tests/node-runtime-smoke` 和 `node --test apps/model-gateway/tests/*.test.mjs` 属于迁移前 Node/NAPI 链路，不能作为当前验证命令；历史报告原文保留，并明确其运行版本与日期。

旧 Agent/SciFact/统一智能体评测适配器仍依赖迁移前网关，尚未适配当前 Rust。参考保留的 [Agent 历史说明](../../../../tests/agent-performance-benchmark/README.md)、[检索历史说明](../../../../tests/retrieval-benchmark/README.md) 与 [统一智能体历史说明](../../../../tools/development/fixtures/unified-agent/README.md)。保留固定题集、评分和原始报告；不能直接运行旧入口或把原生 smoke 当作完整基准替代。归档旧入口时同时检查静态导入、动态启动、源码指纹和文档调用者，保留可核验的原路径/字节哈希，不移动独立 C# 客户端或 WebView 检查。

当前原生链路有一个固定检索评估入口：`cargo test --manifest-path apps/agent-core/Cargo.toml --test retrieval_evaluation`，数据集为 `tests/retrieval-benchmark/datasets/native-baseline.json`。词法阶段默认运行，不需要模型资产；语义阶段加 `-- --ignored` 并设置 `KYNXA_LOCAL_RETRIEVAL_BUNDLE_ROOT`，用真实本地 E5 生成分块与查询向量走生产混合融合，另报纯余弦诊断列。两阶段都在隔离临时数据根发布正式索引并调用生产融合排序，报告 nDCG@10、Recall@10、MRR@10 与单独计列的语义诊断项；不读用户数据、不下载资产、不调用模型供应商。重排尚未覆盖。改动检索召回、分词、切块、向量空间或融合权重后与同一数据集版本和 `pairingId` 的成绩比较，不把它当作 BEIR 榜单或 Agent 任务完成率。评分定义与数据集身份继续复用 `tests/retrieval-benchmark/metrics.mjs`。

桌面默认启动随包原生网关。必要的手动集成测试可调用 `tools/development/start-native-gateway.ps1`，明确指定准备好的 BundleRoot、独立 DataHome/ModelHome/ExtensionHome 和端口。该启动命令会实际启动进程，只在任务需要时执行；不能为文档检查启动、停止或替换用户日常网关。

正式 WinUI 桌面需要有效包身份，按工程/WinApp 或已注册包入口启动；直接运行裸 EXE 的 COM 身份错误不代表网关失效。分支预览使用独立包身份、数据、模型/扩展目录及端口；不能覆盖主包注册或与日常网关共写 Data。纯 WebView 夹具按自身工程规定启动，不套用正式桌面的包身份要求。

主 Transcript UI 使用独立测试窗口和会话，结果在 `%TEMP%` 的测试文件中。旧 `markdown-ui-smoke`、`scroll-follow-smoke`、原生公式测试只验证兼容路径；它们通过不能代替当前 WebView2 聊天的复制和排版检查。

## 数据与可观察验收

- 测试用独立临时 Data、虚构连接与模拟上游，不能连接日常使用的数据根目录或消耗用户真实 API。不要打印原始凭据和私有聊天。
- 测试若会初始化 `StoragePaths` 或存储服务，在初始化前显式传入临时数据位置；桌面使用 `KYNXA_DATA_HOME`。不要依赖测试窗口自然隔离存储，也不要通过修改用户 `~/.kynxa/storage.json` 达到隔离。
- 优先验证外部行为：草稿未提交不保存、打开不改变排序、移动后范围正确、取消不污染下一聊天、冲突不丢数据；避免只断言函数内部每一步。
- 状态型 UI 只检查本次受影响的状态和主交互；初始、选中、悬停/焦点、展开/收起及缩放按实际变化选择，不为局部改动遍历全部状态。不要仅凭编译通过声称操作已实测正确。
- 切换、关闭、异常与晚到结果属于相关生命周期验证。缓存/性能改动需观察其命中、失效与实际耗时，不能只以“用了缓存”作为改善证据。
- 检查与停止条件统一遵循[最低必要验证与推进节奏](#最低必要验证与推进节奏)。
- 记录实际执行命令、通过/失败及未验证原因。文档里的旧结果不是本次证据，环境问题也不能被描述成代码验证通过。

## 协作与交付

- 独立模块、只读审查和独立验证可委派，约定目标、相关约束、共享接口和写入文件归属，避免两个代理同时编辑同一文件。
- 不固定模型、推理强度或代理数量。默认继承当前会话，仅在用户明确要求时覆盖；此前“多开 Astra/ultra”不再是本项目开发要求。
- 主代理复核实际差异和接口兼容，运行必要集成检查。代理完成消息和单个模块测试不能自动证明整个功能成立。
- 检查用户已有改动后再编辑；必要时使用隔离工作树，但不强制每次创建。不用 `reset`、批量覆盖或清理来解决不理解的工作区状态。
- 相关行为/合同变更时更新现有维护文档；简单样式修改不用扩展架构说明，也不要求五人任务报告。
- 完成后说明具体行为、相关文件和验证结果。提交、推送、发布遵循已有授权，不重复请求已获许可，也不把代码修改授权扩大为发布授权。
