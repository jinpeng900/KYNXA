# models

主责：C · 模型与上下文。

维护云端/本地模型协议、能力/预算、流读取、上下文与历史配对投影、模型配置。

可读取 Data 的配置路径和结果公开投影，复用 Platform；不依赖 Tools 执行服务或 Orchestration。历史压缩只修改请求投影，不修改正式原文。

参见 [五人职责](../../../docs/team/README.md)、[依赖与合同](../../../docs/architecture/team-boundaries.md)。根目录 `server.mjs`、`initialize-storage.mjs`、`migrate-storage.mjs` 为稳定启动入口；不要在本目录再创建另一份正式服务或数据源。

跨域变更同步生产调用方、共享 DTO、测试和随包清单。新增/移动模块后运行 `node tools/development/check-architecture.mjs`；回归选择见 [开发规则](../../../.agents/skills/kynxa-development/SKILL.md)。

`local-model-resources.mjs` 提供外部本机运行时只读观察。Ollama（包括其中运行的 Qwen）通过
[官方 `/api/ps`](https://docs.ollama.com/api/ps) 和
[只读 `/api/show`](https://docs.ollama.com/api-reference/show-model-details) 获取是否加载、实际加载上下文、服务报告的模型大小及显存量。
只接本机 loopback，请求有超时/取消、禁止重定向、有界解码与短期缓存；不读取环境变量或启动命令，不导出响应中的模板与个人模型路径，也不启停、卸载或杀用户模型。

非 Ollama 的本机接口可通过 llama.cpp [只读 `/props`](https://github.com/ggml-org/llama.cpp/blob/master/tools/server/README.md#get-props-get-server-global-properties) 核验后端及实际 slot 的 `default_generation_settings.n_ctx`。
观察请求附带所选模型及 `autoload=false`，不会为了探测新版路由服务而加载模型。只有 `n_ctx`、`total_slots`、`params`、`chat_template` 等官方字段族符合有界合同才标记为 `backend: 'llama.cpp'`、`source: 'llama-cpp-props'`；模型名称、provider ID 与端口不能代替该证据。
返回 `endpointOrigin` 将观察绑定到同一服务。睡眠状态保留已声明的上下文上限，但 `runtimeContextTokens` 不冒充已分配上下文；内存量、GPU 设备与 KV shape 仍未知。

结构元信息可估算 KV cache，但 dtype/滑动窗口等未知参数保留不确定性；服务 `size/size_vram` 可能已含 KV，不能把估算再次加到观测驻留量扣预算。
外部模型不是 KYNXA 拥有的资源租约，系统 free memory 已反映其占用，只用于协调后台工作。
`beginGeneration` 只知道本应用调用是否进行；没有本应用请求不等于其他客户端已空闲。没有标准资源接口的本地 OpenAI/Qwen 服务明确返回未知，不能伪造精确 RAM/GPU 占用。

`external-model-demand.mjs` 只计算尚未发生的分配：已加载上下文覆盖目标时增量为零；实际扩张只计算
新增 KV；冷加载估算权重与 KV。冷加载优先采用有界 `/api/tags` 的序列化模型大小，再回退参数量与
量化估计，保留运行余量。服务未给出上下文时仍协调已知权重，KV 保留未知，不能称为完整加载准入。
连接的 1M 输入上限不是 Ollama `num_ctx`；当前 OpenAI 协议未请求该扩张，不据此预约 1M 的 KV。

A 的 `orchestration/external-model-admission.mjs` 将未来增量接到共享 Rust 资源服务，先退役本应用
自有且确认空闲的 GPU 嵌入/重排，再派发生成。活跃原生任务、未确认取消和用户拥有的 Ollama 不被
停止。GPU 上传的主机暂存仍是观察建议，未知外部接口或设备映射保留未确认状态，不冒充完整准入。
模型同身份的并发请求共享预约；读取完整响应才释放预测保护。取消后的未知分配保留，确认目标已
加载后可解除未来分配预约，但这不等于取消的生成已成功，也不解除其他未知显存租约。

`local-tool-compatibility.mjs` 为已核验的本机 llama.cpp 提供工具声明投影。llama.cpp 的 [grammar parser](https://github.com/ggml-org/llama.cpp/blob/master/src/llama-grammar.cpp) 会限制重复次数与展开复杂度的乘积；日志中的 `char{0,2000}` 和带上界的数组规则可能在采样器初始化前遭拒。
`projectLocalToolCatalog(catalog, { connection, localModel })` 返回 `{ catalog, applied, mode, omittedConstraintCount }`：默认 `bounded` 去除有限 `maxLength/maxItems` 和大于 32 的 `minLength/minItems`，保留工具数量、`name/wireName`、字段类型、`required/enum`、属性与引用。
该目录仅供 `toolDeclarations` 生成模型请求；调用解码、权限与执行器必须继续使用原始目录和原始 schema，投影不改变真实参数验证。

`localToolGrammarRejection(body, { status })` 只将明确的 HTTP 400 grammar 拒绝分类为固定标记；已读错误体可与上下文错误分类共享，避免重复消费响应流。
`planLocalToolGrammarRecovery({ rejection, connection, localModel, catalog, recoveryAttempts, hasVisibleOutput, executedToolCalls })` 在同一被拒模型步骤尚无可见输出及工具执行、重试次数为零时，返回一次 `structural` 投影与 `recoveryAttempts: 1`。
此步骤继续保留所有工具、类型、必填及枚举，进一步移除 pattern/format/数值区间等声明约束；投影没有变化、证据不可信或再次拒绝时返回 `null`。调用方不重放以前成功的步骤或执行回执，失败仍按模型错误返回。
兼容性测试使用合成 schema、原始执行验证与模拟 `/props` 响应，不启动或请求真实模型。
