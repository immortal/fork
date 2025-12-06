test: fmt clippy
  RUST_TEST_THREADS=1 cargo test

fmt:
  cargo fmt --all -- --check

clippy:
  cargo clippy --all-targets --all-features

update:
  cargo update

clean:
  cargo clean

# Coverage report
coverage:
  cargo llvm-cov --all-features --workspace

# Check if working directory is clean
check-clean:
    #!/usr/bin/env bash
    if [[ -n $(git status --porcelain) ]]; then
        echo "❌ Working directory is not clean. Commit or stash your changes first."
        git status --short
        exit 1
    fi
    echo "✅ Working directory is clean"

# Check if on develop branch
check-develop:
    #!/usr/bin/env bash
    current_branch=$(git branch --show-current)
    if [[ "$current_branch" != "develop" ]]; then
        echo "❌ Not on develop branch (currently on: $current_branch)"
        echo "Switch to develop branch first: git checkout develop"
        exit 1
    fi
    echo "✅ On develop branch"

# Check if tag already exists for a given version
check-tag-not-exists version:
    #!/usr/bin/env bash
    set -euo pipefail
    version="{{version}}"

    git fetch --tags --quiet

    if git rev-parse -q --verify "refs/tags/${version}" >/dev/null 2>&1; then
        echo "❌ Tag ${version} already exists!"
        exit 1
    fi

    echo "✅ No tag exists for version ${version}"

_bump bump_kind: check-develop check-clean clean update test
    #!/usr/bin/env bash
    set -euo pipefail

    bump_kind="{{bump_kind}}"

    cleanup() {
        status=$?
        if [ $status -ne 0 ]; then
            echo "↩️  Restoring version files after failure..."
            git checkout -- Cargo.toml Cargo.lock >/dev/null 2>&1 || true
        fi
        exit $status
    }
    trap cleanup EXIT

    previous_version=$(cargo metadata --no-deps --format-version 1 | jq -r '.packages[0].version')
    echo "ℹ️  Current version: ${previous_version}"

    echo "🔧 Bumping ${bump_kind} version..."
    cargo set-version --bump "${bump_kind}"
    new_version=$(cargo metadata --no-deps --format-version 1 | jq -r '.packages[0].version')
    echo "📝 New version: ${new_version}"

    validate_bump() {
        local previous=$1 bump=$2 current=$3
        IFS=. read -r prev_major prev_minor prev_patch <<<"${previous}"
        IFS=. read -r new_major new_minor new_patch <<<"${current}"

        case "${bump}" in
            patch)
                (( new_major == prev_major && new_minor == prev_minor && new_patch == prev_patch + 1 )) || { echo "❌ Expected patch bump from ${previous}, got ${current}"; exit 1; }
                ;;
            minor)
                (( new_major == prev_major && new_minor == prev_minor + 1 && new_patch == 0 )) || { echo "❌ Expected minor bump from ${previous}, got ${current}"; exit 1; }
                ;;
            major)
                (( new_major == prev_major + 1 && new_minor == 0 && new_patch == 0 )) || { echo "❌ Expected major bump from ${previous}, got ${current}"; exit 1; }
                ;;
        esac
    }

    validate_bump "${previous_version}" "${bump_kind}" "${new_version}"

    echo "🔍 Verifying tag does not exist for ${new_version}..."
    git fetch --tags --quiet
    if git rev-parse -q --verify "refs/tags/${new_version}" >/dev/null 2>&1; then
        echo "❌ Tag ${new_version} already exists!"
        exit 1
    fi

    echo "🔄 Updating dependencies..."
    cargo update

    echo "🧹 Running clean build..."
    cargo clean

    echo "🧪 Running tests with new version (via just test)..."
    just test

    git add .
    git commit -m "bump version to ${new_version}"
    git push origin develop
    echo "✅ Version bumped and pushed to develop"

# Bump version and commit (patch level)
bump:
    @just _bump patch

# Bump minor version
bump-minor:
    @just _bump minor

# Bump major version
bump-major:
    @just _bump major

# Internal function to handle the merge and tag process
_deploy-merge-and-tag:
    #!/usr/bin/env bash
    set -euo pipefail

    new_version=$(cargo metadata --no-deps --format-version 1 | jq -r '.packages[0].version')
    echo "🚀 Starting deployment for version $new_version..."

    # Double-check tag doesn't exist (safety check)
    echo "🔍 Verifying tag doesn't exist..."
    git fetch --tags --quiet
    if git rev-parse -q --verify "refs/tags/${new_version}" >/dev/null 2>&1; then
        echo "❌ Tag ${new_version} already exists on remote!"
        echo "This should not happen. The tag may have been created in a previous run."
        exit 1
    fi

    # Ensure develop is up to date
    echo "🔄 Ensuring develop is up to date..."
    git pull origin develop

    # Switch to main and merge develop
    echo "🔄 Switching to main branch..."
    git checkout main
    git pull origin main

    echo "🔀 Merging develop into main..."
    if ! git merge develop --no-edit; then
        echo "❌ Merge failed! Please resolve conflicts manually."
        git checkout develop
        exit 1
    fi

    # Create signed tag
    echo "🏷️  Creating signed tag $new_version..."
    git tag -s "$new_version" -m "Release version $new_version"

    # Push main and tag atomically
    echo "⬆️  Pushing main branch and tag..."
    if ! git push origin main "$new_version"; then
        echo "❌ Push failed! Rolling back..."
        git tag -d "$new_version"
        git checkout develop
        exit 1
    fi

    # Switch back to develop
    echo "🔄 Switching back to develop..."
    git checkout develop

    echo "✅ Deployment complete!"
    echo "🎉 Version $new_version has been released"
    echo "📋 Summary:"
    echo "   - develop branch: bumped and pushed"
    echo "   - main branch: merged and pushed"
    echo "   - tag $new_version: created and pushed"
    echo "🔗 Monitor release: https://github.com/nbari/pg_exporter/actions"

# Deploy: merge to main, tag, and push everything
deploy: bump _deploy-merge-and-tag

# Deploy with minor version bump
deploy-minor: bump-minor _deploy-merge-and-tag

# Deploy with major version bump
deploy-major: bump-major _deploy-merge-and-tag
