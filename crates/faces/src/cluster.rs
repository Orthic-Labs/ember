//! Grouping unnamed faces by likeness: average-link agglomerative clustering on cosine similarity.
//!
//! Two groups merge while the faces in them are alike on average, so one stray look-alike cannot drag two people into
//! one group. Ties go to the lower index, so the same faces always give the same groups. The work grows with the square
//! of the faces in a pass, so a pass takes at most `max_per_pass` faces and a longer list is clustered in passes that
//! never merge with each other.
//!
//! This only suggests: a group is a set of faces for the user to name, or to leave alone.

use crate::matching::cosine;

/// Most faces one pass clusters together (the work is a look at every pair of them).
pub const MAX_PER_PASS: usize = 2000;

/// Groups of `embeddings` (unit vectors), as ascending indices into it. Groups come largest first, then by their lowest
/// index, and every index is in exactly one group.
///
/// Groups merge while their average similarity is at least `threshold`, most alike pair first. `cannot(i, j)` says two
/// faces must stay apart (the user said one is not the other's person): no group ends up holding both, and a group
/// holding such a pair is never merged with another. A face without a usable embedding (another length, or a number that
/// is not finite) stays alone. Faces are clustered `max_per_pass` at a time (at least one), in the order given.
pub fn cluster(embeddings: &[&[f32]], threshold: f32, max_per_pass: usize, cannot: &dyn Fn(usize, usize) -> bool) -> Vec<Vec<usize>> {
    let pass = max_per_pass.max(1);
    let mut out: Vec<Vec<usize>> = Vec::new();
    let mut start = 0;
    while start < embeddings.len() {
        let end = embeddings.len().min(start.saturating_add(pass));
        let chunk = embeddings.get(start..end).unwrap_or_default();
        for group in cluster_pass(chunk, threshold, &|i: usize, j: usize| cannot(start + i, start + j)) {
            out.push(group.into_iter().map(|i| i + start).collect());
        }
        start = end;
    }
    out.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.first().cmp(&b.first())));
    out
}

/// The best partner of row `k`: the active face most alike to it (the lower index on a tie), if there is one.
fn best_of(sim: &[f32], active: &[bool], n: usize, k: usize) -> Option<usize> {
    let mut best: Option<(usize, f32)> = None;
    for j in (0..n).filter(|&j| j != k && active.get(j) == Some(&true)) {
        let v = sim.get(k * n + j).copied().unwrap_or(f32::NEG_INFINITY);
        if best.is_none_or(|(_, b)| v > b) {
            best = Some((j, v));
        }
    }
    best.map(|(j, _)| j)
}

/// One pass: average-link merging of a chunk. A cannot-link is a similarity of minus infinity, which the average of
/// anything with it stays at, so a group holding a kept-apart pair can never reach the threshold.
fn cluster_pass(emb: &[&[f32]], threshold: f32, cannot: &dyn Fn(usize, usize) -> bool) -> Vec<Vec<usize>> {
    let n = emb.len();
    let mut sim = vec![f32::NEG_INFINITY; n * n];
    for i in 0..n {
        for j in (i + 1)..n {
            let c = match (emb.get(i), emb.get(j)) {
                (Some(a), Some(b)) if !cannot(i, j) && !cannot(j, i) => cosine(a, b),
                _ => None,
            };
            let v = c.unwrap_or(f32::NEG_INFINITY);
            sim[i * n + j] = v;
            sim[j * n + i] = v;
        }
    }
    let mut size = vec![1usize; n];
    let mut active = vec![true; n];
    let mut members: Vec<Vec<usize>> = (0..n).map(|i| vec![i]).collect();
    let mut best: Vec<Option<usize>> = (0..n).map(|k| best_of(&sim, &active, n, k)).collect();
    loop {
        // the most alike pair: the highest similarity, then the lowest pair of indices
        let mut pick: Option<(f32, usize, usize)> = None;
        for a in (0..n).filter(|&a| active[a]) {
            let Some(b) = best[a] else { continue };
            let v = sim[a * n + b];
            if v < threshold {
                continue;
            }
            let (lo, hi) = (a.min(b), a.max(b));
            if pick.is_none_or(|(pv, pa, pb)| v > pv || (v == pv && (lo, hi) < (pa, pb))) {
                pick = Some((v, lo, hi));
            }
        }
        let Some((_, keep, gone)) = pick else { break };
        let (sk, sg) = (size[keep] as f32, size[gone] as f32);
        for k in (0..n).filter(|&k| active[k] && k != keep && k != gone) {
            let v = (sk * sim[keep * n + k] + sg * sim[gone * n + k]) / (sk + sg);
            sim[keep * n + k] = v;
            sim[k * n + keep] = v;
        }
        active[gone] = false;
        size[keep] += size[gone];
        let moved = std::mem::take(&mut members[gone]);
        members[keep].extend(moved);
        for k in (0..n).filter(|&k| active[k]) {
            if k == keep || best[k] == Some(keep) || best[k] == Some(gone) {
                best[k] = best_of(&sim, &active, n, k);
            } else {
                // only the merged row changed for this one: it may now be the most alike
                let cand = sim[k * n + keep];
                let beats = match best[k] {
                    None => true,
                    Some(c) => {
                        let cur = sim[k * n + c];
                        cand > cur || (cand == cur && keep < c)
                    }
                };
                if beats {
                    best[k] = Some(keep);
                }
            }
        }
    }
    (0..n)
        .filter(|&i| active[i])
        .map(|i| {
            let mut m = members[i].clone();
            m.sort_unstable();
            m
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unit vector of 64 dimensions with the given parts.
    fn unit(parts: &[(usize, f32)]) -> Vec<f32> {
        let mut v = vec![0.0f32; 64];
        for (i, x) in parts {
            v[*i] = *x;
        }
        let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.iter().map(|x| x / n).collect()
    }

    fn refs(v: &[Vec<f32>]) -> Vec<&[f32]> {
        v.iter().map(|e| &e[..]).collect()
    }

    fn free(_: usize, _: usize) -> bool {
        false
    }

    #[test]
    fn three_groups_of_look_alikes_cluster_into_three() {
        // faces 0..12 interleave three people (face k is person k % 3), each a little different from the others
        let mut all: Vec<Vec<f32>> = (0..12).map(|k| unit(&[(k % 3, 1.0), (12 + k / 3, 0.05 * (k / 3) as f32)])).collect();
        all.push(unit(&[(40, 1.0)]));
        let groups = cluster(&refs(&all), 0.45, MAX_PER_PASS, &free);
        assert_eq!(groups, vec![vec![0, 3, 6, 9], vec![1, 4, 7, 10], vec![2, 5, 8, 11], vec![12]]);
        // the same faces give the same groups every time
        assert_eq!(groups, cluster(&refs(&all), 0.45, MAX_PER_PASS, &free));
        // at a bar nothing reaches, every face is alone
        assert_eq!(cluster(&refs(&all), 1.5, MAX_PER_PASS, &free).len(), 13);
    }

    #[test]
    fn a_rejected_pair_is_split_and_no_group_holds_both() {
        // faces 0, 1 and 2 look the same; 0 and 1 must stay apart, so 2 joins one of them and the other stands alone
        let same = unit(&[(0, 1.0)]);
        let all = vec![same.clone(), same.clone(), same];
        let rejected = |i: usize, j: usize| (i, j) == (0, 1);
        assert_eq!(cluster(&refs(&all), 0.45, MAX_PER_PASS, &rejected), vec![vec![0, 2], vec![1]]);
        // the mark counts in either direction
        let flipped = |i: usize, j: usize| (i, j) == (1, 0);
        assert_eq!(cluster(&refs(&all), 0.45, MAX_PER_PASS, &flipped), vec![vec![0, 2], vec![1]]);
    }

    #[test]
    fn passes_are_capped_and_never_merge_across_each_other() {
        let same = unit(&[(3, 1.0)]);
        let all: Vec<Vec<f32>> = (0..10).map(|_| same.clone()).collect();
        // one pass takes everything
        assert_eq!(cluster(&refs(&all), 0.45, 100, &free), vec![(0..10).collect::<Vec<_>>()]);
        // at most 4 faces a pass: passes of 4, 4 and 2, and no group crosses a pass
        let capped = cluster(&refs(&all), 0.45, 4, &free);
        assert_eq!(capped, vec![vec![0, 1, 2, 3], vec![4, 5, 6, 7], vec![8, 9]]);
        assert!(capped.iter().all(|g| g.len() <= 4));
        // a pass of zero is treated as one: every face alone, every index listed once
        assert_eq!(cluster(&refs(&all), 0.45, 0, &free).len(), 10);
        // a face with no usable embedding stays alone, and does not stop the others
        let bad = vec![same.clone(), vec![f32::NAN; 64], same.clone(), vec![1.0; 3]];
        assert_eq!(cluster(&refs(&bad), 0.45, MAX_PER_PASS, &free), vec![vec![0, 2], vec![1], vec![3]]);
        assert!(cluster(&[], 0.45, MAX_PER_PASS, &free).is_empty());
    }
}
