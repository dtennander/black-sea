FROM rust:alpine AS builder
RUN apk add --no-cache musl-dev
WORKDIR /app
COPY . .
RUN cargo build --release --bin server

FROM alpine:latest
COPY --from=builder /app/target/release/server /server
EXPOSE 7456
CMD ["/server"]
