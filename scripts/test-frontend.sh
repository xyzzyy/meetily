#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/../frontend"

test_list=$(mktemp)
trap 'rm -f "$test_list"' EXIT

if ! find tests -type f \( -name '*.test.mjs' -o -name '*.test.js' -o -name '*.test.ts' -o -name '*.test.tsx' \) -print0 > "$test_list"; then
  echo "Failed to discover frontend tests." >&2
  exit 1
fi
if [[ ! -s "$test_list" ]]; then
  echo "No frontend test files found." >&2
  exit 1
fi

# Each file runs in a separate process. This keeps module mocks in one test
# from changing imports in another test. Use the runner each file imports.
while IFS= read -r -d '' test_file; do
  if grep -q 'bun:test' "$test_file"; then
    bun test "./$test_file"
  else
    node --test "$test_file"
  fi
done < "$test_list"
