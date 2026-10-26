//! Semantic discovery supplements existing lexical candidates; it never grants tool authority.
//! 语义发现仅补充现有词法候选，不赋予工具执行权限。

use std::collections::{HashMap, HashSet};

use serde_json::{Value, json};

/// Embed only the active request; excluded clauses must not pull rejected capabilities back in.
/// 仅嵌入当前有效请求，被排除的语句不能把已拒绝能力重新带回候选。
pub(super) fn query_text(query: &str) -> Option<String> {
    let projection = super::intent::analyze_clauses(query);
    let active = projection["activeText"].as_str().unwrap_or(query).trim();
    if active.is_empty() || active.len() > 16_000 { return None; }
    let exact = active.is_ascii() && !active.contains(char::is_whitespace)
        && (active.contains('.') || active.starts_with("k_"));
    (!exact).then(|| active.to_owned())
}

pub(super) fn descriptor_text(descriptor: &Value) -> Option<String> {
    if descriptor["enabled"] == false || descriptor["available"] == false
        || descriptor["runtimeCompatible"] == false { return None; }
    let name = descriptor["name"].as_str()?;
    let description = descriptor["description"].as_str().unwrap_or("");
    Some(format!("{name}\n{description}"))
}

pub(super) fn cosine(left: &[f32], right: &[f32]) -> Option<f64> {
    if left.is_empty() || left.len() != right.len() { return None; }
    let (mut dot, mut a, mut b) = (0.0f64, 0.0f64, 0.0f64);
    for (&left, &right) in left.iter().zip(right) {
        if !left.is_finite() || !right.is_finite() { return None; }
        let (left,right) = (f64::from(left),f64::from(right));
        dot += left * right; a += left * left; b += right * right;
    }
    (a > 0.0 && b > 0.0).then(|| (dot / (a.sqrt() * b.sqrt())).clamp(-1.0, 1.0))
}

pub(super) fn combine(query: &str, descriptors: &[Value], lexical: &[Value], semantic: &[(usize,f64)]) -> Vec<Value> {
    let projection = super::intent::analyze_clauses(query);
    let active = projection["activeText"].as_str().unwrap_or(query);
    let excluded = projection["excludedClauses"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    let rejected = |index: usize| descriptors.get(index).is_none_or(|tool| {
        let name = tool["name"].as_str().unwrap_or("");
        descriptor_text(tool).is_none() || (!name.is_empty() && !active.contains(name)
            && excluded.iter().any(|clause| clause["text"].as_str().is_some_and(|text| text.contains(name))))
    });
    let mut semantic = semantic.iter().copied()
        .filter(|(index,score)| score.is_finite() && *score >= 0.55 && !rejected(*index))
        .collect::<Vec<_>>();
    semantic.sort_by(|a,b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    // This bounds added hints, not discoverable or loadable tools; all lexical results remain intact.
    // 此处仅限制新增语义提示的数量，不限制可发现或可加载能力；完整词法结果继续保留。
    semantic.truncate(12);
    let semantic_ranks: HashMap<usize,(usize,f64)> = semantic.iter().enumerate()
        .map(|(rank,(index,score))| (*index,(rank,*score))).collect();
    let mut selected = Vec::new();
    let mut seen = HashSet::new();
    for (rank,hit) in lexical.iter().enumerate() {
        let Some(index) = hit.as_u64().or_else(||hit["index"].as_u64()).map(|v|v as usize)
            .filter(|index|*index < descriptors.len()) else { continue; };
        if !seen.insert(index) { continue; }
        let original = hit["score"].as_u64().unwrap_or(0);
        let score = if original >= 5000 {
            // Exact names and existing strong path/action rules keep their established priority.
            // 完整工具名及既有明确路径、动作规则保留原先的优先级。
            original
        } else {
            let fused = 0.9 / (40 + rank + 1) as f64
                + semantic_ranks.get(&index).map_or(0.0, |(rank,_)| 1.0 / (40 + rank + 1) as f64);
            (fused * 100_000.0) as u64
        };
        selected.push(json!({"index":index,"score":score,"lexicalScore":original,
            "semanticSimilarity":semantic_ranks.get(&index).map(|(_,score)|score)}));
    }
    for (rank,(index,score)) in semantic.iter().enumerate() {
        if seen.insert(*index) {
            selected.push(json!({"index":index,"score":(100_000.0 / (40 + rank + 1) as f64) as u64,
                "lexicalScore":0,"semanticSimilarity":score}));
        }
    }
    selected.sort_by(|left,right| right["score"].as_u64().cmp(&left["score"].as_u64())
        .then(left["index"].as_u64().cmp(&right["index"].as_u64())));
    selected
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_tool_discovery_keeps_explicit_names_and_adds_cross_language_candidates() {
        let descriptors = vec![json!({"name":"filesystem.read","description":"Read a local file"}),
            json!({"name":"mcp.blender.scene","description":"Inspect the current Blender scene"})];
        let lexical = vec![json!({"index":0,"score":100000})];
        let merged = combine("查看场景，使用 filesystem.read 读文件",&descriptors,&lexical,&[(1,0.88)]);
        assert_eq!(merged[0]["index"],0); assert_eq!(merged[1]["index"],1);
        let cross_language = combine("检查三维场景",&descriptors,&[],&[(1,0.88)]);
        assert_eq!(cross_language[0]["index"],1);
        assert_eq!(lexical[0]["score"],100000, "Input declarations are not reordered or rewritten.");
    }

    #[test]
    fn semantic_tool_discovery_ignores_exclusions_disabled_cards_and_invalid_vectors() {
        let descriptors = vec![json!({"name":"filesystem.write"}),json!({"name":"mcp.blender.scene","enabled":false}),
            json!({"name":"filesystem.read"})];
        let merged = combine("不要使用 filesystem.write。改用 filesystem.read 读取内容。",&descriptors,
            &[json!({"index":2,"score":100000})],&[(0,0.99),(1,0.98),(2,0.9),(9,0.99)]);
        assert_eq!(merged.len(),1); assert_eq!(merged[0]["index"],2);
        assert!(query_text("filesystem.read").is_none());
        assert!(query_text("读取合同内容").is_some());
        assert_eq!(cosine(&[1.0,0.0],&[1.0,0.0]),Some(1.0));
        assert!(cosine(&[0.0],&[0.0]).is_none());
        assert!(cosine(&[f32::NAN],&[1.0]).is_none());
    }
}
