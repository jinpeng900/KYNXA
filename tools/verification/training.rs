use kynxa_agent_core_native::{gateway_contract::{GatewayConfig,GatewayError},tools::gateway::{self,GatewayToolContext,ToolRuntime}};
use serde_json::{Value,json};
use std::sync::Arc;
use std::{io::Write, path::{Path,PathBuf}};

struct Fixture {
    context: GatewayToolContext,
    config: GatewayConfig,
    _cancel:tokio::sync::watch::Sender<bool>,
    root:PathBuf,
}
impl Fixture {
    fn new()->Self {
        let root=std::env::temp_dir().join(format!("kynxa-native-tools-{}",uuid::Uuid::new_v4()));
        for directory in ["work","data","extensions","official/Skills"]{std::fs::create_dir_all(root.join(directory)).unwrap();}
        let(cancel,cancellation)=tokio::sync::watch::channel(false);
        let context=GatewayToolContext{data_home:root.join("data"),extension_root:root.join("extensions"),workdir:root.join("work"),
            toolhost_exe:root.join("ToolHost.exe"),permission_mode:"smart".into(),conversation_id:uuid::Uuid::new_v4().to_string(),project_id:None,
            authorized_roots:vec![root.join("work")],cancellation,runtime:Arc::new(ToolRuntime::default()),config:json!({"officialToolsRoot":root.join("official"),"mcpServers":[]}),approved:false,events:None,external_execute:None};
        let config=GatewayConfig{data_root:context.data_home.clone(),data_home:context.data_home.join("Models"),
            extension_root:context.extension_root.clone(),application_root:root.clone(),port:0,
            toolhost_exe:context.toolhost_exe.clone(),vector_extension:root.join("vec0.dll"),inference_library:root.join("onnxruntime.dll")};
        Self{context,config,_cancel:cancel,root}
    }
    fn record_path(&self,family:&str,id:&str,name:&str)->PathBuf {
        std::fs::read_dir(self.context.data_home.join("Training")).unwrap().filter_map(Result::ok)
            .map(|scope|scope.path().join(family).join(id).join(name)).find(|path|path.is_file()).unwrap()
    }
    fn record(&self,family:&str,id:&str,name:&str)->Value {
        let path=self.record_path(family,id,name);
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    }
    fn dataset(&self,id:&Value)->Value {
        self.record("datasets",id.as_str().unwrap(),"dataset.json")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        // Every fixture lives in this test-only temp namespace, never a configured user directory.
        assert_eq!(self.root.parent(),Some(std::env::temp_dir().as_path()));
        assert!(self.root.file_name().unwrap().to_string_lossy().starts_with("kynxa-native-tools-"));
        let _=kynxa_agent_core_native::storage::sqlite_connections::drain_root(&self.context.data_home);
        let _=std::fs::remove_dir_all(&self.root);
    }
}
async fn handle(action:&str,input:&Value,context:&GatewayToolContext,_config:&GatewayConfig)->Result<Value,GatewayError> {
    let action=action.strip_prefix("training.").unwrap_or(action);
    let tool=if ["capabilities","datasets.list","jobs.list","jobs.read","jobs.predict"].contains(&action){"training.inspect"}else{"training.manage"};
    gateway::execute(tool,json!({"action":action,"payload":input}),context).await
}
fn docx(path:&Path,text:&str) {
    let mut archive=zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    archive.start_file("word/document.xml",zip::write::SimpleFileOptions::default()).unwrap();
    archive.write_all(format!("<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body><w:p><w:r><w:t>{text}</w:t></w:r></w:p></w:body></w:document>").as_bytes()).unwrap();
    archive.finish().unwrap();
}

struct WorkbenchApi {
    client:reqwest::Client,
    url:String,
    chat:String,
    other:String,
    server:tokio::task::JoinHandle<()>,
}
impl WorkbenchApi {
    async fn new(fixture:&Fixture)->Self {
        use kynxa_agent_core_native::{gateway as http_gateway,orchestration::gateway_runtime::GatewayRuntime,storage::gateway as data};
        let chat=fixture.context.conversation_id.clone();let other=uuid::Uuid::new_v4().to_string();
        for id in [&chat,&other] {data::ensure_conversation(&fixture.context.data_home,id,"Training preview fixture").unwrap();}
        let app=http_gateway::router(GatewayRuntime::new(fixture.config.clone()));
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let address=listener.local_addr().unwrap();
        let server=tokio::spawn(async move{axum::serve(listener,app).await.unwrap();});
        Self{client:reqwest::Client::builder().no_proxy().build().unwrap(),url:format!("http://{address}/agent/workspace/workbench"),chat,other,server}
    }
    async fn post(&self,chat:&str,action:&str,payload:Value)->(reqwest::StatusCode,Value) {
        let response=self.client.post(&self.url).json(&json!({"conversationId":chat,"action":action,"payload":payload})).send().await.unwrap();
        (response.status(),response.json().await.unwrap())
    }
    async fn ok(&self,action:&str,payload:Value)->Value {
        let(status,value)=self.post(&self.chat,action,payload).await;assert!(status.is_success(),"{action}: {value}");value
    }
}
impl Drop for WorkbenchApi {fn drop(&mut self){self.server.abort();}}

async fn wait_training(api:&WorkbenchApi,id:&Value)->Value {
    tokio::time::timeout(std::time::Duration::from_secs(60),async{loop{
        let value=api.ok("training.jobs.read",json!({"jobId":id})).await;
        assert_ne!(value["job"]["state"],"failed","{value}");
        if value["job"]["state"]=="completed"{return value;}
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }}).await.expect("training did not complete")
}
async fn wait_trial(api:&WorkbenchApi,id:&Value,trial:&Value)->Value {
    tokio::time::timeout(std::time::Duration::from_secs(60),async{loop{
        let value=api.ok("training.jobs.trial.read",json!({"jobId":id,"trialId":trial})).await;
        assert_ne!(value["trial"]["state"],"failed","{value}");
        if matches!(value["trial"]["state"].as_str(),Some("completed"|"cancelled")){return value;}
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }}).await.expect("trial did not settle")
}
#[tokio::test]
async fn workbench_prediction_form_schema_preserves_feature_names_and_frozen_categories() {
    if std::env::var_os("KYNXA_TRAINING_PYTHON").is_none(){eprintln!("SKIP real prediction schema: set KYNXA_TRAINING_PYTHON.");return;}
    let fixture=Fixture::new();let api=WorkbenchApi::new(&fixture).await;
    let data=api.ok("training.datasets.create",json!({"format":"table","text":"area,kind,price\n10,flat,20\n20,house,50\n12,flat,24\n22,house,52","targetColumn":"price","split":{"validation":0,"test":0}})).await;
    let job=api.ok("training.jobs.create",json!({"datasetId":data["dataset"]["id"],"backend":"builtin","algorithm":"linear.regression","task":"regression","parameters":{"maxSteps":4,"epochs":4,"validationSplit":0,"testSplit":0,"loggingSteps":1}})).await;
    let id=job["job"]["id"].clone();api.ok("training.jobs.start",json!({"jobId":id})).await;wait_training(&api,&id).await;
    let state=fixture.record_path("jobs",id.as_str().unwrap(),"state.json");let before=std::fs::read(&state).unwrap();
    let schema=api.ok("training.jobs.predict.schema",json!({"jobId":id})).await;
    assert_eq!(schema["kind"],"tabular");assert_eq!(schema["fields"][0]["name"],"area");assert_eq!(schema["fields"][0]["kind"],"numeric");
    assert_eq!(schema["fields"][1]["name"],"kind");assert_eq!(schema["fields"][1]["kind"],"categorical");
    assert_eq!(schema["fields"][1]["options"],json!(["flat","house"]));
    let value=api.ok("training.jobs.predict",json!({"jobId":id,"samples":[{"features":[11,"flat"]},{"features":[null,"new-category"]}]})).await;
    assert_eq!(value["count"],2);assert!(value["predictions"][0]["prediction"].as_f64().unwrap().is_finite());
    assert_eq!(std::fs::read(state).unwrap(),before);
    let(status,_)=api.post(&api.other,"training.jobs.predict.schema",json!({"jobId":id})).await;assert!(!status.is_success());
}
#[tokio::test]
async fn workbench_local_hf_trains_trials_compares_cancels_and_keeps_training_receipts() {
    let Some(source)=std::env::var_os("KYNXA_TRAINING_HF_FIXTURE").map(PathBuf::from).filter(|path|path.join("config.json").is_file())else{eprintln!("SKIP local HF trial: set KYNXA_TRAINING_HF_FIXTURE to a tiny local model and KYNXA_TRAINING_PYTHON with torch/transformers.");return;};
    let fixture=Fixture::new();let api=WorkbenchApi::new(&fixture).await;
    let base=fixture.root.join("selected-base");std::fs::create_dir(&base).unwrap();
    for entry in std::fs::read_dir(source).unwrap(){let entry=entry.unwrap();if entry.file_type().unwrap().is_file(){std::fs::copy(entry.path(),base.join(entry.file_name())).unwrap();}}
    api.ok("training.models.register",json!({"path":base})).await;
    let data=api.ok("training.datasets.create",json!({"kind":"text","samples":[{"text":"user : hello assistant : good day [EOS]","split":"train"},{"text":"user : sky assistant : blue [EOS]","split":"train"}],"split":{"validation":0,"test":0}})).await;
    let created=api.ok("training.jobs.create",json!({"datasetId":data["dataset"]["id"],"backend":"transformers","algorithm":"text.sft","modelPath":base,
        "parameters":{"device":"cpu","precision":"fp32","cpuThreads":2,"maxSteps":2,"batchSize":1,"learningRate":0.001,"loggingSteps":1,"checkpointEvery":2,"maxLength":64,"validationSplit":0,"testSplit":0}})).await;
    let id=created["job"]["id"].clone();api.ok("training.jobs.start",json!({"jobId":id})).await;let completed=wait_training(&api,&id).await;
    assert!(!completed["metrics"].as_array().unwrap().is_empty());
    let state_path=fixture.record_path("jobs",id.as_str().unwrap(),"state.json");let state_before=std::fs::read(&state_path).unwrap();
    let metrics_before=std::fs::read(state_path.parent().unwrap().join("metrics.jsonl")).unwrap();
    let schema=api.ok("training.jobs.predict.schema",json!({"jobId":id})).await;assert_eq!(schema["kind"],"text");assert_eq!(schema["available"],true);
    let trial_id=uuid::Uuid::new_v4().to_string();let request=json!({"jobId":id,"trialId":trial_id,"messages":[{"role":"user","content":"hello"}],"mode":"compare","maxNewTokens":8});
    let started=api.ok("training.jobs.trial.start",request.clone()).await;assert_eq!(started["trial"]["id"],trial_id);
    let repeated=api.ok("training.jobs.trial.start",request.clone()).await;assert_eq!(repeated["trial"]["id"],trial_id);
    let result=wait_trial(&api,&id,&json!(trial_id)).await;let answers=result["trial"]["results"].as_array().unwrap();assert_eq!(answers.len(),2);
    assert_eq!(answers[0]["mode"],"base");assert_eq!(answers[1]["mode"],"trained");assert!(answers.iter().all(|a|a["outputTokens"].as_u64().unwrap()>0));
    let(status,_)=api.post(&api.other,"training.jobs.trial.read",json!({"jobId":id,"trialId":trial_id})).await;assert!(!status.is_success());
    let cancelled_id=uuid::Uuid::new_v4().to_string();let mut cancel_request=request.clone();cancel_request["trialId"]=json!(cancelled_id);cancel_request["mode"]=json!("trained");
    api.ok("training.jobs.trial.start",cancel_request).await;
    api.ok("training.jobs.trial.cancel",json!({"jobId":id,"trialId":cancelled_id})).await;
    let cancelled=wait_trial(&api,&id,&json!(cancelled_id)).await;assert_eq!(cancelled["trial"]["state"],"cancelled");
    assert_eq!(std::fs::read(&state_path).unwrap(),state_before);assert_eq!(std::fs::read(state_path.parent().unwrap().join("metrics.jsonl")).unwrap(),metrics_before);
    let error=handle("training.jobs.trial.start",&request,&fixture.context,&fixture.config).await.unwrap_err();assert_eq!(error.code,"INVALID_TOOL_ARGUMENTS");
    let model_record=std::fs::read_dir(state_path.parent().unwrap().parent().unwrap().parent().unwrap().join("models")).unwrap().next().unwrap().unwrap().path().join("model.json");
    std::fs::remove_file(model_record).unwrap();
    let mut denied=request;denied["trialId"]=json!(uuid::Uuid::new_v4().to_string());
    let(status,value)=api.post(&api.chat,"training.jobs.trial.start",denied).await;assert!(!status.is_success(),"Revoked model authorization must be checked again: {value}");
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
}

#[tokio::test]
async fn workbench_environment_prepares_offline_reuses_inflight_and_cancels_without_replacing_active_python() {
    let Some(original_python)=std::env::var_os("KYNXA_TRAINING_PYTHON").map(PathBuf::from).filter(|path|path.is_file())else{
        eprintln!("SKIP real isolated environment: set KYNXA_TRAINING_PYTHON.");return;
    };
    let fixture=Fixture::new();let api=WorkbenchApi::new(&fixture).await;
    let first=api.ok("training.environment.prepare",json!({"backend":"builtin"})).await;
    let duplicate=api.ok("training.environment.prepare",json!({"backend":"builtin"})).await;
    assert_eq!(duplicate["setup"]["id"],first["setup"]["id"]);assert_eq!(duplicate["reused"],true);
    let ready=tokio::time::timeout(std::time::Duration::from_secs(30),async{loop{
        let value=api.ok("training.environment.read",json!({})).await;
        assert_ne!(value["setup"]["state"],"failed","{value}");
        if value["setup"]["state"]=="ready"{break value;}
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    }}).await.expect("offline environment did not become ready");
    let selected=PathBuf::from(ready["pythonPath"].as_str().unwrap());
    assert!(selected.is_file());assert_ne!(selected,original_python);
    assert!(selected.starts_with(fixture.context.data_home.join("Runtime/training-envs")));
    assert_eq!(ready["setup"]["probe"]["available"],true);
    assert!(!fixture.context.data_home.join("Runtime/training-bootstrap").exists(),"Existing Python must not trigger a bootstrap download for builtin.");
    assert!(!fixture.context.data_home.join("Runtime/training-package-cache").exists(),"Builtin preparation must stay offline.");
    let isolated=api.post(&api.other,"training.environment.read",json!({})).await.1;
    assert_eq!(isolated["setup"]["state"],"idle");assert_ne!(isolated["pythonPath"],ready["pythonPath"]);
    let data=api.ok("training.datasets.create",json!({"kind":"tabular","samples":[
        {"features":[0.],"label":0.,"split":"train"},{"features":[1.],"label":2.,"split":"train"},
        {"features":[2.],"label":4.,"split":"train"},{"features":[3.],"label":6.,"split":"train"}]})).await;
    let configuration=json!({"datasetId":data["dataset"]["id"],"backend":"builtin","algorithm":"linear.regression","task":"regression",
        "parameters":{"epochs":2,"batchSize":2,"learningRate":0.02,"checkpointEvery":1}});
    let job=api.ok("training.jobs.create",configuration.clone()).await;let job_id=job["job"]["id"].clone();
    let config_path=fixture.record_path("jobs",job_id.as_str().unwrap(),"config.json");let frozen=std::fs::read(&config_path).unwrap();
    let listed=api.ok("training.jobs.list",json!({})).await;
    let preview=api.ok("training.resources.preview",configuration).await;
    assert!(preview["plan"]["memoryBytes"].as_u64().unwrap()>0);assert_eq!(preview["plan"]["estimate"],true);
    assert_eq!(std::fs::read(&config_path).unwrap(),frozen);
    assert_eq!(api.ok("training.jobs.list",json!({})).await["jobs"],listed["jobs"]);
    // Simulate a missing backend only inside this disposable venv, without changing production workers.
    let site=std::process::Command::new(&selected).args(["-I","-B","-X","utf8","-c","import sysconfig; print(sysconfig.get_path('purelib'))"]).output().unwrap();
    assert!(site.status.success());let site=PathBuf::from(String::from_utf8_lossy(&site.stdout).trim());
    assert!(std::fs::canonicalize(&site).unwrap().starts_with(std::fs::canonicalize(fixture.context.data_home.join("Runtime/training-envs")).unwrap()));
    let hook=site.join("sitecustomize.py");std::fs::write(&hook,"import sys\nsys.modules['builtin_backend'] = None\n").unwrap();
    api.ok("training.jobs.start",json!({"jobId":job_id})).await;
    let failed=tokio::time::timeout(std::time::Duration::from_secs(15),async{loop{
        let value=api.ok("training.jobs.read",json!({"jobId":job_id})).await;
        if value["job"]["state"]=="failed"{break value;}
        assert_ne!(value["job"]["state"],"completed");
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    }}).await.unwrap();
    assert!(failed["job"]["message"].as_str().unwrap().contains("builtin_backend"));
    assert!(!config_path.parent().unwrap().join("runtime.json").exists(),"A failed dependency check cannot pin an unusable environment.");
    std::fs::remove_file(&hook).unwrap();
    api.ok("training.environment.prepare",json!({"backend":"builtin"})).await;
    let repaired=tokio::time::timeout(std::time::Duration::from_secs(30),async{loop{
        let value=api.ok("training.environment.read",json!({})).await;assert_ne!(value["setup"]["state"],"failed","{value}");
        if value["setup"]["state"]=="ready"{break value;}
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    }}).await.unwrap();
    assert_ne!(repaired["pythonPath"],ready["pythonPath"]);
    let selected=PathBuf::from(repaired["pythonPath"].as_str().unwrap());
    api.ok("training.jobs.start",json!({"jobId":job_id})).await;
    tokio::time::timeout(std::time::Duration::from_secs(15),async{loop{
        let value=api.ok("training.jobs.read",json!({"jobId":job_id})).await;
        assert_ne!(value["job"]["state"],"failed","{value}");
        if value["job"]["state"]=="completed"{break;}
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    }}).await.expect("small training did not complete");
    let binding_path=config_path.parent().unwrap().join("runtime.json");let binding=std::fs::read(&binding_path).unwrap();
    let runtime:Value=serde_json::from_slice(&binding).unwrap();assert_eq!(runtime["pythonPath"],json!(selected));
    // Installing a new scope environment must not alter an existing trained job.
    api.ok("training.environment.prepare",json!({"backend":"builtin"})).await;
    let newer=tokio::time::timeout(std::time::Duration::from_secs(30),async{loop{
        let value=api.ok("training.environment.read",json!({})).await;assert_ne!(value["setup"]["state"],"failed","{value}");
        if value["setup"]["state"]=="ready"{break value;}
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    }}).await.unwrap();
    assert_ne!(newer["pythonPath"],repaired["pythonPath"]);assert_eq!(std::fs::read(&binding_path).unwrap(),binding);
    let prediction=api.ok("training.jobs.predict",json!({"jobId":job_id,"samples":[{"features":[1.]}]})).await;
    assert_eq!(prediction["count"],1);assert_eq!(std::fs::read(&config_path).unwrap(),frozen);
    let unavailable=selected.with_extension("unavailable");std::fs::rename(&selected,&unavailable).unwrap();
    let(status,error)=api.post(&api.chat,"training.jobs.predict",json!({"jobId":job_id,"samples":[{"features":[1.]}]})).await;
    std::fs::rename(&unavailable,&selected).unwrap();
    assert!(!status.is_success());assert_eq!(error["code"],"TRAINING_JOB_ENVIRONMENT_MISSING");
    assert_eq!(std::fs::read(&binding_path).unwrap(),binding,"Missing pinned Python cannot silently rebind to the current scope.");
    let previous=newer["pythonPath"].clone();
    // A second generation creates a new directory and must never upgrade the active one.
    let second=api.ok("training.environment.prepare",json!({"backend":"builtin"})).await;
    assert_ne!(second["setup"]["id"],first["setup"]["id"]);
    api.ok("training.environment.cancel",json!({})).await;
    let cancelled=tokio::time::timeout(std::time::Duration::from_secs(10),async{loop{
        let value=api.ok("training.environment.read",json!({})).await;
        if value["setup"]["state"]=="cancelled"{break value;}
        assert_ne!(value["setup"]["state"],"ready","Cancellation must be observed before switching interpreter.");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }}).await.expect("environment cancellation did not settle");
    assert_eq!(cancelled["pythonPath"],previous);assert!(selected.is_file());
    let output=std::process::Command::new(&selected).args(["-I","-B","-c","print('still-usable')"]).output().unwrap();
    assert!(output.status.success());assert_eq!(String::from_utf8_lossy(&output.stdout).trim(),"still-usable");
    let(status,_)=api.post(&api.chat,"training.environment.prepare",json!({"backend":"arbitrary-package"})).await;
    assert!(!status.is_success());
    let error=handle("training.environment.prepare",&json!({"backend":"builtin"}),&fixture.context,&fixture.config).await.unwrap_err();
    assert_eq!(error.code,"INVALID_TOOL_ARGUMENTS","Chat tools must not expose dependency installation.");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
}

#[tokio::test]
async fn workbench_previews_keep_scope_original_pixels_and_frozen_image_versions() {
    use base64::Engine;
    let fixture=Fixture::new();let api=WorkbenchApi::new(&fixture).await;
    let source=fixture.root.join("selected-original.png");
    let image=image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(800,600,image::Rgb([28,96,180])));
    image.save(&source).unwrap();
    let imported=api.ok("training.sources.import",json!({"sources":[{"path":source}]})).await;
    let id=imported["sources"][0]["importId"].clone();
    let preview=api.ok("training.sources.preview",json!({"importId":id})).await;
    assert_eq!(preview["originalWidth"],800);assert_eq!(preview["originalHeight"],600);
    assert_eq!(preview["width"],384);assert_eq!(preview["height"],288);
    let png=preview["dataUrl"].as_str().unwrap().strip_prefix("data:image/png;base64,").unwrap();
    let bytes=base64::engine::general_purpose::STANDARD.decode(png).unwrap();
    assert_eq!(image::load_from_memory(&bytes).unwrap().to_rgb8().get_pixel(20,20).0,[28,96,180]);
    let created=api.ok("training.datasets.create",json!({"kind":"image","sources":[{"importId":id,"text":"Blue image"}],"split":{"validation":0,"test":0}})).await;
    let dataset=created["dataset"]["id"].clone();
    let page=api.ok("training.datasets.read",json!({"datasetId":dataset})).await;
    assert_eq!(page["total"],1);assert_eq!(page["rows"][0]["hasImage"],true);
    assert!(page["rows"][0].get("imagePath").is_none());assert_eq!(std::fs::canonicalize(page["rows"][0]["source"]["path"].as_str().unwrap()).unwrap(),std::fs::canonicalize(&source).unwrap());
    assert_eq!(page["statistics"]["missingLabels"],1);assert_eq!(page["statistics"]["missingDescriptions"],0);
    for (action,payload) in [("training.sources.preview",json!({"importId":id})),("training.datasets.read",json!({"datasetId":dataset}))] {
        let(status,value)=api.post(&api.other,action,payload).await;assert!(!status.is_success());assert_eq!(value["code"],"TRAINING_NOT_FOUND");
    }
    let(status,_)=api.post(&api.chat,"training.sources.preview",json!({"path":source})).await;assert!(!status.is_success());
    let manifest=fixture.record_path("imports",id.as_str().unwrap(),"source.json");
    let record:Value=serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
    std::fs::write(manifest.parent().unwrap().join(record["name"].as_str().unwrap()),b"changed imported bytes").unwrap();
    let(status,value)=api.post(&api.chat,"training.sources.preview",json!({"importId":id})).await;assert!(!status.is_success());assert_eq!(value["code"],"TRAINING_SOURCE_CHANGED");
    let frozen=api.ok("training.sources.preview",json!({"datasetId":dataset,"sampleIndex":0})).await;
    assert_eq!(frozen["sha256"],preview["sha256"]);
    let(status,_)=api.post(&api.chat,"training.sources.preview",json!({"datasetId":dataset,"sampleIndex":1})).await;assert!(!status.is_success());
}

#[tokio::test]
async fn workbench_dataset_pagination_preserves_table_names_locations_and_cleaning_counts() {
    let fixture=Fixture::new();let api=WorkbenchApi::new(&fixture).await;
    let source=fixture.root.join("prices.csv");let mut csv=String::from("area,kind,price\n");
    for row in 0..25 {csv.push_str(&format!("{row},stone,{}\n",100+row));}csv.push_str("24,stone,124\n");
    std::fs::write(&source,csv).unwrap();
    let imported=api.ok("training.sources.import",json!({"sources":[{"path":source}]})).await;
    let created=api.ok("training.datasets.create",json!({"sources":[{"importId":imported["sources"][0]["importId"]}],"kind":"tabular","targetColumn":"price","split":{"validation":0,"test":0}})).await;
    let id=created["dataset"]["id"].clone();let path=fixture.record_path("datasets",id.as_str().unwrap(),"dataset.json");let original=std::fs::read(&path).unwrap();
    let first=api.ok("training.datasets.read",json!({"datasetId":id,"offset":0,"limit":20})).await;
    assert_eq!(first["featureNames"],json!(["area","kind"]));assert_eq!(first["targetName"],"price");assert_eq!(first["nextOffset"],20);
    assert_eq!(first["rows"].as_array().unwrap().len(),20);assert_eq!(first["total"],25);assert_eq!(first["statistics"]["duplicatesRemoved"],1);
    let last=api.ok("training.datasets.read",json!({"datasetId":id,"offset":20,"limit":20})).await;
    assert_eq!(last["rows"].as_array().unwrap().len(),5);assert!(last["nextOffset"].is_null());
    let features=last["rows"][0]["features"].as_array().unwrap();assert_eq!(features.len(),2);assert_eq!(features[0].as_f64(),Some(20.));assert_eq!(features[1].as_str(),Some("stone"));assert_eq!(last["rows"][0]["label"].as_f64(),Some(120.));
    assert_eq!(last["rows"][0]["sourceRow"],22);assert_eq!(std::fs::canonicalize(last["rows"][0]["source"]["path"].as_str().unwrap()).unwrap(),std::fs::canonicalize(&source).unwrap());
    assert_eq!(std::fs::read(path).unwrap(),original);
}

#[tokio::test]
async fn workbench_experiment_notes_do_not_rewrite_inputs_and_cleanup_keeps_all_live_references() {
    let fixture=Fixture::new();let api=WorkbenchApi::new(&fixture).await;
    let data=api.ok("training.datasets.create",json!({"kind":"tabular","samples":[{"features":[0.],"label":0},{"features":[1.],"label":1}],"split":{"validation":0,"test":0}})).await;
    let created=api.ok("training.jobs.create",json!({"name":"Frozen name","datasetId":data["dataset"]["id"],"backend":"builtin","algorithm":"mlp.classifier","parameters":{"epochs":2}})).await;
    let id=created["job"]["id"].clone();let config_path=fixture.record_path("jobs",id.as_str().unwrap(),"config.json");
    let dir=config_path.parent().unwrap();let config_before=std::fs::read(&config_path).unwrap();let dataset_before=std::fs::read(dir.join("dataset.json")).unwrap();
    api.ok("training.jobs.update",json!({"jobId":id,"name":"Experiment renamed","notes":"Retain chosen baseline","parameters":{"epochs":999}})).await;
    assert_eq!(std::fs::read(&config_path).unwrap(),config_before);assert_eq!(std::fs::read(dir.join("dataset.json")).unwrap(),dataset_before);
    let read=api.ok("training.jobs.read",json!({"jobId":id})).await;assert_eq!(read["job"]["name"],"Experiment renamed");assert_eq!(read["job"]["notes"],"Retain chosen baseline");assert_eq!(read["job"]["parameters"]["epochs"],2);
    let checkpoint=dir.join("checkpoint");std::fs::create_dir_all(checkpoint.join("hf")).unwrap();
    for step in 1..=6 {std::fs::write(checkpoint.join(format!("estimator-{step}.joblib")),format!("model-{step}")).unwrap();}
    for step in 1..=4 {let folder=checkpoint.join("hf").join(format!("checkpoint-{step}"));std::fs::create_dir_all(&folder).unwrap();std::fs::write(folder.join("weights.bin"),b"hf-weight").unwrap();}
    let pointer=json!({"estimatorCheckpoint":checkpoint.join("estimator-1.joblib")});std::fs::write(checkpoint.join("training.json"),serde_json::to_vec(&pointer).unwrap()).unwrap();
    let state_path=dir.join("state.json");let mut state:Value=serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
    state["state"]=json!("paused");state["artifacts"]=json!([{"path":checkpoint.join("estimator-2.joblib"),"kind":"file"},{"path":checkpoint.join("hf"),"kind":"directory"}]);
    std::fs::write(&state_path,serde_json::to_vec(&state).unwrap()).unwrap();
    let preview=api.ok("training.jobs.cleanup.preview",json!({"jobId":id})).await;assert_eq!(preview["count"],2,"{preview}");
    assert!(preview["candidates"].as_array().unwrap().iter().all(|item|matches!(Path::new(item["path"].as_str().unwrap()).file_name().unwrap().to_str().unwrap(),"estimator-3.joblib"|"estimator-4.joblib")));
    std::fs::write(checkpoint.join("estimator-4.joblib"),b"changed checkpoint with different size").unwrap();
    let(status,_)=api.post(&api.chat,"training.jobs.cleanup.apply",json!({"jobId":id,"token":preview["token"]})).await;assert!(!status.is_success());assert!(checkpoint.join("estimator-3.joblib").is_file());
    let current=api.ok("training.jobs.cleanup.preview",json!({"jobId":id})).await;
    let cleaned=api.ok("training.jobs.cleanup.apply",json!({"jobId":id,"token":current["token"]})).await;assert_eq!(cleaned["removedCount"],2);
    for step in [1,2,5,6] {assert!(checkpoint.join(format!("estimator-{step}.joblib")).is_file());}
    for step in 1..=4 {assert!(checkpoint.join("hf").join(format!("checkpoint-{step}/weights.bin")).is_file());}
    assert!(checkpoint.join("training.json").is_file());assert!(!checkpoint.join("estimator-3.joblib").exists());assert!(!checkpoint.join("estimator-4.joblib").exists());
    assert_eq!(std::fs::read(&config_path).unwrap(),config_before);assert_eq!(std::fs::read(dir.join("dataset.json")).unwrap(),dataset_before);
    let(status,_)=api.post(&api.other,"training.jobs.update",json!({"jobId":id,"name":"Cross-scope overwrite"})).await;assert!(!status.is_success());
}
fn pdf(path:&Path,count:usize) {
    use lopdf::{Document,Object,Stream,dictionary};
    let mut document=Document::with_version("1.5");let pages=document.new_object_id();
    let font=document.add_object(dictionary!{"Type"=>"Font","Subtype"=>"Type1","BaseFont"=>"Helvetica"});
    let resources=document.add_object(dictionary!{"Font"=>dictionary!{"F1"=>font}});
    let mut children=Vec::<Object>::new();
    for n in 1..=count {
        let content=document.add_object(Stream::new(dictionary!{},format!("BT /F1 12 Tf 20 100 Td (Real training source page {n}.) Tj ET").into_bytes()));
        children.push(document.add_object(dictionary!{"Type"=>"Page","Parent"=>pages,"MediaBox"=>vec![0.into(),0.into(),200.into(),200.into()],"Resources"=>resources,"Contents"=>content}).into());
    }
    document.objects.insert(pages,Object::Dictionary(dictionary!{"Type"=>"Pages","Kids"=>children,"Count"=>count as i64}));
    let catalog=document.add_object(dictionary!{"Type"=>"Catalog","Pages"=>pages});document.trailer.set("Root",catalog);document.save(path).unwrap();
}

#[tokio::test]
async fn csv_dataset_is_created_through_real_api_and_preserves_training_only_preprocessing() {
    let fixture=Fixture::new();
    std::fs::write(fixture.context.workdir.join("samples.csv"),"amount,kind,target\n2,red,yes\n,\"blue,green\",no\n3,red,yes\n").unwrap();
    let created=handle("training.datasets.create",&json!({"name":"真实CSV","kind":"tabular","sources":[{"path":"samples.csv"}],
        "targetColumn":"target","split":{"validation":0,"test":0}}),&fixture.context,&fixture.config).await.unwrap();
    let dataset=fixture.dataset(&created["dataset"]["id"]);
    assert_eq!(dataset["sampleCount"],3);assert_eq!(dataset["featureNames"],json!(["amount","kind"]));
    assert!(dataset["samples"].as_array().unwrap().iter().any(|row|row["features"][0].is_null()&&row["features"][1]=="blue,green"));
    assert!(dataset["samples"].as_array().unwrap().iter().all(|row|row["features"].as_array().unwrap().len()==2));
    let listed=handle("training.datasets.list",&json!({}),&fixture.context,&fixture.config).await.unwrap();
    assert_eq!(listed["datasets"][0]["sampleCount"],3);assert!(listed["datasets"][0].get("samples").is_none());
    assert_eq!(dataset["sources"][0]["sha256"].as_str().unwrap().len(),64);
}

#[tokio::test]
async fn pasted_frontend_table_and_jsonl_are_samples_instead_of_raw_corpus_text() {
    let fixture=Fixture::new();
    let table=handle("training.datasets.create",&json!({"format":"table","text":"x,kind,label\n1,red,A\n2,blue,B",
        "targetColumn":"","featureColumns":[],"split":{"validation":0,"test":0}}),&fixture.context,&fixture.config).await.unwrap();
    let dataset=fixture.dataset(&table["dataset"]["id"]);
    assert_eq!(dataset["kind"],"tabular");assert_eq!(dataset["sampleCount"],2);assert_eq!(dataset["featureNames"],json!(["x","kind"]));
    let text="{\"messages\":[{\"role\":\"user\",\"content\":\"hello\"},{\"role\":\"assistant\",\"content\":\"hi\"}]}\n{\"prompt\":\"two\",\"completion\":\"second\"}";
    let conversation=handle("training.datasets.create",&json!({"format":"auto","text":text}),&fixture.context,&fixture.config).await.unwrap();
    let dataset=fixture.dataset(&conversation["dataset"]["id"]);
    assert_eq!(dataset["kind"],"text");assert_eq!(dataset["sampleCount"],2);
    assert!(dataset["samples"].as_array().unwrap().iter().any(|row|row["messages"][0]["content"]=="hello"));
    let bad=handle("training.datasets.create",&json!({"format":"conversation","text":"not a conversation record"}),&fixture.context,&fixture.config).await;
    assert!(bad.is_err());
}

#[tokio::test]
async fn actual_xlsx_cells_become_typed_features_without_formula_recalculation() {
    let fixture=Fixture::new();
    let written=gateway::execute("document.create",json!({"path":"typed.xlsx","sheets":[{"name":"Records","rows":[
        ["amount","when","code","storedFormula","target"],
        [2,"2026-01-02","001",{"formula":"A2+1","cachedValue":3},"yes"],
        [null,"2026-01-03","002",{"formula":"A3+1","cachedValue":1},"no"]]}],"expectedHash":null}),&fixture.context).await.unwrap();
    assert!(written["sha256"].is_string());
    let created=handle("training.datasets.create",&json!({"format":"auto","sources":[{"path":"typed.xlsx"}],"sheetName":"Records","targetColumn":"target"}),&fixture.context,&fixture.config).await.unwrap();
    let dataset=fixture.dataset(&created["dataset"]["id"]);assert_eq!(dataset["kind"],"tabular");assert_eq!(dataset["sampleCount"],2);
    let row=dataset["samples"].as_array().unwrap().iter().find(|row|row["label"]=="yes").unwrap();
    assert_eq!(row["features"],json!([2.,"2026-01-02","001",3.]));
    assert_eq!(row["source"]["sheetName"],"Records");assert_eq!(row["source"]["formulasEvaluated"],false);
}

#[tokio::test]
async fn native_document_extraction_keeps_all_pdf_windows_and_each_document_in_one_split() {
    let fixture=Fixture::new();docx(&fixture.context.workdir.join("notes.docx"),"Actual Word training evidence.");
    pdf(&fixture.context.workdir.join("long.pdf"),101);
    let created=handle("training.datasets.create",&json!({"kind":"text","sources":[{"path":"notes.docx"},{"path":"long.pdf"}],"chunkChars":64}),
        &fixture.context,&fixture.config).await.unwrap();
    let dataset=fixture.dataset(&created["dataset"]["id"]);let samples=dataset["samples"].as_array().unwrap();
    let text=samples.iter().filter_map(|row|row["text"].as_str()).collect::<String>();
    assert!(text.contains("Actual Word training evidence."));assert!(text.contains("page 101."));
    let mut splits=std::collections::HashMap::new();
    for row in samples {
        let group=row["groupId"].as_str().unwrap();let split=row["split"].as_str().unwrap();
        assert!(splits.insert(group,split).is_none_or(|old|old==split));
    }
    assert_eq!(splits.len(),2);assert_eq!(dataset["sourceCount"],2);
}

#[tokio::test]
async fn workbench_deletes_idle_datasets_in_scope_and_keeps_frozen_job_inputs() {
    let fixture=Fixture::new();let api=WorkbenchApi::new(&fixture).await;
    let rows=json!([{"features":[0.,0.],"label":0,"split":"train"},{"features":[1.,1.],"label":1,"split":"train"}]);
    let first=api.ok("training.datasets.create",json!({"kind":"tabular","name":"first","samples":rows})).await["dataset"]["id"].clone();
    let second=api.ok("training.datasets.create",json!({"kind":"tabular","name":"second","samples":rows})).await["dataset"]["id"].clone();
    let job=api.ok("training.jobs.create",json!({"datasetId":first,"backend":"builtin","algorithm":"mlp","task":"classification","parameters":{"epochs":1}})).await["job"]["id"].clone();
    let(status,_)=api.post(&api.other,"training.datasets.delete.apply",json!({"datasetId":first})).await;
    assert_eq!(status,reqwest::StatusCode::NOT_FOUND,"another conversation must not reach this dataset");
    assert_eq!(api.ok("training.datasets.delete.preview",json!({"datasetId":first})).await["dependentJobs"],0);
    assert!(fixture.record_path("datasets",first.as_str().unwrap(),"dataset.json").is_file(),"preview must not delete");
    let frozen=fixture.record_path("jobs",job.as_str().unwrap(),"dataset.json");let scope=frozen.ancestors().nth(3).unwrap().to_path_buf();
    api.ok("training.datasets.delete.apply",json!({"datasetId":first})).await;
    let names=std::fs::read_dir(scope.join("datasets")).unwrap().map(|entry|entry.unwrap().file_name().to_string_lossy().to_string()).collect::<Vec<_>>();
    assert_eq!(names,vec![second.as_str().unwrap().to_owned()],"no partial or hidden copy may remain");
    let listed=api.ok("training.datasets.list",json!({})).await;
    assert_eq!(listed["datasets"].as_array().unwrap().len(),1);assert_eq!(listed["datasets"][0]["id"],second);
    assert_eq!(api.ok("training.jobs.read",json!({"jobId":job})).await["job"]["datasetId"],first);assert!(frozen.is_file());
    let(status,_)=api.post(&api.chat,"training.datasets.delete.apply",json!({"datasetId":first})).await;
    assert_eq!(status,reqwest::StatusCode::NOT_FOUND);
    let(status,_)=api.post(&api.chat,"training.datasets.delete.apply",json!({"datasetId":"../../jobs"})).await;
    assert_eq!(status,reqwest::StatusCode::BAD_REQUEST);
    assert!(handle("training.datasets.delete.apply",&json!({"datasetId":second}),&fixture.context,&fixture.config).await.is_err(),"chat models cannot delete datasets");
    assert!(fixture.record_path("datasets",second.as_str().unwrap(),"dataset.json").is_file());
}

#[tokio::test]
async fn deleting_an_image_dataset_reports_unfinished_jobs_that_read_its_assets() {
    let fixture=Fixture::new();let api=WorkbenchApi::new(&fixture).await;
    let source=fixture.root.join("selected-delete.png");image::RgbImage::from_pixel(16,16,image::Rgb([24,80,160])).save(&source).unwrap();
    let imported=api.ok("training.sources.import",json!({"sources":[{"path":source}]})).await;
    let data=api.ok("training.datasets.create",json!({"format":"image","sources":imported["sources"]})).await["dataset"]["id"].clone();
    let job=api.ok("training.jobs.create",json!({"datasetId":data,"backend":"builtin","algorithm":"mlp","task":"classification","parameters":{"epochs":1}})).await["job"]["id"].clone();
    assert_eq!(fixture.record("jobs",job.as_str().unwrap(),"state.json")["state"],"ready");
    assert_eq!(api.ok("training.datasets.delete.preview",json!({"datasetId":data})).await["dependentJobs"],1);
    let frozen=fixture.dataset(&data)["samples"][0]["imagePath"].as_str().unwrap().to_owned();assert!(Path::new(&frozen).is_file());
    api.ok("training.datasets.delete.apply",json!({"datasetId":data})).await;
    assert!(!Path::new(&frozen).exists());
}

#[tokio::test]
async fn a_running_job_protects_only_its_own_dataset_from_deletion() {
    let Some(python)=std::env::var_os("KYNXA_TRAINING_PYTHON").map(PathBuf::from).filter(|path|path.is_file()) else {
        eprintln!("SKIP running dataset protection: set KYNXA_TRAINING_PYTHON to an installed Python interpreter.");return;
    };
    assert!(python.is_absolute());let fixture=Fixture::new();let api=WorkbenchApi::new(&fixture).await;
    let rows=(0..96).map(|i|json!({"features":[i as f64/95.,1.-i as f64/95.],"label":if i<48{0}else{1},"split":"train"})).collect::<Vec<_>>();
    let used=api.ok("training.datasets.create",json!({"kind":"tabular","samples":rows})).await["dataset"]["id"].clone();
    let other=api.ok("training.datasets.create",json!({"kind":"tabular","samples":rows})).await["dataset"]["id"].clone();
    let id=api.ok("training.jobs.create",json!({"datasetId":used,"backend":"builtin","algorithm":"mlp.classifier","task":"classification",
        "parameters":{"epochs":30,"maxSteps":200,"batchSize":8,"hiddenLayers":[16,8],"learningRate":0.05,"loggingSteps":1,"checkpointEvery":5}})).await["job"]["id"].clone();
    api.ok("training.jobs.start",json!({"jobId":id})).await;
    for action in ["training.datasets.delete.preview","training.datasets.delete.apply"] {
        let(status,value)=api.post(&api.chat,action,json!({"datasetId":used})).await;
        assert_eq!(status,reqwest::StatusCode::CONFLICT,"{value}");
    }
    api.ok("training.datasets.delete.apply",json!({"datasetId":other})).await;
    api.ok("training.jobs.stop",json!({"jobId":id})).await;
    tokio::time::timeout(std::time::Duration::from_secs(30),async{loop{
        let(status,_)=api.post(&api.chat,"training.datasets.delete.apply",json!({"datasetId":used})).await;
        if status.is_success(){return;}
        assert_eq!(status,reqwest::StatusCode::CONFLICT);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }}).await.expect("a finished run must release its dataset");
}

#[tokio::test]
async fn image_dataset_keeps_original_pixels_in_an_independent_frozen_asset() {
    let fixture=Fixture::new();let path=fixture.context.workdir.join("image.png");
    image::RgbImage::from_pixel(16,16,image::Rgb([24,80,160])).save(&path).unwrap();
    let original=std::fs::read(&path).unwrap();
    let created=handle("training.datasets.create",&json!({"format":"image","sources":[{"path":"image.png","label":"blue"}]}),&fixture.context,&fixture.config).await.unwrap();
    let dataset=fixture.dataset(&created["dataset"]["id"]);
    let frozen=Path::new(dataset["samples"][0]["imagePath"].as_str().unwrap());
    assert_ne!(frozen,path);assert_eq!(dataset["samples"][0]["label"],"blue");
    image::RgbImage::from_pixel(16,16,image::Rgb([240,40,0])).save(&path).unwrap();
    assert_eq!(std::fs::read(frozen).unwrap(),original);
    assert_eq!(dataset["samples"][0]["source"]["sha256"],kynxa_agent_core_native::storage::managed_files::digest(&original));
}

#[tokio::test]
async fn training_inspection_cannot_dispatch_a_write_or_read_private_application_data() {
    let fixture=Fixture::new();
    let error=gateway::execute("training.inspect",json!({"action":"datasets.create","payload":{"text":"not authorized by a read operation"}}),&fixture.context).await.unwrap_err();
    assert_eq!(error.code,"INVALID_TOOL_ARGUMENTS");
    assert!(!fixture.context.data_home.join("Training").exists());
    std::fs::write(fixture.context.data_home.join("private.txt"),"private application data").unwrap();
    let error=handle("training.datasets.create",&json!({"sources":[{"path":fixture.context.data_home.join("private.txt")}]}),
        &fixture.context,&fixture.config).await.unwrap_err();
    assert_eq!(error.code,"PROTECTED_APP_DATA");
}

#[tokio::test]
async fn duplicate_job_submission_uses_frozen_dataset_and_keeps_original_run_identity() {
    let fixture=Fixture::new();
    let created=handle("training.datasets.create",&json!({"kind":"tabular","samples":[
        {"features":[0.,0.],"label":0,"split":"train"},{"features":[1.,1.],"label":1,"split":"train"}]}),&fixture.context,&fixture.config).await.unwrap();
    let request=json!({"datasetId":created["dataset"]["id"],"backend":"builtin","algorithm":"mlp","task":"classification",
        "parameters":{"epochs":2,"hiddenLayers":[4]},"idempotencyKey":"one-user-request"});
    let first=handle("training.jobs.create",&request,&fixture.context,&fixture.config).await.unwrap();
    let second=handle("training.jobs.create",&request,&fixture.context,&fixture.config).await.unwrap();
    assert_eq!(first["job"]["id"],second["job"]["id"]);
    let list=handle("training.jobs.list",&json!({}),&fixture.context,&fixture.config).await.unwrap();
    assert_eq!(list["jobs"].as_array().unwrap().len(),1);
    assert_eq!(list["jobs"][0]["state"],"ready");
    let original=fixture.dataset(&created["dataset"]["id"]);
    let frozen=fixture.record("jobs",first["job"]["id"].as_str().unwrap(),"dataset.json");
    assert_eq!(original,frozen);
}

#[tokio::test]
async fn native_http_picker_grants_are_scoped_and_keep_the_selected_original_bytes() {
    use kynxa_agent_core_native::{gateway as http_gateway, orchestration::gateway_runtime::GatewayRuntime,storage::gateway as data};
    let fixture=Fixture::new();let chat=fixture.context.conversation_id.clone();let other=uuid::Uuid::new_v4().to_string();
    data::ensure_conversation(&fixture.context.data_home,&chat,"Training import").unwrap();
    data::ensure_conversation(&fixture.context.data_home,&other,"Another chat").unwrap();
    let runtime=GatewayRuntime::new(fixture.config.clone());let app=http_gateway::router(runtime);
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let address=listener.local_addr().unwrap();
    let server=tokio::spawn(async move{axum::serve(listener,app).await.unwrap();});
    let client=reqwest::Client::builder().no_proxy().build().unwrap();let url=format!("http://{address}/agent/workspace/workbench");
    let source=fixture.root.join("outside.txt");std::fs::write(&source,"User-selected original bytes.").unwrap();
    let response=client.post(&url).json(&json!({"conversationId":chat,"action":"training.sources.import","payload":{"sources":[{"path":source}]}})).send().await.unwrap();
    let status=response.status();let imported:Value=response.json().await.unwrap();assert!(status.is_success(),"{imported}");
    let id=imported["sources"][0]["importId"].as_str().unwrap();
    std::fs::write(&source,"Changed original does not change the frozen import.").unwrap();
    let response=client.post(&url).json(&json!({"conversationId":chat,"action":"training.datasets.create","payload":{"format":"text","sources":[{"importId":id}]}})).send().await.unwrap();
    let status=response.status();let created:Value=response.json().await.unwrap();assert!(status.is_success(),"{created}");
    assert_eq!(created["dataset"]["preview"][0]["text"],"User-selected original bytes.");
    let response=client.post(&url).json(&json!({"conversationId":other,"action":"training.datasets.create","payload":{"sources":[{"importId":id}]}})).send().await.unwrap();
    assert!(!response.status().is_success());let rejected:Value=response.json().await.unwrap();assert_eq!(rejected["code"],"TRAINING_NOT_FOUND");
    let stored=fixture.dataset(&created["dataset"]["id"]);let frozen=Path::new(stored["sources"][0]["path"].as_str().unwrap());
    std::fs::write(frozen,"tampered").unwrap();
    let response=client.post(&url).json(&json!({"conversationId":chat,"action":"training.datasets.create","payload":{"sources":[{"importId":id}]}})).send().await.unwrap();
    let rejected:Value=response.json().await.unwrap();assert_eq!(rejected["code"],"TRAINING_SOURCE_CHANGED");server.abort();
}

#[tokio::test]
async fn real_worker_pauses_at_a_checkpoint_and_resumes_the_same_model_run() {
    let Some(python)=std::env::var_os("KYNXA_TRAINING_PYTHON").map(PathBuf::from).filter(|path|path.is_file()) else {
        eprintln!("SKIP real training worker: set KYNXA_TRAINING_PYTHON to an installed Python interpreter.");return;
    };
    assert!(python.is_absolute());let fixture=Fixture::new();
    let rows=(0..96).map(|i|json!({"features":[i as f64/95.,1.-i as f64/95.],"label":if i<48{0}else{1},"split":"train"})).collect::<Vec<_>>();
    let data=handle("training.datasets.create",&json!({"kind":"tabular","samples":rows}),&fixture.context,&fixture.config).await.unwrap();
    let created=handle("training.jobs.create",&json!({"datasetId":data["dataset"]["id"],"backend":"builtin","algorithm":"mlp.classifier","task":"classification",
        "parameters":{"epochs":30,"maxSteps":200,"batchSize":8,"hiddenLayers":[16,8],"learningRate":0.05,"loggingSteps":1,"checkpointEvery":5}}),&fixture.context,&fixture.config).await.unwrap();
    let id=created["job"]["id"].clone();let request=json!({"jobId":id});
    handle("training.jobs.start",&request,&fixture.context,&fixture.config).await.unwrap();
    let wait=async {
        loop {
            let result=handle("training.jobs.read",&request,&fixture.context,&fixture.config).await.unwrap();
            assert_ne!(result["job"]["state"],"failed","{result}");
            if result["job"]["step"].as_u64().unwrap_or(0)>0 && !result["metrics"].as_array().unwrap().is_empty(){return result;}
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    };
    let before=tokio::time::timeout(std::time::Duration::from_secs(20),wait).await.unwrap();
    handle("training.jobs.pause",&request,&fixture.context,&fixture.config).await.unwrap();
    let wait=async {loop {
        let result=handle("training.jobs.read",&request,&fixture.context,&fixture.config).await.unwrap();
        assert_ne!(result["job"]["state"],"failed","{result}");
        if result["job"]["state"]=="paused"{return result;}
        assert_ne!(result["job"]["state"],"completed","Training completed before pause was applied.");
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }};
    let paused=tokio::time::timeout(std::time::Duration::from_secs(20),wait).await.unwrap();
    assert!(paused["job"]["step"].as_u64().unwrap()>=before["job"]["step"].as_u64().unwrap());
    assert!(paused["job"]["canResume"].as_bool().unwrap_or(false));
    let mut restored=fixture.context.clone();restored.runtime=Arc::new(ToolRuntime::default());
    handle("training.jobs.resume",&request,&restored,&fixture.config).await.unwrap();
    let wait=async {loop {
        let result=handle("training.jobs.read",&request,&restored,&fixture.config).await.unwrap();
        assert_ne!(result["job"]["state"],"failed","{result}");
        if result["job"]["state"]=="completed"{return result;}
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }};
    let completed=tokio::time::timeout(std::time::Duration::from_secs(30),wait).await.unwrap();
    assert_eq!(completed["job"]["id"],id);assert_eq!(completed["job"]["step"],200);
    let metrics=completed["metrics"].as_array().unwrap();let first=metrics.first().unwrap()["loss"].as_f64().unwrap();
    let last=metrics.last().unwrap()["loss"].as_f64().unwrap();assert!(first.is_finite()&&last.is_finite()&&last<first,"first={first}, last={last}");
    assert!(!completed["artifacts"].as_array().unwrap().is_empty());
    let limited=handle("training.jobs.read",&json!({"jobId":id,"metricLimit":before["nextMetric"]}),&restored,&fixture.config).await.unwrap();
    assert_eq!(limited["metrics"],before["metrics"]);assert_eq!(limited["nextMetric"],before["nextMetric"]);assert_eq!(limited["hasMoreMetrics"],false);
    let limit_end=handle("training.jobs.read",&json!({"jobId":id,"afterMetric":before["nextMetric"],"metricLimit":before["nextMetric"]}),&restored,&fixture.config).await.unwrap();
    assert!(limit_end["metrics"].as_array().unwrap().is_empty());
    for action in ["training.jobs.pause","training.jobs.resume"] {
        let result=handle(action,&request,&restored,&fixture.config).await.unwrap();assert_eq!(result["job"]["state"],"completed");assert_eq!(result["job"]["step"],200);
    }
    let prediction=handle("training.jobs.predict",&json!({"jobId":id,"samples":[{"features":[0.,1.]},{"features":[1.,0.]}]}),&restored,&fixture.config).await.unwrap();
    assert_eq!(prediction["count"],2);assert_eq!(prediction["predictions"][0]["prediction"],0);assert_eq!(prediction["predictions"][1]["prediction"],1);
    let unchanged=handle("training.jobs.read",&request,&restored,&fixture.config).await.unwrap();assert_eq!(unchanged["job"]["state"],"completed");assert_eq!(unchanged["nextMetric"],completed["nextMetric"]);
    let listed=handle("training.jobs.list",&json!({}),&restored,&fixture.config).await.unwrap();assert_eq!(listed["jobs"].as_array().unwrap().len(),1);
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
}

#[tokio::test]
async fn invalid_worker_parameters_fail_clearly_and_release_the_active_registration() {
    let Some(_python)=std::env::var_os("KYNXA_TRAINING_PYTHON").map(PathBuf::from).filter(|path|path.is_file())else{eprintln!("SKIP worker failure test: set KYNXA_TRAINING_PYTHON.");return;};
    let fixture=Fixture::new();let data=handle("training.datasets.create",&json!({"kind":"tabular","samples":[
        {"features":[0.],"label":0,"split":"train"},{"features":[1.],"label":1,"split":"train"}]}),&fixture.context,&fixture.config).await.unwrap();
    let created=handle("training.jobs.create",&json!({"datasetId":data["dataset"]["id"],"backend":"builtin","algorithm":"mlp.classifier","parameters":{"learningRate":0.}}),&fixture.context,&fixture.config).await.unwrap();
    let request=json!({"jobId":created["job"]["id"]});
    for _ in 0..2 {
        let started=handle("training.jobs.start",&request,&fixture.context,&fixture.config).await.unwrap();
        assert_ne!(started["job"]["state"],"failed","A prior failure must not leave a permanent active registration.");
        let wait=async {loop {
            let read=handle("training.jobs.read",&request,&fixture.context,&fixture.config).await.unwrap();
            if read["job"]["state"]=="failed"{return read;}
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }};
        let failed=tokio::time::timeout(std::time::Duration::from_secs(10),wait).await.unwrap();
        assert!(!failed["job"]["message"].as_str().unwrap_or("").is_empty());
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let listed=handle("training.jobs.list",&json!({}),&fixture.context,&fixture.config).await.unwrap();assert_eq!(listed["jobs"].as_array().unwrap().len(),1);
}

#[tokio::test]
async fn registered_model_directories_stream_to_new_targets_and_never_overwrite_or_export_unregistered_files() {
    let fixture=Fixture::new();
    let data=handle("training.datasets.create",&json!({"kind":"tabular","samples":[{"features":[0.],"label":0,"split":"train"},{"features":[1.],"label":1,"split":"train"}]}),&fixture.context,&fixture.config).await.unwrap();
    let created=handle("training.jobs.create",&json!({"datasetId":data["dataset"]["id"],"backend":"builtin","algorithm":"mlp.classifier"}),&fixture.context,&fixture.config).await.unwrap();
    let id=created["job"]["id"].as_str().unwrap();let state_path=fixture.record_path("jobs",id,"state.json");let dir=state_path.parent().unwrap();
    let bundle=dir.join("artifacts/export-fixture");std::fs::create_dir_all(bundle.join("nested")).unwrap();
    std::fs::write(bundle.join("config.json"),b"{\"fixture\":true}").unwrap();
    let weight=bundle.join("nested/weights.bin");let file=std::fs::File::create(&weight).unwrap();file.set_len(17*1024*1024).unwrap();drop(file);
    let mut state:Value=serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
    state["artifacts"]=json!([{"path":bundle,"kind":"directory","title":"registered export fixture"}]);std::fs::write(&state_path,serde_json::to_vec(&state).unwrap()).unwrap();
    let destination=fixture.context.workdir.join("exported-model");let request=json!({"jobId":id,"artifactPath":bundle,"destination":destination});
    let exported=handle("training.jobs.export",&request,&fixture.context,&fixture.config).await.unwrap();assert_eq!(exported["fileCount"],2);
    assert_eq!(std::fs::read(destination.join("config.json")).unwrap(),b"{\"fixture\":true}");
    let copied=std::fs::read(destination.join("nested/weights.bin")).unwrap();assert_eq!(copied.len(),17*1024*1024);assert!(copied.iter().all(|b|*b==0));
    let error=handle("training.jobs.export",&request,&fixture.context,&fixture.config).await.unwrap_err();assert_eq!(error.code,"TRAINING_EXPORT_EXISTS");
    assert_eq!(std::fs::read(destination.join("config.json")).unwrap(),b"{\"fixture\":true}");
    let unknown=dir.join("unregistered.txt");std::fs::write(&unknown,b"not a registered artifact").unwrap();let denied=fixture.context.workdir.join("must-not-appear.txt");
    assert!(handle("training.jobs.export",&json!({"jobId":id,"artifactPath":unknown,"destination":denied}),&fixture.context,&fixture.config).await.is_err());assert!(!denied.exists());
    let conflict=fixture.context.workdir.join("wrong-version");
    let error=handle("training.jobs.export",&json!({"jobId":id,"artifactPath":bundle,"destination":conflict,"sourceExpectedHash":"0".repeat(64)}),&fixture.context,&fixture.config).await.unwrap_err();
    assert_eq!(error.code,"TRAINING_EXPORT_CONFLICT");assert!(!conflict.exists());
    let linked=dir.join("linked.bin");std::fs::hard_link(&weight,&linked).unwrap();
    state["artifacts"].as_array_mut().unwrap().push(json!({"path":linked,"kind":"file"}));std::fs::write(&state_path,serde_json::to_vec(&state).unwrap()).unwrap();
    let link_target=fixture.context.workdir.join("hardlink-export.bin");
    assert!(handle("training.jobs.export",&json!({"jobId":id,"artifactPath":linked,"destination":link_target}),&fixture.context,&fixture.config).await.is_err());assert!(!link_target.exists());
    std::fs::remove_file(linked).unwrap();
    fixture._cancel.send_replace(true);let cancelled=fixture.context.workdir.join("cancelled-export");
    let error=handle("training.jobs.export",&json!({"jobId":id,"artifactPath":bundle,"destination":cancelled}),&fixture.context,&fixture.config).await.unwrap_err();
    assert_eq!(error.code,"TOOL_CANCELLED");assert!(!cancelled.exists());
    assert!(!std::fs::read_dir(&fixture.context.workdir).unwrap().filter_map(Result::ok).any(|entry|entry.file_name().to_string_lossy().starts_with(".kynxa-training-export-")));
}
