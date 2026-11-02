"""Short real CPU checks; all run data is confined to a temporary directory."""
from pathlib import Path
import json
import os
import subprocess
import sys
import tempfile
import time

WORKER=Path(__file__).resolve().parents[2]/"apps/agent-core/training-runtime/training_worker.py"


def write(path, value):path.write_text(json.dumps(value),encoding="utf-8")
def read(path):return json.loads(path.read_text(encoding="utf-8"))
def command(directory, mode="run", env=None):
    return subprocess.run([sys.executable,"-I","-u",str(WORKER),mode,"--job-dir",str(directory)],capture_output=True,text=True,encoding="utf-8",timeout=90,env=env)
def make(root,name,algorithm,parameters,rows):
    path=root/name;path.mkdir();write(path/"config.json",{"schemaVersion":1,"id":name,"algorithm":algorithm,"parameters":parameters});write(path/"dataset.json",{"schemaVersion":1,"kind":"tabular","samples":rows});return path
def successful(path):
    process=command(path);assert process.returncode==0,process.stderr
    state=read(path/"state.json");assert state["state"]=="completed",state
    return state


def main():
    results=[]
    with tempfile.TemporaryDirectory(prefix="kynxa-training-verify-") as temp:
        root=Path(temp)
        regression=[{"features":[i/10],"label":2*i/10+3,"split":"train" if i<32 else "validation"} for i in range(40)]
        path=make(root,"regression","linear.regression",{"epochs":100,"batchSize":8,"learningRate":.05,"loggingSteps":10},regression)
        state=successful(path);summary=read(path/"artifacts"/"summary.json")
        assert summary["train"]["loss"]<.005,summary
        metrics=[json.loads(v) for v in (path/"metrics.jsonl").read_text(encoding="utf-8").splitlines()]
        assert metrics[-1]["loss"]<metrics[0]["loss"]*.1
        assert metrics[-1]["rssBytes"]>0
        results.append("real-regression-convergence-and-memory")
        prepare=command(path,"prepare");assert prepare.returncode==0,prepare.stderr
        prepared=json.loads(prepare.stdout);assert prepared["device"]=="cpu" and prepared["modelStructure"]["layerSizes"]==[1,1]
        before=(path/"state.json").read_bytes();write(path/"prediction.json",{"samples":[{"features":[2]}]})
        prediction=subprocess.run([sys.executable,"-I",str(WORKER),"predict","--job-dir",str(path),"--input-file",str(path/"prediction.json")],capture_output=True,text=True,encoding="utf-8",timeout=20)
        assert prediction.returncode==0,prediction.stderr
        assert abs(json.loads(prediction.stdout)["predictions"][0]["prediction"]-7)<.2
        assert (path/"state.json").read_bytes()==before
        results.append("frozen-preprocessing-prediction-preserves-training-state")
        xor=[{"features":[a,b],"label":int(bool(a)!=bool(b)),"split":"train"} for _ in range(4) for a,b in [(0,0),(0,1),(1,0),(1,1)]]
        path=make(root,"xor","mlp.classifier",{"epochs":100,"hiddenLayers":[8],"activation":"tanh","batchSize":4,"learningRate":.03,"validationSplit":0},xor)
        successful(path);summary=read(path/"artifacts"/"summary.json");assert summary["train"]["accuracy"]>.99,summary
        results.append("real-nonlinear-classifier")
        rows=[{"features":[i/40,(i%7)/7],"label":i%2,"split":"train"} for i in range(80)]
        parameters={"epochs":10,"batchSize":4,"hiddenLayers":[16,8],"learningRate":.005,"loggingSteps":1,"checkpointEvery":2,"seed":9}
        baseline=make(root,"baseline","mlp.classifier",parameters,rows);successful(baseline)
        path=make(root,"resume","mlp.classifier",parameters,rows)
        process=subprocess.Popen([sys.executable,"-I","-u",str(WORKER),"run","--job-dir",str(path)],stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True,encoding="utf-8")
        end=time.monotonic()+20
        while time.monotonic()<end:
            try:
                if read(path/"state.json").get("step",0)>=2:break
            except (OSError,json.JSONDecodeError):pass
            time.sleep(.01)
        write(path/"control.json",{"action":"pause"});out,err=process.communicate(timeout=20)
        assert process.returncode==0,err
        paused=read(path/"state.json");assert paused["state"]=="paused" and 0<paused["step"]<200,paused
        (path/"control.json").unlink();successful(path)
        expected=read(baseline/"artifacts"/"model.json");actual=read(path/"artifacts"/"model.json")
        assert expected==actual,"weights changed across exact resume"
        results.append("mid-training-pause-resume-exact-optimizer-and-rng")
        path=make(root,"owner-exit","mlp.classifier",parameters,rows)
        env=os.environ.copy();env["KYNXA_TRAINING_OWNER_PID"]="2147483647"
        p=command(path,env=env);assert p.returncode==0,p.stderr
        assert read(path/"state.json")["state"]=="paused"
        results.append("owner-exit-saves-checkpoint")
        path=make(root,"unknown","linear.regression",{"hiddenLayers":[5]},regression)
        p=command(path,"prepare");assert p.returncode!=0
        assert "hiddenLayers" in json.loads(p.stdout)["error"]["message"]
        results.append("irrelevant-parameter-rejected")
        leakage=[{"features":[0],"label":0,"groupId":"same","split":"train"},{"features":[1],"label":1,"groupId":"same","split":"validation"}]
        path=make(root,"leakage","logistic.regression",{},leakage)
        p=command(path,"prepare");assert p.returncode!=0 and json.loads(p.stdout)["error"]["code"]=="TRAINING_DATA_LEAKAGE"
        results.append("source-group-leakage-rejected")
        mixed=[{"features":[None,"red"],"label":1,"split":"train"},{"features":[2,"blue"],"label":0,"split":"train"},{"features":[4,"red"],"label":1,"split":"train"},{"features":[1000,"new"],"label":0,"split":"validation"}]
        path=make(root,"preprocess","logistic.regression",{"epochs":2},mixed);successful(path)
        preprocessing=read(path/"artifacts"/"model.json")["preprocessing"]
        assert preprocessing["columns"][0]["fill"]==3 and "new" not in preprocessing["columns"][1]["categories"]
        results.append("preprocessing-fit-only-on-train")
        path=make(root,"invalid-layer","mlp.classifier",{"hiddenLayers":[4],"layersToTransform":[-1]},xor)
        assert command(path,"prepare").returncode!=0
        config=read(path/"config.json");config["parameters"]["layersToTransform"]=[2];write(path/"config.json",config)
        assert command(path,"prepare").returncode!=0
        config["parameters"]["layersToTransform"]=[1];write(path/"config.json",config)
        prepared=command(path,"prepare");assert prepared.returncode==0,prepared.stderr
        structure=json.loads(prepared.stdout)["modelStructure"];assert structure["totalParameterCount"]==22 and structure["trainableParameterCount"]==10,structure
        results.append("prepare-validates-real-layer-range-and-trainable-count")
        print(json.dumps({"passed":len(results),"checks":results,"realTraining":True,"temporaryArtifactsRemoved":True}))


if __name__=="__main__":main()
