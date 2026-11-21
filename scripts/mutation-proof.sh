#!/usr/bin/env bash
# Mutation proof for the attack suite (workspace rule, 2026-11-24): every attack
# test must fail when exactly the protection it asserts is removed. Each file in
# crates/sandbox-server/tests/attacks/mutants/mutant-<test>.patch is a named
# mutant; this script applies it, runs the matching attack test, demands red,
# and restores the tree. A mutant that still applies but passes is a hole; a
# mutant that no longer applies is stale evidence — both fail.
#
# Re-review-4 (N-5) hardening:
#   * header values are validated before they are word-split into the cargo
#     command — a patch header is input, not code;
#   * the kill test runs GREEN on the clean tree first, so a later FAILED is the
#     mutant's doing, never a broken baseline;
#   * a kill is the named test's own `... FAILED` line under `running 1 test`,
#     not any failed suite in the log;
#   * a trap restores an applied mutant on interrupt — a killed proof cannot
#     leave the tree wounded (mutants now live in lib sources too);
#   * the proof build is `-D warnings`, like every other gate.
set -u -o pipefail
cd "$(dirname "$0")/.."
command -v brew >/dev/null 2>&1 && export PATH="$(brew --prefix rustup)/bin:$PATH"
mutants_dir="crates/sandbox-server/tests/attacks/mutants"
out="target/mutation-proof-last.log"
failures=0

# A mutant stays applied only while this names its patch — the EXIT trap
# restores it on any exit path, so an interrupt cannot leave the tree wounded.
applied=""
restore() {
    if [ -n "$applied" ]; then
        git apply -R "$applied" 2>/dev/null || true
        applied=""
    fi
}
trap restore EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# Header values reach the cargo command through a shell word-split: they must be
# a cargo target or a qualified test path and nothing else — anything looser
# could smuggle flags (or worse) into the proof.
valid_target='^-p [a-z0-9_-]+( --lib| --test [a-z0-9_-]+)?$'
valid_filter='^[a-zA-Z0-9_:]+$'

for patch in "$mutants_dir"/mutant-*.patch; do
    name="$(basename "$patch" .patch)"
    # Several mutants may share one test: mutant-<test>__<tag>.patch strips the tag.
    test="${name#mutant-}"
    test="${test%%__*}"
    # Most mutants die to an attack test; a patch may name another cargo target
    # with a leading `## proof-target: <cargo test args>` line (e.g. a lib test in
    # the crate it wounds — sandbox-http's refusal path has no attack-test reach).
    target="-p sandbox-server --test attacks"
    header="$(sed -n 's|^## proof-target: ||p' "$patch" | head -1)"
    if [ -n "$header" ]; then
        if ! printf '%s' "$header" | grep -Eq "$valid_target"; then
            echo "BAD HEADER: $name proof-target '$header' is not a cargo target"
            failures=$((failures + 1))
            continue
        fi
        target="$header"
    fi
    # `--exact` matches the full test path; a mutant whose kill test is nested in
    # a module names it with `## proof-filter: <qualified path>`.
    filter="$(sed -n 's|^## proof-filter: ||p' "$patch" | head -1)"
    if [ -n "$filter" ]; then
        if ! printf '%s' "$filter" | grep -Eq "$valid_filter"; then
            echo "BAD HEADER: $name proof-filter '$filter' is not a test path"
            failures=$((failures + 1))
            continue
        fi
    else
        filter="$test"
    fi
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

    # The green baseline (N-5): the kill test must pass on the clean tree, or a
    # later FAILED could be a broken baseline instead of the mutant's doing.
    # shellcheck disable=SC2086 # the target is the patch's own words
    if ! env RUSTFLAGS="-D warnings" cargo test $target "$filter" -- --exact --nocapture >"$out" 2>&1; then
        echo "NO BASELINE: $filter does not pass on the clean tree (see $out)"
        failures=$((failures + 1))
        continue
    fi

    git apply "$patch"
    applied="$patch"
    rc=0
    # shellcheck disable=SC2086 # the target is the patch's own words
    env RUSTFLAGS="-D warnings" cargo test $target "$filter" -- --exact --nocapture >"$out" 2>&1 || rc=$?
    git apply -R "$patch"
    applied=""
    # shellcheck disable=SC2086 # paths come from the patch's own +++ headers
    after="$(git hash-object $touched | tr '\n' ' ')"
    [ "$after" = "$before" ] || { echo "UNRESTORED: $name left the tree dirty"; exit 1; }

    if [ "$rc" -eq 0 ]; then
        echo "SURVIVED: $test passed with the protection removed (see $out)"
        failures=$((failures + 1))
    elif grep -qF "running 1 test" "$out" \
        && grep -qF "test ${filter} ... FAILED" "$out"; then
        echo "red as required: $(grep -o 'panicked.*' "$out" | head -1 | cut -c1-110)"
    else
        # A nonzero exit without the target's own FAILED line is not a kill: a
        # mutant that cannot compile, or a run that killed a sibling test, is
        # invalid evidence, not a dead mutant (N-5).
        echo "INVALID: $name did not fail the target test itself (see $out)"
        failures=$((failures + 1))
    fi
done
if [ "$failures" -eq 0 ]; then
    echo "all mutants died as required"
else
    echo "$failures mutant problem(s)" >&2
fi
exit "$failures"
