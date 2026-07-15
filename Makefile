# Cross-platform Make targets. Avoid GNU/BSD-specific tools (sed -i, etc.).
# Anything non-trivial is delegated to a checked-in Python script.

PROTO_DIR  := proto
PROTO_FILE := $(PROTO_DIR)/kaguya/v1/kaguya.proto

# ── Proto (Python) ──
# Delegates to talker/scripts/gen_proto.py: generates flat-layout stubs into
# talker/proto/, also writes mypy-protobuf .pyi stubs, and patches the gRPC
# import to be relative. Pure Python — works on macOS, Linux, Windows.
proto-py:
	cd talker && uv run python scripts/gen_proto.py

# ── Proto (Rust) ──
# tonic-build in gateway/build.rs regenerates Rust stubs on every cargo build.
proto-rs:
	cd gateway && cargo build

# ── Both ──
proto: proto-py proto-rs

# ── Tests ──
# `npm test --if-present` silently skips when there's no test script —
# reasoner is currently scaffolding only.
test:
	cd gateway  && cargo test
	cd supervisor && cargo test
	cd talker   && uv run pytest
	cd reasoner && npm test --if-present

# ── Lint ──
# rustc/clippy strict, ruff for Python, buf for proto. Each tool only fails
# the target if it itself is failing — install the tools you use.
lint:
	cd gateway && cargo build
	cd supervisor && cargo build
	cd talker  && uv run ruff check .
	buf lint $(PROTO_DIR)

# ── Format ──
# Writes formatting changes in place. CI typically uses `--check` flavors
# (cargo fmt --check, ruff format --check, buf format --diff) — see lint
# semantics. This target is for local "fix it" runs.
format:
	cd gateway && cargo fmt
	cd supervisor && cargo fmt
	cd talker  && uv run ruff format .
	buf format -w $(PROTO_DIR)

# ── Sandbox (Docker backend) ──
# Build the base image used by the `docker` sandbox backend. Only needed when
# config/kaguya.runtime.toml sets [sandbox] backend = "docker". The native
# backend (default) needs none of this.
SANDBOX_IMAGE ?= kaguya-sandbox:latest
sandbox-image:
	docker build -t $(SANDBOX_IMAGE) -f docker/sandbox.Dockerfile docker

# Reap orphaned sandbox containers left by a hard crash (graceful shutdown
# already cleans up). Safe to run anytime; matches the `kaguya.sandbox` label.
sandbox-clean:
	docker ps -aq --filter label=kaguya.sandbox=1 | xargs -r docker rm -f

# ── Clean ──
clean:
	cd gateway && cargo clean
	rm -rf talker/.venv
	rm -rf reasoner/node_modules

.PHONY: proto proto-py proto-rs test lint format clean sandbox-image sandbox-clean
