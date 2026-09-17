# Application-managed JSON service

The toolkit still runs every existing CLI command directly. The optional
`serve` mode adds HTTP access to the same commands and successful JSON results.
Ship `pdf-toolkit` and the matching PDFium library together, as produced by
`./build.sh`. No external queue, database, or Node.js runtime is needed by the
service itself. PM2 is optional.

## Start and configure

```bash
./dist/pdf-toolkit serve
./dist/pdf-toolkit serve --max-workers 16 --bind 127.0.0.1:8787
./dist/pdf-toolkit serve --help
```

| Argument | Default | Meaning |
|---|---|---|
| `--bind` | `127.0.0.1:8787` | HTTP listener |
| `--max-workers` | **8** | Maximum simultaneously executing CLI processes |
| `--max-queue` | `32` | Additional request capacity beyond worker count |
| `--worker-threads` | `1` | Rayon threads per child, particularly relevant to downsampling |
| `--timeout-seconds` | `3600` | Deadline covering queueing and execution |
| `--shutdown-grace-seconds` | `30` | Time for running work to finish on shutdown |

With defaults, eight jobs can execute and up to forty requests can be admitted.
Admission also covers incoming bodies and outgoing JSON streams, so slow clients
cannot create an unbounded collection of buffers or temporary files. At capacity,
new job requests receive JSON `503` with `Retry-After: 1`. Health/status requests
remain available. Request bodies are limited to 64 KiB and ten seconds to arrive.
The execution deadline does not include delivery of a completed response.

The worker count is a concurrency limit, **not a RAM reservation or a memory
cap**. Eight simultaneous downsampling or large dossier jobs can exceed a small
host's RAM. Tune it against representative PDFs and reserve resources for the
parent application. The service does not change rendering resolution or the
existing downsampling batch budget.

The same configuration works for render/text, dossier/model/building, and
downsampling hosts; all commands remain available. Customize the limits per host.

## Requests and the existing JSON contract

Pass arguments as separate strings, exactly as in the CLI, without a shell:

```bash
curl --fail-with-body http://127.0.0.1:8787/v1/run \
  -H 'Content-Type: application/json' \
  -d '{"args":["inspect","/data/input.pdf"]}'

mkdir -p /data/jobs/job-123
curl --fail-with-body http://127.0.0.1:8787/v1/render \
  -H 'Content-Type: application/json' \
  -d '{"args":["/data/input.pdf","--pages","1-3","--scale-width-px","1400","--output","/data/jobs/job-123"]}'
```

`POST /v1/run` takes the command as the first element. Alternatively,
`POST /v1/{command}` takes only its arguments. All eleven commands use the
existing CLI parser, including `model`, `building`, and every model/parcel/answers
option. `serve`, help, and version requests cannot be run as worker jobs.

Successful responses are the CLI's exact JSON bytes, including partial-page
results, with `Content-Type: application/json`. See [the JSON contract](json-api.md).
Worker stderr is spooled separately; failure responses include at most 8 KiB of
its beginning. Successful stderr progress/warnings are not included in the HTTP
body; the existing JSON warning/error fields remain unchanged.

Files are on the **service host's filesystem**. Input, parcel, answers, and output
paths resolve against its working directory. Prefer absolute paths. `cut`,
`render`, and `text` require an explicit `--output` over HTTP; `downsample` already
requires it in the CLI. Use an existing, separate output directory per request,
and a distinct output file for each downsample request. The caller owns these
artifacts and their eventual deletion. There is no upload/download API in this
version; colocated applications or applications with shared storage use the paths
already returned by the toolkit. CLI defaults are unchanged.

| HTTP status | Meaning |
|---|---|
| `200` | Successful CLI result, including its normal partial results |
| `400` | Invalid JSON or command arguments |
| `401` | Missing or incorrect configured bearer token |
| `408` | Request body took too long |
| `413` | Request body too large or unreadable |
| `415` | Request is not `application/json` |
| `422` | CLI failed; includes a bounded stderr diagnostic |
| `500` | Coordinator/spawn/I/O failure |
| `503` | Capacity exhausted or shutdown in progress |
| `504` | Queue/execution deadline expired; worker terminated and reaped |

Service errors have the shape `{"error":{"message":"..."}}`. A timeout or
cancelled process may leave partial output artifacts; callers should discard a
failed job's output directory. Do not retry paid model work automatically without
considering duplicate calls. There is no persisted job queue or replay on restart.

## Lifecycle and health

| Operation | Interface |
|---|---|
| Start | Spawn `pdf-toolkit serve` and retain its process handle |
| Health | `GET /health` → `{"status":"ok"}` |
| Ready | `GET /ready` → `{"status":"ready"}`; `503` while stopping |
| Status | `GET /status` → PID, version, uptime, active/outstanding counts and limits |
| Stop | Send SIGINT/SIGTERM, or `POST /shutdown` (`202`) |
| Restart | Stop, wait for exit, then spawn a new service |

`/ready` describes coordinator readiness; it does not run a PDF or probe hosted
model credentials. Startup fails if binding the port fails. Poll `/ready` with a
startup deadline; a process manager's "online" status alone is not readiness.

On shutdown the listener closes, queued work is rejected, and active jobs get the
configured grace period. Remaining children are killed and reaped. Slow HTTP
connections are cut off after grace plus two seconds. The supervising application
should allow an additional margin before forcibly killing the service.

Each worker monitors a pipe owned by the coordinator. If the coordinator exits
or is forcibly killed, EOF terminates the worker, including while a native call
or hosted-model request is blocked. Worker processes must not be detached.

## Idle memory

There are **no idle workers**. PDFium, decoded images, document geometry, Rayon
threads and model-client allocations live in disposable CLI processes. Once a
worker exits, its private process memory is reclaimed. The coordinator uses
bounded request bodies and streams disk-spooled JSON in 16 KiB chunks. Temporary
stdout/stderr files are automatically removed; put `TMPDIR` on disk rather than
a RAM-backed filesystem if large results are expected.

After all responses finish, `/status` should report `active_workers: 0` and
`outstanding_requests: 0`. Coordinator RSS can retain small allocator/runtime
high-water allocations; exact startup RSS is not guaranteed. Validate a stable
idle baseline over repeated workload bursts. Filesystem cache is reclaimable OS
memory and should be distinguished from retained process memory. PM2 itself also
has its own independent memory overhead.

## PM2

The repository's [`ecosystem.config.js`](../ecosystem.config.js) launches the
native binary with `interpreter: "none"`, one fork-mode coordinator, and eight
workers. Do **not** set PM2 instances to eight; the coordinator owns concurrency.

```bash
pm2 start ecosystem.config.js
pm2 status pdf-toolkit
pm2 logs pdf-toolkit
pm2 restart pdf-toolkit
pm2 stop pdf-toolkit
pm2 delete pdf-toolkit
```

To change the worker count through the ecosystem configuration:

```bash
PDF_TOOLKIT_MAX_WORKERS=16 pm2 start ecosystem.config.js
# For an existing process, re-read the configuration and environment:
PDF_TOOLKIT_MAX_WORKERS=12 pm2 restart ecosystem.config.js --update-env
```

| Environment variable read by the ecosystem file | Default |
|---|---|
| `PDF_TOOLKIT_BINARY` | `dist/pdf-toolkit`, relative to the ecosystem file |
| `PDF_TOOLKIT_CWD` | Directory containing the ecosystem file |
| `PDF_TOOLKIT_BIND` | `127.0.0.1:8787` |
| `PDF_TOOLKIT_MAX_WORKERS` | `8` |
| `PDF_TOOLKIT_MAX_QUEUE` | `32` |
| `PDF_TOOLKIT_WORKER_THREADS` | `1` |
| `PDF_TOOLKIT_TIMEOUT_SECONDS` | `3600` |
| `PDF_TOOLKIT_SHUTDOWN_GRACE_SECONDS` | `30` |

The ecosystem translates these values into CLI arguments. They are not separately
read by `serve` when invoked directly. PM2's kill timeout is derived from the grace
period plus ten seconds. `treekill: false` lets the coordinator drain its own
workers; the parent pipe handles forced coordinator termination. PM2 readiness
IPC is disabled because this is a native executable; use `/ready`.

For application-managed stopping under PM2, use `pm2 stop` rather than
`POST /shutdown`: PM2's configured autorestart would restart a process stopped
through the HTTP endpoint. Use `pm2 restart`, not zero-downtime cluster reload,
when changing the service configuration.

## Private access

Loopback is the default. To bind a private network address, set
`PDF_TOOLKIT_SERVICE_TOKEN`; non-loopback listeners refuse to start without it.
When configured, **every endpoint**, including health and shutdown, requires:

```text
Authorization: Bearer <token>
```

Supply the token and hosted-model API credentials through the parent process or
PM2 environment; do not commit secrets to the ecosystem file. This is a trusted
internal API with access to the service account's filesystem, not a sandbox or a
public upload endpoint. Network deployments should use a protected network or TLS
termination; the built-in listener is HTTP.
