"""Private result trials; no provider registration, downloads, or training writes.
训练台私有试用，不注册聊天模型、不下载权重、不改训练状态。
"""
from __future__ import annotations

import argparse
import gc
import importlib.util
import json
import os
from pathlib import Path
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parent))
os.environ["HF_HUB_OFFLINE"] = "1"
os.environ["TRANSFORMERS_OFFLINE"] = "1"
os.environ["HF_HUB_DISABLE_TELEMETRY"] = "1"
os.environ["TOKENIZERS_PARALLELISM"] = "false"
from training_worker import Job, TrainingError, atomic_json, now, process_alive, read_json


def local_model(path):
    root = Path(path).resolve(strict=True)
    if not root.is_dir():
        raise TrainingError("模型目录不存在。", "TRAINING_MODEL_UNAVAILABLE")
    # Reject linked files and index traversal before a model loader sees them.
    # 加载前拒绝链接及分片索引越界。
    for current, directories, files in os.walk(root, followlinks=False):
        for name in directories + files:
            entry = Path(current) / name
            if entry.is_symlink() or not entry.resolve().is_relative_to(root):
                raise TrainingError("模型目录包含外部链接，请使用完整的本地权重。", "TRAINING_MODEL_FORBIDDEN")
        for name in files:
            if name.endswith(".index.json"):
                for shard in (read_json(Path(current) / name).get("weight_map") or {}).values():
                    if not (root / shard).resolve().is_relative_to(root):
                        raise TrainingError("模型分片路径超出授权目录。", "TRAINING_MODEL_FORBIDDEN")
    return root


def model_paths(job):
    if job.algorithm not in ["text.sft", "text.lora"]:
        raise TrainingError("当前产物不支持文本试聊。", "TRAINING_MODEL_UNSUPPORTED")
    base = local_model(job.config["modelPath"])
    artifact = local_model(job.directory / "artifacts" / ("adapter" if job.algorithm == "text.lora" else "model"))
    if not artifact.is_relative_to((job.directory / "artifacts").resolve()):
        raise TrainingError("训练产物路径无效。", "TRAINING_MODEL_FORBIDDEN")
    if job.algorithm == "text.lora":
        config = read_json(artifact / "adapter_config.json")
        declared = config.get("base_model_name_or_path")
        if declared and Path(declared).resolve() != base:
            raise TrainingError("适配器记录的底座与训练时授权的模型不一致。", "TRAINING_MODEL_FORBIDDEN")
    return base, artifact


def dependencies(job):
    needed = ["torch", "transformers"] + (["peft"] if job.algorithm == "text.lora" else [])
    missing = [name for name in needed if importlib.util.find_spec(name) is None]
    if missing:
        raise TrainingError("训练环境还缺少：" + "、".join(missing) + "。请在训练台准备环境。", "TRAINING_DEPENDENCY_MISSING")


def schema(job):
    if job.algorithm not in ["text.sft", "text.lora"]:
        from prediction import schema as table_schema
        return table_schema(job)
    model_paths(job)
    try:
        dependencies(job)
        available, reason = True, ""
    except TrainingError as error:
        available, reason = False, str(error)
    return {"schemaVersion": 1, "kind": "text", "available": available, "reason": reason,
            "algorithm": job.algorithm, "modes": ["trained", "base", "compare"]}


def prepare(job, request):
    dependencies(job)
    base, artifact = model_paths(job)
    import torch
    device = job.parameters.get("device", "auto")
    if device == "auto":
        device = "cuda" if torch.cuda.is_available() else "cpu"
    if device == "cuda" and not torch.cuda.is_available():
        raise TrainingError("训练环境当前没有可用CUDA，请改用CPU环境或准备CUDA运行环境。", "TRAINING_DEVICE_UNAVAILABLE")
    if device not in ["cuda", "cpu"]:
        raise TrainingError("试用设备配置无效。")
    config_path = (artifact if job.algorithm == "text.sft" and request["mode"] == "trained" else base) / "config.json"
    cfg = read_json(config_path)
    weights = max(sum(p.stat().st_size for p in root.rglob("*") if p.is_file() and p.suffix in [".safetensors", ".bin", ".pt"])
                  for root in [base, artifact])
    # Sequential comparison needs only one model resident; reserve conservative fp32 plus KV.
    # 比较时逐个加载，按FP32权重和KV预留，不同时常驻两份模型。
    tokens = min(len(json.dumps(request["messages"], ensure_ascii=False)) + request["maxNewTokens"],
                 int(cfg.get("max_position_embeddings", cfg.get("n_positions", 8192))))
    kv = 2 * max(tokens, 1) * int(cfg.get("hidden_size", cfg.get("n_embd", 2048))) * int(cfg.get("num_hidden_layers", cfg.get("n_layer", 24))) * 4
    model_memory = weights * 2 + kv + 256 * 1024**2
    return {"device": device, "gpuTotalBytes": torch.cuda.get_device_properties(0).total_memory if device == "cuda" else 0,
            "cpuThreads": min(max(int(job.parameters.get("cpuThreads", 2)), 1), 4),
            "memoryBytes": 512 * 1024**2 + (min(weights, 512 * 1024**2) if device == "cuda" else model_memory),
            "gpuMemoryBytes": model_memory if device == "cuda" else 0}


def update(directory, **values):
    record = read_json(directory / "state.json")
    record.update(values, updatedAt=now())
    atomic_json(directory / "state.json", record)


def run(job, directory, request):
    dependencies(job)
    base, artifact = model_paths(job)
    import torch
    from transformers import AutoModelForCausalLM, AutoTokenizer, StoppingCriteria, StoppingCriteriaList
    plan = read_json(directory / "plan.json")
    torch.set_num_threads(plan["cpuThreads"])
    torch.set_num_interop_threads(1)
    device = plan["device"]
    dtype = torch.bfloat16 if device == "cuda" and torch.cuda.is_bf16_supported() else torch.float32
    owner = os.environ.get("KYNXA_TRAINING_OWNER_PID")

    class OwnerAlive(StoppingCriteria):
        def __call__(self, input_ids, scores, **kwargs):
            return bool(owner and not process_alive(int(owner)))

    modes = ["base", "trained"] if request["mode"] == "compare" else [request["mode"]]
    results = []
    for mode in modes:
        if owner and not process_alive(int(owner)):
            update(directory, state="cancelled", message="网关已关闭，已有试用结果保留。")
            return
        update(directory, state="loading", activeModel=mode, message="正在加载原底座。" if mode == "base" else "正在加载训练产物。", results=results)
        model_root = artifact if mode == "trained" and job.algorithm == "text.sft" else base
        tokenizer_root = artifact if mode == "trained" and (artifact / "tokenizer_config.json").is_file() else base
        tokenizer = AutoTokenizer.from_pretrained(tokenizer_root, local_files_only=True, trust_remote_code=False)
        model = AutoModelForCausalLM.from_pretrained(model_root, local_files_only=True, trust_remote_code=False, torch_dtype=dtype)
        if mode == "trained" and job.algorithm == "text.lora":
            from peft import PeftModel
            model = PeftModel.from_pretrained(model, artifact, local_files_only=True, is_trainable=False)
        model.to(device).eval()
        if tokenizer.chat_template:
            prompt = tokenizer.apply_chat_template(request["messages"], tokenize=False, add_generation_prompt=True)
        else:
            prompt = "\n".join(("用户" if m["role"] == "user" else "助手") + ": " + m["content"] for m in request["messages"]) + "\n助手:"
        inputs = tokenizer(prompt, return_tensors="pt", add_special_tokens=not bool(tokenizer.chat_template))
        input_tokens = int(inputs["input_ids"].shape[-1])
        context = getattr(model.config, "max_position_embeddings", getattr(model.config, "n_positions", None))
        if isinstance(context, int) and input_tokens + request["maxNewTokens"] > context:
            raise TrainingError(f"本次输入{input_tokens} token，加回复上限超过模型{context} token窗口，请缩短试聊内容或降低回复长度。", "TRAINING_TRIAL_CONTEXT_FULL")
        inputs = {key: value.to(device) for key, value in inputs.items()}
        update(directory, state="generating", message="原底座正在回复。" if mode == "base" else "训练产物正在回复。", inputTokens=input_tokens)
        started = time.monotonic()
        with torch.inference_mode():
            output = model.generate(**inputs, max_new_tokens=request["maxNewTokens"], do_sample=False,
                                    pad_token_id=tokenizer.pad_token_id if tokenizer.pad_token_id is not None else tokenizer.eos_token_id,
                                    stopping_criteria=StoppingCriteriaList([OwnerAlive()]))
        generated = output[0, input_tokens:]
        results.append({"mode": mode, "reply": tokenizer.decode(generated, skip_special_tokens=True),
                        "inputTokens": input_tokens, "outputTokens": len(generated), "elapsedMs": round((time.monotonic() - started) * 1000)})
        update(directory, results=results)
        del model, tokenizer, inputs, output, generated
        gc.collect()
        if device == "cuda":
            torch.cuda.empty_cache()
    update(directory, state="completed", message="试用完成。", results=results)


def main():
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8", errors="strict")
        sys.stderr.reconfigure(encoding="utf-8", errors="replace")
    parser = argparse.ArgumentParser()
    parser.add_argument("action", choices=["schema", "prepare", "run"])
    parser.add_argument("--job-dir", required=True, type=Path)
    parser.add_argument("--trial-dir", type=Path)
    args = parser.parse_args()
    try:
        job = Job(args.job_dir)
        if read_json(args.job_dir / "state.json").get("state") != "completed":
            raise TrainingError("请先选择已完成训练的模型。")
        if args.action == "schema":
            value = schema(job)
        else:
            request = read_json(args.trial_dir / "request.json")
            if args.action == "prepare":
                value = prepare(job, request)
            else:
                run(job, args.trial_dir, request)
                value = {"status": "completed"}
        print(json.dumps(value, ensure_ascii=False, allow_nan=False))
    except Exception as error:
        code = getattr(error, "code", "TRAINING_DEPENDENCY_MISSING" if isinstance(error, ImportError) else "TRAINING_TRIAL_FAILED")
        message = str(error)[:2000]
        if args.action == "run" and args.trial_dir:
            update(args.trial_dir, state="failed", message=message, error={"code": code, "message": message})
        print(json.dumps({"error": {"code": code, "message": message}}, ensure_ascii=False))
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
