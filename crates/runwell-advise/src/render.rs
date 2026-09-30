//! Compact Markdown advice for humans.
use crate::{Finding, Report};
use std::collections::BTreeMap;
pub(crate) fn markdown(report: &Report) -> String {
    let mut text = format!(
        "{} findings across {} rules. Savings are estimates per affected path and must not be added together.\n",
        report.findings.len(),
        report
            .findings
            .iter()
            .map(|f| &f.rule)
            .collect::<std::collections::BTreeSet<_>>()
            .len()
    );
    let mut ranked: Vec<_> = report
        .findings
        .iter()
        .filter(|f| f.estimated_savings.is_some())
        .collect();
    ranked.sort_by(|a, b| {
        b.estimated_savings
            .as_ref()
            .map_or(0.0, |s| s.p50)
            .total_cmp(&a.estimated_savings.as_ref().map_or(0.0, |s| s.p50))
    });
    for f in ranked.into_iter().take(5) {
        if let Some(s) = &f.estimated_savings {
            text.push_str(&format!(
                "\n- {} · {} · estimated {:.2}/{:.2} min p50/p90",
                f.rule,
                escape(&f.job),
                s.p50,
                s.p90
            ));
        }
    }
    let mut groups: BTreeMap<&str, Vec<&Finding>> = BTreeMap::new();
    for f in &report.findings {
        groups.entry(&f.rule).or_default().push(f);
    }
    for (rule, findings) in groups {
        text.push_str(&format!("\n\n## {rule}\n\n| Severity | File:line | Job / step | Finding and evidence | Fix |\n| --- | --- | --- | --- | --- |\n"));
        for f in &findings {
            text.push_str(&format!(
                "| {:?} | {}:{} | {}{} | {}<br>Evidence: `{}` | {}{} |\n",
                f.severity,
                escape(&f.file),
                f.line,
                escape(&f.job),
                f.step
                    .as_ref()
                    .map_or(String::new(), |s| format!(" / {}", escape(s))),
                escape(&f.message),
                escape(&f.evidence["observations"].to_string()),
                f.fix.kind,
                f.fix
                    .reason
                    .as_ref()
                    .map_or(String::new(), |s| format!(": {}", escape(s)))
            ));
        }
        for f in findings {
            text.push_str(&format!(
                "\nSuggested change for {}:{}:\n\n```yaml\n{}\n```\n\n",
                f.file, f.line, f.fix.snippet
            ));
        }
    }
    for change in &report.changes {
        text.push_str(&format!("\n```diff\n{}```\n", change.diff));
    }
    for reason in &report.refused {
        text.push_str(&format!("\nRefused: {reason}\n"));
    }
    text
}
fn escape(s: &str) -> String {
    s.replace('|', "\\|")
        .replace('\n', "<br>")
        .replace('`', "'")
}
