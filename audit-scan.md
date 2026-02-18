# Codebase Audit Report — `fujin`

**Date:** 2026-02-19
**Auditor:** Automated first-pass audit (senior-engineer review)
**Scope:** Project structure, dependency health, build & tooling
**Language:** Rust (stable, edition 2021)

---

## Executive Summary

`fujin` is an early-stage (v0.1.0) Rust CLI tool that orchestrates multi-stage AI agent pipelines using the Claude Code subprocess. The codebase is well-organised as a four-crate Cargo workspace, uses modern Rust idioms, and carries **zero unsafe code**. The primary risk areas are the **complete absence of CI/CD infrastructure**, **no linting or formatting enforcement**, **no supply-chain security tooling**, and several **dependency hygiene issues** — most notably the controversial `serde_yml` crate and multiple out-of-range-but-available upgrades for terminal UI crates.

---

## 1. Project Structure

### 1.1 Repository Layout

```
fujin/
├── Cargo.toml              # Workspace manifest (resolver = "2")
├── Cargo.lock              # Lock file (format version 4)
├── rust-toolchain.toml     # Toolchain pin (channel = "stable", unpinned)
├── README.md
├── .gitignore
├── docs/
│   └── pipeline-authoring.md
└── crates/
    ├── fujin-cli/          # Binary crate — CLI entry point
    │   ├── Cargo.toml
    │   └── src/main.rs     (~754 lines)
    ├── fujin-core/         # Library crate — pipeline engine
    │   ├── Cargo.toml
    │   ├── src/            (13 modules)
    │   └── tests/
    │       └── pipeline_tests.rs  (636 lines, 8 integration tests)
    ├── fujin-config/       # Library crate — YAML config parsing
    │   ├── Cargo.toml
    │   └── src/            (3 modules)
    └── fujin-tui/          # Library crate — ratatui terminal UI
        ├── Cargo.toml
        └── src/            (8 modules in screens/ and widgets/)
```

### 1.2 Module Inventory

| Crate | Purpose | Source Files |
|---|---|---|
| `fujin-cli` | Binary; command routing, event display | 1 |
| `fujin-core` | Pipeline runner, agent subprocess, workspace diffing, checkpoints, context | 13 |
| `fujin-config` | YAML/JSON config structs and validation | 3 |
| `fujin-tui` | Interactive TUI (browser + live execution screens) | 8 |
| **Total** | | **25 + 1 test** |

### 1.3 File-Type Counts

| Type | Count | Notes |
|---|---|---|
| `.rs` | 26 | 25 production + 1 integration test file |
| `.toml` | 6 | 1 workspace + 4 crate manifests + 1 toolchain |
| `.yaml` | 2 | Built-in pipeline templates |
| `.md` | 2 | README + authoring guide |
| `.lock` | 1 | Cargo.lock |
| **Total tracked** | **~37** | Excluding `target/` and `.git/` |

### 1.4 Architecture

The codebase follows a clean layered design:

```
fujin-cli  ──depends──▶  fujin-core  ──depends──▶  fujin-config
    │                                                     ▲
    └──────────────▶  fujin-tui  ─────────────────────────┘
```

No circular dependencies. Event-driven architecture (`PipelineEvent` channel) decouples execution from display.

### 1.5 Findings

| # | Finding | Severity |
|---|---|---|
| S-01 | No `examples/` directory — no runnable usage examples shipped with the crate | **Low** |
| S-02 | `fujin-cli/src/main.rs` is 754 lines; a refactor into sub-modules would improve maintainability | **Low** |
| S-03 | No `.cargo/config.toml` — no per-workspace compiler flags, target configuration, or credential helpers configured | **Low** |

---

## 2. Dependencies

### 2.1 Direct Dependency Summary

**Workspace-level dependencies declared in root `Cargo.toml`:**

| Crate | Declared Range | Locked Version | Status |
|---|---|---|---|
| tokio | `"1"` | 1.49.0 | ✅ Current within range |
| async-trait | `"0.1"` | 0.1.89 | ✅ Current within range |
| serde | `"1"` | 1.0.228 | ✅ Current within range |
| serde_json | `"1"` | 1.0.149 | ✅ Current within range |
| **serde_yml** | `"0.0.12"` | 0.0.12 | ⚠️ See D-01 below |
| clap | `"4"` | 4.5.59 | ✅ Current within range |
| thiserror | `"2"` | 2.0.18 | ✅ Current within range |
| anyhow | `"1"` | 1.0.101 | ✅ Current within range |
| tracing | `"0.1"` | 0.1.44 | ✅ Current within range |
| tracing-subscriber | `"0.3"` | 0.3.22 | ✅ Current within range |
| dirs | `"6"` | 6.0.0 | ✅ Current within range |
| chrono | `"0.4"` | 0.4.43 | ✅ Current within range |
| sha2 | `"0.10"` | 0.10.9 | ✅ Current within range |
| uuid | `"1"` | 1.21.0 | ✅ Current within range |
| handlebars | `"6"` | 6.4.0 | ✅ Current within range |
| walkdir | `"2"` | 2.5.0 | ✅ Current within range |
| **console** | `"0.15"` | 0.15.11 | ⚠️ 0.16.2 available outside range |
| **indicatif** | `"0.17"` | 0.17.11 | ⚠️ 0.18.4 available outside range |
| **ratatui** | `"0.29"` | 0.29.0 | ⚠️ 0.30.0 available outside range |
| **crossterm** | `"0.28"` | 0.28.1 | ⚠️ 0.29.0 available outside range |
| dotenvy | `"0.15"` | 0.15.7 | ✅ Current within range |

### 2.2 Dependency Findings

---

#### D-01 — `serde_yml` is a community-controversial pre-release crate

**Severity: High**

`serde_yml = "0.0.12"` is pinned to the only version of a pre-release-versioned (`0.0.x`) crate by author `sebastienrousseau`. When the widely-trusted `serde_yaml` (by David Tolnay) was deprecated in early 2024, `serde_yml` appeared under a near-identical name. Community discussion raised the following concerns:

- The crate and its sub-dependency `libyml` (also `0.0.x`) are **not audited to the same standard** as serde_yaml was.
- The name similarity to `serde_yaml` creates potential **confusion and dependency-confusion risk**.
- Using a crate at `0.0.x` signals **no API stability guarantee**.
- The project's config format (YAML) was questioned by Tolnay himself when he deprecated `serde_yaml`, citing fundamental ambiguity issues (e.g., the Norway problem).

**Recommendation:** Evaluate switching the pipeline config format to **TOML** (used by Cargo itself; well-audited `toml` crate by Tolnay). This would also eliminate user-facing YAML ambiguity. If YAML must be retained, prefer the deprecated-but-well-reviewed `serde_yaml` 0.9.x which is still functional, or wait for a stable community-endorsed replacement.

---

#### D-02 — Terminal UI crates have new major/minor versions available outside the declared range

**Severity: Medium**

Four crates used in `fujin-tui` have newer versions published beyond their semver-pinned range:

| Crate | Current | Available | Notes |
|---|---|---|---|
| `ratatui` | 0.29.0 | 0.30.0 | ratatui has rapid release cadence; 0.30 adds new layout APIs |
| `crossterm` | 0.28.1 | 0.29.0 | ratatui 0.30 requires crossterm 0.29 — these must be upgraded together |
| `console` | 0.15.11 | 0.16.2 | Breaking API changes in 0.16 |
| `indicatif` | 0.17.11 | 0.18.4 | Breaking API changes in 0.18 |

**Recommendation:** Plan a coordinated TUI dependency upgrade. Start with `ratatui` + `crossterm` (upgrade together, consult the ratatui CHANGELOG for migration steps), then independently evaluate `console` and `indicatif`.

---

#### D-03 — `rust-toolchain.toml` does not pin a specific Rust version

**Severity: Medium**

```toml
# rust-toolchain.toml
[toolchain]
channel = "stable"
```

Using `channel = "stable"` without a specific version (e.g., `channel = "1.85.0"`) means every developer and every future CI runner will silently pick up whatever is current stable at the time. This can cause **reproducibility drift** — a breaking change in a future stable Rust release could silently break a build, with no easy way to bisect which toolchain version introduced the regression.

**Recommendation:** Pin the toolchain to a specific stable version, e.g.:
```toml
[toolchain]
channel = "1.85.0"
components = ["rustfmt", "clippy"]
```
Update periodically and intentionally.

---

#### D-04 — `tokio` is pulled in with `features = ["full"]`

**Severity: Low**

`tokio = { version = "1", features = ["full"] }` enables every tokio feature (net, fs, process, signal, sync, time, rt-multi-thread, macros, io-util, etc.). This increases binary size and compile time unnecessarily. While it is a common approach during early development, production crates should declare only the features they actually use.

**Recommendation:** Audit which tokio features each crate actually uses and restrict feature flags accordingly. This is a compile-time and binary-size concern, not a correctness or security issue.

---

#### D-05 — No `cargo audit` / RustSec advisory check is performed

**Severity: High**

There is no `cargo audit` step, no `deny.toml` for `cargo-deny`, and no advisory database check anywhere in the project. Security vulnerabilities discovered in transitive dependencies will go **undetected**.

**Recommendation:** Install and run `cargo audit` locally and in CI:
```bash
cargo install cargo-audit
cargo audit
```
Consider adding `cargo-deny` with a `deny.toml` for richer control:
```bash
cargo install cargo-deny
cargo deny check
```

---

#### D-06 — `bumpalo` transitive dependency has a free semver-compatible patch update

**Severity: Low**

`bumpalo` 3.19.1 → 3.20.1 is a free `cargo update` within the declared range. This is a transitive dependency (likely via `wasm-bindgen` or JS interop in one of the terminal crates). Not a security issue, but keeping transitive deps fresh reduces accumulation of technical debt.

**Recommendation:** Run `cargo update` periodically and commit the updated `Cargo.lock`.

---

#### D-07 — Potential supply-chain anomaly: `serde_core` and `zmij` in transitive closure

**Severity: Medium**

The `cargo tree` output surfaced references to crates named `serde_core` and `zmij` in the transitive dependency graph. These are unusual names that may represent legitimate internal reorganisation crates published by the serde ecosystem, or may be worth scrutiny.

**Recommendation:** Run the following to confirm their provenance:
```bash
cargo tree | grep -E "serde_core|zmij"
cargo audit
```
Verify these crates are published by expected trusted authors on crates.io. If provenance is unclear, escalate as a potential supply-chain substitution concern.

---

### 2.3 Dev Dependencies

| Crate | Version | Used In | Notes |
|---|---|---|---|
| `tempfile` | `"3"` (→ 3.25.0) | `fujin-core` tests | Current within range ✅ |
| `tokio` (test-util, macros) | workspace | `fujin-core` tests | ✅ |
| `async-trait` | workspace | `fujin-core` tests | ✅ |

Dev dependencies are clean. Appropriate use of `tempfile` for filesystem tests.

---

## 3. Build & Tooling

### 3.1 Build Configuration

The workspace uses a standard Cargo workspace with `resolver = "2"` (the modern feature resolver). All crate manifests correctly inherit `version`, `edition`, and `license` from `[workspace.package]`. All dependency version constraints are centrally managed via `[workspace.dependencies]`.

**Build Findings:**

| # | Finding | Severity |
|---|---|---|
| B-01 | No CI/CD pipeline exists — zero automation for test, lint, or release | **Critical** |
| B-02 | No `rustfmt.toml` — code formatting is not enforced or configured | **High** |
| B-03 | No `clippy.toml` or Clippy lint configuration — static analysis is ad hoc | **High** |
| B-04 | No `deny.toml` — no supply-chain policy (allowed licenses, banned crates, advisory blocks) | **High** |
| B-05 | No `.cargo/config.toml` for workspace-level Rustflags | **Low** |
| B-06 | No `Makefile` or `Taskfile` — no developer task runner for common commands | **Low** |
| B-07 | No `Dockerfile` or container build — distribution as a container is unsupported | **Low** |
| B-08 | `rust-toolchain.toml` uses floating `"stable"` channel — see D-03 | **Medium** |

---

### 3.2 CI/CD — Critical Gap

**Severity: Critical**

There are **no CI/CD configuration files** of any kind in the repository:

- No `.github/workflows/` directory
- No `.circleci/config.yml`
- No `.travis.yml`
- No `Jenkinsfile`

This means:
- Pull requests are never automatically tested
- Formatting and linting are never enforced
- The binary is never automatically built for release
- Security advisories against dependencies are never checked

**Recommendation:** Create a minimum GitHub Actions workflow at `.github/workflows/ci.yml`:

```yaml
name: CI
on: [push, pull_request]
jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: rustfmt, clippy
      - run: cargo fmt --all --check
      - run: cargo clippy --all-targets --all-features -- -D warnings
      - run: cargo test --all
      - run: cargo audit
```

---

### 3.3 Linting & Formatting — Absent

**Severity: High**

Neither `rustfmt` nor `clippy` are configured at the workspace level:

- No `rustfmt.toml` or `.rustfmt.toml` found
- No `clippy.toml` or `.clippy.toml` found
- No `#![deny(clippy::all)]` or `#![warn(...)]` crate-level lint gates in any `lib.rs` or `main.rs`

**Only one lint suppression was found:**
```rust
// crates/fujin-core/src/agent.rs, line 139
#[allow(dead_code)]
struct StreamLine { ... }
```
This is appropriate (deserialization struct with optional fields), but it was applied without a comment explaining the rationale.

**Recommendation:**
1. Create `rustfmt.toml` at the workspace root with agreed formatting preferences.
2. Add `#![warn(clippy::all, clippy::pedantic)]` to each `lib.rs` and `main.rs`, suppressing individual lints as needed with `#[allow(...)]` and an explanatory comment.
3. Consider crate-level `#![deny(missing_docs)]` for the public library crates (`fujin-core`, `fujin-config`).

---

### 3.4 Code Quality Observations

**Positive indicators (no findings required):**
- ✅ Zero `unsafe` blocks across the entire codebase
- ✅ Zero `todo!()` or `unimplemented!()` macros in production code
- ✅ Zero `panic!()` macros in production code
- ✅ All subprocess prompts are passed via **stdin** (not as shell arguments) — eliminates shell injection risk
- ✅ Config hashed with SHA-256 to detect checkpoint/config drift
- ✅ Proper async cancellation via `AtomicBool` flag

**Minor code quality findings:**

| # | Finding | Severity |
|---|---|---|
| Q-01 | `main.rs:265` — `ProgressStyle::with_template(...).unwrap()` on a hardcoded string. Low risk but technically can panic; prefer `expect("valid static template")` with a descriptive message | **Low** |
| Q-02 | `agent.rs:435` — `.unwrap()` on a `select!` branch result; semantically safe but undocumented. A comment explaining why this cannot be `None` would improve maintainability | **Low** |
| Q-03 | `agent.rs:139` — `#[allow(dead_code)]` has no explanatory comment. Team conventions should require a comment for every lint suppression | **Low** |
| Q-04 | No `examples/` directory. Complex tool with no runnable examples beyond README snippets | **Low** |
| Q-05 | No documentation comments (`///`) on any public `struct` or `fn` in `fujin-core` or `fujin-config`. Since these are library crates, public API documentation is expected | **Medium** |

---

### 3.5 Test Coverage

The test suite is well-structured for the codebase size:

| Crate | Unit Tests | Integration Tests |
|---|---|---|
| `fujin-config` | 3 (validation logic) | — |
| `fujin-core` | 25 (agent, checkpoint, context, util, workspace) | 8 (full pipeline runs with MockRuntime) |
| `fujin-tui` | 27 (screen state machines) | — |
| `fujin-cli` | 0 | — |
| **Total** | **55** | **8** |

**Test findings:**

| # | Finding | Severity |
|---|---|---|
| T-01 | `fujin-cli` has **zero tests**. The CLI entry point (754 lines) has no unit or integration test coverage | **Medium** |
| T-02 | No code coverage measurement tool configured (`cargo-tarpaulin`, `cargo-llvm-cov`, etc.) | **Medium** |
| T-03 | Tests rely on a `MockRuntime` that does not verify the exact arguments passed to the `claude` subprocess. A subprocess argument regression would not be caught | **Low** |

---

## 4. Consolidated Findings by Severity

### Critical

| ID | Area | Finding |
|---|---|---|
| B-01 | Build/CI | No CI/CD pipeline of any kind — tests, linting, and security checks are never automated |

### High

| ID | Area | Finding |
|---|---|---|
| D-01 | Dependencies | `serde_yml 0.0.12` is a pre-release, community-controversial crate with audit concerns |
| D-05 | Dependencies | No `cargo audit` or `cargo-deny` — advisory vulnerabilities in dependencies go undetected |
| B-02 | Tooling | No `rustfmt.toml` — formatting is not enforced |
| B-03 | Tooling | No `clippy.toml` and no crate-level Clippy configuration — static analysis is absent |
| B-04 | Tooling | No `deny.toml` — no supply-chain license or advisory policy |

### Medium

| ID | Area | Finding |
|---|---|---|
| D-02 | Dependencies | `ratatui`, `crossterm`, `console`, `indicatif` all have newer versions outside the declared semver range |
| D-03 | Build | `rust-toolchain.toml` uses floating `"stable"` — toolchain version is not reproducible |
| D-07 | Dependencies | Unusual transitive crates (`serde_core`, `zmij`) warrant provenance verification |
| Q-05 | Code Quality | No `///` doc comments on public API items in library crates |
| T-01 | Testing | `fujin-cli` (754 lines) has zero test coverage |
| T-02 | Testing | No code coverage tooling configured |

### Low

| ID | Area | Finding |
|---|---|---|
| S-01 | Structure | No `examples/` directory |
| S-02 | Structure | `main.rs` at 754 lines is a monolith — refactor into sub-modules |
| S-03 | Structure | No `.cargo/config.toml` |
| D-04 | Dependencies | `tokio` uses `features = ["full"]` — wasteful for production builds |
| D-06 | Dependencies | `bumpalo` has a free patch update available via `cargo update` |
| B-05 | Build | No `.cargo/config.toml` for workspace-level Rustflags |
| B-06 | Build | No `Makefile`/`Taskfile` for developer task automation |
| B-07 | Build | No `Dockerfile` or container build support |
| Q-01 | Code Quality | `main.rs:265` — `.unwrap()` on a static string; prefer `.expect("msg")` |
| Q-02 | Code Quality | `agent.rs:435` — `.unwrap()` without explanatory comment |
| Q-03 | Code Quality | `#[allow(dead_code)]` in `agent.rs` lacks an explanatory comment |
| Q-04 | Code Quality | No `examples/` directory for CLI usage |
| T-03 | Testing | `MockRuntime` does not verify subprocess argument correctness |

---

## 5. Prioritised Remediation Roadmap

### Immediate (before any public release or team onboarding)

1. **Add GitHub Actions CI** (B-01) — a basic `fmt + clippy + test + audit` workflow gate on every PR
2. **Run `cargo audit`** (D-05) — establish a clean baseline; fix any advisories found
3. **Resolve `serde_yml` strategy** (D-01) — decide: TOML migration, retain with documented rationale, or switch to `serde_yaml` 0.9.x
4. **Verify `serde_core`/`zmij` provenance** (D-07) — run `cargo tree | grep -E "serde_core|zmij"` to confirm trusted authorship

### Short-term (within next sprint)

5. **Add `rustfmt.toml`** (B-02) and run `cargo fmt --all` to normalize existing code
6. **Add Clippy configuration** (B-03) with `#![warn(clippy::all)]` per crate
7. **Add `deny.toml`** (B-04) to enforce allowed licenses and block advisory crates
8. **Pin `rust-toolchain.toml`** (D-03) to a specific stable version with `components = ["rustfmt", "clippy"]`

### Medium-term (backlog)

9. **Add tests for `fujin-cli`** (T-01)
10. **Configure code coverage** (T-02) with `cargo-llvm-cov` or `cargo-tarpaulin`
11. **Add `///` documentation** (Q-05) to all public API items in `fujin-core` and `fujin-config`
12. **Upgrade TUI crate cluster** (D-02): `ratatui` 0.30 + `crossterm` 0.29 together, then `console` and `indicatif`
13. **Refactor `main.rs`** (S-02) into sub-modules
14. **Restrict `tokio` features** (D-04) per-crate to reduce compile time and binary size

---

*End of audit report.*
