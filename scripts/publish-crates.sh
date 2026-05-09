#!/usr/bin/env bash
set -euo pipefail

VERSION="${VERSION:-0.1.0}"
DRY_RUN=0
ALLOW_DIRTY=0
SKIP_CHECKS=0
CRATES=(ptytrace ptyrender ptyrecord ptyroom)
USER_AGENT="ptyroom-publish-script/0.1 (https://github.com/corygabrielsen/ptyroom)"

usage() {
    cat <<'USAGE'
Usage: scripts/publish-crates.sh [--dry-run] [--allow-dirty] [--skip-checks]

Publishes the ptyroom workspace crates in dependency order:
  ptytrace -> ptyrender -> ptyrecord -> ptyroom

Environment:
  VERSION=0.1.0   crate version to wait for on crates.io

Notes:
  --dry-run can fully verify only crates whose internal dependencies are
  already indexed on crates.io. Before the first release, it verifies
  ptytrace and skips dependent crates with an explicit message.
USAGE
}

while (($#)); do
    case "$1" in
        --dry-run)
            DRY_RUN=1
            ;;
        --allow-dirty)
            ALLOW_DIRTY=1
            ;;
        --skip-checks)
            SKIP_CHECKS=1
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "unknown argument: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
    shift
done

publish_args=()
if ((ALLOW_DIRTY)); then
    publish_args+=(--allow-dirty)
fi

crate_version_exists() {
    local crate="$1"
    local version="$2"
    curl -fsS \
        -A "$USER_AGENT" \
        "https://crates.io/api/v1/crates/${crate}/${version}" \
        >/dev/null 2>&1
}

internal_deps_indexed() {
    local crate="$1"
    case "$crate" in
        ptytrace)
            return 0
            ;;
        ptyrender|ptyroom)
            crate_version_exists ptytrace "$VERSION"
            ;;
        ptyrecord)
            crate_version_exists ptytrace "$VERSION" &&
                crate_version_exists ptyrender "$VERSION"
            ;;
        *)
            return 1
            ;;
    esac
}

wait_for_crate_version() {
    local crate="$1"
    local version="$2"
    local max_attempts=60
    local attempt

    for ((attempt = 1; attempt <= max_attempts; attempt++)); do
        if crate_version_exists "$crate" "$version"; then
            echo "indexed: ${crate} ${version}"
            return 0
        fi
        echo "waiting for ${crate} ${version} to appear in crates.io index (${attempt}/${max_attempts})"
        sleep 10
    done

    echo "timed out waiting for ${crate} ${version} in crates.io index" >&2
    return 1
}

run_checks() {
    cargo fmt --all --check
    cargo clippy --workspace --all-targets --all-features -- -D warnings
    cargo test --workspace
    cargo build --workspace --bins
    PTYROOM_SMOKE_SKIP_BUILD=1 scripts/smoke-local.sh
    cargo doc --workspace --no-deps
    cargo sort --workspace --check
    cargo machete
    git diff --check
}

if ((SKIP_CHECKS == 0)); then
    run_checks
fi

for crate in "${CRATES[@]}"; do
    if ((DRY_RUN)); then
        if ! internal_deps_indexed "$crate"; then
            echo "skip dry-run for ${crate}: required internal crate version is not indexed on crates.io yet"
            continue
        fi
        cargo publish --dry-run -p "$crate" "${publish_args[@]}"
    else
        cargo publish --dry-run -p "$crate" "${publish_args[@]}"
        cargo publish -p "$crate" "${publish_args[@]}"
        wait_for_crate_version "$crate" "$VERSION"
    fi
done
