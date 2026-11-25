//! Minimal YAML frontmatter reader (flat `key: value`, inline `[a, b]` lists and
//! `- a` block lists). No YAML crate; never panics.

use serde_json::{Map, Value};

fn unquote(s: &str) -> &str {
    let s = s.trim();
    for q in ['"', '\''] {
        if s.len() >= 2 && s.starts_with(q) && s.ends_with(q) {
            return &s[1..s.len() - 1];
        }
    }
    s
}

fn scalar(s: &str) -> Value {
    let t = s.trim();
    if t.len() >= 2 && t.starts_with('[') && t.ends_with(']') {
        let items = t[1..t.len() - 1]
            .split(',')
            .map(|x| unquote(x))
            .filter(|x| !x.is_empty())
            .map(|x| Value::String(x.to_string()))
            .collect();
        return Value::Array(items);
    }
    Value::String(unquote(t).to_string())
}

/// Returns `(fields, body)`. Without a closing `---` fence the whole input is
/// the body and fields are empty.
pub fn parse(md: &str) -> (Map<String, Value>, &str) {
    let mut map = Map::new();
    let rest = md.strip_prefix('\u{feff}').unwrap_or(md);
    let Some(rest) = rest
        .strip_prefix("---\r\n")
        .or_else(|| rest.strip_prefix("---\n"))
    else {
        return (map, md);
    };
    // find closing fence line
    let mut pos = 0;
    let mut end = None;
    for line in rest.split_inclusive('\n') {
        if line.trim_end() == "---" {
            end = Some((pos, pos + line.len()));
            break;
        }
        pos += line.len();
    }
    let Some((fm_end, body_start)) = end else {
        return (map, md);
    };
    let (fm, body) = (&rest[..fm_end], &rest[body_start..]);

    let lines: Vec<&str> = fm.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        i += 1;
        if line.trim().is_empty()
            || line.trim_start().starts_with('#')
            || line.starts_with([' ', '\t', '-'])
        {
            continue;
        }
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let key = k.trim().to_string();
        let v = v.trim();
        let is_block = matches!(v, ">" | "|" | ">-" | "|-" | ">+" | "|+");
        if v.is_empty() || is_block {
            // collect following indented / "- " lines
            let mut items: Vec<&str> = Vec::new();
            while i < lines.len() {
                let l = lines[i];
                let t = l.trim();
                if t.is_empty() && is_block {
                    items.push("");
                } else if l.starts_with([' ', '\t']) || t.starts_with("- ") || t == "-" {
                    items.push(t);
                } else {
                    break;
                }
                i += 1;
            }
            if is_block {
                let sep = if v.starts_with('>') { " " } else { "\n" };
                let text = items.join(sep);
                map.insert(key, Value::String(text.trim().to_string()));
            } else if items.iter().any(|t| t.starts_with("- ")) {
                let arr = items
                    .iter()
                    .filter_map(|t| t.strip_prefix("- "))
                    .map(|t| Value::String(unquote(t).to_string()))
                    .collect();
                map.insert(key, Value::Array(arr));
            } else {
                map.insert(key, Value::String(String::new()));
            }
        } else {
            let mut text = v.to_string();
            while i < lines.len()
                && lines[i].starts_with([' ', '\t'])
                && !lines[i].trim().is_empty()
            {
                text.push(' ');
                text.push_str(lines[i].trim());
                i += 1;
            }
            let val = if matches!(key.as_str(), "description" | "name") {
                Value::String(unquote(&text).to_string())
            } else {
                scalar(&text)
            };
            map.insert(key, val);
        }
    }
    (map, body)
}

/// String field helper.
pub fn str_of(m: &Map<String, Value>, k: &str) -> Option<String> {
    m.get(k).and_then(|v| v.as_str()).map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontmatter_handles_lists_and_quotes() {
        let md = "---\nname: \"my skill\"\ndescription: 'It: does things'\ntools: [Read, \"Grep\", Bash]\nallowed:\n  - a\n  - \"b c\"\nnote: >\n  folded\n  text\n---\n# Body\n";
        let (m, body) = parse(md);
        assert_eq!(m["name"], "my skill");
        assert_eq!(m["description"], "It: does things");
        assert_eq!(m["tools"], serde_json::json!(["Read", "Grep", "Bash"]));
        assert_eq!(m["allowed"], serde_json::json!(["a", "b c"]));
        assert_eq!(m["note"], "folded text");
        assert_eq!(body, "# Body\n");
    }

    #[test]
    fn plain_multiline_and_bracket_description() {
        let (m, _) = parse("---\ndescription: foo\n  bar baz\nname: [x]\n---\n");
        assert_eq!(m["description"], "foo bar baz");
        assert_eq!(m["name"], "[x]");
        let (m, _) = parse("---\ndescription: [WIP] thing\ntools: [A]\n---\n");
        assert_eq!(m["description"], "[WIP] thing");
        assert_eq!(m["tools"], serde_json::json!(["A"]));
    }

    #[test]
    fn no_frontmatter_or_unterminated() {
        assert!(parse("# hi\n").0.is_empty());
        let (m, b) = parse("---\nname: x\nno end");
        assert!(m.is_empty());
        assert_eq!(b, "---\nname: x\nno end");
        assert!(parse("").0.is_empty());
    }
}
