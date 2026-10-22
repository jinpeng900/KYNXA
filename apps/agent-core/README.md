# Rust Agent 业务核心与原生网关

当前正式后端是 Windows x64 原生 Rust 网关，随首个正式版 1.7（安装发行版本 `1.7.0`）发布。`src/bin/kynxa-gateway.rs` 启动公开 HTTP/SSE，`src/gateway.rs` 处理传输，`src/gateway_contract.rs` 维护配置与错误合同。桌面启动随包 `runtime/agent/kynxa-gateway.exe`；后端不启动或发货 Node/NAPI。WinUI/C# 与 WebView2 界面保持，渲染 JavaScript 继续保留。实际接线见 [独立 Rust 网关](../../docs/architecture/native-gateway.md)。

## 六个领域

| 目录 | 职责 |
|---|---|
| `src/orchestration/` | 任务、步骤、来源/证据发布、上下文预算、审批、取消、恢复与完成核验；`gateway_loop.rs`、`gateway_runtime.rs` 接真实请求生命周期 |
| `src/models/` | OpenAI Chat/Responses、Anthropic、Ollama 请求与流、能力/配置、历史/私有续接、语义压缩及原生 ORT E5 嵌入 |
| `src/tools/` | 权限、发现、MCP/Skill、受控文件/来源扫描、网页/CDP、终端和 C# ToolHost；保存实际执行回执，不由模型声明成功 |
| `src/retrieval/` | 词法/E5 向量/RRF 轻量排序/原生 ANN、分块/结构、来源与证据版本、后台索引/资料作业及资源授权 |
| `src/storage/` | 正式会话/记忆/资料、JSONL/CAS、历史分页、受控文件与 SQLite 事务、归属/迁移和私有归档 |
| `src/resources/` | 请求资源需求、准入/驻留、设备资格、外部模型观察和独立 Rust 资源进程 IPC；未确认回收不释放占用 |

纯规则继续按领域限制导入及循环，实际 I/O 文件逐项登记为 `integrationModules` / `executionModules`。入口与接线不得重建另一套业务账本；文件归属与允许依赖见 [团队边界](../../docs/architecture/team-boundaries.md) 和 [机器清单](../../docs/team/module-ownership.json)。`src/lib.rs` 组合领域；`src/main.rs` 保留有界 JSONL 核心控制入口，不是桌面的 HTTP 网关入口。

正式日志、稳定 ID、CAS、原始 UTF-16 身份、工具调用/结果配对和私有供应商续接由领域服务维护。SSE 回放缓存有界，不能替代正式历史；传输 `completed` 与任务核验状态分开。成功副作用不重复派发，执行未知时保留事实并只作合法观察，不能切换空账本继续。模型/扩展/数据目录由配置与存储路径模块确定，不写死开发机位置。

## 构建与启动

从仓库根执行，开发构建需要 Rust 1.88+、Windows MSVC、项目声明的 .NET/Windows SDK。依赖版本锁定在 `Cargo.toml` / `Cargo.lock`；当前正式随包范围为 Windows x64。

```powershell
powershell.exe -NoProfile -File apps/desktop/Build/prepare-native-runtime.ps1 -RuntimeIdentifier win-x64 -Configuration Debug
dotnet build apps/desktop/KYNXA.Desktop.csproj -p:Platform=x64
```

原生 EXE、固定 DLL/模型及许可证准备到 `artifacts/runtime/native-desktop/win-x64/`，由桌面构建清单复制至 `runtime/`；工具宿主位于 `ToolHost/`，官方工具是 `apps/official-tools/` 静态资源。正式用户无需安装 Rust、Cargo、Node 或 Python。开发手动启动使用 [start-native-gateway.ps1](../../tools/development/start-native-gateway.ps1)，显式指定准备好的随包目录、独立测试数据/模型/扩展目录和端口；不得另启动一个写日常数据的网关。存储初始化由原生入口的 `--initialize-storage <绝对目录>` 完成。

## 验证

```powershell
cargo test --manifest-path apps/agent-core/Cargo.toml --locked
node tools/development/check-architecture.mjs
```

Node 命令仅是仓库开发期静态守卫。随包启动、PRI 与旧载荷清理检查使用 `tests/native-runtime-smoke`；`tests/native-gateway-contract-smoke` 读取本轮 `gateway_http` 实际生成的 SSE 并验证未修改的桌面解析器。具体参数、临时存储/模拟上游及验证限制见 [验证参考](../../.agents/skills/kynxa-development/references/validation.md)。多人协作统一构建，按影响选择测试；源码、编译或历史 Node 成绩不能代替本轮实际验收。CPU 推理对照不代表 DirectML、多 GPU 或长期性能已验收。
