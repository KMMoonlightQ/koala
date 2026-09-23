//! Context metadata from https://models.dev/api.json (no credentials are sent).
//! The bundled snapshot contains only provider endpoints, IDs and context limits.
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path, time::Duration};

const CATALOG_URL: &str = "https://models.dev/api.json";
const CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(transparent)]
pub struct ModelCatalog(BTreeMap<String, Provider>);

#[derive(Debug, Deserialize, Serialize)]
struct Provider {
    api: Option<String>,
    models: BTreeMap<String, Model>,
}

#[derive(Debug, Deserialize, Serialize)]
struct Model {
    limit: Limit,
}

#[derive(Debug, Deserialize, Serialize)]
struct Limit {
    context: u64,
}

impl ModelCatalog {
    pub fn bundled() -> Self {
        serde_json::from_str(include_str!("models-context.json"))
            .expect("valid bundled model catalog")
    }

    /// Fetch metadata only; never use the LLM client's credentials or headers.
    pub async fn fetch(url: &str) -> Result<Self, reqwest::Error> {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()?
            .get(url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await
    }

    /// Exact IDs only. Endpoint-specific limits take precedence over native IDs.
    pub fn context_window(&self, endpoint: &str, model: &str) -> Option<u64> {
        let endpoint = endpoint.trim_end_matches('/');
        let mut matched: Vec<_> = self
            .0
            .iter()
            .filter_map(|(id, provider)| {
                let api = provider
                    .api
                    .as_deref()
                    .or_else(|| native_endpoint(id))?
                    .trim_end_matches('/');
                (endpoint == api
                    || endpoint
                        .strip_prefix(api)
                        .is_some_and(|rest| rest.starts_with('/')))
                .then_some((api.len(), provider))
            })
            .collect();
        matched.sort_by_key(|(len, _)| std::cmp::Reverse(*len));
        if let Some((specificity, _)) = matched.first() {
            // Providers can share an endpoint with complementary model lists.
            // Conflicting limits at the same endpoint are deliberately unresolved.
            return agreed_capacity(
                matched
                    .iter()
                    .take_while(|(len, _)| len == specificity)
                    .filter_map(|(_, provider)| capacity(provider, model)),
            );
        }
        if let Some((provider, id)) = model.split_once('/')
            && let Some(provider) = self.0.get(provider)
        {
            return capacity(provider, model).or_else(|| capacity(provider, id));
        }
        // Native model IDs conventionally carry the provider name, e.g. deepseek-v4-flash.
        if let Some((_, provider)) = self.0.iter().find(|(id, provider)| {
            model.starts_with(&format!("{id}-")) && capacity(provider, model).is_some()
        }) {
            return capacity(provider, model);
        }
        // A bare ID without a known provider is usable only if all entries agree.
        agreed_capacity(
            self.0
                .values()
                .filter_map(|provider| capacity(provider, model)),
        )
    }

    pub fn context_window_for_provider(
        &self,
        provider: &str,
        endpoint: &str,
        model: &str,
    ) -> Option<u64> {
        if provider == "openai_compatible" {
            return self.context_window(endpoint, model);
        }
        let catalog_provider = match provider {
            "gemini" => "google",
            "open_router" => "openrouter",
            "moonshot" => "moonshotai",
            "ollama_cloud" => "ollama-cloud",
            other => other,
        };
        (!endpoint.is_empty())
            .then(|| self.context_window(endpoint, model))
            .flatten()
            .or_else(|| {
                self.0
                    .get(catalog_provider)
                    .and_then(|entry| capacity(entry, model))
            })
    }
}

// models.dev omits `api` when a provider's native SDK supplies the default.
fn native_endpoint(provider: &str) -> Option<&'static str> {
    match provider {
        "openai" => Some("https://api.openai.com/v1"),
        "anthropic" => Some("https://api.anthropic.com/v1"),
        "google" => Some("https://generativelanguage.googleapis.com/v1beta/openai"),
        "xai" => Some("https://api.x.ai/v1"),
        _ => None,
    }
}

fn agreed_capacity(mut limits: impl Iterator<Item = u64>) -> Option<u64> {
    let first = limits.next()?;
    limits.all(|limit| limit == first).then_some(first)
}

fn capacity(provider: &Provider, model: &str) -> Option<u64> {
    provider
        .models
        .get(model)
        .map(|m| m.limit.context)
        .filter(|v| *v > 0)
}

async fn load(cache: Option<&Path>, url: &str) -> ModelCatalog {
    let cached = cache.and_then(|path| {
        let bytes = std::fs::read(path).ok()?;
        let catalog = serde_json::from_slice::<ModelCatalog>(&bytes).ok()?;
        (!catalog.0.is_empty()).then_some(catalog)
    });
    let fresh = cache
        .and_then(|p| std::fs::metadata(p).ok())
        .and_then(|m| m.modified().ok())
        .and_then(|time| time.elapsed().ok())
        .is_some_and(|age| age < CACHE_TTL);
    if fresh && let Some(catalog) = cached {
        return catalog;
    }
    if let Ok(catalog) = ModelCatalog::fetch(url).await
        && !catalog.0.is_empty()
    {
        if let Some(path) = cache {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(bytes) = serde_json::to_vec(&catalog) {
                let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
                if std::fs::write(&temporary, bytes).is_ok() {
                    let _ = std::fs::rename(&temporary, path);
                }
                let _ = std::fs::remove_file(temporary);
            }
        }
        return catalog;
    }
    cached.unwrap_or_else(ModelCatalog::bundled)
}

pub async fn catalog() -> &'static ModelCatalog {
    static CATALOG: tokio::sync::OnceCell<ModelCatalog> = tokio::sync::OnceCell::const_new();
    CATALOG
        .get_or_init(|| async {
            let cache = dirs::cache_dir().map(|p| p.join("koala/models-context.json"));
            let initial = cache
                .as_deref()
                .and_then(|path| std::fs::read(path).ok())
                .and_then(|bytes| serde_json::from_slice::<ModelCatalog>(&bytes).ok())
                .filter(|catalog| !catalog.0.is_empty())
                .unwrap_or_else(ModelCatalog::bundled);
            // Refresh for the next process without putting network latency on startup.
            tokio::spawn(async move {
                load(cache.as_deref(), CATALOG_URL).await;
            });
            initial
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_provider_limits_without_guessing_ambiguous_or_similar_ids() {
        let catalog: ModelCatalog = serde_json::from_value(serde_json::json!({
            "deepseek": {"api":"https://api.deepseek.com", "models": {
                "deepseek-v4-flash": {"limit":{"context":1_000_000}},
                "shared": {"limit":{"context":128_000}},
                "zero": {"limit":{"context":0}}
            }},
            "proxy": {"api":"https://proxy.example/v1", "models": {
                "deepseek-v4-flash": {"limit":{"context":64_000}},
                "shared": {"limit":{"context":64_000}}
            }},
            "proxy-extra": {"api":"https://proxy.example/v1", "models": {
                "extra-model": {"limit":{"context":256_000}},
                "shared": {"limit":{"context":32_000}}
            }}
        }))
        .unwrap();
        for (endpoint, model, expected) in [
            ("https://proxy.example/v1", "extra-model", Some(256_000)),
            ("https://proxy.example/v1", "shared", None),
            (
                "https://api.deepseek.com/v1",
                "deepseek-v4-flash",
                Some(1_000_000),
            ),
            (
                "https://proxy.example/v1/",
                "deepseek-v4-flash",
                Some(64_000),
            ),
            (
                "https://custom.example/v1",
                "deepseek-v4-flash",
                Some(1_000_000),
            ),
            (
                "https://custom.example/v1",
                "deepseek/deepseek-v4-flash",
                Some(1_000_000),
            ),
            ("https://custom.example/v1", "shared", None),
            ("https://api.deepseek.com.evil/v1", "shared", None),
            (
                "https://api.deepseek.com/v1",
                "deepseek-v4-flash-custom",
                None,
            ),
            ("https://api.deepseek.com/v1", "zero", None),
        ] {
            assert_eq!(
                catalog.context_window(endpoint, model),
                expected,
                "{endpoint} {model}"
            );
        }
    }

    #[test]
    fn resolves_native_openai_endpoint_when_catalog_omits_api() {
        let catalog = ModelCatalog::bundled();
        assert_eq!(
            catalog.context_window("https://api.openai.com/v1", "gpt-4.1"),
            Some(1_047_576)
        );
    }

    #[tokio::test]
    async fn offline_uses_cached_metadata_or_bundled_snapshot() {
        let path = std::env::temp_dir().join(format!("koala-models-{}.json", uuid::Uuid::new_v4()));
        let fixture = r#"{"private":{"api":"https://private.example/v1","models":{"custom":{"limit":{"context":64000}}}}}"#;
        std::fs::write(&path, fixture).unwrap();
        // A stale cache remains usable if refreshing it fails.
        std::fs::File::open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH))
            .unwrap();
        let cached = load(Some(&path), "http://127.0.0.1:0").await;
        assert_eq!(
            cached.context_window("https://private.example/v1", "custom"),
            Some(64_000)
        );
        std::fs::write(&path, "invalid JSON").unwrap();
        let bundled = load(Some(&path), "http://127.0.0.1:0").await;
        assert_eq!(
            bundled.context_window("https://api.deepseek.com/v1", "deepseek-v4-flash"),
            Some(1_000_000)
        );
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn refresh_fetches_public_metadata_and_reuses_fresh_cache() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/api.json", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = [0; 4096];
            let n = socket.read(&mut bytes).await.unwrap();
            let request = String::from_utf8_lossy(&bytes[..n]).to_lowercase();
            assert!(request.starts_with("get /api.json "));
            assert!(!request.contains("authorization:"));
            let body = r#"{"private":{"api":"https://private.example/v1","models":{"custom":{"limit":{"context":64000}}}}}"#;
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        });
        let path = std::env::temp_dir().join(format!("koala-models-{}.json", uuid::Uuid::new_v4()));
        let fetched = load(Some(&path), &url).await;
        assert_eq!(
            fetched.context_window("https://private.example/v1", "custom"),
            Some(64_000)
        );
        server.await.unwrap();
        // No running server: a fresh cache must supply the same metadata.
        let cached = load(Some(&path), &url).await;
        assert_eq!(
            cached.context_window("https://private.example/v1", "custom"),
            Some(64_000)
        );
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    #[ignore = "requires access to the public models.dev API"]
    async fn live_deepseek_v4_flash_context_window() {
        let catalog = ModelCatalog::fetch(CATALOG_URL).await.unwrap();
        let window = catalog
            .context_window("https://api.deepseek.com/v1", "deepseek-v4-flash")
            .unwrap();
        println!(
            "deepseek-v4-flash: context_window={window} tokens, 75%={} tokens",
            window * 75 / 100
        );
        assert_eq!(window, 1_000_000);
    }
}
