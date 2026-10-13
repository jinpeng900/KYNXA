use std::collections::HashSet;

use serde_json::Value;

use crate::models::protocol::{array, string};

fn member_name(node: &Value) -> Option<&Value> {
    if node["type"] != "MemberExpression" {
        return None;
    }
    if node["computed"] == true {
        node["property"].get("value")
    } else {
        node["property"].get("name")
    }
}
fn window(node: &Value, aliases: &HashSet<String>) -> bool {
    if node["type"] == "Identifier" {
        return aliases.contains(string(&node["name"]));
    }
    if node["type"] != "MemberExpression" {
        return false;
    }
    let member = member_name(node).map(string).unwrap_or("");
    ["window", "self", "top", "parent"].contains(&member) && window(&node["object"], aliases)
        || member == "defaultView"
            && node["object"]["type"] == "Identifier"
            && node["object"]["name"] == "document"
        || member == "contentWindow"
}
fn foreground(node: &Value, windows: &HashSet<String>, functions: &HashSet<String>) -> bool {
    node["type"] == "Identifier" && functions.contains(string(&node["name"]))
        || node["type"] == "MemberExpression"
            && (member_name(node).is_some_and(|member| member == "bringToFront")
                || member_name(node)
                    .is_some_and(|member| ["focus", "open"].contains(&string(member)))
                    && window(&node["object"], windows))
}
fn children<'a>(node: &'a Value, stack: &mut Vec<&'a Value>) {
    if let Some(fields) = node.as_object() {
        for value in fields.values().rev() {
            if let Some(items) = value.as_array() {
                stack.extend(items.iter().rev());
            } else if value.is_object() {
                stack.push(value);
            }
        }
    }
}

/// Acorn supplies syntax; native policy follows static aliases and rejects executable foreground requests.
/// Acorn 提供语法；原生政策跟踪静态别名并拒绝可执行的前台请求。
pub fn requests_foreground(syntax: &Value) -> bool {
    let mut windows = ["window", "globalThis", "self"]
        .into_iter()
        .map(str::to_owned)
        .collect::<HashSet<_>>();
    let mut functions = HashSet::new();
    let mut stack = vec![syntax];
    while let Some(node) = stack.pop() {
        if node["type"] == "VariableDeclarator" {
            if node["id"]["type"] == "Identifier" {
                if window(&node["init"], &windows) {
                    windows.insert(string(&node["id"]["name"]).into());
                }
                if foreground(&node["init"], &windows, &functions) {
                    functions.insert(string(&node["id"]["name"]).into());
                }
            }
            if node["id"]["type"] == "ObjectPattern" && window(&node["init"], &windows) {
                for property in array(&node["id"]["properties"]) {
                    let name = property["key"]
                        .get("name")
                        .filter(|value| !value.is_null())
                        .unwrap_or(&property["key"]["value"]);
                    if ["focus", "open"].contains(&string(name))
                        && property["value"]["type"] == "Identifier"
                    {
                        functions.insert(string(&property["value"]["name"]).into());
                    }
                }
            }
        }
        children(node, &mut stack);
    }
    stack.push(syntax);
    while let Some(node) = stack.pop() {
        if node["type"] == "CallExpression" {
            let callee = if node["callee"]["type"] == "ChainExpression" {
                &node["callee"]["expression"]
            } else {
                &node["callee"]
            };
            if foreground(callee, &windows, &functions)
                || callee["type"] == "Identifier"
                    && ["bringToFront", "eval", "Function"].contains(&string(&callee["name"]))
            {
                return true;
            }
            if callee["type"] == "MemberExpression" {
                let member = member_name(callee);
                if member.is_some_and(|member| ["call", "apply", "bind"].contains(&string(member)))
                    && foreground(&callee["object"], &windows, &functions)
                    || window(&callee["object"], &windows)
                        && (member.is_none()
                            || member.is_some_and(|member| {
                                ["eval", "Function"].contains(&string(member))
                            }))
                {
                    return true;
                }
            }
        }
        if node["type"] == "NewExpression"
            && node["callee"]["type"] == "Identifier"
            && node["callee"]["name"] == "Function"
        {
            return true;
        }
        children(node, &mut stack);
    }
    false
}

/// Native JavaScript syntax parsing retains the old foreground alias guard without Acorn/Node.
/// 原生 JavaScript 语法解析保留原前台别名保护，不依赖 Acorn 或 Node。
fn tree_window(node: tree_sitter::Node, source: &str, aliases: &HashSet<String>) -> bool {
    let text = |node: tree_sitter::Node| node.utf8_text(source.as_bytes()).unwrap_or("");
    if node.kind() == "identifier" {
        return aliases.contains(text(node));
    }
    if !["member_expression", "subscript_expression"].contains(&node.kind()) {
        return false;
    }
    let member = node
        .child_by_field_name("property")
        .or_else(|| node.child_by_field_name("index"))
        .map(|node| text(node).trim_matches(['\'', '"']));
    let object = node.child_by_field_name("object");
    member.is_some_and(|member| ["window", "self", "top", "parent"].contains(&member))
        && object.is_some_and(|object| tree_window(object, source, aliases))
        || member == Some("defaultView") && object.is_some_and(|object| text(object) == "document")
        || member == Some("contentWindow")
}
pub(crate) fn script_requests_foreground(source: &str) -> Result<bool, String> {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_javascript::LANGUAGE.into())
        .map_err(|_| "BROWSER_SCRIPT_PARSER_UNAVAILABLE")?;
    let tree = parser
        .parse(source, None)
        .ok_or("BROWSER_SCRIPT_PARSER_UNAVAILABLE")?;
    if tree.root_node().has_error() {
        return Err("BROWSER_SCRIPT_SYNTAX_INVALID".into());
    }
    let mut nodes = Vec::new();
    let mut pending = vec![tree.root_node()];
    while let Some(node) = pending.pop() {
        nodes.push(node);
        let mut cursor = node.walk();
        pending.extend(node.named_children(&mut cursor));
    }
    let text = |node: tree_sitter::Node| node.utf8_text(source.as_bytes()).unwrap_or("");
    let member = |node: tree_sitter::Node| {
        node.child_by_field_name("property")
            .or_else(|| node.child_by_field_name("index"))
            .map(|property| text(property).trim_matches(['\'', '"']))
    };
    let mut windows = ["window", "globalThis", "self", "top", "parent"]
        .into_iter()
        .map(str::to_owned)
        .collect::<HashSet<_>>();
    let mut functions = HashSet::new();
    for _ in 0..4 {
        let previous = windows.len() + functions.len();
        for node in &nodes {
            if node.kind() != "variable_declarator" {
                continue;
            }
            let (Some(name), Some(value)) = (
                node.child_by_field_name("name"),
                node.child_by_field_name("value"),
            ) else {
                continue;
            };
            if name.kind() == "identifier" {
                if tree_window(value, source, &windows) {
                    windows.insert(text(name).to_owned());
                }
                if ["member_expression", "subscript_expression"].contains(&value.kind())
                    && value
                        .child_by_field_name("object")
                        .is_some_and(|object| tree_window(object, source, &windows))
                    && member(value).is_some_and(|member| ["focus", "open"].contains(&member))
                {
                    functions.insert(text(name).to_owned());
                }
                if functions.contains(text(value)) {
                    functions.insert(text(name).to_owned());
                }
            } else if name.kind() == "object_pattern" && tree_window(value, source, &windows) {
                let mut cursor = name.walk();
                for entry in name.named_children(&mut cursor) {
                    if entry.kind() == "pair_pattern" {
                        if let (Some(key), Some(value)) = (
                            entry.child_by_field_name("key"),
                            entry.child_by_field_name("value"),
                        ) {
                            if ["focus", "open"].contains(&text(key).trim_matches(['\'', '"'])) {
                                functions.insert(text(value).to_owned());
                            }
                        }
                    } else if ["focus", "open"].contains(&text(entry)) {
                        functions.insert(text(entry).to_owned());
                    }
                }
            }
        }
        if previous == windows.len() + functions.len() {
            break;
        }
    }
    for node in nodes {
        if !["call_expression", "new_expression"].contains(&node.kind()) {
            continue;
        }
        let Some(callee) = node
            .child_by_field_name("function")
            .or_else(|| node.child_by_field_name("constructor"))
        else {
            continue;
        };
        if functions.contains(text(callee))
            || ["eval", "Function", "bringToFront"].contains(&text(callee))
        {
            return Ok(true);
        }
        if ["member_expression", "subscript_expression"].contains(&callee.kind()) {
            let object = callee.child_by_field_name("object");
            let name = member(callee);
            if name == Some("bringToFront")
                || object.is_some_and(|object| tree_window(object, source, &windows))
                    && (name.is_none()
                        || name.is_some_and(|name| {
                            ["focus", "open", "eval", "Function"].contains(&name)
                        }))
            {
                return Ok(true);
            }
            if name.is_some_and(|name| ["call", "apply", "bind"].contains(&name))
                && object.is_some_and(|object| {
                    functions.contains(text(object))
                        || object
                            .child_by_field_name("object")
                            .is_some_and(|target| tree_window(target, source, &windows))
                            && member(object).is_some_and(|name| ["focus", "open"].contains(&name))
                })
            {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

#[cfg(test)]
mod native_syntax_tests {
    #[test]
    fn native_foreground_guard_follows_aliases_and_preserves_input_focus() {
        for source in [
            "const w=window.top;w.focus()",
            "const {open:launch}=globalThis;launch('url')",
            "const f=document.defaultView.open;f.call(null,'url')",
            "window['focus']()",
            "const focus=window.focus;focus()",
        ] {
            assert!(
                super::script_requests_foreground(source).unwrap(),
                "{source}"
            );
        }
        for source in [
            "document.querySelector('input').focus()",
            "const message='window.open()'; console.log(message)",
            "({focus:()=>false}).focus()",
        ] {
            assert!(
                !super::script_requests_foreground(source).unwrap(),
                "{source}"
            );
        }
        assert!(super::script_requests_foreground("const = {").is_err());
    }
}
