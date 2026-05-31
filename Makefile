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
        provider-vsphere-run-local \
        docker-build docker-build-amd64 docker-build-arm64 \
        docker-build-chainguard docker-buildx docker-buildx-chainguard docker-push \
        sbom vexctl-install vex-validate vex-assemble \
        vex-auto-presence vex-auto-reachability \
        kind-install kind-create kind-delete kind-load \
        kind-deploy-crds kind-deploy-controller kind-up kind-down kind-status \
        kind-deploy-provider-vsphere \
        vcsim-up vcsim-down vcsim-logs \
        docs docs-serve docs-clean docs-deploy \
        calm-diagrams calm-docify calm-validate

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

provider-vsphere-run-local: ## Run the vSphere provider locally (point it at $$VSPHERE_ENDPOINT / vcsim)
	@echo "Running banlieue provider vsphere locally (KUBECONFIG=$$KUBECONFIG)..."
	@echo "  Provider CRs are read from your kube context;"
	@echo "  the actual vCenter endpoint comes from Provider.spec.connection.endpoint."
	@echo "  For vcsim: 'make vcsim-up' first, then create a Provider with endpoint=https://127.0.0.1:8989/sdk."
	RUST_LOG="$(RUST_LOG_VSPHERE)" \
	  cargo run -p banlieue --features vcsim -- provider vsphere --no-leader-elect

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

# Internal: shared cross-compile body. Picks up cross if installed, otherwise
# falls back to native compilation via a host-installed gcc cross-toolchain.
.PHONY: _build-linux
_build-linux:
	@if command -v cross >/dev/null 2>&1; then \
		echo "Building with cross for $$TRIPLE..."; \
		cross build --release --target $$TRIPLE -p $(BINARY); \
	elif command -v $$LINKER >/dev/null 2>&1 || [ "$$(uname -s)-$$(uname -m)" = "Linux-$${TRIPLE%%-*}" ]; then \
		echo "Building natively / via host gcc cross-toolchain for $$TRIPLE..."; \
		rustup target add $$TRIPLE >/dev/null 2>&1 || true; \
		cargo build --release --target $$TRIPLE -p $(BINARY); \
	else \
		echo "ERROR: neither 'cross' nor host gcc cross-toolchain found for $$TRIPLE."; \
		echo "  Install cross: cargo install cross"; \
		echo "  OR on macOS: brew tap messense/macos-cross-toolchains && brew install $$TRIPLE"; \
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
		kind create cluster --name $(KIND_CLUSTER_NAME) --image $(KIND_NODE_IMAGE) --config deploy/kind/cluster.yaml --wait 120s; \
	fi
	@kubectl --context kind-$(KIND_CLUSTER_NAME) cluster-info

kind-delete: ## Delete the local kind cluster
	@if kind get clusters 2>/dev/null | grep -qx $(KIND_CLUSTER_NAME); then \
		kind delete cluster --name $(KIND_CLUSTER_NAME); \
	else \
		echo "✓ no cluster named '$(KIND_CLUSTER_NAME)' — nothing to delete"; \
	fi

kind-down: kind-delete ## Alias for kind-delete

kind-deploy-crds: kind-create crds ## Apply CRDs + create $(NAMESPACE) on the kind cluster (creates cluster if missing)
	kubectl --context kind-$(KIND_CLUSTER_NAME) apply -f $(CRD_OUT_DIR)/
	kubectl --context kind-$(KIND_CLUSTER_NAME) apply -f deploy/controller/namespace.yaml

kind-load: kind-create ## Cross-compile $(BINARY) and load the image into the kind cluster (creates cluster if missing)
	@HOST_ARCH=$$(uname -m); \
		case "$$HOST_ARCH" in \
			arm64|aarch64) TRIPLE=aarch64-unknown-linux-gnu; ARCH=arm64; LINKER=aarch64-linux-gnu-gcc ;; \
			x86_64|amd64)  TRIPLE=x86_64-unknown-linux-gnu;  ARCH=amd64; LINKER=x86_64-linux-gnu-gcc ;; \
			*) echo "ERROR: unsupported host arch: $$HOST_ARCH"; exit 1 ;; \
		esac; \
		echo "Cross-compiling $(BINARY) for $$TRIPLE..."; \
		if ! command -v $$LINKER >/dev/null 2>&1 && ! command -v cross >/dev/null 2>&1 ; then \
			if [ "$$(uname -s)" != "Linux" ]; then \
				echo "ERROR: neither 'cross' nor '$$LINKER' found."; \
				echo "  macOS: brew tap messense/macos-cross-toolchains && brew install $$TRIPLE"; \
				echo "  Or:    cargo install cross"; \
				exit 1; \
			fi; \
		fi; \
		rustup target add $$TRIPLE >/dev/null 2>&1 || true; \
		cargo build --release --target $$TRIPLE -p $(BINARY); \
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

kind-deploy-controller: kind-deploy-crds ## Deploy banlieue-controller to the kind cluster (uses $(KIND_IMAGE))
	@echo "Applying namespace + RBAC..."
	@kubectl --context kind-$(KIND_CLUSTER_NAME) apply -f deploy/controller/namespace.yaml
	@for i in 1 2 3 4 5 6 7 8 9 10; do \
		if kubectl --context kind-$(KIND_CLUSTER_NAME) get namespace $(NAMESPACE) >/dev/null 2>&1; then \
			break; \
		fi; \
		echo "  waiting for namespace $(NAMESPACE) ($$i/10)..."; sleep 1; \
	done
	@kubectl --context kind-$(KIND_CLUSTER_NAME) apply -R -f deploy/controller/
	@echo "Overriding controller image to $(KIND_IMAGE) (locally built)..."
	@kubectl --context kind-$(KIND_CLUSTER_NAME) -n $(NAMESPACE) set image \
		deployment/banlieue-controller controller=$(KIND_IMAGE)
	@kubectl --context kind-$(KIND_CLUSTER_NAME) -n $(NAMESPACE) rollout status \
		deployment/banlieue-controller --timeout=180s

kind-deploy-provider-vsphere: kind-deploy-crds ## Deploy banlieue-provider-vsphere to the kind cluster (uses $(KIND_IMAGE))
	@echo "Applying namespace + RBAC + manifests..."
	@kubectl --context kind-$(KIND_CLUSTER_NAME) apply -f deploy/controller/namespace.yaml
	@for i in 1 2 3 4 5 6 7 8 9 10; do \
		if kubectl --context kind-$(KIND_CLUSTER_NAME) get namespace $(NAMESPACE) >/dev/null 2>&1; then \
			break; \
		fi; \
		echo "  waiting for namespace $(NAMESPACE) ($$i/10)..."; sleep 1; \
	done
	@kubectl --context kind-$(KIND_CLUSTER_NAME) apply -R -f deploy/provider-vsphere/
	@echo "Overriding provider image to $(KIND_IMAGE) (locally built)..."
	@kubectl --context kind-$(KIND_CLUSTER_NAME) -n $(NAMESPACE) set image \
		deployment/banlieue-provider-vsphere provider=$(KIND_IMAGE)
	@kubectl --context kind-$(KIND_CLUSTER_NAME) -n $(NAMESPACE) rollout status \
		deployment/banlieue-provider-vsphere --timeout=180s

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
	@echo "    kubectl --context kind-$(KIND_CLUSTER_NAME) apply -f examples/"

kind-status: ## Show cluster, controller, and CR status
	@echo "=== kind clusters ==="
	@kind get clusters 2>/dev/null || echo "(none)"
	@echo ""
	@echo "=== controller pods (namespace $(NAMESPACE)) ==="
	@kubectl --context kind-$(KIND_CLUSTER_NAME) -n $(NAMESPACE) get pods 2>/dev/null || echo "(cluster unreachable or namespace absent)"
	@echo ""
	@echo "=== banlieue CRs (all namespaces) ==="
	@for k in providers virtualmachines vmclasses vmimages vspheremachines; do \
		echo "--- $$k ---"; \
		kubectl --context kind-$(KIND_CLUSTER_NAME) get $$k -A 2>/dev/null || echo "(unreachable or CRD missing)"; \
	done
