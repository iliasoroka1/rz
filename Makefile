BINARY = target/release/rz
INSTALL_PATH = $(HOME)/.cargo/bin/rz

.PHONY: build install clean

build:
	cargo build --release
	/usr/bin/codesign -s - -f $(BINARY)

install: build
	cp $(BINARY) $(INSTALL_PATH)

clean:
	cargo clean
