FROM rust:1-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY web ./web
RUN cargo build --release --locked

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --home /data tgvault && mkdir -p /data && chown tgvault /data
COPY --from=build /src/target/release/tgvault /usr/local/bin/tgvault
USER tgvault
ENV TGV_BIND=0.0.0.0:8787 TGV_DATA_DIR=/data
VOLUME ["/data"]
EXPOSE 8787
HEALTHCHECK --interval=30s --timeout=3s CMD ["/bin/bash", "-c", "exec 3<>/dev/tcp/127.0.0.1/8787"]
ENTRYPOINT ["tgvault"]
CMD ["serve"]
