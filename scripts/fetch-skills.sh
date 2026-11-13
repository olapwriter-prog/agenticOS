#!/bin/sh
# Build the phone's skill library from the catalogs in skills/sources.tsv, at their pinned commits.
#   scripts/fetch-skills.sh [PACK_DIR] [WORK_DIR]     (default: ./skillpack and a temporary dir)
# Each catalog is cloned at its commit, imported (only skills under a license that allows
# redistribution are kept), and a report per catalog is written to PACK_DIR/reports/.
set -eu
cd "$(dirname "$0")/.."
PACK=${1:-$PWD/skillpack}
WORK=${2:-$(mktemp -d)}
cargo build -q --release -p agentos-skillpack
BIN=$PWD/target/release/agentos-skillpack
mkdir -p "$PACK/reports"
grep -v '^#' skills/sources.tsv | while IFS="$(printf '\t')" read -r name repo commit lic evidence; do
	[ -n "$name" ] || continue
	dir="$WORK/$name"
	if [ ! -d "$dir/.git" ]; then
		git init -q "$dir"
		git -C "$dir" remote add origin "$repo"
	fi
	git -C "$dir" fetch -q --depth 1 origin "$commit"
	git -C "$dir" checkout -q FETCH_HEAD
	if [ "$lic" = "-" ]; then
		"$BIN" import "$PACK" "$dir" --source "$name" --repo "$repo" --commit "$commit" >"$PACK/reports/$name.json"
	else
		"$BIN" import "$PACK" "$dir" --source "$name" --repo "$repo" --commit "$commit" --repo-license "$lic" --evidence "$evidence" >"$PACK/reports/$name.json"
	fi
	printf '%-16s kept %s, skipped %s\n' "$name" "$(sed -n 's/.*"kept":\([0-9]*\).*/\1/p' "$PACK/reports/$name.json")" "$(sed -n 's/.*"skipped":\([0-9]*\).*/\1/p' "$PACK/reports/$name.json")"
done
echo "pack: $PACK ($(du -sh "$PACK" | cut -f1))"
