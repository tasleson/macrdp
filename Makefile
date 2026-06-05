.PHONY: help build build-cli dev run-cli run-cli-ipc clean clean-cli check release

# Default target
help:
	@echo "macrdp Makefile"
	@echo ""
	@echo "  开发:"
	@echo "    make build-cli        编译 RDP 服务端"
	@echo "    make run-cli          运行 CLI (端口 3389)"
	@echo "    make run-cli-ipc      运行 CLI + IPC socket"
	@echo "    make check            检查所有编译"
	@echo ""
	@echo "  发布:"
	@echo "    make release          构建 release 版 RDP 服务端"
	@echo ""
	@echo "  清理:"
	@echo "    make clean            清理所有构建产物"
	@echo "    make clean-cli        仅清理 CLI"

# === CLI ===

build-cli:
	cargo build -p macrdp-server

run-cli:
	cargo run -p macrdp-server

run-cli-ipc:
	cargo run -p macrdp-server -- --ipc-socket /tmp/macrdp.sock

# === 发布 ===

release:
	@echo "=== 构建 release 版 RDP 服务端 ==="
	cargo build --release -p macrdp-server
	@echo ""
	@echo "=== 构建完成 ==="
	@for bin in target/release/macrdp-server target/*/release/macrdp-server; do \
		if [ -f "$$bin" ]; then ls -lh "$$bin"; exit 0; fi; \
	done; \
	echo "未找到 release 二进制: macrdp-server" >&2; exit 1

# === 全局 ===

build: build-cli
dev: run-cli

clean: clean-cli

clean-cli:
	cargo clean

check:
	@echo "检查 CLI..."
	@cargo build -p macrdp-server
	@echo "全部通过."
