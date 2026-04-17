//! shroudb-cli — interactive command-line client for ShrouDB.

use clap::Parser;
use rustyline::error::ReadlineError;
use rustyline::hint::HistoryHinter;
use shroudb_client::Response;
use shroudb_client::connection::Connection;

/// Known command names for tab completion.
const COMMANDS: &[&str] = &[
    "AUTH",
    "PING",
    "PUT",
    "PUTIF",
    "GET",
    "DELETE",
    "DELIF",
    "DELPREFIX",
    "LIST",
    "VERSIONS",
    "NAMESPACE",
    "PIPELINE",
    "SUBSCRIBE",
    "UNSUBSCRIBE",
    "HEALTH",
    "CONFIG",
    "COMMAND",
    "help",
    "quit",
    "exit",
];

// ---------------------------------------------------------------------------
// Tab-completion helper
// ---------------------------------------------------------------------------

struct ShrouDBHelper {
    hinter: HistoryHinter,
}

impl rustyline::completion::Completer for ShrouDBHelper {
    type Candidate = String;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &rustyline::Context<'_>,
    ) -> rustyline::Result<(usize, Vec<String>)> {
        let word_start = line[..pos].rfind(' ').map(|i| i + 1).unwrap_or(0);
        let prefix = &line[word_start..pos];
        let matches: Vec<String> = COMMANDS
            .iter()
            .filter(|c| c.to_uppercase().starts_with(&prefix.to_uppercase()))
            .map(|c| c.to_string())
            .collect();
        Ok((word_start, matches))
    }
}

impl rustyline::hint::Hinter for ShrouDBHelper {
    type Hint = String;

    fn hint(&self, line: &str, pos: usize, ctx: &rustyline::Context<'_>) -> Option<String> {
        self.hinter.hint(line, pos, ctx)
    }
}

impl rustyline::highlight::Highlighter for ShrouDBHelper {}
impl rustyline::validate::Validator for ShrouDBHelper {}
impl rustyline::Helper for ShrouDBHelper {}

// ---------------------------------------------------------------------------
// CLI args
// ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(
    name = "shroudb-cli",
    about = "Interactive client for ShrouDB",
    version
)]
struct Cli {
    /// Connection URI (e.g., shroudb://localhost:6399, shroudb+tls://token@host:6399).
    #[arg(long)]
    uri: Option<String>,

    /// Server host.
    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    /// Server port.
    #[arg(short, long, default_value_t = 6399)]
    port: u16,

    /// Output responses as JSON.
    #[arg(long)]
    json: bool,

    /// Output raw RESP3 wire format.
    #[arg(long)]
    raw: bool,

    /// Connect with TLS.
    #[arg(long)]
    tls: bool,

    /// Execute a single command and exit (non-interactive).
    #[arg(trailing_var_arg = true)]
    command: Vec<String>,
}

#[derive(Clone, Copy)]
enum OutputMode {
    Human,
    Json,
    Raw,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let output_mode = if cli.raw {
        OutputMode::Raw
    } else if cli.json {
        OutputMode::Json
    } else {
        OutputMode::Human
    };

    let (addr, mut conn) = if let Some(ref uri) = cli.uri {
        let config = shroudb_client::parse_uri(uri)?;
        let addr = format!("{}:{}", config.host, config.port);
        let mut conn = if config.tls {
            Connection::connect_tls(&addr).await?
        } else {
            Connection::connect(&addr).await?
        };
        if let Some(token) = &config.auth_token {
            let auth_args = vec!["AUTH".to_string(), token.clone()];
            conn.send_command(&auth_args).await?;
        }
        (addr, conn)
    } else {
        let addr = format!("{}:{}", cli.host, cli.port);
        let conn = if cli.tls {
            Connection::connect_tls(&addr).await?
        } else {
            Connection::connect(&addr).await?
        };
        (addr, conn)
    };

    // Non-interactive: execute single command and exit.
    if !cli.command.is_empty() {
        let response = conn.send_command(&cli.command).await?;
        print_output(&response, output_mode);
        return Ok(());
    }

    // Interactive REPL.
    let version = env!("CARGO_PKG_VERSION");
    println!("shroudb-cli v{version} \u{2014} connected to {addr}");
    println!("Type 'help' for commands, Ctrl-C to exit.");
    println!();

    let config = rustyline::Config::builder().auto_add_history(true).build();
    let helper = ShrouDBHelper {
        hinter: HistoryHinter::new(),
    };
    let mut rl = rustyline::Editor::with_config(config)?;
    rl.set_helper(Some(helper));

    let history_path = dirs_home().join(".shroudb_history");
    if let Err(e) = rl.load_history(&history_path)
        && !matches!(e, ReadlineError::Io(_))
    {
        eprintln!("warning: could not load history: {e}");
    }

    loop {
        match rl.readline("shroudb> ") {
            Ok(line) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }

                if let Some(cmd) = line
                    .strip_prefix("help ")
                    .or_else(|| line.strip_prefix("HELP "))
                {
                    print_command_help(cmd.trim());
                    continue;
                }

                if line.eq_ignore_ascii_case("help") {
                    print_help();
                    continue;
                }
                if line.eq_ignore_ascii_case("quit") || line.eq_ignore_ascii_case("exit") {
                    break;
                }

                let args = shell_words(line);
                match conn.send_command(&args).await {
                    Ok(response) => print_output(&response, output_mode),
                    Err(e) => eprintln!("error: {e}"),
                }
            }
            Err(ReadlineError::Interrupted) => break,
            Err(ReadlineError::Eof) => break,
            Err(e) => {
                eprintln!("readline error: {e}");
                break;
            }
        }
    }

    let _ = rl.save_history(&history_path);
    Ok(())
}

fn print_output(resp: &Response, mode: OutputMode) {
    match mode {
        OutputMode::Human => resp.print(0),
        OutputMode::Json => {
            let json_val = resp.to_json();
            match serde_json::to_string_pretty(&json_val) {
                Ok(s) => println!("{s}"),
                Err(e) => eprintln!("(JSON serialization error: {e})"),
            }
        }
        OutputMode::Raw => {
            let raw = resp.to_raw();
            print!("{raw}");
        }
    }
}

fn dirs_home() -> std::path::PathBuf {
    std::env::var("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
}

/// Split a line into words, respecting double-quoted and single-quoted strings.
fn shell_words(input: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut chars = input.chars().peekable();

    #[derive(PartialEq)]
    enum State {
        Normal,
        DoubleQuote,
        SingleQuote,
    }

    let mut state = State::Normal;

    while let Some(ch) = chars.next() {
        match state {
            State::Normal => match ch {
                '"' => state = State::DoubleQuote,
                '\'' => state = State::SingleQuote,
                ' ' | '\t' => {
                    if !current.is_empty() {
                        words.push(std::mem::take(&mut current));
                    }
                }
                _ => current.push(ch),
            },
            State::DoubleQuote => match ch {
                '"' => state = State::Normal,
                '\\' => {
                    if let Some(&next) = chars.peek() {
                        if next == '"' || next == '\\' {
                            current.push(chars.next().unwrap());
                        } else {
                            current.push(ch);
                        }
                    } else {
                        current.push(ch);
                    }
                }
                _ => current.push(ch),
            },
            State::SingleQuote => match ch {
                '\'' => state = State::Normal,
                _ => current.push(ch),
            },
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

fn print_help() {
    println!(
        r#"
Commands:

  Data Operations
    PUT <ns> <key> <value>
    PUT <ns> <key> VALUE <value> META <json>
    GET <ns> <key> [VERSION <n>] [META]
    DELETE <ns> <key>
    LIST <ns> [PREFIX <prefix>] [CURSOR <cursor>] [LIMIT <n>]
    VERSIONS <ns> <key> [LIMIT <n>] [FROM <version>]

  Namespace Operations
    NAMESPACE CREATE <name> [SCHEMA <json>] [MAX_VERSIONS <n>] [TOMBSTONE_RETENTION <secs>]
    NAMESPACE DROP <name> [FORCE]
    NAMESPACE LIST [CURSOR <cursor>] [LIMIT <n>]
    NAMESPACE INFO <name>
    NAMESPACE ALTER <name> [SCHEMA <json>] [MAX_VERSIONS <n>] [TOMBSTONE_RETENTION <secs>]
    NAMESPACE VALIDATE <name>

  Operational
    AUTH <token>
    PING
    HEALTH
    CONFIG GET <key>
    CONFIG SET <key> <value>
    COMMAND LIST

  Other
    help [<command>]   Show help for a specific command
    quit / exit        Disconnect
"#
    );
}

fn print_command_help(cmd: &str) {
    match cmd.to_uppercase().as_str() {
        "PUT" => println!(
            r#"PUT <namespace> <key> <value>
PUT <namespace> <key> VALUE <value> META <json> TTL <ms>

  Store a value at the given key. Auto-increments the version.

  TTL  Optional expiry in milliseconds. The entry is auto-deleted
       at or after (server wall clock + TTL). Entry-level; a later
       PUT/PUTIF without TTL writes a TTL-less entry — TTL is NOT
       inherited across writes.

  Example:
    PUT myapp.users user:1 alice
    PUT myapp.users user:1 VALUE alice META '{{"role":"admin"}}'
    PUT myapp.sessions sess:abc token TTL 3600000
"#
        ),
        "GET" => println!(
            r#"GET <namespace> <key> [VERSION <n>] [META]

  Retrieve the value at a key. Returns the latest version by default.

  VERSION  Fetch a specific version.
  META     Include metadata in the response.

  Example:
    GET myapp.users user:1
    GET myapp.users user:1 VERSION 1 META
"#
        ),
        "DELETE" => println!(
            r#"DELETE <namespace> <key>

  Delete a key by writing a tombstone. The key's version history
  is preserved and accessible via VERSIONS.

  Example:
    DELETE myapp.sessions sess:abc
"#
        ),
        "PUTIF" => println!(
            r#"PUTIF <namespace> <key> <value> EXPECT <version> [META <json>]

  Compare-and-swap PUT. Writes the entry only if the key's current
  active version matches EXPECT. On mismatch returns:

    -VERSIONCONFLICT current=<actual>

  so the caller can retry without re-reading the key first.

  EXPECT 0 means "key must not exist or must be tombstoned"
  (insert-or-resurrect). EXPECT N > 0 means "current version
  must equal N" (strict update; resurrection from a matching
  tombstone version also works).

  Example:
    PUTIF myapp.users user:1 alice EXPECT 0
    PUTIF myapp.users user:1 alice-updated EXPECT 3
"#
        ),
        "DELIF" => println!(
            r#"DELIF <namespace> <key> EXPECT <version>

  Compare-and-swap DELETE. Writes a tombstone only if the key's
  current active version matches EXPECT. On mismatch returns:

    -VERSIONCONFLICT current=<actual>

  A missing or already-tombstoned key returns -NOTFOUND regardless
  of EXPECT.

  Example:
    DELIF myapp.sessions sess:abc EXPECT 2
"#
        ),
        "DELPREFIX" => println!(
            r#"DELPREFIX <namespace> <prefix>

  Tombstone all active keys in the namespace whose byte
  representation starts with <prefix>. Returns {{deleted: <count>}}.

  Empty prefix is rejected — use NAMESPACE DROP for full teardown.
  If the prefix matches more than the configured per-call cap
  (default 100,000), returns:

    -PREFIXTOOLARGE matched=<n> limit=<m>

  and no keys are deleted. The caller refines the prefix and
  retries.

  Example:
    DELPREFIX myapp.sessions user:alice:
"#
        ),
        "LIST" => println!(
            r#"LIST <namespace> [PREFIX <prefix>] [CURSOR <cursor>] [LIMIT <n>]

  List active keys in a namespace. Tombstoned keys are excluded.

  Example:
    LIST myapp.users PREFIX user: LIMIT 50
"#
        ),
        "VERSIONS" => println!(
            r#"VERSIONS <namespace> <key> [LIMIT <n>] [FROM <version>]

  Show version history for a key, including tombstones.
  Most recent versions first. Does not include values.

  Example:
    VERSIONS myapp.users user:1 LIMIT 5
"#
        ),
        "NAMESPACE" => println!(
            r#"NAMESPACE CREATE <name> [SCHEMA <json>] [MAX_VERSIONS <n>] [TOMBSTONE_RETENTION <secs>]
NAMESPACE DROP <name> [FORCE]
NAMESPACE LIST [CURSOR <cursor>] [LIMIT <n>]
NAMESPACE INFO <name>
NAMESPACE ALTER <name> [SCHEMA <json>] [MAX_VERSIONS <n>]
NAMESPACE VALIDATE <name>

  Manage namespaces. CREATE and DROP require admin access.
  VALIDATE checks existing entries against the current schema.

  Example:
    NAMESPACE CREATE myapp.users
    NAMESPACE INFO myapp.users
    NAMESPACE DROP temp FORCE
"#
        ),
        "AUTH" => println!(
            r#"AUTH <token>

  Authenticate the connection. Required before any data command
  when the server has auth enabled.

  Example:
    AUTH my-secret-token
"#
        ),
        "CONFIG" => println!(
            r#"CONFIG GET <key>
CONFIG SET <key> <value>

  Get or set runtime configuration. CONFIG SET requires admin access.

  Example:
    CONFIG GET max_connections
"#
        ),
        "HEALTH" => println!(
            r#"HEALTH

  Check server health status.
"#
        ),
        "PING" => println!(
            r#"PING

  Test connectivity. Returns PONG.
"#
        ),
        _ => println!("Unknown command: {cmd}. Type 'help' for all commands."),
    }
}
