# Builds the Linux x64 release assets of Ogun on Ubuntu 22.04 (glibc 2.35), the oldest base Tauri's AppImage
# supports: ogun, ogund, ogun-server and ogun-ingest, and Ogun Studio as .AppImage and .deb.
#   docker build -f deploy/pre-alpha/release/linux.Dockerfile -t ogun-rel-linux:local deploy/pre-alpha/release
#   docker run --rm -v A:/ogun-demo:/src/ogun:ro -v A:/zeus-core:/src/zeus-core:ro -v ogun-rel-out:/out ... (build.sh)
FROM ubuntu:22.04
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates curl build-essential pkg-config git rsync file xz-utils \
      libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev libssl-dev libxdo-dev \
 && rm -rf /var/lib/apt/lists/*
RUN curl -fsSL https://nodejs.org/dist/v22.22.2/node-v22.22.2-linux-x64.tar.xz | tar -xJ -C /usr/local --strip-components=1
ENV RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo PATH=/usr/local/cargo/bin:$PATH
RUN curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain 1.90.0 \
 && cargo install tauri-cli --version "^2.6" --locked
COPY build-linux.sh /usr/local/bin/build-linux.sh
RUN chmod 0755 /usr/local/bin/build-linux.sh
CMD ["build-linux.sh"]
