use std::{
    collections::HashMap,
    fs,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, anyhow};
use rusqlite::Connection;
use serde_json::Value;
use tempfile::tempdir;
use zip::ZipArchive;

use crate::{
    AppState,
    db::{CardRecord, DenseCardPoint},
    models::DeckModel,
    services::text::{chunk_text, hash_text, normalize_text},
};

pub async fn ingest_deck(
    state: AppState,
    deck_id: i64,
    archive_path: std::path::PathBuf,
) -> Result<()> {
    tokio::task::spawn_blocking(move || ingest_deck_blocking(state, deck_id, &archive_path))
        .await
        .context("join deck ingestion task")?
}

fn ingest_deck_blocking(state: AppState, deck_id: i64, archive_path: &Path) -> Result<()> {
    let extract_dir = state.config.decks_dir.join(deck_id.to_string());
    fs::create_dir_all(&extract_dir)?;

    let temp = tempdir()?;
    let file = fs::File::open(archive_path)?;
    let mut archive = ZipArchive::new(file)?;
    for i in 0..archive.len() {
        let mut item = archive.by_index(i)?;
        let enclosed_name = item
            .enclosed_name()
            .ok_or_else(|| anyhow!("APKG contains unsafe path: {}", item.name()))?;
        let out_path = temp.path().join(enclosed_name);
        if item.name().ends_with('/') {
            fs::create_dir_all(&out_path)?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut output = fs::File::create(&out_path)?;
        std::io::copy(&mut item, &mut output)?;
    }

    let collection_path = ["collection.anki21", "collection.anki2"]
        .iter()
        .map(|name| temp.path().join(name))
        .find(|path| path.exists())
        .ok_or_else(|| anyhow!("APKG missing collection.anki21/collection.anki2"))?;
    let media_map_path = temp.path().join("media");

    let conn = Connection::open(collection_path)?;
    let (models_json, decks_json): (String, String) =
        conn.query_row("SELECT models, decks FROM col LIMIT 1", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;

    let models_value: Value = serde_json::from_str(&models_json)?;
    let decks_value: Value = serde_json::from_str(&decks_json)?;
    let deck_names = parse_name_map(&decks_value);

    let mut models = Vec::new();
    for (original_model_id, model_value) in models_value.as_object().into_iter().flatten() {
        models.push(DeckModel {
            id: state.db.next_id(),
            import_id: deck_id,
            original_model_id: original_model_id.parse::<i64>()?,
            name: model_value
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("Unknown model")
                .to_string(),
            css: model_value
                .get("css")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            model_json: serde_json::to_string(model_value)?,
        });
    }

    let mut note_lookup = HashMap::new();
    let mut note_stmt = conn.prepare("SELECT id, guid, mid, tags, flds FROM notes ORDER BY id")?;
    let note_rows = note_stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
        ))
    })?;

    let mut note_count = 0i64;
    for note_row in note_rows {
        let (original_note_id, guid, original_model_id, tags, fields_raw) = note_row?;
        let fields = fields_raw
            .split('\u{1f}')
            .map(normalize_text)
            .collect::<Vec<_>>();
        let fields_json = serde_json::to_string(&fields)?;
        let fields_joined = fields.join(" | ");
        note_lookup.insert(
            original_note_id,
            (
                guid,
                original_model_id,
                tags.trim().to_string(),
                fields_json,
                fields_joined,
            ),
        );
        note_count += 1;
    }

    let mut dense_points = Vec::new();
    let mut card_count = 0i64;
    let mut card_stmt = conn.prepare("SELECT id, nid, did, ord FROM cards ORDER BY id")?;
    let card_rows = card_stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
        ))
    })?;
    for row in card_rows {
        let (original_card_id, original_note_id, original_deck_id, ord) = row?;
        let Some((guid, original_model_id, tags, fields_json, fields_joined)) =
            note_lookup.get(&original_note_id)
        else {
            continue;
        };
        let deck_name = deck_names
            .get(&original_deck_id)
            .cloned()
            .unwrap_or_else(|| "Imported deck".into());
        let card_id = state.db.next_id();
        let note_hash = hash_text(&format!("{guid}:{ord}"));

        for (chunk_index, chunk) in chunk_text(fields_joined, 400, 60).into_iter().enumerate() {
            let payload = CardRecord {
                point_id: state.db.next_id(),
                import_id: deck_id,
                card_id,
                original_note_id,
                original_card_id,
                original_deck_id,
                original_model_id: *original_model_id,
                deck_name: deck_name.clone(),
                guid: guid.clone(),
                ord,
                chunk_index: chunk_index as i64,
                note_hash: note_hash.clone(),
                card_text_clean: fields_joined.clone(),
                fields_json: fields_json.clone(),
                fields_joined: fields_joined.clone(),
                tags: tags.clone(),
                chunk_text: chunk.clone(),
            };
            dense_points.push((payload, format!("passage: {chunk}")));
        }
        card_count += 1;
    }

    let warning = embed_card_points(&state, dense_points)?;

    if media_map_path.exists() {
        let media_json = fs::read_to_string(&media_map_path)?;
        let media_map: HashMap<String, String> = serde_json::from_str(&media_json)?;
        let media_dir = extract_dir.join("media");
        fs::create_dir_all(&media_dir)?;
        for (archive_name, media_name) in media_map {
            let source = temp.path().join(safe_relative_path(&archive_name)?);
            if !source.exists() {
                continue;
            }
            let target = media_dir.join(safe_relative_path(&media_name)?);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&source, &target)?;
        }
    }

    let mut record = state
        .db
        .get_deck_record(deck_id)?
        .ok_or_else(|| anyhow!("deck {deck_id} missing"))?;
    record.status = "ready".into();
    record.warning = warning;
    record.error = None;
    record.note_count = note_count;
    record.card_count = card_count;
    record.models = models;
    state.db.replace_deck(&record)?;
    Ok(())
}

fn parse_name_map(value: &Value) -> HashMap<i64, String> {
    value
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(id, item)| {
            let parsed_id = id.parse::<i64>().ok()?;
            let name = item.get("name")?.as_str()?.to_string();
            Some((parsed_id, name))
        })
        .collect()
}

fn safe_relative_path(raw: &str) -> Result<PathBuf> {
    let normalized = raw.replace('\\', "/");
    if normalized.len() >= 2
        && normalized.as_bytes()[1] == b':'
        && normalized.as_bytes()[0].is_ascii_alphabetic()
    {
        return Err(anyhow!("unsafe relative path: {raw}"));
    }
    let path = Path::new(&normalized);
    let mut safe = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => safe.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(anyhow!("unsafe relative path: {raw}"));
            }
        }
    }
    if safe.as_os_str().is_empty() {
        return Err(anyhow!("empty relative path"));
    }
    Ok(safe)
}

fn embed_card_points(
    state: &AppState,
    payloads: Vec<(CardRecord, String)>,
) -> Result<Option<String>> {
    if payloads.is_empty() {
        return Ok(None);
    }
    let runtime = tokio::runtime::Handle::current();
    let inputs = payloads
        .iter()
        .map(|(_, text)| text.clone())
        .collect::<Vec<_>>();
    let embeddings = runtime.block_on(state.services.external.embed(&inputs));
    let (dense_points, warning) = match embeddings {
        Ok(vectors) => (
            vectors
                .into_iter()
                .zip(payloads)
                .map(|(vector, (payload, _))| DenseCardPoint {
                    point_id: payload.point_id,
                    vector,
                    payload,
                })
                .collect::<Vec<_>>(),
            None,
        ),
        Err(error) => (
            payloads
                .into_iter()
                .map(|(payload, _)| DenseCardPoint {
                    point_id: payload.point_id,
                    vector: vec![0.0; state.config.embedding_vector_size],
                    payload,
                })
                .collect::<Vec<_>>(),
            Some(format!(
                "Dense indexing unavailable, stored lexical-only records: {error}"
            )),
        ),
    };
    state.db.save_card_points(dense_points)?;
    Ok(warning)
}

#[cfg(test)]
mod tests {
    use super::safe_relative_path;
    use std::path::PathBuf;

    #[test]
    fn accepts_normal_relative_paths() {
        assert_eq!(
            safe_relative_path("media/image.png").unwrap(),
            PathBuf::from("media/image.png")
        );
        assert_eq!(
            safe_relative_path(r"nested\cards\sound.mp3").unwrap(),
            PathBuf::from("nested/cards/sound.mp3")
        );
    }

    #[test]
    fn rejects_escape_attempts() {
        assert!(safe_relative_path("../secret").is_err());
        assert!(safe_relative_path("/absolute/path").is_err());
        assert!(safe_relative_path(r"C:\windows\system32").is_err());
        assert!(safe_relative_path("./").is_err());
    }
}
