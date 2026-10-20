import { spawn } from "node:child_process";
import { createRequire } from "node:module";
import path from "node:path";
import { parseLauncherArgs, resolveDeclaredPath } from "./runtime-selection.mjs";

try {
  const { options, viteArgs } = parseLauncherArgs(process.argv.slice(2));
  const env = { ...process.env };
  if (options.runtimeConfig !== undefined) env.KAGUYA_RUNTIME_CONFIG = resolveDeclaredPath(process.cwd(), options.runtimeConfig);
  if (options.profile !== undefined) env.KAGUYA_RUNTIME_PROFILE = options.profile;
  const require = createRequire(import.meta.url);
  const vite = path.join(path.dirname(require.resolve("vite/package.json")), "bin/vite.js");
  // Pass arguments directly to Node; deployment paths never pass through a shell.
  const child = spawn(process.execPath, [vite, ...viteArgs], { env, stdio: "inherit", windowsHide: true });
  child.on("error", (error) => { console.error(error.message); process.exitCode = 1; });
  child.on("exit", (code) => { process.exitCode = code ?? 1; });
  for (const signal of ["SIGINT", "SIGTERM"]) process.on(signal, () => child.kill(signal));
} catch (error) {
  console.error(error.message);
  process.exitCode = 1;
}
