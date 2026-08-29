//! What happens to a backend's results before the model sees them: the domains
//! the call asked for, the count it is allowed, and the markdown it reads.

use crate::backend::Hit;

/// The most one search returns. Past this a search is a reading list.
pub(crate) const MAX_HITS: usize = 8;

/// The hits the call asked for, at most `MAX_HITS` of them.
pub(crate) fn filter(hits: Vec<Hit>, allowed: &[String], blocked: &[String]) -> Vec<Hit> {
    hits.into_iter()
        .filter(|hit| permitted(&host(&hit.url), allowed, blocked))
        .take(MAX_HITS)
        .collect()
}

/// An allow list is exclusive: given one, the block list has nothing left to
/// say, and a result whose URL has no host belongs to neither list.
fn permitted(host: &str, allowed: &[String], blocked: &[String]) -> bool {
    if allowed.is_empty() {
        !blocked.iter().any(|domain| covers(domain, host))
    } else {
        allowed.iter().any(|domain| covers(domain, host))
    }
}

fn covers(domain: &str, host: &str) -> bool {
    host == domain || host.ends_with(&format!(".{domain}"))
}

fn host(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))
        .unwrap_or_default()
}

/// A numbered list; nothing found is an answer, not a failure.
pub(crate) fn render(query: &str, hits: &[Hit]) -> String {
    if hits.is_empty() {
        return format!("No results for \"{query}\".");
    }
    let entries: Vec<String> = hits
        .iter()
        .enumerate()
        .map(|(index, hit)| entry(index + 1, hit))
        .collect();
    format!("Results for \"{query}\":\n\n{}", entries.join("\n\n"))
}

fn entry(number: usize, hit: &Hit) -> String {
    let head = format!("{number}. {} — {}", hit.title, hit.url);
    match hit.snippet.is_empty() {
        true => head,
        false => format!("{head}\n   {}", hit.snippet),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(url: &str) -> Hit {
        Hit {
            title: "Title".into(),
            url: url.into(),
            snippet: "Snippet".into(),
        }
    }

    fn urls(hits: Vec<Hit>) -> Vec<String> {
        hits.into_iter().map(|hit| hit.url).collect()
    }

    fn domains(list: &[&str]) -> Vec<String> {
        list.iter().map(|d| (*d).to_string()).collect()
    }

    #[test]
    fn with_no_lists_everything_is_kept() {
        let found = vec![hit("https://a.example/1"), hit("https://b.example/2")];
        assert_eq!(urls(filter(found, &[], &[])).len(), 2);
    }

    #[test]
    fn an_allow_list_keeps_the_domain_and_its_subdomains_only() {
        let found = vec![
            hit("https://rust-lang.org/a"),
            hit("https://doc.rust-lang.org/b"),
            hit("https://other.org/c"),
            hit("https://notrust-lang.org/d"),
        ];
        assert_eq!(
            urls(filter(found, &domains(&["rust-lang.org"]), &[])),
            vec!["https://rust-lang.org/a", "https://doc.rust-lang.org/b"]
        );
    }

    #[test]
    fn a_block_list_drops_the_domain_and_its_subdomains() {
        let found = vec![
            hit("https://spam.example/a"),
            hit("https://deep.spam.example/b"),
            hit("https://good.org/c"),
        ];
        assert_eq!(
            urls(filter(found, &[], &domains(&["spam.example"]))),
            vec!["https://good.org/c"]
        );
    }

    #[test]
    fn an_allow_list_settles_it_when_both_are_given() {
        let found = vec![hit("https://a.example/1"), hit("https://b.example/2")];
        assert_eq!(
            urls(filter(
                found,
                &domains(&["a.example"]),
                &domains(&["a.example"])
            )),
            vec!["https://a.example/1"]
        );
    }

    #[test]
    fn no_more_than_eight_results_are_returned() {
        let found: Vec<Hit> = (0..20)
            .map(|i| hit(&format!("https://a.example/{i}")))
            .collect();
        assert_eq!(filter(found, &[], &[]).len(), MAX_HITS);
    }

    #[test]
    fn results_are_a_numbered_list_of_title_url_and_snippet() {
        let rendered = render("rust async", &[hit("https://a.example/1")]);
        assert_eq!(
            rendered,
            "Results for \"rust async\":\n\n1. Title — https://a.example/1\n   Snippet"
        );
    }

    #[test]
    fn a_hit_with_no_snippet_is_one_line() {
        let bare = Hit {
            snippet: String::new(),
            ..hit("https://a.example/1")
        };
        assert!(render("q", &[bare]).ends_with("1. Title — https://a.example/1"));
    }

    #[test]
    fn nothing_found_says_so() {
        assert_eq!(
            render("nothing at all", &[]),
            "No results for \"nothing at all\"."
        );
    }
}
