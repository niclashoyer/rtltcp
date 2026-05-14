# rtltcp Stability Improvement Project Plan

## Executive Summary

This plan addresses critical stability issues in the rtltcp codebase that cause production crashes on Ubuntu 24 LTS / ARM. The crash report confirms multiple panic scenarios triggered by normal network events (client disconnect, connection reset). The codebase has **4 critical bugs**, **5 high-severity issues**, and numerous medium/low improvements needed.

---

## Problem Statement

### Production Crash Evidence (crash.report.txt)

Three distinct crash scenarios identified from live systemd logs:

| Timestamp | Trigger | Panic Location | Error |
|-----------|---------|---------------|-------|
| 01:29:47 | Client disconnect (EOF) | `main.rs:93` + `main.rs:160` | `UnexpectedEof: "failed to fill whole buffer"` → `Any { .. }` |
| 01:35:17 | Connection reset | `main.rs:93` + `main.rs:154` | `ConnectionReset by peer` → `can't exit normally: "Disconnected(..)"` |
| 01:52:17 | Connection reset | `main.rs:93` + `main.rs:154` | `ConnectionReset by peer` → `can't exit normally: "Disconnected(..)"` |

**Crash pattern:** Client disconnect → control thread panics on line 93 → mutex poisoned → main thread panics on line 154 or 160 → systemd restart loop.

### Root Causes

1. **Line 93**: `stream.read_exact(&mut buf).unwrap()` — panics on any I/O error
2. **Line 150** (plan reference: actual line 150): `receiver.recv().unwrap()` — panics if sender drops
3. **Line 166** (plan reference: actual line 165): `sender.try_send(()).expect("can't exit normally")` — panics on channel failure
4. **Lines 101, 106, 113, 117, 123, 128, 134, 137**: All device operations use `.unwrap()` — panic on any device error
5. **Lines 171-172**: `thread.join().unwrap()` — double-panic on thread failure
6. **Line 64**: **Does not compile** — `format!("{}:{}", args, address, args.port)` is a syntax error
7. **Line 75**: `std::process::exit(0)` — bypasses all cleanup
8. **Line 169**: `reader.read_async(...).unwrap()` — panics if async read returns an error

### Additional Issues Identified by Code Review

9. **Control thread hangs on normal `read_async` completion**: If `read_async` returns normally (not cancelled), no signal is sent to `thread_cancel`, which blocks forever on `receiver.recv()`. `thread_cancel.join()` then blocks forever — daemon hangs instead of exiting.

10. **`should_exit` flag is ineffective**: Lines 93-96 check `should_exit` AFTER `stream.read_exact()`. If the client disconnects, `read_exact` errors first (even after fixing the `.unwrap()`, it returns `Err`). The exit flag is never reached unless a successful read happens first. The proposed fix to handle read errors (break on disconnect) addresses this, but the flag remains redundant for the control thread path.

11. **Command 0x03 logic bug (functional, not cosmetic)**: When `gain_mode > 0`, the code logs "manual tuner gain requested" then "setting automatic gain control to on" and calls `enable_agc()`. Per the rtl-tcp protocol, `gain_mode > 0` means manual gain (AGC disabled), not automatic. The code does the opposite of the protocol spec. This is a functional bug, not just a logging issue.

12. **Mutex poisoning cascade**: One device error → panic → mutex poisoned → every subsequent `.lock().unwrap()` panics. This is addressed by Task 1.2 but is the root cause of the multi-panic crash chains in the crash report.

13. **Zero-capacity channel causes immediate `process::exit`**: `sync_channel(0)` requires both ends ready simultaneously. If the control thread is blocked on `read_exact`, `try_send` in the Ctrl-C handler always fails → `process::exit(0)` triggered. This is the entry point for the hard exit path.

---

## Phased Implementation Plan

### WAVE 1: Emergency Stabilization (Days 1-2)

**Goal:** Stop the crashes. The code must compile and handle disconnections without panicking.

#### Task 1.1: Fix Compile Error
- **File:** `src/main.rs`, line 64
- **Change:** `format!("{}:{}", args.address, args.port)`
- **Risk:** None — this is a syntax fix
- **Validation:** `cargo build` succeeds without `daemon_systemd` feature

#### Task 1.2: Replace All `.unwrap()` on I/O and Device Operations
- **File:** `src/main.rs`
- **Note on line numbers:** Line references in this plan are approximate — verify against source during implementation. Known offsets: `receiver.recv().unwrap()` is at line 150, `sender.try_send()` is at line 166.
- **Changes:**
  - **Line 93**: Replace `stream.read_exact(&mut buf).unwrap()` with:
    ```rust
    match stream.read_exact(&mut buf) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof ||
                  e.kind() == std::io::ErrorKind::ConnectionReset ||
                  e.kind() == std::io::ErrorKind::BrokenPipe => {
            info!("client disconnected");
            break;
        }
        Err(e) => {
            info!("read error: {}", e);
            break;
        }
    }
    ```
  - **Lines 101, 106, 113, 117, 123, 128, 134, 137**: Replace `.unwrap()` on all `ctl.lock().unwrap().set_*().unwrap()` chains with:
    ```rust
    match ctl.lock() {
        Ok(mut guard) => {
            if let Err(e) = guard.set_center_freq(freq) {
                info!("failed to set center freq: {}", e);
            }
        }
        Err(poisoned) => {
            info!("mutex poisoned, exiting control thread");
            break;
        }
    }
    ```
  - **Line 150**: Replace `receiver.recv().unwrap()` with `let _ = receiver.recv();`
  - **Line 152**: Replace `ctl.lock().unwrap().cancel_async_read()` with:
    ```rust
    if let Ok(mut guard) = ctl.lock() {
        guard.cancel_async_read();
    }
    ```
  - **Line 166**: Replace `sender.try_send(()).expect(...)` with `let _ = sender.try_send(());`
  - **Lines 171-172**: Replace `thread.join().unwrap()` with graceful error logging
  - **Line 169**: Handle `read_async(...).unwrap()` result:
    ```rust
    let read_result = reader.read_async(args.buffers, 0, |bytes| {
        // ... callback unchanged
    });
    if let Err(e) = read_result {
        info!("read_async error: {}", e);
    }
    ```

- **Risk:** Low — behavior change is from "crash" to "log and continue/exit"
- **Validation:** No panics on simulated disconnect; `cargo clippy` clean

#### Task 1.2b: Fix Normal Completion Hang (Critical — Missing from Initial Plan)
- **File:** `src/main.rs`, around lines 163-172
- **Issue:** If `read_async` returns normally (not via `cancel_async_read`), no signal is ever sent to `thread_cancel`, which blocks forever on `receiver.recv()`. Then `thread_cancel.join()` blocks forever — daemon hangs.
- **Fix:** Use `Arc<AtomicBool>` to signal from the `read_async` callback path too:
  ```rust
  let (sender, receiver) = sync_channel(1);
  let sender_ctrlc = sender.clone();
  let sender_done = sender.clone(); // NEW: signal when read_async completes normally
  let should_exit = Arc::new(AtomicBool::new(false));

  // In read_async callback (line 165):
  reader.read_async(args.buffers, 0, |bytes| {
      // ... existing write logic
  }).ok(); // Don't panic on error
  // After read_async returns, send signal so cancel thread wakes:
  let _ = sender_done.try_send(());

  // In cancel thread (line 150):
  receiver.recv().ok(); // Handle both signal sources gracefully
  ```
- **Risk:** Low — ensures clean exit on all completion paths
- **Validation:** Binary exits cleanly after client disconnects (not just after Ctrl-C)

#### Task 1.3: Fix Rendezvous Channel Deadlock
- **File:** `src/main.rs`, line 67
- **Change:** `sync_channel(0)` → `sync_channel(1)`
- **Rationale:** Zero-capacity channel requires both ends ready simultaneously. When control thread is blocked on `stream.read_exact()`, Ctrl-C handler's `try_send` always fails (channel has no buffer), triggering `process::exit(0)` — bypassing all cleanup. A 1-slot buffer allows the signal to queue until the receiver is ready.
- **Risk:** None — 1-slot buffer is sufficient for a single exit signal
- **Interaction with Task 1.4:** After Task 1.4 removes `process::exit(0)`, this becomes less critical for crash prevention, but still improves robustness for normal exit signaling.

#### Task 1.4: Replace `process::exit(0)` with Graceful Signal
- **File:** `src/main.rs`, lines 70-78
- **Change:** Remove `std::process::exit(0)` from the Ctrl-C handler. Instead, only rely on `sender.try_send(())` and the channel's signaling mechanism. If `try_send` fails, do nothing — the program will exit gracefully when the control thread detects the disconnect or when `should_exit` is set.
- **Rationale:** `process::exit(0)` bypasses `Drop` for the `BufWriter`, skipping final data flush, device cleanup, and thread joining. After switching to `sync_channel(1)` in Task 1.3, the `try_send` should succeed reliably. Even if it fails, the program should not hard-exit.
- **Risk:** Low — changes from hard exit to cooperative shutdown

**Wave 1 Deliverables:**
- [ ] Code compiles with and without `daemon_systemd` feature
- [ ] No panics on client disconnect (tested with `nc` + kill)
- [ ] No panics on device errors (tested with USB unplug simulation)
- [ ] Clean shutdown on Ctrl-C (device closed, threads joined)
- [ ] `cargo clippy --all-targets --all-features -- -D warnings` passes

---

### WAVE 2: Structural Improvements (Days 3-5)

**Goal:** Make the code maintainable, documented, and robust.

#### Task 2.1: Define Named Constants for Protocol Values
- **File:** `src/main.rs` (top of file)
- **Add:**
  ```rust
  const COMMAND_HEADER_SIZE: usize = 5;

  /// RTL-TCP protocol command codes
  const CMD_SET_FREQUENCY: u8 = 0x01;
  const CMD_SET_SAMPLE_RATE: u8 = 0x02;
  const CMD_SET_GAIN_MODE: u8 = 0x03;
  const CMD_SET_TUNER_GAIN: u8 = 0x04;
  const CMD_SET_PPM: u8 = 0x05;
  const CMD_SET_AGC: u8 = 0x08;

  /// Magic packet sent to client on connect:
  /// "RTL0" (4 bytes) + tuner type 5 (4 bytes BE) + max gain value 0x1d (4 bytes BE)
  const MAGIC_PACKET: &[u8] = b"RTL0\x00\x00\x00\x05\x00\x00\x00\x1d";
  ```
- **Replace:** All magic numbers and byte sequences throughout the control thread and handshake
- **Note:** The magic packet is 12 bytes (not 8). Use the `&[u8]` slice constant and `.write_all(MAGIC_PACKET)` instead of constructing a `Vec`.
- **Risk:** None — purely cosmetic/maintainability

#### Task 2.2: Define Custom Error Type
- **File:** `src/main.rs` (new module or top-level)
- **Add:**
  ```rust
  #[derive(Debug)]
  enum Error {
      Io(std::io::Error),
      DeviceOpen(String),
      DeviceControl(String),
      ChannelSend,
      MutexPoisoned,
  }
  impl std::fmt::Display for Error { ... }
  impl std::error::Error for Error { ... }
  impl From<std::io::Error> for Error { ... }
  ```
- **Change:** `main() -> Result<(), Error>` instead of `Result<(), Box<dyn std::error::Error>>`
- **Risk:** Low — internal type, no API break

#### Task 2.3: Fix Command 0x03 Logic Bug (Functional, Not Cosmetic)
- **File:** `src/main.rs`, lines 108-118
- **Current behavior:**
  - `gain_mode > 0` (manual gain requested) → logs "manual tuner gain requested" + "setting automatic gain control to on" + calls `enable_agc()`
  - `gain_mode <= 0` → logs "disabling agc" + calls `disable_agc()`
- **Per the rtl-tcp protocol:** `gain_mode > 0` means manual gain (AGC disabled); `gain_mode == 0` means automatic gain (AGC enabled). The code does the opposite.
- **Fix options:**
  1. **If the implementation is wrong** (matches original C code behavior): Invert the logic — call `enable_agc()` when `gain_mode == 0`, `disable_agc()` when `gain_mode > 0`. Verify against the [original rtl_tcp.c](https://github.com/pinkavaj/rtl-sdr/blob/master/src/rtl_tcp.c).
  2. **If the implementation intentionally inverts the protocol** (to make "manual gain" feel more intuitive): Document this clearly in a comment and leave unchanged.
- **Recommendation:** Investigate the original C implementation first. If it matches this code, the bug predates this project and may be intentional. Add a code comment either way.
- **Risk:** Medium — changing this could break clients that depend on the current (possibly inverted) behavior. Test with known-good client.
- **This is not just a logging fix** — the actual device calls (`enable_agc()` / `disable_agc()`) are inverted relative to the protocol spec.

#### Task 2.4: Add BufWriter Flush Before Shutdown
- **File:** `src/main.rs`, before thread joins
- **Add:** `buf_write_stream.flush()?;` before `thread_cancel.join()`
- **Risk:** Low — ensures buffered data is sent before exit

#### Task 2.5: Add `[profile.release]` to Cargo.toml
- **File:** `Cargo.toml`
- **Add:**
  ```toml
  [profile.release]
  lto = true
  codegen-units = 1
  strip = "symbols"
  ```
- **⚠️ Important: Do NOT add `panic = "abort"`**
  - With `panic = "abort"`, cleanup code (BufWriter flush, device closing, thread joining, systemd notification) **will not run** on panic.
  - For a daemon that manages USB hardware, unclean abort leaves the device in an undefined state.
  - The default `panic = "unwind"` allows `Drop` implementations to run, ensuring the RTL-SDR device is closed properly.
  - If binary size is a concern, `strip = "symbols"` and `lto = true` provide the most benefit without the cleanup penalty.

#### Task 2.6: Add `rust-version` to Cargo.toml
- **File:** `Cargo.toml`
- **Add:** `rust-version = "1.74"` (minimum for clap 4.5)
- **Risk:** None — metadata only

#### Task 2.7: Verify `ctrlc` SIGTERM Handling (Before Adding Dependencies)
- **File:** `src/main.rs`, current `ctrlc::set_handler` call
- **Issue:** Task 4.4 proposes adding SIGTERM handling. However, `ctrlc` already handles SIGINT, SIGTERM, and SIGHUP on Unix by default.
- **Action:** Test that the existing `ctrlc` handler catches SIGTERM before adding a dependency like `signal-hook`. Run `kill -TERM <pid>` on the running binary and verify clean shutdown.
- **If `ctrlc` handles SIGTERM correctly**: No additional dependency needed.
- **If `ctrlc` does NOT handle SIGTERM**: Add `signal-hook` dependency for broader signal support.
- **Risk:** None — investigation task, may eliminate a dependency entirely.

**Wave 2 Deliverables:**
- [ ] All magic numbers replaced with named constants (including 12-byte MAGIC_PACKET)
- [ ] Custom error type with Display/From implementations
- [ ] Command 0x03 logic investigated and corrected (or documented if intentionally inverted)
- [ ] BufWriter flushed before shutdown
- [ ] Release profile optimized for size/speed (no `panic = "abort"`)
- [ ] MSRV declared in Cargo.toml
- [ ] `ctrlc` SIGTERM support verified (no extra dependency if already supported)
- [ ] `cargo clippy` clean

---

### WAVE 3: CI/CD Modernization (Days 6-7)

**Goal:** Update all abandoned actions and fix cross-compilation.

#### Task 3.1: Replace Abandoned `actions-rs/*` Actions
- **Files:** `.github/workflows/ci.yml`, `audit.yml`, `cd.yml`
- **Replacements:**
  | Old | New |
  |-----|-----|
  | `actions-rs/toolchain@v1` | `dtolnay/rust-toolchain@stable` |
  | `actions-rs/cargo@v1` | `run: cargo ...` (direct shell) |
  | `actions-rs/tarpaulin@v0.1` | `taiki-e/install-action@cargo-tarpaulin` |
  | `actions-rs/audit-check@v1` | `rustsec/audit-check@v2` |

#### Task 3.2: Update All Outdated Actions
- **Replacements:**
  | Old | New |
  |-----|-----|
  | `actions/checkout@v2` | `actions/checkout@v4` |
  | `Swatinem/rust-cache@v1` | `Swatinem/rust-cache@v2` |
  | `softprops/action-gh-release@v1` | `softprops/action-gh-release@v2` |
  | `coverallsapp/github-action@master` | `coverallsapp/github-action@v2` |

#### Task 3.3: Fix Cross-Compilation in CD Workflow
- **File:** `.github/workflows/cd.yml`
- **Issue:** `librtlsdr-dev` installed via apt but cross-compilation needs cross-compiled library
- **Fix:** Use `cross` with custom Docker images that include `librtlsdr` or switch to `cargo-zigbuild`
- **Alternative:** Remove non-x86 targets from CD until cross-compilation dependencies are resolved

#### Task 3.4: Add MSRV Test Job
- **File:** `.github/workflows/ci.yml`
- **Add:** Job that runs with `toolchain: "1.74"` to verify MSRV compatibility
- **Risk:** None — additive CI check

**Wave 3 Deliverables:**
- [ ] No references to `actions-rs/*` in any workflow
- [ ] All actions pinned to current versions
- [ ] CD cross-compilation builds succeed (or targets removed)
- [ ] MSRV CI job passes
- [ ] `cargo audit` passes with no critical vulnerabilities

---

### WAVE 4: Testing & Hardening (Days 8-10)

**Goal:** Add tests, improve error resilience, and prepare for production use.

#### Task 4.1: Add Unit Tests for Protocol Parsing
- **File:** `src/main.rs` (add `#[cfg(test)]` module)
- **Tests:**
  - Parse command 0x01 (frequency) from byte array
  - Parse command 0x02 (sample rate) from byte array
  - Parse command 0x03 (gain mode) from byte array
  - Parse command 0x04 (tuner gain) from byte array
  - Parse command 0x05 (PPM) from byte array
  - Parse command 0x08 (AGC) from byte array
  - Unknown command handling
- **Risk:** None — tests only

#### Task 4.2: Add Integration Test Stub
- **File:** `tests/integration.rs`
- **Test:** Verify binary starts, binds to port, accepts connection (mock device if needed)
- **Note:** Full integration requires hardware; test what can be tested without RTL-SDR dongle
- **Risk:** Low — mock-based tests

#### Task 4.3: Add Input Validation
- **File:** `src/main.rs`, Args struct
- **Add:** Validation for:
  - `address` must be a valid IP/hostname
  - `port` must be > 0 and < 65536
  - `buffers` must be > 0 and <= 32 (reasonable limit)
  - `tcp_buffers` must be > 0 and <= 10MB
- **Risk:** Low — additive validation

#### Task 4.4: Verify and Enhance SIGTERM Handling
- **File:** `src/main.rs`
- **Current:** Only handles SIGINT (Ctrl-C) via `ctrlc` crate
- **Depends on:** Task 2.7 (SIGTERM verification)
- **Action:** If `ctrlc` already handles SIGTERM (expected on Unix), verify it triggers the same graceful shutdown path as Ctrl-C. If `ctrlc` does not handle SIGTERM, add `signal-hook` dependency.
- **Test:** Run `kill -TERM <pid>` on running binary; verify clean shutdown with `journalctl -u rtltcp1234.service`
- **Risk:** Low — additive signal handling

#### Task 4.5: Add Connection Rejection for Multiple Clients
- **File:** `src/main.rs`
- **Current:** Accepts one connection, then blocks
- **Add:** If a second client connects while one is active, reject with error message
- **Risk:** Low — prevents confusing behavior with multiple SDR consumers

**Wave 4 Deliverables:**
- [ ] At least 6 unit tests for command parsing
- [ ] Integration test stub (can run without hardware)
- [ ] CLI argument validation with user-friendly error messages
- [ ] SIGTERM verified as handled by `ctrlc` (or `signal-hook` added if needed)
- [ ] Multiple client connection handling documented/rejected
- [ ] `cargo test --all-features` passes
- [ ] Coverage target: > 50% on protocol parsing code

---

## Risk Assessment

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| Breaking existing client compatibility | Low | High | Preserve rtl-tcp protocol exactly; only change error handling; test with known client |
| `rtlsdr_mt` API changes in newer versions | Medium | Medium | Pin dependency version; test with current version |
| Cross-compilation complexity | High | Medium | Remove non-x86 CD targets temporarily; fix separately |
| Command 0x03 logic change breaks clients | Medium | Medium | Verify against original C implementation before changing; if inverted behavior is intentional, document it |
| Daemon hangs instead of exits (normal read_async completion) | High | Medium | Task 1.2b addresses this specifically |
| `panic = "abort"` leaves USB device unclean | High | High | Do NOT include `panic = "abort"` in release profile |
| systemd notification timing | Low | Medium | Test socket activation thoroughly after changes |
| `ctrlc` doesn't handle SIGTERM on all platforms | Low | Low | Task 2.7 verifies before adding dependency |

---

## Success Criteria

### Must Have (Wave 1-2)
- [ ] No panics in 24-hour production test with simulated disconnects
- [ ] No panics when USB dongle is unplugged during operation
- [ ] Clean shutdown via Ctrl-C and SIGTERM
- [ ] Code compiles with all feature combinations
- [ ] `cargo clippy` clean on all targets

### Should Have (Wave 3)
- [ ] CI/CD uses current, maintained actions
- [ ] Cross-compilation builds succeed for all target platforms
- [ ] MSRV verification in CI

### Nice to Have (Wave 4)
- [ ] > 50% test coverage
- [ ] Unit tests for all protocol commands
- [ ] Input validation with helpful error messages
- [ ] Multiple client rejection with clear error

---

## Estimated Timeline

| Wave | Duration | Effort | Dependencies |
|------|----------|--------|--------------|
| Wave 1: Emergency Stabilization | 2 days | 8-12 hours | None |
| Wave 2: Structural Improvements | 3 days | 12-16 hours | Wave 1 complete |
| Wave 3: CI/CD Modernization | 2 days | 8-10 hours | Wave 1 complete |
| Wave 4: Testing & Hardening | 3 days | 12-16 hours | Wave 2 complete |
| **Total** | **10 days** | **40-54 hours** | Sequential waves |

---

## File Change Summary

| File | Wave 1 | Wave 2 | Wave 3 | Wave 4 |
|------|--------|--------|--------|--------|
| `src/main.rs` | **Major** (lines 64, 67, 70-78, 93, 101, 106, 110-118, 123, 128, 134, 137, 150, 152, 166, 169, 171-172 + new signal path) | **Major** (constants, error type, command 0x03 fix, flush) | — | **Major** (tests, validation, SIGTERM verification, multi-client) |
| `Cargo.toml` | — | **Minor** (profile.release, rust-version) | — | — |
| `.github/workflows/ci.yml` | — | — | **Major** (all actions updated) | **Minor** (MSRV job) |
| `.github/workflows/cd.yml` | — | — | **Major** (all actions updated, cross-fix) | — |
| `.github/workflows/audit.yml` | — | — | **Major** (all actions updated) | — |
| `tests/integration.rs` | — | — | — | **New file** |

## Issues Addressed Per Wave

| Wave | Issue(s) Addressed | Severity |
|------|-------------------|-----------|
| 1 | Compile error (line 64), unwrap panics (lines 93, 150, 166, 169, 171-172), channel deadlock (line 67), hard exit (line 75), normal-completion hang (new) | 4 Critical + 5 High |
| 2 | Magic numbers, custom error type, command 0x03 logic bug, BufWriter flush, release profile, MSRV, ctrlc SIGTERM check | 3 Medium + 3 Low |
| 3 | Abandoned CI actions, outdated actions, cross-compilation, MSRV CI job | 1 Critical + 3 High |
| 4 | Unit tests, integration tests, input validation, SIGTERM handling, multi-client handling | 2 High + 3 Medium |

---

## Notes

- This plan assumes the existing `rtlsdr_mt` crate API remains stable
- Hardware testing required for full validation (RTL2832 USB dongle)
- systemd socket activation must be tested after all changes
- The crash report shows this is running on **Ubuntu 24 LTS / ARM (aarch64)** — all fixes must be validated on this platform
- Current production uses systemd service name `rtltcp1234.service` — verify service files still work after changes
