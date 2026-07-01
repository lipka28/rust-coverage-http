# rust-coverage-http

Collect Rust code coverage from running applications (including in Kubernetes) via HTTP, without filesystem requirements.

## Overview

This project provides a mechanism to collect LLVM-based code coverage from instrumented Rust binaries at runtime, over HTTP. See also [go-coverage-http](../go-coverage-http/) for the Go equivalent.

### Architecture

```
┌──────────────────────────────────────────────────────────────────┐
│                     Instrumented Application                      │
│  ┌─────────────────────┐      ┌──────────────────────────────┐   │
│  │  Your App (:8000)   │      │  Coverage Server (:53700)    │   │
│  │  /health, /greet... │      │  /coverage, /health          │   │
│  └─────────────────────┘      └──────────────────────────────┘   │
│           LLVM coverage counters (in-memory)                     │
└──────────────────────────────────┬───────────────────────────────┘
                                   │ kubectl port-forward (53700)
                                   ▼
┌──────────────────────────────────────────────────────────────────┐
│                       coverport CLI                               │
│  Collect profraw → llvm-profdata merge → llvm-cov report/html    │
└──────────────────────────────────────────────────────────────────┘
```

### How It Works

1. **Build-time**: Compile your Rust app with `RUSTFLAGS="-C instrument-coverage" LLVM_PROFILE_FILE=/dev/null cargo build ...` (LLVM inserts profiling counters)
2. **Runtime**: The `coverage-server` library starts an HTTP server that serializes coverage data directly from memory using `__llvm_profile_write_buffer()` — no disk I/O, works on fully read-only filesystems
3. **Test-time**: [coverport](https://github.com/konflux-ci/coverport) fetches profraw data via HTTP, then uses `llvm-profdata` and `llvm-cov` to generate reports

### Key Features

| Aspect | Details |
|--------|---------|
| Instrumentation | `RUSTFLAGS="-C instrument-coverage"` |
| Runtime API | `__llvm_profile_write_buffer()` via FFI (fully in-memory, no disk I/O) |
| Output format | LLVM profraw |
| Report tools | `llvm-profdata`, `llvm-cov` |
| Report formats | LCOV, HTML, text summary |
| Counter reset | `__llvm_profile_reset_counters()` (enables per-test coverage) |
| Runtime | Framework-agnostic — works with any async runtime or synchronous apps |

## Components

### coverage-server (library)

Embedded HTTP server to add to your instrumented binary. Works with any application — no specific async runtime required:

```rust
fn main() {
    #[cfg(feature = "coverage")]
    let _handle = coverage_server::start_coverage_server_standalone(53700);

    // ... rest of your application, any framework, any runtime ...
}
```

**Endpoints:**
- `GET/POST /coverage` — Returns base64-encoded profraw data as JSON
- `GET/POST /coverage/reset` — Resets coverage counters (for per-test coverage)
- `GET /health` — Health check

### Coverage collection with coverport

[coverport](https://github.com/konflux-ci/coverport) is a multi-language coverage collection CLI that handles the full pipeline: discover pods, collect profraw over HTTP, merge, generate reports, and upload to Codecov.

```bash
# Collect from a running app (local or via URL)
coverport collect --url http://localhost:53700/coverage --test-name e2e -o ./coverage-output

# Collect from Kubernetes pods
coverport collect -n my-namespace -l app=my-service --test-name e2e -o ./coverage-output

# Process into reports (LCOV, text summary, optional HTML)
COVERAGE_BINARY=./target/release/my-app coverport process \
    --coverage-dir=./coverage-output --format=rust --generate-html \
    --skip-clone --upload=false
```

coverport auto-detects Rust by the `profraw_data` field in the JSON response.

### example-app

Demo HTTP application showing integration with coverage-server.

## Integrating with Your Application

The `coverage-server` crate is the only piece you embed in your application. Coverage collection and report generation is handled externally by [coverport](https://github.com/konflux-ci/coverport).

### Adding the dependency

Choose whichever method suits your setup:

Add `coverage-server` as an optional dependency behind a feature flag:

```toml
[features]
coverage = ["dep:coverage-server"]

[dependencies]
coverage-server = { git = "https://github.com/lipka28/rust-coverage-http.git", subdirectory = "coverage-server", optional = true }
```

You can pin to a branch or tag:

```toml
coverage-server = { git = "https://github.com/lipka28/rust-coverage-http.git", tag = "v0.1.0", subdirectory = "coverage-server", optional = true }
```

Other source options:

```toml
# crates.io (if/when the crate is published)
coverage-server = { version = "0.1", optional = true }

# Path or git submodule (for monorepos or vendored setups)
coverage-server = { path = "vendor/rust-coverage/coverage-server", optional = true }
```

No additional dependencies (like tokio) are needed — the standalone server brings its own runtime.

### Minimal integration

Add two lines to your `main()` — works with any application, any framework, any async runtime (or none):

```rust
fn main() {
    #[cfg(feature = "coverage")]
    let _coverage = coverage_server::start_coverage_server_standalone(53700);

    // The rest of your application is completely unchanged.
    // Works with axum, actix-web, tonic, synchronous apps, CLI tools, etc.
}
```

The coverage server spawns on its own background thread with its own tokio runtime. It shares the LLVM coverage counters with your app but uses a separate port (53700), so there is zero interference with your application's logic.


### Building with coverage instrumentation

The coverage server requires both the `coverage` feature flag and LLVM instrumentation:

```bash
# Production build (no coverage, coverage-server not even compiled)
cargo build --release

# Coverage-instrumented build (for test environments)
RUSTFLAGS="-C instrument-coverage" LLVM_PROFILE_FILE=/dev/null cargo build --release --features coverage
```

Without `-C instrument-coverage`, the LLVM profile symbols won't be linked. Without `--features coverage`, the coverage-server dependency is not included at all.

### Collecting coverage (external — not part of your app)

Use [coverport](https://github.com/konflux-ci/coverport) to collect and process coverage:

```bash
# Collect from a running app
coverport collect --url http://localhost:53700/coverage --test-name e2e -o ./coverage-output

# Or collect from Kubernetes pods
coverport collect -n my-namespace -l app=my-service --test-name e2e -o ./coverage-output

# Process into reports (LCOV, text summary, HTML)
COVERAGE_BINARY=./target/release/my-app coverport process \
    --coverage-dir=./coverage-output --format=rust --generate-html \
    --skip-clone --upload=false
```

## Quick Start

### Prerequisites

- Rust 1.70+
- `llvm-tools-preview` component: `rustup component add llvm-tools-preview`
- (Optional) `cargo-binutils`: `cargo install cargo-binutils`
- (For K8s) kubectl, kind

### Local Development

```bash
# Build with coverage instrumentation
RUSTFLAGS="-C instrument-coverage" LLVM_PROFILE_FILE=/dev/null cargo build -p example-app

# Run the app (LLVM_PROFILE_FILE=/dev/null suppresses stray .profraw files on exit)
LLVM_PROFILE_FILE=/dev/null ./target/debug/example-app

# In another terminal, exercise the app
curl http://localhost:8000/health
curl http://localhost:8000/greet?name=World
curl "http://localhost:8000/calculate?a=5&b=3"

# Collect coverage with coverport
coverport collect --url http://localhost:53700/coverage --test-name local-test -o ./coverage-output

# Process into reports
COVERAGE_BINARY=./target/debug/example-app coverport process \
    --coverage-dir=./coverage-output --format=rust --generate-html \
    --skip-clone --upload=false

# View the HTML report
xdg-open ./coverage-output/local-test/html/index.html
```

### Kubernetes (Kind)

```bash
# Create a kind cluster
kind create cluster --config kind-config.yaml --name coverage-demo

# Build the image with coverage
docker build --build-arg ENABLE_COVERAGE=true -t rust-coverage-demo:latest -f Dockerfile.local .

# Load into kind
kind load docker-image rust-coverage-demo:latest --name coverage-demo

# Deploy
kubectl apply -f k8s-deployment.yaml

# Wait for pod to be ready
kubectl wait --for=condition=ready pod -l app=rust-coverage-demo -n coverage-demo --timeout=60s

# Exercise the app
curl http://localhost:8000/health
curl http://localhost:8000/greet?name=Rust

# Collect coverage
coverport collect -n coverage-demo -l app=rust-coverage-demo --test-name e2e -o ./coverage-output

# Process into reports
COVERAGE_BINARY=./target/release/example-app coverport process \
    --coverage-dir=./coverage-output --format=rust --generate-html \
    --skip-clone --upload=false
```

## Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `COVERAGE_PORT` | `53700` | Port for the coverage HTTP server |
| `COVERAGE_BINARY` | — | Path to instrumented binary (used by coverport for report generation) |
| `LLVM_PROFILE_FILE` | — | Set to `/dev/null` to suppress stray `.profraw` files during build/run |
| `APP_PORT` | `8000` | Application HTTP port (example-app only) |
| `RUST_LOG` | — | Log level filter (e.g. `info`, `debug`) |

## Output Structure

```
coverage-output/
└── e2e-tests/
    ├── rust-coverage-server.profraw   # Raw LLVM profile data
    ├── metadata.json                  # Collection metadata
    ├── coverage.profdata              # Merged profile data
    ├── coverage.txt                   # Text summary report
    ├── lcov.info                      # LCOV format (for Codecov/SonarCloud)
    └── html/                          # HTML report directory
        └── index.html
```

## CI/CD Integration

### Codecov

```bash
# After generating lcov.info
codecov upload-process --file coverage-output/e2e-tests/lcov.info --flag e2e-tests
```

### GitHub Actions Example

```yaml
- name: Build with coverage
  run: |
    rustup component add llvm-tools-preview
    RUSTFLAGS="-C instrument-coverage" LLVM_PROFILE_FILE=/dev/null cargo build --release -p example-app

- name: Deploy and test
  run: |
    # ... deploy to kind, run E2E tests ...
    
- name: Collect coverage
  run: |
    coverport collect -n coverage-demo -l app=my-app --test-name e2e -o ./coverage-output

- name: Process and upload coverage
  run: |
    COVERAGE_BINARY=./target/release/example-app coverport process \
      --coverage-dir=./coverage-output --format=rust \
      --codecov-token=${{ secrets.CODECOV_TOKEN }} \
      --codecov-flags=e2e-tests
```

## Technical Details

### LLVM Profile Functions

The coverage server uses three LLVM runtime functions via FFI to collect coverage data **entirely in memory** (no disk I/O):

- `__llvm_profile_get_size_for_buffer()` — Returns the byte size needed to hold the serialized profile data
- `__llvm_profile_write_buffer(buf)` — Writes the profraw data into a caller-provided in-memory buffer
- `__llvm_profile_reset_counters()` — Resets all counters to zero (enables per-test coverage)

These are only available when the binary is compiled with `-C instrument-coverage`.

### Security Considerations

- The coverage endpoint has **no authentication** — only use in test environments
- Never expose port 53700 in production

### Limitations

- Requires the instrumented binary on the client machine for `llvm-cov` report generation
- The binary must match the exact build that's running in the pod
- LLVM tools version should match the Rust compiler's LLVM version
- Rust coverage is binary-level (not source-package-level), so the full binary is needed for reports
