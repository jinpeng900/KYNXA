# 本地中英文嵌入

`EmbeddingService` 为请求提供真正的本地语义向量，不创建第二份正式聊天或记忆存储。Orchestration 持有生命周期并将此服务注入 Data 的检索流程；Data 不反向导入 Models。

## 默认模型和运行边界

- 模型：`Xenova/multilingual-e5-small`，固定 revision `761b726dd34fb83930e26aab4e9ac3899aa1fa78`，ONNX q8，384 维。基础 `intfloat/multilingual-e5-small` 模型卡声明 MIT，原始模型卡、转换说明及许可原文随包保留。
- `embedding-profile.mjs` 是权重、分词、版本、哈希及下载来源的唯一清单。模型资产约 136 MB，位于忽略的 `artifacts/runtime/embedding/builtin-multilingual` 构建缓存；构建目标仅按校验生成的资产白名单复制到安装目录的 `runtime/embedding/builtin-multilingual`，缓存中的其他文件不随包提供。
- 每模型一个按需独立 Node 进程，默认申请可核验的 GPU 后端；实际设备、内存与 CPU 线程由统一资源服务批准，不再固定两线程或四条一组。不需要 Python、Docker 或远端 API。原生 ONNX Runtime 和 SQLite 向量扩展由锁定的 npm 依赖随包提供。
- 应用运行阶段禁止远端模型、CDN/WASM 回退及所有 推理进程内 `fetch`。默认模型目录只读；没有权重、校验失败或 native 初始化失败时明确报告错误，由检索流程保留关键词能力，不伪造向量或自动上传文本。
- 原生 `sqlite-vec@0.1.9` npm 分发支持 Windows x64；Windows arm64 的完整向量链路需要另行编译与发布验收，不声称已支持。
- Windows x64 同时包含 ONNX 所需的四个 app-local VC 核心运行库。`Build/vc-runtime.json` 固定原始 Microsoft 签名文件版本与摘要；`prepare-vc-runtime.ps1` 只从正式版 Visual Studio Redist 或已经校验的构建缓存取得文件，不复制系统目录 DLL。原始 Microsoft Runtime 许可随包提供。构建机首次需要有许可的对应 Redist；安装用户不需要另下载或安装 VC。

## 接口

`model-registry.mjs` 登记原有 `builtin-multilingual` 嵌入、`builtin-multilingual-reranker` 重排，
以及已实现的可选 `builtin-multilingual-dml-q8` 嵌入和 `builtin-multilingual-reranker-dml-q8` 重排。
`resolveRetrievalModelProfile(kind, profileId)` 返回固定模型、版本、
维数、输入投影、语言和资产摘要；未知、跨类型或 `null` profile 返回
`RETRIEVAL_MODEL_PROFILE_UNSUPPORTED`，不能因为配置格式合法就偷偷换成默认模型。

`EmbeddingRouter` 按精确 profile 拥有独立按需服务。应用层 `embeddingDevicePolicy: auto|cpu|gpu`
决定建库目标；auto 仅在 Windows 单 NVIDIA 映射已核验、GPU 后端声明了对应空间且状态为
ready/loading 时选择 GPU。迁移期间旧 CPU 空间继续查询，新 GPU 空间覆盖当前来源后才切换；
GPU 故障优先复用可用的兼容 CPU 空间，没有兼容空间时保留词法，不给 GPU 向量改 CPU 标签。
显式 gpu 偏好不会以 CPU 冒充成功。语义关闭和资产缺失不会自动反复排队迁移。

服务构造和每次推理均可显式传入 `profileId`，`status(profileId)` 报告所请求配置的真实支持情况。
`ready` 且 `assetVerification: pending` 表示文件存在并等待首次推理进程完整校验，不能当成已验证权重；
校验通过且加载成功才报告 `loaded: true` 和 `assetVerification: verified`。推理失败不会伪造模型就绪。

```js
const embedding = new EmbeddingService({ profileId: 'builtin-multilingual' });
const query = await embedding.embedQuery(text, { signal, profileId: 'builtin-multilingual' });
// { vector, profileId, modelVersion, dimensions }
// 查询向量及固定模型版本。
const documents = await embedding.embedDocuments(texts, { signal, profileId: 'builtin-multilingual' });
// { vectors, profileId, modelVersion, dimensions }
// 文档向量数组及固定模型版本。
const status = embedding.status();
await embedding.close();
```

查询按模型卡添加 `query: `，文档添加 `passage: `；均使用平均池化和归一化。
公开请求默认可含 512 条、原生接纳最多 128 个请求，并同时限制 UTF-8 字节、保守 token 估算及实际批次 token；
`requestLimits` 可在有限范围内调整，不再靠固定 64 条或 32 个调用方 Promise 判断容量。
取消后仍执行或排队的原生 payload 持续计入 ticket，worker 确认 settled/idle 或退出后才解除背压。
内部组大小与 token 预算依据已批准资源及吞吐反馈计算，按实际 token 长度分桶减少 padding，返回顺序不变。
每条最多 512 个 token，包含前缀及特殊 token；超长返回 `EMBEDDING_INPUT_TOO_LONG` 和实际计数，不静默截断。
调用方应拆分来源或保留词法检索。关闭先等待原生调用结束及释放确认；超时只回收本服务拥有的进程。

## 统一资源预约和真实后端验证

`inference-resources.mjs` 只描述固定模型的需求，并使用 Platform 的唯一 `ResourceBudgetService`：
Orchestration 可将同一 `resourceService` 注入嵌入与重排构造函数。独立服务默认共享同一预算实例；
模型关闭只释放自己的预约，不关闭其他模型仍使用的公共服务。

- tokenizer、驻留模型内存、GPU 显存与执行 CPU 分开预约。冷 `fitDocuments` 只申请 tokenizer 内存和 CPU，不提前加载模型或申请 GPU；真正推理时只申请模型内存差额。
- CPU 执行预约在 worker 确认 idle 后释放；调用方取消不代表原生任务已结束。驻留 RAM/GPU 在模型空闲时仍计入总账，正常退役或确认 CPU 回退后才释放对应资源。
- `EmbeddingRouter` 对已有会话执行空闲回收：默认连续空闲两分钟后，在至多三十秒的巡检间隔内退役 CPU/GPU worker；连续查询重新开始保温期。前台资源准入不足时可调用 `releaseIdleResources` 提前让出已空闲会话，实际归还额度仍等待原生释放确认与进程退出。正在准备、执行或取消但尚未收到 settled/idle 的请求不能回收。新请求等待同一退役屏障后按原 profile 与设备偏好重建，旧操作不重放；失败的退役不能创建未核验的替代进程。
- 预约定期续期；监控无法续期时停止接受新任务，不把未知资源当作空闲。服务失效的保守降级由 Platform 统一处理。
- 多个调用方共享启动申请；取消其中一个不会取消其他调用方。线程额度变化只在完成请求后重建原生会话，保持 tokenizer、权重、dtype、池化和归一化不变。
- CPU batch 受批准线程、RAM 和 token 预算限制；GPU batch 独立按显存、主机供数缓冲和 token 预算计算，不因 CPU 只分到一个线程而固定降到四条。输入先按实际 token 分桶，短输入可以采用更大组，长输入仍受 padding token 上限限制。内存分配失败最多减批五次；兼容 CPU profile 的 GPU 单条批次仍失败时，释放 GPU 会话后用已预约 CPU 重算一次。独立 GPU profile 拒绝改用 CPU 产生向量。取消永远不会触发恢复。
- 原生进程崩溃拒绝所属请求。后续新调用方最多重启一次；GPU 崩溃会禁用此模型实例的相同后端，再由新调用走 CPU，不自动重放失败或取消的请求。
- 模型 PID 登记到自己的驻留预约；资源服务只采集拥有者后代，不扫描其他软件命令行。启动需求按固定模型、dtype、已固定的 hidden size/attention heads、序列长度和 batch 估算；独立推理进程的工作负载 RSS 增量可按模型/backend/dtype 分组修正后续申请，并保留 12.5%＋32 MiB 余量。该增量包含原生运行时和 tokenizer，不是精确权重 RAM，更不当作 GPU 显存。每个分组最多 16 个样本、全局最多 64 组，根进程 RSS 和其他 PID 样本不接纳。
- CPU、DML 与任务各自的热反馈以 `tokens/s` 为单位；冷加载、排队不参与热吞吐 AIMD。批次缩小直接在完成原生调用后应用，不重建模型；线程、后端或设备 ID 改动只在完成请求后重建。状态最多保存 16 条调整原因，并区分额度已批准与 worker 真正已应用。
- 驻留内存每次扩容完整登记新租约，按 64 MiB 台阶申请；关闭时释放全部扩容租约，不能覆盖前一张扩容回执而留下永久账目。

`inference-backend.mjs` 对 GPU 使用固定 q8 模型、真实推理和 CPU 参考输出校验。
本轮部署范围只验证 Windows x64。ONNX Runtime Node 1.21.0 的 Windows GPU 后端是 DirectML，
不能在 Windows 上直接把 `device` 改成 `cuda`；不因其他系统的历史分支存在而声称它们已通过本轮验收。
DirectML 必须由资源层确认 NVML 物理 GPU 与 DXGI adapter 的 UUID/LUID 映射；
无法核实则报告 `GPU_DEVICE_MAPPING_UNVERIFIED`。多显卡及 AMD/Intel 映射不属于本轮新承诺。
GPU 会话禁止隐式 CPU 算子回退，实际输出必须在 `1e-4` 相对/绝对容差内与相同 q8 的 CPU 参考兼容，
成功后才报告 `gpuValidated: true`；失败保留原 CPU 模型并返回有类型的诊断，不自动换 fp16 权重或混用向量空间。

原有 CPU 空间的严格 GPU 探测仍可能报告 `CPU_OPERATOR_FALLBACK_REQUIRED`，此时保留 CPU 数学路径。
可选 GPU profile 通过真实 operator trace 审计允许少量 CPU 形状算子，同时使用独立向量空间：

- `builtin-multilingual-dml-q8` 复用原 q8 权重、tokenizer 和许可证，不重复下载资产；模型版本和实现身份标明 `dml-hybrid-v1`。
- CPU/GPU 的 q8 数值并非逐项相同，不能将其直接混用。原 CPU space `6e803abd…24a2` 保持不变；GPU space 为 `b098e42d…363b`。选择 GPU profile 后应重建对应语义投影，旧记录、原文和旧 CPU 索引不靠重命名伪装迁移。
- 在独立临时目录，仅对中英文本和代码合成探针启用 ONNX native profiling；释放会话刷新 trace，再读取有界记录。必须发现实际 GPU 矩阵计算算子、验证多个向量归一化及 CPU 参考方向、复测 GPU 可重复性，再创建不记录正式用户输入的常规会话。
- `inferenceBackend.executionMode: 'hybrid'`、`cpuOperatorFallback: true` 和 `operatorAudit` 明确报告实际分区，不能称全 GPU。GPU profile 缺少资源或审计失败时拒绝投影，不输出带 GPU 空间 ID 的 CPU 向量；后续新调用可有界重新审计一次已退出的 GPU worker。
- 2026-10-08 的真实短测中，E5 探针记录 800 个 DirectML kernel（其中 192 个矩阵计算）与 4 个 CPU 形状 kernel；GPU 重排记录 830 个 DirectML kernel与 16 个 CPU 形状 kernel，中英检索和排序链路通过。
- 八条较长合成文本的两次热推理中，GPU 平均约 459 ms、CPU 约 784 ms；短问题则 GPU 约 17 ms、CPU 约 4.5 ms。这些仅是小型样本，不是普遍 1.7 倍加速或完整任务成功率基准，不能因为可用 GPU 就让简单聊天等待 GPU 初始化。

完整准确率、长时峰值、设备变化和安装包首次运行仍需之后验收；本轮仅执行短测。
ONNX Runtime 1.21 Node 的 profiling 配置与 native dispose 行为已按
[官方配置实现](https://github.com/microsoft/onnxruntime/blob/v1.21.0/js/node/src/session_options_helper.cc)
及[会话实现](https://github.com/microsoft/onnxruntime/blob/v1.21.0/js/node/src/inference_session_wrap.cc)核实。

```js
const embedding = new EmbeddingService({ resourceService, cpuThreads: 12, devicePreference: 'auto' });
// The service grants the actual thread count; this number is a request, not permission to exceed the budget.
// 实际线程数以资源服务批准结果为准；此数字是需求，不是超额使用资源的权限。
const status = embedding.status();
// status.resourceReservation / status.inferenceBackend report allocation and validated execution separately.
// 分开报告资源预约与实际通过验证的执行后端。
```

```js
const gpuEmbedding = new EmbeddingService({ resourceService, profileId: 'builtin-multilingual-dml-q8' });
const gpuReranker = new RerankerService({ resourceService, profileId: 'builtin-multilingual-reranker-dml-q8' });
// Changing the selected profile requires compatible index rebuilding; it does not relabel existing vectors.
// 切换 profile 需要重建兼容索引，不能直接给旧向量改标签。
```

缓存和索引必须同时记录 `profileId` 与 `modelVersion`，不能因为 builtin ID 未变就复用不同权重的向量。相似度分数不是回答正确率；阈值需要按中英文、代码和资料评测集验证。

结果还包含 `embeddingSpaceId`、`inputProjectionVersion`、`modelAssetSignature` 和 `assetSignature`。
向量空间 ID 为 SHA-256 的 64 位小写十六进制值，绑定实际模型、版本、维数、E5 前缀、平均池化、L2 归一化，
以及权重和分词器等运行资产哈希。来源分块与 `embeddingInputVersion` 由 Data 单独管理；
模型空间不借用资料派生版本，也不能仅靠维数一致混用其他模型向量。

推理 IPC 请求按独立递增 ID 配对。取消只拒绝所属请求，晚到结果不改变后续请求；发送失败立即清理等待项。
重排模型载入失败会报告原始资源错误并自然退役，过期 `ready` 通知不能把错误状态恢复成就绪。
关闭等待模型释放确认，超时仅终止本服务拥有的独立进程并等待退出，不影响网关进程中的原生状态。

## 可选离线重排

`RerankerService` 复用现有 ONNX 依赖，按问题与片段配对打分。固定 `Xenova/bge-reranker-base` revision `280bcc27a84e0b898c251e06fddb25171bd9b101`，q8、最多 512 输入 token；公开权重、分词器、原模型卡、转换说明和 MIT 许可由 `reranker-profile.mjs` 固定并校验。它是兼顾本机 CPU/包体的可选基线，不声称是最新排行榜模型。

设置中的“复杂查询重排”默认关闭；启用后仅研究/复杂任务对最多 20 个去重候选按需打分。普通聊天、简单查询和未配置实例不加载权重。推理 worker 禁止联网，原片段、原始排序分数、来源引用保持不变；仅打分预览可截短，返回实际截短数量。sigmoid 分数是相对相关性，不是答案正确概率。重排缺资产或失败时保留已有检索并返回诊断，不中断聊天。

开发构建使用 `npm run prepare:reranker`；安装包通过 `IncludeBundledRerankerModel` 按校验白名单带入约 296 MB 的公开资产，用户无需 Python 或数据库服务。推理首次加载、取消和关闭以实际原生验收为准；不把缺资产时的降级测试描述成重排成功。

```powershell
$env:KYNXA_TEST_NATIVE_RERANK = '1'
node --test tests/retrieval-reranker.test.mjs
```

## 构建与验证

在 `apps/model-gateway` 中运行：

```powershell
npm ci --no-audit --no-fund
npm run prepare:embedding
node models/retrieval/prepare-embedding-model.mjs --offline
node --test tests/retrieval-embedding.test.mjs
```

只有开发/构建还原首次联网下载。安装包包含权重和运行库，用户首次使用不需要另下载这些组件。桌面构建通过 `IncludeBundledEmbeddingModel` 自动准备资产；完整安装包的复制、许可、native 加载和断网首用仍需在发布验收中验证。

构建并发使用操作系统持有的独占锁，进程被终止也会自动释放。`bundle-files.txt`、VC 的运行库清单和构建日志仅用于构建；安装包只保留清单指定的模型、运行库及许可证，不复制遗留锁或多余缓存文件。

本轮已验证 Debug x64 构建输出中自己的 Node、模型资产和 native 依赖能生成 384 维向量；四个 VC 核心 DLL 实际从输出包内加载。此结果不替代正式安装器、空白 Windows 机器和 Windows arm64 的发布验收。

原生嵌入和重排分属独立进程，避免同一个 Node 进程中多个 ONNX worker 共享原生状态；任一子进程崩溃会拒绝其待处理请求，由检索协调器保留词法能力。父进程断开会触发退役及有界看门狗。输入过长仍按真实 tokenizer 拒绝；索引器只剔除被明确定位的超限块并继续同批合法块，保留原文和缺失向量诊断。
