# A lichess-bot image with the arche engine and a Polyglot opening book baked in.
#
#   docker build -f docker/Dockerfile -t arche-lichess-bot .
#   docker run -e LICHESS_BOT_TOKEN=<token> arche-lichess-bot

ARG DEBIAN_RELEASE=bookworm
ARG PYTHON_VERSION=3.13
ARG RUST_VERSION=1.90

FROM rust:${RUST_VERSION}-slim-${DEBIAN_RELEASE} AS engine
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY basic_engine ./basic_engine
# v2 is popcnt and sse4.2, everything a hosted runner or a vps has had for
# fifteen years; the image is not the artifact a stranger with old hardware
# reaches for, the release page's baseline build is
ENV RUSTFLAGS="-C target-cpu=x86-64-v2"
RUN cargo build --release --locked --bin arche

FROM python:${PYTHON_VERSION}-slim-${DEBIAN_RELEASE} AS book
# Pinned commit of lichess-org/chess-openings, a CC0 data set of named openings.
ARG OPENINGS_REF=51b886249b9e418498d25b6e39b926c3de99c29a
WORKDIR /book
ADD https://codeload.github.com/lichess-org/chess-openings/tar.gz/${OPENINGS_REF} openings.tar.gz
RUN pip install --no-cache-dir "chess~=1.11" \
    && mkdir openings \
    && tar -xzf openings.tar.gz --strip-components=1 -C openings
COPY docker/build_book.py .
RUN python3 build_book.py openings arche-book.bin

FROM python:${PYTHON_VERSION}-slim-${DEBIAN_RELEASE} AS lichess-bot-source
ARG LICHESS_BOT_REF=388b6c9de4fa6e6bbd75f166b3c8b29486fa096b
ADD https://codeload.github.com/lichess-bot-devs/lichess-bot/tar.gz/${LICHESS_BOT_REF} lichess-bot.tar.gz
RUN mkdir /lichess-bot && tar -xzf lichess-bot.tar.gz --strip-components=1 -C /lichess-bot

FROM python:${PYTHON_VERSION}-slim-${DEBIAN_RELEASE}
ARG ARCHE_VERSION=dev
LABEL org.opencontainers.image.title="arche-lichess-bot"
LABEL org.opencontainers.image.description="The arche chess engine bundled with lichess-bot and an opening book"
LABEL org.opencontainers.image.source="https://github.com/aywrite/arche"
LABEL org.opencontainers.image.version="${ARCHE_VERSION}"
LABEL org.opencontainers.image.licenses="GPL-3.0-or-later AND AGPL-3.0-or-later"

ENV PYTHONDONTWRITEBYTECODE=1
ENV PYTHONUNBUFFERED=1
WORKDIR /lichess-bot

COPY --from=lichess-bot-source /lichess-bot/ ./
RUN pip install --no-cache-dir -r requirements.txt

COPY --from=engine /src/target/release/arche engines/arche
COPY LICENSE engines/arche-LICENSE
COPY --from=book /book/arche-book.bin engines/arche-book.bin
COPY docker/config.yml config.yml

RUN useradd --create-home --uid 1000 bot && chown -R bot:bot /lichess-bot
USER bot

CMD ["python3", "lichess-bot.py", "--disable_auto_logging"]
