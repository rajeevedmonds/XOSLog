# Contributing to xoslog

Thank you for your interest in contributing to xoslog. This document explains
how to build, test, and submit changes to the project.

## Project overview

`xoslog` is a thread-safe, robust logging library for Linux written in pure
Rust. Two constraints shape every change:

- **Zero dependencies**: no runtime or build-time dependencies are added
  unless a contributor can make a very strong case for one.
- **No `unsafe`**: the crate enforces `#![forbid(unsafe_code)]` at compile
  time. All new code must compile and pass tests without `unsafe`.

## Getting started

### Prerequisites

A recent stable Rust toolchain (edition 2021). On Debian/Ubuntu:

```bash
# Install curl and a C linker/compiler toolchain
sudo apt install -y curl build-essential

# Install Rust via rustup (unattended)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal

# Load cargo/rustc into the current shell
source "$HOME/.cargo/env"
```

Any recent stable Rust works; the crate targets edition 2021.

### Build and test

```bash
# Debug build
cargo build

# Run the full test suite, including integration and doc tests
cargo test --release

# Build the API documentation
cargo doc --no-deps
```

Before submitting a change, run the full suite with `cargo test --release`
and make sure it is green.

## How to make changes

1. **Open an issue first** for non-trivial changes so the maintainer and other
   contributors can discuss the design before implementation.
2. **Fork the repository** and create a feature branch with a descriptive
   name, e.g. `add-json-sink`.
3. **Keep changes focused**: a single logical change per pull request makes
   review faster and history easier to follow.
4. **Add tests**: every new behavior should be covered by tests. Integration
   tests live in `tests/` and mirror the public API, e.g. `json.rs`,
   `syslog.rs`, `rotation.rs`, `filter.rs`, `threading.rs`.
5. **Update the README** when you change public behavior. The README documents
   features, usage examples, and formatting; keep it in sync with the code.
6. **Document new public items** with `///` rustdoc comments. Public API
   surfaces are part of the crate's contract.

## Coding conventions

- Follow `rustfmt` defaults for formatting.
- Keep the crate dependency-free and free of `unsafe` code.
- Name public items clearly and document them; avoid abbreviations.
- Handle errors explicitly. `xoslog` favors `Result`-returning builders and
  deterministic error paths over panics or silently swallowed failures.
- Do not leave dead code, debug `println!`, or commented-out blocks behind.

## Running a single test file

```bash
cargo test --test syslog
```

## Submitting a pull request

- Target the `main` branch.
- In the PR description, summarize the change and link any related issue.
- Ensure CI passes; the repository builds releases from version tags.
- Once your change is merged, releases are cut by tagging a version like
  `v1.0.0` — the `Generate GitHub Release` workflow creates the release
  automatically from that tag.

## Reporting issues

Please include:

- The `xoslog` version you are using.
- Your Linux distribution and Rust toolchain version (`rustc --version`).
- A minimal reproduction snippet or test case.
- Expected versus actual behavior.

## License

By contributing, you agree that your contributions are licensed under the MIT
License — see [LICENSE](LICENSE).
