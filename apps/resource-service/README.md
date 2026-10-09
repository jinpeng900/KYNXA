# KYNXA resource service

该 Rust 子进程由网关拥有，不是系统级服务。通过私有 stdin/stdout JSONL 接收
`health`、`snapshot`、`acquire`、`cancelAcquire`、`renew`、`release`、`registerExecutor`、`report`；单帧最多 64 KiB，有限排队，拥有者
关闭输入流时退出。不读取其他应用的命令行、文件或私有内容。

基础指标使用 `sysinfo` 采集逻辑 CPU 数、CPU 使用率和系统可用内存，每秒最多更新一次。
首次 CPU 使用率未知。Windows NVIDIA GPU 使用系统驱动的 `nvml.dll` 只读采样，限定
`LOAD_LIBRARY_SEARCH_SYSTEM32`；不搜索工作目录、不运行 `nvidia-smi` 或 PowerShell。
GPU 活跃或有预约时最多每秒采样，空闲时降至每 5 秒；仅管理 physical device 0。
没有兼容驱动、无显卡、平台不支持或采样失败时，显存返回 `null` 和 `unknown`，不批准
GPU 申请。只验证传感器可读不能证明模型已经在 GPU 上执行。

CPU、主机驻留内存和 GPU 显存可分别申请，并通过同一个原子预算扣除既有预约。
GPU 申请保留最多 512 MiB 系统余量；使用内存的工作进程退出并确认回收后再释放预约。
Rust 主机内存预算保留 512 MiB 及三成空闲内存，且最多使用总内存的一半；降级预算
保留 512 MiB 及四成空闲内存，且最多使用总内存的四成。尚未兑现和占用未知的预约另行扣除；
同轮系统可用内存与已核验自有进程 RSS 增量可确认 RAM 兑现，避免完整重复扣减。
兑现以首次登记基线、PID 与启动时间为准，同 PID 去重，释放后的分配器残留不能抵扣新预约。
总内存一半的硬限仍按完整预约约束。未取得每进程显存归属证据时，GPU 仍为未核验预约，
不以全局 free memory 的下降推断某个模型已经兑现，也不放宽隔离。
lease 最多 128 个，期限为 1～300 秒。过期 lease 进入隔离保留状态，不自动重新分配，
避免失联进程仍占内存时重复批准。资源服务异常时，网关仍保留原预约并转为更保守的
CPU/内存预算；不会因监控失联增加资源或批准 GPU。

资源紧张时 `acquire` 可指定 `waitMs: 0..60000`。等待队列最多 32 条，前台优先；后台
等待两秒后获得优先机会，每四次准入也为后台保留机会。取消从队列移除，绝不自动恢复；
超过期限返回可恢复的等待超时。排队期限与 IPC 故障超时分开，不把正常等待误判成监控崩溃。

`report(leaseId, feedback)` 接收吞吐、延迟、队列长度、分配失败、前台延迟和进度。
不同 taskId、backend、phase 与 unit 的吞吐各自建立有限历史，不混比 CPU/GPU、文档/秒与 token/秒。
只有 hot-inference 样本可驱动吞吐增长；冷加载、排队和一般操作分别记录。内存或前台压力立即减半
后台份额；增加份额需要同一任务吞吐改善、延迟不恶化及五秒保持时间。新批准的后台 CPU
线程数和 `suggestions.batchMultiplier` 会应用调整，既有执行不被擅自迁移设备。

预约返回 `suggestions`：ANN 分片/缓存字节、建图并发、候选数量、融合数量和证据 token
预算。ANN 内存不超过该预约已经批准的内存，执行并发不超过批准 CPU；调用方还需遵守
模型上下文和自身数据一致性约束。这是同一资源权威的建议，不另设独立抢占调度器。

`registerExecutor` 检查拥有者 PID 或实际后代进程，并记录真实启动时间以识别 PID 复用。
每秒仅刷新已登记 PID 的 RSS、CPU 和观测峰值；不读命令行/环境。工作线程共享 root RSS，
同一 PID 去重；未知指标保持 null。确认进程退出或 PID 复用后才回收相关预约；最近 32 条
结束记录保留观测峰值。采样不能捕捉所有瞬时峰值，外部既有 Ollama 不被注册为可管理进程。

Data worker 使用 `resource-worker-client.mjs` 委托到拥有者的同一服务，取消、截止时间和
消息数量有界。线程退出不能证明其 fork 子进程停止；子进程状态无法确认时继续隔离保留
内存，不提前回收。普通任务的续租与实际完成后释放由 `resource-task.mjs` 统一接线。

`ResourceBudgetService.reconcile({ restartService, signal })` 是显式受控恢复入口。服务故障后先停止
准入并确认旧服务退役，再向新服务恢复完整未释放账本、核对登记 PID 启动身份与实际退出。
未知所有者继续隔离；恢复失败不能把债务清零。worker 只能请求已有服务，不能擅自重启全局权威。

Node API 位于 `apps/model-gateway/platform/resources/resource-client.mjs`，同一个网关使用
`sharedResourceBudget()` 的单实例。检索协调层决定工作价值；资源服务只决定可批准的
CPU/内存/显存，实际模型后端、设备序号映射、批量和 token 限制仍由推理层管理。
CUDA_VISIBLE_DEVICES 与 DirectML/DXGI 序号不一定等于 NVML physical device 0。Windows
DirectML 映射通过 NVML UUID、系统 CUDA 驱动 UUID/LUID 与 DXGI AdapterLuid 逐项核对，
只有相同设备才返回 `executionProvider: dml`、`executionDeviceId` 与 `mappingStatus: verified`。
无法映射时保留遥测但不能据此启动另一个 GPU；不支持的后端需明确降级。

开发构建：

```powershell
cargo test --manifest-path apps/resource-service/Cargo.toml
node tools/development/prepare-resource-runtime.mjs --runtime win-x64 --required
```

产物与第三方许可证位于 `artifacts/runtime/resources/win-x64/`，随包放在
`runtime/resource/`。最终用户无需 Rust、Cargo 或 Python。没有 Rust 开发工具链的
开发机可以使用明确标识的保守降级；发布流水线应使用 `--required`。
构建使用编码 Rust flags 和 `--remap-path-prefix` 去掉仓库绝对路径；源码、脚本和锁定依赖的
指纹改变后重建缓存，白名单文件逐项 SHA-256 核对。当前发布验收范围仅 Windows x64。

本轮仅做短测；真实 GPU 模型算子由推理层单独验收。资源服务已验收 Windows x64 构建、
NVML 采样、严格 DML 映射、预约、有限排队/取消/公平规则、反馈控制和自有 PID 检查。
多 GPU、AMD/Intel GPU、磁盘/温度，以及长时间公平性、峰值覆盖和整机压力需要完整验收，
不能把短测当成全部硬件调度的性能保证。
