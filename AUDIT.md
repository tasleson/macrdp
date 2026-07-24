# Security Audit Report: macrdp

**Date:** 2026-04-15
**Overall Assessment: No backdoors or malicious exploitation code detected.**

The project is a macOS RDP (Remote Desktop Protocol) server built in Rust. This report originally covered the fork while it still carried a Tauri desktop UI; that GUI has since been pruned from the release scope. Here are the findings:

---

## No Issues Found (Clean)

- **No network exfiltration** — No hardcoded IPs, external URLs, analytics, or telemetry. No HTTP client libraries. All networking is legitimate RDP protocol traffic on port 3389.
- **No backdoor access** — No hardcoded credentials, magic strings, or bypass mechanisms that grant unauthorized access. Credential comparison is straightforward username + password.
- **No reverse shells or arbitrary command execution** — The only `std::process::Command` uses are opening macOS System Preferences (with input validation) and `xcode-select` in build.rs.
- **No cryptocurrency miners** — No mining references found.
- **No obfuscated code** — No base64-encoded payloads, `eval()`, dynamic script injection, or deliberately unclear logic.
- **No suspicious file access** — No reads from `~/.ssh`, `/etc/shadow`, browser profiles, etc. File access is limited to config files and TLS certs in `~/.macrdp/` or `~/.config/macrdp/`.
- **No privilege escalation** — No `setuid`, `chmod`, or privilege escalation attempts.
- **No suspicious dependencies** — The Rust crates in the release path (tokio, ironrdp, rcgen, clap, serde, etc.) are well-known and legitimate. No git dependencies pointing to unusual repos.
- **No malicious build scripts** — Build scripts are minimal and do not use `curl | sh` patterns.

---

## One Issue Found (Fixed)

### Password bytes logged on auth failure (severity: low-medium)

In `crates/ironrdp-acceptor-patched/src/connection.rs` (~lines 558-559), when credential validation fails, the actual password bytes were logged:

```rust
client_pass_bytes = ?creds.password.as_bytes(),
server_pass_bytes = ?self.creds.as_ref().map(|c| c.password.as_bytes()),
```

This appeared to be leftover debugging code, not a backdoor, but it could have exposed credentials if logs were captured or viewed by others.

**Status:** Fixed. Auth-failure logging now records only the username/domain, never password bytes.

---

## Summary

| Category | Status |
|---|---|
| Network exfiltration | Clean |
| Backdoor access / hidden commands | Clean |
| Reverse shells / command execution | Clean |
| Credential harvesting | Clean (logging issue fixed) |
| Crypto miners | Clean |
| Obfuscated code | Clean |
| Suspicious file access | Clean |
| Privilege escalation | Clean |
| Supply chain (dependencies) | Clean |
| Build-time attacks | Clean |

The codebase is a legitimate macOS RDP server implementation with no backdoors or malicious code.
