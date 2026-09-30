//! Scalar source boundaries, independent of decoded YAML values.
use yaml_rust2::scanner::TScalarStyle;
pub(crate) fn scalar_end(source: &str, start: usize, style: TScalarStyle, flow: bool) -> usize {
    let Some(tail) = source.get(start..) else {
        return start;
    };
    let quote = match style {
        TScalarStyle::SingleQuoted => '\'',
        TScalarStyle::DoubleQuoted => '"',
        _ => '\0',
    };
    if quote != '\0' {
        let mut chars = tail.char_indices().peekable();
        chars.next();
        while let Some((i, c)) = chars.next() {
            if quote == '"' && c == '\\' {
                chars.next();
                continue;
            }
            if c == quote {
                if quote == '\'' && chars.peek().is_some_and(|(_, c)| *c == quote) {
                    chars.next();
                    continue;
                }
                return start + i + c.len_utf8();
            }
        }
        return start;
    }
    if matches!(style, TScalarStyle::Literal | TScalarStyle::Folded) {
        return block_end(source, start);
    }
    let mut chars = tail.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let next = chars.peek().map(|(_, c)| *c);
        let previous = tail[..i].chars().next_back();
        let end = matches!(c, '\r' | '\n')
            || (flow && matches!(c, ',' | ']' | '}'))
            || (c == '#' && previous.is_none_or(char::is_whitespace))
            || (c == ':'
                && next.is_none_or(|n| n.is_whitespace() || (flow && ",[]{}".contains(n))));
        if end {
            return start + tail[..i].trim_end().len();
        }
    }
    start + tail.trim_end().len()
}
fn block_end(source: &str, start: usize) -> usize {
    let line_start = source[..start].rfind('\n').map_or(0, |i| i + 1);
    let parent = source[line_start..]
        .chars()
        .take_while(|c| *c == ' ')
        .count();
    let Some(header_end) = source[start..].find('\n').map(|i| start + i + 1) else {
        return source.len();
    };
    let explicit = source[start..header_end]
        .chars()
        .find_map(|c| c.to_digit(10))
        .map(|n| parent + n as usize);
    let mut required = explicit;
    let mut end = header_end;
    for line in source[header_end..].split_inclusive('\n') {
        let indent = line.chars().take_while(|c| *c == ' ').count();
        if !line.trim().is_empty() {
            let needed = *required.get_or_insert(indent);
            if indent <= parent || indent < needed {
                break;
            }
        }
        end += line.len();
    }
    end
}
