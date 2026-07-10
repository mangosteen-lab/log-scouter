VERSION ?= $(shell sed -nE 's/^version = "([^"]+)"/\1/p' Cargo.toml | head -n 1)
PACKAGE_VERSION := $(shell sed -nE 's/^version = "([^"]+)"/\1/p' Cargo.toml | head -n 1)
TAG ?= v$(VERSION)
REMOTE ?= origin
BRANCH ?= master

.PHONY: help version test check-version check-clean check-branch check-tag publish-release release release-status

help:
	@printf '%s\n' 'Targets:'
	@printf '  %-20s %s\n' 'make version' 'Print the package version and release tag.'
	@printf '  %-20s %s\n' 'make test' 'Run cargo tests.'
	@printf '  %-20s %s\n' 'make publish-release' 'Create and push v$(VERSION), triggering the GitHub release workflow.'
	@printf '  %-20s %s\n' 'make release-status' 'Show recent release workflow runs with gh, when available.'
	@printf '%s\n' ''
	@printf '%s\n' 'Update Cargo.toml and Cargo.lock first; VERSION must match Cargo.toml.'
	@printf '%s\n' 'Override REMOTE=origin or BRANCH=master only when needed.'

version:
	@printf 'version=%s\n' '$(VERSION)'
	@printf 'tag=%s\n' '$(TAG)'

test:
	cargo test --locked

check-version:
	@test -n '$(PACKAGE_VERSION)' || { echo 'Could not read version from Cargo.toml.' >&2; exit 1; }
	@test '$(VERSION)' = '$(PACKAGE_VERSION)' || { \
		echo 'VERSION=$(VERSION) does not match Cargo.toml version $(PACKAGE_VERSION).' >&2; \
		echo 'Update Cargo.toml and Cargo.lock before publishing.' >&2; \
		exit 1; \
	}

check-clean:
	@test -z "$$(git status --porcelain)" || { \
		echo 'Working tree is dirty. Commit or stash changes before publishing a release.' >&2; \
		git status --short >&2; \
		exit 1; \
	}

check-branch:
	@test "$$(git branch --show-current)" = '$(BRANCH)' || { \
		echo 'Publish releases from $(BRANCH). Current branch: '"$$(git branch --show-current)" >&2; \
		exit 1; \
	}
	git fetch $(REMOTE) $(BRANCH) --tags
	@git diff --quiet HEAD $(REMOTE)/$(BRANCH) || { \
		echo 'Local HEAD does not match $(REMOTE)/$(BRANCH). Pull or push commits first.' >&2; \
		exit 1; \
	}

check-tag:
	@! git rev-parse -q --verify 'refs/tags/$(TAG)' >/dev/null || { \
		echo 'Local tag $(TAG) already exists.' >&2; \
		exit 1; \
	}
	@! git ls-remote --exit-code --tags $(REMOTE) 'refs/tags/$(TAG)' >/dev/null 2>&1 || { \
		echo 'Remote tag $(TAG) already exists on $(REMOTE).' >&2; \
		exit 1; \
	}

publish-release: check-version check-clean check-branch check-tag
	git tag -a '$(TAG)' -m 'Release $(TAG)'
	git push $(REMOTE) '$(TAG)'
	@printf '%s\n' 'Pushed $(TAG). GitHub Actions will publish the release assets.'
	@printf '%s\n' 'https://github.com/mangosteen-lab/log-scouter/actions/workflows/release.yml'

release: publish-release

release-status:
	@if command -v gh >/dev/null 2>&1; then \
		gh run list --workflow release.yml --limit 5; \
	else \
		echo 'gh is not installed. Open https://github.com/mangosteen-lab/log-scouter/actions/workflows/release.yml'; \
	fi
