# Technical Documentation

## How Rust Coverage Instrumentation Works

### LLVM Source-Based Code Coverage

Rust uses LLVM's source-based code coverage, which works at the compiler level:

1. **Compilation**: When `-C instrument-coverage` is passed to `rustc`, the compiler inserts coverage mapping data and counter instrumentation into the generated code
2. **Runtime**: As the program executes, counters are incremented for each executed region
3. **Profile dump**: Normally, counters are written to a `.profraw` file at program exit via an `atexit` handler

### Runtime Profile Functions

LLVM's instrumentation runtime provides C functions that we call via FFI:

```c
// Set the output filename for the profraw data
void __llvm_profile_set_filename(const char *filename);

// Trigger an immediate write of profile data (normally happens at exit)
int __llvm_profile_write_file(void);  // returns 0 on success

// Reset all counters to zero
void __llvm_profile_reset_counters(void);
```

These functions are linked into the binary automatically when `-C instrument-coverage` is used.

### Profile Data Flow

```
Source Code
    │
    ▼ (rustc -C instrument-coverage)
Instrumented Binary
    │
    ├── Coverage Mapping (embedded in binary)
    │   └── Maps counter IDs → source regions
    │
    └── Counter Array (runtime, in-memory)
        └── Incremented as code executes
            │
            ▼ (__llvm_profile_get_size_for_buffer + __llvm_profile_write_buffer)
        In-memory profraw buffer (Vec<u8>)
            │
            ▼ (base64 encode, HTTP response)
        Client receives profraw bytes
            │
            ▼ (llvm-profdata merge)
        .profdata file (on client)
            │
            ▼ (llvm-cov report/show/export)
        Coverage Reports (text, HTML, LCOV)
```

## Coverage Server Architecture

### Initialization

The coverage server can start in two ways:

**Standalone mode** (recommended — works with any app):
```
main()
  ├── start_coverage_server_standalone(53700)  →  std::thread::spawn(own tokio runtime → HTTP server on :53700)
  └── app logic                           →  any framework, any runtime
```

Both the application and the coverage server share the same process memory, including LLVM coverage counters.

### Request Handling (`/coverage`) — Fully In-Memory

1. Call `__llvm_profile_get_size_for_buffer()` to learn the required buffer size
2. Allocate a `Vec<u8>` of that size
3. Call `__llvm_profile_write_buffer(buf)` to serialize the profraw data directly into memory
4. Base64-encode the buffer
5. Return JSON response with the encoded data

**No filesystem access is performed.** This is the critical difference from the naive
`__llvm_profile_write_file()` approach — the data never touches disk, making the server
compatible with read-only root filesystems and requiring zero writable volumes.

### Counter Reset (`/coverage/reset`)

Calls `__llvm_profile_reset_counters()` to zero all counters. This enables per-test-case coverage collection:

```
Test A starts → exercises code → collect coverage → reset
Test B starts → exercises code → collect coverage → reset
...
```

### Thread Safety

LLVM's profile runtime is thread-safe. The counter increments use atomic operations (since Rust defaults to `-C instrument-coverage` which implies atomic counters for multi-threaded programs).

## Coverage Collection (coverport)

Collection and report generation is handled by [coverport](https://github.com/konflux-ci/coverport), a multi-language coverage CLI.

### Collection Flow

```
1. Pod Discovery (or direct URL)
   coverport collect -n <ns> -l <selector>
   coverport collect --url http://localhost:53700/coverage

2. Port Forward (automatic for K8s)
   kubectl port-forward pod/<name> 0:<target-port>
   
3. HTTP Request
   GET http://127.0.0.1:<local-port>/coverage
   
4. Response Processing
   JSON → base64 decode → save .profraw file
   
5. Metadata
   Save metadata.json with pod/namespace/timestamp info
```

### Report Generation

```
coverport process --coverage-dir=./coverage-output --format=rust

1. Merge (llvm-profdata merge --sparse *.profraw -o coverage.profdata)
   Combines multiple profraw files into a single indexed profile

2. Text Report (llvm-cov report --instr-profile=coverage.profdata <binary>)
   Summary table with per-file coverage percentages

3. HTML Report (llvm-cov show --format=html --instr-profile=... <binary>)
   Interactive HTML with line-by-line coverage highlighting

4. LCOV Export (llvm-cov export --format=lcov --instr-profile=... <binary>)
   Industry-standard format for CI tools (Codecov, Coveralls, SonarCloud)
```

The instrumented binary must be available locally (via `COVERAGE_BINARY` env var or auto-discovered in `target/`).

## Comparison with Traditional Approaches

### Traditional: LLVM_PROFILE_FILE + Volume Mounts

```yaml
# Requires writable volume and deployment changes
containers:
  - name: app
    env:
      - name: LLVM_PROFILE_FILE
        value: /coverage/%p-%m.profraw
    volumeMounts:
      - name: coverage
        mountPath: /coverage
volumes:
  - name: coverage
    persistentVolumeClaim:
      claimName: coverage-pvc
```

Problems:
- Requires PVC or hostPath volume
- Must modify deployment manifests
- Data only available after pod termination (or with sidecar complexity)
- Doesn't work with read-only filesystems without volume exceptions

### This Approach: HTTP Server (Fully In-Memory)

```yaml
# No volume mounts, no manifest changes beyond the image
containers:
  - name: app
    securityContext:
      readOnlyRootFilesystem: true  # works!
    # Just the instrumented image - coverage server is built in
```

Advantages:
- Works with fully read-only root filesystems — no disk writes at all
- No volume mounts, PVC, emptyDir, or tmpfs required
- Collect coverage anytime without restarting the pod
- Reset counters for per-test granularity
- Single crate addition to the build (embed `coverage-server`)
- Data serialized directly from LLVM counters in process memory

## Build Considerations

### Production vs Test Builds

The Dockerfile supports both modes:

- **Production** (`ENABLE_COVERAGE=false`): Standard optimized build, no coverage overhead
- **Test** (`ENABLE_COVERAGE=true`): Instrumented build with ~10-20% runtime overhead

The `coverage-server` crate is gated behind a Cargo feature (`coverage`). When the feature is disabled, the dependency is not compiled at all, producing a clean production binary. When enabled, the LLVM FFI functions (`__llvm_profile_*`) must be present (via `-C instrument-coverage`), or linking will fail.

### Binary Size Impact

Coverage instrumentation adds:
- ~5-15% to binary size (coverage mapping data)
- Counter arrays proportional to the number of code regions

The `coverage-server` library adds ~50KB to the binary (axum HTTP server code).

## Limitations

1. **Binary required on client**: `llvm-cov` needs the instrumented binary to map counters back to source
2. **Binary must match**: The profraw data is only compatible with the exact binary that produced it
3. **LLVM version coupling**: `llvm-profdata` and `llvm-cov` must match the LLVM version used by rustc
4. **No incremental collection**: Counters accumulate; use `/coverage/reset` between tests for per-test data
5. **Memory allocation per request**: Each `/coverage` request allocates a buffer the size of the profraw data (typically a few MB) — this is freed after the response is sent
