include .env
export

TARGET = x86_64-unknown-linux-musl

# ─── Local ───────────────────────────────────────────────────────────────────

host:
	cargo run -p app -- --host

viewer:
	cargo run -p app -- --viewer

# ─── Deploy ──────────────────────────────────────────────────────────────────

deploy:
	cargo build -p signaling-server --release --target $(TARGET)
	scp -i $(EC2_KEY) target/$(TARGET)/release/signaling-server $(EC2_HOST):~/signaling-server
	ssh -i $(EC2_KEY) $(EC2_HOST) "sudo systemctl restart signaling-server"
	@echo "Deploy concluído."

# ─── EC2 ─────────────────────────────────────────────────────────────────────

logs:
	ssh -i $(EC2_KEY) $(EC2_HOST) "sudo journalctl -u signaling-server -f"

status:
	ssh -i $(EC2_KEY) $(EC2_HOST) "sudo systemctl status signaling-server"

.PHONY: host viewer deploy logs status
