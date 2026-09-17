const path = require("node:path");

function integer(name, fallback, min, max) {
  const raw = process.env[name] ?? String(fallback);
  const value = Number(raw);
  if (
    !/^\d+$/.test(raw) ||
    !Number.isSafeInteger(value) ||
    value < min ||
    value > max
  ) {
    throw new Error(`${name} must be an integer between ${min} and ${max}`);
  }
  return value;
}

const grace = integer("PDF_TOOLKIT_SHUTDOWN_GRACE_SECONDS", 30, 0, 86400);

module.exports = {
  apps: [
    {
      name: "pdf-toolkit",
      cwd: path.resolve(process.env.PDF_TOOLKIT_CWD || __dirname),
      script: path.resolve(
        __dirname,
        process.env.PDF_TOOLKIT_BINARY || "dist/pdf-toolkit",
      ),
      interpreter: "none",
      exec_mode: "fork",
      instances: 1,
      args: [
        "serve",
        "--bind",
        process.env.PDF_TOOLKIT_BIND || "127.0.0.1:8787",
        "--max-workers",
        String(integer("PDF_TOOLKIT_MAX_WORKERS", 8, 1, 1024)),
        "--max-queue",
        String(integer("PDF_TOOLKIT_MAX_QUEUE", 32, 0, 4096)),
        "--worker-threads",
        String(integer("PDF_TOOLKIT_WORKER_THREADS", 1, 1, 1024)),
        "--timeout-seconds",
        String(integer("PDF_TOOLKIT_TIMEOUT_SECONDS", 3600, 1, 86400)),
        "--shutdown-grace-seconds",
        String(grace),
      ],
      autorestart: true,
      watch: false,
      min_uptime: "5s",
      max_restarts: 10,
      exp_backoff_restart_delay: 100,
      // Let the coordinator drain/reap children. A parent-lifetime pipe also
      // terminates workers if PM2 must SIGKILL the coordinator.
      treekill: false,
      kill_timeout: (grace + 10) * 1000,
      // This Rust process exposes /ready; it does not send Node IPC readiness.
      wait_ready: false,
      time: true,
    },
  ],
};
