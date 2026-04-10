BIOS ?= $(HOME)/Downloads/ps1\ bios/SCPH1001.BIN
GAME ?=

.PHONY: emu editor

emu:
	cargo run --release -p app -- --bios $(BIOS) $(if $(GAME),--game $(GAME))

editor:
	@echo "Editor not implemented yet"
