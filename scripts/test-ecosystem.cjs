const assert = require("node:assert/strict");
const { execFileSync } = require("node:child_process");
const configPath = require.resolve("../ecosystem.config.js");
const options = Object.keys(process.env).filter((name) =>
  name.startsWith("PDF_TOOLKIT_"),
);
const saved = Object.fromEntries(
  options.map((name) => [name, process.env[name]]),
);
function load(overrides = {}) {
  for (const name of Object.keys(process.env)) {
    if (name.startsWith("PDF_TOOLKIT_") && name !== "PDF_TOOLKIT_BINARY")
      delete process.env[name];
  }
  Object.assign(process.env, overrides);
  delete require.cache[configPath];
  return require(configPath).apps[0];
}
try {
  const defaults = load();
  const resolved = JSON.parse(
    execFileSync(defaults.script, ["serve", "--print-config"], {
      encoding: "utf8",
    }),
  );
  assert.deepEqual(defaults.args, ["serve"]);
  assert.equal(resolved.max_workers, 8);
  assert.equal(
    defaults.kill_timeout,
    (resolved.shutdown_grace_seconds + 10) * 1000,
  );
  const updated = load({
    PDF_TOOLKIT_MAX_WORKERS: "16",
    PDF_TOOLKIT_SHUTDOWN_GRACE_SECONDS: "45",
  });
  assert.equal(updated.kill_timeout, 55000);
  assert.equal(updated.args[updated.args.indexOf("--max-workers") + 1], "16");
  assert.throws(() => load({ PDF_TOOLKIT_MAX_WORKERS: "0" }));
  assert.throws(() => load({ PDF_TOOLKIT_MEMORY_BUDGET_MIB: "1" }));
  console.log("PM2 configuration defaults, overrides and validation passed");
} finally {
  for (const name of Object.keys(process.env)) {
    if (name.startsWith("PDF_TOOLKIT_")) delete process.env[name];
  }
  Object.assign(process.env, saved);
}
