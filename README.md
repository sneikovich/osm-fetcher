# overpass-api-fetcher

Rust-клієнт до [Overpass API](https://wiki.openstreetmap.org/wiki/Overpass_API) (OpenStreetMap). Містить три частини:

| ціль | що це |
|---|---|
| `overpass` | CLI |
| `overpass-server` | HTTP API (stateless, для контейнера) |
| `overpass` (lib) | бібліотека: `Query`, `Client`, `Element` |

Потрібен Rust ≥ 1.85 (edition 2024).

## Збірка й тести

```sh
cargo build --release
cargo test                 # live-тест проти overpass-api.de: cargo test -- --ignored
```

## CLI

```sh
cargo run --bin overpass -- -t amenity=cafe --around 50.4501,30.5234,300
cargo run --bin overpass -- -t shop=bakery -t name -k node --bbox 50.40,30.40,50.50,30.60
cargo run --bin overpass -- --ql '[out:json];node["amenity"="cafe"](around:300,50.45,30.52);out center;'
```

| прапорець | значення |
|---|---|
| `-t, --tag` | `key=value` або `key`, можна кілька (логічне І) |
| `--bbox` | `south,west,north,east` |
| `--around` | `lat,lon,radius_m` (несумісний з `--bbox`) |
| `-k, --kind` | `node` / `way` / `relation`, за замовчуванням усі |
| `--timeout` | таймаут на сервері Overpass, с (25) |
| `--ql` | сирий Overpass QL, обов'язково `[out:json]`; несумісний з іншими фільтрами |
| `--endpoint` | URL інтерпретатора (`https://overpass-api.de/api/interpreter`) |
| `--retries` | повтори при 429/504, з експоненційною затримкою 2, 4, 8 с (3) |
| `--json` | вивести JSON замість таблиці |

Таблиця йде в stdout, кількість елементів і повідомлення про повтори — у stderr. Код виходу 1 при помилці.

## HTTP-сервер

```sh
cargo run --bin overpass-server
OVERPASS_LISTEN=127.0.0.1:9000 OVERPASS_ENDPOINT=https://overpass.private.coffee/api/interpreter \
  cargo run --bin overpass-server
```

| змінна | прапорець | за замовчуванням |
|---|---|---|
| `OVERPASS_LISTEN` | `--listen` | `0.0.0.0:8080` |
| `OVERPASS_ENDPOINT` | `--endpoint` | `https://overpass-api.de/api/interpreter` |
| `OVERPASS_RETRIES` | `--retries` | `3` |

Endpoint задається лише на сервері, клієнт API його змінити не може. Сервер коректно завершується по SIGTERM/SIGINT.

### `POST /api/query`

```sh
curl -s localhost:8080/api/query -H 'content-type: application/json' \
  -d '{"tags":["amenity=cafe"],"around":[50.4501,30.5234,300],"kind":"node","timeout":25}'
```

| поле | тип | обов'язкове |
|---|---|---|
| `tags` | `string[]`, `key=value` або `key` | так, ≥ 1 |
| `bbox` | `[south, west, north, east]` | ні |
| `around` | `[lat, lon, radius_m]`, radius > 0 | ні, несумісне з `bbox` |
| `kind` | `"node"` / `"way"` / `"relation"` | ні |
| `timeout` | `1..=180`, с | ні, 25 |

Невідомі поля відхиляються.

Відповідь 200 — JSON Overpass як є: `{"elements":[...]}`. Координати: у `node` — `lat`/`lon`, у `way`/`relation` — `center`.

Помилки — `{"error":"..."}`:

| код | причина |
|---|---|
| 400 | невалідний запит |
| 502 | Overpass повернув помилку, невалідний JSON або мережева помилка |
| 503 | Overpass перевантажений (429) після всіх повторів |
| 504 | Overpass не встиг (504) після всіх повторів |

### `GET /healthz`

`200 ok`. Overpass не перевіряє.

## Docker

```sh
docker build -t overpass-server .
docker run --rm -p 8080:8080 -e OVERPASS_ENDPOINT=... overpass-server
```

Образ: distroless `cc-debian13:nonroot`, ~35 МБ. Shell і curl в образі немає, тому healthcheck — ззовні через `/healthz`.

Разом із фронтендом: `compose.yaml` у корені `app/`, див. [../frontend/README.md](../frontend/README.md).

## Відомі проблеми

- Публічний `overpass-api.de` часто віддає 429/504. Альтернатива: `https://overpass.private.coffee/api/interpreter`.
- Запит без `bbox`/`around` шукає по всій планеті — з поширеним тегом гарантовано впаде по таймауту.
