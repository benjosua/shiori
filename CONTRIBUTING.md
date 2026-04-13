# Contributing

Thanks for your interest in Shiori.

## Development Setup

1. Install Rust `1.85` or newer.
2. Install [`just`](https://github.com/casey/just) if you want the shortcut commands.
3. Start Qdrant locally with `just qdrant-up`, your own Docker command, or a separate Qdrant install.
4. Optionally start a TEI-compatible embedding service, or run the included `tei_shim`.
5. Run the app with `cargo run --bin shiori` or `just run`.

If you do not run a TEI-compatible service, the app still works with lexical-only search.

If you want to test Office document ingest, install `unoconvert` and its LibreOffice or unoserver runtime.

## Reporting Bugs

- Use the bug report issue template when the problem is reproducible
- Include the operating system, Rust version, and Qdrant version
- Include logs, stack traces, or screenshots when they help
- For security issues, follow [SECURITY.md](SECURITY.md) instead of opening a public issue with exploit details

## Before Opening a Pull Request

Please run:

```bash
cargo fmt
cargo check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

Also run `just e2e` when your change affects ingest, search, export, Qdrant integration, or request handling.

If your change affects ingest, search, export behavior, or developer setup, include a short note describing how you tested it and update the relevant docs.

## Pull Request Notes

- Keep changes focused.
- Document new environment variables in `.env.example` and `README.md`.
- Avoid committing local data, model files, or generated artifacts.
- Keep local secrets in an untracked `.env` or `.env.local` file instead of editing `.env.example`.
- Add or update tests when you change behavior.
- Call out breaking changes or migration steps clearly in the pull request description.
