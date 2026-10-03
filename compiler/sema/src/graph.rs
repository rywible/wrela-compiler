//! A depth-first walk of a small directed graph, for the checks that report cycles: supertraits,
//! types that hold themselves, and constants whose values refer to themselves.

/// What [`dfs_cycles`] finds, in the order the walk finds it.
pub(crate) enum Visit<'a, E> {
    /// An edge back to a node on the current path, which closes a cycle. Each node on the
    /// cycle, from the edge's target to its source, with the edge the walk left it by; the last
    /// node's edge is the one that closes the cycle.
    Cycle(&'a [(usize, &'a E)]),
    /// A node whose edges have all been followed: every node it reaches is done before it, or
    /// is on the current path.
    Done(usize),
}

/// Walks a graph depth-first: from each node in order (unless an earlier walk visited it), and
/// along each node's edges (`edges[node]`) in order. `target` is the node an edge goes to.
pub(crate) fn dfs_cycles<E>(
    edges: &[Vec<E>],
    target: impl Fn(&E) -> usize,
    mut visit: impl FnMut(Visit<'_, E>),
) {
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        New,
        Active,
        Done,
    }
    let mut mark = vec![Mark::New; edges.len()];
    for root in 0..edges.len() {
        if mark[root] != Mark::New {
            continue;
        }
        // (node, next edge to follow)
        let mut stack: Vec<(usize, usize)> = vec![(root, 0)];
        mark[root] = Mark::Active;
        while let Some(&mut (node, ref mut next)) = stack.last_mut() {
            let Some(edge) = edges[node].get(*next) else {
                mark[node] = Mark::Done;
                stack.pop();
                visit(Visit::Done(node));
                continue;
            };
            *next += 1;
            let to = target(edge);
            match mark[to] {
                Mark::New => {
                    mark[to] = Mark::Active;
                    stack.push((to, 0));
                }
                Mark::Active => {
                    let at = stack.iter().position(|&(x, _)| x == to).unwrap_or(0);
                    // Each node on the stack has followed its edge to the node above it.
                    let cycle: Vec<(usize, &E)> =
                        stack[at..].iter().map(|&(x, next)| (x, &edges[x][next - 1])).collect();
                    visit(Visit::Cycle(&cycle));
                }
                Mark::Done => {}
            }
        }
    }
}
