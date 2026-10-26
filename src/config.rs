use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

#[derive(Clone, Deserialize)]
pub struct Config {
    pub images_file: PathBuf,
    pub database: PathBuf,
    pub output_dir: PathBuf,
    #[serde(default)]
    pub check: CheckConfig,
    #[serde(default)]
    pub registries: HashMap<String, RegistryConfig>,
}

#[derive(Clone, Deserialize)]
pub struct CheckConfig {
    #[serde(default = "default_concurrency")]
    pub concurrency: usize,
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
}

impl Default for CheckConfig {
    fn default() -> Self {
        Self {
            concurrency: default_concurrency(),
            timeout_secs: default_timeout_secs(),
        }
    }
}

#[derive(Clone, Default, Deserialize)]
pub struct RegistryConfig {
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub password_file: Option<PathBuf>,
}

#[derive(Clone)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let path = fs::canonicalize(path)
            .with_context(|| format!("failed to resolve config file {}", path.display()))?;
        let contents = fs::read_to_string(&path)
            .with_context(|| format!("failed to read config file {}", path.display()))?;
        let mut config: Self = toml::from_str(&contents)
            .with_context(|| format!("failed to parse config file {}", path.display()))?;

        let base = path.parent().unwrap_or_else(|| Path::new("."));
        config.images_file = resolve_path(base, config.images_file);
        config.database = resolve_path(base, config.database);
        config.output_dir = resolve_path(base, config.output_dir);
        for registry in config.registries.values_mut() {
            if let Some(password_file) = registry.password_file.take() {
                registry.password_file = Some(resolve_path(base, password_file));
            }
        }

        let registries = config
            .registries
            .into_iter()
            .map(|(host, registry)| (normalize_host(&host), registry))
            .collect();
        config.registries = registries;

        config.validate()?;
        Ok(config)
    }

    pub fn credentials_for(&self, host: &str) -> Result<Option<Credentials>> {
        let Some(registry) = self.registries.get(&normalize_host(host)) else {
            return Ok(None);
        };

        let password = match (&registry.password, &registry.password_file) {
            (Some(password), None) => password.clone(),
            (None, Some(path)) => {
                let password = fs::read_to_string(path)
                    .with_context(|| format!("failed to read password file {}", path.display()))?;
                password.trim_end_matches(['\r', '\n']).to_string()
            }
            (None, None) => return Ok(None),
            (Some(_), Some(_)) => {
                bail!(
                    "registry {} has both password and password_file configured",
                    host
                )
            }
        };

        let Some(username) = &registry.username else {
            bail!("registry {} has a password but no username", host);
        };

        Ok(Some(Credentials {
            username: username.clone(),
            password,
        }))
    }

    fn validate(&self) -> Result<()> {
        if self.check.concurrency == 0 {
            bail!("check.concurrency must be greater than zero");
        }
        if self.check.timeout_secs == 0 {
            bail!("check.timeout_secs must be greater than zero");
        }
        if !self.images_file.is_absolute() {
            bail!("images_file must be an absolute path after resolution");
        }
        if !self.database.is_absolute() {
            bail!("database must be an absolute path after resolution");
        }
        if !self.output_dir.is_absolute() {
            bail!("output_dir must be an absolute path after resolution");
        }

        for (host, registry) in &self.registries {
            if host.is_empty() || host.contains('/') || host.chars().any(char::is_whitespace) {
                bail!("invalid registry host in config: {host}");
            }
            if registry.password.is_some() && registry.password_file.is_some() {
                bail!(
                    "registry {} has both password and password_file configured",
                    host
                );
            }
            if registry.username.is_none()
                && (registry.password.is_some() || registry.password_file.is_some())
            {
                bail!("registry {} has a password but no username", host);
            }
        }

        Ok(())
    }
}

fn resolve_path(base: &Path, path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        base.join(path)
    }
}

fn normalize_host(host: &str) -> String {
    let host = host.trim().to_ascii_lowercase();
    match host.as_str() {
        "index.docker.io" | "registry-1.docker.io" => "docker.io".to_string(),
        _ => host,
    }
}

fn default_concurrency() -> usize {
    4
}

fn default_timeout_secs() -> u64 {
    30
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_paths_relative_to_the_config_file() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        fs::write(
            &config_path,
            r#"
            images_file = "images.txt"
            database = "state/hublot.db"
            output_dir = "site"

            [check]
            concurrency = 2
            timeout_secs = 15

            [registries."index.docker.io"]
            username = "me"
            password = "secret"
            "#,
        )
        .unwrap();

        let config = Config::load(&config_path).unwrap();
        assert_eq!(config.images_file, dir.path().join("images.txt"));
        assert_eq!(config.database, dir.path().join("state/hublot.db"));
        assert_eq!(config.output_dir, dir.path().join("site"));
        assert!(config.registries.contains_key("docker.io"));
    }

    #[test]
    fn reads_credentials_from_files() {
        let dir = tempfile::tempdir().unwrap();
        let secret = dir.path().join("token");
        fs::write(&secret, "top-secret\n").unwrap();
        let config_path = dir.path().join("config.toml");
        fs::write(
            &config_path,
            format!(
                r#"
                images_file = "images.txt"
                database = "hublot.db"
                output_dir = "site"

                [registries."ghcr.io"]
                username = "me"
                password_file = "{}"
                "#,
                secret.display()
            ),
        )
        .unwrap();

        let config = Config::load(&config_path).unwrap();
        let credentials = config.credentials_for("ghcr.io").unwrap().unwrap();
        assert_eq!(credentials.username, "me");
        assert_eq!(credentials.password, "top-secret");
        assert!(config.credentials_for("quay.io").unwrap().is_none());
    }

    #[test]
    fn rejects_ambiguous_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        fs::write(
            &config_path,
            r#"
            images_file = "images.txt"
            database = "hublot.db"
            output_dir = "site"

            [registries."ghcr.io"]
            username = "me"
            password = "one"
            password_file = "two"
            "#,
        )
        .unwrap();

        assert!(Config::load(&config_path).is_err());
    }

    #[test]
    fn empty_registry_config_is_anonymous() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        fs::write(
            &config_path,
            r#"
            images_file = "images.txt"
            database = "hublot.db"
            output_dir = "site"

            [registries."registry.yziadi.fyi"]
            "#,
        )
        .unwrap();

        let config = Config::load(&config_path).unwrap();
        assert!(config
            .credentials_for("registry.yziadi.fyi")
            .unwrap()
            .is_none());
    }
}
