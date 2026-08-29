//! The public documentation a fetch may read without asking. Membership here is
//! the tool's `read_only` claim for that one call, so a docs lookup passes the
//! default gate and every other host still puts the question to a person.
//!
//! An entry is a host, or a host and a path prefix. A host matches itself and
//! its subdomains; a path prefix matches on segment boundaries, so
//! `github.com/anthropics` is not `github.com/anthropics-evil`.

use crate::canonical::Canonical;

const APPROVED: &[&str] = &[
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
    "docs.rs",
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

/// Whether this URL is public documentation the tool may read unasked.
pub(crate) fn is_documentation(url: &Canonical) -> bool {
    APPROVED
        .iter()
        .any(|entry| covers(entry, url.host(), url.path()))
}

fn covers(entry: &str, host: &str, path: &str) -> bool {
    let (entry_host, prefix) = match entry.split_once('/') {
        Some((entry_host, prefix)) => (entry_host, Some(prefix)),
        None => (entry, None),
    };
    same_site(entry_host, host) && prefix.is_none_or(|prefix| under(path, prefix))
}

fn same_site(entry_host: &str, host: &str) -> bool {
    host == entry_host || host.ends_with(&format!(".{entry_host}"))
}

/// The prefix has lost its leading slash to the split; a segment boundary is
/// what keeps `/anthropics` off `/anthropics-evil`.
fn under(path: &str, prefix: &str) -> bool {
    let prefix = format!("/{prefix}");
    path == prefix || path.starts_with(&format!("{prefix}/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approved(url: &str) -> bool {
        let url = Canonical::parse(url).expect("a valid URL");
        is_documentation(&url)
    }

    #[test]
    fn a_host_entry_matches_any_path_under_it() {
        assert!(approved("https://docs.rs/tokio/latest/tokio/"));
        assert!(approved("https://doc.rust-lang.org/book/"));
        assert!(approved("https://developer.mozilla.org/en-US/docs/Web"));
    }

    #[test]
    fn a_host_entry_matches_its_subdomains() {
        assert!(approved("https://api.docs.rs/x"));
        assert!(approved("https://blog.kotlinlang.org/whatever"));
    }

    #[test]
    fn a_path_prefix_entry_matches_on_segment_boundaries_only() {
        assert!(approved("https://github.com/anthropics"));
        assert!(approved(
            "https://github.com/anthropics/anthropic-sdk-python"
        ));
        assert!(!approved("https://github.com/anthropics-evil/repo"));
        assert!(!approved("https://github.com/other/repo"));
    }

    #[test]
    fn a_host_that_is_not_on_the_list_is_not_documentation() {
        assert!(!approved("https://example.com/"));
        assert!(!approved("https://www.anthropic.com/"));
        assert!(!approved("https://notdocs.rs/x"));
    }
}
