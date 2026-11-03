import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import os from 'node:os';
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const resources = fileURLToPath(new URL('../../apps/desktop/Resources/', import.meta.url));
const browser = process.argv[2];
if (!browser || !fs.existsSync(browser)) throw new Error('Pass an installed Edge or Chromium executable.');
const profile = fs.mkdtempSync(path.join(os.tmpdir(), 'kynxa-transcript-smoke-'));
const host = `window.chrome ||= {}; window.chrome.webview = {
  addEventListener(type, handler) { if (type === 'message') window.receive = data => handler({data}); }, postMessage() {} };`;
const test = `(async () => {
const checks = [], errors = [];
window.addEventListener('error', e => errors.push(e.message));
const check = (value, name) => { if (!value) throw Error(name); checks.push(name); };
const tick = () => new Promise(resolve => setTimeout(resolve, 100));
const waitFor = async predicate => {
  const deadline = performance.now() + 1500;
  while (!predicate() && performance.now() < deadline) await tick();
};
const send = command => window.receive(command);
const text = '长回复正文 '.repeat(12000);
const segment = {id:'answer',round:1,order:0,phase:'commentary',status:'completed',content:text,html:'<p>'+text+'</p>'};
const message = {id:'reply',role:'assistant',status:'streaming',streaming:true,content:text,html:'',
  reasoningHtml:'',assistantSegments:[segment],toolActivities:[],generationElapsedMs:90000,generationTimingState:'live',generationClockId:'clock'};
try {
  send({type:'render',conversationId:'chat',revision:1,openAtBottom:true,messages:[message]});
  await waitFor(() => document.querySelector('.message-body'));
  check(document.querySelector('.message-body')?.textContent.includes('长回复正文'),'long body rendered');
  check(!!document.querySelector('.streaming-dot'),'active reply shows generation marker');
  const body = document.querySelector('.message-body');
  const range = document.createRange(); range.selectNodeContents(body); getSelection().addRange(range);
  send({type:'renderPatch',conversationId:'chat',revision:2,messages:[{...message,reuseBody:true,
    content:null,html:null,reasoningHtml:null,assistantSegments:null,executionPhase:'waiting-resource'}]}); await tick();
  check(window.transcriptState().pending && getSelection().toString().includes('长回复正文'),'selection defers body patch');
  send({type:'messageState',conversationId:'chat',revision:3,message:{id:'reply',role:'assistant',status:'completed',
    streaming:false,waiting:false,error:'',canRetry:false,generationElapsedMs:90000,generationTimingState:'settled',generationClockId:'clock'}});
  check(!document.querySelector('.streaming-dot'),'terminal marker clears while selection is active');
  getSelection().removeAllRanges(); document.dispatchEvent(new Event('selectionchange')); await tick();
  check(document.querySelector('.message-body')?.textContent.includes('长回复正文'),'deferred patch retains full body');
  check(!document.querySelector('.streaming-dot'),'older body patch cannot reopen terminal generation');
  send({type:'renderPatch',conversationId:'chat',revision:4,messages:[{...message,status:'completed',streaming:false,
    content:'最终结果',assistantSegments:[{...segment,phase:'final_answer',content:'最终结果',html:'<p>最终结果</p>'}]}]});
  await waitFor(() => document.querySelector('.message-body')?.textContent.includes('最终结果'));
  check(document.querySelector('.message-body')?.textContent.includes('最终结果'),'final full body replaces reused source');
  send({type:'render',conversationId:'other',revision:5,messages:[{id:'other-reply',role:'assistant',status:'completed',streaming:false,
    content:'另一个聊天',html:'<p>另一个聊天</p>',assistantSegments:[],toolActivities:[]}]}); await tick();
  check(!document.getElementById('messages').textContent.includes('最终结果'),'chat navigation does not leak prior reply');
  send({type:'render',conversationId:'chat',revision:6,messages:[{...message,status:'completed',streaming:false,
    assistantSegments:[{...segment,phase:'final_answer'}]}]}); await tick();
  check(document.querySelector('.message-body')?.textContent.includes('长回复正文') && !document.querySelector('.streaming-dot'),
    'reopening settled conversation restores body without generation marker');
  check(errors.length === 0,'no browser errors');
  const result = document.createElement('pre'); result.id='verification-result'; result.textContent=JSON.stringify({passed:checks.length,checks}); document.body.append(result);
} catch(error) {
  const result = document.createElement('pre'); result.id='verification-result'; result.textContent=JSON.stringify({passed:false,error:error.message,checks,errors,
    state:window.transcriptState(),bodies:[...document.querySelectorAll('.message-body')].map(node=>({hidden:node.hidden,text:node.textContent.slice(0,50)}))}); document.body.append(result);
}
})();`;
const server = http.createServer((request, response) => {
  const url = new URL(request.url, 'http://127.0.0.1');
  if (url.pathname === '/host.js' || url.pathname === '/test.js') {
    response.setHeader('Content-Type', 'text/javascript; charset=utf-8'); response.end(url.pathname === '/host.js' ? host : test); return;
  }
  const file = path.resolve(resources, '.' + decodeURIComponent(url.pathname));
  if (!file.startsWith(resources) || !fs.existsSync(file) || !fs.statSync(file).isFile()) { response.writeHead(404).end(); return; }
  if (url.pathname === '/Transcript/index.html') {
    const html = fs.readFileSync(file, 'utf8').replace(/<meta http-equiv="Content-Security-Policy"[^>]*>/, '')
      .replace('<head>', '<head><script src="/host.js"></script>').replace('</head>', '<script src="/test.js" defer></script></head>');
    response.setHeader('Content-Type', 'text/html; charset=utf-8'); response.end(html); return;
  }
  const types = {'.js':'text/javascript','.css':'text/css','.html':'text/html','.woff2':'font/woff2'};
  response.setHeader('Content-Type', types[path.extname(file)] || 'application/octet-stream'); fs.createReadStream(file).pipe(response);
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
try {
  const child = spawn(browser, ['--headless','--disable-gpu','--no-first-run','--no-default-browser-check',
    '--disable-background-networking',`--user-data-dir=${profile}`,'--dump-dom','--virtual-time-budget=5000',
    `http://127.0.0.1:${server.address().port}/Transcript/index.html`], {windowsHide:true});
  let output='', errors=''; child.stdout.on('data', chunk => output+=chunk); child.stderr.on('data', chunk => errors+=chunk);
  const timeout = setTimeout(() => child.kill(), 20000);
  const code = await new Promise((resolve,reject) => {child.on('exit',resolve);child.on('error',reject);}); clearTimeout(timeout);
  const raw = output.match(/<pre id="verification-result">([\s\S]*?)<\/pre>/)?.[1];
  if (!raw) throw new Error('Missing DOM result: '+errors.slice(-800));
  const result = JSON.parse(raw.replace(/&quot;/g,'"').replace(/&amp;/g,'&').replace(/&lt;/g,'<').replace(/&gt;/g,'>'));
  console.log(JSON.stringify(result)); if (!result.passed || code !== 0) process.exitCode=1;
} finally {
  await new Promise(resolve => server.close(resolve));
  // Only the unique test browser profile is removed; installed browsers and user profiles are untouched.
  // 仅删除本次测试的唯一浏览器目录，不改安装目录和用户配置。
  if (path.dirname(profile) !== os.tmpdir() || !path.basename(profile).startsWith('kynxa-transcript-smoke-')) throw new Error('Unexpected profile path');
  fs.rmSync(profile,{recursive:true,force:true,maxRetries:5,retryDelay:100});
}
