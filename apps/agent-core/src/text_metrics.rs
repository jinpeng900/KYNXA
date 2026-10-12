/// Match the existing code-point heuristic; this is not a provider tokenizer.
/// 保持既有按 Unicode 码点的保守估算；该算法不是供应商分词器。
pub fn estimate_tokens(text: &str) -> u64 {
    let (mut tokens, mut ascii, mut whitespace) = (0_u64, 0_u64, 0_u64);
    let flush = |tokens: &mut u64, ascii: &mut u64, whitespace: &mut u64| {
        *tokens += if *ascii > 24 {
            *ascii
        } else {
            ascii.div_ceil(3)
        };
        *tokens += whitespace.div_ceil(6);
        *ascii = 0;
        *whitespace = 0;
    };
    for character in text.chars() {
        if character.is_ascii_alphanumeric() || character == '_' {
            if whitespace > 0 {
                flush(&mut tokens, &mut ascii, &mut whitespace);
            }
            ascii += 1;
        } else if matches!(character, '\n' | '\r' | '\t') {
            flush(&mut tokens, &mut ascii, &mut whitespace);
            tokens += 1;
        } else if character.is_whitespace() && character != '\u{0085}' || character == '\u{feff}' {
            if ascii > 0 {
                flush(&mut tokens, &mut ascii, &mut whitespace);
            }
            whitespace += 1;
        } else {
            flush(&mut tokens, &mut ascii, &mut whitespace);
            let code = character as u32;
            tokens += if code >= 0x10000 {
                4
            } else if (0x2e80..=0xd7ff).contains(&code) {
                2
            } else if code > 0x7f {
                character.len_utf8() as u64
            } else {
                1
            };
        }
    }
    flush(&mut tokens, &mut ascii, &mut whitespace);
    tokens
}
