# tome-collector

Receives the anonymous usage statistics that The Shuffling Tome servers send
once their admin allows it (see the main README, "Usage statistics"). It shows
them on a password-protected metrics page together with the project's GitHub
numbers: stars, forks, watchers, open issues, page views, unique visitors,
clones, release downloads, container image pulls and sponsors.

The app sends to `https://www.luminaldata.com/app/shufflingtome/v1/ping`, so
this runs on that server behind its web server.

| Path | |
|---|---|
| `POST /app/shufflingtome/v1/ping` | pings from servers |
| `GET /app/shufflingtome/` | the metrics page (any user name, `TOME_COLLECTOR_PASSWORD`) |
| `GET /app/shufflingtome/api/metrics` | the same numbers as JSON |
| `GET /app/shufflingtome/health` | `ok` |

## What it keeps

One row per server and day, holding the day's last ping (the fields of
`shared/src/telemetry.rs`), for 400 days. It also keeps GitHub's numbers per
day and each release's downloads. Each ping is checked against the listed
fields and values; anything else is refused. IP addresses are never stored:
they are held in memory only to limit each address to 10 pings an hour and 10
wrong passwords in 15 minutes.

The SQLite file is `/data/collector.db` in the container's volume. Back it up
the way you back up the server's other data.

## Install with Docker Compose

On the server, in a copy of this folder (`compose.yaml`, `collector.env.example`):

1. `cp collector.env.example collector.env`, then set:
   * `TOME_COLLECTOR_PASSWORD` to a long random password;
   * `GITHUB_TOKEN`, optional: a fine-grained token with read-only
     "Administration" access to the repository, needed for traffic and sponsors.
2. Get the image. The image workflow publishes it as
   `ghcr.io/johnflarson/tome-collector:latest`. If the package is private, run
   `docker login ghcr.io` first, or make the package public in its settings on
   GitHub. Then `docker compose pull`.
   Without the image, build it from a checkout of the repository with
   `docker compose up -d --build`.
3. `docker compose up -d`, then check with `docker compose ps` (healthy) and
   `curl http://127.0.0.1:8790/app/shufflingtome/health`.

It listens on `127.0.0.1:8790` only. The web server passes the path on unchanged.

**nginx**, in the `server` block for www.luminaldata.com:

```nginx
location /app/shufflingtome/ {
    proxy_pass http://127.0.0.1:8790;
    proxy_set_header Host $host;
    proxy_set_header X-Forwarded-For $remote_addr;
    proxy_set_header X-Forwarded-Proto $scheme;
    client_max_body_size 8k;
}
location = /app/shufflingtome {
    return 301 /app/shufflingtome/;
}
```

**Caddy:**

```caddy
www.luminaldata.com {
    handle /app/shufflingtome* {
        reverse_proxy 127.0.0.1:8790
    }
}
```

Caddy sets `X-Forwarded-For` itself.

Test it from anywhere: `curl https://www.luminaldata.com/app/shufflingtome/health`
should print `ok`. Then open `https://www.luminaldata.com/app/shufflingtome/`.

Updates: `docker compose pull && docker compose up -d`.

## Without Docker

`cargo build --release -p tome-collector` gives `target/release/tome-collector`.
It reads the same variables from its environment. `TOME_COLLECTOR_BIND`
defaults to `127.0.0.1:8790` and `TOME_COLLECTOR_DB` to `collector.db` in the
working folder.

## Sending to your own collector

Self-hosters can run this too: point the app at it with
`MTG_SCAN_TELEMETRY_URL=https://your.server/path/v1/ping` (or `[telemetry] url`).
