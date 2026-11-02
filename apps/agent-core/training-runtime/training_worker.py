"""Versioned, file-based training worker. / 独立训练进程，不依赖聊天连接。"""
from __future__ import annotations

import argparse
import contextlib
import hashlib
import importlib.util
import json
import math
import os
import random
from pathlib import Path
import sys
import time
import traceback
from datetime import datetime, timezone
sys.path.insert(0, str(Path(__file__).resolve().parent))

SCHEMA = 1
ALIASES = {"mlp": "mlp.classifier", "linear_regression": "linear.regression",
           "logistic_regression": "logistic.regression", "text_lora": "text.lora"}
BUILTIN = ["mlp.classifier", "mlp.regressor", "linear.regression", "logistic.regression", "kmeans"]
SKLEARN = ["random_forest.classifier", "random_forest.regressor", "gradient_boosting.classifier", "gradient_boosting.regressor"]
HF = ["text.sft", "text.lora", "vlm.sft"]
RESOURCE_PARAMETERS = {"cpuThreads", "memoryMiB", "gpuMemoryMiB"}
NET_PARAMETERS = {"epochs", "maxSteps", "batchSize", "learningRate", "hiddenLayers", "activation", "optimizer", "weightDecay", "gradientClipNorm", "seed", "validationSplit", "testSplit", "loggingSteps", "checkpointEvery", "earlyStoppingPatience", "shuffle", "layersToTransform", "learningRateSchedule", "warmupSteps"}


def supported_parameters(algorithm):
    if algorithm in BUILTIN:
        keys = {"clusters", "tolerance", "epochs", "seed", "validationSplit", "testSplit"} if algorithm == "kmeans" else NET_PARAMETERS.copy()
        if algorithm in ["linear.regression", "logistic.regression"]:keys -= {"hiddenLayers", "activation", "layersToTransform"}
    elif algorithm in SKLEARN:
        keys = {"estimators", "maxDepth", "seed", "validationSplit", "testSplit"}
        if algorithm.startswith("gradient_boosting"):keys |= {"learningRate"}
    elif algorithm == "vision.classifier":
        keys = NET_PARAMETERS - {"activation", "earlyStoppingPatience", "shuffle", "learningRateSchedule", "warmupSteps"}
        keys |= {"imageSize", "device"}
    else:
        keys = {"epochs", "maxSteps", "batchSize", "learningRate", "optimizer", "weightDecay", "gradientClipNorm", "seed", "validationSplit", "testSplit", "loggingSteps", "checkpointEvery", "maxLength", "loraRank", "loraAlpha", "loraDropout", "layersToTransform", "targetModules", "gradientAccumulationSteps", "precision", "device", "gradientCheckpointing", "learningRateSchedule", "warmupSteps", "quantization"}
        if algorithm == "text.sft":keys -= {"loraRank", "loraAlpha", "loraDropout", "targetModules", "quantization"}
        if algorithm == "vlm.sft":keys -= {"maxLength"}
    return sorted(keys | RESOURCE_PARAMETERS)


class TrainingError(Exception):
    def __init__(self, message, code="TRAINING_INVALID_CONFIGURATION"):
        super().__init__(message)
        self.code = code


def atomic_json(path: Path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(path.name + ".tmp")
    with temporary.open("w", encoding="utf-8", newline="\n") as f:
        json.dump(value, f, ensure_ascii=False, allow_nan=False, separators=(",", ":"))
        f.flush()
        os.fsync(f.fileno())
    for attempt in range(8):
        try:
            os.replace(temporary, path)
            break
        except PermissionError:
            # Windows readers may briefly omit FILE_SHARE_DELETE. Retry the rename;
            # never truncate the published snapshot while the UI is reading it.
            if attempt==7:raise
            time.sleep(min(.005 * 2 ** attempt,.2))


def read_json(path: Path):
    with path.open(encoding="utf-8-sig") as f:
        return json.load(f)


def now():
    return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")


def split_samples(source, parameters):
    keys = ["train", "validation", "test"]
    if any(row.get("split") for row in source):
        groups = {key: [i for i, row in enumerate(source) if row.get("split", "train") == key] for key in keys}
        if sum(map(len, groups.values())) != len(source):raise TrainingError("split只能是train、validation或test。", "TRAINING_DATA_INVALID")
        owners = {}
        for key, ids in groups.items():
            for i in ids:
                group = str(source[i].get("groupId", "row:" + str(i)))
                if group in owners and owners[group] != key:raise TrainingError("同一来源组跨训练与评估集合。", "TRAINING_DATA_LEAKAGE")
                owners[group] = key
        return groups
    sources = {}
    for i, row in enumerate(source):sources.setdefault(str(row.get("groupId", "row:" + str(i))), []).append(i)
    ids = list(sources);random.Random(int(parameters.get("seed", 42))).shuffle(ids)
    val = float(parameters.get("validationSplit", .2));test = float(parameters.get("testSplit", 0))
    if not 0 <= val < 1 or not 0 <= test < 1 or val + test >= 1:raise TrainingError("验证集与测试集比例不正确。")
    nv = int(len(ids) * val);nt = int(len(ids) * test)
    return {"validation": [i for g in ids[:nv] for i in sources[g]], "test": [i for g in ids[nv:nv+nt] for i in sources[g]], "train": [i for g in ids[nv+nt:] for i in sources[g]]}


def parameter_schema():
    return {
        "epochs": {"type": "integer", "minimum": 1, "default": 20},
        "maxSteps": {"type": "integer", "minimum": 1},
        "batchSize": {"type": "integer", "minimum": 1, "default": 16},
        "learningRate": {"type": "number", "exclusiveMinimum": 0, "default": 0.001},
        "hiddenLayers": {"type": "array", "items": {"type": "integer", "minimum": 1}, "default": [32, 16]},
        "activation": {"enum": ["relu", "tanh"], "default": "relu"},
        "optimizer": {"enum": ["adam", "sgd"], "default": "adam"},
        "weightDecay": {"type": "number", "minimum": 0, "default": 0},
        "gradientClipNorm": {"type": "number", "minimum": 0, "default": 1},
        "validationSplit": {"type": "number", "minimum": 0, "exclusiveMaximum": 1, "default": 0.2},
        "testSplit": {"type": "number", "minimum": 0, "exclusiveMaximum": 1, "default": 0},
        "seed": {"type": "integer", "default": 42},
        "loggingSteps": {"type": "integer", "minimum": 1, "default": 10},
        "checkpointEvery": {"type": "integer", "minimum": 1, "default": 50},
        "earlyStoppingPatience": {"type": "integer", "minimum": 0, "default": 0},
        "layersToTransform": {"type": "array", "minItems":1, "items": {"type": "integer", "minimum": 0}},
        "learningRateSchedule": {"enum": ["constant", "linear", "cosine"], "default": "constant"},
        "warmupSteps": {"type": "integer", "minimum": 0, "default": 0},
        "cpuThreads": {"type": "integer", "minimum": 1, "default": 2},
        "memoryMiB": {"type": "integer", "minimum": 1},
        "gpuMemoryMiB": {"type": "integer", "minimum": 0},
        "estimators": {"type": "integer", "minimum": 1, "default": 100},
        "maxDepth": {"type": "integer", "minimum": 1},
        "clusters": {"type": "integer", "minimum": 1, "default": 3},
        "tolerance": {"type": "number", "minimum": 0, "default": 1e-6},
        "imageSize": {"type": "integer", "minimum": 8, "default": 64},
        "device": {"enum": ["auto", "cpu", "cuda"], "default": "auto"},
        "precision": {"enum": ["auto", "fp32", "fp16", "bf16"], "default": "auto"},
        "maxLength": {"type": "integer", "minimum": 2, "default": 1024},
        "loraRank": {"type": "integer", "minimum": 1, "default": 8},
        "loraAlpha": {"type": "integer", "minimum": 1, "default": 16},
        "loraDropout": {"type": "number", "minimum": 0, "exclusiveMaximum": 1, "default": .05},
        "gradientAccumulationSteps": {"type": "integer", "minimum": 1, "default": 1},
        "quantization": {"enum": ["none", "4bit"], "default": "none"},
        "targetModules": {"type": "array", "items": {"type": "string"}},
        "gradientCheckpointing": {"type": "boolean", "default": False},
    }


def algorithm_defaults(algorithm):
    """Reported defaults match execution; UI realtime logging is an explicit override.
    公布值与执行器一致；界面按步记录指标属于显式设置。
    """
    allowed = set(supported_parameters(algorithm))
    defaults = {key: value["default"] for key, value in parameter_schema().items()
                if key in allowed and "default" in value}
    if algorithm in HF:
        defaults.update(epochs=3, batchSize=1, learningRate=2e-5)
        if algorithm == "vlm.sft":
            # Presence enables LoRA; omitting these preserves the full-training default.
            # 显式字段会启用LoRA，省略才保留原本的全量训练行为。
            for key in ("loraRank", "loraAlpha", "loraDropout"):
                defaults.pop(key, None)
    elif algorithm == "vision.classifier":
        defaults["hiddenLayers"] = [16, 32]
    elif algorithm.startswith("gradient_boosting"):
        defaults["learningRate"] = .1
    if "shuffle" in allowed:
        defaults["shuffle"] = True
    return defaults


def has_module(name):
    try:
        return importlib.util.find_spec(name) is not None
    except (ImportError, ValueError):
        return False


def capabilities():
    modules = {m: has_module(m) for m in ["numpy", "sklearn", "torch", "transformers", "peft", "trl", "datasets", "PIL", "safetensors", "bitsandbytes", "accelerate"]}
    backends = [{"id": "builtin", "name": "轻量CPU训练", "available": True,
                 "algorithms": BUILTIN, "resume": "exact", "pauseExact": True, "devices": ["cpu"]}]
    for identifier, required, algorithms, resume in [
        ("sklearn", ["sklearn", "numpy"], SKLEARN, "estimator-boundary"),
        ("torch", ["torch", "PIL", "numpy"], ["vision.classifier"], "optimizer-step"),
        ("transformers", ["torch", "transformers", "datasets", "accelerate"], ["text.sft"], "checkpoint"),
        ("peft", ["torch", "transformers", "datasets", "accelerate", "peft"], ["text.lora"], "checkpoint"),
        ("trl", ["torch", "transformers", "datasets", "accelerate", "trl", "PIL"], ["vlm.sft"], "checkpoint"),
    ]:
        missing = [m for m in required if not modules[m]]
        backends.append({"id": identifier, "available": not missing, "algorithms": algorithms,
                         "missingDependencies": missing, "reason": "缺少训练依赖：" + ", ".join(missing) if missing else None,
                         "resume": resume, "pauseExact": True, "devices": ["cpu", "cuda"] if "torch" in required else ["cpu"]})
    return {"schemaVersion": SCHEMA, "python": sys.version.split()[0], "executable": sys.executable,
            "modules": modules, "backends": backends, "parameterSchema": parameter_schema(),
            "algorithmParameters": {a:supported_parameters(a) for a in BUILTIN + SKLEARN + HF + ["vision.classifier"]},
            "algorithmDefaults": {a:algorithm_defaults(a) for a in BUILTIN + SKLEARN + HF + ["vision.classifier"]},
            "predictionAlgorithms":BUILTIN+SKLEARN,
            "note": "available只代表运行依赖齐备，模型架构、权重、设备及资料仍需检查。"}


class Job:
    def __init__(self, directory: Path):
        self.directory = directory.resolve(strict=True)
        self.config = read_json(self.directory / "config.json")
        self.dataset = read_json(self.directory / "dataset.json")
        self.parameters = self.config.get("parameters") or {}
        self.algorithm = ALIASES.get(self.config.get("algorithm"), self.config.get("algorithm"))
        self.config["algorithm"] = self.algorithm
        self.id = self.config.get("id", self.directory.name)
        self.started = time.monotonic()
        self.previous_elapsed = 0
        self.state = {"schemaVersion": SCHEMA, "id": self.id, "state": "preparing", "status": "preparing",
                      "step": 0, "epoch": 0, "progress": 0, "canResume": False, "artifacts": []}
        for folder in ["artifacts", "checkpoint"]:
            (self.directory / folder).mkdir(exist_ok=True)
        # Frozen dataset identity prevents continuation against changed samples.
        # 数据集摘要绑定断点，不能换了资料还沿用旧优化器状态。
        self.dataset_hash = hashlib.sha256(json.dumps(self.dataset, sort_keys=True, ensure_ascii=False).encode()).hexdigest()

    def elapsed(self):
        return self.previous_elapsed + time.monotonic() - self.started

    def update(self, state=None, **values):
        if state:
            self.state["state"] = self.state["status"] = state
        self.state.update(values)
        self.state.update(updatedAt=now(), elapsedSeconds=round(self.elapsed(), 3))
        atomic_json(self.directory / "state.json", self.state)

    def metric(self, **values):
        metric = {"timestamp": now(), "elapsedSeconds": round(self.elapsed(), 3), **resource_usage(), **values}
        for key, value in list(metric.items()):
            if isinstance(value, float) and not math.isfinite(value):
                raise TrainingError("训练出现非有限数值，请调整学习率或检查输入。", "TRAINING_NUMERICAL_FAILURE")
        with (self.directory / "metrics.jsonl").open("a", encoding="utf-8", newline="\n") as f:
            f.write(json.dumps(metric, ensure_ascii=False, allow_nan=False, separators=(",", ":")) + "\n")
        self.state["latestMetrics"] = metric

    def control(self):
        try:
            c = read_json(self.directory / "control.json")
            if c.get("action") in ["pause", "stop"]:return c["action"]
        except (OSError, json.JSONDecodeError):
            pass
        owner = os.environ.get("KYNXA_TRAINING_OWNER_PID")
        if owner and not process_alive(int(owner)):return "pause"
        return None

    def artifact(self, path, kind, title):
        path = Path(path).resolve()
        if not path.is_relative_to(self.directory):
            raise TrainingError("训练产物超出本次作业目录。")
        item = {"path": str(path), "relativePath": str(path.relative_to(self.directory)).replace("\\", "/"), "kind": kind, "title": title}
        self.state["artifacts"] = [x for x in self.state["artifacts"] if x["path"] != item["path"]] + [item]
        return item

    def checkpoint(self, payload, step, epoch):
        path = self.directory / "checkpoint" / "training.json"
        atomic_json(path, {"schemaVersion": SCHEMA, "algorithm": self.algorithm, "datasetHash": self.dataset_hash,
                           "step": step, "epoch": epoch, "elapsedSeconds": self.elapsed(), **payload})
        self.state.update(checkpoint={"path": str(path), "step": step, "epoch": epoch}, canResume=True)

    def restore(self):
        path = self.directory / "checkpoint" / "training.json"
        if not path.exists():
            return None
        payload = read_json(path)
        if payload.get("datasetHash") != self.dataset_hash or payload.get("algorithm") != self.algorithm:
            raise TrainingError("训练配置或数据集与断点不一致，请创建新实验。", "TRAINING_CHECKPOINT_MISMATCH")
        self.previous_elapsed = float(payload.get("elapsedSeconds", 0))
        self.state.update(step=payload.get("step", 0), epoch=payload.get("epoch", 0), canResume=True)
        return payload

    def validate(self):
        if self.config.get("schemaVersion", 1) != SCHEMA or self.dataset.get("schemaVersion", 1) != SCHEMA:
            raise TrainingError("不支持此训练协议版本。")
        if self.algorithm not in BUILTIN + SKLEARN + HF + ["vision.classifier"]:
            raise TrainingError("未接入此训练算法。")
        unknown = set(self.parameters) - set(supported_parameters(self.algorithm))
        if unknown:raise TrainingError("此算法不支持这些参数：" + ", ".join(sorted(unknown)))
        for key, value in self.parameters.items():
            schema = parameter_schema().get(key, {})
            kind = schema.get("type")
            if kind in ["integer", "number"]:
                if isinstance(value, bool) or not isinstance(value, int if kind == "integer" else (int,float)) or not math.isfinite(value):raise TrainingError(key+"类型不正确。")
                if "minimum" in schema and value<schema["minimum"] or "exclusiveMinimum" in schema and value<=schema["exclusiveMinimum"] or "exclusiveMaximum" in schema and value>=schema["exclusiveMaximum"]:raise TrainingError(key+"超出此参数有效范围。")
            if kind == "array":
                if not isinstance(value,list):raise TrainingError(key+"应为数组。")
                if len(value)<schema.get("minItems",0):raise TrainingError(key+"至少需要选择一个实际模型层。")
                item=schema.get("items",{})
                for element in value:
                    if item.get("type")=="integer" and (not isinstance(element,int) or isinstance(element,bool)):raise TrainingError(key+"只接受整数索引或宽度。")
                    if item.get("type")=="string" and not isinstance(element,str):raise TrainingError(key+"只接受模块名称文本。")
                    if "minimum" in item and element<item["minimum"]:raise TrainingError(key+"含有无效索引或宽度。")
            if kind == "boolean" and not isinstance(value,bool):raise TrainingError(key+"应为布尔值。")
            if "enum" in schema and value not in schema["enum"]:raise TrainingError(key+"值不受支持。")
        samples = self.dataset.get("samples")
        if not isinstance(samples, list) or len(samples) < 2:
            raise TrainingError("训练数据至少需要两个有效样本。", "TRAINING_DATA_INVALID")
        for key in ["epochs", "batchSize", "loggingSteps", "checkpointEvery"]:
            value = self.parameters.get(key, parameter_schema()[key]["default"])
            if isinstance(value, bool) or not isinstance(value, int) or value < 1:
                raise TrainingError(key + "必须为正整数。")
        if not isinstance(self.parameters.get("learningRate", .001), (int, float)) or not 0 < self.parameters.get("learningRate", .001) < float("inf"):
            raise TrainingError("学习率必须为有限正数。")
        supported = next(b for b in capabilities()["backends"] if self.algorithm in b["algorithms"])
        if not supported["available"]:
            raise TrainingError(supported["reason"], "TRAINING_DEPENDENCY_MISSING")
        threads = self.parameters.get("cpuThreads", 2)
        if not isinstance(threads, int) or isinstance(threads, bool) or threads < 1:raise TrainingError("cpuThreads必须为正整数。")
        for key in ["OMP_NUM_THREADS", "MKL_NUM_THREADS", "OPENBLAS_NUM_THREADS"]:os.environ[key] = str(threads)
        device="cpu";cuda_available=False;gpu_total_bytes=0;structure={}
        if self.algorithm in BUILTIN+SKLEARN:
            if self.config.get("modelPath"):raise TrainingError("此算法使用本次配置创建模型；已有实验请使用原运行ID续训。", "TRAINING_MODEL_UNSUPPORTED")
            from builtin_backend import samples as prepare_samples
            classification = self.algorithm.endswith("classifier") or self.algorithm=="logistic.regression"
            prepared_x,_,labels,_,_,_=prepare_samples(self,classification,self.algorithm=="kmeans")
            if self.algorithm in BUILTIN and self.algorithm!="kmeans":
                hidden=self.parameters.get("hiddenLayers",[32,16]) if self.algorithm.startswith("mlp") else []
                if not isinstance(hidden,list) or any(not isinstance(x,int) or isinstance(x,bool) or x<1 for x in hidden):raise TrainingError("hiddenLayers应为正整数数组。")
                shape=[len(prepared_x[0]),*hidden,len(labels) if classification else 1]
                selected=self.parameters.get("layersToTransform",list(range(len(shape)-1)))
                if any(index>=len(shape)-1 for index in selected):raise TrainingError("训练层索引超出实际网络范围。")
                counts=[(a+1)*b for a,b in zip(shape,shape[1:])]
                structure={"layerSizes":shape,"parameterCount":sum(counts),"totalParameterCount":sum(counts),
                           "trainableParameterCount":sum(value for index,value in enumerate(counts) if index in selected),
                           "selectedLayers":selected,"layerValidation":"validated"}
        else:
            split_samples(samples,self.parameters)
            import torch
            torch.set_num_threads(threads)
            cuda_available=torch.cuda.is_available()
            device=self.parameters.get("device","auto")
            if device=="auto":device="cuda" if cuda_available else "cpu"
            if device=="cuda" and not cuda_available:raise TrainingError("所选CUDA训练设备不可用。", "TRAINING_DEVICE_UNAVAILABLE")
            if device=="cuda":gpu_total_bytes=torch.cuda.get_device_properties(torch.cuda.current_device()).total_memory
            if self.algorithm in HF:
                model = Path(self.config.get("modelPath") or "")
                if not self.config.get("modelPath") or not model.is_dir() or not (model/"config.json").is_file():raise TrainingError("训练底座应为包含config.json和权重的本地目录。", "TRAINING_MODEL_UNSUPPORTED")
                if not any(model.glob("*.safetensors")) and not any(model.glob("*.bin")):raise TrainingError("训练底座目录缺少safetensors或PyTorch权重文件。", "TRAINING_MODEL_UNSUPPORTED")
                metadata=read_json(model/"config.json")
                text_config=metadata.get("text_config") or metadata
                layer_count=text_config.get("num_hidden_layers",text_config.get("n_layer",text_config.get("num_layers")))
                selected=self.parameters.get("layersToTransform")
                if selected is not None and isinstance(layer_count,int) and any(index>=layer_count for index in selected):raise TrainingError("训练层索引超出模型配置中的层数。")
                structure={"modelType":metadata.get("model_type"),"hiddenLayers":layer_count,"hiddenSize":text_config.get("hidden_size"),"vocabularySize":text_config.get("vocab_size"),
                           "totalParameterCount":None,"trainableParameterCount":None,
                           "layerValidation":"validated-from-config" if isinstance(layer_count,int) else "deferred-until-model-load",
                           "parameterCountStatus":"模型加载后统计真实参数量"}
                if selected is not None and not isinstance(layer_count,int):structure["layerValidationReason"]="此结构未声明可识别的层数，加载实际模块后检查索引，不将未知结构标成已通过。"
                if self.parameters.get("quantization")=="4bit":
                    if device!="cuda":raise TrainingError("此4bit微调路径需要可用CUDA设备。", "TRAINING_DEVICE_UNAVAILABLE")
                    if not has_module("bitsandbytes"):raise TrainingError("4bit微调缺少bitsandbytes。", "TRAINING_DEPENDENCY_MISSING")
            else:
                if self.config.get("modelPath"):raise TrainingError("当前图像分类使用可配置CNN，尚不接受任意预训练权重。", "TRAINING_MODEL_UNSUPPORTED")
                channels=self.parameters.get("hiddenLayers",[16,32]);selected=self.parameters.get("layersToTransform",list(range(len(channels)+1)))
                if not channels or any(index>len(channels) for index in selected):raise TrainingError("训练层索引超出CNN结构范围。")
                label_count=len({str(row.get("label")) for row in samples});width=3;counts=[]
                for value in channels:counts.append((3*3*width+1)*value);width=value
                counts.append((width+1)*label_count)
                structure={"channels":channels,"parameterCount":sum(counts),"totalParameterCount":sum(counts),"trainableParameterCount":sum(value for index,value in enumerate(counts) if index in selected),"layerValidation":"validated","selectedLayers":selected}
        return {"schemaVersion": SCHEMA, "id": self.id, "algorithm": self.algorithm, "samples": len(samples), "backend": supported["id"], "valid": True, "device":device,"cudaAvailable":cuda_available,"gpuTotalBytes":gpu_total_bytes,"modelStructure":structure,"supportedParameters": supported_parameters(self.algorithm)}


def process_alive(pid):
    if os.name == "nt":
        import ctypes
        from ctypes import wintypes
        kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel.OpenProcess.restype = wintypes.HANDLE
        kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
        kernel.GetExitCodeProcess.argtypes = [wintypes.HANDLE, ctypes.POINTER(wintypes.DWORD)]
        kernel.CloseHandle.argtypes = [wintypes.HANDLE]
        handle = kernel.OpenProcess(0x1000, False, pid)
        if not handle:return False
        try:
            code = wintypes.DWORD()
            return bool(kernel.GetExitCodeProcess(handle, ctypes.byref(code))) and code.value == 259
        finally:kernel.CloseHandle(handle)
    try:os.kill(pid, 0);return True
    except PermissionError:return True
    except ProcessLookupError:return False


def resource_usage():
    result = {}
    if os.name == "nt":
        try:
            import ctypes
            from ctypes import wintypes
            class Counters(ctypes.Structure):
                _fields_ = [("cb", wintypes.DWORD), ("PageFaultCount", wintypes.DWORD)] + [(k, ctypes.c_size_t) for k in ["PeakWorkingSetSize", "WorkingSetSize", "QuotaPeakPagedPoolUsage", "QuotaPagedPoolUsage", "QuotaPeakNonPagedPoolUsage", "QuotaNonPagedPoolUsage", "PagefileUsage", "PeakPagefileUsage"]]
            c = Counters();c.cb = ctypes.sizeof(c)
            kernel=ctypes.WinDLL("kernel32");kernel.GetCurrentProcess.restype=wintypes.HANDLE
            psapi=ctypes.WinDLL("psapi");psapi.GetProcessMemoryInfo.argtypes=[wintypes.HANDLE, ctypes.POINTER(Counters), wintypes.DWORD]
            if psapi.GetProcessMemoryInfo(kernel.GetCurrentProcess(), ctypes.byref(c), c.cb):result["rssBytes"] = c.WorkingSetSize
        except (OSError, AttributeError):pass
    else:
        try:result["rssBytes"] = int(Path("/proc/self/statm").read_text().split()[1]) * os.sysconf("SC_PAGE_SIZE")
        except (OSError, IndexError, ValueError):pass
    torch = sys.modules.get("torch")
    if torch is not None and torch.cuda.is_available():result["gpuAllocatedBytes"] = torch.cuda.memory_allocated()
    return result


def main():
    # Isolated Python ignores PYTHONIOENCODING. / 隔离模式忽略编码环境变量。
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8", errors="strict")
        sys.stderr.reconfigure(encoding="utf-8", errors="replace")
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8", errors="strict")
        sys.stderr.reconfigure(encoding="utf-8", errors="replace")
    parser = argparse.ArgumentParser()
    parser.add_argument("command", choices=["capabilities", "prepare", "run", "predict"])
    parser.add_argument("--job-dir", type=Path)
    parser.add_argument("--input-file", type=Path)
    args = parser.parse_args()
    job = None
    try:
        if args.command == "capabilities":
            result = capabilities()
        else:
            if not args.job_dir:
                raise TrainingError("缺少训练目录。")
            job = Job(args.job_dir)
            if args.command=="predict":
                if not args.input_file or not args.input_file.resolve().is_relative_to(job.directory):raise TrainingError("预测输入必须位于本作业目录。")
                from prediction import predict
                result=predict(job,read_json(args.input_file))
            else:result = job.validate()
            if args.command == "run":
                if job.algorithm in BUILTIN:
                    from builtin_backend import run
                elif job.algorithm in SKLEARN:
                    from sklearn_backend import run
                elif job.algorithm == "vision.classifier":
                    from vision_backend import run
                else:
                    from transformer_backend import run
                with contextlib.redirect_stdout(sys.stderr):run(job)
                result = job.state
        print(json.dumps(result, ensure_ascii=False, allow_nan=False))
        return 0
    except Exception as exc:
        error = {"code": getattr(exc, "code", "TRAINING_EXECUTION_FAILED"), "message": str(exc)}
        if job and args.command=="run":
            try:job.update("failed", error=error, message=str(exc))
            except OSError:traceback.print_exc(file=sys.stderr)
        traceback.print_exc(file=sys.stderr)
        print(json.dumps({"schemaVersion": SCHEMA, "state": "failed", "error": error}, ensure_ascii=False))
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
