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
	cd talker   && uv run pytest
	cd reasoner && npm test --if-present

# ── Lint ──
# rustc/clippy strict, ruff for Python, buf for proto. Each tool only fails
# the target if it itself is failing — install the tools you use.
lint:
	cd gateway && cargo build
	cd talker  && uv run ruff check .
	buf lint $(PROTO_DIR)

# ── Clean ──
clean:
	cd gateway && cargo clean
	rm -rf talker/.venv
	rm -rf reasoner/node_modules

.PHONY: proto proto-py proto-rs test lint clean
