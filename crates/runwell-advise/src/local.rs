//! Proof checks for local block insertions and scalar replacements.
use crate::{
    model::Edit,
    yaml::{Node, Value},
};
use yaml_rust2::scanner::TScalarStyle;

pub(crate) fn newline(source: &str) -> &str {
    if source.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    }
}
pub(crate) fn indent(source: &str, node: &Node) -> Result<String, String> {
    let start = source[..node.span.start].rfind('\n').map_or(0, |i| i + 1);
    let prefix = &source[start..node.span.start];
    if !prefix.chars().all(|c| c == ' ') {
        return Err("flow style or inline mapping has no safe block insertion point".into());
    }
    Ok(prefix.into())
}
pub(crate) fn insert(source: &str, map: &Node, lines: &[String]) -> Result<Edit, String> {
    if map.inherited() {
        return Err("anchors, aliases, or merge keys make this mapping shared".into());
    }
    let first = map
        .pairs()
        .first()
        .ok_or("empty or non-block mapping requires a manual edit")?
        .0
        .clone();
    let prefix = indent(source, &first)?;
    if source
        .get(map.span.start..)
        .is_some_and(|s| s.starts_with('{'))
        || map
            .pairs()
            .iter()
            .any(|(k, _)| k.span.column != first.span.column)
    {
        return Err("mapping is not a uniform block mapping".into());
    }
    let start = first.span.start - prefix.len();
    Ok(Edit {
        start,
        end: start,
        replacement: lines
            .iter()
            .map(|l| format!("{prefix}{l}{}", newline(source)))
            .collect(),
    })
}
pub(crate) fn unit(map: &Node) -> usize {
    map.pairs()
        .iter()
        .filter_map(|(k, v)| {
            v.pairs()
                .first()
                .map(|(c, _)| c.span.column.saturating_sub(k.span.column))
        })
        .find(|n| *n > 0)
        .unwrap_or(2)
}
pub(crate) fn scalar(source: &str, node: &Node, replacement: &str) -> Result<Edit, String> {
    if node.shared {
        return Err("scalar is anchored or aliased".into());
    }
    let Value::Scalar(_, style) = &node.value else {
        return Err("not a scalar".into());
    };
    let raw = source
        .get(node.span.start..node.span.end)
        .ok_or("invalid source span")?;
    let replacement = match style {
        TScalarStyle::SingleQuoted if raw.starts_with('\'') && raw.ends_with('\'') => {
            format!("'{replacement}'")
        }
        TScalarStyle::DoubleQuoted if raw.starts_with('"') && raw.ends_with('"') => {
            format!("\"{replacement}\"")
        }
        TScalarStyle::Plain if !raw.contains(['\n', '\r']) => replacement.into(),
        _ => return Err("scalar style cannot safely be replaced".into()),
    };
    Ok(Edit {
        start: node.span.start,
        end: node.span.end,
        replacement,
    })
}
