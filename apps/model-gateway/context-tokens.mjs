/** Conservative heuristic, not a provider tokenizer. Requests also reserve safety headroom. */
export function estimateTokens(text) {
  let tokens = 0, ascii = 0, whitespace = 0;
  const flush = () => {
    tokens += (ascii > 24 ? ascii : Math.ceil(ascii / 3)) + Math.ceil(whitespace / 6);
    ascii = 0; whitespace = 0;
  };
  for (const character of String(text ?? '')) {
    if (/[a-z0-9_]/i.test(character)) {
      if (whitespace) flush();
      ascii++;
    } else if (character === '\n' || character === '\r' || character === '\t') {
      flush(); tokens++;
    } else if (/\s/u.test(character)) {
      if (ascii) flush();
      whitespace++;
    } else {
      flush();
      const code = character.codePointAt(0);
      tokens += code >= 0x10000 ? 4 : code >= 0x2e80 && code <= 0xd7ff ? 2 : code > 0x7f ? Buffer.byteLength(character) : 1;
    }
  }
  flush();
  return tokens;
}

export function estimateMessageTokens(messages, system = '') {
  return messages.reduce((sum, item) => sum + 8 + estimateTokens(item.content), 0)
    + (system ? 8 + estimateTokens(system) : 0);
}
