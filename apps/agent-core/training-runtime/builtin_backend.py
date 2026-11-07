"""Small CPU networks and k-means with real optimizer checkpoints."""
from __future__ import annotations
import math
import random
from pathlib import Path
from training_worker import TrainingError, atomic_json, split_samples


def tuple_state(value):
    return tuple(tuple_state(x) for x in value) if isinstance(value, list) else value


def samples(job, classification=True, unsupervised=False):
    source = job.dataset["samples"]
    dim = len(source[0].get("features", []))
    if not dim:
        raise TrainingError("表格训练需要非空features数组。", "TRAINING_DATA_INVALID")
    labels = []
    for row in source:
        values = row.get("features")
        if not isinstance(values, list) or len(values) != dim:
            raise TrainingError("所有样本的特征维度必须一致。", "TRAINING_DATA_INVALID")
        for value in values:
            if value is not None and (isinstance(value, bool) or not isinstance(value, (str, int, float)) or isinstance(value, float) and not math.isfinite(value)):
                raise TrainingError("特征只接受数值、文本类别或空值。", "TRAINING_DATA_INVALID")
        if not unsupervised:
            target = row.get("label")
            if classification:
                if not isinstance(target, (str, int, float)) or isinstance(target, bool) or isinstance(target, float) and not math.isfinite(target):
                    raise TrainingError("分类样本需要有效标签。", "TRAINING_DATA_INVALID")
                if target not in labels:labels.append(target)
            elif not isinstance(target, (int, float)) or isinstance(target, bool) or not math.isfinite(target):
                raise TrainingError("回归目标必须为有限数值。", "TRAINING_DATA_INVALID")
    if classification and not unsupervised and len(labels) < 2:
        raise TrainingError("分类训练至少需要两种标签。", "TRAINING_DATA_INVALID")
    groups = split_samples(source, job.parameters)
    if len(groups["train"]) < 2:
        raise TrainingError("划分后训练集至少需要两个样本。", "TRAINING_DATA_INVALID")
    if classification and not unsupervised and len({str(source[i]["label"]) for i in groups["train"]}) < 2:
        raise TrainingError("分类训练集本身至少需要两种标签，请调整数据划分。", "TRAINING_DATA_INVALID")
    train = groups["train"];encoders = [];columns = [[] for _ in source]
    for j in range(dim):
        values = [source[i]["features"][j] for i in train]
        if any(isinstance(v, str) for v in values):
            categories = list(dict.fromkeys(str(v) if v is not None else None for v in values))
            encoders.append({"kind": "categorical", "categories": categories, "unknownIndex": len(categories)})
            for i, row in enumerate(source):
                value = str(row["features"][j]) if row["features"][j] is not None else None
                index = categories.index(value) if value in categories else len(categories)
                columns[i].extend(float(index == k) for k in range(len(categories) + 1))
        else:
            numeric = [float(v) for v in values if v is not None]
            fill = sum(numeric) / len(numeric) if numeric else 0.
            encoders.append({"kind": "numeric", "fill": fill})
            for i, row in enumerate(source):
                value = row["features"][j]
                if isinstance(value, str):
                    raise TrainingError("评估集的数值特征出现文本，请检查列类型。", "TRAINING_DATA_INVALID")
                columns[i].append(float(value) if value is not None else fill)
    width = len(columns[0])
    means = [sum(columns[i][j] for i in train) / len(train) for j in range(width)]
    scales = [max(math.sqrt(sum((columns[i][j] - means[j]) ** 2 for i in train) / len(train)), 1e-12) for j in range(width)]
    x = [[(v - means[j]) / scales[j] for j, v in enumerate(row)] for row in columns]
    y = [labels.index(row["label"]) if classification else float(row["label"]) for row in source] if not unsupervised else []
    job.preprocessing = {"columns": encoders, "means": means, "scales": scales, "fitSplit": "train"}
    return x, y, labels, groups, means, scales


def activate(v, name):
    return max(0., v) if name == "relu" else math.tanh(v)


def forward(x, layers, activation, classification):
    values = [x]
    for index, layer in enumerate(layers):
        v = [sum(w * a for w, a in zip(weights, values[-1])) + bias for weights, bias in zip(layer["weights"], layer["biases"])]
        if index != len(layers) - 1:
            v = [activate(z, activation) for z in v]
        elif classification:
            maximum = max(v);e = [math.exp(z - maximum) for z in v];total = sum(e);v = [z / total for z in e]
        values.append(v)
    return values


def evaluate(ids, x, y, layers, activation, classification):
    if not ids:
        return {}
    loss = 0;correct = 0;absolute = 0
    for i in ids:
        pred = forward(x[i], layers, activation, classification)[-1]
        if classification:
            loss -= math.log(max(pred[y[i]], 1e-15));correct += max(range(len(pred)), key=pred.__getitem__) == y[i]
        else:
            loss += (pred[0] - y[i]) ** 2;absolute += abs(pred[0] - y[i])
    return {"loss": loss / len(ids), **({"accuracy": correct / len(ids)} if classification else {"mae": absolute / len(ids)})}


def run(job):
    if job.algorithm == "kmeans":
        return run_kmeans(job)
    p = job.parameters;classification = job.algorithm.endswith("classifier") or job.algorithm == "logistic.regression"
    x, y, labels, groups, means, scales = samples(job, classification)
    hidden = p.get("hiddenLayers", [32, 16]) if job.algorithm.startswith("mlp") else []
    if not isinstance(hidden, list) or any(not isinstance(z, int) or isinstance(z, bool) or z < 1 for z in hidden):
        raise TrainingError("hiddenLayers应为正整数数组。")
    activation = p.get("activation", "relu");optimizer = p.get("optimizer", "adam")
    if activation not in ["relu", "tanh"] or optimizer not in ["adam", "sgd"]:
        raise TrainingError("轻量后端支持relu/tanh及adam/sgd。")
    shape = [len(x[0]), *hidden, len(labels) if classification else 1]
    train_layers = p.get("layersToTransform", list(range(len(shape) - 1)))
    if not isinstance(train_layers, list) or not train_layers or any(not isinstance(z, int) or isinstance(z, bool) or not 0 <= z < len(shape)-1 for z in train_layers):
        raise TrainingError("训练层索引必须对应实际网络层，且至少选择一层。")
    rng = random.Random(int(p.get("seed", 42)))
    layers = [{"weights": [[rng.gauss(0, math.sqrt(2 / (a + b))) for _ in range(a)] for _ in range(b)], "biases": [0.] * b} for a, b in zip(shape, shape[1:])]
    moments = [[[[0., 0.] for _ in row] for row in layer["weights"]] + [[[0., 0.] for _ in layer["biases"]]] for layer in layers]
    step = 0;epoch = 0;cursor = 0;order = [];best = None;stale = 0
    restored = job.restore()
    if restored:
        if restored.get("shape") != shape or restored.get("activation") != activation or restored.get("optimizer") != optimizer or restored.get("groups") != groups or restored.get("trainLayers") != train_layers:
            raise TrainingError("网络结构或优化器与断点不同，请创建新实验。", "TRAINING_CHECKPOINT_MISMATCH")
        layers = restored["layers"];moments = restored["moments"];step = restored["step"];epoch = restored["epoch"]
        cursor = restored["cursor"];order = restored["order"];rng.setstate(tuple_state(restored["rng"]))
        best = restored.get("best");stale = restored.get("stale", 0)
    epochs = int(p.get("epochs", 20));batch_size = int(p.get("batchSize", 16));lr = float(p.get("learningRate", .001))
    max_steps = int(p.get("maxSteps") or math.ceil(len(groups["train"]) / batch_size) * epochs)
    decay = float(p.get("weightDecay", 0));clip = float(p.get("gradientClipNorm", 1));logging = int(p.get("loggingSteps", 10))
    if decay < 0 or not math.isfinite(decay) or clip < 0 or not math.isfinite(clip):
        raise TrainingError("权重衰减和梯度裁剪必须为有限非负数。")
    job.update("running", step=step, epoch=epoch, backend="builtin", device="cpu", canResume=True,
               trainableParameters=sum(sum(len(row) for row in layer["weights"]) + len(layer["biases"]) for i,layer in enumerate(layers) if i in train_layers),
               message="正在训练" if not restored else "已从真实优化器断点续训")

    def checkpoint():
        job.checkpoint({"shape": shape, "activation": activation, "optimizer": optimizer, "layers": layers,
                        "moments": moments, "cursor": cursor, "order": order, "rng": rng.getstate(),
                        "means": means, "scales": scales, "labels": labels, "best": best, "stale": stale,
                        "groups": groups, "trainLayers": train_layers}, step, epoch)

    def controlled():
        action = job.control()
        if action:
            checkpoint();job.update("paused" if action == "pause" else "stopped", step=step, epoch=epoch, message="训练已暂停并保存断点" if action == "pause" else "训练已停止并保存断点")
            return True
        return False

    if controlled():
        return
    reason = "epochs"
    while epoch < epochs and step < max_steps:
        if not order:
            order = groups["train"][:]
            if p.get("shuffle", True):rng.shuffle(order)
            cursor = 0
        batch = order[cursor:cursor + batch_size]
        gradients = [{"weights": [[0.] * len(row) for row in layer["weights"]], "biases": [0.] * len(layer["biases"])} for layer in layers]
        for i in batch:
            values = forward(x[i], layers, activation, classification)
            delta = [z - (j == y[i]) for j, z in enumerate(values[-1])] if classification else [2 * (values[-1][0] - y[i])]
            for index in range(len(layers) - 1, -1, -1):
                for a, dz in enumerate(delta):
                    gradients[index]["biases"][a] += dz / len(batch)
                    for b, v in enumerate(values[index]):gradients[index]["weights"][a][b] += dz * v / len(batch)
                if index:
                    delta = [sum(layers[index]["weights"][a][b] * dz for a, dz in enumerate(delta)) * ((1. if v > 0 else 0.) if activation == "relu" else 1 - v * v) for b, v in enumerate(values[index])]
        norm = math.sqrt(sum(g * g for layer in gradients for row in [*layer["weights"], layer["biases"]] for g in row))
        factor = min(1., clip / max(norm, 1e-12)) if clip else 1.
        step += 1
        schedule = p.get("learningRateSchedule", "constant");warmup = int(p.get("warmupSteps", 0))
        if schedule not in ["constant", "linear", "cosine"] or warmup < 0:raise TrainingError("学习率调度配置无效。")
        phase = min(1., max(0., (step - warmup) / max(1, max_steps - warmup)))
        multiplier = min(1., step / warmup) if warmup else 1.
        if step > warmup:
            if schedule == "linear":multiplier *= 1 - phase
            elif schedule == "cosine":multiplier *= .5 * (1 + math.cos(math.pi * phase))
        current_lr = lr * multiplier
        for index, (layer, grad) in enumerate(zip(layers, gradients)):
            if index not in train_layers:continue
            weight_rows = [*layer["weights"], layer["biases"]];grad_rows = [*grad["weights"], grad["biases"]]
            for a, (weights, gs) in enumerate(zip(weight_rows, grad_rows)):
                for b, g in enumerate(gs):
                    g = g * factor + (decay * weights[b] if a < len(layer["weights"]) else 0)
                    if optimizer == "adam":
                        m, v = moments[index][a][b];m = .9 * m + .1 * g;v = .999 * v + .001 * g * g;moments[index][a][b] = [m, v]
                        g = (m / (1 - .9 ** step)) / (math.sqrt(v / (1 - .999 ** step)) + 1e-8)
                    weights[b] -= current_lr * g
        cursor += len(batch)
        epoch_done = cursor >= len(order)
        if epoch_done:
            epoch += 1;order = [];cursor = 0
        if step % logging == 0 or epoch_done or step == max_steps:
            train = evaluate(groups["train"], x, y, layers, activation, classification)
            validation = evaluate(groups["validation"], x, y, layers, activation, classification)
            job.metric(step=step, epoch=epoch, learningRate=current_lr, **train, **({"validationLoss": validation["loss"], **{"validation" + k[0].upper() + k[1:]: v for k, v in validation.items() if k != "loss"}} if validation else {}))
            job.update("running", step=step, epoch=epoch, progress=min(1., max(epoch / epochs, step / max_steps)))
            if epoch_done and validation:
                if best is None or validation["loss"] < best:
                    best = validation["loss"];stale = 0
                    atomic_json(job.directory / "artifacts" / "best-model.json", {"schemaVersion": 1, "algorithm": job.algorithm, "layers": layers, "activation": activation, "means": means, "scales": scales, "labels": labels, "step": step, "preprocessing":job.preprocessing})
                else:stale += 1
                if int(p.get("earlyStoppingPatience", 0)) > 0 and stale >= int(p["earlyStoppingPatience"]):
                    reason = "early-stopping";break
        if step % int(p.get("checkpointEvery", 50)) == 0:checkpoint()
        if controlled():return
    if step >= max_steps and epoch < epochs:reason = "max-steps"
    checkpoint()
    path = job.directory / "artifacts" / "model.json"
    atomic_json(path, {"schemaVersion": 1, "backend": "builtin", "algorithm": job.algorithm, "layers": layers,
                       "activation": activation, "means": means, "scales": scales, "labels": labels, "shape": shape,
                       "preprocessing": job.preprocessing})
    job.artifact(path, "model", "训练模型")
    best_path = job.directory / "artifacts" / "best-model.json"
    if best_path.exists():job.artifact(best_path, "model", "验证集最佳模型")
    summary = {"algorithm": job.algorithm, "backend": "builtin", "steps": step, "epochs": epoch, "completionReason": reason,
               "train": evaluate(groups["train"], x, y, layers, activation, classification),
               "validation": evaluate(groups["validation"], x, y, layers, activation, classification),
               "test": evaluate(groups["test"], x, y, layers, activation, classification)}
    summary_path = job.directory / "artifacts" / "summary.json";atomic_json(summary_path, summary);job.artifact(summary_path, "report", "训练评估结果")
    job.update("completed", step=step, epoch=epoch, progress=1., completionReason=reason, message="训练完成，模型与评估结果已保存")


def run_kmeans(job):
    p = job.parameters;x, _, _, groups, means, scales = samples(job, False, True)
    train = groups["train"];k = int(p.get("clusters", 3));epochs = int(p.get("epochs", 20))
    if k < 1 or k > len(train):raise TrainingError("聚类数必须在1到训练样本数之间。")
    rng = random.Random(int(p.get("seed", 42)));centers = [x[i][:] for i in rng.sample(train, k)];step = 0
    restored = job.restore()
    if restored:
        centers = restored["centers"];step = restored["step"]
        if len(centers) != k or restored.get("groups") != groups:raise TrainingError("聚类数或数据划分与断点不一致。", "TRAINING_CHECKPOINT_MISMATCH")
    job.update("running", backend="builtin", device="cpu", canResume=True, message="正在聚类")
    def save():job.checkpoint({"centers": centers, "means": means, "scales": scales, "groups":groups}, step, step)
    while step < epochs:
        action = job.control()
        if action:
            save();job.update("paused" if action == "pause" else "stopped", step=step, epoch=step, message="聚类已保存迭代断点");return
        batches = [[] for _ in centers]
        for i in train:
            nearest = min(range(k), key=lambda c: sum((a - b) ** 2 for a, b in zip(x[i], centers[c])))
            batches[nearest].append(i)
        new = [[sum(x[i][j] for i in ids) / len(ids) for j in range(len(x[0]))] if ids else centers[c][:] for c, ids in enumerate(batches)]
        shift = sum((a - b) ** 2 for old, values in zip(centers, new) for a, b in zip(old, values));centers = new;step += 1
        inertia = sum(min(sum((a - b) ** 2 for a, b in zip(x[i], c)) for c in centers) for i in train)
        job.metric(step=step, epoch=step, inertia=inertia)
        save();job.update("running", step=step, epoch=step, progress=step / epochs)
        if shift <= float(p.get("tolerance", 1e-6)):break
    path = job.directory / "artifacts" / "model.json";atomic_json(path, {"schemaVersion": 1, "algorithm": "kmeans", "centers": centers, "means": means, "scales": scales,"preprocessing":job.preprocessing})
    job.artifact(path, "model", "聚类模型");job.update("completed", step=step, epoch=step, progress=1., message="聚类完成，模型已保存")
