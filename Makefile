BINARY = target/release/rz
INSTALL_PATH = $(HOME)/.cargo/bin/rz

.PHONY: build install clean publish

build:
	cargo build --release

install: build
	cp $(BINARY) $(INSTALL_PATH)
	/usr/bin/codesign -s - -f $(INSTALL_PATH)

publish: install
	-cargo publish -p rz-agent-protocol
	cargo publish -p rz-agent
	git push origin main

clean:
	cargo clean
