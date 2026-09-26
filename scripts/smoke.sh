#!/usr/bin/env bash
# End-to-end smoke test: SQLite + local store in a temp dir.
# Usage: cargo build --workspace && scripts/smoke.sh
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
B=$ROOT/target/debug
S=$(mktemp -d)
trap 'kill ${SP:-0} 2>/dev/null || true; rm -rf "$S"' EXIT
cd "$S"
mkdir -p data/part rawdir

export DATABASE_URL="sqlite://$S/ds.db?mode=rwc" DS_STORE=$S/store DS_BIND=127.0.0.1:18080
export XDG_CACHE_HOME=$S/cache RUST_LOG=warn DS_URL=http://127.0.0.1:18080
# (DS_BIND above; `dataset-server --port N` also works)

DS_PASSWORD=secretpw "$B/dataset-server" useradd admin
"$B/dataset-server" > server.log 2>&1 & SP=$!
sleep 1
eval "$(DS_PASSWORD=secretpw "$B/ds" login --username admin)"

head -c 80000000 /dev/urandom > data/part/big.bin   # > 32 MB: exercises multipart
printf 'a,b\n1,2\n' > data/train.csv
echo hello > data/readme.txt
echo raw > rawdir/r.txt

"$B/ds" dataset create raw > /dev/null
"$B/ds" dataset create multicredit > /dev/null
"$B/ds" version create raw --from rawdir > /dev/null

echo "== v1 (everything uploaded)"
RUST_LOG=info "$B/ds" version create multicredit --from data --branch training \
  --producer pipeline:run-1 --input raw@latest --meta rows=3347976 --meta format=parquet 2>&1 | grep uploading

printf 'a,b\n1,2\n3,4\n' > data/train.csv
echo "== v2 (only train.csv uploaded)"
RUST_LOG=info "$B/ds" version create multicredit --from data --parent training --branch training \
  --meta rows=3401120 2>&1 | grep uploading

V1=$("$B/ds" version list multicredit | python3 -c 'import json,sys; print(json.load(sys.stdin)[-1]["id"])')
"$B/ds" tag multicredit v1-frozen "$V1" > /dev/null

echo "== diff v1-frozen → training"
"$B/ds" diff multicredit v1-frozen training | python3 -c '
import json, sys
d = json.load(sys.stdin); s = d["storage"]
print("changed:", [c["path"] for c in s["changed"]], "unchanged:", s["unchanged"],
      "new_blob_bytes:", s["new_blob_bytes"], "metadata:", d["structure"]["metadata_changes"])'

echo "== pull"
"$B/ds" pull multicredit@training --to out > /dev/null && diff -r data out && echo "identical"

echo "== resumed pull"
head -c 1000000 out/part/big.bin > out/part/big.bin.part && rm out/part/big.bin
"$B/ds" pull multicredit@training --to out > /dev/null && cmp data/part/big.bin out/part/big.bin && echo "resumed ok"

echo "== lineage of v1"
"$B/ds" lineage "multicredit@$V1" | python3 -c 'import json,sys; print(json.load(sys.stdin)["nodes"])'

echo "== range read"
curl -sf -H "Authorization: Bearer $DS_TOKEN" -H "Range: bytes=0-4" "$DS_URL/api/datasets/multicredit/versions/training/files/readme.txt"; echo

echo "== unauthenticated read (expect 401)"
curl -s -o /dev/null -w '%{http_code}\n' "$DS_URL/api/datasets"

echo "== web UI served (expect 200 200)"
curl -s -o /dev/null -w '%{http_code} ' "$DS_URL/"; curl -s -o /dev/null -w '%{http_code}\n' "$DS_URL/assets/app.js"

echo "== browser session: cookie write without CSRF header (expect 403), with header (expect 201)"
curl -s -c jar -H 'content-type: application/json' -d '{"username":"admin","password":"secretpw"}' "$DS_URL/api/auth/login" > /dev/null
curl -s -o /dev/null -w '%{http_code} ' -b jar -H 'content-type: application/json' -d '{"name":"x1"}' "$DS_URL/api/datasets"
curl -s -o /dev/null -w '%{http_code}\n' -b jar -H 'x-ds-csrf: 1' -H 'content-type: application/json' -d '{"name":"x2"}' "$DS_URL/api/datasets"

echo "== gc dry run (expect 0 candidates)"
"$B/ds" gc --min-age-hours 0
