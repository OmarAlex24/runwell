use crate::{Error, Rewrite};

pub(crate) fn normalize(path: &str) -> Result<String, Error> {
    let mut decoded = Vec::with_capacity(path.len());
    let mut input = path.bytes();
    while let Some(byte) = input.next() {
        if byte == b'%' {
            let high = input.next().and_then(|v| (v as char).to_digit(16));
            let low = input.next().and_then(|v| (v as char).to_digit(16));
            let byte = (high.ok_or(Error::Path)? * 16 + low.ok_or(Error::Path)?) as u8;
            if byte == b'/' {
                return Err(Error::Path);
            }
            decoded.push(byte);
        } else {
            decoded.push(byte);
        }
    }
    let path = String::from_utf8(decoded).map_err(|_| Error::Path)?;
    if !path.starts_with('/')
        || path.contains("//")
        || path.split('/').any(|s| matches!(s, "." | ".."))
    {
        return Err(Error::Path);
    }
    // Moby's version mux uses /v[0-9.]+, not a semantic-version parser.
    if let Some(rest) = path.strip_prefix("/v")
        && let Some((version, tail)) = rest.split_once('/')
        && !version.is_empty()
        && version.bytes().all(|v| v.is_ascii_digit() || v == b'.')
    {
        return Ok(format!("/{tail}"));
    }
    Ok(path)
}

/// Decode paths and match POST routes with Moby's optional /v[0-9.]+ prefix.
/// Ambiguous slash/dot encodings are rejected even for pass-through operations.
pub fn route(method: &str, path: &str) -> Result<Option<Rewrite>, Error> {
    let path = normalize(path)?;
    if method != "POST" {
        return Ok(None);
    }
    Ok(match path.as_str() {
        "/containers/create" => Some(Rewrite::Container),
        "/networks/create" | "/volumes/create" => Some(Rewrite::Labels),
        "/build" => Some(Rewrite::Build),
        _ if resource_action(&path, "/containers/", "/update") => Some(Rewrite::Update),
        _ => None,
    })
}

pub(crate) fn upgrade_allowed(method: &str, path: &str) -> Result<bool, Error> {
    let path = normalize(path)?;
    Ok(method == "POST"
        && (matches!(path.as_str(), "/session" | "/grpc")
            || resource_action(&path, "/containers/", "/attach")
            || resource_action(&path, "/exec/", "/start")))
}

fn resource_action(path: &str, prefix: &str, suffix: &str) -> bool {
    path.strip_prefix(prefix)
        .and_then(|p| p.strip_suffix(suffix))
        .is_some_and(|id| !id.is_empty() && !id.contains('/'))
}
