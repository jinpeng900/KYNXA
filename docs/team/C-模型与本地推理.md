# C：模型与本地推理

更新日期：2026-10-12。[团队边界](../architecture/team-boundaries.md)。交接分支：`kynxa_team/c-models`。

## 代码范围

`apps/agent-core/src/models/`：连接/发现、能力、供应商与工具协议、流式、输出预算、context/history 与真实 ORT 推理。`gateway.rs` 执行供应商 HTTP，`gateway_protocol.rs` 保存协议原文与映射，`gateway_stream.rs` 负责帧接线，`gateway_config.rs` 负责正式配置与本地观察，`gateway_context.rs` 执行既有摘要/压缩计划，`gateway_inference.rs` 拥有嵌入/重排。`apps/desktop/Services/Models` 拥有模型 API、SSE、预设与选择偏好；`apps/shared/Chat` 拥有 DTO，版本由 A 协调。

tool-protocols/tool-streaming 属模型映射；原生签名/加密续接与来源绑定保留，私有续接不能进入公开接口。

纯协议、流状态、上下文和记忆选择、历史配对与模型错误恢复计算留在既有规则模块；I/O 文件的跨域依赖按机器清单逐一登记。原生实现支持 OpenAI Chat/Responses、Anthropic 与 Ollama 的普通/工具流，接收取消与超时，不使用 Node/NAPI 或 SDK 子进程。发送前拒绝不计 HTTP 尝试，网络失败按真实 attempt 记录，不等于收费证明。

`gateway_inference.rs` 及纯 profile/批计划模块拥有固定 tokenizer/模型资产、CPU/DirectML 推理、池化归一化、实际设备与数值资格检查。E 提供实际资源授额和驻留租约，确认原生会话释放后才归还驻留；A 协调随包 DLL/资源，不能猜 GPU 设备或仅凭 provider 加载宣称已验证。更换模型、引擎或向量空间须协调索引兼容和固定小样本对照。检索耗时与质量验证使用固定配置，不能把模型差异当作纯 Agent 优化收益。

## 维护与交付

核对四种协议、普通/工具流最终状态、图片、取消、发现与凭据脱敏。同轮工具调用上限统一 32，ID/参数/正式记录在派发前校验，截断调用不得执行。总窗口、输入硬限、输出上限和记忆配额分开；大配置不代表供应商能力，未知代理/本地部署不猜额度。

context/history 只做请求投影，不截断 E 正式日志；保留完整有效轮次与调用/观察配对，不把公开思考当最终事实，也不注入兄弟聊天原文。`gateway_protocol.rs` 通过存储服务读取已校验公开结果和内部私有续传，验证来源、owner、前缀与原始哈希；`gateway_context.rs` 的语义摘要经 E 正式来源回执提交，失败可降级。失败/部分/执行未知的任务状态进入历史与摘要身份，不能只因传输完成认定已核验成功。纯域边界与集成执行边界分别遵循机器清单。

ChatStreamReader 与网关 SSE 同步核对事件、默认值、HTTP/机器错误码和取消。协议变化联系 A/B；工具字段联系 D；正式消息兼容联系 E。

## 验证

对应 Rust protocol/stream/output/capabilities/context/history 测试、真实本机模拟上游 HTTP/SSE，以及 chat-stream、model-context、model-presets 客户端 smoke；不消耗真实模型凭据。ORT 小样本验证明确 DLL/资产 SHA、token ID、向量空间、数值误差和实际设备；编译不能代替 CPU 对照或 DML 资格复现。旧 Node 测试成绩仅属历史证据。

本轮保留 UiText 依赖和公开合同。本地启停、下载校验/恢复另按需求推进，脚本启动不等于完整模型管理。
