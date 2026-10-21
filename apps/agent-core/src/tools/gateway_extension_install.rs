//! Explicit extension installation reuses owned host jobs, receipts, configuration and Skill import.
//! 显式扩展安装复用宿主作业、正式回执、配置与 Skill 导入；启动时不下载依赖。

use super::gateway::{self, GatewayToolContext, failure};
use crate::gateway_contract::GatewayError;
use serde_json::{Value,json};
use std::path::Path;

pub(crate) fn supports(name:&str)->bool { matches!(name,"extension.install"|"extension.finish"|"extension.configure") }
pub(crate) fn descriptors()->Vec<Value> {
    vec![
        json!({"name":"extension.install","source":"builtin","description":"Explicitly install a requested MCP or Skill using an existing trusted host terminal job. Never used on application startup. Supply a pinned version, its real HTTPS source, and the reviewed PowerShell installation script. The configured extension root is returned in the receipt and supplied as KYNXA_EXTENSION_INSTALL_ROOT. Installers run with existing host approval and resource admission. This starts one owned job; use extension.finish with this call's actual resultRef until verified. Never rerun a pending/unknown installation.","inputSchema":{"type":"object","properties":{"kind":{"type":"string","enum":["mcp","skill"]},"scope":{"type":"string","enum":["project","global"],"default":"project","description":"Skill installation scope. Project by default; global requires an explicit user-approved import and does not imply runtime verification."},"id":{"type":"string","pattern":"^[a-z][a-z0-9-]{1,39}$"},"sourceUrl":{"type":"string","minLength":1,"maxLength":8192},"version":{"type":"string","minLength":1,"maxLength":128},"script":{"type":"string","minLength":1,"maxLength":15000},"server":{"type":"object"},"directory":{"type":"string","minLength":1,"maxLength":4096},"expectedFingerprint":{"type":["string","null"]},"reason":{"type":"string","minLength":1,"maxLength":2000}},"required":["kind","id","sourceUrl","version","script","expectedFingerprint","reason"],"additionalProperties":false}}),
        json!({"name":"extension.finish","source":"builtin","description":"Observe the original extension.install job by its private archived resultRef. A running/unknown job is never restarted. Only an actual completed zero-exit receipt can proceed to normalized MCP configuration and a real handshake, or the existing checked Skill package import. Repeating finish after success is idempotent for an unchanged installation.","inputSchema":{"type":"object","properties":{"resultRef":{"type":"object","properties":{"id":{"type":"string","minLength":1},"sha256":{"type":"string"},"bytes":{"type":"integer","minimum":0}},"required":["id"],"additionalProperties":false},"reason":{"type":"string","minLength":1,"maxLength":2000}},"required":["resultRef","reason"],"additionalProperties":false}}),
        json!({"name":"extension.configure","source":"builtin","description":"Register and verify an already installed MCP server without downloading anything, or import an existing checked Skill directory through the standard Skill importer. Supply expectedFingerprint:null only for a new MCP ID; updating an existing ID requires its current configurationFingerprint from discovery. Personal OAuth remains a user browser login. Connection failure is reported as configured-but-not-ready, never as success.","inputSchema":{"type":"object","properties":{"kind":{"type":"string","enum":["mcp","skill"]},"scope":{"type":"string","enum":["project","global"],"default":"project","description":"Skill installation scope. Project by default; global requires an explicit user-approved import and does not imply runtime verification."},"server":{"type":"object"},"directory":{"type":"string","minLength":1,"maxLength":4096},"expectedFingerprint":{"type":["string","null"]},"reason":{"type":"string","minLength":1,"maxLength":2000}},"required":["kind","expectedFingerprint","reason"],"additionalProperties":false}}),
    ]
}
fn validated(args:&Value)->Result<Value,GatewayError> {
    match args["kind"].as_str() {
        Some("mcp")=>super::service_config::normalize_server(&args["server"])
            .map_err(|e|failure(&e.code,&e.message,e.status_code)),
        Some("skill") if args["directory"].as_str().is_some_and(|s|Path::new(s).is_absolute())=>Ok(Value::Null),
        _=>Err(failure("INVALID_TOOL_ARGUMENTS","请提供 MCP 配置或 Skill 的完整绝对目录。",400)),
    }
}
async fn configure(args:&Value,ctx:&GatewayToolContext)->Result<Value,GatewayError> {
    let server=validated(args)?;
    if args["kind"]=="skill" {
        let result=gateway::import_skill(json!({"directory":args["directory"],"scope":args["scope"].as_str().unwrap_or("project")}),ctx).await?;
        gateway::invalidate_skill_metadata(ctx);
        return Ok(json!({"status":"installed","kind":"skill","verification":result,"runtimeVerified":false}));
    }
    // Installed runtime validation is performed before saving; npm/npx bootstrap is never a startup command.
    // 保存前核对已安装运行时；npm/npx 引导安装不能成为启动命令。
    if server["transport"]=="stdio" { super::gateway_mcp::validate_runtime(&server)?; }
    let mut input=gateway::get_config(ctx).await?;
    let mut servers=input["mcpServers"].as_array().cloned().unwrap_or_default();
    let position=servers.iter().position(|s|s["id"]==server["id"]);
    if let Some(index)=position {
        let current=super::gateway_tool_memory::server_fingerprint(&servers[index]);
        let existing=super::service_config::normalize_server(&servers[index]).map_err(|e|failure(&e.code,&e.message,e.status_code))?;
        if existing!=server && args["expectedFingerprint"]!=current {
            return Err(failure("MCP_CONFIGURATION_CONFLICT","已有 MCP 配置已改变；请读取当前指纹后再更新。",409));
        }
        if existing!=server {servers[index]=server.clone();}
    } else {
        if !args["expectedFingerprint"].is_null() {return Err(failure("MCP_CONFIGURATION_CONFLICT","原 MCP 配置已不存在，未按过期指纹新建。",409));}
        servers.push(server.clone());
    }
    let changed=input["mcpServers"]!=json!(servers);
    let mut current=ctx.clone();
    if changed {
        input["expectedRevision"]=input["revision"].clone();input["mcpServers"]=json!(servers);
        current.config=gateway::update_config(input,ctx).await?;
    } else {current.config=input;}
    let server=current.config["mcpServers"].as_array().into_iter().flatten().find(|s|s["id"]==server["id"]).cloned().unwrap_or(server);
    let new_fingerprint=super::gateway_tool_memory::server_fingerprint(&server);
    match super::gateway_mcp::connection(&server,&current).await {
        Ok(client)=> {
            let count=client.lock().await.tools.len();
            let _=super::gateway_mcp::catalog_selected(&current,vec![server.clone()]).await?;
            Ok(json!({"status":"installed","kind":"mcp","serverId":server["id"],"configurationFingerprint":new_fingerprint,"ready":true,"toolCount":count}))
        },
        Err(error)=>Ok(json!({"status":"configured","kind":"mcp","serverId":server["id"],"configurationFingerprint":new_fingerprint,"ready":false,"code":error.code,"message":error.message,"requiresUserLogin":server["auth"]["type"]=="oauth-authorization-code"})),
    }
}
pub(crate) async fn execute(name:&str,args:Value,ctx:&GatewayToolContext)->Result<Value,GatewayError> {
    if name=="extension.configure" {return configure(&args,ctx).await;}
    if name=="extension.install" {
        validated(&args)?;
        let id=args["id"].as_str().unwrap_or("");
        if !regex::Regex::new(r"^[a-z][a-z0-9-]{1,39}$").unwrap().is_match(id) {return Err(failure("INVALID_TOOL_ARGUMENTS","扩展安装 ID 无效。",400));}
        let source=url::Url::parse(args["sourceUrl"].as_str().unwrap_or("")).map_err(|_|failure("INVALID_TOOL_ARGUMENTS","请提供真实的 HTTPS 安装来源。",400))?;
        if source.scheme()!="https" || !source.username().is_empty() || source.password().is_some() {return Err(failure("INVALID_TOOL_ARGUMENTS","安装来源必须使用 HTTPS，凭据不能放入 URL。",400));}
        let root=ctx.extension_root.join("Install").join(id);
        super::local_files::ensure_directory(&root).map_err(|e|failure(&e.code,&e.message,e.status_code))?;
        let quoted=super::local_files::path_text(&root).replace('\'',"''");
        let script=format!("$env:KYNXA_EXTENSION_INSTALL_ROOT = '{quoted}'\r\n{}",args["script"].as_str().unwrap_or(""));
        let job=super::gateway_process::execute("terminal.host.start",json!({"shell":"powershell","script":script,"cwd":root,"timeoutMs":21600000,"visible":false,"reason":args["reason"]}),ctx).await?;
        let Some(job_id)=job["jobId"].as_str().filter(|s|!s.is_empty()) else {return Ok(json!({"status":"unknown","isError":true,"code":"EXTENSION_INSTALL_UNKNOWN","job":job,"message":"安装启动状态尚未核验，不要重复执行。"}));};
        let handle=json!({"kind":"extension-install","conversationId":ctx.conversation_id,"toolCallId":ctx.config["activeToolCallId"],"jobId":job_id,"arguments":args,"installRoot":root});
        return Ok(json!({"status":"installing","recoveryHandle":handle,"job":job,"nextAction":"Use extension.finish with this call's original resultRef; never run the installer again while pending/unknown."}));
    }
    let root=ctx.data_home.clone();let conversation=ctx.conversation_id.clone();let reference=args["resultRef"].clone();
    let archived=tokio::task::spawn_blocking(move ||crate::storage::gateway::load_private_tool_result(&root,&conversation,reference["id"].as_str().unwrap_or("")))
        .await.map_err(|_|failure("EXTENSION_ARCHIVE_FAILED","安装回执无法读取。",503))??;
    if args["resultRef"]["sha256"].as_str().is_some_and(|hash|archived["sha256"]!=hash) {return Err(failure("EXTENSION_ARCHIVE_CHANGED","原安装回执指纹不匹配。",409));}
    let handle=&archived["canonical"]["recoveryHandle"];
    if handle["kind"]!="extension-install" || handle["conversationId"]!=ctx.conversation_id || handle["toolCallId"]!=archived["toolCallId"] {return Err(failure("EXTENSION_INSTALL_SCOPE_INVALID","不是此聊天的原安装回执。",403));}
    let job=super::gateway_process::execute("terminal.host.read",json!({"jobId":handle["jobId"],"offset":0,"limit":1000}),ctx).await;
    let job=match job {Ok(value)=>value,Err(error)=>return Ok(json!({"status":"unknown","recoveryHandle":handle,"code":error.code,"message":"原安装作业暂不可核验，未重跑安装器。"}))};
    if job["status"]!="completed" {return Ok(json!({"status":if ["starting","running"].iter().any(|s|job["status"]==*s){"installing"}else{"unknown"},"job":job,"recoveryHandle":handle}));}
    if job["receipt"]["completed"]!=true || job["receipt"]["exitCode"]!=0 || job["receipt"]["isError"]==true {
        return Ok(json!({"status":"failed","isError":true,"job":job,"recoveryHandle":handle,"message":"安装命令未取得完整零退出码回执，未启用扩展。"}));
    }
    configure(&handle["arguments"],ctx).await
}

#[cfg(test)] mod tests {
    use super::*;
    #[test] fn remote_configuration_requires_valid_auth_and_installer_directory_is_explicit() {
        assert!(validated(&json!({"kind":"skill","directory":"relative"})).is_err());
        assert!(validated(&json!({"kind":"mcp","server":{"id":"sample","name":"Sample","enabled":true,"transport":"streamable-http","url":"https://mcp.example/rpc","auth":{"type":"oauth-authorization-code","clientId":"public-client"}}})).is_ok());
        assert!(descriptors().iter().all(|v|v["inputSchema"]["additionalProperties"]==false));
    }
    #[tokio::test] async fn configuration_verifies_actual_tools_is_idempotent_and_rejects_stale_overwrite() {
        use axum::{Json,Router,routing::post};
        async fn handler(Json(request):Json<Value>)->Json<Value> {
            let result=match request["method"].as_str().unwrap_or("") {
                "initialize"=>json!({"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"installed-fixture","version":"1"}}),
                "tools/list"=>json!({"tools":[{"name":"echo","inputSchema":{"type":"object"}}]}),
                _=>json!({}),
            };
            Json(json!({"jsonrpc":"2.0","id":request["id"],"result":result}))
        }
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let address=listener.local_addr().unwrap();
        let service=tokio::spawn(async move{axum::serve(listener,Router::new().route("/mcp",post(handler))).await.unwrap();});
        let (ctx,_keep)=gateway::test_context();let root=ctx.workdir.parent().unwrap().to_owned();
        let mut args=json!({"kind":"mcp","expectedFingerprint":null,"server":{"id":"installed-fixture","name":"Installed fixture","enabled":true,"transport":"streamable-http","url":format!("http://{address}/mcp")}});
        let result=configure(&args,&ctx).await.unwrap();assert_eq!(result["ready"],true);assert_eq!(result["toolCount"],1);
        let first=gateway::get_config(&ctx).await.unwrap();
        let repeated=configure(&args,&ctx).await.unwrap();assert_eq!(repeated["ready"],true);
        assert_eq!(gateway::get_config(&ctx).await.unwrap()["revision"],first["revision"]);
        args["server"]["url"]=json!(format!("http://{address}/changed"));
        assert_eq!(configure(&args,&ctx).await.unwrap_err().code,"MCP_CONFIGURATION_CONFLICT");
        assert_eq!(gateway::get_config(&ctx).await.unwrap()["revision"],first["revision"]);
        super::super::gateway_mcp::shutdown(&ctx).await;service.abort();std::fs::remove_dir_all(root).unwrap();
    }
}
