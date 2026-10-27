use crate::{config::Config, transport::Route};

pub fn matches_domain(host: &str, domain: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    host == domain
        || host
            .strip_suffix(domain)
            .is_some_and(|prefix| prefix.ends_with('.'))
}

pub fn generate(cfg: &Config, route: &Route) -> String {
    let domains = serde_json::to_string(&cfg.domains).expect("string serialization cannot fail");
    let fallback =
        serde_json::to_string(&route.pac_fallback()).expect("string serialization cannot fail");
    format!(
        r#"function FindProxyForURL(url, host) {{
    host = host.toLowerCase().replace(/\.$/, "");
    var domains = {domains};
    for (var i = 0; i < domains.length; i++) {{
        var d = domains[i];
        if (host === d || (host.length > d.length && host.slice(-(d.length + 1)) === "." + d)) {{
            return "PROXY 127.0.0.1:{port}";
        }}
    }}
    return {fallback};
}}
"#,
        port = cfg.port
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn suffix_requires_a_label_boundary() {
        for host in ["claude.ai", "api.claude.ai", "CLAUDE.AI."] {
            assert!(matches_domain(host, "claude.ai"));
        }
        for host in [
            "evilclaude.ai",
            "claude.ai.evil.com",
            "claude.aiattacker.com",
        ] {
            assert!(!matches_domain(host, "claude.ai"));
        }
    }
    #[test]
    fn protected_rule_has_no_direct_fallback() {
        let pac = generate(
            &Config::default(),
            &Route::Http("127.0.0.1:7890".parse().unwrap()),
        );
        assert!(pac.contains("return \"PROXY 127.0.0.1:8888\";"));
        assert!(!pac.contains("; DIRECT"));
        assert!(!pac.contains("dnsDomainIs"));
    }
}
