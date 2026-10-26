use std::collections::HashSet;

use anyhow::{anyhow, bail, Context, Result};
use regex::Regex;
use serde::Deserialize;

use crate::{
    db::{MODE_DIGEST, MODE_LATEST, MODE_SEMVER},
    reference::ImageRef,
    version::{
        display_version, display_version_with_regex, is_semver_like, parse_version_with_regex,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateMode {
    Semver,
    Latest,
    Digest,
}

impl UpdateMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Semver => MODE_SEMVER,
            Self::Latest => MODE_LATEST,
            Self::Digest => MODE_DIGEST,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ImageSpec {
    pub image: ImageRef,
    pub version_regex: Option<Regex>,
    pub mode: UpdateMode,
    pub parsed_version: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawImageFile {
    #[serde(default)]
    images: Vec<RawImage>,
}

#[derive(Debug, Deserialize)]
struct RawImage {
    image: String,
    #[serde(default)]
    version_regex: Option<String>,
    #[serde(default)]
    mode: Option<UpdateMode>,
}

pub fn parse_image_file(input: &str) -> Result<Vec<ImageSpec>> {
    let file: RawImageFile = toml::from_str(input).context("failed to parse image file as TOML")?;
    let mut seen = HashSet::new();
    let mut images = Vec::new();

    for raw in file.images {
        let image = ImageRef::parse(&raw.image)
            .with_context(|| format!("invalid image reference {}", raw.image))?;
        let version_regex = raw
            .version_regex
            .map(|pattern| {
                Regex::new(&pattern)
                    .with_context(|| format!("invalid version_regex for {}", image.canonical()))
            })
            .transpose()?;

        let mode = raw.mode.unwrap_or_else(|| {
            if version_regex.is_some() || is_semver_like(&image.tag) {
                UpdateMode::Semver
            } else {
                UpdateMode::Digest
            }
        });

        if mode == UpdateMode::Digest && version_regex.is_some() {
            bail!(
                "{}: version_regex is not used with mode = \"digest\"",
                image.canonical()
            );
        }

        let parsed_version = match mode {
            UpdateMode::Digest => None,
            UpdateMode::Semver | UpdateMode::Latest => {
                if let Some(regex) = &version_regex {
                    if parse_version_with_regex(regex, &image.tag).is_none() {
                        bail!(
                            "{}: version_regex did not capture a parsable version from tag {}",
                            image.canonical(),
                            image.tag
                        );
                    }
                    display_version_with_regex(regex, &image.tag)
                } else {
                    Some(display_version(&image.tag).ok_or_else(|| {
                        anyhow!(
                            "{}: tag {} is not semver-like; set version_regex or mode = \"digest\"",
                            image.canonical(),
                            image.tag
                        )
                    })?)
                }
            }
        };

        if !seen.insert(image.canonical()) {
            bail!("duplicate image reference {}", image.canonical());
        }

        images.push(ImageSpec {
            image,
            version_regex,
            mode,
            parsed_version,
        });
    }

    Ok(images)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_toml_entries_and_modes() {
        let images = parse_image_file(
            r#"
            [[images]]
            image = "nginx:1.27.3"

            [[images]]
            image = "registry.yziadi.fyi/icanfixit:latest"
            mode = "digest"
            "#,
        )
        .unwrap();

        assert_eq!(images.len(), 2);
        assert_eq!(images[0].mode, UpdateMode::Semver);
        assert_eq!(images[0].parsed_version.as_deref(), Some("1.27.3"));
        assert_eq!(images[1].mode, UpdateMode::Digest);
        assert_eq!(images[1].parsed_version, None);
    }

    #[test]
    fn parses_custom_regex_for_home_assistant() {
        let images = parse_image_file(
            r#"
            [[images]]
            image = "homeassistant/home-assistant:2026.7.0.dev202605290320"
            version_regex = '^(?P<version>\d{4}\.\d+\.\d+)'
            mode = "latest"
            "#,
        )
        .unwrap();

        assert_eq!(images[0].mode, UpdateMode::Latest);
        assert_eq!(images[0].parsed_version.as_deref(), Some("2026.7.0"));
    }

    #[test]
    fn rejects_duplicates() {
        let error = parse_image_file(
            r#"
            [[images]]
            image = "nginx:1.27.3"

            [[images]]
            image = "docker.io/library/nginx:1.27.3"
            "#,
        )
        .unwrap_err();

        assert!(format!("{error:#}").contains("duplicate"));
    }

    #[test]
    fn rejects_invalid_regex() {
        let error = parse_image_file(
            r#"
            [[images]]
            image = "nginx:1.27.3"
            version_regex = '('
            "#,
        )
        .unwrap_err();

        assert!(format!("{error:#}").contains("invalid version_regex"));
    }

    #[test]
    fn rejects_semver_mode_without_a_parsable_tag() {
        let error = parse_image_file(
            r#"
            [[images]]
            image = "nginx:latest"
            mode = "semver"
            "#,
        )
        .unwrap_err();

        assert!(format!("{error:#}").contains("not semver-like"));
    }

    #[test]
    fn rejects_digest_mode_with_regex() {
        let error = parse_image_file(
            r#"
            [[images]]
            image = "nginx:latest"
            version_regex = '^(?P<version>\d+)'
            mode = "digest"
            "#,
        )
        .unwrap_err();

        assert!(format!("{error:#}").contains("not used with mode"));
    }
}
