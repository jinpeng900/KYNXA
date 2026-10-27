export function normalizeSegments(values) {
  if (!Array.isArray(values)) return [];
  return values.map(value => Array.isArray(value) ? { startMs: value[0], endMs: value[1], text: value[2] } : value)
    .filter(value => value && Number.isFinite(value.startMs) && Number.isFinite(value.endMs)
    && value.startMs >= 0 && value.endMs >= value.startMs && typeof value.text === 'string')
    .map(({ startMs, endMs, text }) => ({ startMs, endMs, text }));
}

export function readSubtitleSegments(text) {
  const lines = String(text).replace(/^\uFEFF/, '').replace(/\r/g, '').split('\n'), result = [];
  const milliseconds = stamp => {
    const values = stamp.replace(',', '.').split(':').map(Number);
    if (values.length === 2) values.unshift(0);
    return Math.round((values[0] * 3600 + values[1] * 60 + values[2]) * 1000);
  };
  for (let index = 0; index < lines.length; index++) {
    const match = lines[index].match(/^\s*((?:\d+:)?\d{2}:\d{2}[.,]\d{3})\s*-->\s*((?:\d+:)?\d{2}:\d{2}[.,]\d{3})(?:\s.*)?$/);
    if (!match) continue;
    const startMs = milliseconds(match[1]), endMs = milliseconds(match[2]), content = [];
    while (index + 1 < lines.length && lines[index + 1].trim()) content.push(lines[++index]);
    result.push({ startMs, endMs, text: content.join('\n') });
  }
  return normalizeSegments(result);
}

export function subtitleText(segments, format = 'srt') {
  const stamp = milliseconds => {
    const value = Math.max(0, Math.round(milliseconds));
    const seconds = Math.floor(value / 1000);
    return [Math.floor(seconds / 3600), Math.floor(seconds / 60) % 60, seconds % 60]
      .map(part => String(part).padStart(2, '0')).join(':') + (format === 'vtt' ? '.' : ',') + String(value % 1000).padStart(3, '0');
  };
  return (format === 'vtt' ? 'WEBVTT\n\n' : '') + normalizeSegments(segments)
    .map((segment, index) => `${index + 1}\n${stamp(segment.startMs)} --> ${stamp(segment.endMs)}\n${segment.text}\n`).join('\n');
}
