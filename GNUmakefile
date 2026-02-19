# HeavenOS build system — produces a bootable ISO using the Limine bootloader.
#
# Targets:
#   make          — build kernel
#   make iso      — build bootable ISO
#   make run      — build and run in QEMU (BIOS, serial console)
#   make run-uefi — build and run in QEMU (UEFI)
#   make clean    — remove build artifacts
#   make distclean — also remove limine and ovmf downloads

MAKEFLAGS += -rR
.SUFFIXES:

override IMAGE_NAME := heavenos

# QEMU flags: 256 MB RAM, NVMe drive, virtio-net, serial to stdio
QEMUFLAGS ?= -m 256 \
	-drive file=disk.img,format=raw,if=none,id=nvme0 \
	-device nvme,serial=deadbeef,drive=nvme0 \
	-netdev user,id=net0,hostfwd=tcp::8080-:80 \
	-device virtio-net-pci,netdev=net0 \
	-nographic

# ---- Build kernel ----

.PHONY: kernel
kernel:
	cargo build --release
	mkdir -p bin
	cp target/x86_64-unknown-none/release/heavenos-kernel bin/kernel

# ---- Create NVMe disk image (64 MB) ----

disk.img:
	dd if=/dev/zero bs=1M count=64 of=disk.img 2>/dev/null

# ---- Limine bootloader ----

limine/limine:
	rm -rf limine
	git clone https://github.com/limine-bootloader/limine.git --branch=v10.x-binary --depth=1
	$(MAKE) -C limine

# ---- ISO image ----

.PHONY: iso
iso: $(IMAGE_NAME).iso

$(IMAGE_NAME).iso: limine/limine kernel
	rm -rf iso_root
	mkdir -p iso_root/boot iso_root/boot/limine iso_root/EFI/BOOT
	cp -v bin/kernel iso_root/boot/
	cp -v limine.conf iso_root/boot/limine/
	cp -v limine/limine-bios.sys limine/limine-bios-cd.bin limine/limine-uefi-cd.bin iso_root/boot/limine/
	cp -v limine/BOOTX64.EFI iso_root/EFI/BOOT/ 2>/dev/null || true
	cp -v limine/BOOTIA32.EFI iso_root/EFI/BOOT/ 2>/dev/null || true
	xorriso -as mkisofs -b boot/limine/limine-bios-cd.bin \
		-no-emul-boot -boot-load-size 4 -boot-info-table \
		--efi-boot boot/limine/limine-uefi-cd.bin \
		-efi-boot-part --efi-boot-image --protective-msdos-label \
		iso_root -o $(IMAGE_NAME).iso
	./limine/limine bios-install $(IMAGE_NAME).iso
	rm -rf iso_root

# ---- Run in QEMU (BIOS mode — no OVMF needed) ----

.PHONY: run
run: $(IMAGE_NAME).iso disk.img
	qemu-system-x86_64 \
		-M q35 \
		-cdrom $(IMAGE_NAME).iso \
		-boot d \
		$(QEMUFLAGS)

# ---- Run in QEMU (UEFI mode) ----

.PHONY: run-uefi
run-uefi: ovmf/ovmf-code-x86_64.fd ovmf/ovmf-vars-x86_64.fd $(IMAGE_NAME).iso disk.img
	qemu-system-x86_64 \
		-M q35 \
		-drive if=pflash,unit=0,format=raw,file=ovmf/ovmf-code-x86_64.fd,readonly=on \
		-drive if=pflash,unit=1,format=raw,file=ovmf/ovmf-vars-x86_64.fd \
		-cdrom $(IMAGE_NAME).iso \
		$(QEMUFLAGS)

ovmf/ovmf-code-x86_64.fd:
	mkdir -p ovmf
	curl -Lo $@ https://github.com/osdev0/edk2-ovmf-nightly/releases/latest/download/ovmf-code-x86_64.fd

ovmf/ovmf-vars-x86_64.fd:
	mkdir -p ovmf
	curl -Lo $@ https://github.com/osdev0/edk2-ovmf-nightly/releases/latest/download/ovmf-vars-x86_64.fd

# ---- Cleanup ----

.PHONY: clean
clean:
	cargo clean
	rm -rf iso_root bin $(IMAGE_NAME).iso disk.img

.PHONY: distclean
distclean: clean
	rm -rf limine ovmf
