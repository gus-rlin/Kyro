# Only this directory is a build context. No project, key or environment file
# enters the tools image. Workloads use runsc, not the outer runc container.
ARG TOOLS_IMAGE=p2-p1-baseline-tools:20261005
FROM ${TOOLS_IMAGE}
USER root
RUN apt-get update && apt-get install --no-install-recommends -y bzip2 git postgresql-15 && rm -rf /var/lib/apt/lists/*
ADD https://storage.googleapis.com/gvisor/releases/release/20260928.0/x86_64/gvisor.tar.bz2 /tmp/gvisor.tar.bz2
RUN printf '%s  %s\n' 'c8d3a9fd4d4c4f5b8ff213caa4517356be128d18659ec4cde37828fe797f61a9725a602a846c81a8ed19c057a996515d31c081eba343ed4613a89951ba32ed59' '/tmp/gvisor.tar.bz2' | sha512sum -c - && tar -xjf /tmp/gvisor.tar.bz2 -C /usr/local/bin && rm /tmp/gvisor.tar.bz2
RUN mkdir -p /sandbox /input /output /work /candidate /driver /criteria && chown 1000:1000 /sandbox && rmdir /workspace
RUN mkdir -p /opt/rust /opt/cargo /opt/kyro && cp -a /root/.rustup/toolchains/1.96.1-x86_64-unknown-linux-gnu/. /opt/rust/ && chmod -R a+rX /opt/rust
# These locations belong to the public tools image. Retain no package-created
# database, machine TLS private key, root cache, or Cargo credentials in a guest.
RUN rm -rf /root/.cargo /root/.rustup /var/lib/postgresql /var/log/postgresql /etc/postgresql/15 /etc/ssl/private && mkdir -p /etc/ssl/private /var/lib/postgresql
# Cargo vendor verifies registry checksums. Only the public source directory
# produced with --locked is copied; Cargo credentials/configuration are absent.
COPY vendor /opt/vendor
COPY cargo-config.toml /opt/cargo/config.toml
COPY build.sh /opt/kyro/build.sh
RUN chmod -R a+rX /opt/vendor /opt/cargo /opt/kyro
USER 1000:1000
WORKDIR /sandbox
ENTRYPOINT ["/bin/sh"]
