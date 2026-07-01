//! OpenAI-compatible embeddings client used by project memory.

use std::time::Duration;

use serde_json::{json, Value};

use crate::types::ProviderError;

/// Talk to an OpenAI-compatible `/embeddings` endpoint.
#[derive(Debug, Clone)]
pub struct EmbeddingProvider {
    client: reqwest::Client,
    endpoint: String,
    api_key: String,
    model: String,
    max_retries: u32,
}

impl EmbeddingProvider {
    pub fn new(
        api_key: impl Into<String>,
        base_url: &str,
        model: impl Into<String>,
        timeout_s: u64,
        max_retries: u32,
    ) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(timeout_s))
            .build()
            .expect("reqwest client builds with default (rustls) config");
        let endpoint = format!("{}/embeddings", base_url.trim_end_matches('/'));
        EmbeddingProvider {
            client,
            endpoint,
            api_key: api_key.into(),
            model: model.into(),
            max_retries,
        }
    }

    /// Embed a batch of texts. Retries transient transport/status failures.
    pub async fn embed_texts(&self, inputs: &[String]) -> Result<Vec<Vec<f64>>, ProviderError> {
        if inputs.is_empty() {
            return Ok(Vec::new());
        }
        let payload = json!({
            "model": self.model.clone(),
            "input": inputs,
            "encoding_format": "float",
        });

        let mut attempt = 0u32;
        loop {
            match self.post(&payload).await {
                Ok(json) => {
                    return parse_embedding_response(&json, inputs.len()).map_err(ProviderError)
                }
                Err(e) if e.transient && attempt < self.max_retries => {
                    attempt += 1;
                    let backoff = Duration::from_millis(500u64 << (attempt - 1).min(5));
                    tokio::time::sleep(backoff).await;
                }
                Err(e) => return Err(ProviderError(e.message)),
            }
        }
    }

    async fn post(&self, payload: &Value) -> Result<Value, EmbeddingHttpErr> {
        let resp = self
            .client
            .post(&self.endpoint)
            .bearer_auth(&self.api_key)
            .json(payload)
            .send()
            .await
            .map_err(EmbeddingHttpErr::from_reqwest)?;

        let status = resp.status();
        if status.is_success() {
            return resp.json::<Value>().await.map_err(|e| EmbeddingHttpErr {
                message: format!("decode error: {e}"),
                transient: false,
            });
        }
        let code = status.as_u16();
        let transient = matches!(code, 408 | 409 | 429) || (500..600).contains(&code);
        let text = resp.text().await.unwrap_or_default();
        Err(EmbeddingHttpErr {
            message: format!("HTTP {code}: {text}"),
            transient,
        })
    }
}

fn parse_embedding_response(value: &Value, expected_len: usize) -> Result<Vec<Vec<f64>>, String> {
    let data = value
        .get("data")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "embedding response missing data array".to_string())?;
    if data.len() != expected_len {
        return Err(format!(
            "embedding response returned {} vector(s), expected {expected_len}",
            data.len()
        ));
    }

    let mut rows: Vec<(usize, Vec<f64>)> = Vec::with_capacity(data.len());
    for (fallback_index, item) in data.iter().enumerate() {
        let index = item
            .get("index")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .unwrap_or(fallback_index);
        let vector = item
            .get("embedding")
            .and_then(|v| v.as_array())
            .ok_or_else(|| format!("embedding response item {index} missing embedding array"))?
            .iter()
            .map(|v| {
                v.as_f64()
                    .filter(|f| f.is_finite())
                    .ok_or_else(|| format!("embedding response item {index} contains a non-float"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        if vector.is_empty() {
            return Err(format!("embedding response item {index} returned an empty vector"));
        }
        rows.push((index, vector));
    }
    rows.sort_by_key(|(index, _)| *index);
    if rows.iter().enumerate().any(|(i, (index, _))| i != *index) {
        return Err("embedding response indexes are not contiguous".into());
    }
    Ok(rows.into_iter().map(|(_, vector)| vector).collect())
}

struct EmbeddingHttpErr {
    message: String,
    transient: bool,
}

impl EmbeddingHttpErr {
    fn from_reqwest(e: reqwest::Error) -> Self {
        let transient = e.is_timeout() || e.is_connect() || e.is_request();
        EmbeddingHttpErr {
            message: format!("RequestError: {e}"),
            transient,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_embedding_response_orders_by_index() {
        let value = json!({
            "data": [
                {"index": 1, "embedding": [0.0, 1.0]},
                {"index": 0, "embedding": [1.0, 0.0]}
            ]
        });

        let vectors = parse_embedding_response(&value, 2).unwrap();

        assert_eq!(vectors, vec![vec![1.0, 0.0], vec![0.0, 1.0]]);
    }

    #[test]
    fn parse_embedding_response_rejects_shape_mismatch() {
        let value = json!({"data": [{"index": 0, "embedding": []}]});

        let err = parse_embedding_response(&value, 1).unwrap_err();

        assert!(err.contains("empty vector"));
    }
}
