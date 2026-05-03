CARGO ?= cargo
PREFIX ?= /usr/local
BINDIR ?= $(PREFIX)/bin
TARGET = target/release/alsh

.PHONY: all build install uninstall clean test

all: build

build:
	$(CARGO) build --release

install:
	if [ ! -f $(TARGET) ]; then \
		echo "Release binary not found. Run 'make build' first."; \
		exit 1; \
	fi
	install -d $(BINDIR)
	install -m 755 $(TARGET) $(BINDIR)/alsh

uninstall:
	rm -f $(BINDIR)/alsh

clean:
	$(CARGO) clean

test:
	$(CARGO) test
