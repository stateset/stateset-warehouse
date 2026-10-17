FROM node:22-bookworm-slim AS web
WORKDIR /build/dashboard
COPY dashboard/package*.json ./
RUN npm ci --no-audit --no-fund
COPY dashboard/ ./
RUN npm run build

FROM rust:1.90-bookworm AS api
WORKDIR /build
ARG CARGO_BUILD_JOBS=4
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY migrations ./migrations
RUN cargo build --locked --release --jobs "$CARGO_BUILD_JOBS"

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl && rm -rf /var/lib/apt/lists/* && useradd --system --uid 10001 warehouse
WORKDIR /app
COPY --from=api /build/target/release/stateset-warehouse /usr/local/bin/stateset-warehouse
COPY --from=web /build/dashboard/dist ./dashboard/dist
USER 10001
ENV WMS_BIND=0.0.0.0:8088 WMS_DEMO=false
EXPOSE 8088
HEALTHCHECK --interval=10s --timeout=3s --start-period=15s --retries=5 \
  CMD ["curl", "--fail", "--silent", "--show-error", "http://127.0.0.1:8088/ready"]
CMD ["stateset-warehouse"]
