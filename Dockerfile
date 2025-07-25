# syntax=docker/dockerfile:1-labs
ARG GH_BRANCH=main
FROM rust:latest AS stage-rust
ARG GH_BRANCH
ENV GH_BRANCH=${GH_BRANCH}
# crosscompile stuff
RUN apt update && apt upgrade -y
RUN apt install -y gcc-arm-linux-gnueabihf
RUN rustup target add arm-unknown-linux-gnueabihf
# cloning and building
WORKDIR /usr/src/app
ADD . / /usr/src/app
RUN echo $(ls -1 /usr/src/app)
RUN cargo build --release
#RUN cargo build
RUN arm-linux-gnueabihf-strip target/arm-unknown-linux-gnueabihf/release/poor_mans_obd
#RUN arm-linux-gnueabihf-strip target/arm-unknown-linux-gnueabihf/debug/poor_mans_obd
# COPY stage-rust:/usr/src/app/target/arm-unknown-linux-gnueabihf/release/poor_mans_obd .
# Pi Zero W needs special linking/building (https://github.com/manio/aa-proxy-rs/issues/3)
# RUN git clone --depth=1 https://github.com/raspberrypi/tools
# RUN CARGO_TARGET_DIR=pi0w CARGO_TARGET_ARM_UNKNOWN_LINUX_GNUEABIHF_LINKER="./tools/arm-bcm2708/arm-rpi-4.9.3-linux-gnueabihf/bin/arm-linux-gnueabihf-gcc" cargo build --release

# Final stage: only the binary is placed at /poor_mans_obd
FROM scratch AS export
COPY --from=stage-rust /usr/src/app/target/arm-unknown-linux-gnueabihf/release/poor_mans_obd /poor_mans_obd
#COPY --from=stage-rust /usr/src/app/target/arm-unknown-linux-gnueabihf/debug/poor_mans_obd /poor_mans_obd

# docker build --target export --output type=local,dest=./output .