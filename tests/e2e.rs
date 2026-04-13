use std::{
    collections::HashSet,
    io::Cursor,
    net::TcpListener,
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use axum::{
    Json, Router,
    extract::State,
    routing::{get, post},
};
use regex::Regex;
use reqwest::{
    Client, StatusCode,
    header::{CONTENT_TYPE, LOCATION},
    multipart,
    redirect::Policy,
};
use serde::Deserialize;
use serde_json::json;
use tokio::{net::TcpListener as TokioTcpListener, task::JoinHandle, time::sleep};
use zip::ZipArchive;

const VECTOR_SIZE: usize = 8;
const READY_TIMEOUT: Duration = Duration::from_secs(60);
const POLL_INTERVAL: Duration = Duration::from_millis(300);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires a reachable Qdrant instance"]
async fn end_to_end_search_flow() -> Result<()> {
    let qdrant_url = std::env::var("QDRANT_URL").unwrap_or_else(|_| "http://127.0.0.1:6335".into());
    wait_for_http_200(&format!("{qdrant_url}/collections"), READY_TIMEOUT)
        .await
        .with_context(|| format!("Qdrant is not reachable at {qdrant_url}"))?;

    let (tei_base_url, tei_handle) = start_mock_tei(VECTOR_SIZE).await?;
    let temp = tempfile::tempdir().context("create tempdir")?;
    let deck_path = temp.path().join("fixture.apkg");
    create_fixture_apkg(&deck_path)?;

    let app_port = free_port()?;
    let app_base_url = format!("http://127.0.0.1:{app_port}");
    let _app = spawn_app(
        temp.path(),
        app_port,
        &qdrant_url,
        &tei_base_url,
        VECTOR_SIZE,
    )?;

    wait_for_http_200(&app_base_url, READY_TIMEOUT)
        .await
        .context("wait for shiori home page")?;

    let client = Client::builder()
        .redirect(Policy::none())
        .build()
        .context("build reqwest client")?;

    let deck_location = upload_deck(&client, &app_base_url, &deck_path).await?;
    let deck_id = parse_id_from_location(&deck_location, "decks")?;
    let deck_html = wait_for_body(&client, &format!("{app_base_url}{deck_location}"), |body| {
        body.contains("badge ready") && body.contains("4 notes") && body.contains("4 cards")
    })
    .await
    .context("wait for imported deck to be ready")?;
    assert!(deck_html.contains("Shiori IMCI Fixture"));

    let material_text = "IMCI outpatient management of children under five relies on counselling skills and analysis of family practices.";
    let material_location = upload_material_text(&client, &app_base_url, material_text).await?;
    let material_id = parse_id_from_location(&material_location, "materials")?;
    let material_html = wait_for_body(
        &client,
        &format!("{app_base_url}{material_location}"),
        |body| body.contains("badge ready") && body.contains("Run search"),
    )
    .await
    .context("wait for material ingest to be ready")?;
    assert!(material_html.contains("Pasted query"));
    assert!(material_html.contains("Search cards"));

    let search_location = create_search(&client, &app_base_url, material_id, deck_id).await?;
    let search_id = parse_id_from_location(&search_location, "searches")?;
    let search_html = wait_for_body(
        &client,
        &format!("{app_base_url}{search_location}"),
        |body| {
            body.contains("badge ready")
                && body.contains("Counselling skills")
                && body.contains("Integrated Management of Child Health")
        },
    )
    .await
    .context("wait for search results to be ready")?;

    let card_id = first_card_id(&search_html)?;
    add_to_cart(&client, &app_base_url, search_id, card_id).await?;

    let cart_html = fetch_text(&client, &format!("{app_base_url}/searches/{search_id}"))
        .await
        .context("fetch search page after adding cart item")?;
    assert!(cart_html.contains("Remove from cart"));
    assert!(cart_html.contains("Export APKG"));

    let export_bytes = export_apkg(&client, &app_base_url, search_id).await?;
    assert_export_archive(&export_bytes)?;

    let home_html = fetch_text(&client, &app_base_url)
        .await
        .context("fetch dashboard after workflow")?;
    assert!(home_html.contains("fixture.apkg"));
    assert!(home_html.contains("Pasted query"));
    assert!(home_html.contains(&format!("Search {search_id}")));

    tei_handle.abort();
    let _ = tei_handle.await;
    Ok(())
}

fn create_fixture_apkg(path: &Path) -> Result<()> {
    let status = Command::new(env!("CARGO_BIN_EXE_make_fixture_apkg"))
        .arg(path)
        .status()
        .context("run make_fixture_apkg")?;
    if !status.success() {
        bail!("make_fixture_apkg exited with {status}");
    }
    Ok(())
}

fn spawn_app(
    temp_root: &Path,
    app_port: u16,
    qdrant_url: &str,
    tei_base_url: &str,
    vector_size: usize,
) -> Result<ChildGuard> {
    let collection = format!("shiori-e2e-{}", chrono::Utc::now().timestamp_micros());
    let child = Command::new(env!("CARGO_BIN_EXE_shiori"))
        .env("APP_HOST", "127.0.0.1")
        .env("APP_PORT", app_port.to_string())
        .env("APP_DATA_DIR", temp_root.join("data"))
        .env("QDRANT_URL", qdrant_url)
        .env("QDRANT_COLLECTION", collection)
        .env("TEI_URL", tei_base_url)
        .env("EMBEDDING_VECTOR_SIZE", vector_size.to_string())
        .env("RUST_LOG", "warn")
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .context("spawn shiori")?;
    Ok(ChildGuard { child })
}

async fn upload_deck(client: &Client, base_url: &str, deck_path: &Path) -> Result<String> {
    let deck_bytes = tokio::fs::read(deck_path)
        .await
        .with_context(|| format!("read {}", deck_path.display()))?;
    let deck_part = multipart::Part::bytes(deck_bytes)
        .file_name("fixture.apkg")
        .mime_str("application/octet-stream")
        .context("set deck mime type")?;
    let response = client
        .post(format!("{base_url}/decks"))
        .multipart(multipart::Form::new().part("deck_file", deck_part))
        .send()
        .await
        .context("upload deck")?;
    expect_redirect(response, "upload deck")
}

async fn upload_material_text(client: &Client, base_url: &str, text: &str) -> Result<String> {
    let response = client
        .post(format!("{base_url}/materials"))
        .multipart(multipart::Form::new().text("query_text", text.to_string()))
        .send()
        .await
        .context("upload pasted material")?;
    expect_redirect(response, "upload material")
}

async fn create_search(
    client: &Client,
    base_url: &str,
    material_id: i64,
    deck_id: i64,
) -> Result<String> {
    let response = client
        .post(format!("{base_url}/searches"))
        .form(&[
            ("material_id", material_id.to_string()),
            ("selected_deck_import_id", deck_id.to_string()),
        ])
        .send()
        .await
        .context("create search")?;
    expect_redirect(response, "create search")
}

async fn add_to_cart(client: &Client, base_url: &str, search_id: i64, card_id: i64) -> Result<()> {
    let response = client
        .post(format!("{base_url}/searches/{search_id}/cart"))
        .form(&[
            ("card_id", card_id.to_string()),
            ("action", "add".to_string()),
        ])
        .send()
        .await
        .context("add result to export cart")?;
    let location = expect_redirect(response, "add to cart")?;
    if !location.ends_with(&format!("/searches/{search_id}")) {
        bail!("unexpected redirect after cart update: {location}");
    }
    Ok(())
}

async fn export_apkg(client: &Client, base_url: &str, search_id: i64) -> Result<Vec<u8>> {
    let response = client
        .get(format!("{base_url}/searches/{search_id}/export.apkg"))
        .send()
        .await
        .context("download exported apkg")?;
    if response.status() != StatusCode::OK {
        bail!("export failed with {}", response.status());
    }
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    if content_type != "application/octet-stream" {
        bail!("unexpected export content type: {content_type}");
    }
    Ok(response.bytes().await.context("read export body")?.to_vec())
}

fn assert_export_archive(bytes: &[u8]) -> Result<()> {
    let cursor = Cursor::new(bytes.to_vec());
    let mut archive = ZipArchive::new(cursor).context("open exported apkg as zip")?;
    let mut names = HashSet::new();
    for index in 0..archive.len() {
        let file = archive.by_index(index).context("read zip entry")?;
        names.insert(file.name().to_string());
    }
    if !(names.contains("collection.anki21") || names.contains("collection.anki2")) {
        bail!("exported apkg is missing an Anki collection database");
    }
    Ok(())
}

fn parse_id_from_location(location: &str, resource: &str) -> Result<i64> {
    let prefix = format!("/{resource}/");
    let id = location
        .strip_prefix(&prefix)
        .ok_or_else(|| anyhow!("unexpected redirect path: {location}"))?;
    id.parse::<i64>()
        .with_context(|| format!("parse id from {location}"))
}

fn first_card_id(search_html: &str) -> Result<i64> {
    let regex = Regex::new(r#"name="card_id" value="(\d+)""#).context("compile card regex")?;
    let captures = regex
        .captures(search_html)
        .ok_or_else(|| anyhow!("card id not found in search results page"))?;
    captures[1]
        .parse::<i64>()
        .context("parse card id from search page")
}

async fn fetch_text(client: &Client, url: &str) -> Result<String> {
    let response = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    if response.status() != StatusCode::OK {
        bail!("GET {url} returned {}", response.status());
    }
    response
        .text()
        .await
        .with_context(|| format!("read body for {url}"))
}

async fn wait_for_http_200(url: &str, timeout: Duration) -> Result<()> {
    wait_for_condition(timeout, || async {
        match reqwest::get(url).await {
            Ok(response) if response.status() == StatusCode::OK => Ok(Some(())),
            Ok(_) => Ok(None),
            Err(_) => Ok(None),
        }
    })
    .await
    .with_context(|| format!("wait for HTTP 200 from {url}"))
}

async fn wait_for_body<F>(client: &Client, url: &str, predicate: F) -> Result<String>
where
    F: Fn(&str) -> bool,
{
    wait_for_condition(READY_TIMEOUT, || {
        let client = client.clone();
        let url = url.to_string();
        let predicate = &predicate;
        async move {
            let response = client.get(&url).send().await;
            let Ok(response) = response else {
                return Ok(None);
            };
            if response.status() != StatusCode::OK {
                return Ok(None);
            }
            let body = response
                .text()
                .await
                .with_context(|| format!("read body for {url}"))?;
            if predicate(&body) {
                Ok(Some(body))
            } else {
                Ok(None)
            }
        }
    })
    .await
}

async fn wait_for_condition<T, F, Fut>(timeout: Duration, mut action: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<Option<T>>>,
{
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(value) = action().await? {
            return Ok(value);
        }
        if Instant::now() >= deadline {
            bail!("timed out after {}", timeout.as_secs());
        }
        sleep(POLL_INTERVAL).await;
    }
}

fn expect_redirect(response: reqwest::Response, context: &str) -> Result<String> {
    if response.status() != StatusCode::SEE_OTHER {
        bail!(
            "{context} expected 303 See Other, got {}",
            response.status()
        );
    }
    response
        .headers()
        .get(LOCATION)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_string())
        .ok_or_else(|| anyhow!("{context} missing Location header"))
}

fn free_port() -> Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0").context("bind ephemeral port")?;
    Ok(listener.local_addr().context("read local addr")?.port())
}

#[derive(Clone)]
struct MockTeiState {
    vector_size: usize,
}

#[derive(Debug, Deserialize)]
struct EmbedRequest {
    inputs: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct RerankRequest {
    query: String,
    texts: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct RerankPairsRequest {
    queries: Vec<String>,
    texts: Vec<String>,
}

async fn start_mock_tei(vector_size: usize) -> Result<(String, JoinHandle<()>)> {
    let state = MockTeiState { vector_size };
    let app = Router::new()
        .route("/health", get(mock_health))
        .route("/embed", post(mock_embed))
        .route("/rerank", post(mock_rerank))
        .route("/rerank_pairs", post(mock_rerank_pairs))
        .with_state(state);

    let listener = TokioTcpListener::bind("127.0.0.1:0")
        .await
        .context("bind mock tei listener")?;
    let addr = listener.local_addr().context("mock tei local addr")?;
    let handle = tokio::spawn(async move {
        if let Err(error) = axum::serve(listener, app).await {
            panic!("mock tei server failed: {error}");
        }
    });
    Ok((format!("http://{addr}"), handle))
}

async fn mock_health() -> Json<serde_json::Value> {
    Json(json!({ "status": "ok" }))
}

async fn mock_embed(
    State(state): State<MockTeiState>,
    Json(request): Json<EmbedRequest>,
) -> Json<serde_json::Value> {
    Json(json!({
        "embeddings": request
            .inputs
            .iter()
            .map(|text| embed_text(text, state.vector_size))
            .collect::<Vec<_>>()
    }))
}

async fn mock_rerank(Json(request): Json<RerankRequest>) -> Json<serde_json::Value> {
    Json(json!({
        "results": request
            .texts
            .iter()
            .enumerate()
            .map(|(index, text)| json!({ "index": index, "score": overlap_score(&request.query, text) }))
            .collect::<Vec<_>>()
    }))
}

async fn mock_rerank_pairs(Json(request): Json<RerankPairsRequest>) -> Json<serde_json::Value> {
    Json(json!({
        "results": request
            .queries
            .iter()
            .zip(request.texts.iter())
            .enumerate()
            .map(|(index, (query, text))| json!({ "index": index, "score": overlap_score(query, text) }))
            .collect::<Vec<_>>()
    }))
}

fn embed_text(text: &str, vector_size: usize) -> Vec<f32> {
    let mut vector = vec![0.0_f32; vector_size];
    for (index, byte) in text.bytes().enumerate() {
        vector[index % vector_size] += byte as f32 / 255.0;
    }
    let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    if norm > 0.0 {
        for value in &mut vector {
            *value /= norm;
        }
    }
    vector
}

fn overlap_score(query: &str, text: &str) -> f32 {
    let query_terms = tokenize(query);
    if query_terms.is_empty() {
        return 0.0;
    }
    let text_terms = tokenize(text);
    let overlap = query_terms.intersection(&text_terms).count();
    overlap as f32 / query_terms.len() as f32
}

fn tokenize(text: &str) -> HashSet<String> {
    text.split(|ch: char| !ch.is_alphanumeric())
        .filter(|term| term.len() > 2)
        .map(|term| term.to_ascii_lowercase())
        .collect()
}

struct ChildGuard {
    child: Child,
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
