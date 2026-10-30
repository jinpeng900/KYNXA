#!/usr/bin/env node
/** KYNXA phone companion. Node built-ins only. Never exposes a generic gateway proxy. */
import http from 'node:http';
import fs from 'node:fs/promises';
import path from 'node:path';
import os from 'node:os';
import crypto from 'node:crypto';
import { fileURLToPath } from 'node:url';
import { Readable } from 'node:stream';
import { pipeline } from 'node:stream/promises';

const args = new Map();
for (let i=2;i<process.argv.length;i++) { const key=process.argv[i]; if(!key.startsWith('--'))throw new Error('Invalid argument');args.set(key,process.argv[i+1]?.startsWith('--')||!process.argv[i+1]?true:process.argv[++i]); }
if(args.has('--help')) { console.log('node tools/workbench-remote.mjs --host 0.0.0.0 --port 55220 --gateway-url http://127.0.0.1:5218 --admin-token-file <file> --data-dir <node-service> [--public-url https://reachable-host] [--desktop-lifetime]');process.exit(0); }
const host=String(args.get('--host')||'0.0.0.0'),port=Number(args.get('--port')||55220);
if(!Number.isInteger(port)||port<1024||port>65535)throw new Error('Invalid remote port');
const gateway=new URL(String(args.get('--gateway-url')||'http://127.0.0.1:5218'));
if(gateway.protocol!=='http:'||!['127.0.0.1','localhost','[::1]'].includes(gateway.hostname)||gateway.username||gateway.password||gateway.pathname!=='/')throw new Error('Gateway must be a loopback HTTP origin');
if(!args.get('--admin-token-file')||!args.get('--data-dir'))throw new Error('--admin-token-file and --data-dir are required');
const adminToken=(await fs.readFile(String(args.get('--admin-token-file')),'utf8')).replace(/^\uFEFF/,'').trim();
if(adminToken.length<32||adminToken.length>512)throw new Error('Admin token must contain at least 32 characters');
const dataDir=path.resolve(String(args.get('--data-dir'))),devicesFile=path.join(dataDir,'devices.json');
await fs.mkdir(dataDir,{recursive:true});
const staticDir=path.join(path.dirname(fileURLToPath(import.meta.url)),'workbench-remote');
const publicUrl=args.get('--public-url')?new URL(String(args.get('--public-url'))):null;
if(publicUrl&&(!['http:','https:'].includes(publicUrl.protocol)||publicUrl.username||publicUrl.password||publicUrl.pathname!=='/'))throw new Error('Public URL must be a reachable HTTP(S) origin');
const scopesAllowed=new Set(['read','message','steer','download']);
const hash=value=>crypto.createHash('sha256').update(String(value)).digest('hex');
const secret=()=>crypto.randomBytes(32).toString('base64url');
const equal=(a,b)=>{const aa=Buffer.from(hash(a)),bb=Buffer.from(hash(b));return crypto.timingSafeEqual(aa,bb);};
const guid=value=>typeof value==='string'&&/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(value)&&!/^0{8}-0{4}-0{4}-0{4}-0{12}$/.test(value);
const text=(value,max=300)=>typeof value==='string'?value.slice(0,max):'';
const field=(value,a,b)=>value?.[a]??value?.[b];
const fail=(status,message)=>{const e=new Error(message);e.status=status;return e;};
let devices=[],persistTail=Promise.resolve(),stopping=false;
try { const saved=JSON.parse(await fs.readFile(devicesFile,'utf8'));if(saved.version!==1||!Array.isArray(saved.devices))throw new Error('Unsupported device registry');devices=saved.devices.filter(d=>guid(d.id)&&/^[a-f0-9]{64}$/.test(d.tokenHash)&&Array.isArray(d.conversationIds)&&d.conversationIds.every(guid)&&Array.isArray(d.scopes)&&d.scopes.every(s=>scopesAllowed.has(s))); } catch(e){if(e.code!=='ENOENT')throw new Error('Device registry is invalid; existing registry has been preserved');}
const invites=new Map(),pending=new Map(),downloads=new Map(),modelSelections=new Map(),rate=new Map(),controllers=new Set(),streams=new Set();
function persist(){const content=JSON.stringify({version:1,devices},null,2);persistTail=persistTail.catch(()=>{}).then(async()=>{const tmp=devicesFile+'.'+crypto.randomBytes(5).toString('hex')+'.tmp';await fs.writeFile(tmp,content,{mode:0o600});await fs.rename(tmp,devicesFile);});return persistTail;}
function addresses(){const list=[];for(const entries of Object.values(os.networkInterfaces()))for(const item of entries||[])if(item.family==='IPv4'&&!item.internal)list.push(`http://${item.address}:${port}`);return [...new Set(list)];}
function publicOrigin(){return publicUrl?.origin||addresses()[0]||`http://127.0.0.1:${port}`;}
function isLoopback(value){return value==='127.0.0.1'||value==='::1'||value?.startsWith('::ffff:127.');}
function tokenFrom(req){const match=/^Bearer ([^\s]+)$/.exec(req.headers.authorization||'');return match?.[1]||'';}
function deviceFor(req,scope){const token=tokenFrom(req);const tokenHash=hash(token);const device=devices.find(d=>!d.revokedAt&&equal(d.tokenHash,tokenHash));if(!token||!device)throw fail(401,'设备未配对或授权已撤销，请在电脑重新扫码。');if(scope&&!device.scopes.includes(scope))throw fail(403,'电脑尚未允许此操作范围。');device.lastSeenAt=new Date().toISOString();return device;}
function permitConversation(device,value){if(!guid(value)||!device.conversationIds.includes(value.toLowerCase()))throw fail(403,'此设备没有该会话的授权。');return value.toLowerCase();}
function rateLimit(req,bucket,max=90){const key=(req.socket.remoteAddress||'unknown')+':'+bucket,now=Date.now();let entry=rate.get(key);if(!entry||now>entry.until){entry={count:0,until:now+60000};rate.set(key,entry);}if(++entry.count>max)throw fail(429,'请求过于频繁，请稍后重试。');}
function headers(res){res.setHeader('Cache-Control','no-store');res.setHeader('X-Content-Type-Options','nosniff');res.setHeader('Referrer-Policy','no-referrer');res.setHeader('Content-Security-Policy',"default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' data:; frame-ancestors 'none'; base-uri 'none'; form-action 'self'");res.setHeader('Permissions-Policy','camera=(), microphone=(), geolocation=()');}
function json(res,status,data){if(res.destroyed||res.writableEnded)return;res.writeHead(status,{'Content-Type':'application/json; charset=utf-8'});res.end(JSON.stringify(data));}
async function body(req){if(!/^application\/json(?:;|$)/i.test(req.headers['content-type']||''))throw fail(415,'请求必须是 JSON。');let length=0;const parts=[];for await(const chunk of req){length+=chunk.length;if(length>48*1024)throw fail(413,'请求超过大小限制。');parts.push(chunk);}try{const parsed=JSON.parse(Buffer.concat(parts).toString('utf8'));if(!parsed||Array.isArray(parsed)||typeof parsed!=='object')throw new Error();return parsed;}catch{throw fail(400,'JSON 请求无效。');}}
async function gatewayFetch(endpoint,options={}){if(!endpoint.startsWith('/')||endpoint.startsWith('//'))throw fail(500,'内部请求无效。');const controller=new AbortController();controllers.add(controller);const timeout=setTimeout(()=>controller.abort(),options.timeout||30000);try{return await fetch(new URL(endpoint,gateway),{method:options.method||'GET',headers:options.body?{'Content-Type':'application/json'}:{},body:options.body?JSON.stringify(options.body):undefined,redirect:'error',signal:controller.signal});}catch{throw fail(502,'本机网关暂不可达，请确认电脑端仍在运行。');}finally{clearTimeout(timeout);controllers.delete(controller);}}
async function gatewayJson(endpoint,options={}){const r=await gatewayFetch(endpoint,options);const data=await r.json().catch(()=>null);if(!r.ok)throw fail(r.status>=500?502:r.status,text(data?.error?.message||data?.message||'网关暂未接受此操作。',1000));if(!data)throw fail(502,'网关响应格式无效。');return data;}
function catalogChats(catalog){const result=[];const add=(chats,project)=>{for(const chat of chats||[]){const id=field(chat,'id','Id');if(guid(id))result.push({id:id.toLowerCase(),title:text(field(chat,'title','Title')||'未命名会话'),project:project||null});}};add(field(catalog,'chats','Chats'));for(const project of field(catalog,'projects','Projects')||[])add(field(project,'chats','Chats'),text(field(project,'name','Name')));return result;}
async function allowedChats(device){const all=catalogChats(await gatewayJson('/api/conversations/catalog?includeMessages=false'));return all.filter(c=>device.conversationIds.includes(c.id));}
function projectTask(task){const scope=field(task,'scope','Scope');return {id:field(task,'taskId','TaskId'),conversationId:field(scope,'conversationId','ConversationId'),title:text(field(task,'title','Title')||field(task,'goal','Goal'),1000),state:text(field(task,'state','State')),version:field(task,'version','Version'),updatedAt:field(task,'updatedAtMs','UpdatedAtMs')};}
async function stateFor(device,conversationId){const chats=await allowedChats(device);const id=conversationId?permitConversation(device,conversationId):chats[0]?.id;if(!id)return {connected:true,device:{name:device.name,scopes:device.scopes},conversations:chats,conversationId:null,tasks:[],messages:[],downloads:[],notice:'尚无可访问的正式会话。'};if(!chats.some(c=>c.id===id))throw fail(404,'此会话已删除或不可用。');const [taskData,history]=await Promise.all([gatewayJson('/api/tasks?conversationId='+encodeURIComponent(id)),gatewayJson(`/api/conversations/${id}/messages?limit=80&maxBytes=262144`)]);const tasks=(field(taskData,'tasks','Tasks')||[]).map(projectTask).filter(t=>t.conversationId?.toLowerCase()===id);const messages=(field(history,'messages','Messages')||[]).slice(-60).map(m=>({id:field(m,'id','Id'),role:text(field(m,'role','Role'),20),content:text(field(m,'content','Content'),40000),status:text(field(m,'status','Status'),60),createdAt:field(m,'createdAt','CreatedAt')}));return {connected:true,device:{name:device.name,scopes:device.scopes},conversations:chats,conversationId:id,tasks,messages,downloads:device.scopes.includes('download')?[...downloads.values()].filter(d=>d.conversationId===id).map(d=>({id:d.id,title:d.title,version:d.version})):[],updatedAt:new Date().toISOString(),notice:publicUrl?'通过配置的可达地址连接；中继可用性由部署环境决定。':'当前为局域网直连；离开同一网络需要配置可达地址或可信中继。'};}
function cleanup(){const now=Date.now();for(const [key,invite]of invites)if(invite.expiresAt<now||invite.used)invites.delete(key);for(const [id,p]of pending)if(p.expiresAt<now)pending.delete(id);for(const [key,v]of rate)if(v.until<now)rate.delete(key);}
function invitation(conversationId){cleanup();let invite=invites.get(conversationId);if(!invite){const code=crypto.randomBytes(12).toString('base64url');invite={code,conversationId,expiresAt:Date.now()+5*60000,used:false};invites.set(conversationId,invite);}return invite;}
function adminStatus(conversationId){const invite=invitation(conversationId),pendingList=[...pending.values()].filter(p=>p.status==='pending'&&p.conversationId===conversationId).map(p=>({id:p.id,requestId:p.id,name:p.name,requestedAt:p.requestedAt,conversationId:p.conversationId}));return {online:true,port,addresses:addresses(),inviteUrl:publicOrigin()+'/#invite='+invite.code,inviteCode:invite.code,expiresAt:new Date(invite.expiresAt).toISOString(),pending:pendingList,pendingRequests:pendingList,devices:devices.filter(d=>!d.revokedAt).map(d=>({id:d.id,deviceId:d.id,name:d.name,scopes:d.scopes,conversationIds:d.conversationIds,authorizedHere:d.conversationIds.includes(conversationId),lastSeenAt:d.lastSeenAt,createdAt:d.createdAt})),notice:publicUrl?'已配置公开地址；请自行确认公网、TLS和中继可达。':'手机与电脑需在可达的同一局域网。跨网络使用需要额外可达地址或中继。'};}
async function adminAction(input){const conversationId=typeof input.conversationId==='string'?input.conversationId.toLowerCase():'';if(!guid(conversationId))throw fail(400,'管理操作必须绑定有效 conversationId。');const payload=input.payload&&typeof input.payload==='object'?input.payload:{};const provider=text(payload.providerId||payload.provider,200),model=text(payload.modelId||payload.model,240);if(provider&&model)modelSelections.set(conversationId,{provider,model});switch(input.action){case 'remote.status':return adminStatus(conversationId);case 'remote.approve':{const p=pending.get(payload.requestId||payload.id);if(!p||p.status!=='pending'||p.expiresAt<Date.now()||p.conversationId!==conversationId)throw fail(404,'配对申请已失效或会话不一致。');const scopes=Array.isArray(payload.scopes)?payload.scopes:['read','message','steer','download'];if(!scopes.length||!scopes.every(s=>scopesAllowed.has(s))||!scopes.includes('read'))throw fail(400,'设备范围必须包含 read 且在允许列表内。');if(devices.filter(d=>!d.revokedAt).length>=32)throw fail(409,'已达到 32 台设备上限，请先撤销旧设备。');const token=secret(),device={id:crypto.randomUUID(),name:p.name,tokenHash:hash(token),conversationIds:[conversationId],scopes:[...new Set(scopes)],createdAt:new Date().toISOString(),lastSeenAt:null};devices.push(device);try{await persist();}catch{devices=devices.filter(d=>d!==device);throw fail(500,'设备授权无法安全保存，配对未完成。');}p.status='approved';p.deviceToken=token;p.deviceId=device.id;p.expiresAt=Date.now()+2*60000;return {approved:true,deviceId:device.id,...adminStatus(conversationId)};}
case 'remote.reject':{const p=pending.get(payload.requestId||payload.id);if(!p||p.conversationId!==conversationId)throw fail(404,'配对申请不存在。');p.status='rejected';delete p.deviceToken;return {rejected:true,...adminStatus(conversationId)};}
case 'remote.revoke':{const device=devices.find(d=>d.id===(payload.deviceId||payload.id));if(!device)throw fail(404,'设备授权不存在。');device.revokedAt=new Date().toISOString();await persist();for(const stream of streams)if(stream.deviceId===device.id)stream.close();for(const p of pending.values())if(p.deviceId===device.id){p.status='rejected';delete p.deviceToken;}return {revoked:true,...adminStatus(conversationId)};}
case 'remote.grantConversation':{const device=devices.find(d=>!d.revokedAt&&d.id===(payload.deviceId||payload.id));if(!device)throw fail(404,'设备不存在或已撤销。');const chats=catalogChats(await gatewayJson('/api/conversations/catalog?includeMessages=false'));if(!chats.some(c=>c.id===conversationId))throw fail(404,'当前正式会话已不可用。');if(!device.conversationIds.includes(conversationId)){device.conversationIds.push(conversationId);await persist();}return {granted:true,deviceId:device.id,conversationId,...adminStatus(conversationId)};}
case 'remote.registerDownload':{if(!text(payload.path,32768)||!/^([a-f0-9]{64})$/i.test(payload.version||''))throw fail(400,'下载必须指定已授权精确 path 和 SHA-256 version。');let address;try{address=new URL(String(payload.contentUrl||''),gateway);}catch{throw fail(400,'下载授权地址无效。');}if(address.origin!==gateway.origin||address.pathname!=='/agent/workspace/content'||address.searchParams.get('conversationId')?.toLowerCase()!==conversationId||address.searchParams.get('path')!==payload.path||address.searchParams.get('version')?.toLowerCase()!==payload.version.toLowerCase())throw fail(403,'下载地址必须精确匹配本机会话、文件和版本。');const check=await gatewayFetch(address.pathname+address.search,{method:'HEAD'});if(!check.ok&&check.status!==405)throw fail(409,'此文件授权已失效，请重新在电脑打开成果。');const existing=[...downloads.values()].find(d=>d.url===address.href);const item=existing||{id:crypto.randomUUID(),conversationId,title:text(payload.title||path.basename(payload.path),180),path:payload.path,version:payload.version.toLowerCase(),url:address.href};downloads.set(item.id,item);return {registered:true,id:item.id,title:item.title};}
default:throw fail(400,'不支持此管理操作。');}}
function sseHeaders(res){res.writeHead(200,{'Content-Type':'text/event-stream; charset=utf-8','Connection':'keep-alive','X-Accel-Buffering':'no'});res.flushHeaders();}
function event(res,type,data){if(!res.destroyed&&!res.writableEnded)res.write(`event: ${type}\ndata: ${JSON.stringify(data)}\n\n`);}
async function stateStream(req,res,device,conversationId){if([...streams].filter(s=>s.deviceId===device.id).length>=3)throw fail(429,'此设备的状态连接过多。');const id=permitConversation(device,conversationId);sseHeaders(res);let stopped=false,timer;const record={deviceId:device.id,close(){stopped=true;clearTimeout(timer);res.end();streams.delete(record);}};streams.add(record);res.on('close',()=>record.close());async function tick(){if(stopped)return;try{deviceFor(req,'read');event(res,'state',await stateFor(device,id));}catch(e){event(res,'error',{message:e.message,status:e.status||500});if(e.status===401){record.close();return;}}if(!stopped)timer=setTimeout(tick,2500);}await tick();}
async function sendMessage(req,res,device,input){const conversationId=permitConversation(device,input.conversationId),message=text(input.message,16001).trim();if(!message||message.length>16000)throw fail(400,'消息必须为 1–16000 字符。');if(!(await allowedChats(device)).some(c=>c.id===conversationId))throw fail(404,'此会话已不可用。');const mode=input.mode==='steer'?'steer':'message';if(!device.scopes.includes(mode))throw fail(403,'电脑未允许此操作。');if(mode==='steer'){if(!guid(input.taskId))throw fail(400,'请选择要引导的现有任务。');const detail=await gatewayJson(`/api/tasks/${input.taskId}?conversationId=${conversationId}`),task=projectTask(detail.task||detail.Task);if(task.conversationId?.toLowerCase()!==conversationId)throw fail(403,'任务不属于已授权会话。');if(!['running','paused','queued','waiting','waiting_approval'].includes(task.state))throw fail(409,'此任务当前不能引导，请发送新消息。');const result=await gatewayJson(`/api/tasks/${input.taskId}/steer`,{method:'POST',body:{conversationId,expectedVersion:input.expectedVersion||task.version,userMessageId:crypto.randomUUID(),message,action:task.state==='paused'?'continue':'update'}});json(res,200,{accepted:true,mode,task:projectTask(result.task||result.Task)});return;}
  let selection=modelSelections.get(conversationId);
  if(!selection){
    const history=await gatewayJson(`/api/conversations/${conversationId}/messages?limit=60&maxBytes=262144`);
    const last=(field(history,'messages','Messages')||[]).toReversed().find(m=>field(m,'provider','Provider')&&field(m,'model','Model'));
    if(last)selection={provider:field(last,'provider','Provider'),model:field(last,'model','Model')};
  }
  if(!selection)throw fail(409,'请先在电脑为此会话选择模型并刷新手机连接。');
  if([...streams].some(s=>s.deviceId===device.id&&s.kind==='message'))throw fail(409,'此设备仍在观察上一条消息，请等待完成或重新连接。');
  const requestId=guid(input.requestId)?input.requestId:crypto.randomUUID();
  const userMessageId=guid(input.userMessageId)&&input.userMessageId!==requestId?input.userMessageId:crypto.randomUUID();
  const upstream=await gatewayFetch('/api/chat/stream',{method:'POST',timeout:60000,body:{conversationId,message,...selection,requestId,userMessageId,permissionMode:'ask'}});
  if(!upstream.ok){await upstream.body?.cancel();throw fail(upstream.status>=500?502:upstream.status,'网关未接受消息，请在电脑查看任务状态后再决定是否重试。');}
  if(!upstream.headers.get('content-type')?.includes('text/event-stream')){await upstream.body?.cancel();throw fail(502,'网关未返回有效消息流，请在电脑检查任务。');}
  sseHeaders(res);event(res,'accepted',{requestId,userMessageId,conversationId});
  const record={kind:'message',deviceId:device.id,close(){res.end();}};streams.add(record);
  res.on('close',()=>{streams.delete(record);upstream.body?.cancel().catch(()=>{});});
  let buffer='',total=0;
  const contentTypes=new Set(['delta','text_delta','completed','interrupted','content_snapshot','snapshot']);
  try{
    const decoder=new TextDecoder();
    for await(const chunk of upstream.body){
      if(res.destroyed||res.writableEnded||device.revokedAt)break;
      buffer+=decoder.decode(chunk,{stream:true}).replaceAll('\r\n','\n');
      if(buffer.length>2*1024*1024)throw fail(502,'消息流数据超过限制。');
      let index;
      while((index=buffer.indexOf('\n\n'))>=0){
        const frame=buffer.slice(0,index);buffer=buffer.slice(index+2);
        for(const line of frame.split('\n'))if(line.startsWith('data:')){
          let value;try{value=JSON.parse(line.slice(5));}catch{continue;}
          if(value.conversationId&&value.conversationId.toLowerCase()!==conversationId)throw fail(502,'网关返回了不一致的会话身份。');
          if(value.requestId&&value.requestId!==requestId)throw fail(502,'网关返回了不一致的请求身份。');
          const type=text(value.type||'update',60);
          // Keep phone output to user-facing text/status; internal reasoning and tool arguments stay on desktop.
          if(type==='reasoning_delta')continue;
          const approval=type==='approval_required'||type==='task_approval_required';
          const projected={type,requestId,taskId:value.taskId,status:approval?'waiting_approval':text(value.status,100),content:contentTypes.has(type)?text(value.content||value.delta||value.text,64000):'',message:approval?'需要在电脑上批准操作。':text(value.message||value.error?.message,1500)};
          total+=projected.content.length;if(total<2*1024*1024)event(res,'update',projected);
        }
      }
    }
  }catch(e){event(res,'error',{message:e.message||'观察连接已中断，任务仍可在会话状态中恢复。'});}
  finally{streams.delete(record);await upstream.body?.cancel().catch(()=>{});res.end();}
}

const server=http.createServer(async(req,res)=>{headers(res);try{const requestHost=String(req.headers.host||'');const localHosts=new Set([`127.0.0.1:${port}`,`localhost:${port}`,`[::1]:${port}`,...addresses().map(v=>new URL(v).host),publicUrl?.host].filter(Boolean));if(!localHosts.has(requestHost))throw fail(403,'请求主机无效。');if(typeof req.headers.origin==='string'&&!['http://'+requestHost,'https://'+requestHost,publicUrl?.origin].includes(req.headers.origin))throw fail(403,'不允许跨站请求。');if(!req.url?.startsWith('/')||req.url.startsWith('//'))throw fail(400,'请求地址无效。');const url=new URL(req.url,'http://'+requestHost);rateLimit(req,'all',180);if(req.method==='POST'&&url.pathname==='/admin/workbench'){if(!isLoopback(req.socket.remoteAddress)||!equal(tokenFrom(req),adminToken))throw fail(403,'仅允许已认证的本机管理。');json(res,200,await adminAction(await body(req)));return;}
if(req.method==='GET'&&['/','/mobile.css','/mobile.js'].includes(url.pathname)){const name=url.pathname==='/'?'mobile.html':url.pathname.slice(1);const bytes=await fs.readFile(path.join(staticDir,name));res.writeHead(200,{'Content-Type':name.endsWith('.html')?'text/html; charset=utf-8':name.endsWith('.css')?'text/css; charset=utf-8':'text/javascript; charset=utf-8'});res.end(bytes);return;}
if(req.method==='POST'&&url.pathname==='/api/pair'){rateLimit(req,'pair',8);cleanup();if(pending.size>=64)throw fail(429,'配对申请过多。');const input=await body(req),invite=[...invites.values()].find(i=>!i.used&&i.expiresAt>Date.now()&&equal(i.code,input.code||''));if(!invite)throw fail(403,'邀请码已使用或过期，请在电脑重新获取。');invite.used=true;const pollToken=secret(),p={id:crypto.randomUUID(),name:text(input.name||'我的手机',80),conversationId:invite.conversationId,pollHash:hash(pollToken),status:'pending',requestedAt:new Date().toISOString(),expiresAt:Date.now()+5*60000};pending.set(p.id,p);json(res,202,{requestId:p.id,pollToken,status:'pending'});return;}
if(req.method==='GET'&&url.pathname.startsWith('/api/pair/')){rateLimit(req,'poll',60);const p=pending.get(url.pathname.slice('/api/pair/'.length));if(!p||p.expiresAt<Date.now()||!equal(hash(tokenFrom(req)),p.pollHash))throw fail(403,'配对查询凭证无效或过期。');json(res,200,{status:p.status,...(p.status==='approved'?{deviceToken:p.deviceToken,deviceId:p.deviceId,conversationId:p.conversationId}:{})});return;}
if(req.method==='GET'&&url.pathname==='/api/state'){const d=deviceFor(req,'read');json(res,200,await stateFor(d,url.searchParams.get('conversationId')));return;}
if(req.method==='GET'&&url.pathname==='/api/events'){const d=deviceFor(req,'read');await stateStream(req,res,d,url.searchParams.get('conversationId'));return;}
if(req.method==='POST'&&url.pathname==='/api/message'){rateLimit(req,'messages',12);const d=deviceFor(req);await sendMessage(req,res,d,await body(req));return;}
if(req.method==='POST'&&url.pathname==='/api/disconnect'){const d=deviceFor(req);d.revokedAt=new Date().toISOString();await persist();for(const stream of streams)if(stream.deviceId===d.id)stream.close();json(res,200,{disconnected:true});return;}
if(req.method==='GET'&&/^\/api\/downloads\/[a-f0-9-]{36}$/.test(url.pathname)){const d=deviceFor(req,'download'),item=downloads.get(url.pathname.split('/').at(-1));if(!item)throw fail(404,'成果未授权或已失效。');permitConversation(d,item.conversationId);const upstream=await gatewayFetch(new URL(item.url).pathname+new URL(item.url).search);if(!upstream.ok){await upstream.body?.cancel();throw fail(409,'成果已变化或授权失效，请在电脑重新授权该版本。');}res.writeHead(200,{'Content-Type':upstream.headers.get('content-type')||'application/octet-stream','Content-Disposition':`attachment; filename*=UTF-8''${encodeURIComponent(item.title.replace(/[\r\n]/g,''))}`});await pipeline(Readable.fromWeb(upstream.body),res);return;}
throw fail(404,'此接口不可用。');}catch(e){if(res.headersSent){if(!res.writableEnded)res.end();}else json(res,e.status||500,{error:e.status?e.message:'服务暂时无法完成此操作。'});}});
server.requestTimeout=30000;server.headersTimeout=15000;server.maxHeadersCount=40;
const cleanupTimer=setInterval(cleanup,60000);cleanupTimer.unref();
async function shutdown(){if(stopping)return;stopping=true;clearInterval(cleanupTimer);for(const c of controllers)c.abort();for(const stream of streams)stream.close();server.close();server.closeAllConnections();await persistTail.catch(()=>{});process.exit(0);}
process.on('SIGTERM',shutdown);process.on('SIGINT',shutdown);
if(args.has('--desktop-lifetime')){process.stdin.setEncoding('utf8');process.stdin.on('data',value=>{if(value.split(/\r?\n/).some(line=>line.trim()==='shutdown'))shutdown();});process.stdin.on('end',shutdown);process.stdin.resume();}
await new Promise((resolve,reject)=>{server.once('error',reject);server.listen(port,host,resolve);});
// Operational metadata only: no invitation, admin token, device token, or gateway secret.
console.log(JSON.stringify({service:'kynxa-workbench-remote',ready:true,host,port,addresses:addresses(),publicUrl:publicUrl?.origin||null}));
