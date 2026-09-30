//! Marked YAML syntax tree. Scalars stay strings, including Actions expressions.
use crate::Error;
use std::collections::BTreeMap;
use yaml_rust2::{
    parser::{Event, Parser},
    scanner::{Marker, TScalarStyle},
};

#[derive(Debug, Clone, Default)]
pub(crate) struct Span {
    pub start: usize,
    pub end: usize,
    pub line: usize,
    pub column: usize,
}
#[derive(Debug, Clone)]
pub(crate) enum Value {
    Scalar(String, TScalarStyle),
    Map(Vec<(Node, Node)>),
    Seq(Vec<Node>),
}
#[derive(Debug, Clone)]
pub(crate) struct Node {
    pub value: Value,
    pub span: Span,
    pub shared: bool,
}
impl Node {
    pub fn text(&self) -> &str {
        match &self.value {
            Value::Scalar(s, _) => s,
            _ => "",
        }
    }
    pub fn pairs(&self) -> &[(Node, Node)] {
        match &self.value {
            Value::Map(v) => v,
            _ => &[],
        }
    }
    pub fn items(&self) -> &[Node] {
        match &self.value {
            Value::Seq(v) => v,
            _ => &[],
        }
    }
    pub fn get(&self, key: &str) -> Option<&Node> {
        self.pairs()
            .iter()
            .find(|(k, _)| k.text() == key)
            .map(|(_, v)| v)
    }
    pub fn key(&self, key: &str) -> Option<&Node> {
        self.pairs()
            .iter()
            .find(|(k, _)| k.text() == key)
            .map(|(k, _)| k)
    }
    pub fn str(&self, key: &str) -> &str {
        self.get(key).map_or("", Node::text)
    }
    pub fn strings(&self) -> Vec<&str> {
        match &self.value {
            Value::Scalar(s, _) => vec![s],
            Value::Seq(v) => v.iter().flat_map(Node::strings).collect(),
            Value::Map(v) => v.iter().flat_map(|(_, n)| n.strings()).collect(),
        }
    }
    pub fn inherited(&self) -> bool {
        self.shared || self.pairs().iter().any(|(k, _)| k.text() == "<<")
    }
}
struct Reader<'a> {
    parser: Parser<std::str::Chars<'a>>,
    line_offsets: Vec<usize>,
    source: &'a str,
    anchors: BTreeMap<usize, Node>,
    flow_depth: usize,
}
impl Reader<'_> {
    fn next(&mut self) -> Result<(Event, Marker), Error> {
        self.parser
            .next_token()
            .map_err(|e| Error::Parse(e.to_string()))
    }
    fn span(&self, mark: Marker) -> Span {
        let base = self
            .line_offsets
            .get(mark.line().saturating_sub(1))
            .copied()
            .unwrap_or(self.source.len());
        let column_bytes = self.source[base..]
            .char_indices()
            .nth(mark.col())
            .map_or(self.source.len() - base, |(i, _)| i);
        Span {
            start: base + column_bytes,
            end: self.source.len(),
            line: mark.line(),
            column: mark.col(),
        }
    }
    fn node(&mut self, event: Event, mark: Marker) -> Result<Node, Error> {
        let mut span = self.span(mark);
        let (value, anchor) = match event {
            Event::Alias(id) => {
                let mut n = self
                    .anchors
                    .get(&id)
                    .cloned()
                    .ok_or_else(|| Error::Parse("unresolved alias".into()))?;
                n.shared = true;
                span.end = self.source[span.start..]
                    .find(|c: char| c.is_whitespace() || ",]}".contains(c))
                    .map_or(self.source.len(), |i| span.start + i);
                n.span = span;
                return Ok(n);
            }
            Event::Scalar(s, style, id, _) => {
                span.end =
                    crate::spans::scalar_end(self.source, span.start, style, self.flow_depth > 0);
                (Value::Scalar(s, style), id)
            }
            Event::MappingStart(id, _) => {
                let flow = usize::from(self.source[span.start..].starts_with('{'));
                self.flow_depth += flow;
                let mut pairs: Vec<(Node, Node)> = Vec::new();
                loop {
                    let (e, m) = self.next()?;
                    if e == Event::MappingEnd {
                        span.end = self.span(m).start;
                        break;
                    }
                    let key = self.node(e, m)?;
                    if pairs.iter().any(|(k, _)| k.text() == key.text()) {
                        return Err(Error::Parse(format!(
                            "duplicate key at line {}",
                            key.span.line
                        )));
                    }
                    let (e, m) = self.next()?;
                    let val = self.node(e, m)?;
                    pairs.push((key, val));
                }
                self.flow_depth -= flow;
                if flow == 0
                    && let Some((key, _)) = pairs.first()
                {
                    span.start = key.span.start;
                    span.line = key.span.line;
                    span.column = key.span.column;
                }
                // Resolve merge keys for analysis, but retain << to prohibit edits.
                let merges: Vec<Node> = pairs
                    .iter()
                    .filter(|(k, _)| k.text() == "<<")
                    .flat_map(|(_, v)| {
                        if v.items().is_empty() {
                            vec![v.clone()]
                        } else {
                            v.items().to_vec()
                        }
                    })
                    .collect();
                for merge in merges {
                    for (k, v) in merge.pairs() {
                        if !pairs.iter().any(|(key, _)| key.text() == k.text()) {
                            let mut v = v.clone();
                            v.shared = true;
                            pairs.push((k.clone(), v));
                        }
                    }
                }
                (Value::Map(pairs), id)
            }
            Event::SequenceStart(id, _) => {
                let flow = usize::from(self.source[span.start..].starts_with('['));
                self.flow_depth += flow;
                let mut items = Vec::new();
                loop {
                    let (e, m) = self.next()?;
                    if e == Event::SequenceEnd {
                        span.end = self.span(m).start;
                        break;
                    }
                    items.push(self.node(e, m)?);
                }
                self.flow_depth -= flow;
                (Value::Seq(items), id)
            }
            _ => return Err(Error::Parse("expected a YAML node".into())),
        };
        let node = Node {
            value,
            span,
            shared: anchor != 0,
        };
        if anchor != 0 {
            self.anchors.insert(anchor, node.clone());
        }
        Ok(node)
    }
}
/// Parse every document and convert line/column markers to UTF-8 byte offsets.
pub(crate) fn parse(source: &str) -> Result<Vec<Node>, Error> {
    let line_offsets = std::iter::once(0)
        .chain(
            source
                .char_indices()
                .filter_map(|(i, c)| (c == '\n').then_some(i + 1)),
        )
        .collect();
    let mut r = Reader {
        parser: Parser::new_from_str(source),
        source,
        line_offsets,
        anchors: BTreeMap::new(),
        flow_depth: 0,
    };
    let mut docs = Vec::new();
    loop {
        let (event, mark) = r.next()?;
        match event {
            Event::StreamEnd => break,
            Event::DocumentStart => {
                r.anchors.clear();
                let (e, m) = r.next()?;
                docs.push(r.node(e, m)?);
            }
            Event::StreamStart | Event::DocumentEnd => (),
            _ => {
                return Err(Error::Parse(format!(
                    "unexpected event at line {}",
                    mark.line()
                )));
            }
        }
    }
    Ok(docs)
}
