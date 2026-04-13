use std::{process::Command, sync::Arc};

use anyhow::{Context, Result, anyhow};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::config::Config;

#[derive(Clone)]
pub struct ExternalServices {
    pub config: Arc<Config>,
    client: Client,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DenseHit {
    pub point_id: i64,
    pub card_id: i64,
    pub score: f32,
    pub snippet: String,
}

#[derive(Debug, Deserialize)]
struct EmbedResponse {
    embeddings: Vec<Vec<f32>>,
}

#[derive(Debug, Deserialize)]
struct RerankItem {
    index: usize,
    score: f32,
}

#[derive(Debug, Deserialize)]
struct RerankResponse {
    #[serde(default)]
    results: Vec<RerankItem>,
}

#[derive(Debug, Deserialize)]
struct QdrantEnvelope {
    result: QdrantQueryResult,
}

#[derive(Debug, Deserialize)]
struct QdrantQueryResult {
    #[serde(default)]
    points: Vec<QdrantPoint>,
}

#[derive(Debug, Deserialize)]
struct QdrantPoint {
    id: serde_json::Value,
    score: f32,
    payload: serde_json::Value,
}

impl ExternalServices {
    pub fn new(config: Arc<Config>) -> Self {
        Self {
            config,
            client: Client::new(),
        }
    }

    pub async fn embed(&self, inputs: &[String]) -> Result<Vec<Vec<f32>>> {
        let response = self
            .client
            .post(format!("{}/embed", self.config.tei_url))
            .json(&json!({ "inputs": inputs, "normalize": true, "truncate": true }))
            .send()
            .await
            .context("call TEI embed")?;
        if !response.status().is_success() {
            return Err(anyhow!("TEI embed failed with {}", response.status()));
        }
        let body: EmbedResponse = response.json().await.context("parse TEI embed")?;
        Ok(body.embeddings)
    }

    pub async fn rerank(&self, query: &str, candidates: &[String]) -> Result<Vec<(usize, f32)>> {
        let response = self
            .client
            .post(format!("{}/rerank", self.config.tei_url))
            .json(&json!({ "query": query, "texts": candidates, "truncate": true }))
            .send()
            .await
            .context("call TEI rerank")?;
        if !response.status().is_success() {
            return Err(anyhow!("TEI rerank failed with {}", response.status()));
        }
        let body: RerankResponse = response.json().await.context("parse TEI rerank")?;
        Ok(body
            .results
            .into_iter()
            .map(|item| (item.index, item.score))
            .collect())
    }

    pub async fn rerank_pairs(
        &self,
        queries: &[String],
        candidates: &[String],
    ) -> Result<Vec<(usize, f32)>> {
        let response = self
            .client
            .post(format!("{}/rerank_pairs", self.config.tei_url))
            .json(&json!({ "queries": queries, "texts": candidates, "truncate": true }))
            .send()
            .await
            .context("call TEI rerank pairs")?;
        if !response.status().is_success() {
            return Err(anyhow!(
                "TEI rerank pairs failed with {}",
                response.status()
            ));
        }
        let body: RerankResponse = response.json().await.context("parse TEI rerank pairs")?;
        Ok(body
            .results
            .into_iter()
            .map(|item| (item.index, item.score))
            .collect())
    }

    pub async fn dense_search(
        &self,
        vector: Vec<f32>,
        selected_deck_import_id: Option<i64>,
        limit: usize,
    ) -> Result<Vec<DenseHit>> {
        let mut must = Vec::new();
        if let Some(import_id) = selected_deck_import_id {
            must.push(json!({
                "key": "import_id",
                "match": { "value": import_id }
            }));
        }
        let response = self
            .client
            .post(format!(
                "{}/collections/{}/points/query",
                self.config.qdrant_url, self.config.qdrant_collection
            ))
            .json(&json!({
                "query": vector,
                "limit": limit,
                "with_payload": true,
                "filter": {
                    "must": must,
                    "must_not": [
                        { "key": "chunk_index", "match": { "value": -1 } }
                    ]
                }
            }))
            .send()
            .await
            .context("query qdrant")?;
        if !response.status().is_success() {
            return Err(anyhow!(
                "Qdrant dense search failed with {}",
                response.status()
            ));
        }
        let envelope: QdrantEnvelope = response.json().await.context("parse qdrant query")?;
        Ok(envelope
            .result
            .points
            .into_iter()
            .filter_map(|point| {
                let point_id = point.id.as_i64()?;
                let card_id = qdrant_id_to_i64(point.payload.get("card_id")?)?;
                let snippet = point
                    .payload
                    .get("chunk_text")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_string();
                Some(DenseHit {
                    point_id,
                    card_id,
                    score: point.score,
                    snippet,
                })
            })
            .collect())
    }

    pub fn convert_office_to_pdf(
        &self,
        source_path: &std::path::Path,
        output_dir: &std::path::Path,
    ) -> Result<std::path::PathBuf> {
        let status = Command::new(&self.config.unoconvert_bin)
            .arg("--convert-to")
            .arg("pdf")
            .arg("--output")
            .arg(output_dir)
            .arg(source_path)
            .status()
            .with_context(|| format!("run {}", self.config.unoconvert_bin))?;
        if !status.success() {
            return Err(anyhow!(
                "unoconvert failed; install unoserver/unoconvert or set UNOCONVERT_BIN"
            ));
        }
        let mut pdf_path = output_dir.join(
            source_path
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_else(|| "converted".into()),
        );
        pdf_path.set_extension("pdf");
        Ok(pdf_path)
    }
}

fn qdrant_id_to_i64(value: &serde_json::Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_str().and_then(|raw| raw.parse::<i64>().ok()))
}
