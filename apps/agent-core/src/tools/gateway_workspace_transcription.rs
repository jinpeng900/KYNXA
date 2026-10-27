//! Local transcription uses verified workspace bytes and an owned, cancellable CLI process.
use super::gateway::{GatewayToolContext, check_cancelled, failure};
use crate::{gateway_contract::{GatewayConfig, GatewayError}, storage::managed_files};
use serde_json::{Value, json};
use std::{collections::HashMap, path::{Path, PathBuf}, process::Stdio, sync::{Mutex, OnceLock}, time::{Duration, Instant}};
use tokio::{io::AsyncReadExt, sync::watch};

const MAX_AUDIO: u64 = 256 * 1024 * 1024;
const MAX_RESULT: u64 = 4 * 1024 * 1024;
const MAX_SECONDS: u64 = 600;
struct Operation { sender: watch::Sender<bool>, running: bool, created: Instant }
static OPERATIONS: OnceLock<Mutex<HashMap<String, Operation>>> = OnceLock::new();
static SLOT: OnceLock<tokio::sync::Semaphore> = OnceLock::new();
struct Registration(String);
impl Drop for Registration {
    fn drop(&mut self) { if let Ok(mut operations)=OPERATIONS.get_or_init(Default::default).lock() {operations.remove(&self.0);} }
}

fn operation_id(input: &Value) -> Result<String, GatewayError> {
    let value=input["operationId"].as_str().ok_or_else(||failure("INVALID_TOOL_ARGUMENTS","转录须提供 operationId，以便关闭预览时取消。",400))?;
    uuid::Uuid::parse_str(value).map(|id|id.to_string()).map_err(|_|failure("INVALID_TOOL_ARGUMENTS","转录 operationId 必须是 UUID。",400))
}
fn cancelled() -> GatewayError {failure("TOOL_CANCELLED","本地转录已取消。",409)}
fn timeout() -> GatewayError {failure("TRANSCRIPTION_TIMEOUT","本地转录超过 10 分钟，已停止后台进程。",504)}

fn register(id: &str, context: &GatewayToolContext) -> Result<(Registration,watch::Receiver<bool>),GatewayError> {
    let key=format!("{}:{id}",context.conversation_id);
    let mut operations=OPERATIONS.get_or_init(Default::default).lock().map_err(|_|failure("TRANSCRIPTION_STATE_FAILED","转录状态不可用。",500))?;
    operations.retain(|_,operation|operation.running || operation.created.elapsed()<Duration::from_secs(600));
    if operations.get(&key).is_some_and(|operation|operation.running) {return Err(failure("TRANSCRIPTION_ALREADY_RUNNING","此转录请求已在执行。",409));}
    if operations.len()>=64 {return Err(failure("TRANSCRIPTION_BUSY","本地转录队列已满，请稍后重试。",429));}
    let operation=operations.entry(key.clone()).or_insert_with(||{let(sender,_)=watch::channel(false);Operation{sender,running:false,created:Instant::now()}});
    operation.running=true;
    Ok((Registration(key),operation.sender.subscribe()))
}

pub(super) fn cancel(input: &Value, context: &GatewayToolContext) -> Result<Value,GatewayError> {
    let id=operation_id(input)?; let key=format!("{}:{id}",context.conversation_id);
    let mut operations=OPERATIONS.get_or_init(Default::default).lock().map_err(|_|failure("TRANSCRIPTION_STATE_FAILED","转录状态不可用。",500))?;
    operations.retain(|_,operation|operation.running || operation.created.elapsed()<Duration::from_secs(600));
    if operations.len()>=64 && !operations.contains_key(&key) {return Err(failure("TRANSCRIPTION_BUSY","转录取消队列已满。",429));}
    let operation=operations.entry(key).or_insert_with(||{let(sender,_)=watch::channel(true);Operation{sender,running:false,created:Instant::now()}});
    operation.sender.send_replace(true);
    Ok(json!({"status":"cancellation-requested","operationId":id}))
}

fn existing(candidates: impl IntoIterator<Item=PathBuf>) -> Option<PathBuf> {candidates.into_iter().find(|path|path.is_file())}
fn dependencies(application_root: &Path) -> (Option<PathBuf>,Option<PathBuf>,Option<PathBuf>) {
    let root=application_root.join("runtime/transcription");
    let engine=existing(std::env::var_os("KYNXA_TRANSCRIPTION_ENGINE").map(PathBuf::from).into_iter().chain([root.join(if cfg!(windows){"whisper-cli.exe"}else{"whisper-cli"})]));
    let model=existing(std::env::var_os("KYNXA_TRANSCRIPTION_MODEL").map(PathBuf::from).into_iter().chain(engine.as_ref().and_then(|path|path.parent()).map(|path|path.join("ggml-tiny.bin"))).chain([root.join("ggml-tiny.bin")]));
    let mut ffmpeg=std::env::var_os("KYNXA_FFMPEG_CLI").map(PathBuf::from).into_iter().collect::<Vec<_>>();
    ffmpeg.push(root.join(if cfg!(windows){"ffmpeg.exe"}else{"ffmpeg"}));
    if let Some(path)=std::env::var_os("PATH") {ffmpeg.extend(std::env::split_paths(&path).map(|root|root.join(if cfg!(windows){"ffmpeg.exe"}else{"ffmpeg"})));}
    (engine,model,existing(ffmpeg))
}

async fn read_log(stream: impl tokio::io::AsyncRead+Unpin) -> Result<Vec<u8>,GatewayError> {
    let mut bytes=Vec::new();
    stream.take(128*1024+1).read_to_end(&mut bytes).await?;
    if bytes.len()>128*1024 {return Err(failure("TRANSCRIPTION_OUTPUT_LIMIT","转录进程输出超过限制。",413));}
    Ok(bytes)
}

async fn process(program: &Path, args: &[String], directory: &Path, context: &GatewayToolContext, cancellation: &watch::Receiver<bool>, deadline: tokio::time::Instant) -> Result<(),GatewayError> {
    check_cancelled(context)?; if *cancellation.borrow(){return Err(cancelled());}
    let mut command=tokio::process::Command::new(program);
    command.args(args).current_dir(directory).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    super::gateway_process::hide(&mut command);
    #[cfg(windows)] command.creation_flags(0x08000000|0x00004000); // Hidden, below-normal priority.
    let mut child=command.spawn().map_err(|_|failure("TRANSCRIPTION_ENGINE_FAILED","本地转录程序无法启动，请检查运行库与引擎配置。",503))?;
    let owner=match super::gateway_process::OwnedProcessJob::attach(&child){Ok(owner)=>owner,Err(error)=>{super::gateway_process::terminate(&mut child).await;return Err(error);}};
    let stdout=child.stdout.take().unwrap();let stderr=child.stderr.take().unwrap();
    let mut explicit=cancellation.clone();let mut contextual=context.cancellation.clone();
    let work=async {
        let(status,_,_)=tokio::try_join!(async{child.wait().await.map_err(GatewayError::from)},read_log(stdout),read_log(stderr))?;
        if status.success(){Ok(())}else{Err(failure("TRANSCRIPTION_ENGINE_FAILED","本地转录程序未完成，请检查音频格式、模型与运行库。",422))}
    };
    let result=tokio::select! {
        result=work=>result,
        _=super::gateway::wait_for_cancellation(&mut explicit)=>Err(cancelled()),
        _=super::gateway::wait_for_cancellation(&mut contextual)=>Err(cancelled()),
        _=tokio::time::sleep_until(deadline)=>Err(timeout()),
    };
    if result.is_err(){super::gateway_process::terminate(&mut child).await;}
    drop(owner);result
}

fn parse_result(document: Value) -> Result<Value,GatewayError> {
    let invalid=||failure("TRANSCRIPTION_RESULT_INVALID","本地转录输出格式无效。",422);
    let rows=document["transcription"].as_array().ok_or_else(invalid)?;
    if rows.len()>2000{return Err(failure("TRANSCRIPTION_OUTPUT_LIMIT","转录分段超过 2000 项。",413));}
    let mut segments=Vec::new();let mut text=String::new();
    for row in rows {
        let start=row["offsets"]["from"].as_u64().ok_or_else(invalid)?;
        let end=row["offsets"]["to"].as_u64().filter(|end|*end>=start && *end<=24*60*60*1000).ok_or_else(invalid)?;
        let line=row["text"].as_str().ok_or_else(invalid)?.trim();
        if text.len()+line.len()>1024*1024{return Err(failure("TRANSCRIPTION_OUTPUT_LIMIT","转录文本超过 1 MiB。",413));}
        if !text.is_empty(){text.push('\n');}text.push_str(line);
        segments.push(json!({"startMs":start,"endMs":end,"text":line}));
    }
    Ok(json!({"text":text,"segments":segments,"language":document["result"]["language"]}))
}

pub(super) async fn transcribe(input: &Value, context: &GatewayToolContext, config: &GatewayConfig) -> Result<Value,GatewayError> {
    transcribe_at(input,context,&config.application_root).await
}

pub(super) async fn tool(mut input: Value, context: &GatewayToolContext) -> Result<Value,GatewayError> {
    input["operationId"]=json!(uuid::Uuid::new_v4().to_string());
    let root=context.toolhost_exe.parent().and_then(Path::parent)
        .ok_or_else(||failure("TRANSCRIPTION_RUNTIME_MISSING","无法定位本地转录运行时。",503))?;
    transcribe_at(&input,context,root).await
}

async fn transcribe_at(input: &Value, context: &GatewayToolContext, application_root: &Path) -> Result<Value,GatewayError> {
    let id=operation_id(input)?;let(_registration,cancellation)=register(&id,context)?;
    let deadline=tokio::time::Instant::now()+Duration::from_secs(MAX_SECONDS);
    let language=input["language"].as_str().unwrap_or("auto");
    if !matches!(language,"auto"|"zh"|"en"){return Err(failure("INVALID_TOOL_ARGUMENTS","转录语言仅支持 auto、zh、en。",400));}
    if *cancellation.borrow(){return Err(cancelled());}
    let(path,binding)=super::gateway_workspace_workbench::bound(input,context,false).await?;
    let extension=super::gateway_workspace_preview::extension(&path);
    if !super::gateway_workspace_media::format(&path).is_some_and(|(kind,_)|matches!(kind,"audio"|"video")){return Err(failure("TRANSCRIPTION_FORMAT_UNSUPPORTED","请选择音频或视频文件。",422));}
    // Wait before reading large source bytes; queued requests retain only their scoped identities.
    let mut cancellation_wait=cancellation.clone();let mut contextual=context.cancellation.clone();
    let _slot=tokio::select! {
        slot=SLOT.get_or_init(||tokio::sync::Semaphore::new(1)).acquire()=>slot.map_err(|_|failure("TRANSCRIPTION_BUSY","转录队列不可用。",503))?,
        _=super::gateway::wait_for_cancellation(&mut cancellation_wait)=>return Err(cancelled()),
        _=super::gateway::wait_for_cancellation(&mut contextual)=>return Err(cancelled()),
        _=tokio::time::sleep_until(deadline)=>return Err(timeout()),
    };
    let source=path.clone();
    let bytes=tokio::task::spawn_blocking(move||{
        let bytes=super::gateway_workspace_preview::checked_bytes(&source,&binding)?;
        if bytes.len()as u64>MAX_AUDIO{return Err(failure("TRANSCRIPTION_INPUT_LIMIT","转录源文件超过 256 MiB。",413));}Ok(bytes)
    }).await.map_err(|_|failure("TRANSCRIPTION_WORKER_FAILED","音频读取未完成。",500))??;
    let version=managed_files::digest(&bytes);
    if input["version"].as_str().is_some_and(|expected|expected!=version){return Err(failure("TOOL_FILE_CONFLICT","媒体文件已经变化，请刷新后重新转录。",409));}
    let(engine,model,ffmpeg)=dependencies(application_root);
    let native=matches!(extension.as_str(),"wav"|"wave"|"mp3"|"flac"|"ogg"|"oga");
    if engine.is_none()||model.is_none()||!native&&ffmpeg.is_none(){
        return Ok(json!({"status":"dependency-missing","operationId":id,"path":path,"version":version,
            "dependency":{"engineAvailable":engine.is_some(),"modelAvailable":model.is_some(),"ffmpegAvailable":ffmpeg.is_some(),"installDirectory":"runtime/transcription","engineOverride":"KYNXA_TRANSCRIPTION_ENGINE","modelOverride":"KYNXA_TRANSCRIPTION_MODEL","ffmpegOverride":"KYNXA_FFMPEG_CLI"},
            "notice":if engine.is_none()||model.is_none(){"需要本地 whisper-cli 与 ggml-tiny 多语模型；请安装随包转录运行时或配置可信主机路径。"}else{"此格式需要带音频解码器的 FFmpeg；可先导出 WAV/MP3/FLAC/OGG，或配置 KYNXA_FFMPEG_CLI。"}}));
    }
    let temporary=context.data_home.join("Cache/Transcription").join(uuid::Uuid::new_v4().to_string());
    managed_files::inspect(&context.data_home,&temporary,true,false,MAX_RESULT)?;
    let result=async {
        let source_name=format!("source.{}",if extension=="wave"{"wav"}else if extension=="oga"{"ogg"}else{&extension});
        tokio::fs::write(temporary.join(&source_name),bytes).await?;
        // ASCII relative arguments also work when the Windows account/workspace has a Unicode path.
        tokio::fs::copy(model.unwrap(),temporary.join("model.bin")).await?;
        let input_name=if native{source_name}else{
            // Fix the demuxer instead of allowing a renamed playlist to fetch unrelated resources.
            let demuxer=match extension.as_str(){"m4a"|"m4b"|"mp4"|"m4v"|"mov"=>"mov","webm"|"mkv"=>"matroska","avi"=>"avi","aac"=>"aac","aif"|"aiff"=>"aiff","opus"|"ogv"=>"ogg",_=>return Err(failure("TRANSCRIPTION_FORMAT_UNSUPPORTED","此媒体格式暂不支持转录，请导出 WAV。",422))};
            let args=["-nostdin","-v","error","-protocol_whitelist","file,pipe","-y","-f",demuxer,"-i",&source_name,"-vn","-ac","1","-ar","16000","-c:a","pcm_s16le","-t","600","-fs","22000000","converted.wav"].map(str::to_owned);
            process(&ffmpeg.unwrap(),&args,&temporary,context,&cancellation,deadline).await?;
            "converted.wav".into()
        };
        // Do not pass an oversized --duration: v1.8.3 advances past short inputs into padded silence.
        let args=["-m","model.bin","-f",&input_name,"-l",language,"-t","2","-ng","-oj","-of","result","-np"].map(str::to_owned);
        process(&engine.unwrap(),&args,&temporary,context,&cancellation,deadline).await?;
        let bytes=managed_files::read(&context.data_home,&temporary.join("result.json"),MAX_RESULT)?.ok_or_else(||failure("TRANSCRIPTION_RESULT_MISSING","转录程序没有产出结果。",422))?;
        let document:Value=serde_json::from_slice(&bytes).map_err(|_|failure("TRANSCRIPTION_RESULT_INVALID","本地转录结果不是有效 JSON。",422))?;
        let mut result=parse_result(document)?;
        for(key,value)in json!({"status":"completed","operationId":id,"engine":"whisper.cpp","model":"tiny-multilingual","path":path,"version":version,"processingLimitMs":600000,"coverage":if native{"full-source"}else{"up-to-first-10-minutes"},"notice":if native{"本地 tiny 多语模型转录；请核对专有名词与关键内容。"}else{"通过 FFmpeg 转换并转录前 10 分钟；请核对专有名词与关键内容。"}}).as_object().unwrap(){result[key]=value.clone();}
        Ok::<_,GatewayError>(result)
    }.await;
    let _=tokio::fs::remove_dir_all(&temporary).await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn timestamps_and_text_come_only_from_cli_json(){
        let result=parse_result(json!({"result":{"language":"en"},"transcription":[{"offsets":{"from":1200,"to":2500},"text":" hello "}]})).unwrap();
        assert_eq!(result["segments"][0],json!({"startMs":1200,"endMs":2500,"text":"hello"}));
        assert!(parse_result(json!({"transcription":[{"offsets":{"from":20,"to":10},"text":"invalid"}]})).is_err());
    }
    #[tokio::test]
    async fn cancellation_is_scoped_and_survives_delivery_before_registration(){
        let(context,_)=super::super::gateway::test_context();let id=uuid::Uuid::new_v4().to_string();
        cancel(&json!({"operationId":id}),&context).unwrap();
        let(_registration,receiver)=register(&id,&context).unwrap();assert!(*receiver.borrow());
        let mut other=context.clone();other.conversation_id=uuid::Uuid::new_v4().to_string();
        let(_other,receiver)=register(&id,&other).unwrap();assert!(!*receiver.borrow());
        std::fs::remove_dir_all(context.workdir.parent().unwrap()).unwrap();
    }
    #[tokio::test]
    #[ignore = "requires the pinned official whisper runtime and public JFK sample"]
    async fn official_whisper_transcribes_verified_workspace_bytes(){
        let(context,_cancel)=super::super::gateway::test_context();
        let runtime=PathBuf::from(std::env::var_os("KYNXA_TRANSCRIPTION_TEST_RUNTIME").expect("test runtime"));
        let sample=PathBuf::from(std::env::var_os("KYNXA_TRANSCRIPTION_TEST_AUDIO").expect("test sample"));
        let config=GatewayConfig{data_home:context.data_home.clone(),data_root:context.data_home.clone(),extension_root:context.extension_root.clone(),application_root:runtime,port:0,toolhost_exe:context.toolhost_exe.clone(),vector_extension:PathBuf::new(),inference_library:PathBuf::new()};
        let path=context.workdir.join("sample.wav");std::fs::copy(sample,&path).unwrap();
        let mut input=json!({"operationId":uuid::Uuid::new_v4().to_string(),"path":path,"version":"stale","language":"en"});
        assert_eq!(transcribe(&input,&context,&config).await.unwrap_err().code,"TOOL_FILE_CONFLICT");
        input["operationId"]=json!(uuid::Uuid::new_v4().to_string());input["version"]=json!(managed_files::digest(&std::fs::read(&path).unwrap()));
        let result=transcribe(&input,&context,&config).await.unwrap();
        assert_eq!(result["status"],"completed");assert!(result["text"].as_str().unwrap().to_lowercase().contains("country"));
        assert!(!result["segments"].as_array().unwrap().is_empty());assert_eq!(result["version"],input["version"]);
        assert!(result["segments"].as_array().unwrap().last().unwrap()["endMs"].as_u64().unwrap()<=15000,"the 11-second fixture must not be extended into padded silence");
        let output=std::env::var_os("KYNXA_TRANSCRIPTION_TEST_REPORT").expect("test report");std::fs::write(output,serde_json::to_vec_pretty(&result).unwrap()).unwrap();
        let external=context.workdir.parent().unwrap().join("outside.wav");std::fs::copy(&path,&external).unwrap();
        input["operationId"]=json!(uuid::Uuid::new_v4().to_string());input["path"]=json!(external);
        assert_eq!(transcribe(&input,&context,&config).await.unwrap_err().code,"WORKSPACE_PREVIEW_FORBIDDEN");
        std::fs::remove_dir_all(context.workdir.parent().unwrap()).unwrap();
    }
}
