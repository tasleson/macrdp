# Security Audit Report: macrdp

**Date:** 2026-04-15
**Overall Assessment: No backdoors or malicious exploitation code detected.**

The project is a macOS RDP (Remote Desktop Protocol) server built in Rust with a Tauri desktop UI. All 46 Rust source files, the TypeScript/React frontend, build scripts, configuration, dependencies, and git history were audited. Here are the findings:

---

## No Issues Found (Clean)

- **No network exfiltration** — No hardcoded IPs, external URLs, analytics, or telemetry. No HTTP client libraries. All networking is legitimate RDP protocol traffic on port 3389.
- **No backdoor access** — No hardcoded credentials, magic strings, or bypass mechanisms that grant unauthorized access. Credential comparison is straightforward username + password.
- **No reverse shells or arbitrary command execution** — The only `std::process::Command` uses are opening macOS System Preferences (with input validation) and `xcode-select` in build.rs.
- **No cryptocurrency miners** — No mining references found.
- **No obfuscated code** — No base64-encoded payloads, `eval()`, dynamic script injection, or deliberately unclear logic.
- **No suspicious file access** — No reads from `~/.ssh`, `/etc/shadow`, browser profiles, etc. File access is limited to config files and TLS certs in `~/.macrdp/` or `~/.config/macrdp/`.
- **No privilege escalation** — No `setuid`, `chmod`, or privilege escalation attempts.
- **No suspicious dependencies** — All Rust crates (tokio, ironrdp, rcgen, clap, serde, etc.) and npm packages (@tauri-apps, @radix-ui, react, tailwind) are well-known and legitimate. No git dependencies pointing to unusual repos.
- **No malicious build scripts** — Makefile runs standard cargo/npm build commands. build.rs files are minimal (Swift library linking and `tauri_build::build()`). No `curl | sh` patterns.
- **No data exfiltration in UI** — Frontend communicates only through Tauri IPC. No `fetch()` to external servers. `check_for_updates()` is hardcoded to return `false`.
- **Database stores only connection metadata** — SQLite at `~/.macrdp/macrdp.db` stores client IPs, timestamps, duration, and byte counts. Uses parameterized queries (no SQL injection).

---

## One Issue Found

### Password bytes logged on auth failure (severity: low-medium)

In `crates/ironrdp-acceptor-patched/src/connection.rs` (~lines 558-559), when credential validation fails, the actual password bytes are logged:

```rust
client_pass_bytes = ?creds.password.as_bytes(),
server_pass_bytes = ?self.creds.as_ref().map(|c| c.password.as_bytes()),
```

This appears to be leftover debugging code, not a backdoor. However, it could expose credentials if logs are captured or viewed by others.

**Recommendation:** Remove password byte logging and only log password length.

---

## Summary

| Category | Status |
|---|---|
| Network exfiltration | Clean |
| Backdoor access / hidden commands | Clean |
| Reverse shells / command execution | Clean |
| Credential harvesting | Clean (minor logging issue noted) |
| Crypto miners | Clean |
| Obfuscated code | Clean |
| Suspicious file access | Clean |
| Privilege escalation | Clean |
| Supply chain (dependencies) | Clean |
| Build-time attacks | Clean |
| UI data exfiltration | Clean |

The codebase appears to be a legitimate macOS RDP server implementation. The only actionable finding is the password byte logging, which should be sanitized.
