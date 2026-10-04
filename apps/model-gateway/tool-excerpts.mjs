/**
 * Keep both diagnostic ends; the omission marker never disguises this as complete output.
 * 保留诊断开头和结尾，省略标记明确说明内容不完整。
 */
export function toolOutputExcerpt(value, maximumCharacters) {
  const text = String(value ?? '');
  if (maximumCharacters <= 0) return '';
  if (text.length <= maximumCharacters) return text;
  const marker = '\n… [middle omitted] …\n';
  if (maximumCharacters <= marker.length) return marker.slice(0, maximumCharacters);
  const available = maximumCharacters - marker.length;
  let head = Math.ceil(available / 2), tail = text.length - Math.floor(available / 2);
  if (/[\uD800-\uDBFF]/.test(text[head - 1] ?? '')) head--;
  if (/[\uDC00-\uDFFF]/.test(text[tail] ?? '')) tail++;
  return text.slice(0, head) + marker + text.slice(tail);
}
