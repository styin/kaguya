# Cross-platform Make targets. Avoid GNU/BSD-specific tools (sed -i, etc.).
# Anything non-trivial is delegated to a checked-in Python script.

PROTO_DIR  := proto
PROTO_FILE := $(PROTO_DIR)/kaguya/v1/kaguya.proto
# Override when using an already-provisioned environment, e.g.
# make proto TALKER_PYTHON=/absolute/path/to/python
TALKER_PYTHON ?= uv run python
PYTHON ?= python

# ── Proto (Python) ──
# Delegates to talker/scripts/gen_proto.py: generates flat-layout stubs into
# talker/proto/, also writes mypy-protobuf .pyi stubs, and patches the gRPC
# import to be relative. Pure Python — works on macOS, Linux, Windows.
proto-py:
	cd talker && $(TALKER_PYTHON) scripts/gen_proto.py

# ── Proto (Rust) ──
# kaguya-protocol generates the Rust bindings once through tonic-build.
proto-rs:
	cargo build -p kaguya-protocol

# ── Both ──
proto: proto-py proto-rs

# ── Tests ──
test:
	cargo test -p kaguya-protocol
	cd gateway  && cargo test
	cd gateway  && cargo test --features dev-console
	cd supervisor && cargo test
	cd talker   && uv run pytest
	cd console  && npm test

# ── Lint ──
# Build/type checks, formatting, Ruff and protobuf lint. Install the listed tools
# before running; a missing tool fails the target.
lint:
	cargo build --workspace
	cargo fmt --all --check
	cd talker  && uv run ruff check .
	cd talker  && uv run ruff format --check .
	cd console && npm run build
	buf lint $(PROTO_DIR)
	buf format --diff --exit-code $(PROTO_DIR)

# ── Format ──
# Writes formatting changes in place. CI typically uses `--check` flavors
# (cargo fmt --check, ruff format --check, buf format --diff) — see lint
# semantics. This target is for local "fix it" runs.
format:
	cargo fmt --all
	cd talker  && uv run ruff format .
	buf format -w $(PROTO_DIR)

# ── Sandbox (Docker backend) ──
# Build the base image used by the `docker` sandbox backend. Only needed when
# config/kaguya.runtime.toml sets [sandbox] backend = "docker". The native
# backend (default) needs none of this.
SANDBOX_IMAGE ?= kaguya-sandbox:latest
sandbox-image:
	docker build -t $(SANDBOX_IMAGE) -f docker/sandbox.Dockerfile docker

# Remove Kaguya sandbox containers after stopping Supervisor, including
# orphans left by a hard crash. Matches the `kaguya.sandbox` label.
sandbox-clean:
	$(PYTHON) scripts/maintenance.py sandbox-clean

# ── Clean ──
clean:
	cargo clean
	$(PYTHON) scripts/maintenance.py clean

.PHONY: proto proto-py proto-rs test lint format clean sandbox-image sandbox-clean
