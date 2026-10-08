# Unified Agent evaluation fixture / 统一智能体评测样本

The opt-in adapter runs the real `ModelRuntime`, formal conversation/result stores, local retrieval index and resource authority. It never connects to the running desktop gateway. API credentials are read from environment variables into memory; no model connection file is created.

此适配器显式运行实际 `ModelRuntime`、正式聊天与工具归档、本地检索索引和资源服务，不连接正在运行的桌面网关。API 凭据从环境进入内存，不创建模型连接配置文件。

## Short local verification / 本地短测

From the repository root:

```powershell
node --test tools/development/tests/unified-agent-evaluation-adapter.test.mjs
```

This starts a local HTTP fixture, rather than a cloud model. Five variants each repair a fresh copy, read actual retrieval evidence, execute declared Node checks and pass an independent validator. The original deliberately broken fixture remains unchanged. These checks validate the evaluation pipeline; they are not DeepSeek scores, a public benchmark or evidence of improved task quality.

短测只启动本机 HTTP 样本，不调用云端模型。五组分别修复全新副本、读取真实检索归档、运行声明的 Node 测试，再经过独立验收。原始错误样本保持不变。这是评测链路验证，不能称为 DeepSeek 得分、公开基准或能力提升的证明。

## Explicit model evaluation / 显式模型评测

Set `KYNXA_EVAL_BASE_URL` and, when required, `KYNXA_EVAL_API_KEY` in the calling shell. The URL is the base API endpoint, matching a normal KYNXA connection. Optional `KYNXA_EVAL_PROTOCOL` supports `openai-completions`, `openai-responses` and `anthropic-messages`; the default is completions. Do not put keys in a manifest or commit them.

在执行命令的终端设置 `KYNXA_EVAL_BASE_URL` 和需要时的 `KYNXA_EVAL_API_KEY`。地址是普通 KYNXA 连接使用的 API 基础地址。可选 `KYNXA_EVAL_PROTOCOL` 支持三种现有协议，默认 completions。清单和版本控制中不能存放密钥。

Copy the manifest to an owned evaluation directory and set its `model` to one exact available model ID before running:

```powershell
node tools/development/evaluate-unified-retrieval.mjs --manifest tools/development/fixtures/unified-agent/smoke-manifest.json --adapter tools/development/unified-agent-evaluation-adapter.mjs --output artifacts/verification/unified-agent-smoke.json --smoke
```

The example contains one development task only. It is intentionally a smoke test. A full run requires a separately authored held-out split and representative tasks; this change does not perform that run. Relative fixture roots resolve against the repository working directory. A task may override `fixtureRoot`, `validatorRoot` and `sources` to select its own trusted fixture.

样例仅有一条开发任务，刻意只用于短测。完整评测需要另行准备留出集和代表性任务，本次未执行。相对样本目录按仓库工作目录解析。每个任务可覆盖样本根、验收根与资料清单，使用自己的可信样本。

## Real ablations / 实际消融

| Variant | Gap planning | Adaptive resources | Relations | Navigation experience |
| --- | --- | --- | --- | --- |
| baseline | Off | Off | Off | Off |
| gap-only | On | Off | Off | Off |
| resource-only | Off | On | Off | Off |
| gap-resource | On | On | Off | Off |
| integrated | On | On | On | On |

These options enter the constructor-only runtime evaluation policy. They cannot be enabled by model arguments. Disabled relations/experience are absent from the model catalog and rejected by the coordinator. Fixed variants use the same physical capacity authority with CPU requests capped at 2, no GPU approval or feedback tuning, indexing batches of 8, 48 per-channel candidates, 64 fused candidates, 8 selected excerpts and an 8,192-token evidence budget. Adaptive variants use the real grants and advice; they do not bypass hardware pressure limits. Each case has a new authority so previous-case feedback does not leak into the next.

这些开关进入仅构造器可注入的真实评测策略，模型参数不能打开。禁用的关系、经验工具不会出现在模型目录，协调器也会拒绝调用。固定组仍经过同一物理容量审批，CPU 请求最多 2、无 GPU 审批和反馈调优；索引批次 8、每通道候选 48、融合候选 64、选取 8 个摘录，证据预算 8,192 tokens。自适应组使用真实租约与建议，不能绕过硬件压力边界。每个样本使用新资源服务，前一组反馈不污染下一组。

## Sources and acceptance / 来源与验收

- `sources` contains stable evaluation IDs and one regular relative file per ID. Imported UUIDs differ between independent copies. `sourceBindings` records the actual formal source ID, hash and relative path. Recall is based on source IDs read from durable retrieval archives, not citations asserted by the assistant.
- `acceptanceCommands` contains fixed Node test scripts relative to `validatorRoot`, a unique ID, an optional label and a bounded timeout. Arbitrary executables, shell commands or script arguments are unsupported and rejected explicitly. Validators read the copied workspace through `KYNXA_EVALUATION_WORKSPACE`.
- The model's `terminal.run` is limited to `{ "command": "node", "args": ["--test", "declared-check-ID"] }`. It runs the exact declared script with Node permissions and test isolation disabled, using the validator directory as cwd and the copied workspace environment variable. A zero exit code without a nonempty, fully passed TAP test summary is not accepted. The independent evaluator runs the check again after the model finishes; only these later receipts carry `origin: independent-validator`.
- Validator copies live outside the agent's workspace. Their hashes and original fixture hashes are checked before/after execution. Links, special files, oversized files and paths outside the copies are refused. Official MCP, desktop tools, web tools, user skills and arbitrary host terminal commands are not started.

资料以稳定评测 ID 标注，每个 ID 对应一份普通相对路径文件。副本导入产生不同正式 UUID，映射会记录真实 ID、哈希与相对路径。召回统计来自持久检索归档，不采用模型声称的来源。验收只支持清单中固定的 Node 测试；模型通过声明的检查 ID 调用，最后由评测器重新独立执行。验收文件位于模型工作区以外，原样本与验收器均检查哈希，越界工具和链接拒绝执行，不启动默认 MCP、桌面、网页或任意宿主命令。

## Measurement boundaries / 测量边界

All model requests use the same manifest model and sampling configuration through a bounded loopback measurement proxy. Each ordinary multi-round task is one attempt; model requests are counted separately, not mislabeled as retries. Provider token usage is summed only when every request has known usage; otherwise it stays unknown. A supplied seed is transmitted only when the manifest explicitly declares provider support. A recorded seed does not establish deterministic provider behavior.

所有请求使用同一模型与采样配置，通过有界本机转发器记录实际 usage。正常多轮工具循环算一次任务尝试，模型请求另计，不能冒充重试。每次 usage 都明确时才合计，否则保留未知。仅清单声明供应商支持时才发送 seed；记录种子不等于供应商输出已确定。

`peakRssBytes` is the maximum gateway RSS sampled at actual operation boundaries, not a continuous whole-machine or complete child-process peak. `peakGpuBytes` stays unknown because global GPU telemetry cannot establish a single case's allocation peak. `firstUsefulWorkMs` begins at copy/setup start and records the first actual retrieval/tool dispatch. Preparation, retrieval and model transport times are separate; model time includes provider/network and local proxy overhead. Scope violations count observed denied tool attempts; they are not a complete security certification.

内存字段是实际操作边界采样到的网关 RSS 最大值，不是持续采样的整机或全部子进程峰值。GPU 单样本峰值保持未知，不能把全局 GPU 数字冒充任务测量。首次工作时间从副本与初始化开始，到第一次实际检索或工具派发；准备、检索和模型传输耗时分开记录，模型耗时包括网络及本机转发开销。范围违规数仅统计已观察的拒绝调用，不是完整安全认证。

The manifest fixture and validators must be trusted. Node permissions restrict filesystem access and child/worker execution for validation, but this is not an AppContainer network sandbox. Do not evaluate untrusted executable samples with this adapter. Queries beyond the current 4,000-character retrieval contract and multi-window labeled sources are explicitly unsupported, rather than truncated. Real model quality, larger repositories, all three protocols and long-running pressure need subsequent evaluation.

样本与验收器必须可信。Node 权限限制文件和子进程等能力，但不是 AppContainer 网络沙箱，不能用本适配器执行陌生的可执行样本。超过当前 4,000 字符检索合同或一份来源被拆成多个文件窗口时会明确拒绝，不静默截断。真实模型质量、大型仓库、三协议端到端与长期压力仍需后续评测。

## Version adapters and resumable logs / 版本适配与可恢复日志

`tools/development/evaluation-version-adapter.mjs` provides explicit version dispatch. New version-1 Agent manifests reuse `createEvaluationAdapter`; their model, dataset and independent-check scoring remain unchanged. The version helper does not start an evaluation by importing it.

`tools/development/evaluation-version-adapter.mjs` 提供显式版本分派。新版 version-1 清单复用当前实际适配器，模型、数据版本和独立验收评分保持原约定；导入版本模块不会启动评测。

The existing CLI can select the version facade using `--adapter tools/development/evaluation-version-adapter.mjs`; version-1 execution still reaches the same real adapter. Legacy runner injection is an explicit programmatic interface, not an automatic CLI compatibility claim.

现有 CLI 可用 `--adapter tools/development/evaluation-version-adapter.mjs` 选择版本入口，新版 version-1 仍到达同一实际适配器。旧运行器注入属于显式编程接口，不代表 CLI 已自动兼容旧矩阵。

`freezeLegacyLocalizationSuite` accepts the original public-case, selection-only, configuration and scoring-definition bytes. It checks the original issue hashes and emits only public task text and portable settings; private machine paths, selection reasons and patch targets are excluded. Optional Chinese-pair bytes must bind to the same public-document hash and original issue hashes. The function freezes existing translated bytes; it does not certify translation equivalence. Missing Chinese receipts keep that language explicitly unsupported.

`freezeLegacyLocalizationSuite` 接收旧公开题目、选样专用清单、配置及评分定义的原始字节，校验原问题哈希，只输出公开题文与可移植设置，不转发机器路径、选样理由或补丁目标。可选中文配对须绑定相同公开文档与原问题哈希。它冻结已有译文字节，不认证语义等价；没有配对回执的中文组明确不支持。

The legacy 30-case matrix uses A/B indexing, English/Chinese and single/autonomous JSON-action arms. Its patch-target file coverage is not the new five-way Agent task-success metric. Legacy dispatch therefore requires a separately supplied runner pinned to the original model, wire protocol, configuration and scoring hash; a mismatched/missing runner fails explicitly. No Python runner is copied, old index is reused or historical score is merged. `normalizeVersionedEvaluationResult` preserves the original metric and keeps code-repair success unknown.

旧 30 题使用 A/B 索引、英中、单次/自主 JSON-action 组；补丁目标文件覆盖并不是新版五组 Agent 的任务成功率。因此旧版本需要另外提供固定原模型、协议、配置与评分哈希的兼容运行器；缺失或不匹配时明确拒绝。本模块不复制 Python 运行器、不复用旧索引、不合并历史分数。结果归一化保留原指标，代码修复成功率仍为未知。旧矩阵还没有自动变为新版实际执行矩阵，不能宣称已完成等条件桥接。

`createEvaluationRunJournal({ outputRoot, contract, runId })` creates a separate run subdirectory, pins the full contract hash and appends bounded attempt/stop events. `requestStop()` exposes cancellation through the journal signal; the caller must forward that signal to its runner and write `finishAttempt` only after its actual executors settle. Resuming with `resumeRunId` rejects changed model/tasks/settings/scoring, incomplete log tails and unconfirmed active attempts. Interrupted processes require an independent host reconciliation receipt; recovery records `stopped`, `failed` or `blocked`, never a fabricated completed attempt. Completed arms are not silently repeated. The caller chooses new attempts explicitly and retains prior cost records.

`createEvaluationRunJournal({ outputRoot, contract, runId })` 创建独立运行子目录，固定完整合同哈希并追加有界尝试与停止事件。`requestStop()` 通过日志对象的 signal 发出取消；调用方必须传给实际运行器，并在执行器真实结束后才写终态回执。以 `resumeRunId` 恢复时，模型、题目、配置或评分变化、截断日志、未确认结束的尝试都会被拒绝。中断进程须由宿主独立核对回执确认，恢复只能记录停止、失败或阻塞，不能伪造完成。已完成组不静默重跑；新尝试由调用方明确选择，既有成本保留。

```powershell
node --test tools/development/tests/evaluation-version-adapter.test.mjs
```

These tests use synthetic manifests/receipts and temporary directories only. They perform no legacy benchmark, model call, source download or historical-data migration. The journal supplies a reusable stop/resume interface; the existing CLI remains the explicit new version-1 evaluation entry.

短测仅用虚构清单、回执与临时目录，不执行旧基准、模型请求、源码下载或历史数据迁移。日志提供可复用的停止/恢复接口；现有 CLI 仍明确运行新版 version-1 评测入口。
