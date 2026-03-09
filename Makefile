.PHONY: dev proto clean test

# Local development script
dev:
	./scripts/dev.sh

# Generate protobuf bindings
proto:
	./scripts/generate_protos.sh

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
