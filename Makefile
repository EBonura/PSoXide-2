BIOS ?=
GAME ?=

.PHONY: emu editor

emu:
	cargo run --release -p app -- $(if $(BIOS),--bios $(BIOS)) $(if $(GAME),--game $(GAME))

editor:
	@echo "Editor not implemented yet"
