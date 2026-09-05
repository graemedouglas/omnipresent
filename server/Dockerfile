# omni server image. Build context is the REPO ROOT (the build embeds
# client/install.sh):  docker build -f server/Dockerfile .
FROM rust:1-slim AS build
WORKDIR /src
COPY . .
RUN cargo build --release --manifest-path server/Cargo.toml

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/server/target/release/omni /usr/local/bin/omni
# Config at /etc/omni/omni.toml (the default), DB under /var/lib/omni,
# rathole's server.toml wherever the config points — mount all three.
ENTRYPOINT ["omni"]
CMD ["serve"]
