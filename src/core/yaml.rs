use serde_json::{Map, Number, Value};

use super::errors::WbError;

pub fn to_yaml_string(val: &Value) -> String {
    let mut out = String::new();
    write_yaml_value(val, 0, &mut out);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

fn needs_quotes(s: &str) -> bool {
    if s.is_empty() {
        return true;
    }
    if s.trim() != s {
        return true;
    }
    if matches!(
        s.to_ascii_lowercase().as_str(),
        "null" | "~" | "true" | "false" | "yes" | "no" | "on" | "off"
    ) {
        return true;
    }
    if s.parse::<i64>().is_ok() || s.parse::<f64>().is_ok() {
        return true;
    }
    if let Some(first) = s.chars().next() {
        if matches!(
            first,
            '-' | '[' | ']' | '{' | '}' | '*' | '&' | '!' | '|' | '>' | '%' | '@' | '`' | '?' | '#' | '\'' | '"'
        ) {
            return true;
        }
    }
    if s.contains(": ") || s.ends_with(':') || s.contains(" #") || s.contains('\n') || s.contains('\r') || s.contains('\t') {
        return true;
    }
    false
}

fn format_scalar(val: &Value) -> String {
    match val {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => {
            if needs_quotes(s) {
                serde_json::to_string(s).unwrap_or_else(|_| format!("\"{}\"", s))
            } else {
                s.clone()
            }
        }
        Value::Array(arr) if arr.is_empty() => "[]".to_string(),
        Value::Object(obj) if obj.is_empty() => "{}".to_string(),
        _ => unreachable!("non-empty container handled by block writer"),
    }
}

fn format_key(k: &str) -> String {
    if needs_quotes(k) || k.contains(':') {
        serde_json::to_string(k).unwrap_or_else(|_| format!("\"{}\"", k))
    } else {
        k.to_string()
    }
}

fn write_indent(out: &mut String, indent: usize) {
    for _ in 0..indent {
        out.push(' ');
    }
}

fn is_empty_or_scalar(val: &Value) -> bool {
    match val {
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => true,
        Value::Array(a) => a.is_empty(),
        Value::Object(m) => m.is_empty(),
    }
}

fn write_yaml_value(val: &Value, indent: usize, out: &mut String) {
    match val {
        Value::Object(map) => {
            if map.is_empty() {
                out.push_str("{}");
                return;
            }
            for (idx, (k, v)) in map.iter().enumerate() {
                if idx > 0 {
                    out.push('\n');
                }
                write_indent(out, indent);
                out.push_str(&format_key(k));
                out.push(':');
                if is_empty_or_scalar(v) {
                    out.push(' ');
                    out.push_str(&format_scalar(v));
                } else {
                    out.push('\n');
                    write_yaml_value(v, indent + 2, out);
                }
            }
        }
        Value::Array(arr) => {
            if arr.is_empty() {
                out.push_str("[]");
                return;
            }
            for (idx, item) in arr.iter().enumerate() {
                if idx > 0 {
                    out.push('\n');
                }
                write_indent(out, indent);
                out.push('-');
                if is_empty_or_scalar(item) {
                    out.push(' ');
                    out.push_str(&format_scalar(item));
                } else if let Value::Object(map) = item {
                    out.push(' ');
                    let mut entries = map.iter();
                    if let Some((first_k, first_v)) = entries.next() {
                        out.push_str(&format_key(first_k));
                        out.push(':');
                        if is_empty_or_scalar(first_v) {
                            out.push(' ');
                            out.push_str(&format_scalar(first_v));
                        } else {
                            out.push('\n');
                            write_yaml_value(first_v, indent + 4, out);
                        }
                        for (k, v) in entries {
                            out.push('\n');
                            write_indent(out, indent + 2);
                            out.push_str(&format_key(k));
                            out.push(':');
                            if is_empty_or_scalar(v) {
                                out.push(' ');
                                out.push_str(&format_scalar(v));
                            } else {
                                out.push('\n');
                                write_yaml_value(v, indent + 4, out);
                            }
                        }
                    }
                } else {
                    out.push('\n');
                    write_yaml_value(item, indent + 2, out);
                }
            }
        }
        _ => {
            write_indent(out, indent);
            out.push_str(&format_scalar(val));
        }
    }
}

#[derive(Debug, Clone)]
struct Line<'a> {
    lineno: usize,
    indent: usize,
    text: &'a str,
}

fn strip_comment(raw: &str) -> Result<&str, WbError> {
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    let bytes = raw.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if in_double {
            if escaped {
                escaped = false;
                continue;
            }
            if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_double = false;
            }
        } else if in_single {
            if b == b'\'' {
                in_single = false;
            }
        } else if b == b'"' {
            in_double = true;
        } else if b == b'\'' {
            in_single = true;
        } else if b == b'#' && (i == 0 || bytes[i - 1].is_ascii_whitespace()) {
            return Ok(raw[..i].trim_end());
        }
    }
    if in_single || in_double {
        return Err(WbError::validation(format!("YAML 存在未闭合的引号: {raw}")));
    }
    Ok(raw.trim_end())
}

fn preprocess_lines(raw: &str) -> Result<Vec<Line<'_>>, WbError> {
    let raw = raw.strip_prefix('\u{feff}').unwrap_or(raw);
    let mut lines = Vec::new();
    for (idx, line) in raw.lines().enumerate() {
        let lineno = idx + 1;
        let leading = line.len() - line.trim_start_matches([' ', '\t']).len();
        if line[..leading].contains('\t') {
            return Err(WbError::validation(format!("YAML 第 {lineno} 行禁止使用 Tab 缩进")));
        }
        let cleaned = strip_comment(line)?;
        let trimmed = cleaned.trim();
        if trimmed.is_empty() || trimmed == "---" || trimmed == "..." {
            continue;
        }
        lines.push(Line {
            lineno,
            indent: leading,
            text: trimmed,
        });
    }
    Ok(lines)
}

fn split_key_value(text: &str) -> Option<(&str, &str)> {
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    let mut bracket_depth = 0usize;
    let mut brace_depth = 0usize;
    let bytes = text.as_bytes();

    for (i, &b) in bytes.iter().enumerate() {
        if in_double {
            if escaped {
                escaped = false;
                continue;
            }
            if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_double = false;
            }
        } else if in_single {
            if b == b'\'' {
                in_single = false;
            }
        } else {
            match b {
                b'"' => in_double = true,
                b'\'' => in_single = true,
                b'[' => bracket_depth += 1,
                b']' => bracket_depth = bracket_depth.saturating_sub(1),
                b'{' => brace_depth += 1,
                b'}' => brace_depth = brace_depth.saturating_sub(1),
                b':' if bracket_depth == 0 && brace_depth == 0 => {
                    let after = &text[i + 1..];
                    if after.is_empty() || after.starts_with(' ') || after.starts_with('\t') {
                        return Some((text[..i].trim(), after.trim()));
                    }
                }
                _ => {}
            }
        }
    }
    None
}

fn parse_key(raw: &str, lineno: usize) -> Result<String, WbError> {
    let val = parse_scalar(raw, lineno)?;
    match val {
        Value::String(s) => Ok(s),
        Value::Number(n) => Ok(n.to_string()),
        Value::Bool(b) => Ok(b.to_string()),
        Value::Null => Ok("null".to_string()),
        _ => Err(WbError::validation(format!("YAML 第 {lineno} 行键名无效: {raw}"))),
    }
}

fn parse_single_quoted(raw: &str, lineno: usize) -> Result<String, WbError> {
    if raw.len() < 2 || !raw.starts_with('\'') || !raw.ends_with('\'') {
        return Err(WbError::validation(format!("YAML 第 {lineno} 行单引号未闭合")));
    }
    let inner = &raw[1..raw.len() - 1];
    Ok(inner.replace("''", "'"))
}

fn parse_flow_list(raw: &str, lineno: usize) -> Result<Value, WbError> {
    if !raw.starts_with('[') || !raw.ends_with(']') {
        return Err(WbError::validation(format!("YAML 第 {lineno} 行列表语法未闭合: {raw}")));
    }
    let inner = raw[1..raw.len() - 1].trim();
    if inner.is_empty() {
        return Ok(Value::Array(Vec::new()));
    }
    let items = split_flow_items(inner, lineno)?;
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        out.push(parse_inline_value(item, lineno)?);
    }
    Ok(Value::Array(out))
}

fn parse_flow_map(raw: &str, lineno: usize) -> Result<Value, WbError> {
    if !raw.starts_with('{') || !raw.ends_with('}') {
        return Err(WbError::validation(format!("YAML 第 {lineno} 行对象语法未闭合: {raw}")));
    }
    let inner = raw[1..raw.len() - 1].trim();
    if inner.is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    let items = split_flow_items(inner, lineno)?;
    let mut map = Map::new();
    for item in items {
        let (k_raw, v_raw) = split_key_value(item).ok_or_else(|| {
            WbError::validation(format!("YAML 第 {lineno} 行流式对象缺少冒号: {item}"))
        })?;
        let key = parse_key(k_raw, lineno)?;
        let val = parse_inline_value(v_raw, lineno)?;
        map.insert(key, val);
    }
    Ok(Value::Object(map))
}

fn split_flow_items(inner: &str, lineno: usize) -> Result<Vec<&str>, WbError> {
    let mut items = Vec::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    let mut depth_sq = 0usize;
    let mut depth_cur = 0usize;
    let mut start = 0usize;

    for (i, &b) in inner.as_bytes().iter().enumerate() {
        if in_double {
            if escaped {
                escaped = false;
                continue;
            }
            if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_double = false;
            }
        } else if in_single {
            if b == b'\'' {
                in_single = false;
            }
        } else {
            match b {
                b'"' => in_double = true,
                b'\'' => in_single = true,
                b'[' => depth_sq += 1,
                b']' => {
                    if depth_sq == 0 {
                        return Err(WbError::validation(format!("YAML 第 {lineno} 行括号不匹配")));
                    }
                    depth_sq -= 1;
                }
                b'{' => depth_cur += 1,
                b'}' => {
                    if depth_cur == 0 {
                        return Err(WbError::validation(format!("YAML 第 {lineno} 行大括号不匹配")));
                    }
                    depth_cur -= 1;
                }
                b',' if depth_sq == 0 && depth_cur == 0 => {
                    let part = inner[start..i].trim();
                    if part.is_empty() {
                        return Err(WbError::validation(format!("YAML 第 {lineno} 行存在多余逗号")));
                    }
                    items.push(part);
                    start = i + 1;
                }
                _ => {}
            }
        }
    }
    if in_single || in_double || depth_sq != 0 || depth_cur != 0 {
        return Err(WbError::validation(format!("YAML 第 {lineno} 行流式语法未闭合: {inner}")));
    }
    let tail = inner[start..].trim();
    if !tail.is_empty() {
        items.push(tail);
    }
    Ok(items)
}

fn parse_scalar(raw: &str, lineno: usize) -> Result<Value, WbError> {
    let t = raw.trim();
    if t.starts_with('"') {
        let s: String = serde_json::from_str(t)
            .map_err(|e| WbError::validation(format!("YAML 第 {lineno} 行双引号字符串无效: {e}")))?;
        return Ok(Value::String(s));
    }
    if t.starts_with('\'') {
        return Ok(Value::String(parse_single_quoted(t, lineno)?));
    }
    if matches!(t, "null" | "Null" | "NULL" | "~") {
        return Ok(Value::Null);
    }
    if matches!(t, "true" | "True" | "TRUE") {
        return Ok(Value::Bool(true));
    }
    if matches!(t, "false" | "False" | "FALSE") {
        return Ok(Value::Bool(false));
    }
    if let Ok(n) = t.parse::<i64>() {
        return Ok(Value::Number(Number::from(n)));
    }
    if t.contains('.') || t.contains('e') || t.contains('E') {
        if let Ok(f) = t.parse::<f64>() {
            if let Some(n) = Number::from_f64(f) {
                return Ok(Value::Number(n));
            }
        }
    }
    if t.starts_with('[') || t.starts_with('{') || t.starts_with("]]") || t.starts_with("}}") {
        return Err(WbError::validation(format!("YAML 第 {lineno} 行格式无效: {t}")));
    }
    Ok(Value::String(t.to_string()))
}

fn parse_inline_value(raw: &str, lineno: usize) -> Result<Value, WbError> {
    let t = raw.trim();
    if t.starts_with('[') {
        return parse_flow_list(t, lineno);
    }
    if t.starts_with('{') {
        return parse_flow_map(t, lineno);
    }
    parse_scalar(t, lineno)
}

pub fn from_yaml_str(raw: &str) -> Result<Value, WbError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(Value::Null);
    }
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
            return Ok(v);
        }
    }
    let lines = preprocess_lines(raw)?;
    if lines.is_empty() {
        return Ok(Value::Null);
    }
    let (val, next) = parse_node(&lines, 0, lines[0].indent)?;
    if next < lines.len() {
        return Err(WbError::validation(format!(
            "YAML 第 {} 行存在无法解析的缩进或冗余内容: {}",
            lines[next].lineno, lines[next].text
        )));
    }
    Ok(val)
}

fn is_seq_item(text: &str) -> bool {
    text == "-" || text.starts_with("- ")
}

fn parse_node(lines: &[Line<'_>], idx: usize, expected_indent: usize) -> Result<(Value, usize), WbError> {
    let line = &lines[idx];
    if line.indent != expected_indent {
        return Err(WbError::validation(format!(
            "YAML 第 {} 行缩进异常: 期望 {}, 实际 {}",
            line.lineno, expected_indent, line.indent
        )));
    }
    if is_seq_item(line.text) {
        parse_sequence(lines, idx, expected_indent)
    } else if split_key_value(line.text).is_some() {
        parse_mapping(lines, idx, expected_indent)
    } else if idx == 0 && lines.len() == 1 {
        Ok((parse_inline_value(line.text, line.lineno)?, 1))
    } else {
        Err(WbError::validation(format!(
            "YAML 第 {} 行无法识别的语法: {}",
            line.lineno, line.text
        )))
    }
}

fn parse_mapping(lines: &[Line<'_>], mut idx: usize, indent: usize) -> Result<(Value, usize), WbError> {
    let mut map = Map::new();
    while idx < lines.len() {
        let line = &lines[idx];
        if line.indent < indent {
            break;
        }
        if line.indent > indent {
            return Err(WbError::validation(format!(
                "YAML 第 {} 行缩进过多: {}",
                line.lineno, line.text
            )));
        }
        if is_seq_item(line.text) {
            break;
        }
        let (k_raw, v_raw) = split_key_value(line.text).ok_or_else(|| {
            WbError::validation(format!("YAML 第 {} 行缺少键值分隔符 ':': {}", line.lineno, line.text))
        })?;
        let key = parse_key(k_raw, line.lineno)?;
        idx += 1;

        if v_raw.is_empty() {
            if idx < lines.len() && lines[idx].indent > indent {
                let child_indent = lines[idx].indent;
                let (child, next) = parse_node(lines, idx, child_indent)?;
                idx = next;
                map.insert(key, child);
            } else if idx < lines.len() && lines[idx].indent == indent && is_seq_item(lines[idx].text) {
                // Allow zero-indented `- item` directly under `key:` (common PyYAML style)
                let (child, next) = parse_sequence(lines, idx, indent)?;
                idx = next;
                map.insert(key, child);
            } else {
                map.insert(key, Value::Null);
            }
        } else {
            let val = parse_inline_value(v_raw, line.lineno)?;
            map.insert(key, val);
        }
    }
    Ok((Value::Object(map), idx))
}

fn parse_sequence(lines: &[Line<'_>], mut idx: usize, indent: usize) -> Result<(Value, usize), WbError> {
    let mut items = Vec::new();
    while idx < lines.len() {
        let line = &lines[idx];
        if line.indent < indent {
            break;
        }
        if line.indent > indent {
            return Err(WbError::validation(format!(
                "YAML 第 {} 行序列缩进异常: {}",
                line.lineno, line.text
            )));
        }
        if !is_seq_item(line.text) {
            break;
        }
        let rest = line.text.strip_prefix('-').unwrap().trim_start();
        let item_lineno = line.lineno;
        idx += 1;

        if rest.is_empty() {
            if idx < lines.len() && lines[idx].indent > indent {
                let child_indent = lines[idx].indent;
                let (child, next) = parse_node(lines, idx, child_indent)?;
                idx = next;
                items.push(child);
            } else {
                items.push(Value::Null);
            }
        } else if let Some((first_k, first_v)) = split_key_value(rest) {
            let mut obj = Map::new();
            let key = parse_key(first_k, item_lineno)?;
            if first_v.is_empty() {
                if idx < lines.len() && lines[idx].indent > indent + 2 {
                    let child_indent = lines[idx].indent;
                    let (child, next) = parse_node(lines, idx, child_indent)?;
                    idx = next;
                    obj.insert(key, child);
                } else if idx < lines.len() && lines[idx].indent == indent + 2 && is_seq_item(lines[idx].text) {
                    let (child, next) = parse_sequence(lines, idx, indent + 2)?;
                    idx = next;
                    obj.insert(key, child);
                } else {
                    obj.insert(key, Value::Null);
                }
            } else {
                obj.insert(key, parse_inline_value(first_v, item_lineno)?);
            }

            // Read remaining keys of this sequence mapping at indent + 2 (or same level as first key)
            let cont_indent = indent + 2;
            while idx < lines.len() && lines[idx].indent == cont_indent && !is_seq_item(lines[idx].text) {
                let cline = &lines[idx];
                let (ck_raw, cv_raw) = split_key_value(cline.text).ok_or_else(|| {
                    WbError::validation(format!(
                        "YAML 第 {} 行对象条目语法无效: {}",
                        cline.lineno, cline.text
                    ))
                })?;
                let ckey = parse_key(ck_raw, cline.lineno)?;
                idx += 1;
                if cv_raw.is_empty() {
                    if idx < lines.len() && lines[idx].indent > cont_indent {
                        let child_indent = lines[idx].indent;
                        let (child, next) = parse_node(lines, idx, child_indent)?;
                        idx = next;
                        obj.insert(ckey, child);
                    } else if idx < lines.len() && lines[idx].indent == cont_indent && is_seq_item(lines[idx].text) {
                        let (child, next) = parse_sequence(lines, idx, cont_indent)?;
                        idx = next;
                        obj.insert(ckey, child);
                    } else {
                        obj.insert(ckey, Value::Null);
                    }
                } else {
                    obj.insert(ckey, parse_inline_value(cv_raw, cline.lineno)?);
                }
            }
            items.push(Value::Object(obj));
        } else {
            items.push(parse_inline_value(rest, item_lineno)?);
        }
    }
    Ok((Value::Array(items), idx))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn roundtrip_complex_yaml_and_reject_malformed() {
        let sample = json!({
            "schema": 1,
            "id": "TASK-1",
            "title": "重构: 核心引擎 [v2]",
            "items": [
                {
                    "id": "a",
                    "repo": "svc-a",
                    "source_branch": "feat/a",
                    "target_branch": "main",
                    "allowed_paths": ["src/", "tests/"],
                    "depends_on": []
                },
                {
                    "id": "b",
                    "repo": "svc-b",
                    "source_branch": "feat/b",
                    "target_branch": "main",
                    "depends_on": ["a"]
                }
            ],
            "actions": [
                {
                    "id": "act-1",
                    "kind": "manual_review",
                    "title": "核对",
                    "scope": {"v": 1, "files": ["src/"]}
                }
            ]
        });

        let dumped = to_yaml_string(&sample);
        let parsed = from_yaml_str(&dumped).expect("roundtrip parse failed");
        assert_eq!(parsed, sample);

        assert!(from_yaml_str("::: [unclosed yaml\n").is_err());
        assert!(from_yaml_str("a: [1, 2\n").is_err());
    }
}
