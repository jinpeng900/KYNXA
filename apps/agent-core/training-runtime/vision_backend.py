"""Configurable small image classifier with optimizer/data-cursor recovery."""
import math
import os
import random
from pathlib import Path
from training_worker import TrainingError, atomic_json, split_samples
from builtin_backend import tuple_state


def run(job):
    import numpy as np
    import torch
    from PIL import Image, ImageOps
    p = job.parameters;source = job.dataset["samples"]
    torch.set_num_threads(int(p.get("cpuThreads", 2)))
    labels = list(dict.fromkeys(row.get("label") for row in source))
    if any(v is None for v in labels) or len(labels) < 2:raise TrainingError("图像分类需要至少两种有效标签。", "TRAINING_DATA_INVALID")
    for row in source:
        if not Path(row.get("imagePath", "")).is_file():raise TrainingError("图片原件不存在。", "TRAINING_DATA_INVALID")
    device = p.get("device", "auto")
    if device == "auto":device = "cuda" if torch.cuda.is_available() else "cpu"
    if device not in ["cpu", "cuda"] or device == "cuda" and not torch.cuda.is_available():
        raise TrainingError("所选训练设备不可用。", "TRAINING_DEVICE_UNAVAILABLE")
    seed = int(p.get("seed", 42));rng = random.Random(seed);torch.manual_seed(seed)
    channels = p.get("hiddenLayers", [16, 32]);size = int(p.get("imageSize", 64))
    if not channels or any(not isinstance(c, int) or c < 1 for c in channels) or size < 8:raise TrainingError("卷积通道及图片尺寸配置无效。")
    blocks = [];last = 3
    for width in channels:
        blocks.extend([torch.nn.Conv2d(last, width, 3, padding=1), torch.nn.ReLU(), torch.nn.MaxPool2d(2, ceil_mode=True)]);last = width
    blocks.extend([torch.nn.AdaptiveAvgPool2d((1, 1)), torch.nn.Flatten(), torch.nn.Linear(last, len(labels))])
    model = torch.nn.Sequential(*blocks).to(device)
    train_layers=p.get("layersToTransform")
    if train_layers is not None:
        train_layers=set(train_layers)
        if not train_layers or any(not isinstance(v,int) or isinstance(v,bool) or not 0<=v<=len(channels) for v in train_layers):raise TrainingError("选定训练层不在CNN结构内。")
        for index, layer in enumerate(model):
            block=index//3 if index < 3*len(channels) else len(channels)
            for parameter in layer.parameters():parameter.requires_grad_(block in train_layers)
    if job.config.get("modelPath"):
        raise TrainingError("当前vision.classifier为可配置CNN；尚不支持导入任意图像模型结构。请选择内置CNN。", "TRAINING_MODEL_UNSUPPORTED")
    optimizer_name = p.get("optimizer", "adam");lr = float(p.get("learningRate", .001))
    if optimizer_name not in ["adam", "sgd"]:raise TrainingError("图像训练支持adam或sgd。")
    optimizer = (torch.optim.Adam if optimizer_name == "adam" else torch.optim.SGD)([v for v in model.parameters() if v.requires_grad], lr=lr, weight_decay=float(p.get("weightDecay", 0)))
    criterion = torch.nn.CrossEntropyLoss()
    groups = split_samples(source, p)
    if not groups["train"]:raise TrainingError("训练集为空。")
    step = 0;epoch = 0;cursor = 0;order = []
    restored = job.restore();checkpoint_root = job.directory / "checkpoint"
    signature = {"channels": channels, "size": size, "groups": groups, "labels": labels, "optimizer": optimizer_name, "trainLayers":sorted(train_layers) if train_layers is not None else None}
    if restored:
        ckpt=Path(restored.get("tensorCheckpoint") or "").resolve()
        if not ckpt.is_relative_to(checkpoint_root.resolve()):raise TrainingError("图像断点位置无效。", "TRAINING_CHECKPOINT_MISMATCH")
        if restored.get("signature") != signature or not ckpt.is_file():raise TrainingError("图像网络或数据与断点不一致。", "TRAINING_CHECKPOINT_MISMATCH")
        data = torch.load(ckpt, map_location=device, weights_only=True)
        model.load_state_dict(data["model"]);optimizer.load_state_dict(data["optimizer"]);torch.set_rng_state(data["torchRng"].cpu())
        if device == "cuda" and data.get("cudaRng") is not None:torch.cuda.set_rng_state_all(data["cudaRng"])
        rng.setstate(tuple_state(restored["rng"]));step = restored["step"];epoch = restored["epoch"];cursor = restored["cursor"];order = restored["order"]
        for group in optimizer.param_groups:group["lr"] = lr
    epochs = int(p.get("epochs", 20));batch_size = int(p.get("batchSize", 16));max_steps = int(p.get("maxSteps") or math.ceil(len(groups["train"]) / batch_size) * epochs)
    job.update("running", backend="torch", device=device, canResume=True, trainableParameters=sum(v.numel() for v in model.parameters() if v.requires_grad), message="正在训练图像分类网络")

    def tensor(ids):
        images = []
        for i in ids:
            with Image.open(source[i]["imagePath"]) as image:
                image = ImageOps.exif_transpose(image).convert("RGB").resize((size, size))
                images.append(np.asarray(image, dtype=np.float32).transpose(2, 0, 1) / 255.)
        return torch.from_numpy(np.stack(images)).to(device), torch.tensor([labels.index(source[i]["label"]) for i in ids], dtype=torch.long, device=device)

    def save():
        ckpt=checkpoint_root/f"vision-{step}.pt"
        temp = ckpt.with_suffix(".tmp")
        torch.save({"model": model.state_dict(), "optimizer": optimizer.state_dict(), "torchRng": torch.get_rng_state(),
                    "cudaRng": torch.cuda.get_rng_state_all() if device == "cuda" else None}, temp);os.replace(temp, ckpt)
        job.checkpoint({"signature": signature, "cursor": cursor, "order": order, "rng": rng.getstate(),"tensorCheckpoint":str(ckpt)}, step, epoch)
        previous=sorted(checkpoint_root.glob("vision-*.pt"),key=lambda path:int(path.stem.split("-")[-1]))
        for old in previous[:-2]:old.unlink(missing_ok=True)

    def evaluate(ids):
        if not ids:return {}
        total = 0.;correct = 0;model.eval()
        with torch.no_grad():
            for off in range(0, len(ids), batch_size):
                image, target = tensor(ids[off:off+batch_size]);pred = model(image)
                total += criterion(pred, target).item() * len(target);correct += (pred.argmax(-1) == target).sum().item()
        return {"loss": total / len(ids), "accuracy": correct / len(ids)}

    while epoch < epochs and step < max_steps:
        action = job.control()
        if action:
            save();job.update("paused" if action == "pause" else "stopped", step=step, epoch=epoch, message="已保存图像训练断点");return
        if not order:order = groups["train"][:];rng.shuffle(order);cursor = 0
        ids = order[cursor:cursor+batch_size];image, target = tensor(ids);model.train();optimizer.zero_grad()
        loss = criterion(model(image), target)
        if not torch.isfinite(loss):raise TrainingError("损失出现非有限数值。", "TRAINING_NUMERICAL_FAILURE")
        loss.backward();clip = float(p.get("gradientClipNorm", 1))
        if clip:torch.nn.utils.clip_grad_norm_(model.parameters(), clip)
        optimizer.step();step += 1;cursor += len(ids)
        epoch_done = cursor >= len(order)
        if epoch_done:epoch += 1;order = [];cursor = 0
        if step % int(p.get("loggingSteps", 10)) == 0 or epoch_done or step == max_steps:
            validation = evaluate(groups["validation"])
            job.metric(step=step, epoch=epoch, loss=loss.item(), learningRate=lr, **{"validation" + k[0].upper() + k[1:]:v for k,v in validation.items()})
            job.update("running", step=step, epoch=epoch, progress=min(1., max(epoch / epochs, step / max_steps)))
        if step % int(p.get("checkpointEvery", 50)) == 0:save()
    save();path = job.directory / "artifacts" / "model.pt";torch.save(model.state_dict(), path)
    config = job.directory / "artifacts" / "model-config.json";atomic_json(config, {"architecture": "kynxa-cnn", "hiddenLayers": channels, "imageSize": size, "labels": labels})
    report = job.directory / "artifacts" / "summary.json";atomic_json(report, {"train": evaluate(groups["train"]), "validation": evaluate(groups["validation"]), "test": evaluate(groups["test"])})
    for file, kind, title in [(path,"model","图像分类模型"),(config,"configuration","模型结构"),(report,"report","训练评估结果")]:job.artifact(file,kind,title)
    job.update("completed", step=step, epoch=epoch, progress=1., message="图像训练完成，模型与评估结果已保存")
