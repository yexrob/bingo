//! WebFetch preapproved domains (code-related public documentation domains).
//! Only affects WebFetch permission decisions (GET requests); no network sandbox
//! involvement.

const PREAPPROVED: &[&str] = &[
    "modelcontextprotocol.io",
    "github.com/anthropics",
    "agentskills.io",
    // Programming language docs
    "docs.python.org",
    "en.cppreference.com",
    "docs.oracle.com",
    "learn.microsoft.com",
    "developer.mozilla.org",
    "go.dev",
    "pkg.go.dev",
    "www.php.net",
    "docs.swift.org",
    "kotlinlang.org",
    "ruby-doc.org",
    "doc.rust-lang.org",
    "www.typescriptlang.org",
    // Web & JS frameworks
    "react.dev",
    "angular.io",
    "vuejs.org",
    "nextjs.org",
    "expressjs.com",
    "nodejs.org",
    "bun.sh",
    "jquery.com",
    "getbootstrap.com",
    "tailwindcss.com",
    "d3js.org",
    "threejs.org",
    "redux.js.org",
    "webpack.js.org",
    "jestjs.io",
    "reactrouter.com",
    // Python ecosystem
    "docs.djangoproject.com",
    "flask.palletsprojects.com",
    "fastapi.tiangolo.com",
    "pandas.pydata.org",
    "numpy.org",
    "www.tensorflow.org",
    "pytorch.org",
    "scikit-learn.org",
    "matplotlib.org",
    "requests.readthedocs.io",
    "jupyter.org",
    // PHP
    "laravel.com",
    "symfony.com",
    "wordpress.org",
    // Java
    "docs.spring.io",
    "hibernate.org",
    "tomcat.apache.org",
    "gradle.org",
    "maven.apache.org",
    // .NET / C#
    "asp.net",
    "dotnet.microsoft.com",
    "nuget.org",
    "blazor.net",
    // Mobile
    "reactnative.dev",
    "docs.flutter.dev",
    "developer.apple.com",
    "developer.android.com",
    // Data & ML
    "keras.io",
    "spark.apache.org",
    "huggingface.co",
    "www.kaggle.com",
    // Databases
    "www.mongodb.com",
    "redis.io",
    "www.postgresql.org",
    "dev.mysql.com",
    "www.sqlite.org",
    "graphql.org",
    "prisma.io",
    // Cloud & DevOps
    "docs.aws.amazon.com",
    "cloud.google.com",
    "kubernetes.io",
    "www.docker.com",
    "www.terraform.io",
    "www.ansible.com",
    "vercel.com/docs",
    "docs.netlify.com",
    "devcenter.heroku.com",
    // Testing & monitoring
    "cypress.io",
    "selenium.dev",
    // Gaming
    "docs.unity.com",
    "docs.unrealengine.com",
    // Other
    "git-scm.com",
    "nginx.org",
    "httpd.apache.org",
];

/// Whether a URL falls on the preapproved list (built-in entries only).
/// Production callers pass settings extras via `is_preapproved_url_with`; this
/// default entry (no extras) is exercised by the test suite.
#[cfg_attr(not(test), expect(dead_code))]
pub fn is_preapproved_url(url: &str) -> bool {
    is_preapproved_url_with(url, &[])
}

/// Whether a URL falls on the preapproved list, built-in entries plus the
/// settings-configured extras (same entry syntax: host or host/path-prefix).
/// Path-prefix entries require segment boundary matching ("/anthropics" doesn't
/// match "/anthropics-evil").
pub fn is_preapproved_url_with(url: &str, extra: &[String]) -> bool {
    let Ok(parsed) = url::Url::parse(url) else {
        return false;
    };
    let Some(hostname) = parsed.host_str() else {
        return false;
    };
    let pathname = parsed.path();
    for entry in PREAPPROVED.iter().copied().chain(extra.iter().map(|s| s.as_str())) {
        let (host, prefix) = match entry.split_once('/') {
            Some((h, p)) => (h, Some(p.to_string())),
            None => (entry, None),
        };
        if hostname != host {
            continue;
        }
        let matched = match prefix {
            None => true,
            // The entry's path segment ("github.com/anthropics") has no leading slash
            // after the split; add it back when comparing.
            Some(p) => pathname == format!("/{p}") || pathname.starts_with(&format!("/{p}/")),
        };
        if matched {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostname_entries_match_any_path() {
        assert!(is_preapproved_url("https://doc.rust-lang.org/book/"));
        assert!(is_preapproved_url("https://developer.mozilla.org/en-US/docs/Web"));
    }

    #[test]
    fn path_prefix_entries_require_segment_boundary() {
        assert!(is_preapproved_url("https://github.com/anthropics/anthropic-sdk-python"));
        assert!(!is_preapproved_url("https://github.com/anthropics-evil/repo"));
        assert!(!is_preapproved_url("https://github.com/other/repo"));
    }

    #[test]
    fn unknown_domains_rejected() {
        assert!(!is_preapproved_url("https://example.com/"));
        assert!(!is_preapproved_url("https://www.anthropic.com/"));
    }

    #[test]
    fn extra_domains_apply_without_touching_builtins() {
        let extra = ["example.com".to_string(), "corp.example.com/docs".to_string()];
        // Extra host and path-prefix entries match; the path prefix keeps segment
        // boundary semantics.
        assert!(is_preapproved_url_with("https://example.com/a/b", &extra));
        assert!(is_preapproved_url_with("https://corp.example.com/docs/guide", &extra));
        assert!(!is_preapproved_url_with("https://corp.example.com/docs-evil", &extra));
        assert!(!is_preapproved_url_with("https://other.example.com/", &extra));
        // Built-ins still work and extras don't leak into the plain check.
        assert!(is_preapproved_url_with("https://doc.rust-lang.org/book/", &extra));
        assert!(!is_preapproved_url("https://example.com/"));
    }
}
