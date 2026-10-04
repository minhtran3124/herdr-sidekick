//! Fuzzy path matching for the file picker: the query's characters must appear in order
//! (case-insensitive); matches in the file name, at word starts, or in a run rank higher.

/// The best `limit` matches as (index into `paths`, matched char positions), best first.
/// `paths` must already be lowercase (the picker lowercases them once, not per keystroke).
/// An empty query lists the first `limit` paths unranked.
pub fn rank(query: &str, paths: &[String], limit: usize) -> Vec<(usize, Vec<usize>)> {
    if query.is_empty() {
        return (0..paths.len().min(limit)).map(|i| (i, Vec::new())).collect();
    }
    let q: Vec<char> = query.to_lowercase().chars().filter(|c| !c.is_whitespace()).collect();
    let whole: String = q.iter().collect();
    let mut scored: Vec<(i64, usize, Vec<usize>)> =
        paths.iter().enumerate().filter_map(|(i, p)| score(&q, &whole, p).map(|(s, hits)| (s, i, hits))).collect();
    // Best score first; ties keep the sorted path order.
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().take(limit).map(|(_, i, hits)| (i, hits)).collect()
}

/// Greedy left-to-right match with bonuses; None when some query char has no match.
/// `lower` is the lowercased path, `whole` the query as one string.
fn score(q: &[char], whole: &str, lower: &str) -> Option<(i64, Vec<usize>)> {
    // A literal substring is what people type most often: take it whole, preferring the file name.
    let name_start_byte = lower.rfind('/').map_or(0, |i| i + 1);
    if let Some(at) = lower.rfind(whole) {
        let name_start = lower[..name_start_byte].chars().count();
        let at = lower[..at].chars().count();
        let hits: Vec<usize> = (at..at + q.len()).collect();
        let in_name = if at >= name_start { 500 } else { 0 };
        let exact = if lower == whole { 10_000 } else { 0 };
        return Some((1_000 + in_name + exact - lower.chars().count() as i64, hits));
    }

    let p: Vec<char> = lower.chars().collect();
    let name_start = p.iter().rposition(|&c| c == '/').map_or(0, |i| i + 1);
    // Greedy from the left picks early letters in directories ("aiapi" grabs the "a" of
    // "apps/"); greedy from the right keeps the match near the file name ("ai/api"). Score both.
    let left = align_left(q, &p)?;
    let right = align_right(q, &p)?;
    let (sl, sr) = (bonus(&left, &p, name_start), bonus(&right, &p, name_start));
    let (s, hits) = if sr > sl { (sr, right) } else { (sl, left) };
    Some((s - p.len() as i64 / 4, hits))
}

fn align_left(q: &[char], p: &[char]) -> Option<Vec<usize>> {
    let mut pos = 0;
    q.iter()
        .map(|&c| {
            let found = (pos..p.len()).find(|&i| p[i] == c)?;
            pos = found + 1;
            Some(found)
        })
        .collect()
}

fn align_right(q: &[char], p: &[char]) -> Option<Vec<usize>> {
    let mut end = p.len();
    let mut hits = q
        .iter()
        .rev()
        .map(|&c| {
            let found = (0..end).rev().find(|&i| p[i] == c)?;
            end = found;
            Some(found)
        })
        .collect::<Option<Vec<usize>>>()?;
    hits.reverse();
    Some(hits)
}

/// Runs, file-name hits and word starts score; gaps between hits cost.
fn bonus(hits: &[usize], p: &[char], name_start: usize) -> i64 {
    let mut s = 0;
    for (k, &h) in hits.iter().enumerate() {
        if k > 0 && hits[k - 1] + 1 == h {
            s += 15;
        } else if k > 0 {
            s -= (h - hits[k - 1] - 1).min(20) as i64;
        }
        if h >= name_start {
            s += 10;
        }
        if h == 0 || matches!(p[h - 1], '/' | '_' | '-' | '.' | ' ') {
            s += 8;
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::rank;

    fn top(query: &str, paths: &[&str]) -> String {
        let paths: Vec<String> = paths.iter().map(|p| p.to_string()).collect();
        rank(query, &paths, 5).first().map(|(i, _)| paths[*i].clone()).unwrap_or_default()
    }

    #[test]
    fn exact_path_wins_over_longer_paths_containing_it() {
        assert_eq!(top("src/main.rs", &["app/src/main.rs.bak", "src/main.rs", "x/src/main.rs"]), "src/main.rs");
    }

    #[test]
    fn file_name_matches_beat_directory_matches() {
        // "api" in the file name, not only in a directory on the way.
        assert_eq!(top("api", &["apps/api/app/main.py", "apps/web/lib/api.ts"]), "apps/web/lib/api.ts");
    }

    #[test]
    fn scattered_letters_match_in_order_and_runs_rank_higher() {
        let paths = ["apps/api/app/routers/ai/api.py", "apps/api/app/repositories/ai_logs.py"];
        assert_eq!(top("aiapi", &paths), "apps/api/app/routers/ai/api.py");
        assert!(rank("zzq", &paths.map(String::from), 5).is_empty());
    }

    #[test]
    fn match_positions_point_at_the_matched_characters() {
        let paths = vec!["src/lib.rs".to_string()];
        assert_eq!(rank("lib", &paths, 1)[0].1, vec![4, 5, 6]);
    }
}
