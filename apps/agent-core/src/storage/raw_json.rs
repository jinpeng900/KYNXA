/// Remove structural whitespace without changing property order or escaped UTF-16 bytes.
/// 仅去除结构空白，不改变属性顺序和转义的 UTF-16 字节。
pub fn compact(raw: &str) -> String {
    let mut output = String::with_capacity(raw.len());
    let mut quoted = false;
    let mut escaped = false;
    for character in raw.chars() {
        if quoted {
            output.push(character);
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                quoted = false;
            }
        } else if character == '"' {
            quoted = true;
            output.push(character);
        } else if !matches!(character, ' ' | '\t' | '\r' | '\n') {
            output.push(character);
        }
    }
    output
}

/// Project isolated UTF-16 escapes only for scalar validation; hashes and formal writes keep the original bytes.
/// 仅在标量校验投影中修复孤立 UTF-16 转义，哈希和正式写入保留原始字节。
pub fn scalar_projection(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut projected = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\' && index + 1 < bytes.len() {
            if bytes[index + 1] == b'u' && index + 6 <= bytes.len() {
                if let Some(code) = std::str::from_utf8(&bytes[index + 2..index + 6])
                    .ok()
                    .and_then(|value| u16::from_str_radix(value, 16).ok())
                {
                    if (0xd800..=0xdbff).contains(&code)
                        && index + 12 <= bytes.len()
                        && &bytes[index + 6..index + 8] == b"\\u"
                    {
                        if std::str::from_utf8(&bytes[index + 8..index + 12])
                            .ok()
                            .and_then(|value| u16::from_str_radix(value, 16).ok())
                            .is_some_and(|low| (0xdc00..=0xdfff).contains(&low))
                        {
                            projected.extend_from_slice(&bytes[index..index + 12]);
                            index += 12;
                            continue;
                        }
                    }
                    if (0xd800..=0xdfff).contains(&code) {
                        projected.extend_from_slice(b"\\ufffd");
                        index += 6;
                        continue;
                    }
                }
            }
            projected.extend_from_slice(&bytes[index..index + 2]);
            index += 2;
            continue;
        }
        projected.push(bytes[index]);
        index += 1;
    }
    String::from_utf8(projected).unwrap()
}

/// Preserve UTF-16 identity even when two distinct strings have the same scalar repair projection.
/// 两个字符串即使标量修复投影相同，也按原始 UTF-16 保持不同身份。
pub fn utf16_identity(raw: &str) -> Option<Vec<u16>> {
    let inner = raw.strip_prefix('"')?.strip_suffix('"')?;
    let mut characters = inner.chars();
    let mut identity = Vec::new();
    while let Some(character) = characters.next() {
        if character != '\\' {
            let mut units = [0; 2];
            identity.extend_from_slice(character.encode_utf16(&mut units));
            continue;
        }
        let escaped = characters.next()?;
        let unit = match escaped {
            '"' => b'"' as u16,
            '\\' => b'\\' as u16,
            '/' => b'/' as u16,
            'b' => 8,
            'f' => 12,
            'n' => 10,
            'r' => 13,
            't' => 9,
            'u' => {
                let digits: String = (0..4)
                    .map(|_| characters.next())
                    .collect::<Option<String>>()?;
                u16::from_str_radix(&digits, 16).ok()?
            }
            _ => return None,
        };
        identity.push(unit);
    }
    Some(identity)
}
