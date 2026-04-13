use std::path::PathBuf;

use axum::{
    Form, Router,
    body::Body,
    extract::{Multipart, Path, State},
    http::{HeaderMap, HeaderValue, header},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
};
use serde::Deserialize;
use tokio::io::AsyncWriteExt;

use crate::{
    AppState,
    error::{AppError, AppResult},
    services::{deck, export, material, search},
    templates::{
        DeckDetailTemplate, HtmlTemplate, IndexTemplate, MaterialDetailTemplate,
        SearchDetailTemplate,
    },
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(index))
        .route("/decks", post(upload_deck))
        .route("/decks/:id", get(deck_detail))
        .route("/materials", post(upload_material))
        .route("/materials/:id", get(material_detail))
        .route("/searches", post(create_search))
        .route("/searches/:id", get(search_detail))
        .route("/searches/:id/cart", post(update_cart))
        .route("/searches/:id/export.apkg", get(export_apkg))
}

async fn index(State(state): State<AppState>) -> AppResult<impl IntoResponse> {
    let data = state.db.dashboard()?;
    Ok(HtmlTemplate(IndexTemplate { data }))
}

async fn deck_detail(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<impl IntoResponse> {
    let detail = state
        .db
        .get_deck_detail(id)?
        .ok_or_else(|| AppError::not_found(format!("deck {id} not found")))?;
    let should_refresh = detail.deck.status == "pending";
    Ok(HtmlTemplate(DeckDetailTemplate {
        data: detail,
        should_refresh,
    }))
}

async fn material_detail(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<impl IntoResponse> {
    let detail = state
        .db
        .get_material_detail(id)?
        .ok_or_else(|| AppError::not_found(format!("material {id} not found")))?;
    let decks = state.db.list_decks()?;
    let should_refresh = detail.material.status == "pending";
    Ok(HtmlTemplate(MaterialDetailTemplate {
        data: detail,
        decks,
        should_refresh,
    }))
}

async fn search_detail(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<impl IntoResponse> {
    let detail = state
        .db
        .get_search_detail(id)?
        .ok_or_else(|| AppError::not_found(format!("search {id} not found")))?;
    let should_refresh = detail.search.status == "pending";
    Ok(HtmlTemplate(SearchDetailTemplate {
        data: detail,
        should_refresh,
    }))
}

async fn upload_deck(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> AppResult<impl IntoResponse> {
    while let Some(field) = multipart.next_field().await? {
        if field.name() != Some("deck_file") {
            continue;
        }
        let filename = field
            .file_name()
            .ok_or_else(|| AppError::bad_request("missing uploaded filename"))?
            .to_string();
        let safe_name = sanitize_filename::sanitize(&filename);
        let target = state.config.decks_dir.join(format!("upload-{safe_name}"));
        let mut file = tokio::fs::File::create(&target).await?;
        let bytes = field.bytes().await?;
        file.write_all(&bytes).await?;

        let deck_id = state
            .db
            .create_deck_import(&filename, &target.to_string_lossy())?;
        let task_state = state.clone();
        let task_path = target.clone();
        tokio::spawn(async move {
            if let Err(error) = deck::ingest_deck(task_state.clone(), deck_id, task_path).await {
                let _ = task_state.db.update_deck_status(
                    deck_id,
                    "failed",
                    None,
                    Some(&error.to_string()),
                    None,
                    None,
                );
            }
        });
        return Ok(Redirect::to(&format!("/decks/{deck_id}")));
    }
    Err(AppError::bad_request("expected deck_file upload"))
}

async fn upload_material(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> AppResult<impl IntoResponse> {
    let mut query_text: Option<String> = None;
    let mut stored_path: Option<PathBuf> = None;
    let mut filename: Option<String> = None;
    let mut kind = "text".to_string();

    while let Some(field) = multipart.next_field().await? {
        match field.name() {
            Some("material_file") => {
                if let Some(file_name) = field.file_name() {
                    let original = file_name.to_string();
                    let safe_name = sanitize_filename::sanitize(&original);
                    let target = state
                        .config
                        .materials_dir
                        .join(format!("upload-{safe_name}"));
                    let mut file = tokio::fs::File::create(&target).await?;
                    let bytes = field.bytes().await?;
                    file.write_all(&bytes).await?;
                    kind = target
                        .extension()
                        .and_then(|ext| ext.to_str())
                        .unwrap_or("file")
                        .to_string();
                    stored_path = Some(target);
                    filename = Some(original);
                }
            }
            Some("query_text") => {
                query_text = Some(field.text().await?);
            }
            _ => {}
        }
    }

    if let Some(path) = stored_path {
        let filename = filename.unwrap_or_else(|| "Uploaded material".into());
        let material_id = state.db.create_material(&filename, &kind)?;
        let task_state = state.clone();
        tokio::spawn(async move {
            if let Err(error) =
                material::ingest_material(task_state.clone(), material_id, filename, path).await
            {
                let _ = task_state.db.update_material(
                    material_id,
                    "failed",
                    None,
                    None,
                    Some(&error.to_string()),
                );
            }
        });
        return Ok(Redirect::to(&format!("/materials/{material_id}")));
    }

    if let Some(text) = query_text {
        let material_id = state.db.create_material("Pasted query", "text")?;
        state
            .db
            .update_material(material_id, "ready", Some(&text), None, None)?;
        return Ok(Redirect::to(&format!("/materials/{material_id}")));
    }

    Err(AppError::bad_request(
        "expected material_file or query_text",
    ))
}

#[derive(Debug, Deserialize)]
struct SearchForm {
    material_id: Option<i64>,
    query_text: Option<String>,
    selected_deck_import_id: Option<i64>,
}

async fn create_search(
    State(state): State<AppState>,
    Form(form): Form<SearchForm>,
) -> AppResult<impl IntoResponse> {
    let query_text = form.query_text.unwrap_or_default();
    let search_id =
        state
            .db
            .create_search(form.material_id, &query_text, form.selected_deck_import_id)?;
    let task_state = state.clone();
    tokio::spawn(async move {
        if let Err(error) = search::run_search(task_state.clone(), search_id).await {
            let _ = task_state.db.update_search_status(
                search_id,
                "failed",
                false,
                Some(&error.to_string()),
            );
        }
    });
    Ok(Redirect::to(&format!("/searches/{search_id}")))
}

#[derive(Debug, Deserialize)]
struct CartForm {
    card_id: i64,
    action: String,
}

async fn update_cart(
    State(state): State<AppState>,
    Path(search_id): Path<i64>,
    Form(form): Form<CartForm>,
) -> AppResult<impl IntoResponse> {
    state
        .db
        .upsert_cart_item(search_id, form.card_id, form.action != "remove")?;
    Ok(Redirect::to(&format!("/searches/{search_id}")))
}

async fn export_apkg(
    State(state): State<AppState>,
    Path(search_id): Path<i64>,
) -> AppResult<impl IntoResponse> {
    let export_path = export::export_search(&state, search_id)?;
    let bytes = tokio::fs::read(&export_path).await?;
    let filename = export_path
        .file_name()
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|| format!("search-{search_id}.apkg"));
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("attachment; filename=\"{filename}\""))
            .map_err(|_| AppError::internal("invalid content disposition header"))?,
    );
    Response::builder()
        .status(200)
        .body(Body::from(bytes))
        .map(|mut response| {
            *response.headers_mut() = headers;
            response
        })
        .map_err(|err| AppError::internal(err.to_string()))
}
