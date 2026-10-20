import fs from "node:fs";
import path from "node:path";

/** Consume deployment flags before Vite parses its own arguments. */
export function parseLauncherArgs(args) {
  const options = {};
  const viteArgs = [];
  for (let index = 0; index < args.length; index += 1) {
    const arg = args[index];
    if (arg === "--") {
      viteArgs.push(...args.slice(index + 1));
      break;
    }
    const match = /^(--runtime-config|--profile)(?:=(.*))?$/.exec(arg);
    if (!match) {
      viteArgs.push(arg);
      continue;
    }
    const key = match[1] === "--runtime-config" ? "runtimeConfig" : "profile";
    if (options[key] !== undefined) throw new Error(`${match[1]} was supplied twice`);
    const value = match[2] ?? args[++index];
    if (value === undefined || value.startsWith("--") || !value.trim()) {
      throw new Error(`${match[1]} requires a nonempty value`);
    }
    options[key] = value;
  }
  return { options, viteArgs };
}

/** Mirror Rust Path component resolution without drive-relative fallback or root clamping. */
export function resolveDeclaredPath(base, declared, paths = path) {
  requireNonempty(declared, "configured path");
  const root = paths.parse(declared).root;
  const windowsRootRelative = paths.sep === "\\" && (root === "\\" || root === "/");
  if ((!paths.isAbsolute(declared) && root) || windowsRootRelative) {
    throw new Error("path must be absolute or relative to its documented configuration base");
  }
  const joined = paths.isAbsolute(declared) ? declared : `${base}${paths.sep}${declared}`;
  const joinedRoot = paths.parse(joined).root;
  const components = pathComponents(joined.slice(joinedRoot.length), paths);
  const normalized = [];
  for (const component of components) {
    if (!component || component === ".") continue;
    if (component === "..") {
      if (!normalized.length) throw new Error("path escapes its filesystem root");
      normalized.pop();
    } else normalized.push(component);
  }
  return paths.join(joinedRoot, ...normalized);
}

function pathComponents(value, paths = path) {
  return value.split(paths.sep === "\\" ? /[\\/]+/ : /\/+/);
}

function inspectPath(file) {
  try { return fs.statSync(file); }
  catch (error) {
    if (error.code === "ENOENT") return undefined;
    throw error;
  }
}

/** Resolve once; children reload this exact absolute root and selected profile. */
export function resolveRuntimeSelection(options = {}, env = process.env, cwd = process.cwd()) {
  const explicitPath = options.runtimeConfig ?? env.KAGUYA_RUNTIME_CONFIG;
  let configPath;
  if (explicitPath !== undefined) {
    requireNonempty(explicitPath, "runtime configuration path");
    configPath = resolveDeclaredPath(cwd, explicitPath);
  } else {
    configPath = [
      "config/kaguya.runtime.toml",
      "../config/kaguya.runtime.toml",
      "kaguya.runtime.toml",
    ].map((candidate) => resolveDeclaredPath(cwd, candidate)).find((candidate) => inspectPath(candidate)?.isFile());
    if (!configPath) throw new Error("could not find config/kaguya.runtime.toml; select --runtime-config explicitly");
  }
  configPath = fs.realpathSync(configPath);
  const document = parseRuntimeDocument(fs.readFileSync(configPath, "utf8"));
  requireNonempty(document.gatewaySettings, "gateway_settings");
  if (document.pathMode !== "portable") throw new Error("paths.mode must explicitly select 'portable'");
  requireNonempty(document.portableRoot, "paths.portable_root");
  let manifestParts;
  if (document.manifest !== undefined) {
    requireNonempty(document.manifest, "instance.manifest");
    manifestParts = pathComponents(document.manifest);
    if (path.isAbsolute(document.manifest) || path.parse(document.manifest).root || manifestParts.includes("..")) {
      throw new Error("instance.manifest must be relative to portable_root without parent traversal");
    }
    manifestParts = manifestParts.filter((component) => component && component !== ".");
    if (
      manifestParts.length !== 3 || manifestParts[0] !== "instances" ||
      manifestParts[2] !== "instance.toml") {
      throw new Error("instance.manifest must be relative to portable_root as instances/<id>/instance.toml");
    }
    if (!/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(manifestParts[1]) ||
      manifestParts[1] === "00000000-0000-0000-0000-000000000000") {
      throw new Error("instance.manifest directory must be a lowercase canonical non-nil UUID");
    }
  }
  const profile = options.profile ?? env.KAGUYA_RUNTIME_PROFILE ?? document.profile;
  if (profile !== undefined) requireNonempty(profile, "runtime profile");
  let processes;
  if (profile !== undefined) {
    processes = document.profiles.get(profile);
    if (!processes) throw new Error(`runtime profile '${profile}' is not defined`);
  } else {
    if (document.profiles.size > 0) throw new Error("runtime profile must be selected when named profiles are defined");
    processes = document.processes;
  }
  const base = path.dirname(configPath);
  const gatewaySettings = resolveDeclaredPath(base, document.gatewaySettings);
  if (!inspectPath(gatewaySettings)?.isFile()) {
    throw new Error(`selected Gateway settings file is missing: ${gatewaySettings}`);
  }
  const portableRoot = resolveDeclaredPath(base, document.portableRoot);
  const portableState = inspectPath(portableRoot);
  if (portableState && !portableState.isDirectory()) throw new Error("portable root exists but is not a directory");
  return {
    configPath, profile, processes, portableRoot,
    gatewaySettings,
    manifestPath: manifestParts === undefined ? undefined : path.join(portableRoot, ...manifestParts),
  };
}

/** Authoritative selectors must win over inherited and per-process values. */
export function selectionEnvironment(selection, ...layers) {
  const env = Object.assign({}, ...layers);
  env.KAGUYA_RUNTIME_CONFIG = selection.configPath;
  if (selection.profile === undefined) delete env.KAGUYA_RUNTIME_PROFILE;
  else env.KAGUYA_RUNTIME_PROFILE = selection.profile;
  return env;
}

export function runtimeProcess(selection, name, platform = process.platform) {
  const config = selection.processes.get(name);
  if (!config) throw new Error(`runtime process '${name}' is not defined in selected profile`);
  const command = platform === "win32" ? config.command_win32 ?? config.command : config.command;
  requireNonempty(command, `runtime process '${name}' command for ${platform}`);
  requireNonempty(config.cwd, `runtime process '${name}' cwd`);
  if (config.enabled === false) throw new Error(`runtime process '${name}' is disabled`);
  validateBindEndpointPorts(config, `${selection.profile ?? "processes"}.${name}`);
  return {
    ...config,
    command,
    cwd: resolveDeclaredPath(path.dirname(selection.configPath), config.cwd),
  };
}

/** App and direct launch controls share one deployment; they differ in who starts children. */
export function consoleLaunchPlan(selection, inheritedEnv = process.env, platform = process.platform) {
  const gateway = runtimeProcess(selection, "gateway", platform);
  const voice = runtimeProcess(selection, "voice_stack", platform);
  const voiceEnv = {};
  if (voice.bind.talker_grpc) voiceEnv.KAGUYA_TALKER_LISTEN_ADDR = voice.bind.talker_grpc;
  if (voice.bind.listener_grpc) voiceEnv.KAGUYA_LISTENER_GRPC_ADDR = voice.bind.listener_grpc;
  if (voice.bind.listener_audio) {
    const index = voice.bind.listener_audio.lastIndexOf(":");
    voiceEnv.KAGUYA_LISTENER_AUDIO_ADDR = voice.bind.listener_audio.slice(0, index).replace(/^\[|\]$/g, "");
    voiceEnv.KAGUYA_LISTENER_AUDIO_PORT = voice.bind.listener_audio.slice(index + 1);
  }
  return {
    supervisorEnv: selectionEnvironment(selection, inheritedEnv, {
      KAGUYA_SUPERVISOR_AUTOSTART: "false",
      RUST_LOG: inheritedEnv.RUST_LOG ?? "kaguya_supervisor=info",
    }),
    standalone: [
      { name: "gateway_standalone", label: gateway.label ?? "Gateway", command: gateway.command, cwd: gateway.cwd,
        env: selectionEnvironment(selection, inheritedEnv, gateway.env) },
      { name: "voice_stack_standalone", label: voice.label ?? "Voice Stack", command: voice.command, cwd: voice.cwd,
        env: selectionEnvironment(selection, inheritedEnv, voice.env, voiceEnv) },
    ],
  };
}

function requireNonempty(value, label) {
  if (typeof value !== "string" || !value.trim()) throw new Error(`${label} must be a nonempty string`);
}

// This remains a deliberately narrow Console projection, pending R1d reader
// consolidation. Unsupported relevant syntax fails rather than selecting defaults.
// Tables/keys and single-line basic/literal strings follow TOML quoting rules.
export function parseRuntimeDocument(content) {
  const document = { profile: undefined, processes: new Map(), profiles: new Map() };
  const seen = new Set();
  const tables = new Set();
  let section = [];
  for (const [index, raw] of content.split(/\r?\n/).entries()) {
    const label = `runtime TOML line ${index + 1}`;
    const line = stripComment(raw, label).trim();
    if (!line) continue;
    if (line.startsWith("[")) {
      if (line.startsWith("[[") || !line.endsWith("]")) {
        throw new Error(`${label}: Console does not support this table syntax`);
      }
      section = parseKeyPath(line.slice(1, -1), label);
      validateRootKey(section[0], label);
      if (["profile", "gateway_settings"].includes(section[0])) {
        throw new Error(`${label}: ${section[0]} must be a scalar string, not a table`);
      }
      unique(tables, section, label);
      if (section[0] === "profiles" && section[1] && !document.profiles.has(section[1])) {
        document.profiles.set(section[1], new Map());
      }
      continue;
    }
    const separator = unquotedIndex(line, "=");
    if (separator < 0) throw new Error(`${label}: expected a single-line key/value assignment`);
    const keys = parseKeyPath(line.slice(0, separator), label);
    const full = [...section, ...keys];
    validateRootKey(full[0], label);
    if (["profile", "gateway_settings"].includes(full[0]) && full.length !== 1) {
      throw new Error(`${label}: ${full[0]} must be a scalar string, not a table`);
    }
    const rawValue = line.slice(separator + 1).trim();
    const rootField = {
      '["profile"]': "profile",
      '["gateway_settings"]': "gatewaySettings",
      '["paths","mode"]': "pathMode",
      '["paths","portable_root"]': "portableRoot",
      '["instance","manifest"]': "manifest",
    }[JSON.stringify(full)];
    if (rootField) {
      unique(seen, full, label);
      document[rootField] = parseString(rawValue, label);
      continue;
    }
    if (full[0] === "paths" || full[0] === "instance") {
      throw new Error(`${label}: unsupported deployment selection field ${full.join(".")}`);
    }
    let processMap;
    let processName;
    let field;
    if (full[0] === "profiles" && full[2] === "processes") {
      if (!document.profiles.has(full[1])) document.profiles.set(full[1], new Map());
      processMap = document.profiles.get(full[1]);
      processName = full[3];
      field = full.slice(4);
    } else if (full[0] === "processes") {
      processMap = document.processes;
      processName = full[1];
      field = full.slice(2);
    } else continue;
    if (!processName || !field.length) throw new Error(`${label}: process settings require explicit tables or dotted keys`);
    if (!processMap.has(processName)) processMap.set(processName, { env: {}, bind: {}, endpoints: {} });
    const config = processMap.get(processName);
    const [name, childKey] = field;
    if (["env", "bind", "endpoints"].includes(name)) {
      if (field.length !== 2) throw new Error(`${label}: ${name} requires individual string entries`);
      unique(seen, full, label);
      config[name][childKey] = parseString(rawValue, label);
    } else if (field.length === 1 && ["label", "command", "command_win32", "cwd"].includes(name)) {
      unique(seen, full, label);
      config[name] = parseString(rawValue, label);
    } else if (field.length === 1 && name === "enabled") {
      unique(seen, full, label);
      if (rawValue !== "true" && rawValue !== "false") throw new Error(`${label}: enabled must be a boolean`);
      config.enabled = rawValue === "true";
    }
  }
  return document;
}

function validateRootKey(key, label) {
  if (!["profile", "gateway_settings", "paths", "instance", "supervisor_addr", "sandbox", "processes", "profiles"].includes(key)) {
    throw new Error(`${label}: unsupported runtime root field '${key}'`);
  }
}

function unique(seen, keys, label) {
  const key = JSON.stringify(keys);
  if (seen.has(key)) throw new Error(`${label}: duplicate configuration key`);
  seen.add(key);
}

function stripComment(line, label) {
  let quote;
  let escaped = false;
  for (let index = 0; index < line.length; index += 1) {
    const char = line[index];
    if (escaped) { escaped = false; continue; }
    if (quote === '"' && char === "\\") { escaped = true; continue; }
    if (char === quote) quote = undefined;
    else if (!quote && (char === '"' || char === "'")) {
      if (line.slice(index, index + 3) === char.repeat(3)) {
        throw new Error(`${label}: Console requires single-line configuration strings`);
      }
      quote = char;
    } else if (!quote && char === "#") return line.slice(0, index);
  }
  if (quote) throw new Error(`${label}: unterminated string`);
  return line;
}

function unquotedIndex(value, target) {
  let quote;
  let escaped = false;
  for (let index = 0; index < value.length; index += 1) {
    const char = value[index];
    if (escaped) { escaped = false; continue; }
    if (quote === '"' && char === "\\") { escaped = true; continue; }
    if (char === quote) quote = undefined;
    else if (!quote && (char === '"' || char === "'")) quote = char;
    else if (!quote && char === target) return index;
  }
  return -1;
}

function parseKeyPath(value, label) {
  const result = [];
  let rest = value.trim();
  while (rest) {
    const separator = unquotedIndex(rest, ".");
    const key = (separator < 0 ? rest : rest.slice(0, separator)).trim();
    if (/^[A-Za-z0-9_-]+$/.test(key)) result.push(key);
    else result.push(parseString(key, label));
    if (separator < 0) return result;
    rest = rest.slice(separator + 1).trim();
    if (!rest) break;
  }
  throw new Error(`${label}: invalid table/key path`);
}

function parseString(value, label) {
  if (/[\x00-\x08\x0b-\x1f\x7f]/.test(value)) throw new Error(`${label}: invalid control character in string`);
  if (value.startsWith("'") && value.endsWith("'") && !value.slice(1, -1).includes("'")) {
    return value.slice(1, -1);
  }
  if (!/^"(?:[^"\\\x00-\x08\x0b-\x1f]|\\(?:[btnfr"\\]|u[0-9a-fA-F]{4}|U[0-9a-fA-F]{8}))*"$/.test(value)) {
    throw new Error(`${label}: expected a supported single-line TOML string`);
  }
  return value.slice(1, -1).replace(/\\([btnfr"\\]|u[0-9a-fA-F]{4}|U[0-9a-fA-F]{8})/g, (_, escaped) => {
    if (escaped[0] === "u" || escaped[0] === "U") {
      const point = Number.parseInt(escaped.slice(1), 16);
      if (point > 0x10ffff || (point >= 0xd800 && point <= 0xdfff)) throw new Error(`${label}: invalid Unicode scalar`);
      return String.fromCodePoint(point);
    }
    return { b: "\b", t: "\t", n: "\n", f: "\f", r: "\r", '"': '"', "\\": "\\" }[escaped];
  });
}

function validateBindEndpointPorts(config, label) {
  for (const [name, bind] of Object.entries(config.bind)) {
    const endpoint = config.endpoints[name];
    if (!endpoint) continue;
    const bindPort = portOf(bind);
    const endpointPort = portOf(endpoint);
    if (!bindPort || !endpointPort || bindPort !== endpointPort) {
      throw new Error(`${label}.${name} bind/endpoints require matching explicit ports: bind=${bind}, endpoint=${endpoint}`);
    }
  }
}

function portOf(value) {
  const authority = value.replace(/^[^:]+:\/\//, "").split("/", 1)[0];
  const match = /(?:\]|[^:]):([0-9]+)$/.exec(authority);
  return match?.[1];
}
