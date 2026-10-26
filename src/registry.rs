use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

use anyhow::{anyhow, bail, Context, Result};
use reqwest::{
    header::{ACCEPT, LINK, WWW_AUTHENTICATE},
    Method, Response, StatusCode,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use url::Url;

use crate::{
    config::{Config, Credentials},
    reference::ImageRef,
};

const MANIFEST_ACCEPT: &str = concat!(
    "application/vnd.oci.image.index.v1+json, ",
    "application/vnd.docker.distribution.manifest.list.v2+json, ",
    "application/vnd.oci.image.manifest.v1+json, ",
    "application/vnd.docker.distribution.manifest.v2+json, ",
    "*/*"
);

const TAGS_ACCEPT: &str = "application/json";

#[derive(Clone)]
struct TokenEntry {
    token: String,
    expires_at: Instant,
}

enum RequestAuth {
    None,
    Bearer(String),
    Basic(String, String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum AuthChallenge {
    Basic,
    Bearer(BearerChallenge),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BearerChallenge {
    realm: String,
    service: Option<String>,
    scope: Option<String>,
}

#[derive(Deserialize)]
struct TokenResponse {
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
}

#[derive(Deserialize)]
struct TagsResponse {
    #[serde(default)]
    tags: Option<Vec<String>>,
}

pub struct RegistryClient {
    http: reqwest::Client,
    credentials: HashMap<String, Credentials>,
    tokens: Mutex<HashMap<String, TokenEntry>>,
}

impl RegistryClient {
    pub fn new(config: &Config) -> Result<Self> {
        let mut credentials = HashMap::new();
        for host in config.registries.keys() {
            if let Some(credentials_for_host) = config.credentials_for(host)? {
                credentials.insert(host.clone(), credentials_for_host);
            }
        }

        let http = reqwest::Client::builder()
            .user_agent(format!("hublot/{}", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(config.check.timeout_secs))
            .build()
            .context("failed to build HTTP client")?;

        Ok(Self {
            http,
            credentials,
            tokens: Mutex::new(HashMap::new()),
        })
    }

    pub async fn manifest_digest(&self, image: &ImageRef) -> Result<String> {
        let scope = format!("repository:{}:pull", image.repository);
        let url = format!(
            "{}/v2/{}/manifests/{}",
            image.base_url(),
            image.repository,
            image.tag
        );

        let response = self
            .request(
                Method::HEAD,
                &url,
                &image.registry,
                &scope,
                Some(MANIFEST_ACCEPT),
            )
            .await?;

        if response.status().is_success() {
            if let Some(digest) = response
                .headers()
                .get("docker-content-digest")
                .and_then(|value| value.to_str().ok())
            {
                return Ok(digest.trim().to_string());
            }
        } else if response.status() != StatusCode::METHOD_NOT_ALLOWED {
            return Err(response_error(response, "manifest request").await);
        }

        let response = self
            .request(
                Method::GET,
                &url,
                &image.registry,
                &scope,
                Some(MANIFEST_ACCEPT),
            )
            .await?;

        if !response.status().is_success() {
            return Err(response_error(response, "manifest request").await);
        }

        let bytes = response
            .bytes()
            .await
            .context("failed to read manifest body")?;
        let digest = format!("sha256:{}", hex::encode(Sha256::digest(&bytes)));
        Ok(digest)
    }

    pub async fn list_tags(&self, image: &ImageRef) -> Result<Vec<String>> {
        let scope = format!("repository:{}:pull", image.repository);
        let base = image.base_url();
        let mut url = format!("{}/v2/{}/tags/list?n=1000", base, image.repository);
        let mut tags = Vec::new();
        let mut pages = 0usize;

        loop {
            if pages >= 100 {
                bail!(
                    "registry returned too many tag-list pages for {}",
                    image.canonical()
                );
            }

            let response = self
                .request(
                    Method::GET,
                    &url,
                    &image.registry,
                    &scope,
                    Some(TAGS_ACCEPT),
                )
                .await?;

            if !response.status().is_success() {
                return Err(response_error(response, "tag-list request").await);
            }

            let next = response
                .headers()
                .get(LINK)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string);
            let body: TagsResponse = response
                .json()
                .await
                .context("failed to parse tag-list response")?;

            if let Some(page_tags) = body.tags {
                tags.extend(page_tags);
            }

            let Some(link) = next else {
                break;
            };
            let Some(next_url) = next_link(&link, &base)? else {
                break;
            };
            url = next_url;
            pages += 1;
        }

        tags.sort();
        tags.dedup();
        Ok(tags)
    }

    async fn request(
        &self,
        method: Method,
        url: &str,
        registry: &str,
        scope: &str,
        accept: Option<&str>,
    ) -> Result<Response> {
        let mut auth = self
            .cached_token(registry, scope)
            .map(RequestAuth::Bearer)
            .unwrap_or(RequestAuth::None);
        let mut retried = false;

        loop {
            let mut request = self.http.request(method.clone(), url);
            if let Some(accept) = accept {
                request = request.header(ACCEPT, accept);
            }
            request = match &auth {
                RequestAuth::None => request,
                RequestAuth::Bearer(token) => request.bearer_auth(token),
                RequestAuth::Basic(username, password) => {
                    request.basic_auth(username, Some(password))
                }
            };

            let response = request
                .send()
                .await
                .with_context(|| format!("request to {url} failed"))?;

            if response.status() != StatusCode::UNAUTHORIZED || retried {
                return Ok(response);
            }
            retried = true;

            let challenge = auth_challenge(&response).ok_or_else(|| {
                anyhow!(
                    "registry {} returned 401 without a supported authentication challenge",
                    registry
                )
            })?;

            match challenge {
                AuthChallenge::Basic => {
                    let credentials = self.credentials.get(registry).ok_or_else(|| {
                        anyhow!(
                            "registry {} requires basic authentication but no credentials are configured",
                            registry
                        )
                    })?;
                    auth = RequestAuth::Basic(
                        credentials.username.clone(),
                        credentials.password.clone(),
                    );
                }
                AuthChallenge::Bearer(challenge) => {
                    let effective_scope =
                        challenge.scope.clone().unwrap_or_else(|| scope.to_string());
                    let token = self
                        .fetch_token(&challenge, registry, &effective_scope)
                        .await?;
                    if effective_scope != scope {
                        self.store_token(registry, scope, &token);
                    }
                    auth = RequestAuth::Bearer(token);
                }
            }
        }
    }

    async fn fetch_token(
        &self,
        challenge: &BearerChallenge,
        registry: &str,
        scope: &str,
    ) -> Result<String> {
        let mut url = Url::parse(&challenge.realm)
            .with_context(|| format!("invalid token realm {}", challenge.realm))?;
        {
            let mut query = url.query_pairs_mut();
            if let Some(service) = &challenge.service {
                query.append_pair("service", service);
            }
            query.append_pair("scope", scope);
        }

        let mut request = self.http.get(url);
        if let Some(credentials) = self.credentials.get(registry) {
            request = request.basic_auth(&credentials.username, Some(&credentials.password));
        }

        let response = request.send().await.context("token request failed")?;
        if !response.status().is_success() {
            return Err(response_error(response, "token request").await);
        }

        let body: TokenResponse = response
            .json()
            .await
            .context("failed to parse token response")?;
        let token = body
            .token
            .or(body.access_token)
            .ok_or_else(|| anyhow!("token response did not contain a token"))?;

        let expires_in = body.expires_in.unwrap_or(60).saturating_sub(30).max(10);
        self.store_token_with_ttl(registry, scope, &token, Duration::from_secs(expires_in));
        Ok(token)
    }

    fn token_key(registry: &str, scope: &str) -> String {
        format!("{registry}|{scope}")
    }

    fn cached_token(&self, registry: &str, scope: &str) -> Option<String> {
        let tokens = self.tokens.lock().unwrap();
        tokens
            .get(&Self::token_key(registry, scope))
            .filter(|entry| entry.expires_at > Instant::now())
            .map(|entry| entry.token.clone())
    }

    fn store_token(&self, registry: &str, scope: &str, token: &str) {
        self.store_token_with_ttl(registry, scope, token, Duration::from_secs(60));
    }

    fn store_token_with_ttl(&self, registry: &str, scope: &str, token: &str, ttl: Duration) {
        let mut tokens = self.tokens.lock().unwrap();
        tokens.insert(
            Self::token_key(registry, scope),
            TokenEntry {
                token: token.to_string(),
                expires_at: Instant::now() + ttl,
            },
        );
    }
}

fn auth_challenge(response: &Response) -> Option<AuthChallenge> {
    for value in response.headers().get_all(WWW_AUTHENTICATE) {
        let Ok(value) = value.to_str() else {
            continue;
        };
        if let Some(challenge) = parse_authenticate_header(value) {
            return Some(challenge);
        }
    }
    None
}

fn parse_authenticate_header(value: &str) -> Option<AuthChallenge> {
    let (scheme, rest) = value.split_once(' ')?;
    match scheme.to_ascii_lowercase().as_str() {
        "basic" => Some(AuthChallenge::Basic),
        "bearer" => {
            let params = parse_auth_params(rest);
            let realm = params.get("realm")?.clone();
            if realm.is_empty() {
                return None;
            }
            Some(AuthChallenge::Bearer(BearerChallenge {
                realm,
                service: params.get("service").cloned(),
                scope: params.get("scope").cloned(),
            }))
        }
        _ => None,
    }
}

fn parse_auth_params(input: &str) -> HashMap<String, String> {
    let bytes = input.as_bytes();
    let mut params = HashMap::new();
    let mut index = 0usize;

    while index < bytes.len() {
        while index < bytes.len() && (bytes[index] == b',' || bytes[index].is_ascii_whitespace()) {
            index += 1;
        }
        if index >= bytes.len() {
            break;
        }

        let key_start = index;
        while index < bytes.len() && bytes[index] != b'=' && bytes[index] != b',' {
            index += 1;
        }
        if index >= bytes.len() || bytes[index] != b'=' {
            while index < bytes.len() && bytes[index] != b',' {
                index += 1;
            }
            continue;
        }

        let key = input[key_start..index].trim().to_ascii_lowercase();
        index += 1;
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }

        let value = if index < bytes.len() && bytes[index] == b'"' {
            index += 1;
            let mut value = String::new();
            while index < bytes.len() {
                match bytes[index] {
                    b'\\' if index + 1 < bytes.len() => {
                        value.push(bytes[index + 1] as char);
                        index += 2;
                    }
                    b'"' => {
                        index += 1;
                        break;
                    }
                    byte => {
                        value.push(byte as char);
                        index += 1;
                    }
                }
            }
            value
        } else {
            let value_start = index;
            while index < bytes.len() && bytes[index] != b',' {
                index += 1;
            }
            input[value_start..index].trim().to_string()
        };

        params.insert(key, value);
    }

    params
}

fn next_link(header: &str, base: &str) -> Result<Option<String>> {
    let base_url = Url::parse(base).context("invalid registry base URL")?;

    for part in header.split(',') {
        let part = part.trim();
        if !part.to_ascii_lowercase().contains("rel=\"next\"") {
            continue;
        }

        let start = part
            .find('<')
            .ok_or_else(|| anyhow!("invalid pagination Link header"))?;
        let end = part
            .find('>')
            .ok_or_else(|| anyhow!("invalid pagination Link header"))?;
        let target = &part[start + 1..end];
        let url = if target.starts_with("http://") || target.starts_with("https://") {
            Url::parse(target).context("invalid pagination URL")?
        } else {
            base_url.join(target).context("invalid pagination URL")?
        };

        if url.host_str() != base_url.host_str()
            || url.port_or_known_default() != base_url.port_or_known_default()
        {
            bail!("registry returned a cross-host pagination link");
        }

        return Ok(Some(url.to_string()));
    }

    Ok(None)
}

async fn response_error(response: Response, context: &str) -> anyhow::Error {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    let body = body.replace('\n', " ");
    let snippet: String = body.chars().take(300).collect();
    if snippet.is_empty() {
        anyhow!("{context} failed with HTTP {status}")
    } else {
        anyhow!("{context} failed with HTTP {status}: {snippet}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bearer_challenge() {
        let challenge = parse_authenticate_header(
            r#"Bearer realm="https://auth.docker.io/token",service="registry.docker.io",scope="repository:library/nginx:pull""#,
        )
        .unwrap();

        assert_eq!(
            challenge,
            AuthChallenge::Bearer(BearerChallenge {
                realm: "https://auth.docker.io/token".to_string(),
                service: Some("registry.docker.io".to_string()),
                scope: Some("repository:library/nginx:pull".to_string()),
            })
        );
    }

    #[test]
    fn parses_basic_challenge() {
        assert_eq!(
            parse_authenticate_header(r#"Basic realm="registry""#),
            Some(AuthChallenge::Basic)
        );
    }

    #[test]
    fn parses_commas_inside_quoted_values() {
        let challenge = parse_authenticate_header(
            r#"Bearer realm="https://example.test/token",service="example",scope="repository:team/tool:pull""#,
        )
        .unwrap();

        match challenge {
            AuthChallenge::Bearer(challenge) => {
                assert_eq!(challenge.realm, "https://example.test/token");
                assert_eq!(challenge.service.as_deref(), Some("example"));
                assert_eq!(
                    challenge.scope.as_deref(),
                    Some("repository:team/tool:pull")
                );
            }
            _ => panic!("expected bearer challenge"),
        }
    }

    #[test]
    fn parses_next_link() {
        let link = r#"<https://registry-1.docker.io/v2/library/nginx/tags/list?last=1.27&n=1000>; rel="next""#;
        let next = next_link(link, "https://registry-1.docker.io").unwrap();
        assert_eq!(
            next.as_deref(),
            Some("https://registry-1.docker.io/v2/library/nginx/tags/list?last=1.27&n=1000")
        );
    }

    #[test]
    fn rejects_cross_host_next_link() {
        let link = r#"<https://evil.example/v2/tags>; rel="next""#;
        assert!(next_link(link, "https://registry-1.docker.io").is_err());
    }
}
