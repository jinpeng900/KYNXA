import { element, button, toolbar, select, field, input, group, disclosure, alive } from './preview-ui.js';
import { createNotice } from './preview-notice.js';
import { normalizeSegments, readSubtitleSegments, subtitleText } from './media-subtitles.js';

export async function open(options) {
  const { container } = options;
  const player = element(options.kind === 'audio' ? 'audio' : 'video', 'media-player'); player.controls = true; player.preload = 'metadata';
  player.src = options.contentUrl; player.setAttribute('playsinline', '');
  const tools = toolbar(), notes = element('textarea', 'media-note'), marks = element('div', 'media-marks'), feedback = createNotice(container, 'preview-muted');
  notes.placeholder = '记录、粘贴转录或字幕；标记可跳回对应片段。'; notes.setAttribute('aria-label', '媒体笔记');
  let disposed = false, restorePending = true, bookmarks = [], segments = [], transcribing = false, subtitleUrl = null;
  const saved = options.state?.media || {};
  let restorable = { notes: String(saved.notes || '').slice(0, 200000), bookmarks: [], segments: [] }, warnedLargeState = false;
  function getState() {
    const media = { time: player.currentTime || 0, rate: player.playbackRate, volume: player.volume, notes: notes.value,
      bookmarks, segments: segments.map(segment => [segment.startMs, segment.endMs, segment.text]) };
    if (JSON.stringify(media).length <= 900000) { restorable = media; warnedLargeState = false; }
    else if (!warnedLargeState) {
      warnedLargeState = true;
      feedback.show('笔记和字幕较长，请先导出保存；当前内容仍可查看和导出。', true);
    }
    return { media: { ...restorable, time: media.time, rate: media.rate, volume: media.volume } };
  }
  notes.value = String(saved.notes || '').slice(0, 200000); bookmarks = Array.isArray(saved.bookmarks) ? saved.bookmarks.slice(0, 200) : [];
  segments = normalizeSegments(saved.segments);
  const emit = () => { if (!disposed) options.onState?.(getState()); };
  const time = s => { const n = Math.max(0, Math.floor(Number(s) || 0)); return [Math.floor(n / 3600), Math.floor(n / 60) % 60, n % 60].map(v => String(v).padStart(2, '0')).join(':'); };
  function renderMarks() { marks.replaceChildren(); for (const item of bookmarks) marks.append(button(`${time(item.time)} ${item.text}`, () => { player.currentTime = item.time; player.play().catch(() => {}); })); }
  const marker = input('片段说明');
  tools.append(group('播放控制', field('倍速', select([['.5', '.5×'], ['.75', '.75×'], ['1', '1×'], ['1.25', '1.25×'], ['1.5', '1.5×'], ['2', '2×']], String(saved.rate || 1), value => { player.playbackRate = Number(value); emit(); })),
    button('后退 10 秒', () => { player.currentTime = Math.max(0, player.currentTime - 10); }), button('前进 10 秒', () => { player.currentTime = Math.min(player.duration || Infinity, player.currentTime + 10); })),
    button('导出笔记', () => options.onExport?.({ filename: (options.title || '媒体') + '-笔记.md', mimeType: 'text/markdown', encoding: 'utf8', content: '# ' + (options.title || '媒体笔记') + '\n\n' + bookmarks.map(b => `- ${time(b.time)} ${b.text}`).join('\n') + '\n\n' + notes.value })));
  const subtitlePicker = element('input'); subtitlePicker.type = 'file'; subtitlePicker.accept = '.vtt,.srt'; subtitlePicker.hidden = true;
  function attachSubtitles(label = '字幕') {
    for (const old of player.querySelectorAll('track')) old.remove();
    if (subtitleUrl) URL.revokeObjectURL(subtitleUrl);
    subtitleUrl = null;
    if (!segments.length) return;
    subtitleUrl = URL.createObjectURL(new Blob([subtitleText(segments, 'vtt')], { type: 'text/vtt' }));
    const track = element('track'); track.kind = 'subtitles'; track.label = label; track.srclang = 'zh'; track.default = true; track.src = subtitleUrl;
    player.append(track);
  }
  subtitlePicker.addEventListener('change', async () => {
    const f = subtitlePicker.files?.[0]; if (!f) return;
    try {
      if (f.size > 4 * 1024 * 1024) throw new Error('字幕文件最多 4 MiB，请分段后载入。');
      const text = await f.text(); if (disposed) return;
      const parsed = readSubtitleSegments(text); if (!parsed.length) throw new Error('没有读到有效的 SRT 或 VTT 时间片段。');
      segments = parsed; attachSubtitles(f.name); notes.value = text; emit(); feedback.show(`已载入 ${segments.length} 个字幕片段`);
    } catch (error) { if (!disposed) feedback.show(error.message || '字幕载入失败', true); }
    finally { subtitlePicker.value = ''; }
  });
  const subtitleTools = disclosure('字幕与转录', button('载入字幕', () => subtitlePicker.click()), subtitlePicker);
  const language = select([['auto', '自动语言'], ['zh', '中文'], ['en', 'English']], 'auto', () => {});
  const transcribe = button('本地转录', async () => {
    if (transcribing || disposed) return; transcribing = true; transcribe.disabled = true; cancelTranscription.disabled = false; feedback.show('正在本地转录，聊天可以继续使用…');
    try {
      const result = await options.request('media.transcribe', { path: options.path, version: options.version, language: language.value });
      if (disposed) return;
      if (result.status !== 'completed') { feedback.show(result.notice || '转录引擎或模型未准备好', result.status !== 'cancelled'); return; }
      segments = normalizeSegments(result.segments); notes.value = result.text || ''; attachSubtitles();
      bookmarks = segments.map(s => ({ time: s.startMs / 1000, text: s.text })).slice(0, 200); renderMarks(); emit(); feedback.show(`转录完成 · ${segments.length} 个片段`);
    } catch (error) { if (!disposed && error.name !== 'AbortError') feedback.show(error.message || '转录未完成', true); }
    finally { transcribing = false; if (!disposed) { transcribe.disabled = false; cancelTranscription.disabled = true; } }
  });
  const cancelTranscription = button('取消转录', async () => {
    if (!transcribing || disposed || cancelTranscription.disabled) return; cancelTranscription.disabled = true;
    try { const result = await options.request('media.cancel', {}); if (!disposed && transcribing) feedback.show(result.notice || '已请求取消转录…'); }
    catch (error) { if (!disposed && error.name !== 'AbortError') { feedback.show(error.message || '取消转录未完成', true); cancelTranscription.disabled = !transcribing; } }
  });
  cancelTranscription.disabled = true;
  subtitleTools.lastElementChild.append(language, transcribe, cancelTranscription, button('导出字幕 SRT', () => {
    if (!segments.length) { feedback.show('先转录媒体，或使用“载入字幕”读取现有字幕。'); return; }
    options.onExport?.({ filename: (options.title || '媒体') + '.srt', mimeType: 'application/x-subrip', encoding: 'utf8', content: subtitleText(segments) });
  }));
  tools.append(disclosure('时间标记', marker, button('添加时间标记', () => { bookmarks.push({ time: player.currentTime, text: marker.value.trim() || '标记' }); marker.value = ''; renderMarks(); emit(); })), subtitleTools);
  if (options.kind !== 'audio') tools.append(button('保存当前画面', () => {
    if (!player.videoWidth) return; const canvas = document.createElement('canvas'); canvas.width = player.videoWidth; canvas.height = player.videoHeight;
    canvas.getContext('2d').drawImage(player, 0, 0); options.onExport?.({ filename: '视频画面-' + Math.floor(player.currentTime) + '.png', mimeType: 'image/png', encoding: 'base64', content: canvas.toDataURL().split(',')[1] });
  }));
  player.addEventListener('loadedmetadata', () => {
    if (restorePending) { restorePending = false; player.currentTime = Math.min(Math.max(0, saved.time || 0), player.duration || Infinity); player.volume = Math.min(1, Math.max(0, saved.volume ?? 1)); player.playbackRate = saved.rate || 1; }
    if (!transcribing) feedback.show(`${options.title || ''} · ${time(player.duration)}${player.videoWidth ? ` · ${player.videoWidth} × ${player.videoHeight}` : ''}`);
  });
  player.addEventListener('error', () => { if (!disposed) feedback.show('当前系统无法解码此媒体，可转换为 MP4/H.264 或 WAV 后打开。', true); });
  let timer = 0; player.addEventListener('timeupdate', () => { if (!timer) timer = setTimeout(() => { timer = 0; emit(); }, 1000); });
  for (const type of ['pause', 'seeked', 'ratechange', 'volumechange']) player.addEventListener(type, emit); notes.addEventListener('change', emit);
  container.append(player, tools, feedback.node, marks, notes); renderMarks(); attachSubtitles(); alive(options);
  return { getState, setActive(active) { if (!active) player.pause(); }, dispose() { disposed = true; clearTimeout(timer); player.pause(); player.removeAttribute('src'); player.load(); if (subtitleUrl) URL.revokeObjectURL(subtitleUrl); feedback.dispose(); } };
}
