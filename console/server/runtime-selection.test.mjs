import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import test from "node:test";
import {
  consoleLaunchPlan,
  parseLauncherArgs,
  parseRuntimeDocument,
  resolveDeclaredPath,
  resolveRuntimeSelection,
  resolveLaunchSession,
  sameResolvedPath,
  runtimeProcess,
} from "./runtime-selection.mjs";

const INSTANCE_ID = "11111111-2222-3333-4444-555555555555";
const SESSION_ID = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
const SESSION = { instance_id: INSTANCE_ID, session_id: SESSION_ID };
const MANIFEST_LINE = `manifest = "instances/${INSTANCE_ID}/instance.toml"`;

function fixture(t, content = runtimeFixture()) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "kaguya console selection "));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const config = path.join(root, "deployment with spaces", "runtime.toml");
  fs.mkdirSync(path.dirname(config), { recursive: true });
  fs.writeFileSync(config, content);
  fs.mkdirSync(path.join(root, "component settings"));
  fs.writeFileSync(path.join(root, "component settings", "gateway.toml"), "# selected Gateway settings\n");
  return { root, config, env: {} };
}

function runtimeFixture(extra = "") {
  return `profile = "app"
gateway_settings = "../component settings/gateway.toml"
[paths]
mode = "portable"
portable_root = "../portable data"
[instance]
${MANIFEST_LINE}
[profiles.app.processes.gateway]
command = "cargo run"
command_win32 = "cargo.exe run"
cwd = "../gateway app"
[profiles.app.processes.gateway.env]
KAGUYA_RUNTIME_CONFIG = "wrong-root"
KAGUYA_RUNTIME_PROFILE = "wrong-profile"
CUSTOM_SETTING = "kept"
[profiles.app.processes.voice_stack]
command = '.venv/bin/python main.py'
command_win32 = '.venv\\Scripts\\python.exe main.py'
cwd = "../voice app"
[profiles.app.processes.voice_stack.env]
KAGUYA_INSTANCE_ID = "wrong-instance"
KAGUYA_SESSION_ID = "wrong-session"
KAGUYA_RUNTIME_CONFIG = "wrong-voice-root"
KAGUYA_RUNTIME_PROFILE = "wrong-voice-profile"
[profiles.app.processes.voice_stack.bind]
talker_grpc = "0.0.0.0:50053"
listener_audio = "[::]:50056"
[profiles.app.processes.voice_stack.endpoints]
talker_grpc = "http://127.0.0.1:50053"
${extra}`;
}

test("CLI selection flags are consumed while Vite arguments remain intact", () => {
  assert.deepEqual(parseLauncherArgs(["--host", "0.0.0.0", "--runtime-config", "a path/root.toml", "--profile=local", "--port", "3456"]), {
    options: { runtimeConfig: "a path/root.toml", profile: "local" },
    viteArgs: ["--host", "0.0.0.0", "--port", "3456"],
  });
  for (const args of [["--profile"], ["--runtime-config="], ["--profile", "--host"], ["--profile=x", "--profile=y"]]) {
    assert.throws(() => parseLauncherArgs(args));
  }
});

test("declared paths reject Windows drive/root-relative ambiguity and traversal above root", () => {
  const windows = path.win32;
  for (const declared of ["C:runtime.toml", "\\runtime.toml", "/runtime.toml", "C:\\..\\runtime.toml"]) {
    assert.throws(() => resolveDeclaredPath("C:\\work\\console", declared, windows), /documented configuration base|filesystem root/);
  }
  assert.equal(resolveDeclaredPath("C:\\work\\console", "..\\config\\runtime.toml", windows), "C:\\work\\config\\runtime.toml");
  assert.equal(resolveDeclaredPath("C:\\work\\console", "D:\\deploy\\runtime.toml", windows), "D:\\deploy\\runtime.toml");
  assert.equal(resolveDeclaredPath("C:\\work", "\\\\server\\share\\runtime.toml", windows), "\\\\server\\share\\runtime.toml");
  assert.throws(() => resolveDeclaredPath("/work/console", "/../runtime.toml", path.posix), /filesystem root/);
  assert.equal(resolveDeclaredPath("/work/console", "../config/./runtime.toml", path.posix), "/work/config/runtime.toml");
});

test("CLI overrides environment, which overrides root profile", (t) => {
  const { root, config } = fixture(t, runtimeFixture('[profiles."chosen.profile".processes.gateway]\ncommand = "other"\ncwd = "."\n'));
  const selected = resolveRuntimeSelection({ runtimeConfig: config, profile: "chosen.profile" }, {
    KAGUYA_RUNTIME_CONFIG: "nonexistent.toml", KAGUYA_RUNTIME_PROFILE: "not-defined",
  }, root);
  assert.equal(selected.configPath, fs.realpathSync(config));
  assert.equal(selected.profile, "chosen.profile");
  assert.equal(runtimeProcess(selected, "gateway", "linux").command, "other");
  assert.equal(resolveRuntimeSelection({}, { KAGUYA_RUNTIME_CONFIG: config, KAGUYA_RUNTIME_PROFILE: "chosen.profile" }, root).profile, "chosen.profile");
  assert.equal(resolveRuntimeSelection({ runtimeConfig: config }, {}, root).profile, "app");
});

test("discovery uses launcher cwd and documented candidate order", (t) => {
  const { root } = fixture(t);
  const cwd = path.join(root, "console");
  fs.mkdirSync(path.join(cwd, "config"), { recursive: true });
  fs.mkdirSync(path.join(root, "config"));
  const candidates = [path.join(cwd, "config/kaguya.runtime.toml"), path.join(root, "config/kaguya.runtime.toml"), path.join(cwd, "kaguya.runtime.toml")];
  for (const candidate of candidates) fs.writeFileSync(candidate, runtimeFixture());
  fs.mkdirSync(path.join(cwd, "component settings"));
  fs.writeFileSync(path.join(cwd, "component settings/gateway.toml"), "# settings\n");
  for (const candidate of candidates) {
    assert.equal(resolveRuntimeSelection({}, {}, cwd).configPath, fs.realpathSync(candidate));
    fs.unlinkSync(candidate);
  }
  assert.throws(() => resolveRuntimeSelection({}, {}, cwd), /could not find/);
});

test("external root and process cwd resolve without depending on later child cwd", (t) => {
  const { root, config } = fixture(t);
  const unrelated = path.join(root, "another cwd");
  fs.mkdirSync(unrelated);
  const selected = resolveRuntimeSelection({ runtimeConfig: path.relative(unrelated, config) }, {}, unrelated);
  assert.equal(selected.gatewaySettings, path.resolve(path.dirname(config), "../component settings/gateway.toml"));
  assert.equal(selected.manifestPath, path.resolve(path.dirname(config), `../portable data/instances/${INSTANCE_ID}/instance.toml`));
  assert.equal(runtimeProcess(selected, "gateway").cwd, path.resolve(path.dirname(config), "../gateway app"));
  assert.equal(fs.existsSync(selected.portableRoot), false, "selection diagnostics must not create data directories");
});

test("app and direct children preserve one owner selection despite environment collisions", (t) => {
  const { config } = fixture(t);
  const selected = resolveRuntimeSelection({ runtimeConfig: config }, {});
  const inherited = { KAGUYA_RUNTIME_CONFIG: "wrong-parent", KAGUYA_RUNTIME_PROFILE: "wrong-parent-profile", SHARED: "kept", KAGUYA_INSTANCE_ID: "wrong-parent-instance", KAGUYA_SESSION_ID: "wrong-parent-session", kaguya_instance_id: "case-instance", kaguya_session_id: "case-session" };
  const plan = consoleLaunchPlan(selected, SESSION, inherited, "win32");
  const environments = [plan.supervisorEnv, ...plan.standalone.map((child) => child.env)];
  for (const env of environments) {
    assert.equal(env.KAGUYA_RUNTIME_CONFIG, selected.configPath);
    assert.equal(env.KAGUYA_RUNTIME_PROFILE, selected.profile);
    assert.equal(env.SHARED, "kept");
    assert.equal(env.KAGUYA_INSTANCE_ID, INSTANCE_ID);
    assert.equal(env.KAGUYA_SESSION_ID, SESSION_ID);
    assert.equal(env.kaguya_instance_id, undefined);
    assert.equal(env.kaguya_session_id, undefined);
  }
  assert.equal(plan.supervisorEnv.KAGUYA_SUPERVISOR_AUTOSTART, "false");
  assert.equal(plan.standalone[0].command, "cargo.exe run");
  assert.equal(plan.standalone[0].env.CUSTOM_SETTING, "kept");
  assert.equal(plan.standalone[1].env.KAGUYA_LISTENER_AUDIO_ADDR, "::");
  assert.equal(plan.standalone[1].env.KAGUYA_LISTENER_AUDIO_PORT, "50056");
  assert.equal(inherited.KAGUYA_RUNTIME_CONFIG, "wrong-parent", "composition must not mutate parent environment");
});

test("missing selections and required launch settings fail without fallback", (t) => {
  const { root, config } = fixture(t);
  assert.throws(() => resolveRuntimeSelection({ runtimeConfig: "missing.toml" }, { KAGUYA_RUNTIME_CONFIG: config }, root), /ENOENT/);
  assert.throws(() => resolveRuntimeSelection({ runtimeConfig: config, profile: "missing" }, {}), /not defined/);
  assert.throws(() => resolveRuntimeSelection({ runtimeConfig: config }, { KAGUYA_RUNTIME_PROFILE: "" }), /nonempty/);
  assert.throws(() => resolveRuntimeSelection({}, { KAGUYA_RUNTIME_CONFIG: "" }, root), /nonempty/);
  const selected = resolveRuntimeSelection({ runtimeConfig: config }, {});
  assert.throws(() => runtimeProcess(selected, "missing"), /not defined/);
  selected.processes.get("gateway").cwd = undefined;
  assert.throws(() => consoleLaunchPlan(selected, SESSION, {}, "linux"), /cwd/);
  selected.processes.get("gateway").cwd = ".";
  selected.processes.get("gateway").command = undefined;
  assert.throws(() => consoleLaunchPlan(selected, SESSION, {}, "linux"), /command/);
});

test("single-line TOML projection handles quoted names, literals, escapes and comments", () => {
  const document = parseRuntimeDocument(`profile = 'local.name' # comment
[profiles.'local.name'.processes.gateway]
command = "node \\"x#y\\" \\u0041 \\U0001F600"
cwd = 'C:\\path with spaces'
[profiles.'local.name'.processes.gateway.env]
'A.B' = '#literal'
`);
  const gateway = document.profiles.get("local.name").get("gateway");
  assert.equal(gateway.command, 'node "x#y" A 😀');
  assert.equal(gateway.cwd, "C:\\path with spaces");
  assert.equal(gateway.env["A.B"], "#literal");
});

test("malformed or unsupported relevant TOML fails explicitly", () => {
  for (const content of [
    'profile = 1',
    '[profile]\nname = "app"',
    'profile.name = "app"',
    '[gateway_settings]\npath = "gateway.toml"',
    'profile = "unterminated',
    'identity_manifest_path = "old.toml"',
    '[paths]\nmode = 1',
    '[instance]\nmanifest = ["a", "b"]',
    '[profiles.app.processes.gateway]\ncommand = "bad\\q"',
    '[profiles.app.processes.gateway]\nenv = { X = "hidden" }',
    '[profiles.app.processes.gateway]\ncommand = "x"\ncommand = "y"',
    '[profiles.app.processes.gateway]\ncommand = """multi-line"""',
  ]) assert.throws(() => parseRuntimeDocument(content), /runtime TOML line/);
});

test("portable mode is explicit and selecting config requires no existing manifest", (t) => {
  const { config } = fixture(t, runtimeFixture().replace(MANIFEST_LINE, ""));
  assert.equal(resolveRuntimeSelection({ runtimeConfig: config }, {}).manifestPath, undefined);
  fs.writeFileSync(config, runtimeFixture().replace('mode = "portable"', 'mode = "application_data"'));
  assert.throws(() => resolveRuntimeSelection({ runtimeConfig: config }, {}), /portable/);
});

test("manifest selector stays instance-local and cannot escape the portable root", (t) => {
  const { config } = fixture(t);
  for (const manifest of ["../instance.toml", "instances/../instance.toml", "instances/id/nested/instance.toml", "/instances/id/instance.toml", "C:/instances/id/instance.toml", "instances/id/owner.toml", ""]) {
    fs.writeFileSync(config, runtimeFixture().replace(MANIFEST_LINE, `manifest = ${JSON.stringify(manifest)}`));
    assert.throws(() => resolveRuntimeSelection({ runtimeConfig: config }, {}), /instance.manifest/);
  }
  fs.writeFileSync(config, runtimeFixture().replace(MANIFEST_LINE, `manifest = "./instances//${INSTANCE_ID}/./instance.toml"`));
  const selected = resolveRuntimeSelection({ runtimeConfig: config }, {});
  assert.equal(selected.manifestPath, path.join(selected.portableRoot, "instances", INSTANCE_ID, "instance.toml"));
  fs.writeFileSync(config, runtimeFixture().replace(MANIFEST_LINE, `manifest = "instances/id/../${INSTANCE_ID}/instance.toml"`));
  assert.throws(() => resolveRuntimeSelection({ runtimeConfig: config }, {}), /parent traversal/);
});

test("manifest directory IDs are canonical and non-nil even before a manifest exists", (t) => {
  const { config } = fixture(t);
  for (const id of ["not-a-uuid", "AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE", "00000000-0000-0000-0000-000000000000", "11111111222233334444555555555555"]) {
    fs.writeFileSync(config, runtimeFixture().replace(MANIFEST_LINE, `manifest = "instances/${id}/instance.toml"`));
    assert.throws(() => resolveRuntimeSelection({ runtimeConfig: config }, {}), /canonical non-nil UUID/);
  }
  // No version/variant restriction: this is a canonical UUID spelling policy.
  const id = "abcdefab-cdef-0123-4567-89abcdefabcd";
  fs.writeFileSync(config, runtimeFixture().replace(MANIFEST_LINE, `manifest = "instances/${id}/instance.toml"`));
  const selected = resolveRuntimeSelection({ runtimeConfig: config }, {});
  assert.equal(path.basename(path.dirname(selected.manifestPath)), id);
  assert.equal(fs.existsSync(selected.manifestPath), false);
});

test("portable root may be absent but cannot be an existing regular file", (t) => {
  const { config } = fixture(t);
  const selected = resolveRuntimeSelection({ runtimeConfig: config }, {});
  assert.equal(fs.existsSync(selected.portableRoot), false);
  fs.writeFileSync(selected.portableRoot, "not a directory");
  assert.throws(() => resolveRuntimeSelection({ runtimeConfig: config }, {}), /not a directory/);
});

test("declared bind/endpoint disagreement is retained as a launch error", (t) => {
  const { config } = fixture(t, runtimeFixture().replace('talker_grpc = "http://127.0.0.1:50053"', 'talker_grpc = "http://127.0.0.1:50054"'));
  assert.throws(() => consoleLaunchPlan(resolveRuntimeSelection({ runtimeConfig: config }, {}), SESSION, {}), /matching explicit ports/);
});

test("dev wrapper forwards ordinary Vite flags after consuming deployment flags", () => {
  const result = spawnSync(process.execPath, [fileURLToPath(new URL("./dev.mjs", import.meta.url)), "--runtime-config", "a root with spaces.toml", "--profile", "custom", "--version"], { encoding: "utf8" });
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /vite\//);
});


test("resolved deployment paths compare drive and UNC verbatim forms without losing the root", () => {
  const windows = path.win32;
  for (const [ordinary, verbatim] of [
    [String.raw`F:\deploy with spaces\runtime.toml`, String.raw`\\?\F:\deploy with spaces\runtime.toml`],
    [String.raw`\\server\share\deploy with spaces\runtime.toml`, String.raw`\\?\UNC\server\share\deploy with spaces\runtime.toml`],
  ]) {
    assert.equal(sameResolvedPath(ordinary, verbatim, windows), true);
    assert.equal(sameResolvedPath(verbatim, ordinary, windows), true);
    assert.equal(sameResolvedPath(verbatim, windows.join(windows.dirname(ordinary), "other.toml"), windows), false);
  }
  assert.equal(sameResolvedPath(String.raw`\\?\F:\deploy\runtime.toml`, String.raw`G:\deploy\runtime.toml`, windows), false);
  assert.equal(sameResolvedPath(String.raw`\\?\UNC\server\share\runtime.toml`, String.raw`\\other-server\share\runtime.toml`, windows), false);
  assert.equal(sameResolvedPath(String.raw`\\?\UNC\server\share\runtime.toml`, String.raw`\\server\other-share\runtime.toml`, windows), false);
  assert.equal(sameResolvedPath("/deploy/runtime.toml", "/deploy/runtime.toml", path.posix), true);
  assert.equal(sameResolvedPath("/deploy/runtime.toml", "/deploy/other.toml", path.posix), false);
  assert.equal(sameResolvedPath(undefined, "/deploy/runtime.toml", path.posix), false);
  assert.equal(sameResolvedPath("runtime.toml", "/deploy/runtime.toml", path.posix), false);
});

test("Gateway preflight supplies the exact launch pair through an explicit readonly command", (t) => {
  const { config } = fixture(t);
  const selected = resolveRuntimeSelection({ runtimeConfig: config }, {});
  let calls = 0;
  const execute = (command, args, options) => {
    calls += 1;
    assert.equal(command, "cargo");
    assert.deepEqual(args.slice(args.indexOf("--") + 1), ["show-config", "--runtime-config", selected.configPath, "--profile", "app"]);
    assert.equal(options.shell, false);
    assert.equal(options.windowsHide, true);
    assert.equal(options.env.KAGUYA_INSTANCE_ID, undefined);
    assert.equal(options.env.KAGUYA_SESSION_ID, undefined);
    return { status: 0, stdout: JSON.stringify({
      validation_mode: "startup", selection: { runtime_config: path.toNamespacedPath(selected.configPath), profile: "app" },
      instance: { instance_id: INSTANCE_ID, default_session_id: SESSION_ID },
    }) };
  };
  const session = resolveLaunchSession(selected, { KAGUYA_INSTANCE_ID: "stale", KAGUYA_SESSION_ID: "stale" }, execute);
  assert.deepEqual(session, SESSION);
  assert.equal(Object.isFrozen(session), true);
  assert.equal(calls, 1);
  assert.equal(fs.existsSync(selected.portableRoot), false, "Node bootstrap must not read or create instance data");
});

test("launch preflight fails closed before any application process launch", (t) => {
  const { config } = fixture(t);
  const selected = resolveRuntimeSelection({ runtimeConfig: config }, {});
  const valid = { validation_mode: "startup", selection: { runtime_config: selected.configPath, profile: "app" }, instance: { instance_id: INSTANCE_ID, default_session_id: SESSION_ID } };
  const reports = [
    { status: 1, stderr: "configuration failed" },
    { status: 0, stdout: "not-json" },
    { status: 0, stdout: JSON.stringify({ ...valid, validation_mode: "initialization" }) },
    { status: 0, stdout: JSON.stringify({ ...valid, selection: { ...valid.selection, runtime_config: "other" } }) },
    { status: 0, stdout: JSON.stringify({ ...valid, instance: null }) },
    { status: 0, stdout: JSON.stringify({ ...valid, instance: { instance_id: INSTANCE_ID, default_session_id: INSTANCE_ID } }) },
  ];
  for (const result of reports) assert.throws(() => resolveLaunchSession(selected, {}, () => result), /preflight/);
  assert.throws(() => consoleLaunchPlan(selected, undefined, {}), /validated instance\/session pair/);
});
