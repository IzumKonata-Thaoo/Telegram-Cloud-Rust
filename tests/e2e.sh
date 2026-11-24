#!/usr/bin/env bash
# End-to-end + security regression suite against a throwaway server (local backend).
# Usage: bash tests/e2e.sh [path-to-tgvault-binary]
set -u
BIN="${1:-./target/debug/tgvault}"
BIN="$(cd "$(dirname "$BIN")" && pwd)/$(basename "$BIN")"
WORK="$(mktemp -d)"
PORT=18799
B="http://127.0.0.1:$PORT"
PASS=0; FAIL=0
ok()   { PASS=$((PASS+1)); printf '  ok   %s\n' "$1"; }
bad()  { FAIL=$((FAIL+1)); printf '  FAIL %s (got %s)\n' "$1" "$2"; }
expect() { [ "$2" = "$3" ] && ok "$1" || bad "$1" "$2"; }   # name, got, want
code() { curl -s -o /dev/null -w '%{http_code}' "$@"; }
j() { python -c "import sys,json;d=json.load(sys.stdin);print(eval('d'+sys.argv[1]))" "$1"; }

export TGV_BACKEND=local TGV_DATA_DIR="$WORK/data" TGV_BIND="127.0.0.1:$PORT" TGV_CHUNK_MIB=1 TGV_BACKUP_INTERVAL_HOURS=0
export TGV_MASTER_KEYS="1:$(openssl rand -base64 32)" TGV_SERVER_SECRET="$(openssl rand -base64 32)" TGV_ADMIN_PASSWORD="e2e-admin-password-123"
"$BIN" >"$WORK/server.log" 2>&1 &
SRV=$!
trap 'kill $SRV 2>/dev/null; wait $SRV 2>/dev/null; sleep 0.5; rm -rf "$WORK"' EXIT
for _ in $(seq 1 50); do curl -s "$B/healthz" >/dev/null && break; sleep 0.1; done
cd "$WORK"

echo "== session & CSRF"
CSRF=$(curl -s -c jar -H 'content-type: application/json' -d "{\"password\":\"$TGV_ADMIN_PASSWORD\"}" $B/api/v1/auth/login | j "['csrf']")
expect "wrong admin password" "$(code -H 'content-type: application/json' -d '{"password":"nope"}' $B/api/v1/auth/login)" 401
expect "cookie mutation without CSRF" "$(code -b jar -H 'content-type: application/json' -d '{"name":"media"}' $B/api/v1/buckets)" 403
S=(-b jar -H "x-csrf-token: $CSRF" -H 'content-type: application/json')
expect "create bucket" "$(code "${S[@]}" -d '{"name":"media","cors_origins":["https://site.example"]}' $B/api/v1/buckets)" 201
expect "create private bucket" "$(code "${S[@]}" -d '{"name":"private","quota_bytes":3000000}' $B/api/v1/buckets)" 201
KEY=$(curl -s "${S[@]}" -d '{"name":"web","scopes":["read","write","list","share","delete"],"buckets":["media"]}' $B/api/v1/keys | j "['key']")
WKEY=$(curl -s "${S[@]}" -d '{"name":"writer","scopes":["write"],"buckets":["media"]}' $B/api/v1/keys | j "['key']")
A=(-H "authorization: Bearer $KEY"); J=(-H 'content-type: application/json')

echo "== upload / stream / range"
head -c 3145728 /dev/urandom > big.bin; head -c 777 /dev/urandom >> big.bin
SHA=$(sha256sum big.bin | cut -c1-64)
expect "PUT object" "$(code "${A[@]}" -H 'content-type: video/mp4' -T big.bin "$B/api/v1/buckets/media/objects/v/clip%20one.mp4")" 201
expect "GET equals upload" "$(curl -s "${A[@]}" "$B/api/v1/buckets/media/objects/v/clip%20one.mp4" | sha256sum | cut -c1-64)" "$SHA"
GOT=$(curl -s "${A[@]}" -H 'range: bytes=1048570-1048585' "$B/api/v1/buckets/media/objects/v/clip%20one.mp4" | xxd -p)
WANT=$(dd if=big.bin bs=1 skip=1048570 count=16 2>/dev/null | xxd -p)
expect "range across chunk boundary" "$GOT" "$WANT"
expect "suffix range" "$(code "${A[@]}" -H 'range: bytes=-10' "$B/api/v1/buckets/media/objects/v/clip%20one.mp4")" 206
expect "unsatisfiable range" "$(code "${A[@]}" -H 'range: bytes=99999999-' "$B/api/v1/buckets/media/objects/v/clip%20one.mp4")" 416
ETAG=$(curl -s -D - -o /dev/null "${A[@]}" "$B/api/v1/buckets/media/objects/v/clip%20one.mp4" | grep -i '^etag' | cut -d' ' -f2 | tr -d '\r')
expect "If-None-Match -> 304" "$(code "${A[@]}" -H "if-none-match: $ETAG" "$B/api/v1/buckets/media/objects/v/clip%20one.mp4")" 304
printf 'PLAINTEXT-MARKER%.0s' $(seq 1 3000) > t.txt
curl -s -o /dev/null "${A[@]}" -T t.txt "$B/api/v1/buckets/media/objects/t.txt"
expect "no plaintext at rest" "$(grep -l PLAINTEXT-MARKER data/chunks/* 2>/dev/null | wc -l | tr -d ' ')" 0

echo "== multipart"
split -b 1200000 big.bin part_
UP=$(curl -s "${A[@]}" "${J[@]}" -d '{"key":"v/mp.mp4"}' $B/api/v1/buckets/media/uploads | j "['upload_id']")
curl -s -o /dev/null "${A[@]}" -T part_ac $B/api/v1/uploads/$UP/parts/3
curl -s -o /dev/null "${A[@]}" -T part_aa $B/api/v1/uploads/$UP/parts/2
curl -s -o /dev/null "${A[@]}" -T part_aa $B/api/v1/uploads/$UP/parts/1
curl -s -o /dev/null "${A[@]}" -T part_ab $B/api/v1/uploads/$UP/parts/2
expect "complete multipart" "$(code "${A[@]}" -X POST $B/api/v1/uploads/$UP/complete)" 201
expect "multipart content" "$(curl -s "${A[@]}" "$B/api/v1/buckets/media/objects/v/mp.mp4" | sha256sum | cut -c1-64)" "$SHA"
expect "upload of another key not visible to writer" "$(code -H "authorization: Bearer $WKEY" $B/api/v1/uploads/$UP)" 404

echo "== presigned URLs & CORS"
URL=$(curl -s "${A[@]}" "${J[@]}" -d '{"bucket":"media","key":"v/clip one.mp4","expires_in":120}' $B/api/v1/presign | j "['url']")
expect "presigned GET" "$(curl -s "$URL" | sha256sum | cut -c1-64)" "$SHA"
expect "tampered signature" "$(code "${URL%?}0")" 403
expect "signature reused for other key" "$(code "${URL/clip%20one/other}")" 403
expect "CORS allowed origin" "$(curl -s -D - -o /dev/null -H 'origin: https://site.example' "$URL" | grep -ic 'access-control-allow-origin: https://site.example')" 1
expect "CORS foreign origin" "$(curl -s -D - -o /dev/null -H 'origin: https://evil.example' "$URL" | grep -ic access-control-allow-origin)" 0
PURL=$(curl -s "${A[@]}" "${J[@]}" -d '{"bucket":"media","key":"up/a.png","method":"PUT","content_type":"image/png"}' $B/api/v1/presign | j "['url']")
expect "presigned PUT wrong type" "$(code -X PUT -H 'content-type: text/html' --data-binary @t.txt "$PURL")" 403
expect "presigned PUT" "$(code -X PUT -H 'content-type: image/png' --data-binary @t.txt "$PURL")" 201

echo "== share links"
SH=$(curl -s "${A[@]}" "${J[@]}" -d '{"bucket":"media","key":"v/clip one.mp4","password":"pw-123456","max_downloads":1}' $B/api/v1/shares | j "['url']")
expect "locked raw" "$(code "$SH/raw")" 403
expect "wrong password redirects" "$(curl -s -o /dev/null -w '%{redirect_url}' -d 'password=nope' "$SH/unlock" | grep -c 'wrong=1')" 1
curl -s -c sj -o /dev/null -d 'password=pw-123456' "$SH/unlock"
expect "first download" "$(curl -s -b sj -c sj "$SH/raw" | sha256sum | cut -c1-64)" "$SHA"
expect "seek with download ticket" "$(code -b sj -H 'range: bytes=10-20' "$SH/raw")" 206
grep -v tgv_dl_ sj > sj2
expect "max_downloads bypass via Range" "$(code -b sj2 -H 'range: bytes=1-' "$SH/raw")" 404

echo "== authorization"
curl -s -o /dev/null "${S[@]/content-type: application\/json/content-type: text/plain}" -T t.txt "$B/api/v1/buckets/private/objects/secret.txt"
SID=$(curl -s -b jar "$B/api/v1/buckets/private/objects" | j "['objects'][0]['id']")
expect "key outside its bucket" "$(code "${A[@]}" "$B/api/v1/buckets/private/objects/secret.txt")" 403
expect "object id from other bucket" "$(code "${A[@]}" "$B/api/v1/objects/$SID")" 404
expect "copy out of forbidden bucket" "$(code "${A[@]}" "${J[@]}" -d '{"source_bucket":"private","source_key":"secret.txt","dest_key":"x"}' $B/api/v1/buckets/media/copy)" 403
expect "non-admin creates key" "$(code "${A[@]}" "${J[@]}" -d '{"name":"x","scopes":["admin"]}' $B/api/v1/keys)" 403
expect "write-only key hash oracle" "$(code -H "authorization: Bearer $WKEY" "${J[@]}" -d "{\"key\":\"d\",\"sha256\":\"$SHA\"}" $B/api/v1/buckets/media/link-by-hash)" 403
expect "public route on private bucket" "$(code "$B/pub/private/secret.txt")" 404
expect "literal dot segment" "$(code --path-as-is "${A[@]}" -T t.txt "$B/api/v1/buckets/media/objects/a/../b")" 400
expect "control char in key" "$(code "${A[@]}" -T t.txt "$B/api/v1/buckets/media/objects/a%0Ab")" 400

echo "== content safety"
printf '<script>alert(1)</script>' > x.html
curl -s -o /dev/null "${A[@]}" -H 'content-type: text/html' -T x.html "$B/api/v1/buckets/media/objects/x.html"
H=$(curl -s -D - -o /dev/null "${A[@]}" "$B/api/v1/buckets/media/objects/x.html")
expect "html forced to attachment" "$(echo "$H" | grep -ic 'content-disposition: attachment')" 1
expect "sandbox CSP on user content" "$(echo "$H" | grep -ic 'content-security-policy: sandbox')" 1

echo "== quota (atomic at commit)"
head -c 1400000 /dev/urandom > q.bin
QP=()
for i in 1 2 3; do curl -s -o "q$i.out" -b jar -H "x-csrf-token: $CSRF" -T q.bin "$B/api/v1/buckets/private/objects/q$i" & QP+=($!); done
wait "${QP[@]}"   # only the uploads, not the server
USED=$(curl -s -b jar $B/api/v1/buckets/private | j "['bytes']")
[ "$USED" -le 3000000 ] && ok "concurrent uploads stay within quota ($USED bytes)" || bad "quota race" "$USED"

echo "== brute force throttles"
for i in $(seq 1 70); do curl -s -o /dev/null -H 'authorization: Bearer tgv_aaaaaaaaaaaa_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb' $B/api/v1/buckets; done
expect "bad-key flood is rate limited" "$(code -H 'authorization: Bearer tgv_aaaaaaaaaaaa_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb' $B/api/v1/buckets)" 429
expect "valid key still works from the same IP" "$(code "${A[@]}" $B/api/v1/buckets)" 200

echo "== remote import (SSRF guards + a real public download)"
imp() { curl -s -o imp.out -w '%{http_code}' "${A[@]}" "${J[@]}" -d "$1" $B/api/v1/buckets/media/import; }
expect "import loopback refused" "$(imp "{\"url\":\"http://127.0.0.1:$PORT/healthz\",\"wait\":true}")" 400
expect "import cloud metadata refused" "$(imp '{"url":"http://169.254.169.254/latest/meta-data/","wait":true}')" 400
expect "import decimal-encoded loopback refused" "$(imp '{"url":"http://2130706433/","wait":true}')" 400
expect "import IPv4-mapped IPv6 refused" "$(imp '{"url":"http://[::ffff:127.0.0.1]/","wait":true}')" 400
expect "import file:// refused" "$(imp '{"url":"file:///etc/passwd","wait":true}')" 400
expect "import localhost name refused" "$(imp '{"url":"http://localhost/","wait":true}')" 400
expect "import Host header refused" "$(imp '{"url":"https://example.com/","headers":{"Host":"internal"},"wait":true}')" 400
expect "DNS name resolving to 127.0.0.1 refused" "$(imp '{"url":"http://localtest.me/","wait":true}')" 400
grep -q "private or reserved" imp.out && ok "  (refused by the resolver: $(cut -c1-80 imp.out))" || bad "resolver message" "$(cat imp.out)"
SRC="https://raw.githubusercontent.com/rust-lang/rust/master/README.md"
if curl -s -o src.bin -m 20 "$SRC"; then
  expect "import public URL (wait)" "$(imp "{\"url\":\"$SRC\",\"key\":\"imports/readme.md\",\"wait\":true}")" 201
  expect "imported bytes match source" "$(curl -s "${A[@]}" "$B/api/v1/buckets/media/objects/imports/readme.md" | sha256sum | cut -c1-64)" "$(sha256sum src.bin | cut -c1-64)"
  JOB=$(curl -s "${A[@]}" "${J[@]}" -d "{\"url\":\"$SRC\",\"key\":\"imports/async.md\"}" $B/api/v1/buckets/media/import | j "['id']")
  for _ in $(seq 1 40); do ST=$(curl -s "${A[@]}" $B/api/v1/imports/$JOB | j "['state']"); [ "$ST" = done ] || [ "$ST" = failed ] && break; sleep 0.5; done
  expect "async import job finishes" "$ST" done
  expect "other key cannot see the job" "$(code -H "authorization: Bearer $WKEY" $B/api/v1/imports/$JOB)" 404
else
  echo "  skip public import tests (no internet)"
fi

echo "== maintenance"
expect "backup" "$(code -b jar -H "x-csrf-token: $CSRF" -X POST $B/api/v1/admin/backups)" 200
expect "gc" "$(code -b jar -H "x-csrf-token: $CSRF" -X POST $B/api/v1/admin/gc)" 200
BK=$(ls data/backups/tgvault-backup-*.bin | head -1)
"$BIN" decrypt-backup "$BK" restored.db >/dev/null && expect "backup decrypts to SQLite" "$(head -c 15 restored.db)" "SQLite format 3"

echo
echo "passed: $PASS  failed: $FAIL"
[ "$FAIL" -eq 0 ]
