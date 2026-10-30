# Reasoner

The Reasoner service is planned but not implemented. There is no runnable Node.js
package in this directory.

The current integration boundary is `ReasonerService` in
[`proto/kaguya/v1/kaguya.proto`](../proto/kaguya/v1/kaguya.proto), consumed by
[`gateway/src/clients/reasoner.rs`](../gateway/src/clients/reasoner.rs).
Gateway reports unavailable delegation as an error.

Implementation scope and acceptance gates live in
[`docs/implementation-plan-v0.1.0.md`](../docs/implementation-plan-v0.1.0.md).
