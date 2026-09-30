use crate::{DockerProxyConfig, Error, ProxySpec};
use serde_json::{Map, Value, json};

/// Driver reported by the upstream daemon's /info response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CgroupDriver {
    /// Docker expects a systemd slice unit name.
    Systemd,
    /// Docker expects an absolute path in the cgroup filesystem.
    Cgroupfs,
}
impl CgroupDriver {
    /// Reject unknown drivers rather than silently losing attribution.
    pub fn parse(value: &str) -> Result<Self, Error> {
        match value {
            "systemd" => Ok(Self::Systemd),
            "cgroupfs" => Ok(Self::Cgroupfs),
            _ => Err(Error::Config),
        }
    }
    /// Translate systemd dash ancestry when Docker uses cgroupfs instead.
    pub fn parent(self, slice: &str) -> Result<String, Error> {
        let base = slice.strip_suffix(".slice").ok_or(Error::Config)?;
        if base.is_empty()
            || !base.bytes().all(|v| v.is_ascii_alphanumeric() || v == b'-')
            || base.split('-').any(str::is_empty)
        {
            return Err(Error::Config);
        }
        if self == Self::Systemd {
            return Ok(slice.into());
        }
        let mut path = String::new();
        let mut prefix = String::new();
        for part in base.split('-') {
            if !prefix.is_empty() {
                prefix.push('-');
            }
            prefix.push_str(part);
            path.push_str(&format!("/{prefix}.slice"));
        }
        Ok(path)
    }
}
/// Only these API operations require changes; all other payloads stream intact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rewrite {
    /// Force container parent, labels, optional memory cap and socket sources.
    Container,
    /// Merge resource labels.
    Labels,
    /// Protect cgroup attribution during updates.
    Update,
    /// Rewrite query parameters without reading a build context.
    Build,
}
/// Match POST routes with an optional exact /v1.<digits> API prefix.
pub fn route(method: &str, path: &str) -> Option<Rewrite> {
    if method != "POST" {
        return None;
    }
    let path = if let Some(rest) = path.strip_prefix("/v1.") {
        let (version, path) = rest.split_once('/')?;
        if version.is_empty() || !version.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        format!("/{path}")
    } else {
        path.into()
    };
    match path.as_str() {
        "/containers/create" => Some(Rewrite::Container),
        "/networks/create" | "/volumes/create" => Some(Rewrite::Labels),
        "/build" => Some(Rewrite::Build),
        _ => path
            .strip_prefix("/containers/")?
            .strip_suffix("/update")
            .filter(|id| !id.is_empty() && !id.contains('/'))
            .map(|_| Rewrite::Update),
    }
}
/// Immutable rewriting policy for a single proxy.
#[derive(Clone)]
pub struct Rewriter {
    pub(crate) spec: ProxySpec,
    pub(crate) settings: DockerProxyConfig,
    pub(crate) parent: String,
}
impl Rewriter {
    /// Prepare a validated policy after detecting the daemon driver once.
    pub fn new(
        spec: ProxySpec,
        settings: DockerProxyConfig,
        driver: CgroupDriver,
    ) -> Result<Self, Error> {
        settings.validate().map_err(|_| Error::Config)?;
        if spec.job_id == 0
            || spec.node.is_empty()
            || spec.memory_max == 0
            || spec.memory_max > i64::MAX as u64
        {
            return Err(Error::Config);
        }
        let parent = driver.parent(&spec.cgroup_parent)?;
        Ok(Self {
            spec,
            settings,
            parent,
        })
    }
    /// Rewrite a bounded JSON object, preserving unrelated values and limits.
    pub fn json(&self, kind: Rewrite, bytes: &[u8]) -> Result<Vec<u8>, Error> {
        if bytes.len() > self.settings.max_json_bytes {
            return Err(Error::BodyTooLarge(self.settings.max_json_bytes));
        }
        let mut value: Value = serde_json::from_slice(bytes).map_err(|_| Error::Payload)?;
        let object = value.as_object_mut().ok_or(Error::Payload)?;
        match kind {
            Rewrite::Container => {
                self.labels(field(object, "Labels")?)?;
                let host = field(object, "HostConfig")?
                    .as_object_mut()
                    .ok_or(Error::Payload)?;
                set(host, "CgroupParent", json!(self.parent));
                self.host_policy(host)?;
                if self.settings.cap_memory {
                    let memory = take(host, "Memory");
                    let limit = if memory.is_null() {
                        0
                    } else {
                        memory.as_u64().ok_or(Error::Payload)?
                    };
                    host.insert(
                        "Memory".into(),
                        json!(if limit == 0 {
                            self.spec.memory_max
                        } else {
                            limit.min(self.spec.memory_max)
                        }),
                    );
                }
                self.binds(host)?;
            }
            Rewrite::Labels => self.labels(field(object, "Labels")?)?,
            Rewrite::Update => {
                if has_parent(&value) {
                    return Err(Error::CgroupUpdate);
                }
                // Validation only: preserve whitespace and numeric representation.
                return Ok(bytes.to_vec());
            }
            Rewrite::Build => return Err(Error::Payload),
        }
        serde_json::to_vec(&value).map_err(|_| Error::Payload)
    }
    pub(crate) fn labels(&self, labels: &mut Value) -> Result<(), Error> {
        if labels.is_null() {
            *labels = json!({});
        }
        let labels = labels.as_object_mut().ok_or(Error::Payload)?;
        if labels.values().any(|v| !v.is_string()) {
            return Err(Error::Payload);
        }
        labels.insert("io.runwell.job".into(), json!(self.spec.job_id.to_string()));
        labels.insert("io.runwell.node".into(), json!(self.spec.node));
        Ok(())
    }
    fn host_policy(&self, host: &Map<String, Value>) -> Result<(), Error> {
        let enabled = host.iter().any(|(key, value)| {
            (key.eq_ignore_ascii_case("Privileged") && value == true)
                || (["PidMode", "NetworkMode"]
                    .iter()
                    .any(|k| key.eq_ignore_ascii_case(k))
                    && value == "host")
        });
        if enabled {
            tracing::warn!(
                job_id = self.spec.job_id,
                denied = self.settings.deny_host_access,
                "Docker request uses privileged or host namespace access"
            );
            if self.settings.deny_host_access {
                return Err(Error::HostAccess);
            }
        }
        Ok(())
    }
    fn binds(&self, host: &mut Map<String, Value>) -> Result<(), Error> {
        let socket = self.spec.socket(&self.settings);
        let replacement = socket.to_str().ok_or(Error::Config)?;
        for (key, value) in host.iter_mut() {
            if value.is_null() {
                continue;
            }
            if key.eq_ignore_ascii_case("Binds") {
                for bind in value.as_array_mut().ok_or(Error::Payload)? {
                    let text = bind.as_str().ok_or(Error::Payload)?;
                    if let Some((source, rest)) = text.split_once(':')
                        && self.is_socket(source)
                    {
                        *bind = json!(format!("{replacement}:{rest}"));
                    }
                }
            } else if key.eq_ignore_ascii_case("Mounts") {
                for mount in value.as_array_mut().ok_or(Error::Payload)? {
                    let mount = mount.as_object_mut().ok_or(Error::Payload)?;
                    let is_bind = mount
                        .iter()
                        .any(|(k, v)| k.eq_ignore_ascii_case("Type") && v == "bind");
                    if is_bind {
                        for (k, v) in mount.iter_mut() {
                            if k.eq_ignore_ascii_case("Source")
                                && v.as_str().is_some_and(|s| self.is_socket(s))
                            {
                                *v = json!(replacement);
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }
    fn is_socket(&self, source: &str) -> bool {
        source == "/run/docker.sock"
            || source == "/var/run/docker.sock"
            || std::path::Path::new(source) == self.settings.upstream_socket
    }
}
// Go's JSON decoder accepts case-insensitive field names. Remove aliases before
// inserting forced fields so a later duplicate cannot replace attribution.
fn take(object: &mut Map<String, Value>, name: &str) -> Value {
    let keys: Vec<_> = object
        .keys()
        .filter(|k| k.eq_ignore_ascii_case(name))
        .cloned()
        .collect();
    let mut value = Value::Null;
    for key in keys {
        if let Some(v) = object.remove(&key) {
            value = v;
        }
    }
    value
}
fn set(object: &mut Map<String, Value>, name: &str, value: Value) {
    take(object, name);
    object.insert(name.into(), value);
}
fn field<'a>(object: &'a mut Map<String, Value>, name: &str) -> Result<&'a mut Value, Error> {
    let mut value = take(object, name);
    if value.is_null() {
        value = json!({});
    }
    object.insert(name.into(), value);
    object.get_mut(name).ok_or(Error::Payload)
}
fn has_parent(value: &Value) -> bool {
    value.as_object().is_some_and(|o| {
        o.iter().any(|(k, v)| {
            k.eq_ignore_ascii_case("CgroupParent")
                || (k.eq_ignore_ascii_case("HostConfig") && has_parent(v))
        })
    })
}

impl Rewriter {
    /// Merge build labels and replace all cgroupparent parameters. Preserve the
    /// exact encoding/order of unrelated query segments (including duplicates).
    pub fn build_query(&self, path_and_query: &str) -> Result<String, Error> {
        let (path, query) = path_and_query
            .split_once('?')
            .unwrap_or((path_and_query, ""));
        let mut kept = Vec::new();
        let mut labels = json!({});
        for segment in query.split('&').filter(|v| !v.is_empty()) {
            let (key, value) = url::form_urlencoded::parse(segment.as_bytes())
                .next()
                .ok_or(Error::Payload)?;
            match key.as_ref() {
                "cgroupparent" => {}
                "labels" => {
                    let input: Value = serde_json::from_str(&value).map_err(|_| Error::Payload)?;
                    if !input.is_null() {
                        labels
                            .as_object_mut()
                            .ok_or(Error::Payload)?
                            .extend(input.as_object().ok_or(Error::Payload)?.clone());
                    }
                }
                _ => kept.push(segment.to_owned()),
            }
        }
        self.labels(&mut labels)?;
        let labels = serde_json::to_string(&labels).map_err(|_| Error::Payload)?;
        kept.push(
            url::form_urlencoded::Serializer::new(String::new())
                .append_pair("cgroupparent", &self.parent)
                .append_pair("labels", &labels)
                .finish(),
        );
        Ok(format!("{path}?{}", kept.join("&")))
    }
}
