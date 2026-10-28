#!/usr/bin/env bash
set -euo pipefail
# Run from frontend. Preserve the test runner used by each existing test file.
test_list=$(mktemp)
trap 'rm -f "$test_list"' EXIT
find tests -type f \( -name '*.test.mjs' -o -name '*.test.js' -o -name '*.test.ts' -o -name '*.test.tsx' \) -print0 > "$test_list"
if [[ ! -s "$test_list" ]]; then
  echo "No frontend test files found." >&2
  exit 1
fi
while IFS= read -r -d '' test_file; do
  if grep -q 'bun:test' "$test_file"; then
    bun test "./$test_file"
  else
    node --test "$test_file"
  fi
done < "$test_list"
