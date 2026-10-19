# syntax=docker/dockerfile:1
#
# Minimal rend-wui image: rend-wui (static musl binary), a stripped-down
# ffmpeg that only accepts raw PCM on a pipe and encodes the five audio
# formats rend supports, and exactly the shared libraries ffmpeg links
# against (musl's loader plus libmp3lame/libopus/libvorbis/libogg).
# Nothing else —
# the final stage is scratch and every file is copied in explicitly.

ARG FFMPEG_VERSION=8.0
ARG RUST_VERSION=1.89

# --- ffmpeg: audio encoders only ------------------------------------------
FROM alpine:3.21 AS ffmpeg
ARG FFMPEG_VERSION
RUN apk add --no-cache build-base musl-dev nasm pkgconf lame-dev opus-dev libvorbis-dev
WORKDIR /src
ADD https://ffmpeg.org/releases/ffmpeg-${FFMPEG_VERSION}.tar.xz .
RUN tar -xf ffmpeg-${FFMPEG_VERSION}.tar.xz
WORKDIR /src/ffmpeg-${FFMPEG_VERSION}
# The exact component set rend-encode uses: the pcm_s16le demuxer and
# decoder (raw PCM on pipe:0), the five ffmpeg-backed encoders, the muxers
# for their containers, aresample (auto-inserted for s16 -> fltp
# conversion), and the file/pipe protocols. Everything else — video,
# filters, network, all other decoders and protocols — is off.
RUN ./configure \
        --prefix=/opt \
        --enable-static \
        --disable-shared \
        --disable-everything \
        --disable-network \
        --disable-autodetect \
        --disable-doc \
        --enable-libmp3lame \
        --enable-libopus \
        --enable-libvorbis \
        --enable-demuxer=pcm_s16le \
        --enable-decoder=pcm_s16le \
        --enable-encoder=flac,aac,libmp3lame,libopus,libvorbis \
        --enable-muxer=flac,mp3,ipod,mp4,ogg \
        --enable-filter=aresample \
        --enable-protocol=file,pipe \
    && make -j"$(nproc)" \
    && make install \
    && strip /opt/bin/ffmpeg \
    && /opt/bin/ffmpeg -f s16le -ar 44100 -ac 2 -i /dev/zero -c:a flac -t 0.1 -y /tmp/f.flac \
    && /opt/bin/ffmpeg -f s16le -ar 44100 -ac 2 -i /dev/zero -c:a libmp3lame -t 0.1 -y /tmp/f.mp3 \
    && /opt/bin/ffmpeg -f s16le -ar 44100 -ac 2 -i /dev/zero -c:a aac -t 0.1 -y /tmp/f.m4a \
    && /opt/bin/ffmpeg -f s16le -ar 44100 -ac 2 -i /dev/zero -c:a libopus -t 0.1 -y /tmp/f.opus \
    && /opt/bin/ffmpeg -f s16le -ar 44100 -ac 2 -i /dev/zero -c:a libvorbis -t 0.1 -y /tmp/f.ogg \
    && install -d /opt/deps \
    && ldd /opt/bin/ffmpeg | awk '/=> \//{print $3}' | xargs -r -I{} cp -aL {} /opt/deps/

# --- rend-wui: static musl build ------------------------------------------
FROM rust:${RUST_VERSION}-alpine AS builder
RUN apk add --no-cache build-base musl-dev \
    && rustup target add x86_64-unknown-linux-musl
WORKDIR /src
COPY . /src
# The build dir is a cache mount and is not committed to the stage, so the
# finished binary is installed to a regular path.
RUN --mount=type=cache,target=/src/target \
    cargo build --release --target x86_64-unknown-linux-musl --package rend-wui \
    && install -m 0755 target/x86_64-unknown-linux-musl/release/rend-wui /usr/local/bin/rend-wui \
    && strip /usr/local/bin/rend-wui

# --- runtime: the two binaries and ffmpeg's shared libs, on scratch -------
FROM scratch
COPY --from=ffmpeg /opt/bin/ffmpeg /usr/local/bin/ffmpeg
COPY --from=ffmpeg /opt/deps/ /lib/
COPY --from=ffmpeg /lib/ld-musl-x86_64.so.1 /lib/ld-musl-x86_64.so.1
COPY --from=builder /usr/local/bin/rend-wui /usr/local/bin/rend-wui
# Explicit PATH: the bare `rend-wui` entrypoint and rend-wui's `ffmpeg`
# child process are both resolved through it. Without this, startup depends
# on the container runtime's default path.
ENV PATH=/usr/local/bin:/usr/bin:/bin
# Bind on all interfaces — but as an environment default, the
# lowest-precedence setting, so --host, -e REND_WUI_HOST=…, and
# REND_WUI_PORT still take effect. (The port keeps its built-in default,
# 3000, which EXPOSE documents.)
ENV REND_WUI_HOST=0.0.0.0
ENV REND_WUI_OUTPUT_DIR=/output
WORKDIR /output
EXPOSE 3000
VOLUME ["/output"]
ENTRYPOINT ["rend-wui"]
