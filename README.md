# Narwhal

A web file manager for Kubernetes volumes, styled after desktop file managers
like KDE's Dolphin. The UI is Rust compiled to WebAssembly
([Leptos](https://leptos.dev)); the backend is a small [Axum](https://github.com/tokio-rs/axum)
server that confines every operation to one root directory (your PVC).

One container, one port, ~30 MB image, ~350 KB of gzipped WASM.

## Features

- **Places panel** with the volume and its top-level folders (or a fixed list), plus a free-space meter
- **Breadcrumb location bar** — click a segment to jump, click empty space (or Ctrl+L) to type a path
- **Four view modes** (Ctrl+1–4): Icons (with image thumbnails and a zoom slider), Compact,
  Details (sortable columns), and **Columns**, a macOS Finder–style view:
  - one column per folder from the root down; click a folder and its contents open to the right
  - selecting a file shows a preview panel (thumbnail for images, size, dates, Open/Download)
  - Left/Right arrows move between columns, Up/Down within one
  - drag items onto any folder in any column; right-click works in every column
- **Split view** (F3) with independent history per pane; drag between panes to move, hold Ctrl to copy
- **Selection** like a desktop: click, Ctrl+click, Shift+click, arrow keys, Ctrl+A
- **File operations**: new folder/file, inline rename (F2), cut/copy/paste, delete with confirmation,
  upload (button or drag files from your desktop, with progress), download, open in a new tab
- **Filter bar** (Ctrl+I) and **recursive name search** (Ctrl+F)
- **Properties** dialog with recursive folder size and permissions
- Hidden files toggle, natural sorting (`file2` before `file10`), light/dark theme from the OS,
  view preferences remembered per browser, current folder kept in the URL (`#/Photos/2025`)
- Mouse back/forward buttons work; F1 lists every shortcut

## Layout

```
shared/     serde types used by both sides (Entry, Listing, requests…)
server/     Axum API + static file serving         → binary `narwhal`
frontend/   Leptos CSR app built with Trunk         → frontend/dist
              (src/columns.rs is the column view)
deploy/     Kustomize manifests (namespace, PVC, Deployment, Service, Ingress)
Dockerfile  multi-stage build → distroless runtime image
```

## Running locally

Requirements: Rust (stable), the `wasm32-unknown-unknown` target and [Trunk](https://trunkrs.dev).

```sh
rustup target add wasm32-unknown-unknown
cargo install trunk --locked           # or download a release binary

# 1. build the UI once
cd frontend && trunk build --release && cd ..

# 2. serve a folder of your choice
NARWHAL_ROOT=$HOME/some-folder NARWHAL_STATIC_DIR=frontend/dist cargo run -p narwhal-server
# → http://localhost:8080
```

For UI work with live reload, run the server as above and in another terminal
`cd frontend && trunk serve` (port 8081; `/api` is proxied to 8080).

Tests: `cargo test -p narwhal-server -p narwhal-shared` (path traversal, symlink escape,
copy/move/rename/delete, search, naming).

## Container image

```sh
docker build -t ghcr.io/amreshh/narwhal:latest .

# Docker: run as your own user so the mounted folder is writable.
# 127.0.0.1 keeps it off your network: there is no login.
docker run --rm -p 127.0.0.1:8080:8080 --user "$(id -u):$(id -g)" \
  -v "$HOME/Downloads:/data" ghcr.io/amreshh/narwhal:latest

# Rootless Podman: map your user to the image's UID 65532 instead
podman run --rm -p 127.0.0.1:8080:8080 --userns=keep-id:uid=65532,gid=65532 \
  -v "$HOME/Downloads:/data" ghcr.io/amreshh/narwhal:latest
```

The server listens on port **8080** inside the container, so the right-hand side
of `-p` must be `8080`. On SELinux hosts (Fedora, RHEL) add
`--security-opt label=disable`, or `:z` on the volume, to let the container read
the folder.

The build stage needs network access to crates.io and GitHub (Trunk downloads
`wasm-bindgen` and `wasm-opt` on first use). Multi-arch works with
`docker buildx build --platform linux/amd64,linux/arm64`.

## Deploying to Kubernetes

1. Push the image and set its name in `deploy/kustomization.yaml`.
2. Set your hostname in `deploy/ingress.yaml` and **enable authentication**
   (see below).
3. `kubectl apply -k deploy`

To browse an existing volume instead of creating a new one, remove `pvc.yaml`
from the kustomization and set `claimName` in `deployment.yaml`.

The pod runs as UID/GID 65532 with a read-only root filesystem, no capabilities
and the `restricted` Pod Security level; `fsGroup` makes the volume writable.
With a ReadWriteOnce volume keep `replicas: 1` (strategy `Recreate`).

### Configuration

| Variable | Default | Meaning |
|---|---|---|
| `NARWHAL_ROOT` | `/data` | Directory to serve. Nothing outside it is reachable. |
| `NARWHAL_ROOT_NAME` | `Storage` | Name shown for the root in the sidebar and breadcrumb. |
| `NARWHAL_READ_ONLY` | `false` | `true` rejects every write (upload, rename, delete, …). |
| `NARWHAL_PLACES` | *(empty)* | Fixed sidebar entries: `Docs=/documents,Media=/media`. Empty = top-level folders. |
| `NARWHAL_MAX_UPLOAD_MB` | `0` | Request body limit for uploads; `0` = unlimited. |
| `NARWHAL_STATIC_DIR` | `./static` | Built frontend (`/app/static` in the image). |
| `NARWHAL_ALLOWED_HOSTS` | *(empty)* | Comma-separated host names the server answers to, e.g. `files.example.com,localhost`. Other hosts get 403 (blocks DNS rebinding). Empty = any. `/healthz` is always allowed. |
| `NARWHAL_BIND` | `[::]:8080` | Listen address. The default accepts IPv4 and IPv6 (falls back to IPv4 if IPv6 is disabled); `0.0.0.0:8080` forces IPv4 only. |
| `RUST_LOG` | `info,tower_http=info` | Log filter. |

## Security notes

- **There is no built-in login.** Anyone who can reach the Service can read and
  modify the volume. Put it behind your ingress's authentication — the ingress
  manifest has commented annotations for oauth2-proxy — or set `NARWHAL_READ_ONLY=true`.
- Write requests from other sites are refused (checked with the browser's
  `Sec-Fetch-Site` / `Origin` headers), so a malicious page cannot upload or
  delete with your login. Set `NARWHAL_ALLOWED_HOSTS` to your hostname to also
  refuse DNS-rebinding requests. Pages are sent with `X-Frame-Options: DENY`.
- All paths are resolved against the root; `..` is rejected and symlinks that
  point outside the root are refused.
- Files opened "in a new tab" are served with `Content-Security-Policy: sandbox`
  and `nosniff`, so an uploaded HTML or SVG file cannot run scripts against the app.
- Uploads are written to a hidden temp file and renamed into place; name clashes
  get ` (1)`, ` (2)`… suffixes rather than overwriting. Uploads, copies, moves and
  renames never replace an existing file.

## API

All endpoints take and return JSON; paths are virtual (`/` = the root).

| Method | Path | Body / query |
|---|---|---|
| GET | `/api/info` | volume name, read-only flag, disk usage, places |
| GET | `/api/list?path=` | folder listing |
| GET | `/api/search?path=&q=` | recursive name search (max 500 results) |
| GET | `/api/size?path=` | recursive size |
| GET | `/api/download?path=[&inline=true]` | file download (supports Range) |
| POST | `/api/upload?path=` | multipart, one or more `file` fields |
| POST | `/api/mkdir`, `/api/touch` | `{ "path": parent, "name": … }` |
| POST | `/api/rename` | `{ "path": …, "new_name": … }` |
| POST | `/api/transfer` | `{ "paths": [...], "dest": folder, "op": "copy" \| "move" }` |
| POST | `/api/delete` | `{ "paths": [...] }` |
| GET | `/healthz` | liveness/readiness |

## Known limitations

- Dropping whole *folders* from the desktop isn't supported yet (files are).
- Deletes are permanent; there is no trash.
- Thumbnails are the original images (lazy-loaded, ≤25 MB each), not server-side resized.
- Downloading a folder as an archive isn't implemented.
