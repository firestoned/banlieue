# Copyright (c) 2026 Erick Bourgeois, banlieue
# SPDX-License-Identifier: Apache-2.0
#
# banlieue — Kubernetes-native abstract virtualization API.
#
# This Makefile is the single source of workflow truth for both local
# development and CI. Conventions follow the 5-spot project pattern:
#
#   - All workflow logic lives here, not in workflow YAML.
#   - Docker images are built from pre-built Linux binaries (cross-compiled
#     via `cross` or a native toolchain) — never `cargo build` inside the
#     container.
#   - One Dockerfile + one Dockerfile.chainguard, parameterised by BINARY.
#
# Local dev loop (the "ideal" from CLAUDE.md):
#
#   make kind-up                                # create cluster + apply CRDs
#   cargo run -p banlieue -- controller         # run controller out-of-cluster
#
# Full in-cluster loop (needed for the vSphere provider once 1B lands):
#
#   make kind-create                            # create the cluster
#   make crds                                   # generate deploy/crds/
#   make kind-deploy-crds                       # apply CRDs
#   make kind-load                              # build the single banlieue image
#   make kind-deploy-controller                 # apply controller manifests

.DEFAULT_GOAL := help

# ----- Variables ------------------------------------------------------------

# Workspace layout. A single binary now packages every role; the controller
# and each provider are subcommands (`banlieue controller`, `banlieue provider
# vsphere`). See ADR-0004.
WORKSPACE_BINARIES := banlieue

# Default binary for docker-build / kind-load when not specified.
BINARY ?= banlieue

# Image configuration
REGISTRY     ?= ghcr.io
ORG          ?= firestoned
IMAGE_TAG    ?= latest-dev
NAMESPACE    ?= banlieue-system

# Base images (pinned by digest in the Dockerfiles)
BASE_IMAGE            ?= gcr.io/distroless/cc-debian13:nonroot
CHAINGUARD_BASE_IMAGE ?= cgr.dev/chainguard/glibc-dynamic:latest

# Version information
VERSION ?= $(shell git describe --tags --always --dirty 2>/dev/null || echo "dev")
GIT_SHA ?= $(shell git rev-parse HEAD 2>/dev/null || echo "unknown")

# Container tool (docker or podman)
CONTAINER_TOOL ?= docker

# Supply chain (SBOM / VEX / scanning). Versions pinned; CI uses the same.
VEXCTL_VERSION ?= 0.4.1
GRYPE_VERSION  ?= 0.87.0
PRODUCT_PURL   ?= pkg:oci/banlieue
# Inputs for the local auto-vex mirrors (`make vex-auto-*`).
GRYPE_JSON         ?= grype.json
AFFECTED_FUNCTIONS ?= .vex/.affected-functions.json
RELEASE_BINARY     ?= target/release/banlieue
SBOM_FILES         ?= $(wildcard target/release/*.cdx.json docker-sbom-*.json)

# Kind configuration
KIND_VERSION       ?= 0.24.0
KIND_CLUSTER_NAME  ?= banlieue-dev
KIND_NODE_IMAGE    ?= kindest/node:v1.31.0
KIND_IMAGE          = $(REGISTRY)/$(ORG)/$(BINARY):local-dev

# CRD output
CRD_OUT_DIR ?= deploy/crds

# Generated CRD API reference (rendered by the docs site)
API_DOCS_OUT ?= docs/src/reference/api.md

# Logging for the *-run-local targets. `?=` yields to a RUST_LOG passed in the
# environment, so a CLI override wins, e.g. `RUST_LOG=debug,kube=debug make run-local`.
# RUST_LOG_VSPHERE derives from RUST_LOG so the same override flows to the
# provider, while quieting the noisy vim_rs dependency by default; override it
# directly to control vim_rs verbosity.
RUST_LOG          ?= info,kube=warn
RUST_LOG_VSPHERE  ?= $(RUST_LOG),vim_rs=warn

# CALM (FINOS Common Architecture Language Model) configuration
CALM_CLI_VERSION  ?= 1.37.0
CALM_ARCH          := docs/architecture/calm/architecture.json
CALM_TEMPLATES     := docs/architecture/calm/templates/mermaid
CALM_DIAGRAMS_OUT  := docs/src/architecture

# ----- Help -----------------------------------------------------------------

help: ## Show this help
	@echo 'Usage: make [target] [VAR=value ...]'
	@echo ''
	@echo 'Available targets:'
	@awk 'BEGIN {FS = ":.*## "} /^[a-zA-Z0-9_.-]+:.*## / {printf "  %-32s %s\n", $$1, $$2}' $(MAKEFILE_LIST)
	@echo ''
	@echo 'Common variables:'
	@echo '  BINARY=<crate-bin>             (default: $(BINARY))'
	@echo '  KIND_CLUSTER_NAME=<name>       (default: $(KIND_CLUSTER_NAME))'
	@echo '  IMAGE_TAG=<tag>                (default: $(IMAGE_TAG))'

.PHONY: help install build build-debug build-linux-amd64 build-linux-arm64 \
        prepare-binaries-linux-amd64 prepare-binaries-linux-arm64 \
        test test-lib lint format clean crds api-docs run-local \
        provider-vsphere-run-local imagebuilder-run-local \
        docker-build docker-build-amd64 docker-build-arm64 \
        docker-build-chainguard docker-buildx docker-buildx-chainguard docker-push \
        sbom vexctl-install vex-validate vex-assemble \
        vex-auto-presence vex-auto-reachability \
        kind-install kind-create kind-delete kind-load \
        kind-deploy-crds kind-deploy-controller kind-up kind-down kind-status \
        kind-deploy-provider-vsphere \
        vcsim-up vcsim-down vcsim-logs \
        docs docs-serve docs-clean docs-deploy \
        calm-diagrams calm-docify calm-validate \
        k0s-all k0s-vms k0s-config k0s-apply k0s-kubeconfig k0s-destroy k0s-clean \
        k0s-sync k0s-remote-all k0s-remote-vms k0s-remote-config k0s-remote-apply \
        k0s-remote-kubeconfig k0s-remote-destroy k0s-fetch-kubeconfig \
        debian-image debian-image-clean debian-image-sync debian-image-remote \
        debian-image-clean-remote

# ----- Development ----------------------------------------------------------

install: ## Ensure Rust toolchain is installed
	@rustup --version 2>/dev/null || { echo "Install Rust from https://rustup.rs"; exit 1; }
	@echo "✓ rustup: $$(rustup --version)"

build: ## Build all workspace crates (release, native platform)
	cargo build --release --all

build-debug: ## Build all workspace crates (debug)
	cargo build --all

test: ## Run all workspace tests
	cargo test --all

test-lib: ## Run library tests only
	cargo test --all --lib

lint: ## Check formatting and run clippy with -D warnings
	cargo fmt --all -- --check
	cargo clippy --all-targets --all-features -- -D warnings

format: ## Format all crates
	cargo fmt --all

clean: ## Clean build artefacts
	cargo clean

# ----- CALM (architecture-as-code, FINOS) -----------------------------------

calm-validate: ## Validate the CALM architecture against the meta-schema
	@command -v npx >/dev/null 2>&1 || { echo "Error: npx not found. Install Node.js from https://nodejs.org"; exit 1; }
	@npx --yes @finos/calm-cli@$(CALM_CLI_VERSION) validate \
	  -a $(CALM_ARCH) \
	  -f pretty

calm-docify: ## Generate a documentation site from the CALM model (alias of template with docify subcommand)
	@command -v npx >/dev/null 2>&1 || { echo "Error: npx not found. Install Node.js from https://nodejs.org"; exit 1; }
	@echo "Docifying CALM architecture via @finos/calm-cli@$(CALM_CLI_VERSION)..."
	@mkdir -p $(CALM_DIAGRAMS_OUT)
	@rm -f $(CALM_DIAGRAMS_OUT)/system.md $(CALM_DIAGRAMS_OUT)/flows.md $(CALM_DIAGRAMS_OUT)/*.hbs
	@npx --yes @finos/calm-cli@$(CALM_CLI_VERSION) docify \
	  -a $(CALM_ARCH) \
	  -d $(CALM_TEMPLATES) \
	  -o $(CALM_DIAGRAMS_OUT)
	@for f in $(CALM_DIAGRAMS_OUT)/*.hbs; do \
	  [ -e "$$f" ] || continue; \
	  mv "$$f" "$${f%.hbs}"; \
	done
	@echo "✓ CALM docify output written to $(CALM_DIAGRAMS_OUT)/"

calm-diagrams: ## Render CALM Mermaid diagrams into $(CALM_DIAGRAMS_OUT)
	@if [ "$(SKIP_CALM_DIAGRAMS)" = "1" ]; then \
	  echo "SKIP_CALM_DIAGRAMS=1 — using existing files in $(CALM_DIAGRAMS_OUT)"; \
	  for f in flows.md system.md; do \
	    test -f $(CALM_DIAGRAMS_OUT)/$$f || { echo "Error: $(CALM_DIAGRAMS_OUT)/$$f missing"; exit 1; }; \
	  done; \
	else \
	  command -v npx >/dev/null 2>&1 || { echo "Error: npx not found. Install Node.js from https://nodejs.org"; exit 1; }; \
	  echo "Rendering CALM diagrams via @finos/calm-cli@$(CALM_CLI_VERSION)..."; \
	  mkdir -p $(CALM_DIAGRAMS_OUT); \
	  rm -f $(CALM_DIAGRAMS_OUT)/system.md $(CALM_DIAGRAMS_OUT)/flows.md $(CALM_DIAGRAMS_OUT)/*.hbs; \
	  npx --yes @finos/calm-cli@$(CALM_CLI_VERSION) template \
	    -a $(CALM_ARCH) \
	    -d $(CALM_TEMPLATES) \
	    -o $(CALM_DIAGRAMS_OUT); \
	  echo "Stripping .hbs suffix from rendered files..."; \
	  for f in $(CALM_DIAGRAMS_OUT)/*.hbs; do \
	    [ -e "$$f" ] || continue; \
	    mv "$$f" "$${f%.hbs}"; \
	  done; \
	  echo "✓ CALM diagrams written to $(CALM_DIAGRAMS_OUT)/"; \
	fi

# ----- Documentation (MkDocs Material) --------------------------------------

docs: api-docs calm-diagrams ## Build the MkDocs site into docs/site/ (regenerates the API reference + CALM diagrams first)
	@command -v poetry >/dev/null 2>&1 || { echo "Error: Poetry not found. Install: curl -sSL https://install.python-poetry.org | python3 -"; exit 1; }
	@echo "Ensuring documentation dependencies are installed..."
	@cd docs && poetry install --no-interaction --quiet
	@echo "Building MkDocs site..."
	@cd docs && poetry run mkdocs build
	@echo "✓ Documentation built at docs/site/index.html"

docs-serve: ## Serve docs locally with live reload at http://127.0.0.1:8000
	@command -v poetry >/dev/null 2>&1 || { echo "Error: Poetry not found. Install: curl -sSL https://install.python-poetry.org | python3 -"; exit 1; }
	@cd docs && poetry install --no-interaction --quiet
	@echo "Starting MkDocs server at http://127.0.0.1:8000 (live reload)..."
	@cd docs && poetry run mkdocs serve --livereload

docs-clean: ## Remove docs build artefacts, generated diagrams, and venv
	@rm -rf docs/site/ docs/.venv/ docs/poetry.lock
	@rm -f $(CALM_DIAGRAMS_OUT)/system.md $(CALM_DIAGRAMS_OUT)/flows.md
	@echo "✓ Documentation artefacts cleaned"

docs-deploy: docs ## Build and deploy docs to GitHub Pages
	@cd docs && poetry run mkdocs gh-deploy --force
	@echo "✓ Documentation deployed to GitHub Pages"

run-local: crds ## Run the controller locally against your current kube-context
	@echo "Running banlieue controller locally (KUBECONFIG=$$KUBECONFIG)..."
	RUST_LOG="$(RUST_LOG)" cargo run -p banlieue -- controller

vsphere-live-test: ## Run the live vCenter harness against a REAL vCenter (needs VSPHERE_ENDPOINT / VSPHERE_USERNAME / VSPHERE_PASSWORD)
	@# The only coverage of the production JSON transport, TLS/BYOC and vim_rs's
	@# decoding of vCenter's object model. A vcsim-based suite cannot substitute:
	@# vim_rs's vcsim_compat requires its `xml` (SOAP) feature, which production
	@# does not use — see ADR-0014's follow-ups.
	@test -n "$$VSPHERE_ENDPOINT" || { \
	  echo "VSPHERE_ENDPOINT is unset. Example:"; \
	  echo "  VSPHERE_ENDPOINT=https://bar.foo.io/sdk \\"; \
	  echo "  VSPHERE_USERNAME='svc-banlieue@vsphere.local' \\"; \
	  echo "  VSPHERE_PASSWORD='...' \\"; \
	  echo "  [VSPHERE_CA_BUNDLE=/path/ca.pem | VSPHERE_INSECURE=true] \\"; \
	  echo "  [VSPHERE_TEMPLATE=<template-name>] \\"; \
	  echo "    make vsphere-live-test"; \
	  exit 1; }
	@echo "Running the live vCenter harness against $$VSPHERE_ENDPOINT ..."
	cargo test -p banlieue-provider-vsphere --test live_vcenter -- \
	  --ignored --nocapture --test-threads=1

provider-vsphere-run-local: ## Run the vSphere provider locally (point it at $$VSPHERE_ENDPOINT / vcsim)
	@echo "Running banlieue provider vsphere locally (KUBECONFIG=$$KUBECONFIG)..."
	@echo "  Provider CRs are read from your kube context;"
	@echo "  the actual vCenter endpoint comes from Provider.spec.connection.endpoint."
	@echo "  For vcsim: 'make vcsim-up' first, then create a Provider with endpoint=https://127.0.0.1:8989/sdk."
	RUST_LOG="$(RUST_LOG_VSPHERE)" \
	  cargo run -p banlieue --features vcsim -- provider vsphere --no-leader-elect

imagebuilder-run-local: crds ## Run banlieue-imagebuilder locally against your current kube-context (ADR-0010)
	@echo "Running banlieue imagebuilder locally (KUBECONFIG=$$KUBECONFIG)..."
	@echo "  OSArtifact CRs land in --build-namespace (default banlieue-imagebuild);"
	@echo "  make sure kairos-operator is installed and that namespace exists first"
	@echo "  (kubectl apply -f deploy/imagebuilder/namespace.yaml) -- it must NOT be"
	@echo "  banlieue-system, whose 'restricted' PodSecurity level rejects the"
	@echo "  privileged build pods kairos-operator creates."
	RUST_LOG="$(RUST_LOG)" cargo run -p banlieue -- imagebuilder --no-leader-elect

# ----- vcsim (govmomi vCenter simulator) ------------------------------------
#
# Local development against a fake vCenter. Uses the official vmware/vcsim
# container image; default credentials are user:pass on port 8989.

VCSIM_CONTAINER ?= banlieue-vcsim
VCSIM_PORT      ?= 8989
VCSIM_IMAGE     ?= vmware/vcsim:latest

vcsim-up: ## Start a local vcsim container on :$(VCSIM_PORT)
	@command -v docker >/dev/null 2>&1 || { echo "Error: docker not found"; exit 1; }
	@if docker ps -a --format '{{.Names}}' | grep -q "^$(VCSIM_CONTAINER)$$"; then \
	  echo "Container $(VCSIM_CONTAINER) already exists — starting..."; \
	  docker start $(VCSIM_CONTAINER); \
	else \
	  echo "Starting $(VCSIM_CONTAINER) from $(VCSIM_IMAGE) on :$(VCSIM_PORT)..."; \
	  docker run -d --name $(VCSIM_CONTAINER) -p $(VCSIM_PORT):8989 $(VCSIM_IMAGE); \
	fi
	@echo "✓ vcsim listening at https://127.0.0.1:$(VCSIM_PORT)/sdk (user: user / pass: pass)"

vcsim-down: ## Stop and remove the vcsim container
	@command -v docker >/dev/null 2>&1 || { echo "Error: docker not found"; exit 1; }
	@docker rm -f $(VCSIM_CONTAINER) 2>/dev/null && echo "✓ removed $(VCSIM_CONTAINER)" || true

vcsim-logs: ## Tail the vcsim container logs
	@docker logs -f $(VCSIM_CONTAINER)

# ----- Code Generation ------------------------------------------------------

crds: ## Generate CRD YAML files into $(CRD_OUT_DIR) (also refreshes the API reference)
	@cargo run --quiet -p banlieue-api --bin crdgen --features crdgen -- --out-dir $(CRD_OUT_DIR)
	@$(MAKE) --no-print-directory api-docs

api-docs: ## Generate the CRD API reference Markdown into $(API_DOCS_OUT)
	@cargo run --quiet -p banlieue-api --bin crddoc --features crdgen -- --out-file $(API_DOCS_OUT)

# ----- Cross-compile binaries (Linux targets for container builds) ---------
#
# We never compile inside the container. The Dockerfile expects a pre-built
# binary at binaries/<arch>/<binary>.
#
# Local dev on macOS arm64: `make kind-load` (BINARY defaults to `banlieue`)
# transparently cross-compiles to aarch64-unknown-linux-gnu using the GNU
# cross-toolchain installed via `brew install aarch64-unknown-linux-gnu`.

build-linux-amd64: ## Cross-compile $(BINARY) for linux/amd64
	@$(MAKE) _build-linux TRIPLE=x86_64-unknown-linux-gnu LINKER=x86_64-linux-gnu-gcc

build-linux-arm64: ## Cross-compile $(BINARY) for linux/arm64
	@$(MAKE) _build-linux TRIPLE=aarch64-unknown-linux-gnu LINKER=aarch64-linux-gnu-gcc

# Internal: shared cross-compile body.
#
# Prefers a host-installed gcc cross-toolchain (homebrew macos-cross-toolchains
# on Darwin) over `cross`, and only falls back to `cross` when no linker is
# present. The order matters: `cross` builds inside a container, so it is much
# slower, and on Apple Silicon it tries to install a *host* rustup toolchain for
# the foreign arch — which rustup refuses outright, failing the build even
# though a perfectly good cross-linker is sitting on PATH.
.PHONY: _build-linux
_build-linux:
	@if command -v $$LINKER >/dev/null 2>&1 || [ "$$(uname -s)-$$(uname -m)" = "Linux-$${TRIPLE%%-*}" ]; then \
		echo "Building natively / via host gcc cross-toolchain for $$TRIPLE..."; \
		rustup target add $$TRIPLE >/dev/null 2>&1 || true; \
		if command -v $$LINKER >/dev/null 2>&1; then \
			TRIPLE_ENV=$$(echo $$TRIPLE | tr 'a-z-' 'A-Z_'); \
			TRIPLE_US=$$(echo $$TRIPLE | tr '-' '_'); \
			AR_TOOL=$${LINKER%-gcc}-ar; \
			env CARGO_TARGET_$${TRIPLE_ENV}_LINKER=$$LINKER \
				CC_$${TRIPLE_US}=$$LINKER \
				AR_$${TRIPLE_US}=$$AR_TOOL \
				cargo build --release --target $$TRIPLE -p $(BINARY); \
		else \
			cargo build --release --target $$TRIPLE -p $(BINARY); \
		fi; \
	elif command -v cross >/dev/null 2>&1; then \
		echo "No host cross-linker for $$TRIPLE; falling back to cross (container build)..."; \
		cross build --release --target $$TRIPLE -p $(BINARY); \
	else \
		echo "ERROR: neither a host gcc cross-toolchain nor 'cross' found for $$TRIPLE."; \
		echo "  On macOS: brew tap messense/macos-cross-toolchains && brew install $$TRIPLE"; \
		echo "  OR: cargo install cross"; \
		exit 1; \
	fi

prepare-binaries-linux-amd64: build-linux-amd64 ## Stage $(BINARY) at binaries/amd64/
	@mkdir -p binaries/amd64
	@cp target/x86_64-unknown-linux-gnu/release/$(BINARY) binaries/amd64/
	@echo "✓ binaries/amd64/$(BINARY) ready"

prepare-binaries-linux-arm64: build-linux-arm64 ## Stage $(BINARY) at binaries/arm64/
	@mkdir -p binaries/arm64
	@cp target/aarch64-unknown-linux-gnu/release/$(BINARY) binaries/arm64/
	@echo "✓ binaries/arm64/$(BINARY) ready"

# ----- Docker images --------------------------------------------------------

docker-build: ## Build distroless image for $(BINARY) (linux/amd64, loads to local docker)
	@$(MAKE) docker-build-amd64 BINARY=$(BINARY)

docker-build-amd64: prepare-binaries-linux-amd64 ## Build distroless image for $(BINARY) (linux/amd64)
	$(CONTAINER_TOOL) buildx build --load --platform=linux/amd64 \
		-t $(BINARY):$(IMAGE_TAG)-amd64 \
		--build-arg BINARY=$(BINARY) \
		--build-arg VERSION="$(VERSION)" \
		--build-arg GIT_SHA="$(GIT_SHA)" \
		--build-arg BASE_IMAGE="$(BASE_IMAGE)" \
		-f Dockerfile .

docker-build-arm64: prepare-binaries-linux-arm64 ## Build distroless image for $(BINARY) (linux/arm64)
	$(CONTAINER_TOOL) buildx build --load --platform=linux/arm64 \
		-t $(BINARY):$(IMAGE_TAG)-arm64 \
		--build-arg BINARY=$(BINARY) \
		--build-arg VERSION="$(VERSION)" \
		--build-arg GIT_SHA="$(GIT_SHA)" \
		--build-arg BASE_IMAGE="$(BASE_IMAGE)" \
		-f Dockerfile .

docker-build-chainguard: prepare-binaries-linux-amd64 ## Build Chainguard image for $(BINARY) (zero-CVE base)
	$(CONTAINER_TOOL) buildx build --load --platform=linux/amd64 \
		-t $(BINARY):$(IMAGE_TAG)-chainguard \
		--build-arg BINARY=$(BINARY) \
		--build-arg VERSION="$(VERSION)" \
		--build-arg GIT_SHA="$(GIT_SHA)" \
		--build-arg BASE_IMAGE="$(CHAINGUARD_BASE_IMAGE)" \
		-f Dockerfile.chainguard .

docker-buildx: prepare-binaries-linux-amd64 ## Build and push distroless image to $(REGISTRY) (CI)
	$(CONTAINER_TOOL) buildx build --push --platform=linux/amd64 \
		-t $(REGISTRY)/$(ORG)/$(BINARY):$(IMAGE_TAG) \
		--build-arg BINARY=$(BINARY) \
		--build-arg VERSION="$(VERSION)" \
		--build-arg GIT_SHA="$(GIT_SHA)" \
		--build-arg BASE_IMAGE="$(BASE_IMAGE)" \
		-f Dockerfile .

docker-buildx-chainguard: prepare-binaries-linux-amd64 ## Build and push Chainguard image to $(REGISTRY) (CI)
	$(CONTAINER_TOOL) buildx build --push --platform=linux/amd64 \
		-t $(REGISTRY)/$(ORG)/$(BINARY):$(IMAGE_TAG)-chainguard \
		--build-arg BINARY=$(BINARY) \
		--build-arg VERSION="$(VERSION)" \
		--build-arg GIT_SHA="$(GIT_SHA)" \
		--build-arg BASE_IMAGE="$(CHAINGUARD_BASE_IMAGE)" \
		-f Dockerfile.chainguard .

docker-push: ## Push the locally-built $(BINARY) image
	$(CONTAINER_TOOL) push $(REGISTRY)/$(ORG)/$(BINARY):$(IMAGE_TAG)

# ----- Supply chain (SBOM / VEX) --------------------------------------------
# The release pipeline (signing, SLSA provenance, image scanning) lives in
# .github/workflows/build.yaml via actions; these targets cover the bits that
# are also useful locally and that CI shells out to (`make sbom`,
# `make vexctl-install`). See docs/adr/0006-release-and-supply-chain-pipeline.md.

sbom: ## Generate CycloneDX SBOM(s) for the workspace (*.cdx.json per crate)
	@command -v cargo-cyclonedx >/dev/null 2>&1 || cargo install cargo-cyclonedx --locked
	@cargo cyclonedx --format json
	@echo "✓ CycloneDX SBOM(s) generated"

vexctl-install: ## Install openvex/vexctl ($(VEXCTL_VERSION)) if not already present
	@if command -v vexctl >/dev/null 2>&1; then echo "vexctl already installed"; exit 0; fi; \
	if [ "$$(uname -s)" = "Darwin" ]; then \
		brew install vexctl; \
	else \
		arch=$$(uname -m); case "$$arch" in x86_64) arch=amd64 ;; aarch64|arm64) arch=arm64 ;; esac; \
		url="https://github.com/openvex/vexctl/releases/download/v$(VEXCTL_VERSION)/vexctl-linux-$$arch"; \
		echo "Downloading $$url"; \
		curl -fsSLo /tmp/vexctl "$$url"; \
		sudo install -m 0755 /tmp/vexctl /usr/local/bin/vexctl; \
		rm -f /tmp/vexctl; \
	fi; \
	vexctl version

vex-validate: vexctl-install ## Validate that every .vex/*.json parses and merges
	@vexctl merge --id "https://banlieue/local/validate" --author "local" .vex/*.json > /dev/null
	@echo "✓ all .vex/*.json parsed successfully"

vex-assemble: vexctl-install ## Merge .vex/*.json into one OpenVEX document on stdout
	@vexctl merge \
		--id "https://banlieue/local/assemble" \
		--author "$$(git config user.email 2>/dev/null || echo local)" \
		.vex/*.json

vex-auto-presence: ## Run auto-vex-presence locally ($(GRYPE_JSON) + $(SBOM_FILES) required)
	@if [ ! -f "$(GRYPE_JSON)" ]; then echo "ERROR: $(GRYPE_JSON) not found (run grype --output json --file $(GRYPE_JSON))"; exit 1; fi
	@if [ -z "$(SBOM_FILES)" ]; then echo "ERROR: no SBOMs found (target/release/*.cdx.json or docker-sbom-*.json)"; exit 1; fi
	@cargo run --quiet -p banlieue-vex --bin auto-vex-presence -- \
		--grype-json "$(GRYPE_JSON)" \
		$(foreach s,$(SBOM_FILES),--sbom "$(s)") \
		--vex-dir .vex \
		--product-purl "$(PRODUCT_PURL)" \
		--id "https://banlieue/local/auto-presence" \
		--author auto-vex-presence \
		--output vex.auto-presence.json
	@echo "✓ wrote vex.auto-presence.json"

vex-auto-reachability: ## Run auto-vex-reachability locally ($(GRYPE_JSON) + $(RELEASE_BINARY) required)
	@if [ ! -f "$(GRYPE_JSON)" ]; then echo "ERROR: $(GRYPE_JSON) not found"; exit 1; fi
	@if [ ! -f "$(RELEASE_BINARY)" ]; then echo "ERROR: $(RELEASE_BINARY) not found (cargo build --release -p banlieue)"; exit 1; fi
	@if [ "$$(uname -s)" = "Darwin" ]; then \
		nm -gU "$(RELEASE_BINARY)" > /tmp/avr-symbols.txt 2>/dev/null || \
			nm -D --undefined-only "$(RELEASE_BINARY)" > /tmp/avr-symbols.txt; \
	else \
		nm -D --undefined-only "$(RELEASE_BINARY)" > /tmp/avr-symbols.txt; \
	fi
	@cargo run --quiet -p banlieue-vex --bin auto-vex-reachability -- \
		--grype-json "$(GRYPE_JSON)" \
		--binary-symbols /tmp/avr-symbols.txt \
		--affected-functions "$(AFFECTED_FUNCTIONS)" \
		--vex-dir .vex \
		--product-purl "$(PRODUCT_PURL)" \
		--id "https://banlieue/local/auto-reachability" \
		--author auto-vex-reachability \
		--output vex.auto-reachability.json
	@rm -f /tmp/avr-symbols.txt
	@echo "✓ wrote vex.auto-reachability.json"

# ----- kind (local Kubernetes) ---------------------------------------------

kind-install: ## Install kind CLI if missing
	@if command -v kind >/dev/null 2>&1; then \
		echo "✓ kind already installed: $$(kind version)"; \
	else \
		echo "Installing kind v$(KIND_VERSION)..."; \
		OS=$$(uname -s | tr '[:upper:]' '[:lower:]'); \
		ARCH=$$(uname -m); \
		case "$$ARCH" in x86_64) ARCH=amd64 ;; aarch64|arm64) ARCH=arm64 ;; esac; \
		BIN="kind-$${OS}-$${ARCH}"; \
		BASE_URL="https://github.com/kubernetes-sigs/kind/releases/download/v$(KIND_VERSION)"; \
		curl -sSLf -o /tmp/$$BIN "$$BASE_URL/$$BIN"; \
		curl -sSLf -o /tmp/$$BIN.sha256sum "$$BASE_URL/$$BIN.sha256sum"; \
		cd /tmp && \
			EXPECTED=$$(awk '{print $$1}' $$BIN.sha256sum) && \
			if command -v sha256sum >/dev/null 2>&1; then \
				ACTUAL=$$(sha256sum $$BIN | awk '{print $$1}'); \
			else \
				ACTUAL=$$(shasum -a 256 $$BIN | awk '{print $$1}'); \
			fi && \
			if [ "$$EXPECTED" != "$$ACTUAL" ]; then \
				echo "ERROR: kind checksum mismatch"; exit 1; \
			fi; \
		chmod +x /tmp/$$BIN; \
		sudo mv /tmp/$$BIN /usr/local/bin/kind; \
		rm -f /tmp/$$BIN.sha256sum; \
		echo "✓ kind v$(KIND_VERSION) installed"; \
	fi
	@command -v kubectl >/dev/null 2>&1 || { echo "ERROR: kubectl not found on PATH"; exit 1; }

kind-create: kind-install ## Create local kind cluster
	@if kind get clusters 2>/dev/null | grep -qx $(KIND_CLUSTER_NAME); then \
		echo "✓ kind cluster '$(KIND_CLUSTER_NAME)' already exists"; \
	else \
		echo "Creating kind cluster '$(KIND_CLUSTER_NAME)'..."; \
		KUBECONFIG=$(KIND_KUBECONFIG) kind create cluster --name $(KIND_CLUSTER_NAME) --image $(KIND_NODE_IMAGE) --config deploy/kind/cluster.yaml --wait 120s; \
	fi
	@# Always refresh: the cluster may predate this file, or have been created
	@# by a run that wrote the context somewhere else entirely.
	@kind get kubeconfig --name $(KIND_CLUSTER_NAME) > $(KIND_KUBECONFIG)
	@$(KIND_KUBECTL) cluster-info

kind-delete: ## Delete the local kind cluster
	@if kind get clusters 2>/dev/null | grep -qx $(KIND_CLUSTER_NAME); then \
		kind delete cluster --name $(KIND_CLUSTER_NAME); \
	else \
		echo "✓ no cluster named '$(KIND_CLUSTER_NAME)' — nothing to delete"; \
	fi

kind-down: kind-delete ## Alias for kind-delete

kind-deploy-crds: kind-create crds ## Apply CRDs + create $(NAMESPACE) on the kind cluster (creates cluster if missing)
	$(KIND_KUBECTL) apply -f $(CRD_OUT_DIR)/
	$(KIND_KUBECTL) apply -f deploy/controller/namespace.yaml

kind-load: kind-create ## Cross-compile $(BINARY) and load the image into the kind cluster (creates cluster if missing)
	@HOST_ARCH=$$(uname -m); \
		case "$$HOST_ARCH" in \
			arm64|aarch64) TRIPLE=aarch64-unknown-linux-gnu; ARCH=arm64; LINKER=aarch64-linux-gnu-gcc ;; \
			x86_64|amd64)  TRIPLE=x86_64-unknown-linux-gnu;  ARCH=amd64; LINKER=x86_64-linux-gnu-gcc ;; \
			*) echo "ERROR: unsupported host arch: $$HOST_ARCH"; exit 1 ;; \
		esac; \
		echo "Cross-compiling $(BINARY) for $$TRIPLE..."; \
		if ! command -v $$LINKER >/dev/null 2>&1 && [ "$$(uname -s)" != "Linux" ]; then \
			echo "ERROR: cross-toolchain '$$LINKER' not found."; \
			echo "  macOS: brew tap messense/macos-cross-toolchains && brew install $$TRIPLE"; \
			echo "  (rustls/ring cross-compiles with the gcc cross-toolchain — no OpenSSL, no 'cross' needed.)"; \
			exit 1; \
		fi; \
		rustup target add $$TRIPLE >/dev/null 2>&1 || true; \
		if command -v $$LINKER >/dev/null 2>&1; then \
			TRIPLE_ENV=$$(echo $$TRIPLE | tr 'a-z-' 'A-Z_'); \
			TRIPLE_US=$$(echo $$TRIPLE | tr '-' '_'); \
			AR_TOOL=$${LINKER%-gcc}-ar; \
			env CARGO_TARGET_$${TRIPLE_ENV}_LINKER=$$LINKER \
				CC_$${TRIPLE_US}=$$LINKER \
				AR_$${TRIPLE_US}=$$AR_TOOL \
				cargo build --release --target $$TRIPLE -p $(BINARY); \
		else \
			cargo build --release --target $$TRIPLE -p $(BINARY); \
		fi; \
		mkdir -p binaries/$$ARCH; \
		cp target/$$TRIPLE/release/$(BINARY) binaries/$$ARCH/; \
		echo "Building image $(KIND_IMAGE) (linux/$$ARCH)..."; \
		$(CONTAINER_TOOL) build \
			--build-arg BINARY=$(BINARY) \
			--build-arg TARGETARCH=$$ARCH \
			--build-arg VERSION="$(VERSION)" \
			--build-arg GIT_SHA="$(GIT_SHA)" \
			--build-arg BASE_IMAGE="$(BASE_IMAGE)" \
			-t $(KIND_IMAGE) -f Dockerfile .; \
		echo "Loading $(KIND_IMAGE) into kind cluster '$(KIND_CLUSTER_NAME)'..."; \
		kind load docker-image $(KIND_IMAGE) --name $(KIND_CLUSTER_NAME)

kind-deploy-controller: kind-deploy-crds kind-load ## Deploy banlieue-controller to kind (log level: RUST_LOG=debug,kube=debug make kind-deploy-controller)
	@echo "Applying namespace + RBAC..."
	@$(KIND_KUBECTL) apply -f deploy/controller/namespace.yaml
	@for i in 1 2 3 4 5 6 7 8 9 10; do \
		if $(KIND_KUBECTL) get namespace $(NAMESPACE) >/dev/null 2>&1; then \
			break; \
		fi; \
		echo "  waiting for namespace $(NAMESPACE) ($$i/10)..."; sleep 1; \
	done
	@$(KIND_KUBECTL) apply -R -f deploy/controller/
	@echo "Overriding controller image to $(KIND_IMAGE) (locally built)..."
	@$(KIND_KUBECTL) -n $(NAMESPACE) set image \
		deployment/banlieue-controller controller=$(KIND_IMAGE)
	@echo "Setting RUST_LOG=$(RUST_LOG) (env overrides the ConfigMap; CLI: RUST_LOG=debug,kube=debug make kind-deploy-controller)..."
	@$(KIND_KUBECTL) -n $(NAMESPACE) set env \
		deployment/banlieue-controller RUST_LOG="$(RUST_LOG)"
	@$(KIND_KUBECTL) -n $(NAMESPACE) rollout status \
		deployment/banlieue-controller --timeout=180s

kind-deploy-operator: kind-deploy-crds kind-load ## Deploy banlieue-operator to kind (log level: RUST_LOG=debug,kube=debug make kind-deploy-operator)
	@echo "Applying namespace + RBAC..."
	@$(KIND_KUBECTL) apply -f deploy/controller/namespace.yaml
	@for i in 1 2 3 4 5 6 7 8 9 10; do \
		if $(KIND_KUBECTL) get namespace $(NAMESPACE) >/dev/null 2>&1; then \
			break; \
		fi; \
		echo "  waiting for namespace $(NAMESPACE) ($$i/10)..."; sleep 1; \
	done
	@$(KIND_KUBECTL) apply -R -f deploy/operator/
	@# The shared per-backend ClusterRole the operator BINDS to each per-instance
	@# ServiceAccount. The operator cannot create it (minting the permissions it
	@# hands out is the escalation path ADR-0012 refuses), and `bootstrap
	@# operator` installs it in the real flow — so a manual deploy must too, or
	@# every ClusterRoleBinding points at a role that does not exist.
	@echo "Applying shared provider ClusterRole(s)..."
	@for cr in deploy/provider-*/rbac/clusterrole.yaml; do \
	  [ -e "$$cr" ] || continue; \
	  $(KIND_KUBECTL) apply -f "$$cr"; \
	done
	@echo "Overriding operator image to $(KIND_IMAGE) (locally built)..."
	@$(KIND_KUBECTL) -n $(NAMESPACE) set image \
		deployment/banlieue-operator operator=$(KIND_IMAGE)
	@echo "Setting RUST_LOG=$(RUST_LOG) (env overrides the ConfigMap; CLI: RUST_LOG=debug,kube=debug make kind-deploy-operator)..."
	@$(KIND_KUBECTL) -n $(NAMESPACE) set env \
		deployment/banlieue-operator RUST_LOG="$(RUST_LOG)"
	@$(KIND_KUBECTL) -n $(NAMESPACE) rollout status \
		deployment/banlieue-operator --timeout=180s
	@echo ""
	@echo "✓ banlieue-operator is running. Apply a Provider CR and it will create"
	@echo "  that backend's Deployment, ServiceAccount, Role, RoleBinding and"
	@echo "  ClusterRoleBinding (ADR-0003). A ProviderClass must exist first:"
	@echo "    kubectl apply -f examples/08-providerclass-vsphere.yaml"
	@echo ""

operator-run-local: crds ## Run banlieue-operator locally against your current kube-context
	RUST_LOG=$(RUST_LOG) cargo run -p banlieue -- operator --no-leader-elect

kind-deploy-provider-vsphere: kind-deploy-crds kind-load ## Deploy banlieue-provider-vsphere to kind (log level: RUST_LOG=debug,kube=debug make kind-deploy-provider-vsphere)
	@echo "Applying namespace + RBAC + manifests..."
	@$(KIND_KUBECTL) apply -f deploy/controller/namespace.yaml
	@for i in 1 2 3 4 5 6 7 8 9 10; do \
		if $(KIND_KUBECTL) get namespace $(NAMESPACE) >/dev/null 2>&1; then \
			break; \
		fi; \
		echo "  waiting for namespace $(NAMESPACE) ($$i/10)..."; sleep 1; \
	done
	@$(KIND_KUBECTL) apply -R -f deploy/provider-vsphere/
	@echo "Overriding provider image to $(KIND_IMAGE) (locally built)..."
	@$(KIND_KUBECTL) -n $(NAMESPACE) set image \
		deployment/banlieue-provider-vsphere provider=$(KIND_IMAGE)
	@echo "Setting RUST_LOG=$(RUST_LOG_VSPHERE) (env overrides the ConfigMap; CLI: RUST_LOG=debug,kube=debug make kind-deploy-provider-vsphere)..."
	@$(KIND_KUBECTL) -n $(NAMESPACE) set env \
		deployment/banlieue-provider-vsphere RUST_LOG="$(RUST_LOG_VSPHERE)"
	@$(KIND_KUBECTL) -n $(NAMESPACE) rollout status \
		deployment/banlieue-provider-vsphere --timeout=180s

# ----- e2e (ADR-0014) --------------------------------------------------------
#
# The suite asserts the OPERATOR's contract — Provider CR in, workload objects
# out — against a real API server. It deliberately does NOT wait for the spawned
# provider pod to become Ready: its Provider points at `vcenter.invalid`, so the
# pod is expected to stay NotReady. See the test's module docs before editing.

# A kubeconfig scoped to the kind cluster, so the suite never depends on (or
# mutates) whatever context you happen to have selected. Gitignored.
KIND_KUBECONFIG = $(CURDIR)/.kind-kubeconfig-$(KIND_CLUSTER_NAME)

# Every kind-targeting kubectl goes through this. Passing --kubeconfig
# explicitly (not just --context) is what makes the kind workflow independent
# of whichever context you have selected: without it, `kubectl --context
# kind-...` resolves against $KUBECONFIG, which fails outright when that file
# has no such context — and, worse, `kind create cluster` would have written
# the context INTO your selected kubeconfig, mutating (say) a homelab config
# as a side effect of running the local e2e.
KIND_KUBECTL = kubectl --kubeconfig $(KIND_KUBECONFIG) --context kind-$(KIND_CLUSTER_NAME)

# Backends the e2e expects `bootstrap operator` to have seeded. Must match the
# features the binary was built with (crates/banlieue COMPILED_BACKENDS).
E2E_BACKENDS ?= vsphere,libvirt

# Backend used to exercise the `bootstrap provider <backend>` escape hatch.
E2E_ESCAPE_BACKEND ?= vsphere

kind-kubeconfig: kind-create ## Write a kind-scoped kubeconfig to $(KIND_KUBECONFIG)
	@kind get kubeconfig --name $(KIND_CLUSTER_NAME) > $(KIND_KUBECONFIG)
	@echo "✓ wrote $(KIND_KUBECONFIG)"

kind-e2e: kind-deploy-operator kind-kubeconfig ## Run the operator e2e suite against kind (creates the cluster if missing)
	@echo "Running e2e suite against kind-$(KIND_CLUSTER_NAME)..."
	@KUBECONFIG=$(KIND_KUBECONFIG) \
	 BANLIEUE_E2E_IMAGE=$(KIND_IMAGE) \
	 cargo test -p banlieue-operator --test e2e_provider_lifecycle -- \
	   --ignored --nocapture --test-threads=1
	@echo ""
	@echo "✓ e2e suite passed"

kind-bootstrap-install: kind-load kind-kubeconfig ## Install banlieue into kind via `banlieue bootstrap operator` (the documented path)
	@echo "Installing via 'banlieue bootstrap operator' (ADR-0013)..."
	@# `--version local-dev` resolves to $(KIND_IMAGE), which kind-load already
	@# side-loaded, so the pods never pull. This exercises the SAME install path
	@# a real user runs — unlike `kind-deploy-operator`, which applies deploy/
	@# manifests directly and therefore cannot catch bootstrap-only defects.
	@KUBECONFIG=$(KIND_KUBECONFIG) cargo run -q -p banlieue -- \
	  bootstrap operator --namespace $(NAMESPACE) --version local-dev
	@kubectl --kubeconfig $(KIND_KUBECONFIG) -n $(NAMESPACE) rollout status \
	  deployment/banlieue-operator --timeout=180s

kind-e2e-bootstrap: kind-bootstrap-install ## Verify the bootstrap install, then run the operator e2e against it
	@echo "Verifying the bootstrap install..."
	@KUBECONFIG=$(KIND_KUBECONFIG) \
	 BANLIEUE_E2E_NAMESPACE=$(NAMESPACE) \
	 BANLIEUE_E2E_BACKENDS=$(E2E_BACKENDS) \
	 cargo test -p banlieue-operator --test e2e_bootstrap_install -- \
	   --ignored --nocapture --test-threads=1
	@$(MAKE) kind-verify-dry-run
	@$(MAKE) kind-verify-escape-hatch
	@echo "Running the operator e2e against the bootstrap-installed cluster..."
	@KUBECONFIG=$(KIND_KUBECONFIG) \
	 BANLIEUE_E2E_IMAGE=$(KIND_IMAGE) \
	 cargo test -p banlieue-operator --test e2e_provider_lifecycle -- \
	   --ignored --nocapture --test-threads=1
	@echo ""
	@echo "✓ bootstrap install + e2e suite passed"

kind-verify-dry-run: kind-kubeconfig ## Validate `bootstrap --dry-run` output against the real apiserver
	@# ADR-0013 sells --dry-run as the GitOps path, so its output has to be
	@# genuinely applyable. `--dry-run=server` runs full schema validation and
	@# admission WITHOUT persisting anything, which catches a malformed manifest
	@# that `--dry-run=client` would happily accept.
	@echo "Validating bootstrap --dry-run output (server-side)..."
	@cargo run -q -p banlieue -- bootstrap operator \
	  --namespace $(NAMESPACE) --version local-dev --dry-run \
	  | kubectl --kubeconfig $(KIND_KUBECONFIG) apply --dry-run=server -f - >/dev/null
	@echo "✓ --dry-run manifests are accepted by the apiserver"

kind-verify-escape-hatch: kind-kubeconfig ## Verify `bootstrap provider <backend>` installs a standalone, unowned workload
	@echo "Installing a standalone $(E2E_ESCAPE_BACKEND) provider (ADR-0013 escape hatch)..."
	@KUBECONFIG=$(KIND_KUBECONFIG) cargo run -q -p banlieue -- \
	  bootstrap provider $(E2E_ESCAPE_BACKEND) --namespace $(NAMESPACE) --version local-dev
	@kubectl --kubeconfig $(KIND_KUBECONFIG) -n $(NAMESPACE) \
	  get deployment banlieue-provider-$(E2E_ESCAPE_BACKEND) >/dev/null
	@kubectl --kubeconfig $(KIND_KUBECONFIG) -n $(NAMESPACE) \
	  get serviceaccount banlieue-provider-$(E2E_ESCAPE_BACKEND) >/dev/null
	@kubectl --kubeconfig $(KIND_KUBECONFIG) \
	  get clusterrolebinding banlieue-provider-$(E2E_ESCAPE_BACKEND) >/dev/null
	@# The namespaced Role is the security-relevant half of the standalone
	@# install (security review 2026-07-31, CHAIN-002): Secret access lives HERE,
	@# scoped to the install namespace, and deliberately NOT in the cluster-wide
	@# ClusterRole above — which is bound to every provider pod and would
	@# therefore reach every Secret in the cluster.
	@kubectl --kubeconfig $(KIND_KUBECONFIG) -n $(NAMESPACE) \
	  get role banlieue-provider-$(E2E_ESCAPE_BACKEND) >/dev/null
	@kubectl --kubeconfig $(KIND_KUBECONFIG) -n $(NAMESPACE) \
	  get rolebinding banlieue-provider-$(E2E_ESCAPE_BACKEND) >/dev/null
	@# Secret access must be `get` only. `list`/`watch` cannot be constrained by
	@# resourceNames, so granting them here would re-open enumeration of every
	@# Secret in the namespace.
	@verbs=$$(kubectl --kubeconfig $(KIND_KUBECONFIG) -n $(NAMESPACE) \
	    get role banlieue-provider-$(E2E_ESCAPE_BACKEND) \
	    -o jsonpath='{range .rules[?(@.resources[0]=="secrets")]}{.verbs}{end}'); \
	  case "$$verbs" in \
	    *list*|*watch*) \
	      echo "ERROR: standalone provider Role grants Secret enumeration: $$verbs"; exit 1 ;; \
	    *get*) ;; \
	    *) echo "ERROR: standalone provider Role has no Secret get rule (got: $$verbs)"; exit 1 ;; \
	  esac
	@# And the cluster-wide role must hold NO Secret access at all.
	@cw=$$(kubectl --kubeconfig $(KIND_KUBECONFIG) \
	    get clusterrole banlieue-provider-$(E2E_ESCAPE_BACKEND) \
	    -o jsonpath='{range .rules[?(@.resources[0]=="secrets")]}{.verbs}{end}'); \
	  if [ -n "$$cw" ]; then \
	    echo "ERROR: cluster-wide provider ClusterRole grants Secret access ($$cw);"; \
	    echo "       it is bound to every provider pod, so this reaches EVERY Secret."; exit 1; \
	  fi
	@echo "✓ Secret access is namespaced and get-only; none cluster-wide"
	@# The escape hatch is deliberately NOT owned by any Provider — the operator
	@# must neither adopt nor garbage-collect it (ADR-0013).
	@owners=$$(kubectl --kubeconfig $(KIND_KUBECONFIG) -n $(NAMESPACE) \
	    get deployment banlieue-provider-$(E2E_ESCAPE_BACKEND) \
	    -o jsonpath='{.metadata.ownerReferences}'); \
	  if [ -n "$$owners" ]; then \
	    echo "ERROR: a statically installed provider must be unowned, got: $$owners"; exit 1; \
	  fi
	@echo "✓ escape hatch installed an unowned standalone workload"
	@kubectl --kubeconfig $(KIND_KUBECONFIG) -n $(NAMESPACE) \
	  delete deployment banlieue-provider-$(E2E_ESCAPE_BACKEND) >/dev/null
	@kubectl --kubeconfig $(KIND_KUBECONFIG) \
	  delete clusterrolebinding banlieue-provider-$(E2E_ESCAPE_BACKEND) >/dev/null
	@kubectl --kubeconfig $(KIND_KUBECONFIG) -n $(NAMESPACE) \
	  delete role,rolebinding banlieue-provider-$(E2E_ESCAPE_BACKEND) >/dev/null

kind-e2e-logs: ## Dump operator + provider workload state (run this when e2e fails)
	@echo "── operator deployment ──────────────────────────────────────────────"
	-@kubectl --kubeconfig $(KIND_KUBECONFIG) -n $(NAMESPACE) describe deployment/banlieue-operator
	@echo "── operator logs ────────────────────────────────────────────────────"
	-@kubectl --kubeconfig $(KIND_KUBECONFIG) -n $(NAMESPACE) logs deployment/banlieue-operator --tail=200
	@echo "── e2e namespace ────────────────────────────────────────────────────"
	-@kubectl --kubeconfig $(KIND_KUBECONFIG) -n banlieue-e2e get all,roles,rolebindings,serviceaccounts
	-@kubectl --kubeconfig $(KIND_KUBECONFIG) -n banlieue-e2e get providers -o yaml
	-@kubectl --kubeconfig $(KIND_KUBECONFIG) get providerclasses,clusterrolebindings -l app.kubernetes.io/name=banlieue

kind-e2e-ci: ## Run the e2e suite in CI via the DOCUMENTED install path; dumps diagnostics on failure, always tears the cluster down
	@# Deliberately `kind-e2e-bootstrap`, not `kind-e2e`: CI must exercise the
	@# install path real users run (`banlieue bootstrap operator`). The
	@# manifest-apply path stays available locally via `make kind-e2e`.
	@set -e; \
	if $(MAKE) kind-e2e-bootstrap; then \
	  rc=0; \
	else \
	  rc=$$?; \
	  echo "::error::e2e suite failed — dumping cluster state"; \
	  $(MAKE) kind-e2e-logs || true; \
	fi; \
	$(MAKE) kind-delete || true; \
	rm -f $(KIND_KUBECONFIG); \
	exit $$rc

kind-up: kind-create kind-deploy-crds ## One-shot: create cluster + apply CRDs (controller still runs locally)
	@echo ""
	@echo "✓ kind cluster '$(KIND_CLUSTER_NAME)' is ready with CRDs applied."
	@echo ""
	@echo "Run the controller locally (out-of-cluster) with:"
	@echo "    make run-local"
	@echo ""
	@echo "Or build + deploy the controller in-cluster:"
	@echo "    make kind-load"
	@echo "    make kind-deploy-controller"
	@echo ""
	@echo "Apply an example VirtualMachine with:"
	@echo "    $(KIND_KUBECTL) apply -f examples/"

kind-status: ## Show cluster, controller, and CR status
	@echo "=== kind clusters ==="
	@kind get clusters 2>/dev/null || echo "(none)"
	@echo ""
	@echo "=== controller pods (namespace $(NAMESPACE)) ==="
	@$(KIND_KUBECTL) -n $(NAMESPACE) get pods 2>/dev/null || echo "(cluster unreachable or namespace absent)"
	@echo ""
	@echo "=== banlieue CRs (all namespaces) ==="
	@for k in providers virtualmachines vmclasses vmimages vspheremachines; do \
		echo "--- $$k ---"; \
		$(KIND_KUBECTL) get $$k -A 2>/dev/null || echo "(unreachable or CRD missing)"; \
	done

# ----- Dev VM cluster (k0s on libvirt, for exercising the libvirt provider) -

# Bootstraps a 4-node k0s cluster (3 controller+worker, 1 worker) on Kairos
# Hadron VMs via scripts/bootstrap-k0s-cluster.sh + k0sctl. virt-install/
# qemu-img only run on a Linux libvirt host, so the k0s-remote-* targets rsync
# the script to K0S_HYPERVISOR and run it there over SSH instead of locally on
# macOS.

K0S_SCRIPT             := ./scripts/bootstrap-k0s-cluster.sh
K0S_VM_COUNT           ?= 4
K0S_VCPUS              ?= 2
K0S_MEM_MB             ?= 8192
K0S_DISK_GB            ?= 25
K0S_VM_PREFIX          ?= k0s
K0S_CLUSTER_NAME       ?= $(K0S_VM_PREFIX)-cluster
K0S_NODE_ROLES         ?= controller+worker controller+worker controller+worker worker

K0S_LIBVIRT_URI        ?= qemu:///system
K0S_LIBVIRT_NETWORK    ?= default
K0S_LIBVIRT_POOL       ?= default
# Left empty by default so bootstrap-k0s-cluster.sh's own default (generic)
# applies; set explicitly to override.
K0S_OS_VARIANT         ?=
K0S_IMAGE_URL          ?= https://github.com/kairos-io/kairos/releases/download/v4.1.2/kairos-hadron-v0.4.0-core-amd64-generic-v4.1.2.iso
# Path to a locally-downloaded Kairos installer ISO, to use instead of
# downloading K0S_IMAGE_URL.
K0S_BASE_IMAGE_PATH    ?=
# k0s version k0sctl installs on every node. Left empty so the bootstrap
# script's own default applies.
K0S_VERSION            ?=
# k0sctl OS-detection override per host (k0sctl doesn't know Hadron).
# Left empty so the script's default (debian) applies.
K0SCTL_OS_OVERRIDE     ?=

K0S_SSH_PUBKEY         ?= $(HOME)/.ssh/id_ed25519.pub
# Defaults to your own (Mac-side) username -- the VMs get a sudoer account
# matching it, not root. Override to SSH_USER=root for the old behavior.
K0S_SSH_USER           ?= $(USER)
K0S_WORKDIR            ?= $(HOME)/.local/share/k0s-bootstrap
# Set to join each VM to your tailnet on first boot.
K0S_TAILSCALE_AUTHKEY  ?=
# Set to delete the VMs' tailnet devices on destroy (admin console -> Settings
# -> Keys -> API access tokens). Tailnet defaults to "-" (the default tailnet).
K0S_TAILSCALE_API_KEY  ?=
K0S_TAILSCALE_TAILNET  ?=
# Space-separated extra hostnames/IPs for the API server cert's SANs, e.g.
# a stable DNS name pointed at whichever address you connect through.
K0S_EXTRA_SANS         ?=
# Lift k0s's default control-plane taint on controller+worker nodes: the
# topology's only worker is reserved for image builds (tainted
# dedicated=imagebuild), so everything else -- kairos-operator, local-path,
# banlieue -- schedules on the controllers. Set to false to keep the stock
# taint, e.g. with additional general-purpose workers.
K0S_NO_TAINTS          ?= true
# The single address every in-cluster component (notably the konnectivity
# agents behind kubectl logs/exec/port-forward) dials. Defaults to the first
# controller's internal DHCP address, keeping node-to-node traffic on the
# libvirt network. Set to a load balancer / CPLB VIP for a real HA entry point.
K0S_API_EXTERNAL_ADDRESS ?=
# Address the generated kubeconfig points at. Defaults to the Tailscale IP of
# the same node K0S_API_EXTERNAL_ADDRESS resolves to, so kubectl reaches the
# API server whose konnectivity server holds the agent connections.
K0S_KUBECONFIG_SERVER  ?=
# Worker node to dedicate to image builds (label banlieue.io/imagebuild=true,
# taint dedicated=imagebuild:NoSchedule). Empty = the script picks the last
# pure worker in K0S_NODE_ROLES.
K0S_IMAGEBUILD_NODE    ?=

K0S_HYPERVISOR         ?=
K0S_HYPERVISOR_USER    ?= root
K0S_HYPERVISOR_KEY     ?= $(HOME)/.ssh/id_ed25519
K0S_REMOTE_DIR         ?= /root/k0s-bootstrap
K0S_REMOTE_WORKDIR     ?= $(K0S_REMOTE_DIR)/work
# Only the *public* key is synced by k0s-sync. The matching private key must
# exist at this path on K0S_HYPERVISOR yourself (the script SSHes into the
# new guest VMs from wherever it runs) -- copy it over only if you're fine
# with that key living on the hypervisor too:
#   scp -i $(K0S_HYPERVISOR_KEY) $(K0S_HYPERVISOR_KEY) $(K0S_HYPERVISOR_USER)@$(K0S_HYPERVISOR):$(K0S_REMOTE_DIR)/
K0S_REMOTE_SSH_PRIVKEY ?= $(K0S_REMOTE_DIR)/$(notdir $(K0S_HYPERVISOR_KEY))
K0S_SSH                := ssh -i $(K0S_HYPERVISOR_KEY) $(K0S_HYPERVISOR_USER)@$(K0S_HYPERVISOR)

K0S_ENV = VM_COUNT=$(K0S_VM_COUNT) VCPUS=$(K0S_VCPUS) MEM_MB=$(K0S_MEM_MB) DISK_GB=$(K0S_DISK_GB) \
	VM_PREFIX=$(K0S_VM_PREFIX) CLUSTER_NAME=$(K0S_CLUSTER_NAME) NODE_ROLES="$(K0S_NODE_ROLES)" \
	LIBVIRT_URI=$(K0S_LIBVIRT_URI) LIBVIRT_NETWORK=$(K0S_LIBVIRT_NETWORK) LIBVIRT_POOL=$(K0S_LIBVIRT_POOL) \
	OS_VARIANT=$(K0S_OS_VARIANT) IMAGE_URL=$(K0S_IMAGE_URL) \
	BASE_IMAGE_PATH=$(K0S_BASE_IMAGE_PATH) TAILSCALE_AUTHKEY=$(K0S_TAILSCALE_AUTHKEY) EXTRA_SANS="$(K0S_EXTRA_SANS)" \
	TAILSCALE_API_KEY=$(K0S_TAILSCALE_API_KEY) TAILSCALE_TAILNET=$(K0S_TAILSCALE_TAILNET) \
	API_EXTERNAL_ADDRESS=$(K0S_API_EXTERNAL_ADDRESS) KUBECONFIG_SERVER=$(K0S_KUBECONFIG_SERVER) \
	NO_TAINTS=$(K0S_NO_TAINTS) K0S_VERSION=$(K0S_VERSION) IMAGEBUILD_NODE=$(K0S_IMAGEBUILD_NODE) \
	K0SCTL_OS_OVERRIDE=$(K0SCTL_OS_OVERRIDE) \
	SSH_PUBKEY=$(K0S_SSH_PUBKEY) SSH_USER=$(K0S_SSH_USER) WORKDIR=$(K0S_WORKDIR)

k0s-all: ## Full idempotent k0s bootstrap: VMs -> k0sctl config -> apply -> kubeconfig (runs virt-install locally)
	$(K0S_ENV) $(K0S_SCRIPT) all

k0s-vms: ## Create/ensure the k0s dev VMs exist and are running (runs virt-install locally)
	$(K0S_ENV) $(K0S_SCRIPT) vms

k0s-config: ## (Re)generate k0sctl.yaml from the current k0s VM IPs
	$(K0S_ENV) $(K0S_SCRIPT) config

k0s-apply: ## Run k0sctl apply for the k0s dev cluster
	$(K0S_ENV) $(K0S_SCRIPT) apply

k0s-kubeconfig: ## Fetch the k0s dev cluster kubeconfig into $(K0S_WORKDIR)/kubeconfig
	$(K0S_ENV) $(K0S_SCRIPT) kubeconfig

k0s-label: ## Label+taint the imagebuild worker (banlieue.io/imagebuild=true, dedicated=imagebuild:NoSchedule)
	$(K0S_ENV) $(K0S_SCRIPT) label

k0s-destroy: ## Destroy the k0s dev VMs and remove generated config/disks
	$(K0S_ENV) $(K0S_SCRIPT) destroy

k0s-clean: ## Remove the local k0s bootstrap workdir (downloaded image, disks, seed isos)
	rm -rf "$(K0S_WORKDIR)"

k0s-sync: ## Copy the bootstrap script + pubkey onto $(K0S_HYPERVISOR)
	$(K0S_SSH) 'mkdir -p $(K0S_REMOTE_DIR) && command -v rsync >/dev/null || apt-get install -y -qq rsync'
	rsync -az -e "ssh -i $(K0S_HYPERVISOR_KEY)" $(K0S_SCRIPT) $(K0S_SSH_PUBKEY) $(K0S_HYPERVISOR_USER)@$(K0S_HYPERVISOR):$(K0S_REMOTE_DIR)/

K0S_REMOTE_ENV = VM_COUNT=$(K0S_VM_COUNT) VCPUS=$(K0S_VCPUS) MEM_MB=$(K0S_MEM_MB) DISK_GB=$(K0S_DISK_GB) \
	VM_PREFIX=$(K0S_VM_PREFIX) CLUSTER_NAME=$(K0S_CLUSTER_NAME) NODE_ROLES="$(K0S_NODE_ROLES)" \
	LIBVIRT_URI=qemu:///system LIBVIRT_NETWORK=$(K0S_LIBVIRT_NETWORK) LIBVIRT_POOL=$(K0S_LIBVIRT_POOL) \
	OS_VARIANT=$(K0S_OS_VARIANT) IMAGE_URL=$(K0S_IMAGE_URL) \
	BASE_IMAGE_PATH=$(K0S_BASE_IMAGE_PATH) TAILSCALE_AUTHKEY=$(K0S_TAILSCALE_AUTHKEY) EXTRA_SANS="$(K0S_EXTRA_SANS)" \
	TAILSCALE_API_KEY=$(K0S_TAILSCALE_API_KEY) TAILSCALE_TAILNET=$(K0S_TAILSCALE_TAILNET) \
	API_EXTERNAL_ADDRESS=$(K0S_API_EXTERNAL_ADDRESS) KUBECONFIG_SERVER=$(K0S_KUBECONFIG_SERVER) \
	NO_TAINTS=$(K0S_NO_TAINTS) K0S_VERSION=$(K0S_VERSION) IMAGEBUILD_NODE=$(K0S_IMAGEBUILD_NODE) \
	K0SCTL_OS_OVERRIDE=$(K0SCTL_OS_OVERRIDE) \
	SSH_USER=$(K0S_SSH_USER) WORKDIR=$(K0S_REMOTE_WORKDIR) \
	SSH_PUBKEY=$(K0S_REMOTE_DIR)/$(notdir $(K0S_SSH_PUBKEY)) SSH_PRIVKEY=$(K0S_REMOTE_SSH_PRIVKEY)

k0s-remote-all: k0s-sync ## Run 'k0s-all' on $(K0S_HYPERVISOR) over SSH instead of locally
	$(K0S_SSH) 'cd $(K0S_REMOTE_DIR) && $(K0S_REMOTE_ENV) ./bootstrap-k0s-cluster.sh all'

k0s-remote-vms: k0s-sync ## Run 'k0s-vms' on $(K0S_HYPERVISOR) over SSH instead of locally
	$(K0S_SSH) 'cd $(K0S_REMOTE_DIR) && $(K0S_REMOTE_ENV) ./bootstrap-k0s-cluster.sh vms'

k0s-remote-config: k0s-sync ## Run 'k0s-config' on $(K0S_HYPERVISOR) over SSH instead of locally
	$(K0S_SSH) 'cd $(K0S_REMOTE_DIR) && $(K0S_REMOTE_ENV) ./bootstrap-k0s-cluster.sh config'

k0s-remote-apply: k0s-sync ## Run 'k0s-apply' on $(K0S_HYPERVISOR) over SSH instead of locally
	$(K0S_SSH) 'cd $(K0S_REMOTE_DIR) && $(K0S_REMOTE_ENV) ./bootstrap-k0s-cluster.sh apply'

k0s-remote-kubeconfig: k0s-sync ## Run 'k0s-kubeconfig' on $(K0S_HYPERVISOR) over SSH instead of locally
	$(K0S_SSH) 'cd $(K0S_REMOTE_DIR) && $(K0S_REMOTE_ENV) ./bootstrap-k0s-cluster.sh kubeconfig'

k0s-remote-label: k0s-sync ## Run 'k0s-label' on $(K0S_HYPERVISOR) over SSH instead of locally
	$(K0S_SSH) 'cd $(K0S_REMOTE_DIR) && $(K0S_REMOTE_ENV) ./bootstrap-k0s-cluster.sh label'

k0s-remote-destroy: k0s-sync ## Run 'k0s-destroy' on $(K0S_HYPERVISOR) over SSH instead of locally
	$(K0S_SSH) 'cd $(K0S_REMOTE_DIR) && $(K0S_REMOTE_ENV) ./bootstrap-k0s-cluster.sh destroy'

k0s-fetch-kubeconfig: ## Pull the generated kubeconfig from $(K0S_HYPERVISOR) into $(K0S_WORKDIR)
	mkdir -p "$(K0S_WORKDIR)"
	scp -i $(K0S_HYPERVISOR_KEY) $(K0S_HYPERVISOR_USER)@$(K0S_HYPERVISOR):$(K0S_REMOTE_WORKDIR)/kubeconfig "$(K0S_WORKDIR)/kubeconfig"
