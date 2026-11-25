//! Runs `rule_vectors.tsv`, the table every backend must agree with.

use istmo_webview::rules::CompiledRules;
use istmo_webview::{NavigationRule, NavigationRules, RuleAction, UrlPattern};

#[test]
fn shared_rule_vectors() {
    let table = include_str!("rule_vectors.tsv");
    let mut checked = 0;
    for (line_no, line) in table.lines().enumerate() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        let [scheme, host, port, path, url, expect] = cols[..] else {
            panic!("line {}: expected 6 tab-separated columns", line_no + 1);
        };
        let dash = |s: &str| {
            if s == "-" {
                String::new()
            } else {
                s.to_owned()
            }
        };
        let rules = CompiledRules::new(&NavigationRules {
            default_action: RuleAction::Deny,
            rules: vec![NavigationRule {
                pattern: UrlPattern {
                    scheme: scheme.to_owned(),
                    host: host.to_owned(),
                    port: (port != "-").then(|| port.parse().unwrap()),
                    path_prefix: dash(path),
                },
                action: RuleAction::Allow,
            }],
        })
        .unwrap_or_else(|err| panic!("line {}: {err}", line_no + 1));
        let got = rules.evaluate(url, false).action;
        let want = match expect {
            "match" => RuleAction::Allow,
            "nomatch" => RuleAction::Deny,
            other => panic!("line {}: unknown expectation {other}", line_no + 1),
        };
        assert_eq!(
            got,
            want,
            "line {}: {url} against {scheme}://{host}{path}",
            line_no + 1
        );
        checked += 1;
    }
    assert!(checked > 30);
}

#[test]
fn shared_origin_vectors() {
    let table = include_str!("origin_vectors.tsv");
    let mut checked = 0;
    for (line_no, line) in table.lines().enumerate() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let Some((url, want)) = line.split_once('\t') else {
            panic!("line {}: expected 2 tab-separated columns", line_no + 1);
        };
        let got = url::Url::parse(url)
            .ok()
            .as_ref()
            .and_then(istmo_webview::rules::origin_of);
        let want = (want != "-").then(|| want.to_owned());
        assert_eq!(got, want, "line {}: {url}", line_no + 1);
        checked += 1;
    }
    assert!(checked > 10);
}
