SHELL := /bin/bash

.PHONY: macos macos-release linux linux-release musl musl-release windows-gnu windows-msvc clippy test

macos:
	@scripts/build-macos.sh

macos-release:
	@scripts/build-macos.sh --release

linux:
	@scripts/build-linux.sh

linux-release:
	@scripts/build-linux.sh --release

musl:
	@scripts/build-musl.sh --target x86_64-unknown-linux-musl

musl-release:
	@scripts/build-musl.sh --release --target x86_64-unknown-linux-musl

windows-gnu:
	@scripts/build-windows.sh --target x86_64-pc-windows-gnu

windows-msvc:
	@scripts/build-windows.sh --msvc

clippy:
	cargo clippy -- -D warnings

test:
	cargo test

