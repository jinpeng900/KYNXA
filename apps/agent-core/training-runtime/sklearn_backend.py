"""Tree ensembles checkpoint at an estimator boundary. / 树模型按完整树保存。"""
import math
import os
from pathlib import Path
from training_worker import TrainingError, atomic_json
from builtin_backend import samples


def run(job):
    import joblib
    import numpy as np
    from sklearn.ensemble import RandomForestClassifier, RandomForestRegressor, GradientBoostingClassifier, GradientBoostingRegressor
    from sklearn.metrics import accuracy_score, log_loss, mean_absolute_error, mean_squared_error
    classification = job.algorithm.endswith("classifier")
    x, y, labels, groups, means, scales = samples(job, classification)
    x = np.asarray(x);y = np.asarray(y)
    p = job.parameters;estimators = int(p.get("estimators", 100))
    if estimators < 1:raise TrainingError("estimators必须为正整数。")
    maximum = p.get("maxDepth")
    if maximum is not None and (not isinstance(maximum, int) or maximum < 1):raise TrainingError("maxDepth应为正整数或留空。")
    forest = job.algorithm.startswith("random_forest")
    factory = (RandomForestClassifier if classification else RandomForestRegressor) if forest else (GradientBoostingClassifier if classification else GradientBoostingRegressor)
    args = {"n_estimators": 1, "warm_start": True, "random_state": int(p.get("seed", 42)), "max_depth": maximum}
    if forest:args["n_jobs"] = int(p.get("cpuThreads", 2))
    else:args["learning_rate"] = float(p.get("learningRate", .1))
    model = factory(**args)
    checkpoint_root=job.directory/"checkpoint"
    restored = job.restore();step = 0
    if restored:
        checkpoint=Path(restored.get("estimatorCheckpoint") or "").resolve()
        if not checkpoint.is_relative_to(checkpoint_root.resolve()) or not checkpoint.is_file():raise TrainingError("缺少有效树模型断点文件。", "TRAINING_CHECKPOINT_MISSING")
        if restored.get("groups") != groups or restored.get("maxDepth") != maximum or restored.get("learningRate") != args.get("learning_rate"):
            raise TrainingError("数据划分或树训练参数与断点不一致。", "TRAINING_CHECKPOINT_MISMATCH")
        # The model is generated inside this private run directory, never an imported pickle.
        # 只恢复本作业生成的模型，不接收外部pickle作为训练输入。
        model = joblib.load(checkpoint);step = restored["step"]
    job.update("running", backend="sklearn", device="cpu", step=step, canResume=True, message="正在训练树模型")

    def save():
        checkpoint=checkpoint_root/f"estimator-{step}.joblib"
        temp = checkpoint.with_suffix(".tmp")
        joblib.dump(model, temp);os.replace(temp, checkpoint)
        job.checkpoint({"groups": groups, "maxDepth": maximum, "learningRate": args.get("learning_rate"),"estimatorCheckpoint":str(checkpoint)}, step, step)
        previous=sorted(checkpoint_root.glob("estimator-*.joblib"),key=lambda path:int(path.stem.split("-")[-1]))
        for old in previous[:-2]:old.unlink(missing_ok=True)

    def metrics(ids):
        if not ids:return {}
        pred = model.predict(x[ids]);result = {}
        if classification:
            result["accuracy"] = float(accuracy_score(y[ids], pred))
            if len(model.classes_) > 1:
                probability=np.zeros((len(ids),len(labels)))
                probability[:,model.classes_.astype(int)]=model.predict_proba(x[ids])
                result["loss"] = float(log_loss(y[ids], probability, labels=list(range(len(labels)))))
        else:
            result["loss"] = float(mean_squared_error(y[ids], pred));result["mae"] = float(mean_absolute_error(y[ids], pred))
        return result

    while step < estimators:
        action = job.control()
        if action:
            if step:save()
            job.update("paused" if action == "pause" else "stopped", canResume=step > 0,
                       message="已在完整树边界暂停" if action == "pause" else "训练已停止");return
        step += 1;model.set_params(n_estimators=step);model.fit(x[groups["train"]], y[groups["train"]])
        # A forest's training error need not improve monotonically; report observations.
        train = metrics(groups["train"]);validation = metrics(groups["validation"])
        job.metric(step=step, epoch=step, estimators=step, **train,
                   **{"validation" + k[0].upper() + k[1:]: v for k, v in validation.items()})
        save();job.update("running", step=step, epoch=step, progress=step / estimators)
    path = job.directory / "artifacts" / "model.joblib"
    joblib.dump({"model": model, "means": means, "scales": scales, "labels": labels, "algorithm": job.algorithm, "preprocessing":job.preprocessing}, path)
    job.artifact(path, "model", "树模型")
    report = job.directory / "artifacts" / "summary.json"
    atomic_json(report, {"algorithm": job.algorithm, "backend": "sklearn", "estimators": step,
                         "train": metrics(groups["train"]), "validation": metrics(groups["validation"]), "test": metrics(groups["test"]),
                         "featureImportances": model.feature_importances_.tolist()})
    job.artifact(report, "report", "训练评估结果")
    job.update("completed", step=step, epoch=step, progress=1., message="训练完成，模型与评估结果已保存")
