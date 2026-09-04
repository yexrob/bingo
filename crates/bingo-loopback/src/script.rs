//! The one script a served page carries.
//!
//! It is what makes the page a round trip rather than a picture: two functions
//! on `window.bingo`, and one POST to this page's own `/answer`. The path comes
//! from `location.pathname`, not from the token spelled a second time — the URL
//! the browser is already on *is* the token (ADR-0042 §3).

/// Where the script goes when the page has a body to close.
const BODY_END: &str = "</body>";

/// `window.bingo.submit(value)` posts the value and says so; `.cancel()` posts
/// that nobody answered. Either way the first post is the only one: a page that
/// submits twice would answer a call that has already ended.
pub const SCRIPT: &str = r##"<script>
(function () {
  var sent = false;
  function notice(text) {
    var bar = document.createElement("div");
    bar.textContent = text;
    bar.setAttribute("style", "position:fixed;left:0;right:0;top:0;z-index:2147483647;" +
      "padding:10px 14px;text-align:center;font:14px system-ui,-apple-system,sans-serif;" +
      "background:#111;color:#fff");
    document.body.appendChild(bar);
  }
  function post(payload, done) {
    if (sent) { return; }
    sent = true;
    fetch(location.pathname.replace(/\/+$/, "") + "/answer", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(payload)
    }).then(function () {
      notice(done);
    }, function (failure) {
      sent = false;
      notice("bingo did not take that: " + failure);
    });
  }
  window.bingo = {
    submit: function (value) {
      post({ value: value === undefined ? null : value }, "sent — you can close this tab");
    },
    cancel: function () {
      post({ cancelled: true }, "cancelled — you can close this tab");
    }
  };
})();
</script>"##;

/// The page with the script in it, once, as late as the page allows.
///
/// `to_ascii_lowercase` to find the tag however it was cased, and only for the
/// search: it is length-preserving on ASCII, so the index it yields is an index
/// into the page as written.
pub fn inject(html: &str) -> String {
    match html.to_ascii_lowercase().rfind(BODY_END) {
        Some(at) => format!("{}{SCRIPT}{}", &html[..at], &html[at..]),
        None => format!("{html}{SCRIPT}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn injected(html: &str) -> String {
        inject(html)
    }

    #[test]
    fn the_script_names_both_halves_of_the_round_trip() {
        assert!(SCRIPT.contains("window.bingo"), "{SCRIPT}");
        assert!(SCRIPT.contains("submit:"), "{SCRIPT}");
        assert!(SCRIPT.contains("cancel:"), "{SCRIPT}");
        assert!(SCRIPT.contains("\"/answer\""), "{SCRIPT}");
        // The token is never written into the page: the path is already it.
        assert!(SCRIPT.contains("location.pathname"), "{SCRIPT}");
    }

    #[test]
    fn a_page_with_a_body_carries_the_script_just_before_it_closes() {
        let page = injected("<html><body><p>pick</p></body></html>");
        assert_eq!(page.matches("window.bingo").count(), 1, "{page}");
        let script = page.find("<script>").expect("a script");
        let end = page.find("</body>").expect("a body end");
        assert!(script < end, "{page}");
    }

    #[test]
    fn a_page_without_a_body_carries_it_at_the_end() {
        let page = injected("<p>pick</p>");
        assert_eq!(page.matches("window.bingo").count(), 1, "{page}");
        assert!(page.starts_with("<p>pick</p>"), "{page}");
        assert!(page.trim_end().ends_with("</script>"), "{page}");
    }

    /// A page that closes its body twice — a comment, a string — gets the
    /// script at the last one, which is the one that really closes it.
    #[test]
    fn the_last_body_end_is_the_one_used_and_the_case_does_not_matter() {
        let page = injected("<body>a<!-- </body> -->b</BODY>");
        assert_eq!(page.matches("window.bingo").count(), 1, "{page}");
        let script = page.find("<script>").expect("a script");
        assert!(script > page.find("<!--").expect("the comment"), "{page}");
        assert!(page.ends_with("</BODY>"), "{page}");
    }
}
