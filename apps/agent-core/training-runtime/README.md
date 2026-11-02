# 模型训练运行时

训练由独立 Python 进程执行。网关负责作业身份、权限、资源准入和进程生命周期；本目录不读取聊天私有目录、不控制界面，也不把一次聊天回复当成训练进程。

## 入口与能力

```text
python -I -u training_worker.py capabilities
python -I -u training_worker.py prepare --job-dir <作业目录>
python -I -u training_worker.py run --job-dir <作业目录>
python -I -u training_worker.py predict --job-dir <作业目录> --input-file <作业内输入JSON>
```

`capabilities` 和最终结果向标准输出返回一个 UTF-8 JSON 对象；框架日志转入标准错误。`available` 只表示该训练后端依赖齐备，不能代替对权重、架构、设备和数据的检查。

`prepare`返回真实设备探测和可取得的结构信息，不装载完整大模型权重。轻量与树模型的`predict`输入为`{"samples":[{"features":[1,"red",null]}]}`，严格复用训练产物的冻结预处理；输入保存在本作业目录，不修改训练快照。大模型和图像产物由相应推理运行器接入，不冒充普通ML预测。

| 后端 | 算法 | 依赖与边界 |
| --- | --- | --- |
| builtin | MLP分类/回归、线性回归、逻辑回归、k-means | Python标准库，适合小数据与CPU实验 |
| sklearn | 随机森林与梯度提升分类/回归 | `requirements-ml.txt`，按完整树保存 |
| torch | 可配置CNN图像分类 | `requirements-vision.txt`，保存优化器步进状态 |
| transformers / peft | 本地文本SFT、LoRA；兼容条件下4bit LoRA | `requirements-llm.txt`，4bit另需匹配硬件的bitsandbytes |
| trl | 支持架构的视觉语言模型SFT，可选LoRA | 本地视觉底座、processor、原图及图文配对样本 |

神经网络的层配置与已有大模型解冻范围分别处理。当前图像分类器是本地构建的CNN，不承诺直接导入任意预训练视觉网络。大模型路径要求本地可训练权重目录，不将现有GGUF推理文件自动视为训练底座。运行时不自动下载模型或执行远程模型代码。

## 冻结输入

`config.json`：

```json
{"schemaVersion":1,"id":"run-id","algorithm":"mlp.classifier","parameters":{"hiddenLayers":[32,16],"epochs":20,"batchSize":16,"learningRate":0.001,"optimizer":"adam"}}
```

其他顶层字段可包含`modelPath`。具体允许的参数读取`capabilities.algorithmParameters`；不适用于当前算法的参数会被拒绝，不会假装已经生效。`parameterSchema`提供字段类型、默认值及有效范围。

`dataset.json`：

```json
{"schemaVersion":1,"kind":"tabular","samples":[{"features":[1,"red",null],"label":"A","split":"train","groupId":"source-1"},{"features":[2,"blue",3],"label":"B","split":"train","groupId":"source-2"}]}
```

表格特征接受数值、文本类别与空值。均值填补、类别编码和标准化只拟合训练集，随产物保存。显式划分接受`train`、`validation`、`test`；同一`groupId`不能跨集。没有显式划分时按来源组和固定随机种子划分。

文本样本接受`text`、`messages`或`prompt`/`completion`。图片样本接受冻结原件路径`imagePath`及分类`label`，或图文`text`/`messages`配对。资料导入、PDF/Office解析及数据集原件冻结由网关的数据集模块负责，worker只读取已准备好的样本。

## 状态与恢复

- `state.json`：原子发布的正式快照，含`state/status`、步数、轮数、进度、耗时、产物、检查点及最近指标。
- `metrics.jsonl`：追加的真实指标，包含适用的loss、验证损失、准确率、MAE或聚类inertia，并记录可获得的RSS/显存占用。
- `control.json`：`{"action":"pause"}`或`{"action":"stop"}`，在可保存的算法边界执行。
- `checkpoint/`：权重、优化器、数据游标、随机状态及来源身份；不只保存模型权重就声称精确续训。
- `artifacts/`：独立保存模型、配置及评估结果，不覆盖原模型。

续训前由网关清除旧控制指令，继续使用原作业目录。数据集内容不一致会拒绝套用旧断点。`KYNXA_TRAINING_OWNER_PID`指定父网关身份；发现父进程已退出时在可保存边界暂停。网关仍应主动发暂停、等待落盘，并以系统进程组清退兜底。

CPU轻量后端的完整优化器和随机状态可精确恢复；CUDA计算仍受后端确定性及硬件实现影响。进程崩溃或断电只能恢复最后一次完整写入的检查点，不承诺尚未落盘的步数已保存。HF/视觉模型加载阶段尚无优化器检查点时不能伪造续训进度。

树模型的joblib和PyTorch检查点仅恢复本作业自己生成的私有文件，不支持加载不可信外来pickle。运行目录应保持网关已有的权限隔离。

## 短验证

`python -I ../../../tools/verification/training-worker.py`实际运行CPU训练，验证回归收敛、非线性分类、暂停续训权重一致、父进程退出保存、参数边界和数据集泄漏边界。用例使用临时目录，退出时清理产物。可选PyTorch、scikit-learn和大模型路径另按实际已安装环境验证，依赖缺失时明确报告，不能宣称已经跑过。
