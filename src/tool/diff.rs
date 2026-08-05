//! 行级 unified diff（git 格式）生成，Edit/Write 结果预览用。
//! 与 rsmarkdown-tui 的 `Diff::parse_unified` 契约对应（---/+++/@@/-/+/空格）。

/// hunk 上下文行数。
const CONTEXT_LINES: usize = 3;
/// 行级 diff 上限：超过直接全量替换（避免 O(n·m) LCS 内存爆炸）。
const MAX_LCS_LINES: usize = 2_000;

/// 两个文本的行级 unified diff（`--- a/` / `+++ b/` 头）。无变更时返回 None。
pub fn unified_diff(path: &str, old: &str, new: &str) -> Option<String> {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    if a == b {
        return None;
    }
    let mut out = format!("--- a/{path}\n+++ b/{path}\n");
    if a.len() + b.len() > MAX_LCS_LINES {
        let os = 1usize;
        let oe = a.len().max(1);
        let ns = 1usize;
        let ne = b.len().max(1);
        out.push_str(&format!("@@ -{os},{oe} +{ns},{ne} @@\n"));
        for line in &a {
            out.push_str(&format!("-{line}\n"));
        }
        for line in &b {
            out.push_str(&format!("+{line}\n"));
        }
        return Some(out);
    }
    let matches = lcs_path(&a, &b);
    // 变化段：匹配对之间的 a[..] 删除区与 b[..] 添加区（段起点/终点均为开区间）。
    let mut segments: Vec<(usize, usize, usize, usize)> = Vec::new();
    let (mut pi, mut pj) = (0usize, 0usize);
    for &(i, j) in &matches {
        if i > pi || j > pj {
            segments.push((pi, pj, i, j));
        }
        pi = i + 1;
        pj = j + 1;
    }
    if a.len() > pi || b.len() > pj {
        segments.push((pi, pj, a.len(), b.len()));
    }
    if segments.is_empty() {
        return None;
    }
    // 组 hunk：段 ± CONTEXT 行；相邻段间隔 ≤ 2×CONTEXT 时合并（近似合并，行号取并集）。
    let mut hunks: Vec<(usize, usize, usize, usize)> = Vec::new();
    for (s0, s1, s2, s3) in segments {
        let os = s0.saturating_sub(CONTEXT_LINES);
        let oe = (s2 + CONTEXT_LINES).min(a.len());
        let ns = s1.saturating_sub(CONTEXT_LINES);
        let ne = (s3 + CONTEXT_LINES).min(b.len());
        if let Some(last) = hunks.last_mut()
            && os <= last.1 + 1
        {
            last.1 = last.1.max(oe);
            last.3 = last.3.max(ne);
        } else {
            hunks.push((os, oe, ns, ne));
        }
    }
    for (os, oe, ns, ne) in hunks {
        out.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            os + 1,
            oe - os,
            ns + 1,
            ne - ns
        ));
        Hunk { out: &mut out, a: &a, b: &b, matches: &matches }.emit(os, oe, ns, ne);
    }
    Some(out)
}

/// hunk 内按 LCS 匹配对输出 context / removed / added 行。
struct Hunk<'a> {
    out: &'a mut String,
    a: &'a [&'a str],
    b: &'a [&'a str],
    matches: &'a [(usize, usize)],
}

impl Hunk<'_> {
    fn emit(&mut self, os: usize, oe: usize, ns: usize, ne: usize) {
        let mut k = self.matches.partition_point(|&(i, _)| i < os);
        let (mut i, mut j) = (os, ns);
        while i < oe && j < ne {
            if k < self.matches.len() && self.matches[k] == (i, j) {
                self.line(" ", self.a[i]);
                i += 1;
                j += 1;
                k += 1;
            } else if k < self.matches.len() && self.matches[k].0 == i {
                self.line("+", self.b[j]);
                j += 1;
            } else if k < self.matches.len() && self.matches[k].1 == j {
                self.line("-", self.a[i]);
                i += 1;
            } else {
                // 下一个匹配对在更远处：a[i] 先删除，b[j] 后添加（交错输出）。
                self.line("-", self.a[i]);
                i += 1;
            }
        }
        while i < oe {
            self.line("-", self.a[i]);
            i += 1;
        }
        while j < ne {
            self.line("+", self.b[j]);
            j += 1;
        }
    }

    fn line(&mut self, prefix: &str, text: &str) {
        self.out.push_str(prefix);
        self.out.push_str(text);
        self.out.push('\n');
    }
}

/// LCS 匹配对序列（a 索引, b 索引），用于 hunk 内区分 context / 变更。
fn lcs_path(a: &[&str], b: &[&str]) -> Vec<(usize, usize)> {
    let (n, m) = (a.len(), b.len());
    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if a[i] == b[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    let mut path = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if a[i] == b[j] {
            path.push((i, j));
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_texts_yield_none() {
        assert_eq!(unified_diff("f.txt", "a\nb\n", "a\nb\n"), None);
    }

    #[test]
    fn single_line_change() {
        let d = unified_diff("f.txt", "a\nb\nc\n", "a\nb2\nc\n").unwrap();
        assert!(d.contains("@@ -1,3 +1,3 @@"));
        assert!(d.contains("-b"));
        assert!(d.contains("+b2"));
        assert!(d.contains(" a"));
        assert!(d.contains(" c"));
    }

    #[test]
    fn new_file_is_all_added() {
        let d = unified_diff("new.txt", "", "x\ny\n").unwrap();
        assert!(d.contains("+++ b/new.txt"));
        assert!(d.contains("+x"));
        assert!(d.contains("+y"));
    }

    #[test]
    fn deletion_only() {
        let d = unified_diff("f.txt", "a\nb\nc\n", "a\nc\n").unwrap();
        assert!(d.contains("-b"));
        assert!(!d.contains("+b"));
    }

    #[test]
    fn round_trips_through_lib_parser() {
        let old = "one\ntwo\nthree\nfour\nfive\n";
        let new = "one\ntwo\n2.5\nfour\nfive\nsix\n";
        let d = unified_diff("f.txt", old, new).unwrap();
        let parsed = crate::tui::activities::Diff::parse_unified(&d);
        assert_eq!(parsed.path, "f.txt");
        let (added, removed) = parsed.stats();
        assert_eq!(added, 2);
        assert_eq!(removed, 1);
    }

    #[test]
    fn large_files_fallback_to_full_replace() {
        let big = "x\n".repeat(1_100);
        let d = unified_diff("big.txt", &big, &format!("{big}y\n")).unwrap();
        assert!(d.contains("-x"));
        assert!(d.contains("+y"));
    }
}
