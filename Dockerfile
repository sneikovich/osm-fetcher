FROM docker.io/library/rust:1-slim-trixie AS build
WORKDIR /src
# Cache dependencies separately from sources.
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src/bin && echo 'fn main() {}' > src/main.rs && cp src/main.rs src/bin/server.rs \
    && touch src/lib.rs && cargo build --release --bin overpass-server && rm -rf src
COPY src src
RUN touch src/lib.rs src/bin/server.rs && cargo build --release --bin overpass-server

FROM gcr.io/distroless/cc-debian13:nonroot
COPY --from=build /src/target/release/overpass-server /overpass-server
ENV OVERPASS_LISTEN=0.0.0.0:8080
EXPOSE 8080
ENTRYPOINT ["/overpass-server"]
