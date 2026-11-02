"""Check only the selected isolated environment; do not install or change settings."""
import importlib
import json
import sys

if hasattr(sys.stdout, "reconfigure"):
    sys.stdout.reconfigure(encoding="utf-8", errors="strict")
    sys.stderr.reconfigure(encoding="utf-8", errors="replace")

backend = sys.argv[1]
required = {
    "builtin": [], "sklearn": ["numpy", "sklearn", "PIL"],
    "torch": ["torch", "torchvision", "PIL", "numpy"],
    "transformers": ["torch", "transformers", "datasets", "accelerate"],
    "peft": ["torch", "transformers", "datasets", "accelerate", "peft"],
    "trl": ["torch", "transformers", "datasets", "accelerate", "peft", "trl", "PIL"],
}[backend]
modules, missing = {}, []
for name in required:
    try:
        module = importlib.import_module(name)
        modules[name] = getattr(module, "__version__", "available")
    except Exception as error:
        missing.append({"name": name, "reason": str(error)[:240]})
result = {"available": not missing, "pythonVersion": sys.version.split()[0], "pythonPath": sys.executable,
          "modules": modules, "missing": missing, "cudaAvailable": False, "gpus": []}
if "torch" in modules:
    import torch
    result["cudaAvailable"] = torch.cuda.is_available()
    if result["cudaAvailable"]:
        for index in range(torch.cuda.device_count()):
            device = torch.cuda.get_device_properties(index)
            result["gpus"].append({"name": device.name, "memoryBytes": device.total_memory})
        # A working allocation distinguishes driver discovery from usable CUDA.
        # 实际分配区分显卡被识别和 CUDA 真正可用。
        try:
            value = torch.ones(1, device="cuda") + 1
            result["cudaAvailable"] = value.item() == 2
        except Exception as error:
            result["cudaAvailable"] = False
            result["cudaError"] = str(error)[:240]
print(json.dumps(result, ensure_ascii=False), flush=True)
