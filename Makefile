test:
	cargo test --lib --no-default-features

test-all:
	cargo test --features midi
	cargo test --lib --no-default-features

check:
	cargo check --lib --no-default-features

build-go:
	go build ./...

test-go:
	go test ./... -v -count=1
