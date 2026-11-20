#!/usr/bin/env bash
set -euo pipefail

case "$#:${1:-}" in
  0:|1:--check) ;;
  *) echo "Usage: $0 [--check]" >&2; exit 2 ;;
esac
cd "$(dirname "$0")/.."
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

help_text() {
  cargo run --quiet --locked --bin stdo -- "$@" --help
}

help_text > "$work/help"
awk '/^Commands:$/ { listing=1; next } listing && /^$/ { exit }
     listing && $1 != "help" { print $1 }' "$work/help" > "$work/commands"
test -s "$work/commands"
{
  printf '\n```text\n'
  cat "$work/help"
  printf '```\n'
  while IFS= read -r command; do
    printf '\n### `stdo %s`\n\n```text\n' "$command"
    help_text "$command"
    printf '```\n'
  done < "$work/commands"
  printf '\n'
} > "$work/reference"

awk -v block="$work/reference" '
  /^<!-- BEGIN_CLI_REFERENCE -->$/ {
    if (begun++) exit 2
    print
    while ((getline line < block) > 0) print line
    next
  }
  /^<!-- END_CLI_REFERENCE -->$/ { if (!begun || ended++) exit 2 }
  !begun || ended { print }
  END { if (begun != 1 || ended != 1) exit 2 }
' README.md > "$work/readme"
if [[ ${1:-} == --check ]]; then
  diff -u README.md "$work/readme"
elif ! cmp -s README.md "$work/readme"; then
  cat "$work/readme" > README.md
fi
