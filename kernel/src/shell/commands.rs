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

use spin::Mutex;
use smoltcp::wire::Ipv4Address;

/// Stored IP for api.anthropic.com (set via `resolve` command).
/// With DNS resolver (17.1), this is used as a manual override.
/// If 0.0.0.0, the `ask` command will try DNS resolution first.
static API_TARGET_IP: Mutex<Ipv4Address> = Mutex::new(Ipv4Address::new(0, 0, 0, 0));

/// Public accessor for the agent module.
pub(crate) static API_TARGET_IP_ACCESSOR: &Mutex<Ipv4Address> = &API_TARGET_IP;

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
                cmd_ask(&rest, true);
            }
        }
        "askp" => {
            let rest: alloc::string::String = parts.collect::<alloc::vec::Vec<&str>>().join(" ");
            if rest.is_empty() {
                serial_println!("usage: askp <prompt>  (proxy mode)");
            } else {
                cmd_ask(&rest, false);
            }
        }
        "resolve" => {
            let rest: alloc::string::String = parts.collect::<alloc::vec::Vec<&str>>().join("");
            cmd_resolve(&rest);
        }
        "model" => {
            let rest: alloc::string::String = parts.collect::<alloc::vec::Vec<&str>>().join(" ");
            cmd_model(&rest);
        }
        "pin" => {
            let sub = parts.next().unwrap_or("show");
            let rest: alloc::string::String = parts.collect::<alloc::vec::Vec<&str>>().join("");
            cmd_pin(sub, &rest);
        }
        "sql" => {
            let rest: alloc::string::String = parts.collect::<alloc::vec::Vec<&str>>().join(" ");
            if rest.is_empty() {
                serial_println!("usage: sql <statement>");
            } else {
                cmd_sql(&rest);
            }
        }
        "run" => {
            if let Some(path) = parts.next() {
                cmd_run(path);
            } else {
                serial_println!("usage: run <path>   (execute a Lua agent from namespace)");
            }
        }
        "store" => {
            // store <path> <code...>
            if let Some(path) = parts.next() {
                let code: alloc::string::String = parts.collect::<alloc::vec::Vec<&str>>().join(" ");
                if code.is_empty() {
                    serial_println!("usage: store <path> <lua code>");
                } else {
                    cmd_store(path, &code);
                }
            } else {
                serial_println!("usage: store <path> <lua code>");
            }
        }
        "agent" => {
            let rest: alloc::string::String = parts.collect::<alloc::vec::Vec<&str>>().join(" ");
            if rest.is_empty() {
                serial_println!("usage: agent <prompt>");
                serial_println!("  Starts an agentic loop with tool use (read, write, sql, etc.)");
            } else {
                cmd_agent(&rest, true);
            }
        }
        "agentp" => {
            let rest: alloc::string::String = parts.collect::<alloc::vec::Vec<&str>>().join(" ");
            if rest.is_empty() {
                serial_println!("usage: agentp <prompt>  (proxy mode)");
            } else {
                cmd_agent(&rest, false);
            }
        }
        "lua" => cmd_lua_repl(),
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
    serial_println!("  sql <stmt>    execute SQL on the system database");
    serial_println!();
    serial_println!("Lua:");
    serial_println!("  lua             interactive Lua REPL");
    serial_println!("  run <path>      execute a Lua agent from namespace");
    serial_println!("  store <p> <c>   store Lua script at path");
    serial_println!();
    serial_println!("Claude API:");
    serial_println!("  apikey <key>     set Anthropic API key");
    serial_println!("  resolve <ip>     set api.anthropic.com IP (override DNS)");
    serial_println!("  ask <prompt>     send message via TLS (auto-resolves DNS)");
    serial_println!("  askp <prompt>    send message via proxy (plain HTTP)");
    serial_println!("  agent <prompt>   agentic loop with tool use (read/write/sql)");
    serial_println!("  agentp <prompt>  agentic loop via proxy");
    serial_println!("  model <name>     set model (default: claude-sonnet-4-6-20250514)");
    serial_println!("  pin [show|set]   manage TLS certificate SPKI pin");
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
    let total_secs = crate::arch::x86_64::timer::uptime_secs();
    let hours = total_secs / 3600;
    let mins = (total_secs % 3600) / 60;
    let secs = total_secs % 60;
    serial_println!("up {}h {:02}m {:02}s", hours, mins, secs);
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
    // Map well-known paths to synthetic content
    match path {
        "/sys/meminfo" | "sys/meminfo" => { cmd_meminfo(); return; }
        "/sys/uptime" | "sys/uptime" => { cmd_uptime(); return; }
        "/hw/nvme/info" | "hw/nvme/info" => { cmd_nvme_info(); return; }
        "/db/schema" | "db/schema" => {
            match crate::sqlite::exec_and_format(
                "SELECT sql FROM sqlite_master WHERE type='table' ORDER BY name"
            ) {
                Ok(out) => serial_print!("{}", out),
                Err(e) => serial_println!("error: {}", e),
            }
            return;
        }
        _ => {}
    }

    // Try reading from the namespace table (structured query — handles all content)
    let guard = crate::sqlite::DB.lock();
    if let Some(db) = guard.as_ref() {
        let query = alloc::format!(
            "SELECT content FROM namespace WHERE path='{}'",
            path.replace('\'', "''")
        );
        if let Ok(Some(content)) = db.query_value(&query) {
            drop(guard);
            serial_println!("{}", content);
            return;
        }
    }
    drop(guard);
    serial_println!("cat: {}: not found", path);
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

fn cmd_resolve(ip_str: &str) {
    if ip_str.is_empty() {
        let current = *API_TARGET_IP.lock();
        if current == Ipv4Address::new(0, 0, 0, 0) {
            serial_println!("API target: not set (will use DNS)");
            serial_println!("usage: resolve <ip>  (manual override)");
        } else {
            serial_println!("API target: {} (manual override)", current);
        }
        return;
    }

    // Parse IPv4 address
    let octets: alloc::vec::Vec<&str> = ip_str.split('.').collect();
    if octets.len() != 4 {
        serial_println!("Invalid IP format. Use: resolve 1.2.3.4");
        return;
    }
    let mut bytes = [0u8; 4];
    for (i, octet) in octets.iter().enumerate() {
        match octet.parse::<u8>() {
            Ok(b) => bytes[i] = b,
            Err(_) => {
                serial_println!("Invalid IP octet: {}", octet);
                return;
            }
        }
    }
    let ip = Ipv4Address::new(bytes[0], bytes[1], bytes[2], bytes[3]);
    *API_TARGET_IP.lock() = ip;
    serial_println!("API target set to: {}", ip);
}

fn cmd_model(name: &str) {
    if name.is_empty() {
        let current = crate::api::get_model();
        serial_println!("current model: {}", current);
        serial_println!("usage: model <name>");
    } else {
        crate::api::set_model(name);
        serial_println!("model set to: {}", name);
    }
}

fn cmd_ask(prompt: &str, use_tls: bool) {
    // Check API key
    let api_key = match crate::api::get_api_key() {
        Some(k) => k,
        None => {
            serial_println!("Error: API key not set. Run: apikey sk-ant-...");
            return;
        }
    };

    // Check network stack
    let mut net_guard = crate::net::NET_STACK.lock();
    let net = match net_guard.as_mut() {
        Some(n) => n,
        None => {
            serial_println!("Error: network stack not initialized");
            serial_println!("  (need virtio-net device in QEMU)");
            return;
        }
    };

    // Build config based on mode
    let config = if use_tls {
        // Check manual IP override first, then try DNS
        let target_ip = {
            let manual = *API_TARGET_IP.lock();
            if manual != Ipv4Address::new(0, 0, 0, 0) {
                serial_println!("[resolve: {} (manual)]", manual);
                manual
            } else {
                // Try DNS resolution
                serial_println!("[DNS resolve: api.anthropic.com...]");
                match crate::net::dns::resolve_a(net, "api.anthropic.com") {
                    Ok(ip) => {
                        serial_println!("[resolved: {}]", ip);
                        ip
                    }
                    Err(e) => {
                        serial_println!("Error: DNS resolution failed: {}", e);
                        serial_println!("  Fallback: resolve <ip>  (manual)");
                        serial_println!("  Get IP on host: dig +short api.anthropic.com");
                        return;
                    }
                }
            }
        };

        serial_println!("[TLS to {}:443...]", target_ip);
        crate::api::ClaudeConfig {
            api_key,
            model: crate::api::get_model(),
            ..crate::api::ClaudeConfig::direct_tls(target_ip)
        }
    } else {
        serial_println!("[proxy mode: 10.0.2.2:8080...]");
        crate::api::ClaudeConfig {
            api_key,
            model: crate::api::get_model(),
            ..crate::api::ClaudeConfig::default_proxy()
        }
    };

    serial_println!();

    // Send request and stream response
    match crate::api::claude_request(net, &config, prompt, |token| {
        serial_print!("{}", token);
    }) {
        Ok(_) => {
            serial_println!();
        }
        Err(e) => {
            serial_println!();
            serial_println!("[API error: {}]", e);
            if use_tls {
                serial_println!();
                serial_println!("TLS troubleshooting:");
                serial_println!("  1. Verify QEMU has internet: -netdev user,id=net0");
                serial_println!("  2. Fallback: resolve <ip>  (manual override)");
                serial_println!("  3. Fallback: askp <prompt> (uses socat proxy)");
            } else {
                serial_println!();
                serial_println!("Proxy troubleshooting:");
                serial_println!("  socat TCP-LISTEN:8080,fork,reuseaddr \\");
                serial_println!("    OPENSSL:api.anthropic.com:443");
            }
        }
    }
}

fn cmd_pin(sub: &str, arg: &str) {
    match sub {
        "show" | "" => {
            if let Some(pin) = crate::crypto::pin_verifier::get_pin_override() {
                serial_println!("SPKI pin (runtime override):");
                serial_print!("  ");
                for b in &pin {
                    serial_print!("{:02x}", b);
                }
                serial_println!();
            } else {
                serial_println!("SPKI pin: using compiled-in pins");
                serial_println!("  Pinning enforcement: {}", if crate::api::ENFORCE_PINNING { "ON" } else { "OFF" });
            }
        }
        "set" => {
            if arg.is_empty() {
                serial_println!("usage: pin set <64-hex-chars>");
                serial_println!("  Get pin: openssl s_client -connect api.anthropic.com:443 \\");
                serial_println!("    | openssl x509 -pubkey -noout \\");
                serial_println!("    | openssl pkey -pubin -outform der \\");
                serial_println!("    | openssl dgst -sha256 -binary | xxd -p -c32");
                return;
            }
            match parse_hex_hash(arg) {
                Some(hash) => {
                    crate::crypto::pin_verifier::set_pin_override(hash);
                    serial_println!("SPKI pin override set ({} bytes)", hash.len());
                }
                None => {
                    serial_println!("Invalid hex hash. Expected 64 hex characters (32 bytes SHA-256).");
                }
            }
        }
        "clear" => {
            crate::crypto::pin_verifier::clear_pin_override();
            serial_println!("SPKI pin override cleared. Using compiled-in pins.");
        }
        _ => {
            serial_println!("usage: pin [show|set <hex>|clear]");
        }
    }
}

/// Parse a 64-character hex string into a 32-byte array.
fn parse_hex_hash(hex: &str) -> Option<[u8; 32]> {
    let hex = hex.trim();
    if hex.len() != 64 {
        return None;
    }
    let mut result = [0u8; 32];
    for i in 0..32 {
        let byte_str = &hex[i * 2..i * 2 + 2];
        result[i] = u8::from_str_radix(byte_str, 16).ok()?;
    }
    Some(result)
}

fn cmd_sql(query: &str) {
    match crate::sqlite::exec_and_format(query) {
        Ok(output) => {
            serial_print!("{}", output);
        }
        Err(e) => {
            serial_println!("SQL error: {}", e);
        }
    }
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

fn cmd_run(path: &str) {
    serial_println!("[lua] running agent: {}", path);
    match crate::lua::run_agent(path) {
        Ok(()) => serial_println!("[lua] agent finished."),
        Err(e) => serial_println!("[lua] error: {}", e),
    }
}

fn cmd_store(path: &str, code: &str) {
    let guard = crate::sqlite::DB.lock();
    let db = match guard.as_ref() {
        Some(db) => db,
        None => {
            serial_println!("error: database not open");
            return;
        }
    };

    let query = alloc::format!(
        "INSERT OR REPLACE INTO namespace (path, type, content, mtime) \
         VALUES ('{}', 'lua', '{}', strftime('%s','now'))",
        path.replace('\'', "''"),
        code.replace('\'', "''")
    );

    match db.exec(&query) {
        Ok(()) => serial_println!("stored: {} ({} bytes)", path, code.len()),
        Err(e) => serial_println!("error: {}", e),
    }
}

fn cmd_agent(prompt: &str, use_tls: bool) {
    serial_println!("[agent] Starting agentic loop...");
    match super::agent::run_agent_loop(prompt, use_tls) {
        Ok(_) => {
            serial_println!("[agent] Done.");
        }
        Err(e) => {
            serial_println!("[agent] Error: {}", e);
            if use_tls {
                serial_println!("  Fallback: agentp <prompt> (uses proxy)");
            }
        }
    }
}

fn cmd_lua_repl() {
    crate::lua::repl::run();
}
