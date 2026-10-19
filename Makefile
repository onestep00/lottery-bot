.PHONY: build install buy check status doctor test cron uninstall-cron

build:
	cargo build --release --locked

install: build
	install -d -m 700 bin
	install -m 700 target/release/lottery-bot bin/lottery-bot.new
	mv -f bin/lottery-bot.new bin/lottery-bot

buy:
	./bin/lottery-bot buy

check:
	./bin/lottery-bot check

status:
	./bin/lottery-bot status

doctor:
	./bin/lottery-bot doctor

test:
	cargo fmt --check
	cargo test --locked
	cargo clippy --locked --all-targets -- -D warnings

cron:
	./scripts/install-cron.sh

uninstall-cron:
	./scripts/uninstall-cron.sh
