//! Ruby client generator.
//!
//! Produces a Ruby gem structure with:
//! - `lib/{name}.rb`             — main entrypoint
//! - `lib/{name}/client.rb`      — public Client, URI-first, with pool
//! - `lib/{name}/connection.rb`  — internal protocol codec
//! - `lib/{name}/pool.rb`        — connection pool
//! - `lib/{name}/errors.rb`      — error classes
//! - `lib/{name}/types.rb`       — response structs
//! - `{name}.gemspec`            — gem specification

use crate::generator::{GeneratedFile, Generator, Naming};
use crate::spec::{CommandDef, ProtocolSpec};
use heck::ToSnakeCase;
use std::fmt::Write;

pub struct RubyGenerator;

impl Generator for RubyGenerator {
    fn language(&self) -> &'static str {
        "Ruby"
    }

    fn generate(&self, spec: &ProtocolSpec) -> Vec<GeneratedFile> {
        let n = Naming::from_spec(spec);
        vec![
            gen_main(spec, &n),
            gen_connection(spec, &n),
            gen_pool(spec, &n),
            gen_errors(spec, &n),
            gen_types(spec, &n),
            gen_client(spec, &n),
            gen_gemspec(spec, &n),
            gen_readme(spec, &n),
        ]
    }
}

fn ruby_type(spec: &ProtocolSpec, type_name: &str) -> &'static str {
    match spec.types.get(type_name) {
        Some(_) => match type_name {
            "keyspace" | "credential_id" | "token" => "String",
            "integer" | "unix_timestamp" => "Integer",
            "boolean_flag" => "Boolean",
            "json_value" => "Hash",
            _ => "Object",
        },
        None => "Object",
    }
}

// ─── lib/{name}.rb ───────────────────────────────────────────────────────────

fn gen_main(spec: &ProtocolSpec, n: &Naming) -> GeneratedFile {
    let scheme = n
        .uri_schemes
        .first()
        .map(|s| s.as_str())
        .unwrap_or(&n.snake);
    GeneratedFile {
        path: format!("lib/{}.rb", n.snake),
        content: format!(
            r#"# frozen_string_literal: true

# {pascal} — Ruby client for the {pascal} {description}.
#
# Auto-generated from {raw} protocol spec. Do not edit.
#
# Usage:
#
#   client = {pascal}::Client.connect("{scheme}://localhost")
#   result = client.issue("my-keyspace", ttl: 3600)
#   puts result.credential_id, result.token
#   client.close

require_relative "{snake}/errors"
require_relative "{snake}/types"
require_relative "{snake}/connection"
require_relative "{snake}/pool"
require_relative "{snake}/client"

module {pascal}
  VERSION = "{version}"
  DEFAULT_PORT = {port}
end
"#,
            pascal = n.pascal,
            snake = n.snake,
            raw = n.raw,
            scheme = scheme,
            description = n.description,
            version = spec.protocol.version,
            port = n.default_port,
        ),
    }
}

// ─── lib/{name}/connection.rb (internal) ─────────────────────────────────────

fn gen_connection(_spec: &ProtocolSpec, n: &Naming) -> GeneratedFile {
    GeneratedFile {
        path: format!("lib/{}/connection.rb", n.snake),
        content: format!(
            r#"# frozen_string_literal: true

# Internal {pascal} protocol codec.
#
# This class is an implementation detail. Use {pascal}::Client instead.
#
# Auto-generated from {raw} protocol spec. Do not edit.

require "socket"
require "openssl"

module {pascal}
  # @api private
  class Connection
    def initialize(socket)
      @socket = socket
    end

    def self.open(host, port, tls: false)
      sock = TCPSocket.new(host, port)
      if tls
        ctx = OpenSSL::SSL::SSLContext.new
        ctx.set_params(verify_mode: OpenSSL::SSL::VERIFY_PEER)
        ssl = OpenSSL::SSL::SSLSocket.new(sock, ctx)
        ssl.hostname = host
        ssl.connect
        sock = ssl
      end
      new(sock)
    end

    def execute(*args)
      # Encode command
      cmd = "*#{{args.length}}\r\n"
      args.each do |arg|
        s = arg.to_s
        cmd << "$#{{s.bytesize}}\r\n#{{s}}\r\n"
      end
      @socket.write(cmd)
      read_frame
    end

    def close
      @socket.close
    rescue IOError
      # already closed
    end

    private

    def read_frame
      line = @socket.gets("\r\n")
      raise {pascal}::ConnectionError, "Connection closed" if line.nil?

      tag = line[0]
      payload = line[1..-3] # strip \r\n

      case tag
      when "+"
        payload
      when "-"
        code, message = payload.split(" ", 2)
        raise {pascal}::Error.from_server(code, message || "")
      when ":"
        payload.to_i
      when "$"
        len = payload.to_i
        return nil if len < 0

        data = @socket.read(len + 2) # +2 for \r\n
        data[0...len]
      when "*"
        count = payload.to_i
        Array.new(count) {{ read_frame }}
      when "%"
        count = payload.to_i
        hash = {{}}
        count.times do
          key = read_frame
          val = read_frame
          hash[key.to_s] = val
        end
        hash
      when "_"
        nil
      else
        raise {pascal}::Error.new("INTERNAL", "Unknown response type: #{{tag}}")
      end
    end
  end
end
"#,
            pascal = n.pascal,
            raw = n.raw,
        ),
    }
}

// ─── lib/{name}/pool.rb ──────────────────────────────────────────────────────

fn gen_pool(_spec: &ProtocolSpec, n: &Naming) -> GeneratedFile {
    GeneratedFile {
        path: format!("lib/{}/pool.rb", n.snake),
        content: format!(
            r#"# frozen_string_literal: true

# Connection pool for {pascal} clients.
#
# Auto-generated from {raw} protocol spec. Do not edit.

module {pascal}
  # @api private
  class Pool
    # @param host [String]
    # @param port [Integer]
    # @param tls [Boolean]
    # @param auth [String, nil]
    # @param max_idle [Integer] maximum idle connections (default: 4)
    # @param max_open [Integer] maximum total connections, 0 = unlimited (default: 0)
    def initialize(host:, port:, tls: false, auth: nil, max_idle: 4, max_open: 0)
      @host = host
      @port = port
      @tls = tls
      @auth = auth
      @max_idle = max_idle
      @max_open = max_open
      @idle = []
      @open = 0
      @mutex = Mutex.new
    end

    def get
      @mutex.synchronize do
        if (conn = @idle.pop)
          return conn
        end

        @open += 1
      end

      conn = Connection.open(@host, @port, tls: @tls)
      conn.execute("AUTH", @auth) if @auth
      conn
    rescue StandardError
      @mutex.synchronize {{ @open -= 1 }}
      raise
    end

    def put(conn)
      @mutex.synchronize do
        if @idle.length < @max_idle
          @idle.push(conn)
        else
          conn.close
          @open -= 1
        end
      end
    end

    def close
      @mutex.synchronize do
        @idle.each(&:close)
        @idle.clear
        @open = 0
      end
    end
  end
end
"#,
            pascal = n.pascal,
            raw = n.raw,
        ),
    }
}

// ─── lib/{name}/errors.rb ────────────────────────────────────────────────────

fn gen_errors(spec: &ProtocolSpec, n: &Naming) -> GeneratedFile {
    let mut out = format!(
        r#"# frozen_string_literal: true

# {pascal} error types.
#
# Auto-generated from {raw} protocol spec. Do not edit.

module {pascal}
  # Base error for all {pascal} operations.
  class Error < StandardError
    attr_reader :code, :detail

    def initialize(code, detail = "")
      @code = code
      @detail = detail
      super("[#{{code}}] #{{detail}}")
    end

    # @api private
    def self.from_server(code, detail)
      klass = ERROR_MAP[code] || Error
      klass.new(code, detail)
    end
  end

  # Raised when the underlying TCP connection fails.
  class ConnectionError < Error
    def initialize(msg = "connection error")
      super("CONNECTION", msg)
    end
  end

"#,
        pascal = n.pascal,
        raw = n.raw,
    );

    for (code, def) in &spec.error_codes {
        let class_name = code_to_rb_class(code);
        writeln!(out, "  # {}", def.description).unwrap();
        writeln!(out, "  class {class_name} < Error; end").unwrap();
        writeln!(out).unwrap();
    }

    // Error map
    out.push_str("  # @api private\n");
    out.push_str("  ERROR_MAP = {\n");
    for code in spec.error_codes.keys() {
        let class_name = code_to_rb_class(code);
        writeln!(out, "    \"{code}\" => {class_name},").unwrap();
    }
    out.push_str("  }.freeze\n");
    out.push_str("end\n");

    GeneratedFile {
        path: format!("lib/{}/errors.rb", n.snake),
        content: out,
    }
}

fn code_to_rb_class(code: &str) -> String {
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

// ─── lib/{name}/types.rb ────────────────────────────────────────────────────

fn gen_types(spec: &ProtocolSpec, n: &Naming) -> GeneratedFile {
    let mut out = format!(
        r#"# frozen_string_literal: true

# {pascal} response types.
#
# Auto-generated from {raw} protocol spec. Do not edit.

module {pascal}
"#,
        pascal = n.pascal,
        raw = n.raw,
    );

    for (cmd_name, cmd) in &spec.commands {
        if cmd.response.is_empty() || cmd.simple_response {
            continue;
        }
        let struct_name = to_rb_pascal(cmd_name);
        let all_fields: Vec<&str> = cmd.response.iter().map(|f| f.name.as_str()).collect();

        writeln!(out, "  # Response from {} command.", cmd.verb).unwrap();
        writeln!(
            out,
            "  {struct_name}Response = Struct.new({}, keyword_init: true)",
            all_fields
                .iter()
                .map(|f| format!(":{f}"))
                .collect::<Vec<_>>()
                .join(", ")
        )
        .unwrap();
        writeln!(out).unwrap();
    }

    out.push_str("end\n");

    GeneratedFile {
        path: format!("lib/{}/types.rb", n.snake),
        content: out,
    }
}

// ─── lib/{name}/client.rb ────────────────────────────────────────────────────

fn gen_client(spec: &ProtocolSpec, n: &Naming) -> GeneratedFile {
    let scheme = n
        .uri_schemes
        .first()
        .map(|s| s.as_str())
        .unwrap_or(&n.snake);
    let scheme_tls = format!("{}+tls", scheme);
    let mut out = format!(
        r#"# frozen_string_literal: true

# {pascal} client.
#
# Auto-generated from {raw} protocol spec. Do not edit.

require "json"
require "uri"

module {pascal}
  # Client for the {pascal} {description}.
  #
  # Connect using a {pascal} URI:
  #
  #   client = {pascal}::Client.connect("{scheme}://localhost")
  #   result = client.issue("my-keyspace", ttl: 3600)
  #   puts result.credential_id, result.token
  #   client.close
  #
  #   # With TLS and auth:
  #   client = {pascal}::Client.connect("{scheme_tls}://mytoken@prod.example.com/keys")
  #
  class Client
    # Connect to a {pascal} server.
    #
    # @param uri [String] {pascal} connection URI
    #   Format: +{scheme}://[token@]host[:port][/keyspace]+
    #   or +{scheme_tls}://[token@]host[:port][/keyspace]+
    # @param max_idle [Integer] max idle connections in pool (default: 4)
    # @param max_open [Integer] max total connections, 0 = unlimited (default: 0)
    # @return [Client]
    def self.connect(uri = "{scheme}://localhost", max_idle: 4, max_open: 0)
      cfg = parse_uri(uri)
      pool = Pool.new(
        host: cfg[:host],
        port: cfg[:port],
        tls: cfg[:tls],
        auth: cfg[:auth_token],
        max_idle: max_idle,
        max_open: max_open
      )
      new(pool)
    end

    # Close the client and all pooled connections.
    def close
      @pool.close
    end

"#,
        pascal = n.pascal,
        raw = n.raw,
        scheme = scheme,
        scheme_tls = scheme_tls,
        description = n.description,
    );

    // Generate methods
    for (cmd_name, cmd) in &spec.commands {
        gen_ruby_method(&mut out, spec, cmd_name, cmd);
    }

    // Private section: initializer, URI parser, exec helpers
    write!(
        out,
        r#"    private

    def initialize(pool)
      @pool = pool
    end

    def exec(*args)
      conn = @pool.get
      result = conn.execute(*args)
      @pool.put(conn)
      result
    rescue StandardError
      conn&.close
      raise
    end

    def self.parse_uri(uri)
      tls = false
      if uri.start_with?("{scheme_tls}://")
        tls = true
        uri = "{scheme}://#{{uri[{scheme_tls_prefix_len}..]}}"
"#,
        scheme = scheme,
        scheme_tls = scheme_tls,
        scheme_tls_prefix_len = scheme_tls.len() + 3, // "scheme+tls://".len()
    )
    .unwrap();
    write!(
        out,
        r#"
      elsif !uri.start_with?("{scheme}://")
        raise {pascal}::Error.new("BADARG", "Invalid {pascal} URI: #{{uri}}  (expected {scheme}:// or {scheme_tls}://)")
      end

      parsed = URI.parse(uri)
      {{
        host: parsed.host || "localhost",
        port: parsed.port || DEFAULT_PORT,
        tls: tls,
        auth_token: parsed.user,
        keyspace: parsed.path&.delete_prefix("/")&.then {{ |s| s.empty? ? nil : s }}
      }}
    end
  end
end
"#,
        pascal = n.pascal,
        scheme = scheme,
        scheme_tls = scheme_tls,
    )
    .unwrap();

    GeneratedFile {
        path: format!("lib/{}/client.rb", n.snake),
        content: out,
    }
}

fn gen_ruby_method(out: &mut String, spec: &ProtocolSpec, cmd_name: &str, cmd: &CommandDef) {
    if cmd.streaming {
        writeln!(
            out,
            "    # subscribe is not yet supported (requires streaming)."
        )
        .unwrap();
        writeln!(out).unwrap();
        return;
    }

    let method_name = cmd_name.to_snake_case();
    let positional = cmd.positional_params();
    let named = cmd.named_params();
    let has_response = !cmd.response.is_empty() && !cmd.simple_response;
    let response_struct = format!("{}Response", to_rb_pascal(cmd_name));

    // Build signature
    let mut params: Vec<String> = Vec::new();
    for p in &positional {
        if p.required {
            params.push(p.name.clone());
        } else {
            params.push(format!("{}: nil", p.name));
        }
    }
    for p in &named {
        if p.param_type == "boolean_flag" {
            params.push(format!("{}: false", p.name));
        } else {
            params.push(format!("{}: nil", p.name));
        }
    }

    // Doc comment
    writeln!(out, "    # {}", cmd.description).unwrap();
    for p in &positional {
        let rb_type = ruby_type(spec, &p.param_type);
        writeln!(out, "    # @param {} [{rb_type}] {}", p.name, p.description).unwrap();
    }
    for p in &named {
        let rb_type = ruby_type(spec, &p.param_type);
        writeln!(out, "    # @param {} [{rb_type}] {}", p.name, p.description).unwrap();
    }
    if has_response {
        writeln!(out, "    # @return [{response_struct}]").unwrap();
    }

    writeln!(out, "    def {method_name}({})", params.join(", ")).unwrap();
    writeln!(out, "      args = []").unwrap();

    // Verb
    if let Some(sub) = &cmd.subcommand {
        writeln!(out, "      args.push(\"{}\", \"{}\")", cmd.verb, sub).unwrap();
    } else {
        writeln!(out, "      args.push(\"{}\")", cmd.verb).unwrap();
    }

    // Positional
    for p in &positional {
        if p.required {
            writeln!(out, "      args.push({}.to_s)", p.name).unwrap();
        } else {
            writeln!(
                out,
                "      args.push({}.to_s) unless {}.nil?",
                p.name, p.name
            )
            .unwrap();
        }
    }

    // Named
    for p in &named {
        let key = p.key.as_deref().unwrap();
        if p.param_type == "boolean_flag" {
            writeln!(out, "      args.push(\"{key}\") if {}", p.name).unwrap();
        } else if p.param_type == "json_value" {
            writeln!(out, "      unless {}.nil?", p.name).unwrap();
            writeln!(
                out,
                "        args.push(\"{key}\", JSON.generate({}))",
                p.name
            )
            .unwrap();
            writeln!(out, "      end").unwrap();
        } else if p.variadic {
            writeln!(out, "      unless {}.nil? || {}.empty?", p.name, p.name).unwrap();
            writeln!(out, "        args.push(\"{key}\")").unwrap();
            writeln!(out, "        args.concat({}.map(&:to_s))", p.name).unwrap();
            writeln!(out, "      end").unwrap();
        } else {
            writeln!(out, "      unless {}.nil?", p.name).unwrap();
            writeln!(out, "        args.push(\"{key}\", {}.to_s)", p.name).unwrap();
            writeln!(out, "      end").unwrap();
        }
    }

    // Execute
    writeln!(out, "      result = exec(*args)").unwrap();
    if has_response {
        let fields: Vec<String> = cmd
            .response
            .iter()
            .map(|f| format!("{}: result[\"{}\"]", f.name, f.name))
            .collect();
        writeln!(out, "      {response_struct}.new({})", fields.join(", ")).unwrap();
    }
    writeln!(out, "    end").unwrap();
    writeln!(out).unwrap();
}

fn gen_gemspec(spec: &ProtocolSpec, n: &Naming) -> GeneratedFile {
    let scheme = n
        .uri_schemes
        .first()
        .map(|s| s.as_str())
        .unwrap_or(&n.snake);
    GeneratedFile {
        path: format!("{}.gemspec", n.kebab),
        content: format!(
            r#"# frozen_string_literal: true

Gem::Specification.new do |s|
  s.name        = "{kebab}"
  s.version     = "{version}"
  s.summary     = "Ruby client for the {pascal} {description}"
  s.description = "Typed Ruby client for {pascal}. Connects via {scheme}:// URI, manages credentials via the {pascal} protocol."
  s.license     = "MIT"
  s.authors     = ["{pascal}"]
  s.files       = Dir["lib/**/*.rb"]
  s.required_ruby_version = ">= 3.1"
end
"#,
            kebab = n.kebab,
            version = spec.protocol.version,
            pascal = n.pascal,
            scheme = scheme,
            description = n.description,
        ),
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
        let method = cmd_name.to_snake_case();
        cmds.push_str(&format!("- `client.{method}(...)` — {}\n", cmd.description));
    }

    GeneratedFile {
        path: "README.md".into(),
        content: format!(
            r#"# {pascal} Ruby Client

Ruby client for the [{pascal}](https://github.com/shroudb/{kebab}) {description}.

## Install

```bash
gem install {kebab}
```

Or in your Gemfile:

```ruby
gem "{kebab}"
```

## Quick Start

```ruby
require "{snake}"

client = {pascal}::Client.connect("{scheme}://localhost")

# Issue a credential
result = client.issue("my-keyspace", ttl_secs: 3600)
puts result.credential_id, result.token

# Verify it
verified = client.verify("my-keyspace", result.token)
puts verified.state  # "active"

client.close
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

```ruby
client = {pascal}::Client.connect("{scheme}://localhost", max_idle: 8, max_open: 32)
```

## Commands

{cmds}

## Auto-generated

This client was generated by `shroudb-codegen` from `protocol.toml`.
"#,
            pascal = n.pascal,
            kebab = n.kebab,
            snake = n.snake,
            scheme = scheme,
            scheme_tls = scheme_tls,
            port = n.default_port,
            description = n.description,
            cmds = cmds,
        ),
    }
}

fn to_rb_pascal(s: &str) -> String {
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
