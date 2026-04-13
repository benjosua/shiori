set shell := ["bash", "-euo", "pipefail", "-c"]
set dotenv-load := true

default:
  @just --list

fmt:
  cargo fmt --all

fmt-check:
  cargo fmt --all -- --check

check:
  cargo check --all-targets

test:
  cargo test --all-targets

fixture path="data/examples/imci-fixture.apkg":
  cargo run --bin make_fixture_apkg -- {{path}}

run:
  cargo run --bin shiori

tei-shim:
  cargo run --bin tei_shim

qdrant-up:
  #!/usr/bin/env bash
  container="shiori-qdrant"
  if curl -fsS "http://127.0.0.1:6333/collections" >/dev/null; then
    echo "Qdrant already reachable at http://127.0.0.1:6333"
    exit 0
  fi
  if docker container inspect "$container" >/dev/null 2>&1; then
    docker start "$container" >/dev/null
  else
    docker run -d --name "$container" -p 6333:6333 qdrant/qdrant >/dev/null
  fi
  until curl -fsS "http://127.0.0.1:6333/collections" >/dev/null; do
    sleep 1
  done
  echo "Qdrant ready at http://127.0.0.1:6333"

qdrant-down:
  @docker rm -f shiori-qdrant >/dev/null 2>&1 || true

e2e:
  #!/usr/bin/env bash
  container="shiori-qdrant-e2e"
  http_port="6335"
  grpc_port="6336"
  cleanup() {
    docker rm -f "$container" >/dev/null 2>&1 || true
  }
  trap cleanup EXIT
  docker rm -f "$container" >/dev/null 2>&1 || true
  docker run -d --name "$container" \
    -p "$http_port:$http_port" \
    -e QDRANT__SERVICE__HTTP_PORT="$http_port" \
    -e QDRANT__SERVICE__GRPC_PORT="$grpc_port" \
    qdrant/qdrant >/dev/null
  until curl -fsS "http://127.0.0.1:$http_port/collections" >/dev/null; do
    sleep 1
  done
  QDRANT_URL="http://127.0.0.1:$http_port" cargo test --test e2e end_to_end_search_flow -- --ignored --nocapture

ci:
  just fmt-check
  just check
  just test
  just e2e
