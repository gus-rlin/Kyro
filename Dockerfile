# Images official multi-platform pinnees par digest (registre verifie le 2026-10-03).
FROM rust:1.96.1-slim-bookworm@sha256:e18a79fc84dfcfc3ab5ba72290398a644c135c97eaa881447fddc354ee4701a3 AS build

WORKDIR /workspace
RUN apt-get update \
    && apt-get install --yes --no-install-recommends build-essential pkg-config \
    && rm -rf /var/lib/apt/lists/*

COPY . .
RUN cargo build --locked --release \
    --bin kyro-api \
    --bin kyro-worker \
    --bin kyro-migrate

FROM debian:bookworm-20260918-slim@sha256:3783cc01769c7b2b1b83a5c5ad96c815348e28ed7da68e2e3687004faa906251 AS runtime

RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=build --chown=10001:10001 /workspace/target/release/kyro-api /usr/local/bin/kyro-api
COPY --from=build --chown=10001:10001 /workspace/target/release/kyro-worker /usr/local/bin/kyro-worker
COPY --from=build --chown=10001:10001 /workspace/target/release/kyro-migrate /usr/local/bin/kyro-migrate
COPY --from=build --chown=10001:10001 /workspace/config/models.example.json /app/config/models.example.json

ENV KYRO_ENV=production \
    KYRO_BIND=0.0.0.0:8080

WORKDIR /app
USER 10001:10001
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/kyro-api"]
