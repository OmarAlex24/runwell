//! Fixed service host ports collide when runners share a host. Detect numeric
//! host:container bindings, including IP/protocol forms. Scan all non-service job
//! values plus workflow env for literal host-port references; container jobs can still collide through
//! unnecessary host bindings. Only plain numeric/quoted scalars can be
//! replaced; otherwise provide references and a dynamic-port manual snippet.
use super::*;
use crate::local;
pub(super) fn check(job: &Job<'_>, out: &mut Vec<Finding>) {
    if !job.hosted() {
        return;
    }
    let Some(services) = job.node.get("services") else {
        return;
    };
    for (id, service) in services.pairs() {
        for port in service.get("ports").map_or(&[][..], Node::items) {
            let text = port.text();
            let segments: Vec<_> = text.split(':').collect();
            if segments.len() < 2 {
                continue;
            }
            let host = segments[segments.len() - 2];
            let container = segments[segments.len() - 1];
            if host.parse::<u16>().is_err()
                || host == "0"
                || container
                    .split('/')
                    .next()
                    .is_none_or(|s| s.parse::<u16>().is_err())
            {
                continue;
            }
            let mut refs = Vec::new();
            for (key, value) in job
                .node
                .pairs()
                .iter()
                .filter(|(k, _)| k.text() != "services")
            {
                references(value, host, key.text(), &mut refs);
            }
            if let Some(env) = job.workflow.root.get("env") {
                references(env, host, "workflow.env", &mut refs);
            }
            let number = container.split('/').next().unwrap_or(container);
            let access = format!("${{{{ job.services.{}.ports['{number}'] }}}}", id.text());
            let snippet = format!(
                "ports:\n  - '{container}'\n# Replace host-port literals in clients with {access}"
            );
            let mut f = job_finding(
                job,
                port,
                "fixed-service-ports",
                "A fixed service host port can collide with concurrent self-hosted jobs. Let GitHub assign the host port and use its service-port context.",
                &snippet,
                json!({"service":id.text(),"hostPort":host,"containerPort":container,"references":refs,"portContext":access}),
            );
            let edit = if !refs.is_empty() {
                Err("literal host-port references require coordinated client changes".into())
            } else if job.node.inherited()
                || job.workflow.root.get("jobs").is_some_and(|n| n.inherited())
                || services.inherited()
                || service.inherited()
                || service.get("ports").is_some_and(|n| n.shared)
            {
                Err("service configuration is shared through an anchor or alias".into())
            } else {
                local::scalar(job.workflow.source, port, container)
            };
            auto(&mut f, edit);
            out.push(f);
        }
    }
}
fn references(node: &Node, port: &str, path: &str, out: &mut Vec<Value>) {
    if literal(node.text(), port) {
        out.push(json!({"path":path,"line":node.span.line}));
    }
    for (k, v) in node.pairs() {
        references(v, port, &format!("{path}.{}", k.text()), out);
    }
    for (i, v) in node.items().iter().enumerate() {
        references(v, port, &format!("{path}[{i}]"), out);
    }
}
fn literal(text: &str, port: &str) -> bool {
    // Context expressions already use dynamically assigned ports, not literals.
    let mut rest = text;
    while let Some(start) = rest.find("${{") {
        if literal_plain(&rest[..start], port) {
            return true;
        }
        let Some(end) = rest[start..].find("}}") else {
            return true;
        };
        let expression = &rest[start..start + end + 2];
        if !(expression.trim_start().starts_with("${{ job.services.")
            && expression.contains(".ports[")
            && !expression.contains("||")
            && !expression.contains("&&"))
            && literal_plain(expression, port)
        {
            return true;
        }
        rest = &rest[start + end + 2..];
    }
    literal_plain(rest, port)
}
fn literal_plain(text: &str, port: &str) -> bool {
    text.split(|c: char| !c.is_ascii_digit()).any(|s| s == port)
}
