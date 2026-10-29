//! Complete-linkage grouping inside a near-duplicate component and the `find_duplicates` entry point.

use super::*;

/// Split a component into groups where every internal pair meets `threshold`, growing
/// cliques from the strongest remaining edge.
pub(super) fn complete_linkage_groups(
    members: &[usize],
    names: &[String],
    fingerprints: &[u64],
    threshold: f64,
) -> Vec<Vec<usize>> {
    let mut adjacency: HashMap<usize, HashSet<usize>> =
        members.iter().map(|m| (*m, HashSet::new())).collect();
    let mut edges: Vec<(f64, usize, usize)> = Vec::new();
    for (i, &a) in members.iter().enumerate() {
        for &b in &members[i + 1..] {
            let sim = similarity(fingerprints[a], fingerprints[b]);
            if sim >= threshold {
                adjacency.entry(a).or_default().insert(b);
                adjacency.entry(b).or_default().insert(a);
                edges.push((sim, a, b));
            }
        }
    }
    edges.sort_by(|x, y| {
        y.0.total_cmp(&x.0)
            .then_with(|| names[x.1].cmp(&names[y.1]))
            .then_with(|| names[x.2].cmp(&names[y.2]))
    });
    let mut remaining: HashSet<usize> = members.iter().copied().collect();
    let mut groups = Vec::new();
    for (_, a, b) in edges {
        if !remaining.contains(&a) || !remaining.contains(&b) {
            continue;
        }
        let mut clique: HashSet<usize> = HashSet::from([a, b]);
        let mut rest: Vec<usize> = remaining
            .iter()
            .copied()
            .filter(|n| !clique.contains(n))
            .collect();
        rest.sort_by(|x, y| names[*x].cmp(&names[*y]));
        for node in rest {
            if clique.iter().all(|m| adjacency[&node].contains(m)) {
                clique.insert(node);
            }
        }
        for m in &clique {
            remaining.remove(m);
        }
        let mut group: Vec<usize> = clique.into_iter().collect();
        group.sort_by(|x, y| names[*x].cmp(&names[*y]));
        groups.push(group);
    }
    groups
}

pub(super) fn find_root(parent: &mut [usize], mut x: usize) -> usize {
    while parent[x] != x {
        parent[x] = parent[parent[x]];
        x = parent[x];
    }
    x
}

/// Round to 4 decimal places, correctly rounded.
pub(crate) fn round4(x: f64) -> f64 {
    format!("{x:.4}").parse().unwrap_or(x)
}

/// Find exact and near-duplicate groups.
/// Errors: threshold outside `[0,1]` -> `Invalid`; budget exhausted -> `BudgetExceeded`.
pub fn find_duplicates(
    items: &[DuplicateItem],
    options: &DuplicateOptions,
) -> Result<DuplicateReport> {
    let threshold = options.threshold;
    if !threshold.is_finite() || !(0.0..=1.0).contains(&threshold) {
        return Err(ParseError::Invalid(
            "duplicate threshold must be finite and in [0, 1]".to_owned(),
        ));
    }
    let mut excluded_non_indexable = 0u64;
    let kept: Vec<&DuplicateItem> = if options.only_indexable {
        let kept: Vec<_> = items.iter().filter(|it| it.indexable).collect();
        excluded_non_indexable = (items.len() - kept.len()) as u64;
        kept
    } else {
        items.iter().collect()
    };

    if kept.is_empty() {
        return Ok(DuplicateReport {
            ok: true,
            count: 0,
            threshold: None,
            clusters: Vec::new(),
            exact_duplicates: Vec::new(),
            candidate_pairs_checked: 0,
            excluded_non_indexable,
            excluded_no_id: 0,
            excluded_no_text: 0,
            excluded_duplicate_id: 0,
            measured: false,
            reason: Some(UNMEASURED_REASON.to_owned()),
            fingerprints: options.with_fingerprints.then(BTreeMap::new),
        });
    }

    let mut names: Vec<String> = Vec::new();
    let mut texts: Vec<&str> = Vec::new();
    let mut doc_shingles: Vec<Vec<Vec<String>>> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    let mut shingle_count = 0usize;
    let (mut excluded_no_id, mut excluded_no_text, mut excluded_duplicate_id) = (0u64, 0u64, 0u64);
    for it in &kept {
        if it.id.is_empty() {
            excluded_no_id += 1;
        } else if it.text.trim_matches(crate::py_str::is_py_space).is_empty() {
            excluded_no_text += 1;
        } else {
            if !seen.insert(it.id.as_str()) {
                excluded_duplicate_id += 1;
                continue;
            }
            names.push(it.id.clone());
            texts.push(&it.text);
            let tokens = tokenize(&it.text);
            // Count before materializing: one over-budget document must fail without
            // allocating its k Strings per shingle.
            let piece_count = if tokens.is_empty() {
                0
            } else if tokens.len() <= options.k {
                1
            } else {
                tokens.len() - options.k + 1
            };
            if options
                .max_shingles
                .is_some_and(|max| shingle_count + piece_count > max)
            {
                return Err(ParseError::BudgetExceeded(
                    "duplicate shingle budget exceeded".to_owned(),
                ));
            }
            shingle_count += piece_count;
            doc_shingles.push(shingles(&tokens, options.k));
        }
    }

    let template = template_shingles(&doc_shingles.iter().collect::<Vec<_>>());
    let silenced: Option<HashSet<&[String]>> =
        (!template.is_empty()).then(|| template.iter().map(Vec::as_slice).collect());
    let fingerprints: Vec<u64> = doc_shingles
        .iter()
        .map(|sh| weighted_simhash(sh, silenced.as_ref()))
        .collect();

    let mut hash_groups: Vec<(String, Vec<usize>)> = Vec::new();
    let mut hash_index: HashMap<String, usize> = HashMap::new();
    let mut hash_of: Vec<String> = Vec::with_capacity(texts.len());
    for (i, text) in texts.iter().enumerate() {
        let hash = content_hash(text);
        match hash_index.get(&hash) {
            Some(&g) => hash_groups[g].1.push(i),
            None => {
                hash_index.insert(hash.clone(), hash_groups.len());
                hash_groups.push((hash.clone(), vec![i]));
            }
        }
        hash_of.push(hash);
    }
    let mut exact_duplicates: Vec<ExactDuplicateGroup> = hash_groups
        .iter()
        .filter(|(_, members)| members.len() > 1)
        .map(|(hash, members)| {
            let mut ids: Vec<String> = members.iter().map(|m| names[*m].clone()).collect();
            ids.sort();
            ExactDuplicateGroup {
                hash: hash.clone(),
                members: ids,
            }
        })
        .collect();
    exact_duplicates.sort_by(|a, b| a.hash.cmp(&b.hash));

    let bands = 64usize.min((64.0 * (1.0 - threshold)).floor() as usize + 1);
    let band_bits = 64 / bands;
    let mut buckets: Vec<Vec<usize>> = Vec::new();
    let mut bucket_index: HashMap<(usize, u64), usize> = HashMap::new();
    for (doc, fp) in fingerprints.iter().enumerate() {
        let keys = if threshold > 0.0 {
            band_keys(*fp, bands, band_bits)
        } else {
            vec![0]
        };
        for (band, key) in keys.into_iter().enumerate() {
            let slot = *bucket_index.entry((band, key)).or_insert_with(|| {
                buckets.push(Vec::new());
                buckets.len() - 1
            });
            buckets[slot].push(doc);
        }
    }

    let mut parent: Vec<usize> = (0..names.len()).collect();
    let mut candidate_pairs: HashSet<(usize, usize)> = HashSet::new();
    let mut comparisons = 0usize;
    for members in &buckets {
        if members.len() < 2 {
            continue;
        }
        for i in 0..members.len() {
            for j in i + 1..members.len() {
                comparisons += 1;
                if options
                    .max_candidate_comparisons
                    .is_some_and(|max| comparisons > max)
                {
                    return Err(ParseError::BudgetExceeded(
                        "duplicate candidate-comparison budget exceeded".to_owned(),
                    ));
                }
                let (a, b) = (members[i], members[j]);
                if similarity(fingerprints[a], fingerprints[b]) >= threshold {
                    candidate_pairs.insert((a.min(b), a.max(b)));
                    let (ra, rb) = (find_root(&mut parent, a), find_root(&mut parent, b));
                    if ra != rb {
                        parent[rb] = ra;
                    }
                }
            }
        }
    }

    let mut components: Vec<Vec<usize>> = Vec::new();
    let mut component_of_root: HashMap<usize, usize> = HashMap::new();
    for doc in 0..names.len() {
        let root = find_root(&mut parent, doc);
        let slot = *component_of_root.entry(root).or_insert_with(|| {
            components.push(Vec::new());
            components.len() - 1
        });
        components[slot].push(doc);
    }

    let mut clusters: Vec<DuplicateCluster> = Vec::new();
    for members in &components {
        if members.len() < 2 {
            continue;
        }
        for clique in complete_linkage_groups(members, &names, &fingerprints, threshold) {
            let first = &hash_of[clique[0]];
            if clique.iter().all(|m| &hash_of[*m] == first) {
                continue;
            }
            let mut pairs = Vec::new();
            for (i, &a) in clique.iter().enumerate() {
                for &b in &clique[i + 1..] {
                    pairs.push(DuplicatePair {
                        a: names[a].clone(),
                        b: names[b].clone(),
                        similarity: round4(similarity(fingerprints[a], fingerprints[b])),
                    });
                }
            }
            let min_similarity = pairs
                .iter()
                .map(|p| p.similarity)
                .fold(f64::INFINITY, f64::min);
            clusters.push(DuplicateCluster {
                members: clique.iter().map(|m| names[*m].clone()).collect(),
                pairs,
                min_similarity,
            });
        }
    }
    clusters.sort_by(|a, b| b.min_similarity.total_cmp(&a.min_similarity));

    let measured = !candidate_pairs.is_empty();
    Ok(DuplicateReport {
        ok: true,
        count: names.len() as u64,
        threshold: Some(threshold),
        clusters,
        exact_duplicates,
        candidate_pairs_checked: candidate_pairs.len() as u64,
        excluded_non_indexable,
        excluded_no_id,
        excluded_no_text,
        excluded_duplicate_id,
        measured,
        reason: (!measured).then(|| UNMEASURED_REASON.to_owned()),
        fingerprints: options.with_fingerprints.then(|| {
            names
                .iter()
                .cloned()
                .zip(fingerprints.iter().copied())
                .collect()
        }),
    })
}
