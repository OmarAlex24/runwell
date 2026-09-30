//! Unfiltered pushes duplicate PR execution on a finite pool. Detect both events
//! with no positive push branch filter. Filtered or tag-only pushes are guarded.
//! Suggest default/release branches; changing triggers always needs human review.
use super::*;
pub(super) fn check(w: &Workflow<'_>, out: &mut Vec<Finding>) {
    if !w.triggered("push") || !w.triggered("pull_request") {
        return;
    }
    let push = w.root.get("on").and_then(|n| n.get("push"));
    if push.is_some_and(|p| p.get("branches").is_some() || p.get("tags").is_some()) {
        return;
    }
    out.push(finding(w,"",push.unwrap_or(w.root),"push-and-pr-duplicate","Unfiltered push and pull_request triggers run the same PR commit twice. Restrict push to the default and release branches after review.","on:\n  pull_request:\n  push:\n    branches: [<default-branch>, 'release/**']",json!({"unfilteredPush":true,"pullRequest":true})));
}
