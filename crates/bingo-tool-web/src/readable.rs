//! A page as its article. Readability drops the navigation, the scripts and the
//! chrome; what is left is rewritten as markdown, which is the form a model
//! reads a document in.

use dom_smoothie::Readability;
use htmd::HtmlToMarkdown;

#[derive(Debug, thiserror::Error)]
#[error("the page could not be converted to markdown: {0}")]
pub(crate) struct NotMarkdown(#[from] std::io::Error);

/// The article of an HTML page, as markdown. A document Readability makes no
/// article of is converted whole: a page with its chrome still beats no page.
pub(crate) fn markdown(html: &str, url: &str) -> Result<String, NotMarkdown> {
    let content = article(html, url);
    Ok(converter().convert(content.as_deref().unwrap_or(html))?)
}

fn article(html: &str, url: &str) -> Option<String> {
    let mut readability = Readability::new(html, Some(url), None).ok()?;
    let article = readability.parse().ok()?;
    Some(article.content.to_string())
}

/// Readability has already dropped both on the article path; the whole-document
/// fallback has not, and a script in the context is noise the model pays for.
fn converter() -> HtmlToMarkdown {
    HtmlToMarkdown::builder()
        .skip_tags(vec!["script", "style"])
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = r#"
        <html>
          <head><title>Guide</title><style>.a { color: red }</style></head>
          <body>
            <nav><a href="/elsewhere">Navigation</a></nav>
            <article>
              <h1>Installing the thing</h1>
              <p>Run the <a href="https://example.com/install">installer</a> first.</p>
              <h2>Afterwards</h2>
              <p>A second paragraph long enough that Readability keeps the article
                 rather than deciding the page has no content worth extracting at
                 all, which it does for very short documents.</p>
            </article>
            <script>console.log("tracking")</script>
            <footer>Copyright nobody</footer>
          </body>
        </html>
    "#;

    #[test]
    fn headings_and_links_survive_the_conversion() {
        let out = markdown(PAGE, "https://example.com/guide").expect("markdown");
        assert!(out.contains("# Installing the thing"), "got {out}");
        assert!(out.contains("## Afterwards"), "got {out}");
        assert!(
            out.contains("[installer](https://example.com/install)"),
            "got {out}"
        );
    }

    #[test]
    fn the_navigation_and_the_scripts_do_not() {
        let out = markdown(PAGE, "https://example.com/guide").expect("markdown");
        assert!(!out.contains("tracking"), "got {out}");
        assert!(!out.contains("color: red"), "got {out}");
        assert!(!out.contains("Navigation"), "got {out}");
    }

    #[test]
    fn a_document_with_no_article_in_it_is_still_converted() {
        let out = markdown(
            "<html><body><script>x()</script><p>Just this.</p></body></html>",
            "https://example.com/",
        )
        .expect("markdown");
        assert!(out.contains("Just this."), "got {out}");
        assert!(!out.contains("x()"), "got {out}");
    }
}
