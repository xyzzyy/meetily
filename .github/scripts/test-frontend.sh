#!/usr/bin/env bash
set -euo pipefail
# Run from frontend. Preserve per-file process isolation for each test runner.
test_list=$(mktemp)
trap 'rm -f "$test_list"' EXIT
find tests -type f \( -name '*.test.mjs' -o -name '*.test.js' -o -name '*.test.ts' -o -name '*.test.tsx' \) -print0 \
  | LC_ALL=C sort -z > "$test_list"
if [[ ! -s "$test_list" ]]; then
  echo "No frontend test files found." >&2
  exit 1
fi

bun_files=()
node_files=()
while IFS= read -r -d '' test_file; do
  if grep -q 'bun:test' "$test_file"; then
    bun_files+=("./$test_file")
  else
    node_files+=("$test_file")
  fi
done < "$test_list"

status=0
if (( ${#node_files[@]} > 0 )); then
  node --test "${node_files[@]}" || status=1
fi
for test_file in "${bun_files[@]}"; do
  bun test "$test_file" || status=1
done
exit "$status"
