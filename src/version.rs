use std::cmp::Ordering;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedTag {
    prefix: String,
    major: u64,
    minor: u64,
    patch: u64,
    suffix: String,
    prerelease: Option<Prerelease>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Prerelease {
    label: String,
    number: Option<u64>,
    tail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Candidates {
    pub compatible: Option<String>,
    pub latest: Option<String>,
}

impl ParsedTag {
    pub fn parse(tag: &str) -> Option<Self> {
        let (prefix, rest) = if (tag.starts_with('v') || tag.starts_with('V'))
            && tag
                .chars()
                .nth(1)
                .is_some_and(|character| character.is_ascii_digit())
        {
            (tag[..1].to_string(), &tag[1..])
        } else {
            (String::new(), tag)
        };

        let suffix_start = rest.find(['-', '+']);
        let (core, suffix) = match suffix_start {
            Some(index) => (&rest[..index], &rest[index..]),
            None => (rest, ""),
        };

        let mut parts = core.split('.');
        let major = parse_component(parts.next()?)?;
        let minor = match parts.next() {
            Some(component) => parse_component(component)?,
            None => 0,
        };
        let patch = match parts.next() {
            Some(component) => parse_component(component)?,
            None => 0,
        };
        if parts.next().is_some() {
            return None;
        }

        Some(Self {
            prefix,
            major,
            minor,
            patch,
            suffix: suffix.to_string(),
            prerelease: classify_prerelease(suffix),
        })
    }

    fn core(&self) -> (u64, u64, u64) {
        (self.major, self.minor, self.patch)
    }

    fn caret_upper(&self) -> (u64, u64, u64) {
        if self.major > 0 {
            (self.major + 1, 0, 0)
        } else if self.minor > 0 {
            (0, self.minor + 1, 0)
        } else {
            (0, 0, self.patch + 1)
        }
    }

    fn same_channel(&self, other: &Self) -> bool {
        if self.prefix != other.prefix {
            return false;
        }

        match (&self.prerelease, &other.prerelease) {
            (None, None) => self.suffix == other.suffix,
            (Some(current), Some(candidate)) => {
                current.label == candidate.label && current.tail == candidate.tail
            }
            (Some(current), None) => current.tail == other.suffix,
            (None, Some(_)) => false,
        }
    }

    fn cmp_precedence(&self, other: &Self) -> Ordering {
        self.core()
            .cmp(&other.core())
            .then_with(|| match (&self.prerelease, &other.prerelease) {
                (None, None) => Ordering::Equal,
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
                (Some(current), Some(candidate)) => current
                    .label
                    .cmp(&candidate.label)
                    .then_with(|| {
                        current
                            .number
                            .unwrap_or(0)
                            .cmp(&candidate.number.unwrap_or(0))
                    })
                    .then_with(|| current.tail.cmp(&candidate.tail)),
            })
    }

    fn is_prerelease(&self) -> bool {
        self.prerelease.is_some()
    }
}

pub fn select_candidates(current_tag: &str, tags: &[String]) -> Option<Candidates> {
    let current = ParsedTag::parse(current_tag)?;
    let upper = current.caret_upper();
    let mut compatible: Option<(ParsedTag, String)> = None;
    let mut latest: Option<(ParsedTag, String)> = None;

    for tag in tags {
        let Some(candidate) = ParsedTag::parse(tag) else {
            continue;
        };
        if !current.same_channel(&candidate) {
            continue;
        }
        if candidate.cmp_precedence(&current) != Ordering::Greater {
            continue;
        }

        if candidate.core() < upper {
            update_best(&mut compatible, candidate.clone(), tag);
        }
        update_best(&mut latest, candidate, tag);
    }

    Some(Candidates {
        compatible: compatible.map(|(_, tag)| tag),
        latest: latest.map(|(_, tag)| tag),
    })
}

fn update_best(best: &mut Option<(ParsedTag, String)>, candidate: ParsedTag, tag: &str) {
    let should_replace = best.as_ref().map_or(true, |(current, _)| {
        candidate.cmp_precedence(current) == Ordering::Greater
    });

    if should_replace {
        *best = Some((candidate, tag.to_string()));
    }
}

fn parse_component(component: &str) -> Option<u64> {
    if component.is_empty()
        || !component
            .chars()
            .all(|character| character.is_ascii_digit())
    {
        return None;
    }
    component.parse().ok()
}

fn classify_prerelease(suffix: &str) -> Option<Prerelease> {
    let body = suffix.strip_prefix('-')?;
    if body.is_empty() {
        return None;
    }

    let label_len = body
        .chars()
        .take_while(|character| character.is_ascii_alphabetic())
        .count();
    if label_len == 0 {
        return None;
    }

    let label = body[..label_len].to_ascii_lowercase();
    if !is_prerelease_label(&label) {
        return None;
    }

    let rest = &body[label_len..];
    let (number, tail) = parse_prerelease_rest(rest);

    Some(Prerelease {
        label,
        number,
        tail: tail.to_string(),
    })
}

fn parse_prerelease_rest(rest: &str) -> (Option<u64>, &str) {
    if rest.is_empty() {
        return (None, "");
    }

    let digits_part = if rest.starts_with('.') || rest.starts_with('-') {
        &rest[1..]
    } else {
        rest
    };

    let digits_len = digits_part
        .chars()
        .take_while(|character| character.is_ascii_digit())
        .count();

    if digits_len == 0 {
        return (None, rest);
    }

    let number = digits_part[..digits_len].parse().ok();
    let tail = &digits_part[digits_len..];
    (number, tail)
}

fn is_prerelease_label(label: &str) -> bool {
    matches!(
        label,
        "alpha"
            | "a"
            | "beta"
            | "b"
            | "rc"
            | "pre"
            | "preview"
            | "dev"
            | "nightly"
            | "snapshot"
            | "canary"
            | "next"
    )
}

pub fn is_semver_like(tag: &str) -> bool {
    ParsedTag::parse(tag).is_some()
}

#[allow(dead_code)]
pub fn is_prerelease(tag: &str) -> bool {
    ParsedTag::parse(tag).is_some_and(|parsed| parsed.is_prerelease())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidates(current: &str, tags: &[&str]) -> Candidates {
        let owned: Vec<String> = tags.iter().map(|tag| tag.to_string()).collect();
        select_candidates(current, &owned).unwrap()
    }

    #[test]
    fn shows_caret_and_latest_updates() {
        let result = candidates("1.20.0", &["1.20.0", "1.20.1", "1.21.0", "2.0.0", "2.1.0"]);
        assert_eq!(result.compatible.as_deref(), Some("1.21.0"));
        assert_eq!(result.latest.as_deref(), Some("2.1.0"));
    }

    #[test]
    fn shows_patch_update_when_no_newer_minor_exists() {
        let result = candidates("1.20.0", &["1.20.0", "1.20.1"]);
        assert_eq!(result.compatible.as_deref(), Some("1.20.1"));
        assert_eq!(result.latest.as_deref(), Some("1.20.1"));
    }

    #[test]
    fn shows_only_latest_when_caret_has_no_update() {
        let result = candidates("1.20.0", &["1.20.0", "2.0.0"]);
        assert_eq!(result.compatible, None);
        assert_eq!(result.latest.as_deref(), Some("2.0.0"));
    }

    #[test]
    fn follows_npm_caret_semantics_for_zero_versions() {
        let result = candidates("0.2.3", &["0.2.3", "0.2.4", "0.3.0"]);
        assert_eq!(result.compatible.as_deref(), Some("0.2.4"));
        assert_eq!(result.latest.as_deref(), Some("0.3.0"));

        let result = candidates("0.0.3", &["0.0.3", "0.0.4", "0.1.0"]);
        assert_eq!(result.compatible, None);
        assert_eq!(result.latest.as_deref(), Some("0.1.0"));
    }

    #[test]
    fn keeps_suffixes_separate() {
        let result = candidates(
            "1.27.3-alpine",
            &["1.27.3-alpine", "1.27.4-alpine", "1.28.0", "1.28.0-alpine"],
        );
        assert_eq!(result.compatible.as_deref(), Some("1.28.0-alpine"));
        assert_eq!(result.latest.as_deref(), Some("1.28.0-alpine"));

        let result = candidates("1.27.3-alpine", &["1.27.3-alpine", "1.27.4"]);
        assert_eq!(result.compatible, None);
        assert_eq!(result.latest, None);
    }

    #[test]
    fn ignores_prereleases_for_stable_tags() {
        let result = candidates(
            "1.20.0",
            &["1.20.0", "1.20.1", "1.21.0-rc.1", "2.0.0-beta.1"],
        );
        assert_eq!(result.compatible.as_deref(), Some("1.20.1"));
        assert_eq!(result.latest.as_deref(), Some("1.20.1"));
    }

    #[test]
    fn allows_stable_and_prerelease_updates_for_prerelease_tags() {
        let result = candidates(
            "2.0.0-rc.1",
            &["2.0.0-rc.1", "2.0.0-rc.2", "2.0.0", "2.1.0-rc.1"],
        );
        assert_eq!(result.compatible.as_deref(), Some("2.1.0-rc.1"));
        assert_eq!(result.latest.as_deref(), Some("2.1.0-rc.1"));
    }

    #[test]
    fn tracks_shorthand_beta_channel_across_versions() {
        let result = candidates(
            "v1.0.0-b.1",
            &["v0.108.0-b.91", "v1.0.0-b.1", "v1.0.0-b.2", "v1.0.1-b.1"],
        );
        assert_eq!(result.compatible.as_deref(), Some("v1.0.1-b.1"));
        assert_eq!(result.latest.as_deref(), Some("v1.0.1-b.1"));
    }

    #[test]
    fn suggests_same_beta_channel_when_only_a_newer_beta_exists() {
        let result = candidates("v1.0.0-b.1", &["v1.0.0-b.1", "v1.0.0-b.2"]);
        assert_eq!(result.compatible.as_deref(), Some("v1.0.0-b.2"));
        assert_eq!(result.latest.as_deref(), Some("v1.0.0-b.2"));
    }

    #[test]
    fn tracks_rc_channel_across_versions_and_to_stable() {
        let result = candidates(
            "v2.0.0-rc.1",
            &[
                "v2.0.0-rc.1",
                "v2.0.0-rc.2",
                "v2.0.1-rc.1",
                "v2.0.1",
                "v3.0.0-rc.1",
            ],
        );
        assert_eq!(result.compatible.as_deref(), Some("v2.0.1"));
        assert_eq!(result.latest.as_deref(), Some("v3.0.0-rc.1"));
    }

    #[test]
    fn tracks_rc_channel_when_only_a_newer_rc_exists() {
        let result = candidates("v2.0.0-rc.1", &["v2.0.0-rc.1", "v2.0.0-rc.2"]);
        assert_eq!(result.compatible.as_deref(), Some("v2.0.0-rc.2"));
        assert_eq!(result.latest.as_deref(), Some("v2.0.0-rc.2"));
    }

    #[test]
    fn zero_fills_partial_versions() {
        let result = candidates("1.20", &["1.20", "1.21", "1.21.5"]);
        assert_eq!(result.compatible.as_deref(), Some("1.21.5"));
        assert_eq!(result.latest.as_deref(), Some("1.21.5"));
    }

    #[test]
    fn preserves_v_prefix() {
        let result = candidates("v1.20.0", &["1.21.0", "v1.21.0", "v2.0.0"]);
        assert_eq!(result.compatible.as_deref(), Some("v1.21.0"));
        assert_eq!(result.latest.as_deref(), Some("v2.0.0"));
    }

    #[test]
    fn treats_bookworm_as_a_flavor_not_a_prerelease() {
        let result = candidates(
            "1.27.3-bookworm",
            &["1.27.3-bookworm", "1.27.4-bookworm", "1.27.4-alpine"],
        );
        assert_eq!(result.compatible.as_deref(), Some("1.27.4-bookworm"));
        assert_eq!(result.latest.as_deref(), Some("1.27.4-bookworm"));
    }

    #[test]
    fn rejects_non_semver_tags() {
        assert!(ParsedTag::parse("latest").is_none());
        assert!(ParsedTag::parse("stable-alpine").is_none());
        assert!(ParsedTag::parse("1.2.3.4").is_none());
        assert!(select_candidates("latest", &["latest".to_string()]).is_none());
    }
}
