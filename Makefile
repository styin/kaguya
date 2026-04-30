PROTO_DIR  := proto
PROTO_FILE := $(PROTO_DIR)/kaguya/v1/kaguya.proto

# ── Python ──
proto-py:
	python -m grpc_tools.protoc \
		-I$(PROTO_DIR) \
		--python_out=talker/proto \
		--grpc_python_out=talker/proto \
		$(PROTO_FILE)
	@# fix relative imports
	sed -i 's/from kaguya.v1/from talker.proto.kaguya.v1/g' talker/proto/kaguya/v1/*_pb2_grpc.py

# ── Rust (tonic-build in build.rs, cargo handles it) ──
proto-rs:
	cd gateway && cargo build

# ── Both ──
proto: proto-py proto-rs

.PHONY: proto proto-py proto-rs