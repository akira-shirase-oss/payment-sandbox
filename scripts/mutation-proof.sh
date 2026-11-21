#!/usr/bin/env bash
# Mutation proof for the attack suite (workspace rule, 2026-11-24): every attack
# test must fail when exactly the protection it asserts is removed. Each file in
# crates/sandbox-server/tests/attacks/mutants/mutant-<test>.patch is a named
# mutant; this script applies it, runs the matching attack test, demands red,
# and restores the tree. A mutant that still applies but passes is a hole; a
# mutant that no longer applies is stale evidence — both fail.
set -u -o pipefail
cd "$(dirname "$0")/.."
command -v brew >/dev/null 2>&1 && export PATH="$(brew --prefix rustup)/bin:$PATH"
mutants_dir="crates/sandbox-server/tests/attacks/mutants"
out="target/mutation-proof-last.log"
failures=0
for patch in "$mutants_dir"/mutant-*.patch; do
    name="$(basename "$patch" .patch)"
    test="${name#mutant-}"
    echo "== $name"
    if ! git apply --check "$patch"; then
        echo "STALE: $name no longer applies cleanly"
        failures=$((failures + 1))
        continue
    fi
    touched="$(grep -E '^\+\+\+ b/' "$patch" | sed 's|^+++ b/||')"
    # Restore check is content-equality, not git-cleanliness: the tree may carry
    # unrelated uncommitted work, and a touched file may already differ from HEAD.
    # shellcheck disable=SC2086 # paths come from the patch's own +++ headers
    before="$(git hash-object $touched | tr '\n' ' ')"
    git apply "$patch"
    cargo test -p sandbox-server --test attacks "$test" -- --exact --nocapture >"$out" 2>&1
    if [ $? -eq 0 ]; then
        echo "SURVIVED: $test passed with the protection removed (see $out)"
        failures=$((failures + 1))
    else
        grep -q "FAILED" "$out" \
            && echo "red as required: $(grep -o 'panicked.*' "$out" | head -1 | cut -c1-110)"
    fi
    git apply -R "$patch"
    # shellcheck disable=SC2086 # paths come from the patch's own +++ headers
    after="$(git hash-object $touched | tr '\n' ' ')"
    [ "$after" = "$before" ] || { echo "UNRESTORED: $name left the tree dirty"; exit 1; }
done
if [ "$failures" -eq 0 ]; then
    echo "all mutants died as required"
else
    echo "$failures mutant problem(s)" >&2
fi
exit "$failures"
