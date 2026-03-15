.PHONY: dev proto clean test

# Local development script
dev:
	./scripts/dev.sh

# Generate Python protobuf bindings (dev only - stubs are committed for end users)
# Rust stubs regenerate automatically via build.rs on next `cargo build`
proto:
	@echo "==> Regenerating Python proto stubs..."
	cd talker && uv run python scripts/gen_proto.py
	@echo "==> Done. Rust stubs will regenerate automatically on next 'cargo build'."

test:
	@echo "Running tests..."
	cd gateway && cargo test
	cd talker && uv run pytest
	cd reasoner && npm test
	cd tools && npm test

clean:
	@echo "Cleaning artifacts..."
	cd gateway && cargo clean
	rm -rf talker/.venv
	rm -rf reasoner/node_modules
	rm -rf tools/node_modules
