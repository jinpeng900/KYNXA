# 本地中英文嵌入

`EmbeddingService` 为请求提供真正的本地语义向量，不创建第二份正式聊天或记忆存储。Orchestration 持有生命周期并将此服务注入 Data 的检索流程；Data 不反向导入 Models。

## 默认模型和运行边界

- 模型：`Xenova/multilingual-e5-small`，固定 revision `761b726dd34fb83930e26aab4e9ac3899aa1fa78`，ONNX q8，384 维。基础 `intfloat/multilingual-e5-small` 模型卡声明 MIT，原始模型卡、转换说明及许可原文随包保留。
- `embedding-profile.mjs` 是权重、分词、版本、哈希及下载来源的唯一清单。模型资产约 136 MB，位于忽略的 `artifacts/runtime/embedding/builtin-multilingual` 构建缓存；构建目标仅按校验生成的资产白名单复制到安装目录的 `runtime/embedding/builtin-multilingual`，缓存中的其他文件不随包提供。
- 默认 CPU 推理，每实例一个按需 worker，最多两条计算线程，不需要 Python、Docker、GPU 或远端 API。原生 ONNX Runtime 和 SQLite 向量扩展由锁定的 npm 依赖随包提供。
- 应用运行阶段禁止远端模型、CDN/WASM 回退及所有 worker 内 `fetch`。默认模型目录只读；没有权重、校验失败或 native 初始化失败时明确报告错误，由检索流程保留关键词能力，不伪造向量或自动上传文本。
- 原生 `sqlite-vec@0.1.9` npm 分发支持 Windows x64；Windows arm64 的完整向量链路需要另行编译与发布验收，不声称已支持。
- Windows x64 同时包含 ONNX 所需的四个 app-local VC 核心运行库。`Build/vc-runtime.json` 固定原始 Microsoft 签名文件版本与摘要；`prepare-vc-runtime.ps1` 只从正式版 Visual Studio Redist 或已经校验的构建缓存取得文件，不复制系统目录 DLL。原始 Microsoft Runtime 许可随包提供。构建机首次需要有许可的对应 Redist；安装用户不需要另下载或安装 VC。

## 接口

```js
const embedding = new EmbeddingService();
const query = await embedding.embedQuery(text, { signal });
// { vector, profileId, modelVersion, dimensions }
// 查询向量及固定模型版本。
const documents = await embedding.embedDocuments(texts, { signal });
// { vectors, profileId, modelVersion, dimensions }
// 文档向量数组及固定模型版本。
const status = embedding.status();
await embedding.close();
```

查询按模型卡添加 `query: `，文档添加 `passage: `；均使用平均池化和归一化。每批最多 64 条，worker 每组处理四条。每条最多 512 个 token，包含前缀及特殊 token；超长返回 `EMBEDDING_INPUT_TOO_LONG` 和实际计数，不静默截断。调用方应拆分来源或保留词法检索。取消结果不会污染后续请求；关闭等待原生调用结束并释放 ONNX 会话，不强行终止 worker。

缓存和索引必须同时记录 `profileId` 与 `modelVersion`，不能因为 builtin ID 未变就复用不同权重的向量。相似度分数不是回答正确率；阈值需要按中英文、代码和资料评测集验证。

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
