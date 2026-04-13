use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow};

use crate::{AppState, models::MaterialSegment, services::text::normalize_text};

pub async fn ingest_material(
    state: AppState,
    material_id: i64,
    filename: String,
    stored_path: PathBuf,
) -> Result<()> {
    tokio::task::spawn_blocking(move || {
        ingest_material_blocking(state, material_id, &filename, &stored_path)
    })
    .await
    .context("join material ingestion task")?
}

fn ingest_material_blocking(
    state: AppState,
    material_id: i64,
    _filename: &str,
    stored_path: &Path,
) -> Result<()> {
    let extension = stored_path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let working_dir = state.config.materials_dir.join(material_id.to_string());
    fs::create_dir_all(&working_dir)?;

    let (segments, warning) = match extension.as_str() {
        "txt" => {
            let raw = fs::read_to_string(stored_path)?;
            (split_to_segments(&normalize_text(&raw), "Line"), None)
        }
        "pdf" => extract_pdf_segments(stored_path).map(|segments| (segments, None))?,
        "docx" | "pptx" | "doc" | "ppt" => {
            let pdf_path = state
                .services
                .external
                .convert_office_to_pdf(stored_path, &working_dir)?;
            let segments = extract_pdf_segments(&pdf_path)?;
            (segments, Some("Converted through unoconvert".to_string()))
        }
        other => return Err(anyhow!("Unsupported material type: {other}")),
    };

    let source_text = segments
        .iter()
        .map(|segment| segment.text.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");

    let mut record = state
        .db
        .get_material_record(material_id)?
        .ok_or_else(|| anyhow!("material {material_id} not found"))?;
    record.status = "ready".into();
    record.warning = warning;
    record.error = None;
    record.source_text = source_text;
    record.segments = segments;
    state.db.replace_material(&record)?;
    Ok(())
}

fn extract_pdf_segments(path: &Path) -> Result<Vec<MaterialSegment>> {
    let raw = pdf_extract::extract_text(path)
        .with_context(|| format!("extract text from {}", path.display()))?;
    let normalized = raw.replace("\r\n", "\n");
    let page_chunks = normalized
        .split('\u{0c}')
        .map(normalize_text)
        .filter(|page| !page.is_empty())
        .collect::<Vec<_>>();
    if page_chunks.is_empty() {
        return Err(anyhow!(
            "OCR not supported in v1 or the PDF contains no extractable text"
        ));
    }
    Ok(page_chunks
        .into_iter()
        .enumerate()
        .map(|(index, text)| MaterialSegment {
            id: index as i64,
            material_id: 0,
            ordinal: index as i64,
            label: format!("Page {}", index + 1),
            text,
        })
        .collect())
}

fn split_to_segments(text: &str, prefix: &str) -> Vec<MaterialSegment> {
    text.lines()
        .enumerate()
        .filter_map(|(index, line)| {
            let line = line.trim();
            if line.is_empty() {
                None
            } else {
                Some(MaterialSegment {
                    id: index as i64,
                    material_id: 0,
                    ordinal: index as i64,
                    label: format!("{prefix} {}", index + 1),
                    text: line.to_string(),
                })
            }
        })
        .collect()
}
