//! Bounded, non-executing inspection before Blender is allowed to open a source.
//! SDNA member sizes are explicit (including padding); never infer paths from mesh bytes.
use std::{borrow::Cow, collections::HashMap, io::Read};

const LIMIT: usize = 256 * 1024 * 1024;
type Checked<T> = Result<T, &'static str>;

fn unpack(bytes: &[u8]) -> Checked<Cow<'_, [u8]>> {
    if bytes.len() > LIMIT { return Err("blend source exceeds limit"); }
    if bytes.starts_with(b"BLENDER") { return Ok(Cow::Borrowed(bytes)); }
    let reader: Box<dyn Read + '_> = if bytes.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
        let mut decoder = zstd::stream::read::Decoder::new(bytes).map_err(|_| "invalid zstd")?;
        decoder.window_log_max(28).map_err(|_| "zstd window exceeds limit")?;
        Box::new(decoder)
    } else if bytes.starts_with(&[0x1f, 0x8b]) {
        Box::new(flate2::read::MultiGzDecoder::new(bytes))
    } else { return Err("unsupported blend encoding"); };
    let mut output = Vec::new();
    reader.take(LIMIT as u64 + 1).read_to_end(&mut output).map_err(|_| "invalid compressed blend")?;
    if output.len() > LIMIT { return Err("expanded blend exceeds limit"); }
    Ok(Cow::Owned(output))
}

struct Reader<'a> { bytes: &'a [u8], at: usize, little: bool }
impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Checked<&'a [u8]> {
        let end = self.at.checked_add(n).ok_or("overflow")?;
        let out = self.bytes.get(self.at..end).ok_or("truncated blend")?;
        self.at = end; Ok(out)
    }
    fn u16(&mut self) -> Checked<usize> {
        let v = self.take(2)?.try_into().unwrap();
        Ok(if self.little { u16::from_le_bytes(v) } else { u16::from_be_bytes(v) } as usize)
    }
    fn u32(&mut self) -> Checked<usize> {
        let v = self.take(4)?.try_into().unwrap();
        Ok(if self.little { u32::from_le_bytes(v) } else { u32::from_be_bytes(v) } as usize)
    }
    fn u64(&mut self) -> Checked<usize> {
        let v = self.take(8)?.try_into().unwrap();
        usize::try_from(if self.little { u64::from_le_bytes(v) } else { u64::from_be_bytes(v) }).map_err(|_| "overflow")
    }
    fn tag(&mut self, tag: &[u8]) -> Checked<()> { if self.take(tag.len())? == tag { Ok(()) } else { Err("invalid SDNA tag") } }
    fn align(&mut self) -> Checked<()> { self.take((4 - self.at % 4) % 4).map(|_| ()) }
    fn strings(&mut self) -> Checked<Vec<&'a str>> {
        let count = self.u32()?;
        if count > 65_536 { return Err("SDNA string limit"); }
        let mut result = Vec::with_capacity(count);
        for _ in 0..count {
            let tail = self.bytes.get(self.at..).ok_or("truncated strings")?;
            let len = tail.iter().take(4097).position(|v| *v == 0).ok_or("invalid SDNA string")?;
            let value = std::str::from_utf8(self.take(len + 1)?).map_err(|_| "invalid SDNA text")?;
            result.push(&value[..len]);
        }
        self.align()?; Ok(result)
    }
}

struct Block<'a> { code: &'a [u8], body: &'a [u8], sdna: usize, count: usize }
struct Member<'a> { ty: usize, name: &'a str, offset: usize, size: usize, array: usize, pointer: bool }
struct Structure<'a> { ty: usize, members: Vec<Member<'a>> }
struct Dna<'a> { types: Vec<&'a str>, sizes: Vec<usize>, structs: Vec<Structure<'a>>, by_type: HashMap<usize, usize> }

fn array_len(name: &str) -> Checked<usize> {
    let mut result = 1usize;
    let mut rest = name;
    while let Some((_, tail)) = rest.split_once('[') {
        let (count, end) = tail.split_once(']').ok_or("invalid array")?;
        let count = count.parse::<usize>().map_err(|_| "invalid array size")?;
        if count == 0 { return Err("zero array size"); }
        result = result.checked_mul(count).filter(|v| *v <= LIMIT).ok_or("array limit")?;
        rest = end;
    }
    Ok(result)
}

fn dna(bytes: &[u8], little: bool, pointers: usize) -> Checked<Dna<'_>> {
    let mut r = Reader { bytes, at: 0, little };
    r.tag(b"SDNANAME")?; let names = r.strings()?;
    r.tag(b"TYPE")?; let types = r.strings()?;
    r.tag(b"TLEN")?;
    let sizes = (0..types.len()).map(|_| r.u16()).collect::<Checked<Vec<_>>>()?;
    r.align()?; r.tag(b"STRC")?;
    let count = r.u32()?;
    if count > 65_536 { return Err("SDNA structure limit"); }
    let mut structs = Vec::with_capacity(count); let mut by_type = HashMap::new();
    let mut total_members = 0usize;
    for index in 0..count {
        let ty = r.u16()?; let count = r.u16()?;
        let expected = *sizes.get(ty).ok_or("invalid struct type")?;
        total_members = total_members.checked_add(count).filter(|n| *n <= 1_000_000).ok_or("SDNA member limit")?;
        // Blender 5.x reserves SDNA index zero for the empty raw_data pseudo-type.
        if (expected == 0 && count != 0) || by_type.insert(ty, index).is_some() { return Err("invalid struct size/type"); }
        let mut members = Vec::with_capacity(count); let mut offset = 0usize;
        for _ in 0..count {
            let ty = r.u16()?; let name = *names.get(r.u16()?).ok_or("invalid member name")?;
            let unit = *sizes.get(ty).ok_or("invalid member type")?;
            let pointer = name.starts_with('*') || name.starts_with("(*");
            let array = array_len(name)?;
            let size = (if pointer { pointers } else { unit }).checked_mul(array).ok_or("member size overflow")?;
            members.push(Member { ty, name, offset, size, array, pointer });
            offset = offset.checked_add(size).filter(|n| *n <= expected).ok_or("member exceeds structure")?;
        }
        if offset != expected { return Err("SDNA layout does not match size"); }
        structs.push(Structure { ty, members });
    }
    if r.at != bytes.len() { return Err("trailing SDNA data"); }
    Ok(Dna { types, sizes, structs, by_type })
}

fn harmless_path(ty: &str, name: &str) -> bool {
    // Exact fields only: render/bake destinations, saved UI navigation and asset provenance.
    // LibraryWeakReference is metadata; a real linked Library remains forbidden below.
    matches!((ty, name), ("RenderData", "pic") | ("BakeData", "filepath") |
        ("FileGlobal", "filename") | ("FileSelectParams", "dir" | "file" | "renamefile") |
        ("LibraryWeakReference", "library_filepath"))
}

fn inspect(dna: &Dna<'_>, index: usize, body: &[u8], depth: usize, visits: &mut usize) -> Checked<()> {
    if depth > 24 || *visits > 1_000_000 { return Err("blend inspection limit"); }
    let s = dna.structs.get(index).ok_or("invalid SDNA index")?;
    let ty = dna.types[s.ty];
    // These can resolve paths indirectly or evaluate imported data during GLB export.
    if matches!(ty, "Library" | "CacheFile" | "MovieClip" | "bSound" | "Volume" | "Text" | "CustomDataExternal") {
        return Err("external data type requires explicit conversion");
    }
    for f in &s.members {
        *visits += 1;
        if f.pointer { continue; }
        let data = body.get(f.offset..f.offset + f.size).ok_or("truncated structure")?;
        let name = f.name.split('[').next().unwrap_or(f.name);
        if dna.types[f.ty] == "char" && f.array > 1 {
            if harmless_path(ty, name) { continue; }
            let end = data.iter().position(|b| *b == 0).unwrap_or(data.len());
            let text = &data[..end];
            if text.is_empty() { continue; }
            let known_input = matches!((ty, name), ("Image", "name") | ("VFont", "name") |
                ("NodeShaderScript", "filepath") | ("NodeShaderTexIES", "filepath") |
                ("bNodeSocketValueString", "value"));
            if ty == "VFont" && text == b"<builtin>" { continue; }
            let path_field = name.to_ascii_lowercase();
            let path_field = path_field.contains("filepath") || path_field.contains("filename") || path_field.contains("directory") || path_field.contains("cachepath");
            if known_input || path_field || text.contains(&b'/') || text.contains(&b'\\') || text.windows(2).any(|v| v[0].is_ascii_alphabetic() && v[1] == b':') {
                return Err("external input path");
            }
            if ty == "bNodeTree" && name == "idname" && text == b"GeometryNodeTree" { return Err("geometry import evaluation requires explicit conversion"); }
        } else if let Some(nested) = dna.by_type.get(&f.ty) {
            let unit = dna.sizes[f.ty];
            if unit == 0 { return Err("zero struct size"); }
            for item in data.chunks_exact(unit) { inspect(dna, *nested, item, depth + 1, visits)?; }
        }
    }
    Ok(())
}

pub(super) fn self_contained(bytes: &[u8]) -> Checked<()> {
    let source = unpack(bytes)?;
    let bytes = source.as_ref();
    if !bytes.starts_with(b"BLENDER") { return Err("invalid blend header"); }
    let (header, pointers, little, large) = if bytes.get(7..13) == Some(b"17-01v") {
        if !bytes.get(13..17).is_some_and(|v| v.iter().all(u8::is_ascii_digit)) { return Err("invalid format version"); }
        (17, 8, true, true)
    } else {
        let pointers = match bytes.get(7) { Some(b'_') => 4, Some(b'-') => 8, _ => return Err("unsupported blend header") };
        let little = match bytes.get(8) { Some(b'v') => true, Some(b'V') => false, _ => return Err("unsupported endianness") };
        if !bytes.get(9..12).is_some_and(|v| v.iter().all(u8::is_ascii_digit)) { return Err("invalid format version"); }
        (12, pointers, little, false)
    };
    let mut r = Reader { bytes, at: header, little }; let mut blocks = Vec::new(); let mut ended = false;
    while r.at < bytes.len() {
        if blocks.len() >= 1_000_000 { return Err("block limit"); }
        let code = r.take(4)?;
        let (length, sdna, count) = if large {
            let sdna = r.u32()?;
            r.take(pointers)?; // old address
            let length = r.u64()?; let count = r.u64()?;
            (length, sdna, count)
        } else {
            let length = r.u32()?; r.take(pointers)?;
            let sdna = r.u32()?; let count = r.u32()?; (length, sdna, count)
        };
        if length > LIMIT { return Err("block length limit"); }
        let body = r.take(length)?;
        if code == b"ENDB" {
            if length != 0 || r.at != bytes.len() { return Err("invalid blend end"); }
            ended = true; break;
        }
        blocks.push(Block { code, body, sdna, count });
    }
    if !ended { return Err("missing blend end"); }
    let mut schemas = blocks.iter().filter(|b| b.code == b"DNA1");
    let schema = schemas.next().ok_or("missing SDNA")?;
    if schemas.next().is_some() { return Err("duplicate SDNA"); }
    let dna = dna(schema.body, little, pointers)?;
    let mut visits = 0;
    for block in &blocks {
        if matches!(block.code, b"DNA1" | b"REND" | b"TEST") || block.sdna == 0 { continue; }
        let structure = dna.structs.get(block.sdna).ok_or("invalid block SDNA")?;
        let size = dna.sizes[structure.ty];
        if size == 0 { return Err("zero block structure size"); }
        if size.checked_mul(block.count) != Some(block.body.len()) {
            #[cfg(test)] eprintln!("blend block mismatch: {:?} {} size={} count={} bytes={}", block.code, dna.types[structure.ty], size, block.count, block.body.len());
            return Err("block structure count mismatch");
        }
        for body in block.body.chunks_exact(size) { inspect(&dna, block.sdna, body, 0, &mut visits)?; }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_decoding_and_real_sample_sdna() {
        assert!(self_contained(b"BLENDER-v300").is_err());
        assert!(self_contained(&[0x28, 0xb5, 0x2f, 0xfd, 0xff]).is_err());
        assert!(harmless_path("RenderData", "pic"));
        assert!(!harmless_path("Image", "name"));
        assert!(!harmless_path("Scene", "filepath"));
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Samples/robot.blend");
        if root.is_file() {
            let bytes = std::fs::read(root).unwrap();
            assert_eq!(self_contained(&bytes), Ok(()));
            let mut uncompressed = unpack(&bytes).unwrap().into_owned();
            let old = b"robot-render.png";
            let at = uncompressed.windows(old.len()).position(|part| part == old).unwrap();
            uncompressed[at..at + old.len()].copy_from_slice(b"private-file.png");
            assert_eq!(self_contained(&uncompressed), Ok(())); // arbitrary legal output path
            let at = uncompressed.windows(11).position(|part| part == b"RenderData\0").unwrap();
            uncompressed[at + 9] = b'z';
            assert!(self_contained(&uncompressed).is_err()); // exception belongs to the exact type
        }
    }
    #[test]
    fn input_fields_reject_even_bare_filenames() {
        for (ty, name, text) in [("Image", "name[32]", b"secret.png".as_slice()), ("NodeShaderScript", "filepath[32]", b"code.osl"), ("bNodeSocketValueString", "value[32]", b"secret.csv")] {
            let dna = Dna { types: vec!["char", ty], sizes: vec![1, 32], structs: vec![Structure { ty: 1, members: vec![Member { ty: 0, name, offset: 0, size: 32, array: 32, pointer: false }] }], by_type: HashMap::new() };
            let mut body = [0u8; 32]; body[..text.len()].copy_from_slice(text);
            assert!(inspect(&dna, 0, &body, 0, &mut 0).is_err(), "{ty}");
        }
    }
}
