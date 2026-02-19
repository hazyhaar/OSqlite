KERNEL_BUILD_STD = -Zbuild-std=core,alloc,compiler_builtins -Zbuild-std-features=compiler-builtins-mem

.PHONY: build check test clean run

build:
	cargo build $(KERNEL_BUILD_STD)

check:
	cargo check $(KERNEL_BUILD_STD)

release:
	cargo build --release $(KERNEL_BUILD_STD)

test:
	cargo test --target x86_64-unknown-linux-gnu --lib -p heavenos-kernel

clean:
	cargo clean
