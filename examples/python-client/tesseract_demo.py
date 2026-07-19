# SPDX-License-Identifier: AGPL-3.0-only
# SPDX-FileCopyrightText: 2026 Tesseract Contributors
#
# Tesseract Python Demo
# ======================
#
# Prerequisites: pip install requests
# Usage:         python tesseract_demo.py
#
# Make sure Tesseract is running (see docker-compose.yml at the project root).

import json
import sys
import time

import requests

TESSERACT_URL = "http://localhost:3000"


def wait_for_tesseract(url: str, timeout: int = 30) -> None:
    """Block until Tesseract's /health endpoint responds OK."""
    for i in range(timeout):
        try:
            r = requests.get(f"{url}/health", timeout=2)
            if r.status_code == 200:
                print("✅ Tesseract is ready!")
                return
        except requests.ConnectionError:
            pass
        print(f"⏳ Waiting for Tesseract... ({i + 1}s)", end="\r")
        sys.stdout.flush()
        time.sleep(1)
    raise RuntimeError("Tesseract did not become ready in time")


def insert_vector(url: str, id: int, vector: list[float], metadata: dict | None = None) -> int:
    """Insert a vector into Tesseract. Returns the inserted ID."""
    payload = {"id": id, "vector": vector}
    if metadata:
        payload["metadata"] = metadata

    r = requests.post(f"{url}/insert", json=payload, timeout=5)
    r.raise_for_status()
    data = r.json()

    if data.get("success"):
        print(f"   Inserted id={data['id']}")
        return data["id"]
    else:
        raise RuntimeError(f"Insert failed: {data.get('error', 'unknown')}")


def query_vql(url: str, vql: str) -> list[dict]:
    """Execute a VQL query and return the list of results."""
    r = requests.post(f"{url}/query", json={"vql": vql}, timeout=10)
    r.raise_for_status()
    data = r.json()

    if data.get("success"):
        return data["results"]
    else:
        raise RuntimeError(f"Query failed: {data.get('error', 'unknown')}")


def pretty_print(results: list[dict]) -> None:
    """Print query results in a readable format."""
    if not results:
        print("   (no results)")
        return
    for r in results:
        title = r.get("metadata", {}).get("title", "N/A")
        print(f"   id={r['id']:<4}  score={r['score']:.4f}  title={title}")


def main():
    print("=" * 56)
    print("  Tesseract Python Demo — Insert & Semantic Search")
    print("=" * 56)

    # ── Wait for server ─────────────────────────────────────────────
    wait_for_tesseract(TESSERACT_URL)

    # ── Insert sample data ──────────────────────────────────────────
    print("\n📥 Inserting sample vectors...")
    documents = [
        (1, [0.1, 0.2, 0.3], {"title": "The quick brown fox"}),
        (2, [0.4, 0.5, 0.6], {"title": "Jumps over the lazy dog"}),
        (3, [0.7, 0.8, 0.9], {"title": "Vector databases are powerful"}),
        (4, [0.15, 0.25, 0.35], {"title": "Semantic search with Tesseract"}),
    ]

    for id_, vector, metadata in documents:
        insert_vector(TESSERACT_URL, id_, vector, metadata)

    # ── Similarity search ───────────────────────────────────────────
    print("\n🔎 Finding vectors similar to [0.12, 0.22, 0.32]...")
    vql = "FIND SIMILARITY(emb, [0.12, 0.22, 0.32]) LIMIT 3"
    results = query_vql(TESSERACT_URL, vql)
    print(f"   Found {len(results)} result(s):")
    pretty_print(results)

    # ── Hybrid query ────────────────────────────────────────────────
    print("\n🔎 Hybrid query: similar to [0.1, 0.2, 0.3] with source filter...")
    hybrid_vql = (
        'FIND SIMILARITY(emb, [0.1, 0.2, 0.3]) '
        'WHERE metadata->>\'source\' = \'quickstart\' '
        'LIMIT 5'
    )
    hybrid_results = query_vql(TESSERACT_URL, hybrid_vql)
    print(f"   Found {len(hybrid_results)} result(s):")
    pretty_print(hybrid_results)

    print("\n🎉 Demo complete!")


if __name__ == "__main__":
    main()
