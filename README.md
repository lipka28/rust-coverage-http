# rust-coverage-http

Collect Rust code coverage from running applications (including in Kubernetes) via HTTP, without filesystem requirements.

## Overview

This project provides a mechanism to collect LLVM-based code coverage from instrumented Rust binaries at runtime, over HTTP. See also [go-coverage-http](../go-coverage-http/) for the Go equivalent.

### Architecture

```
┌──────────────────────────────────────────────────────────────────┐
│                     Instrumented Application                      │
│  ┌─────────────────────┐      ┌──────────────────────────────┐   │
│  │  Your App (:8000)   │      │  Coverage Server (:9095)     │   │
│  │  /health, /greet... │      │  /coverage, /health          │   │
│  └─────────────────────┘      └──────────────────────────────┘   │
│           LLVM coverage counters (in-memory)                     │
└──────────────────────────────────┬───────────────────────────────┘
                                   │ kubectl port-forward (9095)
                                   ▼
┌──────────────────────────────────────────────────────────────────┐
│                    Coverage Client (local/CI)                     │
│  Collect profraw → llvm-profdata merge → llvm-cov report/html    │
└──────────────────────────────────────────────────────────────────┘
```

### How It Works

1. **Build-time**: Compile your Rust app with `RUSTFLAGS="-C instrument-coverage" LLVM_PROFILE_FILE=/dev/null cargo build ...` (LLVM inserts profiling counters)
2. **Runtime**: The `coverage-server` library starts an HTTP server that serializes coverage data directly from memory using `__llvm_profile_write_buffer()` — no disk I/O, works on fully read-only filesystems
3. **Test-time**: The `coverage-client` fetches profraw data via HTTP, then uses `llvm-profdata` and `llvm-cov` to generate reports

### Key Features

| Aspect | Details |
|--------|---------|
| Instrumentation | `RUSTFLAGS="-C instrument-coverage"` |
| Runtime API | `__llvm_profile_write_buffer()` via FFI (fully in-memory, no disk I/O) |
| Output format | LLVM profraw |
| Report tools | `llvm-profdata`, `llvm-cov` |
| Report formats | LCOV, HTML, text summary |
| Counter reset | `__llvm_profile_reset_counters()` (enables per-test coverage) |

## Components

### coverage-server (library)

Embedded HTTP server to add to your instrumented binary:

```rust
use coverage_server::CoverageServer;

#[tokio::main]
async fn main() {
    // Start coverage server on port 9095 (or COVERAGE_PORT env var)
    let _handle = coverage_server::start_coverage_server().await;
    
    // ... your application code ...
}
```

**Endpoints:**
- `GET/POST /coverage` — Returns base64-encoded profraw data as JSON
- `GET/POST /coverage/reset` — Resets coverage counters (for per-test coverage)
- `GET /health` — Health check

### coverage-client (library + CLI)

Collects coverage from pods and generates reports.

**As a library:**
```rust
use coverage_client::CoverageClient;

let mut client = CoverageClient::new("my-namespace", "./coverage-output");
client.set_binary_path("./target/release/my-app");
client.set_source_dir("./");

// Collect from a pod
let pod = client.get_pod_name("app=my-service")?;
client.collect_from_pod(&pod, "e2e-tests", 9095).await?;

// Generate reports
client.process_reports("e2e-tests")?;
```

**As a CLI:**
```bash
# Collect coverage from a pod
coverage-client -n my-namespace -b ./target/release/my-app \
    collect --selector app=my-service --test-name e2e

# Generate reports from collected data
coverage-client -b ./target/release/my-app report --test-name e2e

# Or collect and report in one step
coverage-client -n my-namespace -b ./target/release/my-app \
    collect-and-report --selector app=my-service
```

**Default report filters:**

Reports automatically exclude files matching these patterns:
- `coverage_server` / `coverage-server` — the coverage server library itself
- `.cargo/registry` — third-party crates from crates.io
- `.rustup/toolchains` — Rust standard library sources

You can override these with `--filter` (replaces all defaults):
```bash
# Custom filters (replaces defaults, so re-add any you still want)
coverage-client -b ./target/release/my-app \
    --filter 'coverage.server' --filter '.cargo/' --filter '.rustup/toolchains' \
    --filter 'my_test_utils' \
    report --test-name e2e
```

Or via the library:
```rust
client.add_filter("my_test_utils");  // adds to defaults
client.set_filters(vec![...]);       // replaces defaults
```

### example-app

Demo HTTP application showing integration with coverage-server.

## Integrating with Your Application

The `coverage-server` crate is the only piece you embed in your application. The `coverage-client` is a separate tool that runs externally (on your laptop, in CI) to collect and process the data — it is never a dependency of your app.

### Adding the dependency

Choose whichever method suits your setup:

**Git dependency** (simplest — no publishing step, just push this repo to GitHub):

```toml
[dependencies]
coverage-server = { git = "https://github.com/your-org/rust-coverage.git", subdirectory = "coverage-server" }
tokio = { version = "1", features = ["full"] }
```

You can pin to a branch or tag:

```toml
coverage-server = { git = "https://github.com/your-org/rust-coverage.git", tag = "v0.1.0", subdirectory = "coverage-server" }
```

**crates.io** (if/when the crate is published):

```toml
[dependencies]
coverage-server = "0.1"
tokio = { version = "1", features = ["full"] }
```

**Path or git submodule** (for monorepos or vendored setups):

```toml
[dependencies]
coverage-server = { path = "vendor/rust-coverage/coverage-server" }
tokio = { version = "1", features = ["full"] }
```

### Minimal integration

Add a single line to your `main()` — works with any async runtime (axum, actix-web, tonic, plain tokio):

```rust
#[tokio::main]
async fn main() {
    // Starts coverage HTTP server on port 9095 in the background.
    // Returns immediately — does not block your app.
    let _coverage_handle = coverage_server::start_coverage_server().await;

    // The rest of your application is completely unchanged.
    // Example: an axum web server
    let app = axum::Router::new()
        .route("/", axum::routing::get(|| async { "Hello" }));

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8000").await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
```

The coverage server runs as a background tokio task in the same process. It shares the LLVM coverage counters with your app but uses a separate port (9095), so there is zero interference with your application's routes or logic.

### Building with coverage instrumentation

The coverage server is only useful when the binary is compiled with LLVM instrumentation:

```bash
RUSTFLAGS="-C instrument-coverage" LLVM_PROFILE_FILE=/dev/null cargo build --release
```

Without this flag, the LLVM profile symbols won't be linked, and the build will fail. For production builds where you don't want coverage, simply omit the crate or use a Cargo feature to conditionally include it.

### Collecting coverage (external — not part of your app)

The `coverage-client` runs separately on the machine where you want the reports:

```bash
# Install or build the client
cargo install --git https://github.com/your-org/rust-coverage.git coverage-client

# Collect from a running pod
coverage-client -n my-namespace -b ./target/release/my-app \
    collect --selector app=my-service

# Generate reports
coverage-client -b ./target/release/my-app report
```

Or use it as a library in your test harness — see the coverage-client section above.

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

# Collect coverage
curl -X POST http://localhost:9095/coverage | jq .

# Or use the client CLI
cargo run -p coverage-client -- \
    -b ./target/debug/example-app \
    -o ./coverage-output \
    collect-url --url http://localhost:9095/coverage --test-name local-test

cargo run -p coverage-client -- \
    -b ./target/debug/example-app \
    -o ./coverage-output \
    report --test-name local-test
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

# Collect coverage via the client
cargo run -p coverage-client -- \
    -n coverage-demo \
    -b ./target/release/example-app \
    collect --selector app=rust-coverage-demo --test-name e2e
```

## Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `COVERAGE_PORT` | `9095` | Port for the coverage HTTP server |
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
    cargo run -p coverage-client -- \
      -n coverage-demo \
      -b ./target/release/example-app \
      collect-and-report --selector app=my-app --test-name e2e

- name: Upload coverage
  uses: codecov/codecov-action@v4
  with:
    files: ./coverage-output/e2e/lcov.info
    flags: e2e-tests
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
- Never expose port 9095 in production

### Limitations

- Requires the instrumented binary on the client machine for `llvm-cov` report generation
- The binary must match the exact build that's running in the pod
- LLVM tools version should match the Rust compiler's LLVM version
- Rust coverage is binary-level (not source-package-level), so the full binary is needed for reports
