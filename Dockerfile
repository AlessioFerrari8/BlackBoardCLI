# The relay as a small image. Build it on your machine, not on the server:
#   docker build -t blackboard-relay .
FROM rust:1.91-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked

FROM debian:bookworm-slim
COPY --from=build /src/target/release/blackboard /usr/local/bin/blackboard
USER 65532:65532
EXPOSE 7879
ENTRYPOINT [ "blackboard", "relay" ]