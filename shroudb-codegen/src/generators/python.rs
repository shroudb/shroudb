//! Python client generator.
//!
//! Produces a self-contained Python package with:
//! - `_connection.py` — internal protocol codec (private)
//! - `_pool.py`       — internal connection pool (private)
//! - `client.py`      — public client, URI-first, pooled
//! - `types.py`       — dataclasses for responses
//! - `errors.py`      — exception hierarchy
//! - `__init__.py`    — re-exports only public API

use crate::generator::{GeneratedFile, Generator, Naming};
use crate::spec::{CommandDef, ProtocolSpec};
use heck::ToSnakeCase;
use std::fmt::Write;

pub struct PythonGenerator;

impl Generator for PythonGenerator {
    fn language(&self) -> &'static str {
        "Python"
    }

    fn generate(&self, spec: &ProtocolSpec) -> Vec<GeneratedFile> {
        let n = Naming::from_spec(spec);
        vec![
            gen_connection(spec, &n),
            gen_pool(spec, &n),
            gen_errors(spec, &n),
            gen_types(spec, &n),
            gen_client(spec, &n),
            gen_init(spec, &n),
            gen_pyproject(spec, &n),
            gen_readme(spec, &n),
        ]
    }
}

fn python_type(spec: &ProtocolSpec, type_name: &str) -> &'static str {
    match spec.types.get(type_name) {
        Some(t) => match t.python_type.as_str() {
            "str" => "str",
            "int" => "int",
            "bool" => "bool",
            "dict[str, Any]" => "dict[str, Any]",
            _ => "Any",
        },
        None => "Any",
    }
}

// ─── _connection.py (private) ────────────────────────────────────────────────

fn gen_connection(_spec: &ProtocolSpec, n: &Naming) -> GeneratedFile {
    GeneratedFile {
        path: format!("{}/_connection.py", n.snake),
        content: format!(
            r#""""
Internal {pascal} protocol codec.

This module is an implementation detail of the {pascal} client library.
Do not import directly — use `{snake}.{pascal}Client` instead.

Auto-generated from {raw} protocol spec. Do not edit.
"""
from __future__ import annotations

import asyncio
import ssl
from typing import Any

from .errors import {pascal}Error

DEFAULT_PORT = {port}


class _Connection:
    """Low-level async connection. Internal use only."""

    def __init__(
        self,
        reader: asyncio.StreamReader,
        writer: asyncio.StreamWriter,
    ) -> None:
        self._reader = reader
        self._writer = writer

    @classmethod
    async def open(cls, host: str, port: int, *, tls: bool = False) -> "_Connection":
        if tls:
            ctx = ssl.create_default_context()
            reader, writer = await asyncio.open_connection(host, port, ssl=ctx)
        else:
            reader, writer = await asyncio.open_connection(host, port)
        return cls(reader, writer)

    async def execute(self, *args: str) -> Any:
        """Send a command and read the response."""
        parts = [f"*{{len(args)}}\r\n"]
        for arg in args:
            encoded = arg.encode("utf-8")
            parts.append(f"${{len(encoded)}}\r\n")
            parts.append(arg)
            parts.append("\r\n")
        self._writer.write("".join(parts).encode("utf-8"))
        await self._writer.drain()
        return await self._read_frame()

    async def _read_frame(self) -> Any:
        prefix = await self._reader.readline()
        if not prefix:
            raise ConnectionError("Connection closed")
        tag = chr(prefix[0])
        payload = prefix[1:].rstrip(b"\r\n").decode("utf-8")

        if tag == "+":
            return payload
        elif tag == "-":
            code, _, message = payload.partition(" ")
            raise {pascal}Error._from_server(code, message)
        elif tag == ":":
            return int(payload)
        elif tag == "$":
            length = int(payload)
            if length < 0:
                return None
            data = await self._reader.readexactly(length + 2)
            return data[:-2].decode("utf-8")
        elif tag == "*":
            count = int(payload)
            return [await self._read_frame() for _ in range(count)]
        elif tag == "%":
            count = int(payload)
            result: dict[str, Any] = {{}}
            for _ in range(count):
                key = await self._read_frame()
                val = await self._read_frame()
                result[str(key)] = val
            return result
        elif tag == "_":
            return None
        else:
            raise {pascal}Error(f"INTERNAL", f"Unknown response type: {{tag}}")

    async def close(self) -> None:
        self._writer.close()
        await self._writer.wait_closed()
"#,
            port = n.default_port,
            pascal = n.pascal,
            snake = n.snake,
            raw = n.raw,
        ),
    }
}

// ─── _pool.py (private) ──────────────────────────────────────────────────────

fn gen_pool(_spec: &ProtocolSpec, n: &Naming) -> GeneratedFile {
    GeneratedFile {
        path: format!("{}/_pool.py", n.snake),
        content: format!(
            r#""""
Internal connection pool.

This module is an implementation detail of the {pascal} client library.
Do not import directly — use `{snake}.{pascal}Client` instead.

Auto-generated from {raw} protocol spec. Do not edit.
"""
from __future__ import annotations

import asyncio
from collections import deque
from typing import Optional

from ._connection import _Connection


class _Pool:
    """Async connection pool for {pascal}. Internal use only."""

    def __init__(
        self,
        host: str,
        port: int,
        *,
        tls: bool = False,
        auth: Optional[str] = None,
        max_idle: int = 4,
        max_open: int = 0,
    ) -> None:
        self._host = host
        self._port = port
        self._tls = tls
        self._auth = auth
        self._max_idle = max_idle
        self._max_open = max_open
        self._idle: deque[_Connection] = deque()
        self._open = 0
        self._lock = asyncio.Lock()

    async def get(self) -> _Connection:
        async with self._lock:
            if self._idle:
                return self._idle.pop()
            self._open += 1

        try:
            conn = await _Connection.open(self._host, self._port, tls=self._tls)
            if self._auth:
                await conn.execute("AUTH", self._auth)
            return conn
        except Exception:
            async with self._lock:
                self._open -= 1
            raise

    async def put(self, conn: _Connection) -> None:
        async with self._lock:
            if len(self._idle) < self._max_idle:
                self._idle.append(conn)
            else:
                await conn.close()
                self._open -= 1

    async def close(self) -> None:
        async with self._lock:
            while self._idle:
                await self._idle.pop().close()
            self._open = 0
"#,
            pascal = n.pascal,
            snake = n.snake,
            raw = n.raw,
        ),
    }
}

// ─── errors.py ───────────────────────────────────────────────────────────────

fn gen_errors(spec: &ProtocolSpec, n: &Naming) -> GeneratedFile {
    let mut out = format!(
        r#""""
{pascal} error types.

Auto-generated from {raw} protocol spec. Do not edit.
"""
from __future__ import annotations


class {pascal}Error(Exception):
    """Base exception for all {pascal} operations."""

    def __init__(self, code: str, message: str) -> None:
        self.code = code
        self.message = message
        super().__init__(f"[{{code}}] {{message}}")

    @classmethod
    def _from_server(cls, code: str, message: str) -> "{pascal}Error":
        """Internal: construct the appropriate error subclass from a server error."""
        subclass = _ERROR_MAP.get(code, {pascal}Error)
        return subclass(code, message)

"#,
        pascal = n.pascal,
        raw = n.raw,
    );

    for (code, def) in &spec.error_codes {
        let class_name = code_to_class(code);
        writeln!(out).unwrap();
        writeln!(
            out,
            "class {class_name}({pascal}Error):\n    \"\"\"{}\"\"\"\n",
            def.description,
            pascal = n.pascal,
        )
        .unwrap();
    }

    out.push_str(&format!(
        "\n_ERROR_MAP: dict[str, type[{pascal}Error]] = {{\n",
        pascal = n.pascal,
    ));
    for code in spec.error_codes.keys() {
        let class_name = code_to_class(code);
        writeln!(out, "    \"{code}\": {class_name},").unwrap();
    }
    out.push_str("}\n");

    GeneratedFile {
        path: format!("{}/errors.py", n.snake),
        content: out,
    }
}

fn code_to_class(code: &str) -> String {
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

// ─── types.py ────────────────────────────────────────────────────────────────

fn gen_types(spec: &ProtocolSpec, n: &Naming) -> GeneratedFile {
    let mut out = format!(
        r#""""
{pascal} response types.

Auto-generated from {raw} protocol spec. Do not edit.
"""
from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, Optional

"#,
        pascal = n.pascal,
        raw = n.raw,
    );

    for (cmd_name, cmd) in &spec.commands {
        if cmd.response.is_empty() || cmd.simple_response {
            continue;
        }
        let class_name = format!("{}Response", to_pascal(cmd_name));
        writeln!(out, "@dataclass").unwrap();
        writeln!(out, "class {class_name}:").unwrap();
        writeln!(out, "    \"\"\"Response from {} command.\"\"\"", cmd.verb).unwrap();
        writeln!(out).unwrap();

        for f in &cmd.response {
            let py_type = python_type(spec, &f.field_type);
            if f.optional {
                writeln!(out, "    {}: Optional[{py_type}] = None", f.name).unwrap();
            } else {
                writeln!(out, "    {}: {py_type} = {}", f.name, py_default(py_type)).unwrap();
            }
        }

        writeln!(out).unwrap();
        writeln!(out, "    @classmethod").unwrap();
        writeln!(
            out,
            "    def _from_dict(cls, data: dict[str, Any]) -> \"{class_name}\":"
        )
        .unwrap();
        writeln!(out, "        return cls(").unwrap();
        for f in &cmd.response {
            if f.optional {
                writeln!(out, "            {} = data.get(\"{}\"),", f.name, f.name).unwrap();
            } else {
                writeln!(out, "            {} = data[\"{}\"],", f.name, f.name).unwrap();
            }
        }
        writeln!(out, "        )").unwrap();
        writeln!(out).unwrap();
        writeln!(out).unwrap();
    }

    GeneratedFile {
        path: format!("{}/types.py", n.snake),
        content: out,
    }
}

fn py_default(py_type: &str) -> &str {
    match py_type {
        "str" => "\"\"",
        "int" => "0",
        "bool" => "False",
        "dict[str, Any]" => "field(default_factory=dict)",
        _ => "None",
    }
}

// ─── client.py ───────────────────────────────────────────────────────────────

fn gen_client(spec: &ProtocolSpec, n: &Naming) -> GeneratedFile {
    let scheme = n
        .uri_schemes
        .first()
        .map(|s| s.as_str())
        .unwrap_or(&n.snake);
    let scheme_tls = format!("{}+tls", scheme);
    let mut out = format!(
        r#""""
{pascal} client.

Auto-generated from {raw} protocol spec. Do not edit.
"""
from __future__ import annotations

import json
from typing import Any, Optional

from .errors import {pascal}Error
from ._connection import DEFAULT_PORT
from ._pool import _Pool
"#,
        pascal = n.pascal,
        raw = n.raw,
    );

    // Import typed response classes
    let mut imports = Vec::new();
    for (cmd_name, cmd) in &spec.commands {
        if !cmd.response.is_empty() && !cmd.simple_response {
            imports.push(format!("{}Response", to_pascal(cmd_name)));
        }
    }
    if !imports.is_empty() {
        writeln!(out, "from .types import {}", imports.join(", ")).unwrap();
    }

    writeln!(out).unwrap();
    write!(
        out,
        r#"
def parse_uri(uri: str) -> dict[str, Any]:
    """Parse a {pascal} connection URI.

    Supported formats::

        {scheme}://localhost
        {scheme}://localhost:{port}
        {scheme_tls}://prod.example.com
        {scheme}://mytoken@localhost:{port}
        {scheme}://mytoken@localhost/sessions
        {scheme_tls}://tok@host:{port}/keys
    """
    tls = False
    if uri.startswith("{scheme_tls}://"):
        tls = True
        rest = uri[len("{scheme_tls}://"):]
    elif uri.startswith("{scheme}://"):
        rest = uri[len("{scheme}://"):]
    else:
        raise ValueError(f"Invalid {pascal} URI: {{uri}}  (expected {scheme}:// or {scheme_tls}://)")

    auth_token = None
    if "@" in rest:
        auth_token, rest = rest.split("@", 1)

    keyspace = None
    if "/" in rest:
        rest, keyspace = rest.split("/", 1)
        if not keyspace:
            keyspace = None

    host = rest
    port = DEFAULT_PORT
    if ":" in host:
        host, port_str = host.rsplit(":", 1)
        try:
            port = int(port_str)
        except ValueError:
            port = DEFAULT_PORT

    return {{
        "host": host,
        "port": port,
        "tls": tls,
        "auth_token": auth_token,
        "keyspace": keyspace,
    }}


class {pascal}Client:
    """Async client for the {pascal} {description}.

    Connect using a {pascal} URI::

        async with await {pascal}Client.connect("{scheme}://localhost") as client:
            result = await client.issue("my-keyspace")
            print(result.token)

        # With TLS and auth:
        client = await {pascal}Client.connect("{scheme_tls}://mytoken@prod.example.com/keys")

        # With pool tuning:
        client = await {pascal}Client.connect("{scheme}://localhost", max_idle=8)
    """

    def __init__(self, pool: _Pool) -> None:
        self._pool = pool

    @classmethod
    async def connect(
        cls,
        uri: str = "{scheme}://localhost",
        *,
        max_idle: int = 4,
        max_open: int = 0,
    ) -> "{pascal}Client":
        """Connect to a {pascal} server.

        Args:
            uri: {pascal} connection URI.
                 Format: ``{scheme}://[token@]host[:port][/keyspace]``
                 or ``{scheme_tls}://[token@]host[:port][/keyspace]``
            max_idle: Maximum idle connections in pool (default: 4).
            max_open: Maximum total connections, 0 = unlimited (default: 0).

        Returns:
            A connected {pascal}Client instance.

        Examples::

            client = await {pascal}Client.connect("{scheme}://localhost")
            client = await {pascal}Client.connect("{scheme_tls}://token@host:{port}/keys")
        """
        cfg = parse_uri(uri)
        pool = _Pool(
            cfg["host"],
            cfg["port"],
            tls=cfg["tls"],
            auth=cfg["auth_token"],
            max_idle=max_idle,
            max_open=max_open,
        )
        return cls(pool)

    async def close(self) -> None:
        """Close the client and all pooled connections."""
        await self._pool.close()

    async def __aenter__(self) -> "{pascal}Client":
        return self

    async def __aexit__(self, *args: Any) -> None:
        await self.close()

    async def _execute(self, *args: str) -> Any:
        """Acquire a pooled connection, execute, and return it."""
        conn = await self._pool.get()
        try:
            result = await conn.execute(*args)
            await self._pool.put(conn)
            return result
        except Exception:
            await conn.close()
            raise
"#,
        pascal = n.pascal,
        scheme = scheme,
        scheme_tls = scheme_tls,
        port = n.default_port,
        description = n.description,
    )
    .unwrap();

    // Generate a method for each command
    for (cmd_name, cmd) in &spec.commands {
        writeln!(out).unwrap();
        gen_python_method(&mut out, spec, cmd_name, cmd);
    }

    GeneratedFile {
        path: format!("{}/client.py", n.snake),
        content: out,
    }
}

fn gen_python_method(out: &mut String, spec: &ProtocolSpec, cmd_name: &str, cmd: &CommandDef) {
    if cmd.streaming {
        gen_python_subscribe(out, cmd);
        return;
    }

    let method_name = cmd_name.to_snake_case();
    let positional = cmd.positional_params();
    let named = cmd.named_params();

    // Build signature
    let mut sig_parts: Vec<String> = vec!["self".into()];
    for p in &positional {
        let py_type = python_type(spec, &p.param_type);
        if p.required {
            sig_parts.push(format!("{}: {py_type}", p.name));
        } else {
            sig_parts.push(format!("{}: Optional[{py_type}] = None", p.name));
        }
    }
    for p in &named {
        let py_type = python_type(spec, &p.param_type);
        if p.param_type == "boolean_flag" {
            sig_parts.push(format!("{}: bool = False", p.name));
        } else if p.param_type == "json_value" {
            sig_parts.push(format!("{}: Optional[dict[str, Any]] = None", p.name));
        } else if p.variadic {
            sig_parts.push(format!("{}: Optional[list[{py_type}]] = None", p.name));
        } else if p.required {
            sig_parts.push(format!("{}: {py_type}", p.name));
        } else {
            sig_parts.push(format!("{}: Optional[{py_type}] = None", p.name));
        }
    }

    // Return type
    let return_type = if cmd.simple_response || cmd.response.is_empty() {
        "None".to_string()
    } else {
        format!("{}Response", to_pascal(cmd_name))
    };

    writeln!(
        out,
        "    async def {method_name}({}) -> {return_type}:",
        sig_parts.join(", ")
    )
    .unwrap();
    writeln!(out, "        \"\"\"{}\"\"\"", cmd.description).unwrap();

    // Build args list
    writeln!(out, "        args: list[str] = []").unwrap();

    if let Some(sub) = &cmd.subcommand {
        writeln!(out, "        args.extend([\"{}\", \"{}\"])", cmd.verb, sub).unwrap();
    } else {
        writeln!(out, "        args.append(\"{}\")", cmd.verb).unwrap();
    }

    for p in &positional {
        if p.required {
            writeln!(out, "        args.append(str({}))", p.name).unwrap();
        } else {
            writeln!(out, "        if {} is not None:", p.name).unwrap();
            writeln!(out, "            args.append(str({}))", p.name).unwrap();
        }
    }

    for p in &named {
        let key = p.key.as_deref().unwrap();
        if p.param_type == "boolean_flag" {
            writeln!(out, "        if {}:", p.name).unwrap();
            writeln!(out, "            args.append(\"{key}\")").unwrap();
        } else if p.param_type == "json_value" {
            writeln!(out, "        if {} is not None:", p.name).unwrap();
            writeln!(
                out,
                "            args.extend([\"{key}\", json.dumps({})])",
                p.name
            )
            .unwrap();
        } else if p.variadic {
            writeln!(out, "        if {} is not None:", p.name).unwrap();
            writeln!(out, "            args.append(\"{key}\")").unwrap();
            writeln!(out, "            args.extend(str(x) for x in {})", p.name).unwrap();
        } else {
            writeln!(out, "        if {} is not None:", p.name).unwrap();
            writeln!(out, "            args.extend([\"{key}\", str({})])", p.name).unwrap();
        }
    }

    writeln!(out, "        result = await self._execute(*args)").unwrap();

    if !cmd.simple_response && !cmd.response.is_empty() {
        writeln!(out, "        return {return_type}._from_dict(result)").unwrap();
    }
    writeln!(out).unwrap();
}

fn gen_python_subscribe(out: &mut String, _cmd: &CommandDef) {
    out.push_str(
        r#"    async def subscribe(self, channel: str):
        """Subscribe to real-time event notifications.

        Args:
            channel: Channel name (e.g. ``"keyspace:tokens"``).

        Yields event arrays as they arrive::

            async for event in client.subscribe("keyspace:tokens"):
                print(event)  # ['issued', 'tokens', 'cred_abc123']
        """
        await self._conn.execute("SUBSCRIBE", channel)
        while True:
            try:
                frame = await self._conn._read_frame()
                yield frame
            except ConnectionError:
                break

"#,
    );
}

// ─── __init__.py ─────────────────────────────────────────────────────────────

fn gen_init(spec: &ProtocolSpec, n: &Naming) -> GeneratedFile {
    let scheme = n
        .uri_schemes
        .first()
        .map(|s| s.as_str())
        .unwrap_or(&n.snake);
    let mut out = format!(
        r#""""
{pascal} — Python client for the {pascal} {description}.

Auto-generated from {raw} protocol spec. Do not edit.

Usage::

    from {snake} import {pascal}Client

    async with await {pascal}Client.connect("{scheme}://localhost") as client:
        result = await client.issue("my-keyspace", ttl_secs=3600)
        print(result.credential_id, result.token)
"""
from .client import {pascal}Client
from .errors import {pascal}Error
"#,
        pascal = n.pascal,
        raw = n.raw,
        snake = n.snake,
        scheme = scheme,
        description = n.description,
    );

    // Re-export typed response classes
    let mut type_names = Vec::new();
    for (cmd_name, cmd) in &spec.commands {
        if !cmd.response.is_empty() && !cmd.simple_response {
            let class_name = format!("{}Response", to_pascal(cmd_name));
            type_names.push(class_name);
        }
    }
    if !type_names.is_empty() {
        writeln!(out, "from .types import {}", type_names.join(", ")).unwrap();
    }

    // Re-export specific error classes
    let mut error_names = Vec::new();
    for code in spec.error_codes.keys() {
        let class_name = code_to_class(code);
        error_names.push(class_name);
    }
    if !error_names.is_empty() {
        writeln!(out, "from .errors import {}", error_names.join(", ")).unwrap();
    }

    writeln!(out).unwrap();
    writeln!(out, "__version__ = \"{}\"", spec.protocol.version).unwrap();

    // __all__
    writeln!(out).unwrap();
    writeln!(out, "__all__ = [").unwrap();
    writeln!(out, "    \"{}Client\",", n.pascal).unwrap();
    writeln!(out, "    \"{}Error\",", n.pascal).unwrap();
    for name in &type_names {
        writeln!(out, "    \"{name}\",").unwrap();
    }
    for name in &error_names {
        writeln!(out, "    \"{name}\",").unwrap();
    }
    writeln!(out, "]").unwrap();

    GeneratedFile {
        path: format!("{}/__init__.py", n.snake),
        content: out,
    }
}

fn gen_pyproject(spec: &ProtocolSpec, n: &Naming) -> GeneratedFile {
    GeneratedFile {
        path: "pyproject.toml".into(),
        content: format!(
            r#"[project]
name = "{kebab}"
version = "{version}"
description = "Python client for the {pascal} {description}"
requires-python = ">=3.10"
license = "MIT"

[build-system]
requires = ["setuptools>=68"]
build-backend = "setuptools.build_meta"
"#,
            kebab = n.kebab,
            version = spec.protocol.version,
            pascal = n.pascal,
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
            r#"# {pascal} Python Client

Python client for the [{pascal}](https://github.com/shroudb/{kebab}) {description}.

## Install

```bash
pip install {kebab}
```

## Quick Start

```python
import asyncio
from {snake} import {pascal}Client

async def main():
    client = await {pascal}Client.connect("{scheme}://localhost")

    # Issue a credential
    result = await client.issue("my-keyspace", ttl_secs=3600)
    print(result.credential_id, result.token)

    # Verify it
    verified = await client.verify("my-keyspace", result.token)
    print(verified.state)  # "active"

    await client.close()

asyncio.run(main())
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

```python
client = await {pascal}Client.connect("{scheme}://localhost", max_idle=8, max_open=32)
```

## Commands

{cmds}

## Context Manager

```python
async with await {pascal}Client.connect("{scheme}://localhost") as client:
    result = await client.issue("tokens")
```

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
