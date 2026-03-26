BINARY = target/release/rz
INSTALL_PATH = $(HOME)/.cargo/bin/rz

.PHONY: build install clean

build:
	cargo build --release

install: build
	cp $(BINARY) $(INSTALL_PATH)
	/usr/bin/codesign -s - -f $(INSTALL_PATH)

clean:
	cargo clean
