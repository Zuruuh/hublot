use std::collections::HashSet;

use anyhow::{bail, Context, Result};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ImageRef {
    pub registry: String,
    pub repository: String,
    pub tag: String,
}

impl ImageRef {
    pub fn parse(input: &str) -> Result<Self> {
        let input = input.trim();
        if input.is_empty() {
            bail!("image reference is empty");
        }
        if input.contains("://") {
            bail!("image reference must not include a URL scheme");
        }
        if input.contains('@') {
            bail!("digest-pinned references are not supported; use a tag");
        }

        let (name, tag) = split_tag(input)?;
        if name.is_empty() {
            bail!("image name is empty");
        }
        if tag.is_empty() {
            bail!("image tag is empty");
        }
        validate_tag(&tag)?;

        let (registry, repository) = split_registry(&name);
        validate_registry(&registry)?;
        let registry = normalize_registry(&registry);
        let repository = normalize_repository(&registry, &repository)?;

        Ok(Self {
            registry,
            repository,
            tag,
        })
    }

    pub fn canonical(&self) -> String {
        format!("{}/{}:{}", self.registry, self.repository, self.tag)
    }

    pub fn api_host(&self) -> &str {
        if self.registry == "docker.io" {
            "registry-1.docker.io"
        } else {
            &self.registry
        }
    }

    pub fn base_url(&self) -> String {
        format!("https://{}", self.api_host())
    }
}

pub fn parse_image_file(input: &str) -> Result<Vec<ImageRef>> {
    let mut seen = HashSet::new();
    let mut images = Vec::new();

    for (index, line) in input.lines().enumerate() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }

        let image = ImageRef::parse(line)
            .with_context(|| format!("invalid image reference on line {}", index + 1))?;
        if seen.insert(image.canonical()) {
            images.push(image);
        }
    }

    Ok(images)
}

fn split_tag(input: &str) -> Result<(String, String)> {
    let last_slash = input.rfind('/');
    let last_colon = input.rfind(':');

    let Some(colon) = last_colon else {
        return Ok((input.to_string(), "latest".to_string()));
    };

    if last_slash.is_some_and(|slash| colon < slash) {
        return Ok((input.to_string(), "latest".to_string()));
    }

    let name = &input[..colon];
    let tag = &input[colon + 1..];
    if name.is_empty() {
        bail!("image name is empty");
    }
    if tag.contains('/') {
        bail!("image tag must not contain '/'");
    }

    Ok((name.to_string(), tag.to_string()))
}

fn split_registry(name: &str) -> (String, String) {
    let Some((first, rest)) = name.split_once('/') else {
        return ("docker.io".to_string(), name.to_string());
    };

    let looks_like_registry = first.contains('.')
        || first.contains(':')
        || first.eq_ignore_ascii_case("localhost")
        || first.starts_with('[');

    if looks_like_registry {
        (first.to_string(), rest.to_string())
    } else {
        ("docker.io".to_string(), name.to_string())
    }
}

fn validate_registry(registry: &str) -> Result<()> {
    if registry.is_empty() {
        bail!("registry host is empty");
    }
    if registry.chars().any(char::is_whitespace) || registry.contains('/') {
        bail!("registry host is invalid");
    }
    if registry.starts_with('[') {
        return Ok(());
    }
    if let Some((host, port)) = registry.rsplit_once(':') {
        if host.is_empty() {
            bail!("registry host is empty");
        }
        if !port.chars().all(|character| character.is_ascii_digit()) {
            bail!("registry port is invalid");
        }
        let port: u16 = port
            .parse()
            .map_err(|_| anyhow::anyhow!("registry port is invalid"))?;
        if port == 0 {
            bail!("registry port must be greater than zero");
        }
    }
    Ok(())
}

fn normalize_registry(registry: &str) -> String {
    let registry = registry.trim().to_ascii_lowercase();
    match registry.as_str() {
        "index.docker.io" | "registry-1.docker.io" => "docker.io".to_string(),
        _ => registry,
    }
}

fn normalize_repository(registry: &str, repository: &str) -> Result<String> {
    if registry.is_empty() {
        bail!("registry host is empty");
    }
    if registry.contains('/') || registry.chars().any(char::is_whitespace) {
        bail!("registry host is invalid");
    }

    let mut repository = repository.trim().to_string();
    if repository.is_empty() {
        bail!("image repository is empty");
    }
    if repository.contains(':') || repository.contains('@') {
        bail!("image repository is invalid");
    }
    if repository.contains(char::is_whitespace) {
        bail!("image repository must not contain whitespace");
    }
    for component in repository.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            bail!("image repository contains an invalid path component");
        }
    }

    if registry == "docker.io" && !repository.contains('/') {
        repository = format!("library/{repository}");
    }

    Ok(repository)
}

fn validate_tag(tag: &str) -> Result<()> {
    if tag.len() > 128 {
        bail!("image tag is longer than 128 characters");
    }
    if tag.chars().any(char::is_whitespace) {
        bail!("image tag must not contain whitespace");
    }

    let mut chars = tag.chars();
    let Some(first) = chars.next() else {
        bail!("image tag is empty");
    };
    if !(first.is_ascii_alphanumeric() || first == '_') {
        bail!("image tag has an invalid first character");
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-')) {
        bail!("image tag contains invalid characters");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(input: &str) -> ImageRef {
        ImageRef::parse(input).unwrap()
    }

    #[test]
    fn defaults_to_docker_hub_latest() {
        let image = parsed("nginx");
        assert_eq!(image.registry, "docker.io");
        assert_eq!(image.repository, "library/nginx");
        assert_eq!(image.tag, "latest");
        assert_eq!(image.canonical(), "docker.io/library/nginx:latest");
    }

    #[test]
    fn keeps_docker_hub_namespaces() {
        let image = parsed("zuruuh/app:1.2.3");
        assert_eq!(image.registry, "docker.io");
        assert_eq!(image.repository, "zuruuh/app");
        assert_eq!(image.tag, "1.2.3");
    }

    #[test]
    fn recognizes_explicit_registries() {
        let image = parsed("ghcr.io/zuruuh/app:v1.2.3");
        assert_eq!(image.registry, "ghcr.io");
        assert_eq!(image.repository, "zuruuh/app");
        assert_eq!(image.tag, "v1.2.3");

        let image = parsed("registry.yziadi.fyi/team/tool");
        assert_eq!(image.registry, "registry.yziadi.fyi");
        assert_eq!(image.repository, "team/tool");
        assert_eq!(image.tag, "latest");
    }

    #[test]
    fn handles_registry_ports() {
        let image = parsed("localhost:5000/team/tool:edge");
        assert_eq!(image.registry, "localhost:5000");
        assert_eq!(image.repository, "team/tool");
        assert_eq!(image.tag, "edge");
    }

    #[test]
    fn normalizes_docker_hub_aliases() {
        let image = parsed("index.docker.io/library/nginx:1.27");
        assert_eq!(image.registry, "docker.io");
        assert_eq!(image.repository, "library/nginx");
        assert_eq!(image.tag, "1.27");
        assert_eq!(parsed("registry-1.docker.io/library/nginx:1.27"), image);
    }

    #[test]
    fn parses_comments_and_deduplicates() {
        let images = parse_image_file(
            r#"
            # a comment
            nginx:1.27
            nginx:1.27 # duplicate
            ghcr.io/zuruuh/app:v1
            "#,
        )
        .unwrap();

        assert_eq!(images.len(), 2);
        assert_eq!(images[0].canonical(), "docker.io/library/nginx:1.27");
        assert_eq!(images[1].canonical(), "ghcr.io/zuruuh/app:v1");
    }

    #[test]
    fn rejects_digest_references() {
        assert!(ImageRef::parse("nginx@sha256:abcd").is_err());
    }

    #[test]
    fn rejects_invalid_tags() {
        assert!(ImageRef::parse("nginx:").is_err());
        assert!(ImageRef::parse("nginx:bad/tag").is_err());
        assert!(ImageRef::parse("nginx:-bad").is_err());
    }
}
