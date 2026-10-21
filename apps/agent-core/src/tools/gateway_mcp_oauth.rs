//! Personal MCP OAuth uses loopback PKCE and current-user encrypted credentials.
//! 个人 MCP OAuth 使用回环 PKCE 与当前用户加密凭据，不把令牌写入模型上下文或配置。

use super::gateway::{GatewayToolContext, failure};
use crate::gateway_contract::GatewayError;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, path::PathBuf, sync::{Arc, OnceLock}, time::Duration};
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::TcpListener, sync::{Mutex, watch}};

struct Login {
    server: String,
    root: PathBuf,
    expires: i64,
    state: watch::Receiver<Value>,
    cancel: watch::Sender<bool>,
}
static LOGINS: OnceLock<Mutex<HashMap<String, Login>>> = OnceLock::new();
static TOKEN_GATES: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();
fn logins() -> &'static Mutex<HashMap<String, Login>> { LOGINS.get_or_init(Default::default) }
fn now() -> i64 { chrono::Utc::now().timestamp() }
fn random() -> Result<String, GatewayError> {
    super::local_files::random_identity().map(|s| s.replace('-', "")).map_err(|_| failure("MCP_OAUTH_RANDOM_FAILED", "无法生成安全授权随机数。", 500))
}
fn key(server: &Value) -> String {
    format!("{:x}", Sha256::digest(serde_json::to_vec(&json!({"url":server["url"],"auth":server["auth"]})).unwrap()))
}
fn credential_path(server: &Value, ctx: &GatewayToolContext) -> PathBuf {
    ctx.extension_root.join("Credentials").join("McpOAuth").join(format!("{}.json", key(server)))
}
fn configured(id: &str, ctx: &GatewayToolContext) -> Result<Value, GatewayError> {
    super::gateway_mcp::servers(ctx).into_iter().find(|s| s["id"] == id && s["auth"]["type"] == "oauth-authorization-code")
        .ok_or_else(|| failure("MCP_OAUTH_NOT_CONFIGURED", "此服务未配置个人 OAuth 登录。", 400))
}
fn safe_endpoint(value: &str) -> Result<url::Url, GatewayError> {
    let endpoint = url::Url::parse(value).map_err(|_| failure("MCP_OAUTH_METADATA_INVALID", "OAuth 地址无效。", 400))?;
    if endpoint.scheme() != "https" || !endpoint.username().is_empty() || endpoint.password().is_some() || endpoint.fragment().is_some() {
        return Err(failure("MCP_OAUTH_METADATA_INVALID", "OAuth 服务端地址必须使用 HTTPS 且不能包含用户信息或片段。", 400));
    }
    Ok(endpoint)
}
fn client() -> Result<reqwest::Client, GatewayError> {
    reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).timeout(Duration::from_secs(30)).build()
        .map_err(|_| failure("MCP_HTTP_FAILED", "无法初始化 OAuth HTTP 客户端。", 500))
}
async fn body(response: reqwest::Response) -> Result<Value, GatewayError> {
    use futures_util::StreamExt;
    let response = response.error_for_status().map_err(|_| failure("MCP_OAUTH_REJECTED", "OAuth 服务拒绝了请求，请检查登录与客户端配置。", 401))?;
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(part) = stream.next().await {
        let part = part.map_err(|_| failure("MCP_OAUTH_NETWORK", "OAuth 响应读取失败。", 502))?;
        if bytes.len().saturating_add(part.len()) > 256 * 1024 { return Err(failure("MCP_OAUTH_METADATA_INVALID", "OAuth 响应超过安全大小。", 502)); }
        bytes.extend_from_slice(&part);
    }
    serde_json::from_slice(&bytes).map_err(|_| failure("MCP_OAUTH_METADATA_INVALID", "OAuth 响应不是有效 JSON。", 502))
}
async fn get_json(client: &reqwest::Client, url: url::Url) -> Result<Value, GatewayError> {
    body(client.get(url).send().await.map_err(|_| failure("MCP_OAUTH_NETWORK", "OAuth 服务暂时无法连接。", 502))?).await
}
fn well_known(issuer: &url::Url, name: &str, appended: bool) -> url::Url {
    let mut result = issuer.clone();
    let path = issuer.path().trim_end_matches('/');
    result.set_path(&if appended { format!("{path}/.well-known/{name}") } else { format!("/.well-known/{name}{path}") });
    result.set_query(None);
    result
}
async fn discover(server: &Value) -> Result<Value, GatewayError> {
    let resource = url::Url::parse(server["url"].as_str().unwrap_or(""))
        .map_err(|_|failure("MCP_OAUTH_METADATA_INVALID","MCP 资源地址无效。",400))?;
    let local_http=resource.scheme()=="http" && resource.host_str().is_some_and(|host|host=="127.0.0.1" || host=="[::1]" || host=="localhost");
    if resource.scheme()!="https" && !local_http {return Err(failure("MCP_OAUTH_METADATA_INVALID","OAuth 资源必须使用 HTTPS 或本机回环地址。",400));}
    let http = client()?;
    let challenge = http.get(resource.clone()).send().await.map_err(|_| failure("MCP_OAUTH_NETWORK", "无法连接 MCP 服务以发现授权配置。", 502))?;
    let authenticate = challenge.headers().get(reqwest::header::WWW_AUTHENTICATE).and_then(|v| v.to_str().ok()).unwrap_or("");
    let metadata_header = regex::Regex::new(r#"(?:^|[ ,])resource_metadata="([^"]+)""#).unwrap().captures(authenticate).map(|c| c[1].to_owned());
    let challenged_scope = regex::Regex::new(r#"(?:^|[ ,])scope="([^"]+)""#).unwrap().captures(authenticate).map(|c| c[1].to_owned());
    let urls = if let Some(url) = metadata_header { vec![safe_endpoint(&url)?] } else {
        let mut root = resource.clone(); root.set_path("/"); root.set_query(None);
        vec![well_known(&resource, "oauth-protected-resource", false), well_known(&root, "oauth-protected-resource", false)]
    };
    let mut protected = None;
    for url in urls { if let Ok(value) = get_json(&http, url).await { protected = Some(value); break; } }
    let protected = protected.ok_or_else(|| failure("MCP_OAUTH_DISCOVERY_FAILED", "服务未提供有效的 OAuth 受保护资源元数据。", 401))?;
    let declared = url::Url::parse(protected["resource"].as_str().unwrap_or(""))
        .map_err(|_|failure("MCP_OAUTH_METADATA_INVALID","授权资源地址无效。",400))?;
    if declared != resource { return Err(failure("MCP_OAUTH_RESOURCE_MISMATCH", "授权资源与配置的 MCP 地址不一致。", 403)); }
    let issuers = protected["authorization_servers"].as_array().ok_or_else(|| failure("MCP_OAUTH_DISCOVERY_FAILED", "服务未声明授权服务器。", 401))?;
    let configured_issuer = server["auth"]["issuer"].as_str().filter(|s| !s.trim().is_empty());
    let issuer_text = configured_issuer.or_else(|| issuers.first().and_then(Value::as_str)).ok_or_else(|| failure("MCP_OAUTH_DISCOVERY_FAILED", "服务没有可用授权服务器。", 401))?;
    let issuer = safe_endpoint(issuer_text)?;
    if !issuers.iter().filter_map(Value::as_str).any(|s| url::Url::parse(s).ok().as_ref() == Some(&issuer)) {
        return Err(failure("MCP_OAUTH_ISSUER_MISMATCH", "配置的 issuer 不属于此 MCP 服务声明的授权服务器。", 403));
    }
    let mut metadata = None;
    for url in [well_known(&issuer,"oauth-authorization-server",false),well_known(&issuer,"openid-configuration",false),well_known(&issuer,"openid-configuration",true)] {
        if let Ok(value) = get_json(&http,url).await { if value["issuer"].as_str().and_then(|s| url::Url::parse(s).ok()).as_ref() == Some(&issuer) { metadata = Some(value); break; } }
    }
    let metadata = metadata.ok_or_else(|| failure("MCP_OAUTH_DISCOVERY_FAILED", "授权服务器元数据缺失或 issuer 不匹配。", 401))?;
    if !metadata["code_challenge_methods_supported"].as_array().is_some_and(|a| a.iter().any(|v| v == "S256")) {
        return Err(failure("MCP_OAUTH_PKCE_REQUIRED", "授权服务器未声明安全的 PKCE S256 支持。", 400));
    }
    let authorization = safe_endpoint(metadata["authorization_endpoint"].as_str().unwrap_or(""))?;
    let token = safe_endpoint(metadata["token_endpoint"].as_str().unwrap_or(""))?;
    let scope = server["auth"]["scope"].as_str().map(str::to_owned).or(challenged_scope).unwrap_or_else(|| protected["scopes_supported"].as_array().into_iter().flatten().filter_map(Value::as_str).collect::<Vec<_>>().join(" "));
    Ok(json!({"issuer":issuer.as_str(),"authorizationEndpoint":authorization.as_str(),"tokenEndpoint":token.as_str(),"resource":resource.as_str(),"scope":scope,"clientId":server["auth"]["clientId"]}))
}

#[cfg(windows)]
fn protect(bytes: &[u8], entropy: &[u8], decrypt: bool) -> Result<Vec<u8>, GatewayError> {
    #[repr(C)] struct Blob { len: u32, data: *mut u8 }
    #[link(name="crypt32")] unsafe extern "system" {
        fn CryptProtectData(input:*const Blob, description:*const u16, entropy:*const Blob, reserved:*mut std::ffi::c_void, prompt:*mut std::ffi::c_void, flags:u32, output:*mut Blob)->i32;
        fn CryptUnprotectData(input:*const Blob, description:*mut *mut u16, entropy:*const Blob, reserved:*mut std::ffi::c_void, prompt:*mut std::ffi::c_void, flags:u32, output:*mut Blob)->i32;
    }
    #[link(name="kernel32")] unsafe extern "system" { fn LocalFree(value:*mut std::ffi::c_void)->*mut std::ffi::c_void; }
    let input = Blob { len: bytes.len() as u32, data: bytes.as_ptr() as *mut u8 };
    let entropy = Blob { len: entropy.len() as u32, data: entropy.as_ptr() as *mut u8 };
    let mut output = Blob { len:0, data:std::ptr::null_mut() };
    // DPAPI forbids interactive prompts; ciphertext is bound to the current Windows user and this configuration.
    // 禁止 DPAPI 交互弹窗；密文绑定当前 Windows 用户与此服务配置。
    let ok = unsafe { if decrypt { CryptUnprotectData(&input,std::ptr::null_mut(),&entropy,std::ptr::null_mut(),std::ptr::null_mut(),1,&mut output) } else { CryptProtectData(&input,std::ptr::null(),&entropy,std::ptr::null_mut(),std::ptr::null_mut(),1,&mut output) } };
    if ok == 0 { return Err(failure("MCP_OAUTH_CREDENTIAL_LOCKED", "Windows 无法保护或解锁此用户的 OAuth 凭据。", 401)); }
    let result = unsafe { std::slice::from_raw_parts(output.data,output.len as usize).to_vec() };
    unsafe { LocalFree(output.data.cast()); }
    Ok(result)
}
#[cfg(not(windows))]
fn protect(_: &[u8], _: &[u8], _: bool) -> Result<Vec<u8>, GatewayError> { Err(failure("MCP_OAUTH_CREDENTIAL_UNSUPPORTED", "当前系统未配置受保护的 OAuth 凭据存储。", 501)) }
fn load(server:&Value,ctx:&GatewayToolContext)->Result<Value,GatewayError> {
    let path=credential_path(server,ctx);
    super::local_files::inspect(&path,true,false).map_err(|_|failure("MCP_OAUTH_CREDENTIAL_LOCKED","OAuth 凭据路径不安全。",403))?;
    let bytes=super::local_files::read_checked(&path,512*1024,false,"MCP_OAUTH_CREDENTIAL_CHANGED")
        .map_err(|_|failure("MCP_OAUTH_LOGIN_REQUIRED","请先点击登录完成浏览器授权。",401))?;
    let value:Value=serde_json::from_slice(&bytes).map_err(|_|failure("MCP_OAUTH_CREDENTIAL_LOCKED","OAuth 凭据文件异常。",401))?;
    let encrypted=URL_SAFE_NO_PAD.decode(value["protected"].as_str().unwrap_or("")).map_err(|_|failure("MCP_OAUTH_CREDENTIAL_LOCKED","OAuth 密文无效。",401))?;
    serde_json::from_slice(&protect(&encrypted,key(server).as_bytes(),true)?).map_err(|_|failure("MCP_OAUTH_CREDENTIAL_LOCKED","OAuth 凭据无法解析。",401))
}
fn save(server:&Value,ctx:&GatewayToolContext,value:&Value)->Result<(),GatewayError> {
    let encrypted=protect(&serde_json::to_vec(value).unwrap(),key(server).as_bytes(),false)?;
    super::local_files::atomic_json(&credential_path(server,ctx),&json!({"version":1,"protected":URL_SAFE_NO_PAD.encode(encrypted)}))
        .map_err(|_|failure("MCP_OAUTH_CREDENTIAL_SAVE_FAILED","OAuth 凭据未能安全保存。",500))
}
async fn token_exchange(metadata:&Value,fields:&[(&str,String)],previous:Option<&Value>)->Result<Value,GatewayError> {
    let endpoint=safe_endpoint(metadata["tokenEndpoint"].as_str().unwrap_or(""))?;
    let value=body(client()?.post(endpoint).form(fields).send().await.map_err(|_|failure("MCP_OAUTH_NETWORK","OAuth 令牌交换失败。",502))?).await?;
    if !value["token_type"].as_str().is_some_and(|s|s.eq_ignore_ascii_case("bearer")) || !value["access_token"].as_str().is_some_and(|s|!s.is_empty() && s.len()<=65536 && !s.contains(['\r','\n'])) {
        return Err(failure("MCP_OAUTH_TOKEN_INVALID","授权服务器返回了无效的 Bearer 令牌。",401));
    }
    let expires=value["expires_in"].as_u64().unwrap_or(3600).min(365*86400);
    let refresh=value.get("refresh_token").cloned().or_else(||previous.and_then(|v|v.get("refreshToken").cloned())).unwrap_or(Value::Null);
    Ok(json!({"accessToken":value["access_token"],"refreshToken":refresh,"expiresAt":now().saturating_add(expires as i64),"metadata":metadata}))
}
async fn token_gate(server:&Value,ctx:&GatewayToolContext)->Arc<Mutex<()>> {
    let id=format!("{}:{}",ctx.extension_root.display(),key(server));
    let mut gates=TOKEN_GATES.get_or_init(Default::default).lock().await;
    gates.retain(|_,gate|Arc::strong_count(gate)>1);
    gates.entry(id).or_default().clone()
}
pub(crate) async fn token(server:&Value,ctx:&GatewayToolContext)->Result<String,GatewayError> {
    let gate=token_gate(server,ctx).await; let _guard=gate.lock().await;
    let mut stored=load(server,ctx)?;
    if stored["expiresAt"].as_i64().unwrap_or(0)<=now()+30 {
        let refresh=stored["refreshToken"].as_str().filter(|s|!s.is_empty()).ok_or_else(||failure("MCP_OAUTH_LOGIN_REQUIRED","登录已过期，请重新进行浏览器授权。",401))?;
        let metadata=&stored["metadata"];
        stored=token_exchange(metadata,&[("grant_type","refresh_token".to_owned()),("refresh_token",refresh.to_owned()),("client_id",metadata["clientId"].as_str().unwrap_or("").to_owned()),("resource",metadata["resource"].as_str().unwrap_or("").to_owned())],Some(&stored)).await?;
        save(server,ctx,&stored)?;
    }
    stored["accessToken"].as_str().map(str::to_owned).ok_or_else(||failure("MCP_OAUTH_LOGIN_REQUIRED","OAuth 登录记录缺少有效令牌。",401))
}

async fn callback(listener:TcpListener,path:String,state:String,mut cancel:watch::Receiver<bool>)->Result<String,GatewayError> {
    let deadline=tokio::time::Instant::now()+Duration::from_secs(600);
    loop {
        let (mut socket,_)=tokio::select! {
            item=listener.accept()=>item.map_err(|_|failure("MCP_OAUTH_CALLBACK_FAILED","本机 OAuth 回调无法接收。",500))?,
            _=tokio::time::sleep_until(deadline)=>return Err(failure("MCP_OAUTH_EXPIRED","授权等待已过期，请重新登录。",408)),
            _=cancel.changed()=>return Err(failure("MCP_OAUTH_CANCELLED","授权已取消。",409)),
        };
        let read=tokio::time::timeout(Duration::from_secs(5),async {
            let mut bytes=Vec::new();let mut part=[0u8;2048];
            loop {
                let size=socket.read(&mut part).await?;
                if size==0 || bytes.len().saturating_add(size)>16384 {return Err(std::io::Error::new(std::io::ErrorKind::InvalidData,"Invalid callback HTTP headers"));}
                bytes.extend_from_slice(&part[..size]);
                if bytes.windows(4).any(|v|v==b"\r\n\r\n") {return Ok::<_,std::io::Error>(String::from_utf8_lossy(&bytes).into_owned());}
            }
        }).await;
        let request=match read {Ok(Ok(request))=>request,_=>continue};
        let target=request.lines().next().and_then(|line|line.strip_prefix("GET ")).and_then(|s|s.split_once(' ').map(|v|v.0));
        let url=target.and_then(|s|url::Url::parse(&format!("http://127.0.0.1{s}")).ok());
        let mut fields=url.as_ref().map(|u|u.query_pairs().into_owned().collect::<HashMap<_,_>>()).unwrap_or_default();
        let valid=url.as_ref().is_some_and(|u|u.path()==path) && fields.get("state")==Some(&state);
        let message=if valid {"Authorization received. You can return to KYNXA."} else {"Invalid authorization callback."};
        let response=format!("HTTP/1.1 {}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{}",if valid {"200 OK"} else {"400 Bad Request"},message.len(),message);
        let _=socket.write_all(response.as_bytes()).await;
        if !valid { continue; }
        if fields.contains_key("error") {return Err(failure("MCP_OAUTH_DENIED","浏览器授权未被允许。",401));}
        return fields.remove("code").filter(|s|!s.is_empty() && s.len()<=8192).ok_or_else(||failure("MCP_OAUTH_CALLBACK_FAILED","授权回调缺少有效代码。",400));
    }
}
pub(crate) async fn start(id:&str,ctx:&GatewayToolContext)->Result<Value,GatewayError> {
    let server=configured(id,ctx)?;
    let client_id=server["auth"]["clientId"].as_str().filter(|s|!s.trim().is_empty()).ok_or_else(||failure("MCP_OAUTH_CLIENT_REQUIRED","请先填写服务商提供的 OAuth 客户端 ID。",400))?;
    let metadata=discover(&server).await?;
    let listener=TcpListener::bind((std::net::Ipv4Addr::LOCALHOST,0)).await.map_err(|_|failure("MCP_OAUTH_CALLBACK_FAILED","无法创建本机授权回调。",500))?;
    let path=format!("/oauth/callback/{}",random()?);
    let redirect=format!("http://127.0.0.1:{}{path}",listener.local_addr().map_err(|_|failure("MCP_OAUTH_CALLBACK_FAILED","无法读取回调端口。",500))?.port());
    let verifier=format!("{}{}",random()?,random()?);
    let challenge=URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let state=random()?; let authorization_id=random()?;
    let mut url=safe_endpoint(metadata["authorizationEndpoint"].as_str().unwrap_or(""))?;
    url.query_pairs_mut().extend_pairs([("response_type","code"),("client_id",client_id),("redirect_uri",redirect.as_str()),("state",state.as_str()),("code_challenge",challenge.as_str()),("code_challenge_method","S256"),("resource",metadata["resource"].as_str().unwrap_or("")),("scope",metadata["scope"].as_str().unwrap_or(""))]);
    let (sender,receiver)=watch::channel(json!({"state":"pending"}));
    let (cancel_tx,cancel)=watch::channel(false); let expires=now()+600;
    let mut sessions=logins().lock().await;
    sessions.retain(|_,login| { let retain=login.expires>now() && !(login.server==id && login.root==ctx.extension_root); if !retain {let _=login.cancel.send(true);} retain });
    if sessions.len()>=128 {return Err(failure("MCP_OAUTH_BUSY","当前授权等待较多，请稍后重试。",429));}
    sessions.insert(authorization_id.clone(),Login{server:id.to_owned(),root:ctx.extension_root.clone(),expires,state:receiver,cancel:cancel_tx});
    drop(sessions);
    let owned=ctx.clone(); let callback_redirect=redirect.clone();
    tokio::spawn(async move {
        let cancelled=cancel.clone();
        let result=async {
            let code=callback(listener,path,state,cancel).await?;
            let token=token_exchange(&metadata,&[("grant_type","authorization_code".to_owned()),("code",code),("redirect_uri",callback_redirect),("client_id",metadata["clientId"].as_str().unwrap_or("").to_owned()),("code_verifier",verifier),("resource",metadata["resource"].as_str().unwrap_or("").to_owned())],None).await?;
            let gate=token_gate(&server,&owned).await; let guard=gate.lock().await;
            // Configuration identity is checked again before credential persistence.
            // 持久化前重新核对配置身份，过期登录不能覆盖另一服务配置。
            let current=super::gateway::get_config(&owned).await?;
            if !current["mcpServers"].as_array().into_iter().flatten().any(|s|s["id"]==server["id"] && key(s)==key(&server)) {return Err(failure("MCP_OAUTH_CONFIG_CHANGED","服务配置已更改，请重新登录。",409));}
            if *cancelled.borrow() {return Err(failure("MCP_OAUTH_CANCELLED","授权已取消。",409));}
            save(&server,&owned,&token)?;
            drop(guard);
            super::gateway_mcp::restart(&server,&owned).await?;
            Ok::<_,GatewayError>(())
        }.await;
        let value=match result { Ok(())=>json!({"state":"connected"}),Err(error)=>json!({"state":if error.code=="MCP_OAUTH_EXPIRED" {"expired"} else {"failed"},"code":error.code,"message":error.message}) };
        let _=sender.send(value);
    });
    Ok(json!({"authorizationId":authorization_id,"authorizationUrl":url.as_str(),"expiresAt":chrono::DateTime::from_timestamp(expires,0).map(|v|v.to_rfc3339()),"redirectUri":redirect}))
}
pub(crate) async fn status(id:&str,authorization:&str,ctx:&GatewayToolContext)->Result<Value,GatewayError> {
    let sessions=logins().lock().await;
    let login=sessions.get(authorization).filter(|v|v.server==id && v.root==ctx.extension_root).ok_or_else(||failure("MCP_OAUTH_NOT_FOUND","没有此授权会话。",404))?;
    let value=login.state.borrow().clone();
    Ok(if value["state"]=="pending" && login.expires<=now() {json!({"state":"expired"})} else {value})
}
pub(crate) async fn logout(id:&str,ctx:&GatewayToolContext)->Result<Value,GatewayError> {
    let server=configured(id,ctx)?;
    { let mut sessions=logins().lock().await; sessions.retain(|_,v| {if v.server==id && v.root==ctx.extension_root {let _=v.cancel.send(true);false} else {true}}); }
    let gate=token_gate(&server,ctx).await; let guard=gate.lock().await;
    let path=credential_path(&server,ctx);
    super::local_files::inspect(&path,true,false).map_err(|_|failure("MCP_OAUTH_CREDENTIAL_LOCKED","OAuth 凭据路径不安全。",403))?;
    if let Err(error)=std::fs::remove_file(path) {if error.kind()!=std::io::ErrorKind::NotFound {return Err(failure("MCP_OAUTH_LOGOUT_FAILED","无法删除本用户 OAuth 凭据。",500));}}
    drop(guard);
    super::gateway_mcp::stop(id,ctx).await?;
    Ok(json!({"state":"disconnected"}))
}
pub(crate) async fn shutdown(ctx:&GatewayToolContext) {
    logins().lock().await.retain(|_,v| {if v.root==ctx.extension_root {let _=v.cancel.send(true);false} else {true}});
}

#[cfg(test)] mod tests {
    use super::*;
    #[test] fn metadata_paths_follow_rfc8414_and_oidc_issuer_paths() {
        let issuer=url::Url::parse("https://auth.example/tenant").unwrap();
        assert_eq!(well_known(&issuer,"oauth-authorization-server",false).as_str(),"https://auth.example/.well-known/oauth-authorization-server/tenant");
        assert_eq!(well_known(&issuer,"openid-configuration",true).as_str(),"https://auth.example/tenant/.well-known/openid-configuration");
        assert!(safe_endpoint("http://auth.example/token").is_err());
        assert!(safe_endpoint("https://secret@auth.example/token").is_err());
    }
    #[cfg(windows)] #[test] fn dpapi_ciphertext_is_not_plaintext_and_requires_configuration_entropy() {
        let plain=b"test-only-token-not-a-real-secret";
        let protected=protect(plain,b"service-a",false).unwrap();
        assert!(!protected.windows(plain.len()).any(|v|v==plain));
        assert_eq!(protect(&protected,b"service-a",true).unwrap(),plain);
        assert!(protect(&protected,b"service-b",true).is_err());
    }
    #[tokio::test] async fn callback_rejects_wrong_state_then_accepts_one_valid_code() {
        let listener=TcpListener::bind((std::net::Ipv4Addr::LOCALHOST,0)).await.unwrap();
        let port=listener.local_addr().unwrap().port(); let (_tx,rx)=watch::channel(false);
        let task=tokio::spawn(callback(listener,"/callback".to_owned(),"correct-state".to_owned(),rx));
        let http=client().unwrap();
        assert_eq!(http.get(format!("http://127.0.0.1:{port}/callback?state=wrong&code=wrong")).send().await.unwrap().status(),400);
        assert_eq!(http.get(format!("http://127.0.0.1:{port}/callback?state=correct-state&code=accepted")).send().await.unwrap().status(),200);
        assert_eq!(task.await.unwrap().unwrap(),"accepted");
    }
}
