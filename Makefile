.DEFAULT_GOAL := help

COVERAGE_MIN_LINES ?= 95
COVERAGE_IGNORE = (^|/)(tests|fuzz)/|/tests\.rs$$
MIRI_SEED ?= 0
FUZZ_SECONDS ?= 60

.PHONY: help check test test-release clippy fmt fmt-check doc msrv miri fuzz-build fuzz coverage audit deny verify prepare-publish

help:
	@echo "check, test, test-release, clippy, fmt, fmt-check, doc, msrv"
	@echo "miri (nightly + Miri), fuzz-build/fuzz (nightly + cargo-fuzz)"
	@echo "coverage (cargo-llvm-cov + llvm-tools-preview), audit, deny"
	@echo "verify, prepare-publish"

check:
	cargo check --locked --all-features --all-targets

test:
	cargo test --locked --all-features --all-targets
	cargo test --locked --all-features --doc

test-release:
	cargo test --locked --release --all-features --all-targets

clippy:
	cargo clippy --locked --all-features --all-targets -- -D warnings

fmt:
	cargo fmt --all
	cargo fmt --manifest-path fuzz/Cargo.toml --all

fmt-check:
	cargo fmt --all -- --check
	cargo fmt --manifest-path fuzz/Cargo.toml --all -- --check

doc:
	RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps --all-features
	cargo test --locked --all-features --doc

msrv:
	cargo +1.85.0 check --locked --all-features --all-targets
	cargo +1.85.0 test --locked --all-features

miri:
	MIRIFLAGS="-Zmiri-strict-provenance -Zmiri-symbolic-alignment-check -Zmiri-seed=$(MIRI_SEED)" cargo +nightly miri test --locked --test miri_test --test pool_provider -- --test-threads=1

fuzz-build:
	cargo +nightly fuzz build pool_lifecycle

fuzz:
	cargo +nightly fuzz run pool_lifecycle -- -max_total_time=$(FUZZ_SECONDS) -max_len=64 -timeout=10 -rss_limit_mb=1024

coverage:
	mkdir -p target/coverage
	cargo llvm-cov --locked --workspace --all-features --all-targets --ignore-filename-regex '$(COVERAGE_IGNORE)' --lcov --output-path target/coverage/lcov.info --fail-under-lines $(COVERAGE_MIN_LINES)
	cargo llvm-cov report --ignore-filename-regex '$(COVERAGE_IGNORE)' --html --output-dir target/coverage
	cargo llvm-cov report --ignore-filename-regex '$(COVERAGE_IGNORE)' --json --summary-only --output-path target/coverage/summary.json

audit:
	cargo audit
	cargo generate-lockfile --manifest-path fuzz/Cargo.toml
	cargo audit --file fuzz/Cargo.lock

deny:
	cargo deny --locked check
	cargo deny --manifest-path fuzz/Cargo.toml --config fuzz/deny.toml check

verify:
	$(MAKE) fmt-check
	$(MAKE) clippy
	$(MAKE) test
	$(MAKE) doc

prepare-publish:
	$(MAKE) verify
	$(MAKE) test-release
	cargo package --locked --list
	cargo publish --locked --dry-run
