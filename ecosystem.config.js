const path = require("node:path");
const { execFileSync } = require("node:child_process");

const cwd = path.resolve(process.env.PDF_TOOLKIT_CWD || __dirname);
const script = path.resolve(
  __dirname,
  process.env.PDF_TOOLKIT_BINARY || "dist/pdf-toolkit",
);
const args = ["serve"];
const overrides = {
  PDF_TOOLKIT_BIND: "--bind",
  PDF_TOOLKIT_MAX_WORKERS: "--max-workers",
  PDF_TOOLKIT_MAX_QUEUE: "--max-queue",
  PDF_TOOLKIT_WORKER_THREADS: "--worker-threads",
  PDF_TOOLKIT_TIMEOUT_SECONDS: "--timeout-seconds",
  PDF_TOOLKIT_SHUTDOWN_GRACE_SECONDS: "--shutdown-grace-seconds",
  PDF_TOOLKIT_WRITE_TIMEOUT_SECONDS: "--write-timeout-seconds",
  PDF_TOOLKIT_MAX_CONNECTIONS: "--max-connections",
  PDF_TOOLKIT_MEMORY_BUDGET_MIB: "--memory-budget-mib",
  PDF_TOOLKIT_WORKER_MEMORY_MIB: "--worker-memory-mib",
  PDF_TOOLKIT_MAX_RENDER_PIXELS: "--max-render-pixels",
};
for (const [name, flag] of Object.entries(overrides)) {
  if (process.env[name] !== undefined) args.push(flag, process.env[name]);
}
// Rust supplies defaults and validates every override. No listener is started.
const config = JSON.parse(
  execFileSync(script, [...args, "--print-config"], {
    cwd,
    encoding: "utf8",
    timeout: 10000,
  }),
);

module.exports = {
  apps: [
    {
      name: "pdf-toolkit",
      cwd,
      script,
      interpreter: "none",
      exec_mode: "fork",
      instances: 1,
      args,
      autorestart: true,
      watch: false,
      min_uptime: "5s",
      max_restarts: 10,
      exp_backoff_restart_delay: 100,
      // The coordinator drains children; their lifetime pipes handle forced exit.
      treekill: false,
      kill_timeout: (config.shutdown_grace_seconds + 10) * 1000,
      // Native process readiness is available at /ready rather than Node IPC.
      wait_ready: false,
      time: true,
    },
  ],
};
