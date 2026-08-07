//! WebFetch 预批准域名（代码类公开文档域名）。
//! 仅影响 WebFetch 权限判定（GET 请求），不涉及网络沙箱。

const PREAPPROVED: &[&str] = &[
    "modelcontextprotocol.io",
    "github.com/anthropics",
    "agentskills.io",
    // 编程语言文档
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
    // Web 与 JS 框架
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
    // Python 生态
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
    // 移动
    "reactnative.dev",
    "docs.flutter.dev",
    "developer.apple.com",
    "developer.android.com",
    // 数据与 ML
    "keras.io",
    "spark.apache.org",
    "huggingface.co",
    "www.kaggle.com",
    // 数据库
    "www.mongodb.com",
    "redis.io",
    "www.postgresql.org",
    "dev.mysql.com",
    "www.sqlite.org",
    "graphql.org",
    "prisma.io",
    // 云与 DevOps
    "docs.aws.amazon.com",
    "cloud.google.com",
    "kubernetes.io",
    "www.docker.com",
    "www.terraform.io",
    "www.ansible.com",
    "vercel.com/docs",
    "docs.netlify.com",
    "devcenter.heroku.com",
    // 测试与监控
    "cypress.io",
    "selenium.dev",
    // 游戏
    "docs.unity.com",
    "docs.unrealengine.com",
    // 其他
    "git-scm.com",
    "nginx.org",
    "httpd.apache.org",
];

/// URL 是否落在预批准列表。路径前缀条目要求段边界匹配
///（"/anthropics" 不匹配 "/anthropics-evil"）。
pub fn is_preapproved_url(url: &str) -> bool {
    let Ok(parsed) = url::Url::parse(url) else {
        return false;
    };
    let Some(hostname) = parsed.host_str() else {
        return false;
    };
    let pathname = parsed.path();
    for entry in PREAPPROVED.iter().copied() {
        let (host, prefix) = match entry.split_once('/') {
            Some((h, p)) => (h, Some(p.to_string())),
            None => (entry, None),
        };
        if hostname != host {
            continue;
        }
        let matched = match prefix {
            None => true,
            // entry 的路径段（"github.com/anthropics"）在 split 后无前导斜杠，比对时补上。
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
}
