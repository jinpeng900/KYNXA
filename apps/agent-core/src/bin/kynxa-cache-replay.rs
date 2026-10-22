//! Development-only offline JSONL replay. Never opens a task database, API connection or tool executor.
//! 仅供开发的离线 JSONL 回放，不打开任务数据库、不调用 API 或工具执行器。

use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;

use kynxa_agent_core_native::models::cache_replay::ReplayAnalyzer;
use serde_json::Value;

const MAXIMUM_LINE_BYTES: u64 = 32 * 1024 * 1024;

fn run() -> Result<(), String> {
    let mut input=None; let mut output=None; let mut price_path=None;
    let mut arguments=std::env::args().skip(1);
    while let Some(argument)=arguments.next() {
        match argument.as_str() {
            "--input" => input=Some(PathBuf::from(arguments.next().ok_or("--input requires an offline JSONL file")?)),
            "--output" => output=Some(PathBuf::from(arguments.next().ok_or("--output requires a path")?)),
            "--prices" => price_path=Some(PathBuf::from(arguments.next().ok_or("--prices requires an explicit price JSON")?)),
            "--help" | "-h" => {
                println!("Offline only: kynxa-cache-replay --input copied-requests.jsonl [--prices explicit-prices.json] [--output report.json]\nFirst line: {{\"type\":\"kynxa.cache-replay.manifest\",\"schemaVersion\":1,\"offlineCopy\":true}}\nRows: recordedAt (RFC3339), taskId, protocol, model, diagnosticEpoch, request (saved wire JSON), usageSource (provider), usage, schemaSnapshotsComplete, historySnapshotComplete.\nComparisons: A original wire; B stable order of exactly the selected current declarations. Missing diagnosticEpoch starts a fresh structural baseline; real reported usage remains available.\nOptional control comparison: messageSources matches each messages/input entry: {{\"id\":\"stable-id\",\"kind\":\"history\"}} or {{\"id\":\"control-v2\",\"kind\":\"control\",\"controlKey\":\"budget\",\"cacheMutationSafe\":true,\"supersedesId\":\"control-v1\"}}. Only explicitly safe user-role notices are compared. immediateInvalidation=true, formal history edits or unsupported sources reset this comparison; no text guessing.\nPrice JSON: model, protocol, currency, uncachedInputPerMillion, cacheReadPerMillion, outputPerMillion, cacheWriteBilling (separate|included-in-input), cacheCreationPerMillion when separate. Multiple books may be supplied as an array or pricebooks array.\nOld or undated records are relative comparisons only. Missing schema/tokenizer is incomplete; strategy reports contain no actual-rate/cost or cache-residency prediction. Original actual usage/cost is counted once.");
                return Ok(());
            }
            _ => return Err("Unknown option; use --help".into()),
        }
    }
    let input=input.ok_or("Provide --input; no production data path is discovered automatically")?;
    if input.extension().is_none_or(|extension| extension!="jsonl") { return Err("Only an explicit copied .jsonl fixture is accepted; databases are never opened".into()); }
    if output.as_ref().is_some_and(|output| output==&input || output.exists()) { return Err("Output must be a new file, distinct from input".into()); }
    let prices=price_path.map(|path| {
        let file=File::open(path).map_err(|_|"Could not open explicit price JSON")?;
        if file.metadata().map_err(|_|"Could not inspect explicit price JSON")?.len()>1024*1024 { return Err("Explicit price JSON exceeds 1 MiB"); }
        serde_json::from_reader::<_,Value>(file).map_err(|_|"Invalid explicit price JSON")
    }).transpose()?;
    let file=File::open(input).map_err(|_|"Could not open the explicit offline JSONL input")?;
    let mut reader=BufReader::new(file);
    let mut analyzer=ReplayAnalyzer::default(); let mut line=Vec::new(); let mut line_number=0; let mut manifest_seen=false;
    loop {
        line.clear();
        let bytes=std::io::Read::by_ref(&mut reader).take(MAXIMUM_LINE_BYTES+1).read_until(b'\n',&mut line)
            .map_err(|_|"Could not read offline JSONL")?;
        if bytes==0 { break; }
        line_number+=1;
        if bytes as u64>MAXIMUM_LINE_BYTES { return Err(format!("JSONL line {line_number} exceeds the development reader limit")); }
        if line.iter().all(u8::is_ascii_whitespace) { continue; }
        let row:Value=serde_json::from_slice(&line).map_err(|_|format!("Invalid JSON at line {line_number}; contents are not logged"))?;
        if !manifest_seen {
            if row["type"]!="kynxa.cache-replay.manifest" || row["schemaVersion"]!=1 || row["offlineCopy"]!=true {
                return Err("First line must declare a schemaVersion 1 offlineCopy manifest; never point at live formal data".into());
            }
            manifest_seen=true;
        } else { analyzer.observe(&row,prices.as_ref()); }
    }
    if !manifest_seen { return Err("Offline input is empty or lacks its manifest".into()); }
    let report=serde_json::to_vec_pretty(&analyzer.report()).map_err(|_|"Could not serialize the public diagnostic report")?;
    if let Some(path)=output {
        use std::io::Write;
        let mut file=File::options().create_new(true).write(true).open(path).map_err(|_|"Could not create a new report file")?;
        file.write_all(&report).map_err(|_|"Could not write diagnostic report")?;
    } else { println!("{}",String::from_utf8_lossy(&report)); }
    Ok(())
}

fn main() {
    if let Err(error)=run() { eprintln!("{error}"); std::process::exit(2); }
}
