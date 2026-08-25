# Running the engine on lichess

`docker/Dockerfile` builds an image containing
[lichess-bot](https://github.com/lichess-bot-devs/lichess-bot), the engine and an opening book,
which is enough to run the engine as a bot account on [lichess.org](https://lichess.org).

```
docker run -e LICHESS_BOT_TOKEN=<token> ghcr.io/aywrite/arche-lichess-bot:latest
```

The token is a lichess API token for a bot account with the `bot:play` scope. The engine
allocates a 256MB transposition table, so give the container at least 512MB of memory. A
different size is asked for through lichess-bot's `uci_options` map, which is commented out in
`docker/config.yml` at the default. To change that or any other setting, mount a replacement
over `/lichess-bot/config.yml`; the defaults are in `docker/config.yml`.

## Tags

Images are published to `ghcr.io/aywrite/arche-lichess-bot` on release only. A release is tagged
`latest`, `<major>.<minor>`, `<version>` and `v<version>`, the last of which matches the git tag
and is the one quoted in the release notes along with its digest. A pull request or a push to
master builds and smoke tests the image without publishing it. The `master` tag that earlier
versions published is no longer updated; a bot still running it is on whatever master was then.

A published image carries a build provenance attestation, filed against this repository and
looked up by digest, so a tag can be checked against where it came from rather than trusted for
being in the right place:

```
gh attestation verify oci://ghcr.io/aywrite/arche-lichess-bot:v<version> --repo aywrite/arche
```

## The book

The book is generated at build time by `docker/build_book.py` from
[lichess-org/chess-openings](https://github.com/lichess-org/chess-openings) (CC0). Each move is
weighted by the number of named openings that play it, so common theory is chosen far more often
than novelties.

## Building and checking an image locally

```
docker build -f docker/Dockerfile -t arche-lichess-bot .
docker/smoke_test.sh arche-lichess-bot
```
