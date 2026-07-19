#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
# SPDX-FileCopyrightText: 2026 Tesseract Contributors
#
# Tesseract Quickstart — insert sample vectors and run a similarity search.
#
# Prerequisites:
#   1. Docker and Docker Compose (or a running Tesseract server on localhost:3000)
#   2. curl and jq installed
#
# Usage:
#   chmod +x quickstart.sh
#   ./quickstart.sh

set -euo pipefail

BASE_URL="${TESSERACT_URL:-http://localhost:3000}"
echo "🔍 Using Tesseract at: $BASE_URL"

# ── Step 1: Health check ────────────────────────────────────────────────
echo "⏳ Waiting for Tesseract to be ready..."
for i in $(seq 1 30); do
  if curl -sf "$BASE_URL/health" > /dev/null 2>&1; then
    echo "✅ Tesseract is ready!"
    break
  fi
  if [ "$i" -eq 30 ]; then
    echo "❌ Timed out waiting for Tesseract. Is it running?"
    exit 1
  fi
  sleep 1
done

# ── Step 2: Insert sample vectors ───────────────────────────────────────
echo ""
echo "📥 Inserting sample vectors..."

insert() {
  local id=$1 vector=$2 title=$3
  local body
  body=$(cat <<EOF
{
  "id": $id,
  "vector": [$vector],
  "metadata": {"title": "$title", "source": "quickstart"}
}
EOF
)
  local status
  status=$(curl -s -o /dev/null -w "%{http_code}" -X POST "$BASE_URL/insert" \
    -H "Content-Type: application/json" -d "$body")
  echo "   Insert id=$id → HTTP $status"
}

insert 1 "0.1, 0.2, 0.3" "The quick brown fox"
insert 2 "0.4, 0.5, 0.6" "Jumps over the lazy dog"
insert 3 "0.7, 0.8, 0.9" "Vector databases are powerful"
insert 4 "0.15, 0.25, 0.35" "Semantic search with Tesseract"

# ── Step 3: Run a similarity query ──────────────────────────────────────
echo ""
echo "🔎 Querying for vectors similar to [0.12, 0.22, 0.32]..."

QUERY_RESULT=$(curl -sf -X POST "$BASE_URL/query" \
  -H "Content-Type: application/json" \
  -d '{"vql": "FIND SIMILARITY(emb, [0.12, 0.22, 0.32]) LIMIT 3"}')

echo ""
echo "$QUERY_RESULT" | jq -r '
  if .success then
    "✅ Query succeeded — \(.results | length) result(s):",
    (.results[] | "   id=\(.id)  score=\(.score | tostring | .[0:6])  title=\(.metadata.title // "N/A")")
  else
    "❌ Query failed: \(.error // "unknown error")"
  end
'

# ── Step 4: Pagination example ──────────────────────────────────────────
echo ""
echo "📄 Pagination example (LIMIT 2 OFFSET 1)..."

PAGE_RESULT=$(curl -sf -X POST "$BASE_URL/query" \
  -H "Content-Type: application/json" \
  -d '{"vql": "FIND SIMILARITY(emb, [0.12, 0.22, 0.32]) LIMIT 2 OFFSET 1"}')

echo ""
echo "$PAGE_RESULT" | jq -r '
  if .success then
    "✅ Page returned \(.results | length) result(s) (total: \(.total)):",
    (.results[] | "   id=\(.id)  score=\(.score | tostring | .[0:6])  title=\(.metadata.title // "N/A")")
  else
    "❌ Query failed: \(.error // "unknown error")"
  end
'

echo ""
echo "🎉 Quickstart complete!"
