"""Use frozen training preprocessing for local model predictions."""
import math
from training_worker import TrainingError, read_json
from builtin_backend import forward


def schema(job):
    """Describe the frozen encoder; display labels never change feature order."""
    path = job.directory / "artifacts" / "model.json"
    if path.is_file():
        model = read_json(path)
    elif (job.directory / "artifacts" / "model.joblib").is_file():
        import joblib
        model = joblib.load(job.directory / "artifacts" / "model.joblib")
    else:
        raise TrainingError("这个训练产物暂不支持表格预测。", "TRAINING_MODEL_UNSUPPORTED")
    columns = (model.get("preprocessing") or {}).get("columns")
    if not columns:
        raise TrainingError("训练产物缺少特征配置。", "TRAINING_MODEL_UNSUPPORTED")
    names = job.dataset.get("featureNames") or []
    example = next((row.get("features") for row in job.dataset.get("samples", [])
                    if isinstance(row.get("features"), list) and len(row["features"]) == len(columns)), [])
    fields = []
    for index, column in enumerate(columns):
        fields.append({"index": index, "name": str(names[index]) if index < len(names) else f"特征 {index + 1}",
                       "kind": column["kind"], "nullable": True,
                       "example": example[index] if index < len(example) else None,
                       "options": column.get("categories", [])[:200]})
    return {"schemaVersion": 1, "kind": "tabular", "available": True,
            "algorithm": job.algorithm, "fields": fields, "maximumSamples": 1000}


def transform(rows, metadata):
    encoder=metadata.get("preprocessing")
    if not encoder:raise TrainingError("训练产物缺少完整预处理配置。", "TRAINING_MODEL_UNSUPPORTED")
    columns=encoder["columns"];means=encoder["means"];scales=encoder["scales"]
    output=[]
    for row in rows:
        values=row.get("features")
        if not isinstance(values,list) or len(values)!=len(columns):raise TrainingError("预测特征数量与训练配置不同。", "TRAINING_DATA_INVALID")
        encoded=[]
        for value,column in zip(values,columns):
            if column["kind"]=="numeric":
                if value is not None and (isinstance(value,bool) or not isinstance(value,(int,float)) or not math.isfinite(value)):raise TrainingError("数值特征类型错误。", "TRAINING_DATA_INVALID")
                encoded.append(float(value) if value is not None else column["fill"])
            else:
                if value is not None and not isinstance(value,(int,float,str)):raise TrainingError("类别特征类型错误。", "TRAINING_DATA_INVALID")
                value=str(value) if value is not None else None;categories=column["categories"]
                index=categories.index(value) if value in categories else column["unknownIndex"]
                encoded.extend(float(index==i) for i in range(len(categories)+1))
        output.append([(v-means[i])/scales[i] for i,v in enumerate(encoded)])
    return output


def predict(job, request):
    rows=request.get("samples")
    if not isinstance(rows,list) or not rows:raise TrainingError("预测需要非空samples数组。", "TRAINING_DATA_INVALID")
    path=job.directory/"artifacts"/"model.json";predictions=[]
    if path.is_file():
        model=read_json(path);x=transform(rows,model)
        for values in x:
            if model["algorithm"]=="kmeans":
                distances=[sum((a-b)**2 for a,b in zip(values,c)) for c in model["centers"]]
                predictions.append({"prediction":min(range(len(distances)),key=distances.__getitem__),"squaredDistances":distances})
            else:
                labels=model.get("labels") or [];pred=forward(values,model["layers"],model["activation"],bool(labels))[-1]
                if labels:
                    index=max(range(len(pred)),key=pred.__getitem__);predictions.append({"prediction":labels[index],"probabilities":[{"label":label,"probability":score} for label,score in zip(labels,pred)]})
                else:predictions.append({"prediction":pred[0]})
    elif (job.directory/"artifacts"/"model.joblib").is_file():
        import joblib
        # Only the worker's own private result file is loaded.
        model=joblib.load(job.directory/"artifacts"/"model.joblib");x=transform(rows,model);estimator=model["model"]
        result=estimator.predict(x);probability=estimator.predict_proba(x) if hasattr(estimator,"predict_proba") else None
        for index,value in enumerate(result):
            labels=model.get("labels") or [];row={"prediction":labels[int(value)] if labels else float(value)}
            if probability is not None:row["probabilities"]=[{"label":labels[int(label)],"probability":float(score)} for label,score in zip(estimator.classes_,probability[index])]
            predictions.append(row)
    else:raise TrainingError("当前产物没有接入独立预测，请使用对应模型运行器。", "TRAINING_MODEL_UNSUPPORTED")
    return {"schemaVersion":1,"id":job.id,"algorithm":job.algorithm,"predictions":predictions,"count":len(predictions)}
