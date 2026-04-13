# Shiori

Shiori is a local-first search workbench for Anki decks and study materials. It imports `.apkg` decks, extracts text from notes and uploaded documents, indexes everything in Qdrant, and helps you search across your material before exporting a curated deck.

## Features

- Import `.apkg` files and index note content, deck names, tags, and media metadata
- Upload `txt`, `pdf`, `doc`, `docx`, `ppt`, and `pptx` materials or paste raw text
- Combine dense retrieval with lexical fallback for resilient search
- Optionally rerank matches through a TEI-compatible embedding/rerank service
- Collect matching cards into a cart and export them as a fresh `.apkg`

## Stack

- Backend: `axum`, `askama`, `tokio`
- Vector store: Qdrant
- Search pipeline: lexical search plus dense retrieval
- Export: `genanki-rs`
- Office conversion: `unoconvert`

## Project Status

Shiori is currently an early-stage project. The core ingest, search, and export workflow is usable, but the interfaces and data model may still evolve. Expect small breaking changes while the project settles.

## Prerequisites

- Rust `1.85` or newer
- Qdrant `v1.8+`
- Docker, if you want to use `just qdrant-up`
- `just`, if you want the shortcut commands from the `justfile`
- `unoconvert` plus LibreOffice or unoserver, only if you need `doc`, `docx`, `ppt`, or `pptx` ingest
- An embedding and reranking service if you want dense retrieval or reranking

Shiori still works without a TEI-compatible service, but searches fall back to lexical retrieval only.

## Quickstart

1. Install Rust and `just`:

```bash
rustup toolchain install stable
cargo install just
```

2. Start Qdrant:

```bash
just qdrant-up
```

3. Optionally start a TEI-compatible service for embeddings and reranking.

Using `text-embeddings-inference`:

```bash
text-embeddings-router \
  --model-id intfloat/multilingual-e5-large-instruct \
  --port 8080
```

Or use the included Ollama-backed shim:

```bash
EMBEDDING_MODEL=nomic-embed-text:latest \
LLM_MODEL=qwen2.5:7b-instruct \
just tei-shim
```

If you skip this step, Shiori will still ingest and search with lexical fallback.

4. Run Shiori:

```bash
just run
```

5. Open [http://127.0.0.1:3000](http://127.0.0.1:3000)

To run without `just`:

```bash
cargo run --bin shiori
```

## Configuration

See [.env.example](.env.example) for a full set of environment variables.

Core variables:

- `APP_HOST` default: `127.0.0.1`
- `APP_PORT` default: `3000`
- `APP_DATA_DIR` default: `./data`
- `QDRANT_URL` default: `http://127.0.0.1:6333`
- `QDRANT_COLLECTION` default: `shiori_card_chunks`
- `TEI_URL` default: `http://127.0.0.1:8080`
- `UNOCONVERT_BIN` default: `unoconvert`
- `RERANK_STRATEGY` default: `best_chunk`
- `EMBEDDING_VECTOR_SIZE` default: `1024`
- `EMBEDDING_MODEL` default: `nomic-embed-text:latest` in `tei_shim`
- `LLM_MODEL` default: unset, which disables LLM reranking in `tei_shim`

## Data and Privacy

- Shiori stores imported decks, extracted material text, generated exports, and temporary working files under `APP_DATA_DIR`
- The application does not include analytics or telemetry
- Qdrant persists indexed data in its own storage location
- Review the material you import before sharing a demo instance with anyone else

## Development

Common local checks:

```bash
just fmt
just check
cargo clippy --all-targets --all-features -- -D warnings
just test
just e2e
```

Useful recipes:

- `just fixture` creates a sample `.apkg` deck at `data/examples/imci-fixture.apkg`
- `just qdrant-down` stops the local development Qdrant container
- `just ci` runs format, compile, non-e2e tests, and the full end-to-end test
- GitHub Actions runs formatting, build, lint, and test checks on pushes and pull requests

Before opening a public pull request, read [CONTRIBUTING.md](CONTRIBUTING.md).

## Limitations

- OCR is not implemented. Image-only PDFs will not extract useful text.
- Export rebuilds a simplified deck instead of restoring the original note model byte-for-byte.
- Dense retrieval depends on a compatible embedding service. Without one, lexical-only indexing is used.

## Open Source

Contribution and support guides live in [CONTRIBUTING.md](CONTRIBUTING.md), [SECURITY.md](SECURITY.md), [SUPPORT.md](SUPPORT.md), and [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md).

## Security

Please do not post exploit details in public issues. See [SECURITY.md](SECURITY.md) for the current disclosure policy.
