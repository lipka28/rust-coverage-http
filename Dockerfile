ARG ENABLE_COVERAGE=false

# Build stage
FROM rust:1.83-bookworm AS builder

WORKDIR /build

# Copy workspace
COPY Cargo.toml ./
COPY coverage-server/ coverage-server/
COPY example-app/ example-app/

ARG ENABLE_COVERAGE

# Build with or without coverage instrumentation
RUN if [ "$ENABLE_COVERAGE" = "true" ]; then \
        echo "Building with coverage instrumentation"; \
        rustup component add llvm-tools-preview; \
        RUSTFLAGS="-C instrument-coverage" cargo build --release -p example-app; \
    else \
        echo "Building without coverage (production)"; \
        cargo build --release -p example-app; \
    fi

# Runtime stage
FROM debian:bookworm-slim AS runtime

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /build/target/release/example-app /app/example-app

ENV APP_PORT=8000
ENV COVERAGE_PORT=9095

EXPOSE 8000
EXPOSE 9095

USER 1000:1000

ENTRYPOINT ["/app/example-app"]
