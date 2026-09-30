//! Repeated PR work consumes scarce self-hosted capacity. Detect push/PR workflows
//! without workflow concurrency; do not overwrite existing groups. Insert only in
//! an unshared block mapping. Warn about unconditional push cancellation, leaving
//! the default branch policy for review because its name is not statically known.
use super::*;
use crate::local;
pub(super) fn check(w: &Workflow<'_>, out: &mut Vec<Finding>) {
    if !w.triggered("pull_request") && !w.triggered("push") {
        return;
    }
    let snippet = "concurrency:\n  group: ${{ github.workflow }}-${{ github.event.pull_request.number || github.ref }}\n  cancel-in-progress: ${{ github.event_name == 'pull_request' }}";
    if let Some(c) = w.root.get("concurrency") {
        if w.triggered("push") && c.str("cancel-in-progress") == "true" {
            out.push(finding(w,"",c,"missing-concurrency","Push runs can cancel each other, including default-branch runs. Limit cancellation to pull requests.",snippet,json!({"cancelInProgress":true,"defaultBranch":"unknown; verify push branch filters"})));
        }
    } else {
        let mut f = finding(
            w,
            "",
            w.root.key("on").unwrap_or(w.root),
            "missing-concurrency",
            "Superseded push/PR runs occupy runner capacity without a workflow concurrency group.",
            snippet,
            json!({"triggers":["push","pull_request"]}),
        );
        let pad = " ".repeat(local::unit(w.root));
        auto(
            &mut f,
            local::insert(
                w.source,
                w.root,
                &[
                    "concurrency:".into(),
                    format!(
                        "{pad}group: ${{{{ github.workflow }}}}-${{{{ github.event.pull_request.number || github.ref }}}}"
                    ),
                    format!(
                        "{pad}cancel-in-progress: ${{{{ github.event_name == 'pull_request' }}}}"
                    ),
                ],
            ),
        );
        out.push(f);
    }
}
