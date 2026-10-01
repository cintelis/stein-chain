#!/usr/bin/env bash
# Tracked text files must not contain control characters other than tab, line
# feed and carriage return.
#
# They get in by accident — an escape sequence taken literally on the way
# through a shell or a script: "\1" becoming byte 0x01 in a sed expression, "\b"
# becoming a backspace in a path — and they are invisible in most editors and
# in review. In a workflow file, a script or a configuration file they change
# what runs. This check (from Sigilant, where it caught exactly that more than once) is
# what stops it reaching main.
#
# git grep, in the C locale, with the characters themselves in a bracket
# expression: no PCRE (which not every grep has in every locale), and an exit
# status that tells "found" (0) from "none" (1) from "could not look" (other).
# The last must fail the check too: a check that cannot run has not passed.
#
# With --staged it looks at what is about to be committed, for the pre-commit
# hook; otherwise at the tracked files as they are, for CI.

set -uo pipefail
cd "$(dirname "$0")/.."

where=()
[ "${1:-}" = "--staged" ] && where=(--cached)
pattern=$(printf '[\001-\010\013\014\016-\037\177]')
found=$(LC_ALL=C git grep "${where[@]}" -I -l -e "$pattern")
status=$?
case $status in
  0)
    echo "text-files: control characters in:" >&2
    echo "$found" | sed 's/^/  /' >&2
    echo "text-files: show them with: cat -A <file> | grep -n '\\^[A-Z@[\\\\^_]'" >&2
    exit 1
    ;;
  1)
    echo "text-files: ok ($(git ls-files | wc -l | tr -d ' ') tracked files, no control characters)"
    ;;
  *)
    echo "text-files: could not search the tracked files (git grep exited $status)" >&2
    exit 1
    ;;
esac
