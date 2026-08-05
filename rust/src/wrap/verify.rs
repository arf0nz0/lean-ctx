//! MCP connection probe: spawns `lean-ctx mcp`, sends a JSON-RPC
//! `initialize` + `tools/list`, and checks that `ctx_read` is present.

use std::io::{BufRead, Write};
use std::process::{Command, Stdio};

const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

pub(super) fn probe_mcp_server(binary: &str) -> bool {
    match probe_inner(binary) {
        Ok(true) => {
            eprintln!("  MCP probe: \x1b[32mctx_read confirmed\x1b[0m");
            true
        }
        Ok(false) => {
            eprintln!("  MCP probe: tools listed but ctx_read not found");
            false
        }
        Err(e) => {
            tracing::debug!("MCP probe failed: {e}");
            eprintln!("  MCP probe: \x1b[33mskipped\x1b[0m (will verify on first IDE call)");
            false
        }
    }
}

fn probe_inner(binary: &str) -> Result<bool, String> {
    let mut child = Command::new(binary)
        .args(["mcp"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("spawn: {e}"))?;

    let stdin = child.stdin.as_mut().ok_or("no stdin")?;

    let init_request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "lean-ctx-wrap-probe", "version": "1.0" }
        }
    });
    let req = serde_json::to_string(&init_request).map_err(|e| format!("serialize init: {e}"))?;
    writeln!(stdin, "Content-Length: {}\r\n\r\n{req}", req.len())
        .map_err(|e| format!("write init: {e}"))?;

    let initialized_notif = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    });
    let notif =
        serde_json::to_string(&initialized_notif).map_err(|e| format!("serialize notif: {e}"))?;
    writeln!(stdin, "Content-Length: {}\r\n\r\n{notif}", notif.len())
        .map_err(|e| format!("write notif: {e}"))?;

    let list_request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list",
        "params": {}
    });
    let list = serde_json::to_string(&list_request).map_err(|e| format!("serialize list: {e}"))?;
    writeln!(stdin, "Content-Length: {}\r\n\r\n{list}", list.len())
        .map_err(|e| format!("write list: {e}"))?;
    stdin.flush().map_err(|e| format!("flush: {e}"))?;

    let stdout = child.stdout.take().ok_or("no stdout")?;
    let reader = std::io::BufReader::new(stdout);

    let found = std::thread::scope(|s| {
        let handle = s.spawn(|| {
            for line in reader.lines() {
                let Ok(line) = line else { break };
                if line.contains("ctx_read") {
                    return true;
                }
            }
            false
        });

        std::thread::sleep(PROBE_TIMEOUT);
        let _ = child.kill();
        handle.join().unwrap_or(false)
    });

    Ok(found)
}
