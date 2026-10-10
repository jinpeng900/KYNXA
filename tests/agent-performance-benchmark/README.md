# KYNXA Agent 系统评测入口

这里测固定模型如何通过 KYNXA 的上下文、记忆、检索、工具循环和正式存储完成任务。`tests/retrieval-benchmark` 中的 SciFact Recall/nDCG 只回答检索组件的问题；它不能替代最终事实正确性、引用支持、跨语言回答、续问或工具任务的成功率。原有 40 道英文 SciFact 也不应重新标成中文或续问题。

## Windows 六任务入口：2026-10-10 适配

`run-windows-six.mjs` 沿用原六题、固定 fixture/gold、30 分钟任务预算及独立数据目录。适配版本为 `windows-six-action-readiness-v2`，评分版本为 `windows-six-received-evidence-v2`；历史结果不能不注明口径就混入新版得分。

- 开始任务只检查当前动作所需的工作区文件、目标词法来源或确认记忆，不等待整个索引作业完成、也不要求已有向量。目标词法准备给出两秒机会，仍未就绪时使用真实文件工具并记录覆盖状态，不能把退路写成索引完成。
- 已有兼容缓存继续复用；允许真实新增文档嵌入。默认每任务最多准入 1,024 个文档块，可用 `--document-embedding-limit` 在 1～100,000 内显式配置。记录实际请求、完成与预算拒绝，不重新全量准备仓库，也不再为维持缓存模式永久禁止新推理。
- 自动证据须同时对应正式归档、当前引用版本和实际模型请求中的完整原文，才可以满足已读条件。仅检索标题、未发送的归档、源文件已变化或者模型自报已读不计分；资料缺失题仍需要同版本完整来源覆盖。代码测试与人工语义审查不因取消重复读取计数而放宽。

这次只修改入口与评分适配。用户要求不再运行测试后，没有执行六题、模型或新增嵌入；此前的 1/6 结果属于旧入口，不能作为 v2 的实测得分。

## 第二版：实际 Pi SDK 同模型对照

`run-comparative.mjs` 使用生产 `ModelRuntime.replyStream` 对照真实 `@earendil-works/pi-agent-core 1.0.3` 原生循环。两边共享 KYNXA 的资料、自动检索、初始系统提示、工具目录和真实 `ToolService`；这只比较共同准备条件下的执行循环/传输/历史，不是完整 Pi CLI 或产品对照。适配器不读取个人 Pi 配置、凭据、插件或技能。SDK 对应 MIT 上游固定提交 `d78dc83d633229d12f8b79631384c4c2717c399f`；测试依赖不会进入应用包。

`system-suite.mjs` 预先冻结 12 类合成任务、JSON 合同和支持片段：中英问候、双向跨语言、最新修订、无证据、跨文档角色/批准条件、长章节、实体续问、真实复制回环、带原版本哈希的编辑、读取失败后恢复。`agent-system-v2` 与旧五题是不同协议，不重评分或替代旧原始结果。

两边使用同一个 DeepSeek 连接，固定 32K 上下文、8K 单次输出、每任务共 12 模型请求/24 工具/98,304 生成 token/120 秒。这是实验预算，不修改用户正式模型配置。默认每题各跑两次，第二次反转 KYNXA/Pi 顺序，共 48 次运行。SDK 依赖预检/导入在任何真实调用前完成；资料导入、嵌入和新实例准备记为 `setupMs`，任务准备/检索/实际生成/工具耗时记为 `durationMs`。供应商默认采样和缓存未完全受控；12 类任务不是 48 个独立抽样问题。

可选依赖在独立 ignored 目录安装。`pi-sdk-lock` 只含无凭据的固定 npm manifest/lock，能用同一依赖锁重新安装；不向应用 `package.json` 添加依赖。Node 需满足上游版本要求，当前本机为 24.14.0：

```powershell
$piBenchmarkRoot = Join-Path (Get-Location) 'artifacts/verification/agent-performance-benchmark/pi-sdk-v1.0.3'
New-Item -ItemType Directory -Path $piBenchmarkRoot -Force | Out-Null
Copy-Item -LiteralPath tests/agent-performance-benchmark/pi-sdk-lock/package.json -Destination $piBenchmarkRoot
Copy-Item -LiteralPath tests/agent-performance-benchmark/pi-sdk-lock/package-lock.json -Destination $piBenchmarkRoot
npm ci --prefix $piBenchmarkRoot --ignore-scripts
node tests/agent-performance-benchmark/run-comparative.mjs
node tests/agent-performance-benchmark/run-comparative.mjs --connection-file <existing-connection-file> --provider deepseek --model deepseek-flash --repeats 2
```

不带连接只显示计划，真实评测仍需显式连接文件；用户已经授权的真实 API 测试可执行。评测目录独占新建，推理前保存 `manifest.json`（任务/评分源码、SDK lock、配置 hash 和计划），逐条追加 `runs.jsonl`，最后新建 `results.json`。上游 key/私有 URL 不写入产物。临时权限只覆盖合成 Work，不能读评分器、隐藏 gold 或真实 Data，也不会启动现有网关、终端、浏览器、桌面或远端 MCP。

原始成功率是所有已声明合同同时满足的严格通过率；`validFinalJson`、字段精确匹配、来源数组、已观察支持正文、实际磁盘/读回/哈希和失败恢复依赖分别报告。文件名存在不等于看过支持片段；首轮不能借后续才读到的资料。`supportedFacts` 仅证明标准支持片段实际在请求或成功工具返回中可见，不等于答案自动正确。格式失败与人工事实复核分别说明，复核不得覆盖原成绩。

每次真实 HTTP 尝试都计数，两个 SSE 请求都加 `include_usage` 用于计量，提示与采样不改。保存输入/输出/cache 用量、完整响应耗时、任务首文本时间、实际请求中的短引用长度以及工具调用/返回缺对数。任务首文本从准备资料完成时起算，包含前序无文字模型轮次和工具耗时；不是某个末尾请求的 TTFT。`finishedToolActivities` 只是已结束正式活动数，配对验收使用实际请求 `orphanToolResults`/`missingToolResults`。无 usage 不填零；失败调用同样计成本，不报告未经核实的金额。

```powershell
node --test --test-concurrency=1 tests/agent-performance-benchmark/system-suite.test.mjs tests/agent-performance-benchmark/comparative.test.mjs tests/agent-performance-benchmark/pi-reference.test.mjs
```

回归采用 localhost 脚本上游，驱动真实 SDK 与生产工具/存储；没有真实模型调用，也不算 Agent 分数。可选 SDK 缺失时集成项明确跳过，缺失检测仍运行。实际同模型结果、独立核验、失败分类与公开同尺度参照见 [完整基准报告](../../docs/architecture/system-benchmark-20261006.md)。本组不覆盖 200 条消息恢复、长任务、真实仓库修复、UI、浏览器或桌面；公开检索基准单独报告。

## 可运行的第一版

`run-agent.mjs` 直接复用生产 `ModelRuntime.reply`、`ToolService`、`ConversationStore`、`MemoryService` 与 `RetrievalCoordinator`。固定 fixture、标准答案、usage 与成本验收集中在纯评测模块 `evaluation.mjs`，入口负责临时环境、执行与 CLI。它不启动已有网关，每个任务/配置建立新的临时 Data、扩展目录和挂载工作目录，执行后关闭运行时并清理。所有资料均为合成 fixture，版本为 `agent-synthetic-v1`；没有调用正式聊天、记忆、资料库或桌面。模型资产只读复用仓库/安装包的固定 E5 清单。

| 任务 | 固定标准与验收 |
| --- | --- |
| 打招呼 | 简短中文回应；无工具、无检索证据、无明确能力列表或模型自我介绍。正常“有什么可以帮你”不因“可以”一词被罚。 |
| 英文资料中文问答 | ORION 实验窗口：2031-11-14 17:40 UTC；审查人 Mara Chen；明确窗口并非已确认发射。需引用实际检索或回读过的 `fixture-notes.md`。 |
| 续问实体 | 第一轮比较 ORION 17:40 与 VEGA 09:15；第二轮必须把 Mara Chen 归给 ORION，不能把 Beatrice Hall 归给 ORION。明确的 VEGA=Beatrice Hall 对照允许。两轮共用同一会话与总预算，资料未变、短历史完整保留，允许引用前轮已检索/回读的同一资料。 |
| 文件依赖回环 | 从 `seed.txt` 实际读取 ticket，新建 `receipt.txt`，再实际读取核实；read/write/read 必须在递增轮次成功完成。磁盘与读回内容必须恰好为 `ticket=KYNXA-731926\n`。 |
| 证据不足 | NOVA 无正式日期/时间；提示预先要求只说明无法确定，不列任何具体日期、时刻或其他项目的安排。 |

这些短任务用于系统烟测，只有 5 个任务，不能代表长程研究、真实软件修复、桌面可靠性或普遍跨语言能力。固定答案检查不使用 LLM 裁判；引用检查仅验证指定资料确实进入当前/前轮证据或工具观察且所问事实正确，不是开放域逐句蕴含评分。NOVA 的数字规则检查的是预先声明的输出约束，不能作为通用幻觉检测器；若输出正确提及其他项目日期也会违反本题合同。招呼规则只识别明确列表/多个工具主题与模型自我介绍，正常简短招呼不因泛用助词被罚。仍需结合保留回答审核误判；评分与 fixture hash 在真实运行前固定，不应按结果事后调整。

## 运行方式与隔离

无参数仅列计划并退出，不调用模型，不悄悄使用 fake：

```powershell
node tests/agent-performance-benchmark/run-agent.mjs
```

真实调用必须显式指定连接文件。支持生产格式 `{ "version": 1, "providers": [...] }` 或单个 connection 对象；多连接/多模型必须明确选择。连接通过生产校验器在内存冻结，不执行 `ModelStore.save`。不要复制凭据到仓库、Data 或产物：

```powershell
node tests/agent-performance-benchmark/run-agent.mjs --connection-file <existing-connection-file> --provider deepseek --model deepseek-flash --rag paired
```

固定同一模型、协议、32,768 上下文、每调用最多 8,192 输出 token、每任务最多 12 次模型调用、24 次工具调用和 120 秒。续问共享任务预算；失败后不自动重试。生产协议保留其默认采样参数，脚本不注入 temperature/seed；这意味着单次运行仍有随机性。正式比较应保持供应商与模型版本可用范围一致，并在预算允许时重复成对任务。

`--rag paired` 对每任务先 off 后 on，共 10 个独立 runs：

- off：本地检索关闭，语义索引关闭；同一英文文件仍可由文件工具读取。
- on：生产默认 hybrid 检索，固定内置 `multilingual-e5-small` q8 版本；rerank 为 null，web 关闭。资料导入与背景 embedding 完成后才开始任务计时，准备时延另列 `setupMs`。若资产不可用、建库失败或缺失向量，明确记录 `setup-error`，不能静默称为 hybrid 验收。

这只是 RAG 开关消融，不能称为“旧完整软件”对照。`--rag off|on` 可单独跑一配置；`--semantic off` 明确用于词法诊断/轻量回归，产物会记录该配置，不能当作跨语言语义检索验收。默认 `--repeats 1` 保留 10 个 runs；可显式 `--repeats 2` 运行 20 个 runs，每任务第一轮 off→on、第二轮 on→off，结果包含每 run 的 `repetition`、每轮和总计成绩/成本。固定模型、任务与 fixture 内容不变；两次重复不足以断言统计显著，也不承诺供应商缓存冷热一致。

```powershell
node tests/agent-performance-benchmark/run-agent.mjs --connection-file <existing-connection-file> --provider deepseek --model deepseek-flash --rag paired --repeats 2
```

工具固定为工作目录内的 read/write/list/search/stat、knowledge search/read、当前会话历史和工具发现/结果读回。测试适配器只收紧工具目录和临时路径权限，实际执行/缓存/历史投影仍走生产实现；不启用终端、桌面、浏览器、MCP、网页或其他网络目的地。

默认只将脱敏 `results.json` 保存至忽略的 `artifacts/verification/agent-performance-benchmark/run-*`。`--output DIRECTORY` 也必须位于该 artifact 根目录下。结果仅输出模型名、协议与配置 hash，不输出连接原件、API key、私有 Base URL 或连接文件位置；供应商响应中若出现相同凭据/URL 字符串，也在交给正式存储前脱敏。临时 Data 不保留。程序只读显式连接文件和模型资产，不操作正式 Data。

输出目录在任何模型调用前独占新建；目录已存在则明确返回 `BENCHMARK_OUTPUT_EXISTS`，不调用上游，也不覆盖原结果。最终 `results.json` 使用只允许新建的写入模式；后续运行应使用新的目录。

若 `runtime.close()` 异常，不能推断原生资源已排空：任务记录 `close-error`、保留 `retainedFixturePath`，整批标为 `globalBlocked` 并停止后续运行；已收集成绩/成本仍输出，未运行项另列，不伪造失败轨迹。CLI 保存结果后返回非零状态。保留目录只含合成资料与脱敏正式日志，排查关闭问题后再清理。清理失败也保留路径与错误，不抹掉本轮结果。

## 指标与成本

每个任务记录 `success` 和各验收项、最终回答、引用资料名、工具名/状态/轮次及检索策略。任务耗时从准备完 fixture 后开始，包含生产上下文准备、检索、供应商响应和工具循环，清理耗时不计入；`setupMs` 单列资料导入、模型加载与背景建库。记录模型调用数、工具调用数、复用数、错误数、工具轮次以及每调用 HTTP 状态、完整响应耗时和 usage。

OpenAI/DeepSeek 的 prompt tokens 已含缓存命中，不重复加；Anthropic 的 cache read/write 字段作为额外输入计入。保留提供的 cache hit/miss、输入、输出与供应商总 token。没有 usage 时明确 `unknown`，整组总 token 为 null，已有用量只列 `known*Tokens`，不估算或把缺失填零。总成本、成功成本和失败成本都累计相应的全部调用、输入/输出 token、耗时与工具数；失败 HTTP 请求也保留。没有固定费率/账单证据，`monetaryCost` 为 null，不虚构美元费用。

配置与 fixture 均有 hash，记录 E5 版本、预算和工具集合。新运行另记 `verifierVersion` 和 `sourceFingerprint`：只读取 `source-fingerprint.mjs` 明确列出的生产与评测源码，逐文件 SHA-256 及组合 hash，不扫描正式 Data、连接或资产；以当次 manifest 文件清单为准。首轮没有源码指纹就如实保留缺失，不能用后来源码补造原运行身份；已有运行的原指纹也不因重评分而改写。固定 gold 不放入模型请求；真正的模型请求只包含任务提示、工作资料、生产系统提示及实际观察。最终结果同时保留 off/on 分组；不要只挑成功任务报告快多少。

## 同一次运行的版本化重评分

首轮 `run-1791217851841-31c2955e/results.json` 保留原始 off/on 各 4/5（80%）。两个续问答案都明确 ORION=Mara Chen，并正确补充 VEGA=Beatrice Hall；原验收器全局禁止出现 Beatrice Hall，造成误判。`agent-verifier-reviewer-relations-v2` 改为明确局部归属并允许正确对照，首轮的 `rescored.json`（各 5/5）完整保留。

随后独立回归发现 v2 对“分别不是”与同一 gold 的肯否矛盾处理不足。v3 拒绝这些情况，并支持明确实体标题/字段下的连续 reviewer 条目、按行或按列的简单表格和实体段切换；“不是已确认发射”不会否定另一个肯定的审查人谓词。QA 也在原有 `goldFacts` 上收紧正确人名归属，不放宽原日期、ORION 命名、语言或引用条件；缺少 ORION 主体的原 off 失败仍失败。

两次运行各有独立 `rescored.agent-verifier-reviewer-relations-v3.json`：首轮仍为 off/on 各 5/5，第二次 `run-1791219385240-b94ff105` 保持 off 9/10、on 10/10。保留原文件 SHA-256、原成绩、verifier 版本/源码 hash，明确 `sameModelRun: true`、`newModelCalls: 0`；只复核人物关系，QA 原 goldFacts 只能收紧，按结果重算成功/失败与各 repetition 分组，所有调用、token、耗时、原源码指纹全保留。三份既有原结果/v2复核字节不变，不把重评分说成另一场模型实验。复核工具只允许按 verifier 版本新建输出，不覆盖任何既有版本：

```powershell
node tests/agent-performance-benchmark/rescore.mjs --results <original-results-json>
```

文件任务的中文提示对应英文回复另记 `languageDiagnostic`，`affectsSuccess: false`；语言一致性未列入首轮文件任务的显式验收合同，不能事后加项改分。人物关系解析也仅覆盖本组固定实体/人名，不代替通用语义核查。首轮额外时间差推导存在错误，但原题只要求比较两个窗口时间，未验算该附加推导；5/5 只代表固定 gold/状态项通过，不能表述为所有附加陈述都正确。

关系解析是限域 verifier：仅继承明确实体标题/字段和连续有标号/角色的条目，空行结束继承范围；不声称可判定任意指代、复杂否定、复杂表格或开放域蕴含。专用纯函数回归覆盖共享否定、肯否矛盾、正确对照、窗口否定、标题/bullet/table 切换、QA 错归与 repetition 汇总，不调用模型：

```powershell
node --test tests/agent-performance-benchmark/evaluation.test.mjs
```

## 功能回归不等于推理分数

```powershell
node --test tests/agent-performance-benchmark/harness.test.mjs
```

测试以本机 HTTP 脚本上游返回固定答案，驱动生产真实工具、文件与检索，验证连接选择、无参数行为、验收器、缓存用量计数、失败 HTTP、unknown usage、关闭异常保留资料及正式日志不含凭据。它明确标为 `functional-regression`，通过率没有 Agent/模型推理能力含义。脚本上游不会连接真实模型。若通过 CLI 使用该标签，必须显式 `--functional-regression` 且端点为 loopback。真实模型任务才标为 `agent-task-evaluation`，脚本与本文不预置任何真实成绩。

## 近期原始论文与可采用的评测方法

下表日期为论文首次公开日期，链接均为论文/作者仓库；可借鉴的方案并不表示已在 KYNXA 执行其完整基准。

| 基准/方法与来源 | 测什么 | 固定模型如何测 KYNXA；本机成本 |
| --- | --- | --- |
| τ²-bench，2025-06-09：[论文](https://arxiv.org/abs/2506.07982)、[作者仓库](https://github.com/sierra-research/tau2-bench) | Agent 和用户共同改变共享环境；沟通、工具、最终状态。 | 借鉴状态 verifier 和独立沟通/执行失败分类；合成文件任务可本机低成本执行。完整基准要额外固定用户模拟器与工具策略，不能混入当前 5 题成绩。 |
| BrowseComp-Plus，2025-08-08：[论文](https://arxiv.org/abs/2508.06600)、[作者仓库](https://github.com/texttron/BrowseComp-Plus) | 固定语料、人工核实支持文档下的多步研究/搜索。 | 下一阶段挑固定公开小集，固定 reader/model、资料快照和查询预算，测最终答案/引用/搜索次数；完整大语料与长轨迹成本较高。 |
| DeepResearch Bench，2025-06-13：[论文](https://arxiv.org/abs/2506.11763)、[作者仓库](https://github.com/Ayanami0730/deep_research_bench) | 100 个专家研究任务；报告质量、有效引用与引用支持。 | 借鉴区分“有引用”与“引用支持事实”；固定来源与人工 gold 可免付费 judge 做小集，完整 RACE/FACT 需要固定裁判版本和额外成本。 |
| Agent Retrieval Bench，2026-07-27：[论文](https://arxiv.org/abs/2607.24882)、[作者仓库](https://github.com/eyuansu62/agent-retrieval-bench) | 25 仓库 427 样本，工作流所需下一份代码上下文、无本地 gold 时拒绝；BCY 按 token 预算衡量上下文覆盖。 | 少量冻结仓库快照可以验证工具读回与上下文预算；文件命中只属于组件/上下文获取指标，不能直接当修复成功。全量约 7.9M chunks，先做小集。 |
| LongMemEval-V2，2026-05-12：[论文](https://arxiv.org/abs/2605.12493)、[作者仓库](https://github.com/xiaowu0162/LongMemEval-V2) | 从 Agent 执行轨迹检索并使用长期经验。 | 固定同一 reader、会话轨迹和召回预算，分开测保留实体、时间、更新与无答案；本版两轮任务只覆盖续问烟测。完整长历史不适合首轮低成本验收。 |
| SWE-bench Pro，2025-09-21：[论文](https://arxiv.org/abs/2509.16941)、[作者仓库](https://github.com/scaleapi/SWE-bench_Pro-os) | 长程真实软件问题的最终测试通过。 | 以后冻结 repo/初态/测试环境与固定模型，比较系统配置；完整 Docker 环境在本机 Windows 通常还需 WSL/镜像资源，应作为单独工程，不声称当前 harness 已兼容。 |
| BrowserGym，2024-12-06：[论文](https://arxiv.org/abs/2412.05467)、[作者仓库](https://github.com/ServiceNow/BrowserGym) | 统一浏览器交互环境与任务。 | 可先借鉴 DOM/页面最终状态 verifier 做本地小网页任务；本版禁用浏览器，原框架在本机 Windows 的运行成本/兼容性尚未实测。 |
| Windows Agent Arena，2024-09-12：[论文](https://arxiv.org/abs/2409.08264)、[作者仓库](https://github.com/microsoft/WindowsAgentArena) | Windows 桌面跨应用动作与最终状态。 | 更贴近以后桌面验收，但 VM/Docker/并行部署成本高；先隔离专用测试工作区，不对正式桌面执行。当前网关 harness 不代表 UI 验收。 |
| SWE-Effi，2025-09-11：[论文](https://arxiv.org/abs/2509.09853)、[作者仓库](https://github.com/Centre-for-Software-Excellence/SWE-Effi) | 正确性与 token/时间预算，尤其昂贵失败。 | 本版直接采用成功+资源+失败成本分开报告的方法，不搬模型排行榜，也不将成功任务的延迟单独当系统效率。 |

效率实验固定同一模型并分辨各项成本。[The Complexity Trap（2025-08-29）论文](https://arxiv.org/abs/2508.21433)和[作者实现](https://github.com/JetBrains-Research/the-complexity-trap)提供旧观察遮蔽对照；其质量结论依赖 Agent 与任务，不能先承诺零损失。第二阶段生产已加入经归档验证的重复/旧版本观察收缩，仅改变模型投影、保留完整日志及工具配对；不是按年龄遮蔽全部旧输出，也不为所有问题增加 LLM 改写、rerank 或摘要调用。

## 针对自动检索和上下文开销的落地取舍

首轮同模型、单份短资料的 off/on 均通过固定任务，模型调用均 15 次；on 工具从 15 减至 12，但输入 token 从 39,423 增至 62,134，总 token 从 42,280 增至 65,075。off 可以直接读同名文件，因此本组很容易封顶；这些观察只能提出“证据注入与重复读回是否值得”的系统问题，不能证明 RAG 总是有益/有害或模型能力差异。

| 论文/作者实现 | 可落地结论与代价 | 复用范围 |
| --- | --- | --- |
| [Adaptive-RAG，NAACL 2024](https://arxiv.org/abs/2403.14403)、[官方实现](https://github.com/starsuzi/Adaptive-RAG) | 在无检索、一次检索和迭代检索间按需求路由。KYNXA 可先让招呼、精确文件操作和已含完整答案的短续问避开自动知识检索；资料问答继续检索，复杂研究才加预算。不能因“中文词面弱”而跳过跨语言证据。 | 设计借鉴；原方法需训练 T5-large 分类器及检索服务，不能直接当零成本桌面组件。先验证既有规则路由，避免为每请求再调用 LLM 分类。 |
| [RAG or Long-Context / Self-Route，EMNLP 2024](https://arxiv.org/abs/2407.16833) | 完整短资料或有效历史已在上下文时，可尝试复用已读证据，缺证据才补检索；按任务同时测准确性、token 和延迟。 | 设计借鉴；论文用模型自我反思选择 RAG/长上下文，不是免费的规则判定，不能照搬为所有请求的额外调用。 |
| [Sufficient Context，ICLR 2025](https://arxiv.org/abs/2411.06037)、[作者仓库](https://github.com/hljoren/sufficientcontext) | 区分“上下文足以回答”和“模型答对”。已有标题/高分/关键词不证明事实支持；固定 fixture 可用来源状态、完整读回和 gold 核实是否还需证据。 | 设计与诊断借鉴；原 sufficiency autorater 用 LLM，调用有成本。现有词面 assessment 只能作支持诊断，不能当跨语言事实裁判或可靠拒答阈值。 |
| [The Complexity Trap，2025](https://arxiv.org/abs/2508.21433)、[作者实现](https://github.com/JetBrains-Research/the-complexity-trap) | 老观察可保留动作/结果引用，仅把大段内容从模型输入遮蔽；最近的依赖读写结果、版本 hash 和回读入口必须保留。无额外摘要模型调用。 | 方法可实现为生产历史投影的局部实验；不改原始日志。质量取决于任务与 scaffold，不承诺零损失，也不适合遮蔽尚未完成的读写依赖。 |
| [Revisiting Text Ranking in Deep Research，SIGIR 2026](https://arxiv.org/abs/2602.21456)、[作者实现](https://github.com/ChuanMeng/text-ranking-in-deep-research) | 检索粒度、候选深度、重排与 Agent 查询写法会影响最终答案、上下文成本与搜索次数。优先预算内片段、条件重排和严格标识查询；在固定模型任务上逐项消融。 | 设计与评测方法借鉴；论文较大的重排/查询转换模型不能直接视为低成本桌面收益。LLM Q2Q 会多一次调用，只在查询错配明确且实际任务收益覆盖成本时试用。 |

以上是可验证的系统假设，不是论文收益可迁移保证。评测入口与重复实验不会修改生产路由或历史算法，不增加任务或官方 benchmark 成绩；重复实验保持 fixture、gold 和模型合同不变。
