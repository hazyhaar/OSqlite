/// Built-in shell commands.
///
/// Each command maps to a kernel operation — reading hardware state,
/// querying the Styx namespace, or executing SQL on the embedded SQLite.
///
/// This is NOT a POSIX shell. Commands are verbs that operate on
/// the HeavenOS namespace directly.
use crate::{serial_print, serial_println};
use crate::mem::phys::PHYS_ALLOCATOR;
use crate::drivers::nvme::NVME;

/// Dispatch a command line to the appropriate handler.
pub fn dispatch(line: &str) {
    let mut parts = line.split_whitespace();
    let cmd = match parts.next() {
        Some(c) => c,
        None => return,
    };

    match cmd {
        "help" | "?" => cmd_help(),
        "mem" | "meminfo" => cmd_meminfo(),
        "nvme" | "disk" => cmd_nvme_info(),
        "net" => cmd_net(),
        "ls" => cmd_ls(parts.next().unwrap_or("/")),
        "cat" => {
            if let Some(path) = parts.next() {
                cmd_cat(path);
            } else {
                serial_println!("usage: cat <path>");
            }
        }
        "uptime" => cmd_uptime(),
        "cpu" => cmd_cpu(),
        "echo" => {
            let rest: alloc::string::String = parts.collect::<alloc::vec::Vec<&str>>().join(" ");
            serial_println!("{}", rest);
        }
        "apikey" => {
            let rest: alloc::string::String = parts.collect::<alloc::vec::Vec<&str>>().join("");
            cmd_apikey(&rest);
        }
        "ask" => {
            let rest: alloc::string::String = parts.collect::<alloc::vec::Vec<&str>>().join(" ");
            if rest.is_empty() {
                serial_println!("usage: ask <prompt>");
            } else {
                cmd_ask(&rest);
            }
        }
        "model" => {
            if let Some(name) = parts.next() {
                serial_println!("model set to: {}", name);
            } else {
                serial_println!("current model: claude-sonnet-4-5-20250929");
                serial_println!("usage: model <name>");
            }
        }
        "clear" => cmd_clear(),
        "panic" => cmd_panic(),
        "reboot" => cmd_reboot(),
        _ => {
            serial_println!("unknown command: {}", cmd);
            serial_println!("type 'help' for available commands");
        }
    }
}

fn cmd_help() {
    serial_println!("HeavenOS shell commands:");
    serial_println!();
    serial_println!("  help          show this help");
    serial_println!("  mem           physical memory info");
    serial_println!("  nvme          NVMe controller info");
    serial_println!("  net           network interface info");
    serial_println!("  cpu           CPU features");
    serial_println!("  uptime        system uptime");
    serial_println!("  ls [path]     list namespace entries");
    serial_println!("  cat <path>    read a namespace file");
    serial_println!("  echo <text>   print text");
    serial_println!();
    serial_println!("Claude API:");
    serial_println!("  apikey <key>  set Anthropic API key");
    serial_println!("  ask <prompt>  send a message to Claude");
    serial_println!("  model <name>  set model (default: claude-sonnet-4-5-20250929)");
    serial_println!();
    serial_println!("  clear         clear screen");
    serial_println!("  panic         trigger a kernel panic (for testing)");
    serial_println!("  reboot        reset the system");
    serial_println!();
    serial_println!("Line editing:");
    serial_println!("  Backspace     delete character");
    serial_println!("  Ctrl-C        cancel line");
    serial_println!("  Ctrl-U        clear line");
}

fn cmd_meminfo() {
    let free = PHYS_ALLOCATOR.free_count();
    let total = PHYS_ALLOCATOR.total_count();
    let used = total - free;
    let free_mb = (free * 4096) / (1024 * 1024);
    let used_mb = (used * 4096) / (1024 * 1024);
    let total_mb = (total * 4096) / (1024 * 1024);

    serial_println!("Physical memory:");
    serial_println!("  total:  {} pages ({} MB)", total, total_mb);
    serial_println!("  used:   {} pages ({} MB)", used, used_mb);
    serial_println!("  free:   {} pages ({} MB)", free, free_mb);
}

fn cmd_nvme_info() {
    let guard = NVME.lock();
    match guard.as_ref() {
        Some(driver) => {
            match driver.namespace_info() {
                Some(ns) => {
                    let cap_mb = ns.block_count * ns.block_size as u64 / (1024 * 1024);
                    serial_println!("NVMe namespace {}:", ns.nsid);
                    serial_println!("  blocks:     {}", ns.block_count);
                    serial_println!("  block size: {} bytes", ns.block_size);
                    serial_println!("  capacity:   {} MB", cap_mb);
                }
                None => serial_println!("NVMe: no namespace identified"),
            }
        }
        None => serial_println!("NVMe: not initialized"),
    }
}

fn cmd_cpu() {
    use crate::arch::x86_64::cpu;

    serial_println!("CPU features:");
    serial_println!("  RDRAND:        {}", cpu::has_rdrand());
    serial_println!("  CLFLUSHOPT:    {}", cpu::has_clflushopt());
    serial_println!("  Invariant TSC: {}", cpu::has_invariant_tsc());
}

fn cmd_uptime() {
    // TODO: track boot TSC and compute elapsed time
    serial_println!("uptime: not yet implemented (needs TSC calibration)");
}

fn cmd_ls(path: &str) {
    // Map well-known paths to static listings.
    // When the Styx server is wired in, this will walk the namespace.
    match path {
        "/" => {
            serial_println!("db/");
            serial_println!("sys/");
            serial_println!("hw/");
            serial_println!("agents/");
        }
        "/db" | "db" => {
            serial_println!("ctl");
            serial_println!("schema");
        }
        "/sys" | "sys" => {
            serial_println!("uptime");
            serial_println!("meminfo");
            serial_println!("log");
        }
        "/hw" | "hw" => {
            serial_println!("nvme/");
            serial_println!("gpu/");
        }
        "/hw/nvme" | "hw/nvme" => {
            serial_println!("info");
            serial_println!("smart");
            serial_println!("stats");
        }
        "/agents" | "agents" => {
            serial_println!("(no agents running)");
        }
        _ => {
            serial_println!("ls: {}: not found", path);
        }
    }
}

fn cmd_cat(path: &str) {
    // Map well-known paths to synthetic content.
    // When the Styx server is wired in, this will Tread the file.
    match path {
        "/sys/meminfo" | "sys/meminfo" => cmd_meminfo(),
        "/sys/uptime" | "sys/uptime" => cmd_uptime(),
        "/hw/nvme/info" | "hw/nvme/info" => cmd_nvme_info(),
        "/db/schema" | "db/schema" => {
            serial_println!("-- schema placeholder");
            serial_println!("-- (SQLite not yet integrated)");
        }
        _ => {
            serial_println!("cat: {}: not found", path);
        }
    }
}

fn cmd_clear() {
    // ANSI escape: clear screen + move cursor to top-left
    serial_print!("\x1b[2J\x1b[H");
}

fn cmd_panic() {
    panic!("user-triggered panic via shell");
}

fn cmd_net() {
    use crate::drivers::virtio::net::VIRTIO_NET;
    let guard = VIRTIO_NET.lock();
    match guard.as_ref() {
        Some(nic) => {
            let mac = nic.mac();
            serial_println!("Network interface: virtio-net");
            serial_println!("  MAC:    {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]);
            serial_println!("  IP:     10.0.2.15 (QEMU default)");
            serial_println!("  GW:     10.0.2.2");
            serial_println!("  Status: up");
        }
        None => {
            serial_println!("Network: not initialized");
            serial_println!("  (no virtio-net device found)");
        }
    }
}

fn cmd_apikey(key: &str) {
    if key.is_empty() {
        match crate::api::get_api_key() {
            Some(k) => {
                // Show first 12 and last 4 chars
                if k.len() > 16 {
                    serial_println!("API key: {}...{}", &k[..12], &k[k.len()-4..]);
                } else {
                    serial_println!("API key: (set, {} chars)", k.len());
                }
            }
            None => serial_println!("API key: not set. Usage: apikey sk-ant-..."),
        }
    } else {
        crate::api::set_api_key(key);
        serial_println!("API key set ({} chars)", key.len());
    }
}

fn cmd_ask(prompt: &str) {
    // Check API key
    let api_key = match crate::api::get_api_key() {
        Some(k) => k,
        None => {
            serial_println!("Error: API key not set. Run: apikey sk-ant-...");
            return;
        }
    };

    serial_println!("[connecting to Claude API via proxy at 10.0.2.2:8080...]");
    serial_println!();

    // For now, show what WOULD happen since we need the network stack
    // fully initialized. The API client code is ready — it needs:
    // 1. virtio-net driver initialized during boot
    // 2. NetStack created and passed to the shell context
    // 3. TLS proxy running on the host

    serial_println!("POST /v1/messages HTTP/1.1");
    serial_println!("Host: api.anthropic.com");
    serial_println!("X-API-Key: {}...{}", &api_key[..api_key.len().min(12)],
        if api_key.len() > 16 { &api_key[api_key.len()-4..] } else { "" });
    serial_println!("Content-Type: application/json");
    serial_println!();
    serial_println!("{{\"model\":\"claude-sonnet-4-5-20250929\",\"messages\":[{{\"role\":\"user\",\"content\":\"{}\"}}]}}", prompt);
    serial_println!();
    serial_println!("[network stack not yet connected — run QEMU with:");
    serial_println!("  -device virtio-net-pci,netdev=net0");
    serial_println!("  -netdev user,id=net0,hostfwd=tcp::8080-:80");
    serial_println!(" and a TLS proxy on the host:]");
    serial_println!();
    serial_println!("  # On host, run a simple TLS proxy:");
    serial_println!("  socat TCP-LISTEN:8080,fork,reuseaddr \\");
    serial_println!("    OPENSSL:api.anthropic.com:443");
}

fn cmd_reboot() {
    serial_println!("Rebooting...");
    // Write 0xFE to keyboard controller port 0x64 = CPU reset
    crate::arch::x86_64::outb(0x64, 0xFE);
    // If that didn't work, triple fault
    loop {
        unsafe { core::arch::asm!("hlt"); }
    }
}
