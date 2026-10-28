APP_ID  := io.github.corzyy.JHSoftware
PREFIX  ?= /usr/local
DESTDIR ?=
BINDIR   = $(DESTDIR)$(PREFIX)/bin
DATADIR  = $(DESTDIR)$(PREFIX)/share

.PHONY: all build install uninstall install-user uninstall-user run clean

all: build

build:
	cargo build --release

# System-wide install (default /usr/local, needs root): sudo make install
install: build
	install -Dm755 target/release/jh-software $(BINDIR)/jh-software
	install -Dm644 data/$(APP_ID).desktop $(DATADIR)/applications/$(APP_ID).desktop
	install -Dm644 data/$(APP_ID).metainfo.xml $(DATADIR)/metainfo/$(APP_ID).metainfo.xml
	install -Dm644 data/icons/hicolor/scalable/apps/$(APP_ID).svg $(DATADIR)/icons/hicolor/scalable/apps/$(APP_ID).svg
	install -Dm644 data/icons/hicolor/symbolic/apps/$(APP_ID)-symbolic.svg $(DATADIR)/icons/hicolor/symbolic/apps/$(APP_ID)-symbolic.svg
	-[ -z "$(DESTDIR)" ] && gtk-update-icon-cache -qtf $(DATADIR)/icons/hicolor || true
	-[ -z "$(DESTDIR)" ] && update-desktop-database -q $(DATADIR)/applications || true

uninstall:
	rm -f $(BINDIR)/jh-software \
	      $(DATADIR)/applications/$(APP_ID).desktop \
	      $(DATADIR)/metainfo/$(APP_ID).metainfo.xml \
	      $(DATADIR)/icons/hicolor/scalable/apps/$(APP_ID).svg \
	      $(DATADIR)/icons/hicolor/symbolic/apps/$(APP_ID)-symbolic.svg

# Per-user install into ~/.local, no root needed: make install-user
install-user: build
	install -Dm755 target/release/jh-software $(HOME)/.local/bin/jh-software
	install -Dm644 data/icons/hicolor/scalable/apps/$(APP_ID).svg $(HOME)/.local/share/icons/hicolor/scalable/apps/$(APP_ID).svg
	install -Dm644 data/icons/hicolor/symbolic/apps/$(APP_ID)-symbolic.svg $(HOME)/.local/share/icons/hicolor/symbolic/apps/$(APP_ID)-symbolic.svg
	install -Dm644 data/$(APP_ID).metainfo.xml $(HOME)/.local/share/metainfo/$(APP_ID).metainfo.xml
	mkdir -p $(HOME)/.local/share/applications
	sed 's|^Exec=jh-software|Exec=$(HOME)/.local/bin/jh-software|' data/$(APP_ID).desktop > $(HOME)/.local/share/applications/$(APP_ID).desktop
	-gtk-update-icon-cache -qtf $(HOME)/.local/share/icons/hicolor || true
	-update-desktop-database -q $(HOME)/.local/share/applications || true

uninstall-user:
	rm -f $(HOME)/.local/bin/jh-software \
	      $(HOME)/.local/share/applications/$(APP_ID).desktop \
	      $(HOME)/.local/share/metainfo/$(APP_ID).metainfo.xml \
	      $(HOME)/.local/share/icons/hicolor/scalable/apps/$(APP_ID).svg \
	      $(HOME)/.local/share/icons/hicolor/symbolic/apps/$(APP_ID)-symbolic.svg \
	      $(HOME)/.config/autostart/$(APP_ID).desktop

run:
	cargo run --release

clean:
	cargo clean
