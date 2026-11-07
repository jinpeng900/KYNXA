"""Local HF fine-tuning; normal tool returns never stand in for training."""
from pathlib import Path
import re
from training_worker import TrainingError, atomic_json, read_json, split_samples


def select_layers(model, requested):
    if requested is None:return
    if not isinstance(requested, list) or not requested or any(not isinstance(i, int) or isinstance(i, bool) or i < 0 for i in requested):
        raise TrainingError("layersToTransform应为非空的非负层索引数组。")
    pattern = re.compile(r"(?:layers|h|blocks)\.(\d+)\.")
    actual = set()
    for name, _ in model.named_parameters():
        match = pattern.search(name)
        if match:actual.add(int(match.group(1)))
    if not actual or not set(requested).issubset(actual):
        raise TrainingError("选定层不在此模型可识别的层结构中，请选择实际模型层。", "TRAINING_MODEL_UNSUPPORTED")
    for name, parameter in model.named_parameters():
        match = pattern.search(name);parameter.requires_grad_(bool(match and int(match.group(1)) in requested))


def run(job):
    import torch
    from datasets import Dataset
    from transformers import AutoModelForCausalLM, AutoTokenizer, Trainer, TrainingArguments, TrainerCallback
    p = job.parameters;torch.set_num_threads(int(p.get("cpuThreads", 2)))
    model_path = Path(job.config.get("modelPath") or "")
    if not job.config.get("modelPath") or not model_path.is_dir() or not (model_path / "config.json").is_file():
        raise TrainingError("请选择含config.json、权重及分词器的本地训练底座；GGUF推理文件需要先准备对应训练权重。", "TRAINING_MODEL_UNSUPPORTED")
    device = p.get("device", "auto")
    if device == "auto":device = "cuda" if torch.cuda.is_available() else "cpu"
    if device not in ["cpu", "cuda"] or device == "cuda" and not torch.cuda.is_available():raise TrainingError("所选训练设备不可用。", "TRAINING_DEVICE_UNAVAILABLE")
    precision = p.get("precision", "auto")
    if precision not in ["auto", "fp32", "fp16", "bf16"]:raise TrainingError("precision应为auto、fp32、fp16或bf16。")
    if precision == "auto":precision = "bf16" if device == "cuda" and torch.cuda.is_bf16_supported() else "fp32"
    if device == "cpu" and precision == "fp16":raise TrainingError("CPU路径不支持此FP16训练配置，请使用FP32或兼容的BF16。")
    dtype = {"fp32":torch.float32,"fp16":torch.float16,"bf16":torch.bfloat16}[precision]
    groups = split_samples(job.dataset["samples"], p)
    if not groups["train"]:raise TrainingError("训练集为空。", "TRAINING_DATA_INVALID")
    max_length = int(p.get("maxLength", 1024))
    if max_length < 2:raise TrainingError("文本序列长度至少为2。")
    quantization = p.get("quantization", "none")
    if quantization not in ["none", "4bit"]:raise TrainingError("quantization只支持none或4bit。")
    kwargs = {"local_files_only":True,"trust_remote_code":False,"torch_dtype":dtype}
    if quantization == "4bit":
        if device != "cuda" or job.algorithm == "text.sft":raise TrainingError("此4bit微调路径需要CUDA和LoRA。", "TRAINING_DEVICE_UNAVAILABLE")
        try:
            from transformers import BitsAndBytesConfig
            import bitsandbytes
        except ImportError:raise TrainingError("4bit微调需要bitsandbytes。", "TRAINING_DEPENDENCY_MISSING")
        kwargs.update(quantization_config=BitsAndBytesConfig(load_in_4bit=True,bnb_4bit_compute_dtype=dtype),device_map={"":torch.cuda.current_device()})
    vision = job.algorithm == "vlm.sft"
    processor = None
    if vision:
        from transformers import AutoProcessor, AutoModelForImageTextToText
        from trl import SFTTrainer, SFTConfig
        processor = AutoProcessor.from_pretrained(str(model_path),local_files_only=True,trust_remote_code=False)
        model = AutoModelForImageTextToText.from_pretrained(str(model_path),**kwargs)
        tokenizer = processor.tokenizer
    else:
        tokenizer = AutoTokenizer.from_pretrained(str(model_path),local_files_only=True,trust_remote_code=False)
        model = AutoModelForCausalLM.from_pretrained(str(model_path),**kwargs)
    if tokenizer.pad_token_id is None:
        if tokenizer.eos_token_id is None:raise TrainingError("训练分词器缺少pad和eos标记。", "TRAINING_MODEL_UNSUPPORTED")
        tokenizer.pad_token = tokenizer.eos_token
    if quantization == "none":model.to(device)
    lora = job.algorithm == "text.lora" or vision and (quantization=="4bit" or any(k in p for k in ["loraRank","loraAlpha","loraDropout","targetModules"]))
    if lora:
        from peft import LoraConfig, get_peft_model, prepare_model_for_kbit_training
        if quantization == "4bit":model = prepare_model_for_kbit_training(model)
        config = {"r":int(p.get("loraRank",8)),"lora_alpha":int(p.get("loraAlpha",16)),"lora_dropout":float(p.get("loraDropout",.05)),"task_type":"CAUSAL_LM","bias":"none"}
        if config["r"] < 1 or not 0 <= config["lora_dropout"] < 1:raise TrainingError("LoRA参数无效。")
        if p.get("targetModules"):config["target_modules"] = p["targetModules"]
        if p.get("layersToTransform") is not None:config["layers_to_transform"] = p["layersToTransform"]
        model = get_peft_model(model,LoraConfig(**config))
    else:select_layers(model,p.get("layersToTransform"))
    trainable = sum(parameter.numel() for parameter in model.parameters() if parameter.requires_grad)
    if not trainable:raise TrainingError("没有可训练参数，请检查解冻范围。")
    seed = int(p.get("seed",42));batch = int(p.get("batchSize",1));accumulation = int(p.get("gradientAccumulationSteps",1))
    if accumulation < 1:raise TrainingError("梯度累积步数必须为正整数。")
    optimizer = p.get("optimizer","adam")
    if optimizer not in ["adam","adamw","sgd"]:raise TrainingError("大模型路径支持adam/adamw/sgd。")
    schedule = p.get("learningRateSchedule","constant")
    if schedule not in ["constant","linear","cosine"]:raise TrainingError("学习率调度配置无效。")
    checkpoint_root = job.directory / "checkpoint" / "hf";checkpoint_root.mkdir(exist_ok=True)
    args = dict(output_dir=str(checkpoint_root),num_train_epochs=float(p.get("epochs",3)),max_steps=int(p.get("maxSteps") or -1),
                per_device_train_batch_size=batch,per_device_eval_batch_size=batch,gradient_accumulation_steps=accumulation,
                learning_rate=float(p.get("learningRate",2e-5)),weight_decay=float(p.get("weightDecay",0)),
                max_grad_norm=float(p.get("gradientClipNorm",1)),lr_scheduler_type=schedule,warmup_steps=int(p.get("warmupSteps",0)),
                logging_steps=int(p.get("loggingSteps",10)),save_steps=int(p.get("checkpointEvery",50)),save_strategy="steps",
                save_total_limit=2,seed=seed,data_seed=seed,report_to=[],dataloader_num_workers=0,
                fp16=precision=="fp16",bf16=precision=="bf16",use_cpu=device=="cpu",gradient_checkpointing=bool(p.get("gradientCheckpointing",False)),
                optim="sgd" if optimizer=="sgd" else "adamw_torch",disable_tqdm=True,eval_strategy="epoch" if groups["validation"] else "no")
    restored = job.restore();resume = None
    if restored:
        resume = restored.get("hfCheckpoint")
        if not resume or not Path(resume).resolve().is_relative_to(checkpoint_root.resolve()) or not Path(resume).is_dir() or restored.get("modelPath") != str(model_path.resolve()) or restored.get("groups") != groups:
            raise TrainingError("模型或数据与HF训练断点不一致。", "TRAINING_CHECKPOINT_MISMATCH")
    stop_action = None
    exact_optimizer=(torch.optim.Adam([v for v in model.parameters() if v.requires_grad],lr=float(p.get("learningRate",2e-5)),weight_decay=float(p.get("weightDecay",0))),None) if optimizer=="adam" else (None,None)
    class Progress(TrainerCallback):
        def on_step_end(self,args,state,control,**kwargs):
            nonlocal stop_action
            stop_action = job.control()
            if stop_action:control.should_save=True;control.should_training_stop=True
            job.update("running",step=state.global_step,epoch=float(state.epoch or 0),progress=min(1.,state.global_step/max(1,state.max_steps)))
            return control
        def on_log(self,args,state,control,logs=None,**kwargs):
            values = {}
            for source,destination in [("loss","loss"),("eval_loss","validationLoss"),("learning_rate","learningRate"),("grad_norm","gradientNorm")]:
                if logs and source in logs:values[destination]=float(logs[source])
            if values:job.metric(step=state.global_step,epoch=float(state.epoch or 0),**values)
        def on_save(self,args,state,control,**kwargs):
            path=checkpoint_root/f"checkpoint-{state.global_step}"
            job.checkpoint({"hfCheckpoint":str(path),"modelPath":str(model_path.resolve()),"groups":groups},state.global_step,float(state.epoch or 0))
    def text(row):
        if row.get("messages"):
            if not getattr(tokenizer,"chat_template",None):raise TrainingError("此分词器没有对话模板，请使用纯文本资料。", "TRAINING_MODEL_UNSUPPORTED")
            return tokenizer.apply_chat_template(row["messages"],tokenize=False,add_generation_prompt=False)
        if "prompt" in row:return str(row["prompt"])+str(row.get("completion",""))
        return str(row.get("text", ""))
    if vision:
        from datasets import Image as DatasetImage, Sequence
        def row_value(row):
            path=Path(row.get("imagePath") or "")
            if not path.is_file():raise TrainingError("视觉微调需要原始图片文件。", "TRAINING_DATA_INVALID")
            messages=row.get("messages")
            if not messages:
                answer=row.get("text") or row.get("completion") or row.get("label")
                if answer is None:raise TrainingError("图片缺少配对描述或答案。", "TRAINING_DATA_INVALID")
                messages=[{"role":"user","content":[{"type":"image"},{"type":"text","text":str(row.get("prompt") or "描述这张图片。")} ]},{"role":"assistant","content":[{"type":"text","text":str(answer)}]}]
            return {"messages":messages,"images":[str(path)]}
        def dataset(ids):return Dataset.from_list([row_value(job.dataset["samples"][i]) for i in ids]).cast_column("images",Sequence(DatasetImage()))
        args.update(max_length=None,remove_unused_columns=False)
        trainer=SFTTrainer(model=model,args=SFTConfig(**args),train_dataset=dataset(groups["train"]),eval_dataset=dataset(groups["validation"]) if groups["validation"] else None,processing_class=processor,callbacks=[Progress()],optimizers=exact_optimizer)
    else:
        def dataset(ids):
            rows=[]
            for i in ids:
                value=text(job.dataset["samples"][i])
                if not value.strip():continue
                encoded=tokenizer(value,truncation=True,max_length=max_length,add_special_tokens=True)
                if len(encoded["input_ids"])>=2:rows.append(encoded)
            if not rows:raise TrainingError("文本样本不足，无法构造训练序列。", "TRAINING_DATA_INVALID")
            return Dataset.from_list(rows)
        def collate(rows):
            encoded=tokenizer.pad(rows,padding=True,return_tensors="pt");labels=encoded["input_ids"].clone();labels[encoded["attention_mask"]==0]=-100;encoded["labels"]=labels;return encoded
        trainer=Trainer(model=model,args=TrainingArguments(**args),train_dataset=dataset(groups["train"]),eval_dataset=dataset(groups["validation"]) if groups["validation"] else None,data_collator=collate,callbacks=[Progress()],optimizers=exact_optimizer)
    job.update("running",backend="trl" if vision else "peft" if lora else "transformers",device=device,precision=precision,canResume=bool(resume),trainableParameters=trainable,message="正在微调本地模型")
    if job.control():
        job.update("paused" if job.control()=="pause" else "stopped",canResume=bool(resume),message="训练开始前已停止；尚无优化器步进");return
    trainer.train(resume_from_checkpoint=resume)
    if stop_action:
        job.update("paused" if stop_action=="pause" else "stopped",message="已在训练检查点保存后停止",canResume=True);return
    output=job.directory/"artifacts"/("adapter" if lora else "model");trainer.save_model(str(output));(processor or tokenizer).save_pretrained(str(output))
    job.artifact(output,"adapter" if lora else "model","LoRA适配器" if lora else "微调模型")
    report={"algorithm":job.algorithm,"baseModel":str(model_path.resolve()),"trainableParameters":trainable,"metrics":trainer.state.log_history,"steps":trainer.state.global_step}
    if groups["test"]:
        report["test"]=trainer.evaluate(dataset(groups["test"]),metric_key_prefix="test")
    path=job.directory/"artifacts"/"summary.json";atomic_json(path,report);job.artifact(path,"report","微调评估结果")
    job.update("completed",step=trainer.state.global_step,epoch=float(trainer.state.epoch or 0),progress=1.,message="微调完成，结果已独立保存")
