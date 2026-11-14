# The guest builder image LEZ v0.3.1 builds its own programs with (its Justfile). The image id,
# and so the deployed program, depends on it.
RISC0_DOCKER_CONTAINER_TAG := r0.1.91.1
GUEST := methods/guest/target/riscv32im-risc0-zkvm-elf/docker/logos_escrow.bin

.PHONY: build test
build: ## Build the guest binaries reproducibly in Docker: the escrow, then the example arbiter
	RISC0_DOCKER_CONTAINER_TAG=$(RISC0_DOCKER_CONTAINER_TAG) cargo risczero build --manifest-path methods/guest/Cargo.toml
	@ls -la $(GUEST)
	RISC0_DOCKER_CONTAINER_TAG=$(RISC0_DOCKER_CONTAINER_TAG) cargo risczero build --manifest-path examples/arbiter/guest/Cargo.toml

test: ## Core unit tests, then the guest through LEZ's state machine
	cargo test --workspace
