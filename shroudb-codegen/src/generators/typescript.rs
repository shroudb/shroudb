//! TypeScript client generator.
//!
//! Produces a self-contained npm package with:
//! - `src/connection.ts` — internal protocol codec (not exported)
//! - `src/errors.ts`     — ShrouDB error classes
//! - `src/types.ts`      — TypeScript interfaces for responses
//! - `src/client.ts`     — public `ShrouDBClient`, URI-first, with pool
//! - `src/index.ts`      — re-exports only public ShrouDB API
//! - `package.json`      — npm metadata
//! - `tsconfig.json`     — TypeScript config

use crate::generator::{GeneratedFile, Generator, Naming};
use crate::spec::{CommandDef, ProtocolSpec};
use heck::ToLowerCamelCase;
use std::fmt::Write;

pub struct TypeScriptGenerator;

impl Generator for TypeScriptGenerator {
    fn language(&self) -> &'static str {
        "TypeScript"
    }

    fn generate(&self, spec: &ProtocolSpec) -> Vec<GeneratedFile> {
        let n = Naming::from_spec(spec);
        vec![
            gen_connection(spec, &n),
            gen_pool(spec, &n),
            gen_errors(spec, &n),
            gen_types(spec, &n),
            gen_client(spec, &n),
            gen_index(spec, &n),
            gen_package_json(spec, &n),
            gen_tsconfig(&n),
            gen_readme(spec, &n),
        ]
    }
}

fn ts_type(spec: &ProtocolSpec, type_name: &str) -> &'static str {
    match spec.types.get(type_name) {
        Some(t) => match t.typescript_type.as_str() {
            "string" => "string",
            "number" => "number",
            "boolean" => "boolean",
            "Record<string, unknown>" => "Record<string, unknown>",
            _ => "unknown",
        },
        None => "unknown",
    }
}

// ─── connection.ts (internal) ────────────────────────────────────────────────

fn gen_connection(_spec: &ProtocolSpec, n: &Naming) -> GeneratedFile {
    GeneratedFile {
        path: "src/connection.ts".into(),
        content: format!(
            r#"/**
 * Internal {pascal} protocol codec.
 *
 * This module is an implementation detail of the {pascal} client library.
 * Do not import directly — use `{pascal}Client` instead.
 *
 * Auto-generated from {raw} protocol spec. Do not edit.
 */

import {{ createConnection, Socket }} from "net";
import {{ connect as tlsConnect, TLSSocket }} from "tls";
import {{ {pascal}Error }} from "./errors";

export const DEFAULT_PORT = {port};

type WireValue = string | number | null | WireValue[] | Record<string, WireValue>;

/** @internal */
export class Connection {{
  private buffer = "";
  private resolveQueue: Array<(value: WireValue) => void> = [];
  private rejectQueue: Array<(reason: Error) => void> = [];

  private constructor(private socket: Socket | TLSSocket) {{
    socket.setEncoding("utf-8");
    socket.on("data", (chunk: string) => {{
      this.buffer += chunk;
      this.drain();
    }});
    socket.on("error", (err) => {{
      for (const reject of this.rejectQueue) {{
        reject(err);
      }}
      this.resolveQueue = [];
      this.rejectQueue = [];
    }});
  }}

  /** @internal */
  static async open(host: string, port: number, tls: boolean): Promise<Connection> {{
    return new Promise((resolve, reject) => {{
      if (tls) {{
        const socket = tlsConnect({{ host, port }}, () => resolve(new Connection(socket)));
        socket.on("error", reject);
      }} else {{
        const socket = createConnection({{ host, port }}, () => resolve(new Connection(socket)));
        socket.on("error", reject);
      }}
    }});
  }}

  /** @internal */
  async execute(...args: string[]): Promise<WireValue> {{
    let cmd = `*${{args.length}}\r\n`;
    for (const arg of args) {{
      const bytes = Buffer.byteLength(arg, "utf-8");
      cmd += `$${{bytes}}\r\n${{arg}}\r\n`;
    }}
    this.socket.write(cmd);

    return new Promise<WireValue>((resolve, reject) => {{
      this.resolveQueue.push(resolve);
      this.rejectQueue.push(reject);
      this.drain();
    }});
  }}

  /** @internal */
  close(): void {{
    this.socket.end();
  }}

  private drain(): void {{
    while (this.resolveQueue.length > 0) {{
      const result = this.tryParse();
      if (result === undefined) break;
      const resolve = this.resolveQueue.shift()!;
      this.rejectQueue.shift();
      resolve(result.value);
    }}
  }}

  private tryParse(): {{ value: WireValue; rest: string }} | undefined {{
    return this.parseFrame(this.buffer);
  }}

  private parseFrame(
    buf: string,
  ): {{ value: WireValue; rest: string }} | undefined {{
    const nlIdx = buf.indexOf("\r\n");
    if (nlIdx < 0) return undefined;

    const tag = buf[0];
    const payload = buf.substring(1, nlIdx);
    const after = buf.substring(nlIdx + 2);

    switch (tag) {{
      case "+":
        this.buffer = after;
        return {{ value: payload, rest: after }};
      case "-": {{
        const spaceIdx = payload.indexOf(" ");
        const code = spaceIdx >= 0 ? payload.substring(0, spaceIdx) : payload;
        const detail = spaceIdx >= 0 ? payload.substring(spaceIdx + 1) : "";
        this.buffer = after;
        const reject = this.rejectQueue.shift();
        this.resolveQueue.shift();
        if (reject) reject({pascal}Error._fromServer(code, detail));
        return undefined;
      }}
      case ":":
        this.buffer = after;
        return {{ value: parseInt(payload, 10), rest: after }};
      case "$": {{
        const len = parseInt(payload, 10);
        if (len < 0) {{
          this.buffer = after;
          return {{ value: null, rest: after }};
        }}
        if (after.length < len + 2) return undefined;
        const data = after.substring(0, len);
        const rest = after.substring(len + 2);
        this.buffer = rest;
        return {{ value: data, rest }};
      }}
      case "*": {{
        const count = parseInt(payload, 10);
        let remaining = after;
        const arr: WireValue[] = [];
        for (let i = 0; i < count; i++) {{
          const sub = this.parseFrameFrom(remaining);
          if (!sub) return undefined;
          arr.push(sub.value);
          remaining = sub.rest;
        }}
        this.buffer = remaining;
        return {{ value: arr, rest: remaining }};
      }}
      case "%": {{
        const count = parseInt(payload, 10);
        let remaining = after;
        const map: Record<string, WireValue> = {{}};
        for (let i = 0; i < count; i++) {{
          const keyFrame = this.parseFrameFrom(remaining);
          if (!keyFrame) return undefined;
          remaining = keyFrame.rest;
          const valFrame = this.parseFrameFrom(remaining);
          if (!valFrame) return undefined;
          remaining = valFrame.rest;
          map[String(keyFrame.value)] = valFrame.value;
        }}
        this.buffer = remaining;
        return {{ value: map, rest: remaining }};
      }}
      case "_":
        this.buffer = after;
        return {{ value: null, rest: after }};
      default:
        throw new {pascal}Error("INTERNAL", `Unknown response type: ${{tag}}`);
    }}
  }}

  private parseFrameFrom(
    buf: string,
  ): {{ value: WireValue; rest: string }} | undefined {{
    const saved = this.buffer;
    this.buffer = buf;
    const result = this.tryParse();
    if (!result) {{
      this.buffer = saved;
      return undefined;
    }}
    const rest = this.buffer;
    this.buffer = saved;
    return {{ value: result.value, rest }};
  }}
}}
"#,
            port = n.default_port,
            pascal = n.pascal,
            raw = n.raw,
        ),
    }
}

// ─── pool.ts (internal) ──────────────────────────────────────────────────────

fn gen_pool(_spec: &ProtocolSpec, n: &Naming) -> GeneratedFile {
    GeneratedFile {
        path: "src/pool.ts".into(),
        content: format!(
            r#"/**
 * Internal connection pool.
 *
 * This module is an implementation detail of the {pascal} client library.
 * Do not import directly — use `{pascal}Client` instead.
 *
 * Auto-generated from {raw} protocol spec. Do not edit.
 */

import {{ Connection }} from "./connection";

/** @internal */
export interface PoolOptions {{
  /** Maximum idle connections to keep (default: 4). */
  maxIdle?: number;
  /** Maximum total connections, 0 = unlimited (default: 0). */
  maxOpen?: number;
}}

/** @internal */
export class Pool {{
  private idle: Connection[] = [];
  private open = 0;
  private readonly maxIdle: number;
  private readonly maxOpen: number;

  constructor(
    private readonly host: string,
    private readonly port: number,
    private readonly tls: boolean,
    private readonly auth: string | undefined,
    opts: PoolOptions = {{}},
  ) {{
    this.maxIdle = opts.maxIdle ?? 4;
    this.maxOpen = opts.maxOpen ?? 0;
  }}

  async get(): Promise<Connection> {{
    if (this.idle.length > 0) {{
      return this.idle.pop()!;
    }}

    this.open++;
    try {{
      const conn = await Connection.open(this.host, this.port, this.tls);
      if (this.auth) {{
        await conn.execute("AUTH", this.auth);
      }}
      return conn;
    }} catch (e) {{
      this.open--;
      throw e;
    }}
  }}

  put(conn: Connection): void {{
    if (this.idle.length < this.maxIdle) {{
      this.idle.push(conn);
    }} else {{
      conn.close();
      this.open--;
    }}
  }}

  close(): void {{
    for (const conn of this.idle) {{
      conn.close();
    }}
    this.idle = [];
    this.open = 0;
  }}
}}
"#,
            pascal = n.pascal,
            raw = n.raw,
        ),
    }
}

// ─── errors.ts ───────────────────────────────────────────────────────────────

fn gen_errors(spec: &ProtocolSpec, n: &Naming) -> GeneratedFile {
    let mut out = format!(
        r#"/**
 * {pascal} error types.
 *
 * Auto-generated from {raw} protocol spec. Do not edit.
 */

export class {pascal}Error extends Error {{
  constructor(
    public readonly code: string,
    public readonly detail: string,
  ) {{
    super(`[${{code}}] ${{detail}}`);
    this.name = "{pascal}Error";
  }}

  /** @internal Construct the appropriate error subclass from a server error. */
  static _fromServer(code: string, detail: string): {pascal}Error {{
    const Factory = ERROR_MAP[code] ?? {pascal}Error;
    return new Factory(code, detail);
  }}
}}

"#,
        pascal = n.pascal,
        raw = n.raw,
    );

    for (code, def) in &spec.error_codes {
        let class_name = code_to_ts_class(code);
        writeln!(out, "/** {} */", def.description).unwrap();
        writeln!(
            out,
            "export class {class_name} extends {pascal}Error {{\n  constructor(code: string, detail: string) {{\n    super(code, detail);\n    this.name = \"{class_name}\";\n  }}\n}}\n",
            pascal = n.pascal,
        ).unwrap();
    }

    out.push_str(&format!(
        "const ERROR_MAP: Record<string, typeof {pascal}Error> = {{\n",
        pascal = n.pascal,
    ));
    for code in spec.error_codes.keys() {
        let class_name = code_to_ts_class(code);
        writeln!(out, "  \"{code}\": {class_name},").unwrap();
    }
    out.push_str("};\n");

    GeneratedFile {
        path: "src/errors.ts".into(),
        content: out,
    }
}

fn code_to_ts_class(code: &str) -> String {
    let parts: Vec<&str> = code.split('_').collect();
    let mut name = String::new();
    for part in parts {
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            name.push(first.to_ascii_uppercase());
            for c in chars {
                name.push(c.to_ascii_lowercase());
            }
        }
    }
    name.push_str("Error");
    name
}

// ─── types.ts ────────────────────────────────────────────────────────────────

fn gen_types(spec: &ProtocolSpec, n: &Naming) -> GeneratedFile {
    let mut out = format!(
        r#"/**
 * {pascal} response types.
 *
 * Auto-generated from {raw} protocol spec. Do not edit.
 */

"#,
        pascal = n.pascal,
        raw = n.raw,
    );

    for (cmd_name, cmd) in &spec.commands {
        if cmd.response.is_empty() || cmd.simple_response {
            continue;
        }
        let iface_name = format!("{}Response", to_pascal(cmd_name));
        writeln!(out, "/** Response from {} command. */", cmd.verb).unwrap();
        writeln!(out, "export interface {iface_name} {{").unwrap();
        for f in &cmd.response {
            let field_name = f.name.to_lower_camel_case();
            let field_type = ts_type(spec, &f.field_type);
            if f.optional {
                writeln!(out, "  {field_name}?: {field_type};").unwrap();
            } else {
                writeln!(out, "  {field_name}: {field_type};").unwrap();
            }
        }
        writeln!(out, "}}\n").unwrap();
    }

    GeneratedFile {
        path: "src/types.ts".into(),
        content: out,
    }
}

// ─── client.ts ───────────────────────────────────────────────────────────────

fn gen_client(spec: &ProtocolSpec, n: &Naming) -> GeneratedFile {
    let scheme = n
        .uri_schemes
        .first()
        .map(|s| s.as_str())
        .unwrap_or(&n.snake);
    let scheme_tls = format!("{}+tls", scheme);
    let mut out = format!(
        r#"/**
 * {pascal} client.
 *
 * Auto-generated from {raw} protocol spec. Do not edit.
 */

import {{ DEFAULT_PORT }} from "./connection";
import {{ Pool, type PoolOptions }} from "./pool";
import {{ {pascal}Error }} from "./errors";
"#,
        pascal = n.pascal,
        raw = n.raw,
    );

    // Import types
    let mut type_imports = Vec::new();
    for (cmd_name, cmd) in &spec.commands {
        if !cmd.response.is_empty() && !cmd.simple_response {
            type_imports.push(format!("{}Response", to_pascal(cmd_name)));
        }
    }
    if !type_imports.is_empty() {
        writeln!(
            out,
            "import type {{ {} }} from \"./types\";",
            type_imports.join(", ")
        )
        .unwrap();
    }

    out.push_str(&format!(
        r#"

/**
 * Parse a {pascal} connection URI.
 *
 * Supported formats:
 * - `{scheme}://localhost`
 * - `{scheme}://localhost:{port}`
 * - `{scheme_tls}://prod.example.com`
 * - `{scheme}://mytoken@localhost:{port}`
 * - `{scheme}://mytoken@localhost/sessions`
 * - `{scheme_tls}://tok@host:{port}/keys`
 */
function parseUri(uri: string): {{
  host: string;
  port: number;
  tls: boolean;
  authToken?: string;
  keyspace?: string;
}} {{
  let tls = false;
  let rest: string;
  if (uri.startsWith("{scheme_tls}://")) {{
    tls = true;
    rest = uri.slice("{scheme_tls}://".length);
  }} else if (uri.startsWith("{scheme}://")) {{
    rest = uri.slice("{scheme}://".length);
  }} else {{
    throw new {pascal}Error("BADARG", `Invalid {pascal} URI: ${{uri}}  (expected {scheme}:// or {scheme_tls}://)`);
  }}

  let authToken: string | undefined;
  const atIdx = rest.indexOf("@");
  if (atIdx >= 0) {{
    authToken = rest.substring(0, atIdx);
    rest = rest.substring(atIdx + 1);
  }}

  let keyspace: string | undefined;
  const slashIdx = rest.indexOf("/");
  if (slashIdx >= 0) {{
    const ks = rest.substring(slashIdx + 1);
    keyspace = ks || undefined;
    rest = rest.substring(0, slashIdx);
  }}

  let host = rest;
  let port = {port};
  const colonIdx = host.lastIndexOf(":");
  if (colonIdx >= 0) {{
    const parsed = parseInt(host.substring(colonIdx + 1), 10);
    if (!isNaN(parsed)) {{
      port = parsed;
      host = host.substring(0, colonIdx);
    }}
  }}

  return {{ host, port, tls, authToken, keyspace }};
}}

/**
 * Async client for the {pascal} {description}.
 *
 * Connect using a {pascal} URI:
 *
 * ```ts
 * const client = await {pascal}Client.connect("{scheme}://localhost");
 * const result = await client.issue("my-keyspace", {{ ttlSecs: 3600 }});
 * console.log(result.credentialId, result.token);
 * client.close();
 *
 * // With TLS and auth:
 * const client = await {pascal}Client.connect("{scheme_tls}://mytoken@prod.example.com/keys");
 * ```
 */
export class {pascal}Client {{
  /** @internal */
  private pool: Pool;

  private constructor(pool: Pool) {{
    this.pool = pool;
  }}

  /**
   * Connect to a {pascal} server.
   *
   * @param uri — {pascal} connection URI.
   *   Format: `{scheme}://[token@]host[:port][/keyspace]`
   *   or `{scheme_tls}://[token@]host[:port][/keyspace]`
   * @param poolOptions — Connection pool tuning.
   */
  static async connect(
    uri: string = "{scheme}://localhost",
    poolOptions?: PoolOptions,
  ): Promise<{pascal}Client> {{
    const cfg = parseUri(uri);
    const pool = new Pool(cfg.host, cfg.port, cfg.tls, cfg.authToken, poolOptions);
    return new {pascal}Client(pool);
  }}

  /** Close the client and all pooled connections. */
  close(): void {{
    this.pool.close();
  }}

  /** @internal */
  private async execute(...args: string[]): Promise<unknown> {{
    const conn = await this.pool.get();
    try {{
      const result = await conn.execute(...args);
      this.pool.put(conn);
      return result;
    }} catch (e) {{
      conn.close();
      throw e;
    }}
  }}
"#,
        pascal = n.pascal,
        scheme = scheme,
        scheme_tls = scheme_tls,
        port = n.default_port,
        description = n.description,
    ));

    // Generate methods
    for (cmd_name, cmd) in &spec.commands {
        writeln!(out).unwrap();
        gen_ts_method(&mut out, spec, cmd_name, cmd);
    }

    out.push_str("}\n");

    GeneratedFile {
        path: "src/client.ts".into(),
        content: out,
    }
}

fn gen_ts_method(out: &mut String, spec: &ProtocolSpec, cmd_name: &str, cmd: &CommandDef) {
    if cmd.streaming {
        writeln!(
            out,
            "  // subscribe() requires streaming support — use raw connection"
        )
        .unwrap();
        return;
    }

    let method_name = cmd_name.to_lower_camel_case();
    let positional = cmd.positional_params();
    let named = cmd.named_params();

    // Build params
    let mut params: Vec<String> = Vec::new();
    for p in &positional {
        let ts = ts_type(spec, &p.param_type);
        if p.required {
            params.push(format!("{}: {ts}", p.name.to_lower_camel_case()));
        } else {
            params.push(format!("{}?: {ts}", p.name.to_lower_camel_case()));
        }
    }

    let has_options = !named.is_empty();
    if has_options {
        let mut opt_fields = Vec::new();
        for p in &named {
            if p.param_type == "boolean_flag" {
                opt_fields.push(format!("{}?: boolean", p.name.to_lower_camel_case()));
            } else if p.param_type == "json_value" {
                opt_fields.push(format!(
                    "{}?: Record<string, unknown>",
                    p.name.to_lower_camel_case()
                ));
            } else if p.variadic {
                opt_fields.push(format!("{}?: string[]", p.name.to_lower_camel_case()));
            } else {
                let ts = ts_type(spec, &p.param_type);
                opt_fields.push(format!("{}?: {ts}", p.name.to_lower_camel_case()));
            }
        }
        params.push(format!("options?: {{ {} }}", opt_fields.join("; ")));
    }

    let return_type = if cmd.simple_response || cmd.response.is_empty() {
        "void".to_string()
    } else {
        format!("{}Response", to_pascal(cmd_name))
    };

    writeln!(out, "  /** {} */", cmd.description).unwrap();
    writeln!(
        out,
        "  async {method_name}({params}): Promise<{return_type}> {{",
        params = params.join(", "),
    )
    .unwrap();
    writeln!(out, "    const args: string[] = [];").unwrap();

    if let Some(sub) = &cmd.subcommand {
        writeln!(out, "    args.push(\"{}\", \"{}\");", cmd.verb, sub).unwrap();
    } else {
        writeln!(out, "    args.push(\"{}\");", cmd.verb).unwrap();
    }

    for p in &positional {
        let js_name = p.name.to_lower_camel_case();
        if p.required {
            writeln!(out, "    args.push(String({js_name}));").unwrap();
        } else {
            writeln!(
                out,
                "    if ({js_name} !== undefined) args.push(String({js_name}));"
            )
            .unwrap();
        }
    }

    if has_options {
        for p in &named {
            let js_name = p.name.to_lower_camel_case();
            let key = p.key.as_deref().unwrap();
            if p.param_type == "boolean_flag" {
                writeln!(out, "    if (options?.{js_name}) args.push(\"{key}\");").unwrap();
            } else if p.param_type == "json_value" {
                writeln!(
                    out,
                    "    if (options?.{js_name} !== undefined) args.push(\"{key}\", JSON.stringify(options.{js_name}));"
                ).unwrap();
            } else if p.variadic {
                writeln!(
                    out,
                    "    if (options?.{js_name}) {{ args.push(\"{key}\"); args.push(...options.{js_name}); }}"
                ).unwrap();
            } else {
                writeln!(
                    out,
                    "    if (options?.{js_name} !== undefined) args.push(\"{key}\", String(options.{js_name}));"
                ).unwrap();
            }
        }
    }

    writeln!(out, "    const result = await this.execute(...args);").unwrap();
    if !cmd.simple_response && !cmd.response.is_empty() {
        writeln!(out, "    return result as {return_type};").unwrap();
    }
    writeln!(out, "  }}").unwrap();
}

// ─── index.ts ────────────────────────────────────────────────────────────────

fn gen_index(spec: &ProtocolSpec, n: &Naming) -> GeneratedFile {
    let scheme = n
        .uri_schemes
        .first()
        .map(|s| s.as_str())
        .unwrap_or(&n.snake);
    let mut out = format!(
        r#"/**
 * {pascal} — TypeScript client for the {pascal} {description}.
 *
 * Auto-generated from {raw} protocol spec. Do not edit.
 *
 * @example
 * ```ts
 * import {{ {pascal}Client }} from "{npm_name}";
 *
 * const client = await {pascal}Client.connect("{scheme}://localhost");
 * const result = await client.issue("my-keyspace", {{ ttlSecs: 3600 }});
 * console.log(result.credentialId, result.token);
 * client.close();
 * ```
 */

export {{ {pascal}Client }} from "./client";
export {{ {pascal}Error }} from "./errors";
"#,
        pascal = n.pascal,
        raw = n.raw,
        npm_name = n.npm_name,
        scheme = scheme,
        description = n.description,
    );

    // Re-export typed response interfaces
    let mut type_exports = Vec::new();
    for (cmd_name, cmd) in &spec.commands {
        if !cmd.response.is_empty() && !cmd.simple_response {
            type_exports.push(format!("{}Response", to_pascal(cmd_name)));
        }
    }
    if !type_exports.is_empty() {
        writeln!(
            out,
            "export type {{ {} }} from \"./types\";",
            type_exports.join(", ")
        )
        .unwrap();
    }

    // Re-export error classes
    let mut error_exports = Vec::new();
    for code in spec.error_codes.keys() {
        error_exports.push(code_to_ts_class(code));
    }
    if !error_exports.is_empty() {
        writeln!(
            out,
            "export {{ {} }} from \"./errors\";",
            error_exports.join(", ")
        )
        .unwrap();
    }

    GeneratedFile {
        path: "src/index.ts".into(),
        content: out,
    }
}

fn gen_package_json(spec: &ProtocolSpec, n: &Naming) -> GeneratedFile {
    GeneratedFile {
        path: "package.json".into(),
        content: format!(
            r#"{{
  "name": "{npm_name}",
  "version": "{version}",
  "description": "TypeScript client for the {pascal} {description}",
  "main": "dist/index.js",
  "types": "dist/index.d.ts",
  "scripts": {{
    "build": "tsc",
    "prepublishOnly": "npm run build"
  }},
  "license": "MIT",
  "devDependencies": {{
    "typescript": "^5.0.0"
  }},
  "files": [
    "dist/",
    "src/"
  ]
}}
"#,
            npm_name = n.npm_name,
            version = spec.protocol.version,
            pascal = n.pascal,
            description = n.description,
        ),
    }
}

fn gen_tsconfig(_n: &Naming) -> GeneratedFile {
    GeneratedFile {
        path: "tsconfig.json".into(),
        content: r#"{
  "compilerOptions": {
    "target": "ES2022",
    "module": "Node16",
    "moduleResolution": "Node16",
    "declaration": true,
    "outDir": "dist",
    "rootDir": "src",
    "strict": true,
    "esModuleInterop": true,
    "skipLibCheck": true
  },
  "include": ["src/**/*.ts"]
}
"#
        .into(),
    }
}

fn gen_readme(spec: &ProtocolSpec, n: &Naming) -> GeneratedFile {
    let scheme = n
        .uri_schemes
        .first()
        .map(|s| s.as_str())
        .unwrap_or(&n.snake);
    let scheme_tls = format!("{}+tls", scheme);
    let mut cmds = String::new();
    for (cmd_name, cmd) in &spec.commands {
        if cmd.streaming {
            continue;
        }
        let method = cmd_name.to_lower_camel_case();
        cmds.push_str(&format!("- `client.{method}(...)` — {}\n", cmd.description));
    }

    GeneratedFile {
        path: "README.md".into(),
        content: format!(
            r#"# {pascal} TypeScript Client

TypeScript/Node.js client for the [{pascal}](https://github.com/shroudb/{kebab}) {description}.

## Install

```bash
npm install {npm_name}
```

## Quick Start

```typescript
import {{ {pascal}Client }} from "{npm_name}";

const client = await {pascal}Client.connect("{scheme}://localhost");

// Issue a credential
const result = await client.issue("my-keyspace", {{ ttlSecs: 3600 }});
console.log(result.credentialId, result.token);

// Verify it
const verified = await client.verify("my-keyspace", result.token);
console.log(verified.state); // "active"

client.close();
```

## Connection URI

```
{scheme}://[token@]host[:port][/keyspace]
{scheme_tls}://[token@]host[:port][/keyspace]
```

Examples:
- `{scheme}://localhost` — plain TCP, default port {port}
- `{scheme_tls}://prod.example.com` — TLS
- `{scheme}://mytoken@localhost:{port}/sessions` — auth + keyspace

## Connection Pool

The client maintains an internal connection pool. Tune it at connect time:

```typescript
const client = await {pascal}Client.connect("{scheme}://localhost", {{
  maxIdle: 8,
  maxOpen: 32,
}});
```

## Commands

{cmds}

## Auto-generated

This client was generated by `shroudb-codegen` from `protocol.toml`.
"#,
            pascal = n.pascal,
            kebab = n.kebab,
            npm_name = n.npm_name,
            scheme = scheme,
            scheme_tls = scheme_tls,
            port = n.default_port,
            description = n.description,
            cmds = cmds,
        ),
    }
}

fn to_pascal(s: &str) -> String {
    s.split('_')
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => {
                    let mut s = first.to_uppercase().to_string();
                    s.extend(chars);
                    s
                }
            }
        })
        .collect()
}
