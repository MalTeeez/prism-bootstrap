//! Rule evaluation - `allowed(rules, ctx)` with Mojang semantics.
//!
//! This is the gate every library, native, and modern-arg entry passes through
//! during resolve and assembly. It accepts both rule dialects against a
//! single [`Ctx`]:
//! the MMC arch-in-name token (`os.name == "osx-arm64"`) and the classic Mojang
//! `name`/`arch`/`version`-regex form, plus the `features` gate.

use log::warn;
use regex::Regex;

use crate::model::patch::{Os, Rule};
use crate::platform::Ctx;

/// Decide whether a rule list permits `ctx`.
///
/// Empty/absent rules allow by default. Otherwise every rule is evaluated in
/// order and the last applicable one decides - so an `allow` followed by a
/// more specific `disallow` correctly excludes.
#[must_use]
pub fn allowed(rules: &[Rule], ctx: &Ctx) -> bool {
    if rules.is_empty() {
        return true;
    }

    let mut decision = false;
    for rule in rules {
        if rule_applies(rule, ctx) {
            decision = rule.action == "allow";
        }
    }
    decision
}

/// Whether a single rule's predicates all match `ctx`. A rule with no `os` and
/// no `features` applies unconditionally.
fn rule_applies(rule: &Rule, ctx: &Ctx) -> bool {
    let os_ok = rule.os.as_ref().is_none_or(|os| os_matches(os, ctx));
    let features_ok = rule
        .features
        .as_ref()
        .is_none_or(|features| features.iter().all(|(name, want)| ctx.feature(name) == *want));
    os_ok && features_ok
}

/// Match an `os` predicate in either dialect. Each present field must match;
/// `name` matches the MMC token *or* the classic os name.
fn os_matches(os: &Os, ctx: &Ctx) -> bool {
    let name_ok = os
        .name
        .as_ref()
        .is_none_or(|name| name == &ctx.os_token || name == &ctx.os_name);
    let arch_ok = os.arch.as_ref().is_none_or(|arch| arch == &ctx.arch);
    let version_ok = os
        .version
        .as_ref()
        .is_none_or(|version| version_matches(version, &ctx.version));
    name_ok && arch_ok && version_ok
}

/// Match a classic `os.version` regex against the ctx version. A malformed
/// pattern warns and is treated as non-matching (conservative).
fn version_matches(pattern: &str, version: &str) -> bool {
    match Regex::new(pattern) {
        Ok(regex) => regex.is_match(version),
        Err(error) => {
            warn!("ignoring malformed os.version regex {pattern:?}: {error}");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::patch::Rule;
    use crate::platform::{Platform, expand_platform};

    /// Parse a rule list from inline JSON (test convenience).
    fn rules(json: &str) -> Vec<Rule> {
        serde_json::from_str(json).expect("inline rule fixture should parse")
    }

    #[test]
    fn empty_rules_allow() {
        let ctx = expand_platform(Platform::Linux);
        assert!(allowed(&[], &ctx));
    }

    #[test]
    fn single_allow_and_disallow() {
        let ctx = expand_platform(Platform::Linux);
        assert!(allowed(&rules(r#"[ { "action": "allow" } ]"#), &ctx));
        assert!(!allowed(&rules(r#"[ { "action": "disallow" } ]"#), &ctx));
    }

    #[test]
    fn allow_then_disallow_excludes_linux_but_not_windows() {
        // The "twitch on linux" case: allow all, then disallow
        // linux - last applicable rule wins.
        let twitch = rules(
            r#"[ { "action": "allow" },
                 { "action": "disallow", "os": { "name": "linux" } } ]"#,
        );
        assert!(!allowed(&twitch, &expand_platform(Platform::Linux)));
        assert!(allowed(&twitch, &expand_platform(Platform::WindowsX86)));
        assert!(allowed(&twitch, &expand_platform(Platform::OsxArm64)));
    }

    #[test]
    fn mmc_token_os_name_matches() {
        // A rule keyed on the arch-in-name token resolves directly.
        let rule = rules(r#"[ { "action": "allow", "os": { "name": "osx-arm64" } } ]"#);
        assert!(allowed(&rule, &expand_platform(Platform::OsxArm64)));
        assert!(!allowed(&rule, &expand_platform(Platform::Osx)));
        assert!(!allowed(&rule, &expand_platform(Platform::Linux)));
    }

    #[test]
    fn classic_os_name_matches_both_arches_of_that_os() {
        // Classic `name: osx` (no arch) matches both osx and osx-arm64 targets.
        let rule = rules(r#"[ { "action": "allow", "os": { "name": "osx" } } ]"#);
        assert!(allowed(&rule, &expand_platform(Platform::Osx)));
        assert!(allowed(&rule, &expand_platform(Platform::OsxArm64)));
        assert!(!allowed(&rule, &expand_platform(Platform::Windows)));
    }

    #[test]
    fn classic_name_and_arch_must_both_match() {
        let rule = rules(
            r#"[ { "action": "allow", "os": { "name": "osx", "arch": "arm64" } } ]"#,
        );
        assert!(allowed(&rule, &expand_platform(Platform::OsxArm64)));
        // Right OS, wrong arch -> excluded.
        assert!(!allowed(&rule, &expand_platform(Platform::Osx)));
    }

    #[test]
    fn arch_mismatch_excludes() {
        // name matches linux, but arm64 arch does not match the x86_64 target.
        let rule = rules(
            r#"[ { "action": "allow", "os": { "name": "linux", "arch": "arm64" } } ]"#,
        );
        assert!(!allowed(&rule, &expand_platform(Platform::Linux)));
        assert!(allowed(&rule, &expand_platform(Platform::LinuxArm64)));
    }

    #[test]
    fn features_gate_defaults_false() {
        // A demo-gated arg: allowed only when the feature is set; default-false
        // ctx excludes it.
        let demo = rules(
            r#"[ { "action": "allow", "features": { "is_demo_user": true } } ]"#,
        );
        let mut ctx = expand_platform(Platform::Linux);
        assert!(!allowed(&demo, &ctx));

        ctx.features.insert("is_demo_user".to_owned(), true);
        assert!(allowed(&demo, &ctx));
    }

    #[test]
    fn version_regex_matches_modern_default() {
        // A windows-10 gate matches the modern default version, an old-osx gate
        // does not match osx's modern default.
        let win10 = rules(
            r#"[ { "action": "allow", "os": { "name": "windows", "version": "^10\\." } } ]"#,
        );
        assert!(allowed(&win10, &expand_platform(Platform::Windows)));

        let old_osx = rules(
            r#"[ { "action": "allow", "os": { "name": "osx", "version": "^10\\.5\\." } } ]"#,
        );
        assert!(!allowed(&old_osx, &expand_platform(Platform::Osx)));
    }
}
