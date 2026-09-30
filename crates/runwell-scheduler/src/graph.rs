use std::collections::VecDeque;

/// Longest downstream work, including each node, for an indexed dependency DAG.
/// Rejects out-of-bounds edges, invalid durations and cycles without recursion.
pub fn critical_paths(work: &[f64], needs: &[Vec<usize>]) -> Result<Vec<f64>, GraphError> {
    if work.len() != needs.len() || work.iter().any(|x| !x.is_finite() || *x < 0.0) {
        return Err(GraphError);
    }
    let mut children = vec![Vec::new(); work.len()];
    let mut pending: Vec<_> = needs.iter().map(Vec::len).collect();
    for (child, parents) in needs.iter().enumerate() {
        for &parent in parents {
            let Some(edges) = children.get_mut(parent) else {
                return Err(GraphError);
            };
            edges.push(child);
        }
    }
    let mut ready: VecDeque<_> = pending
        .iter()
        .enumerate()
        .filter_map(|(i, &n)| (n == 0).then_some(i))
        .collect();
    let mut order = Vec::with_capacity(work.len());
    while let Some(i) = ready.pop_front() {
        order.push(i);
        for &c in &children[i] {
            pending[c] -= 1;
            if pending[c] == 0 {
                ready.push_back(c);
            }
        }
    }
    if order.len() != work.len() {
        return Err(GraphError);
    }
    let mut paths = work.to_vec();
    for &i in order.iter().rev() {
        paths[i] += children[i].iter().map(|&c| paths[c]).fold(0.0, f64::max);
    }
    Ok(paths)
}

/// Dependency graph is cyclic, malformed, or has invalid work estimates.
#[derive(Debug, thiserror::Error)]
#[error("invalid or cyclic dependency graph")]
pub struct GraphError;
