//! keyva-cli — interactive command-line client for Keyva.

use clap::Parser;
use keyva_client::Response;
use keyva_client::connection::Connection;
use rustyline::error::ReadlineError;
use rustyline::hint::HistoryHinter;

/// Known command names for tab completion.
const COMMANDS: &[&str] = &[
    "ISSUE",
    "VERIFY",
    "REVOKE",
    "REFRESH",
    "UPDATE",
    "INSPECT",
    "ROTATE",
    "JWKS",
    "KEYSTATE",
    "HEALTH",
    "KEYS",
    "SUSPEND",
    "UNSUSPEND",
    "SCHEMA",
    "AUTH",
    "CONFIG",
    "SUBSCRIBE",
    "help",
    "quit",
    "exit",
];

// ---------------------------------------------------------------------------
// Tab-completion helper
// ---------------------------------------------------------------------------

struct KeyvaHelper {
    hinter: HistoryHinter,
}

impl rustyline::completion::Completer for KeyvaHelper {
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

impl rustyline::hint::Hinter for KeyvaHelper {
    type Hint = String;

    fn hint(&self, line: &str, pos: usize, ctx: &rustyline::Context<'_>) -> Option<String> {
        self.hinter.hint(line, pos, ctx)
    }
}

impl rustyline::highlight::Highlighter for KeyvaHelper {}
impl rustyline::validate::Validator for KeyvaHelper {}
impl rustyline::Helper for KeyvaHelper {}

// ---------------------------------------------------------------------------
// CLI args
// ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(name = "keyva-cli", about = "Interactive client for Keyva", version)]
struct Cli {
    /// Connection URI (e.g., keyva://localhost:6399, keyva+tls://token@host:6399/keyspace).
    /// Overrides --host, --port, and --tls when provided.
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

    /// Output raw RESP3 wire format instead of parsed responses.
    #[arg(long)]
    raw: bool,

    /// Connect with TLS.
    #[arg(long)]
    tls: bool,

    /// Execute a single command and exit (non-interactive).
    #[arg(trailing_var_arg = true)]
    command: Vec<String>,
}

/// Output mode derived from CLI flags.
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
        let config = keyva_client::parse_uri(uri)?;
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
    println!("Connected to keyva at {addr}");
    println!("Type 'help' for command list, 'help <command>' for details, Ctrl-C to exit.\n");

    let config = rustyline::Config::builder().auto_add_history(true).build();
    let helper = KeyvaHelper {
        hinter: HistoryHinter::new(),
    };
    let mut rl = rustyline::Editor::with_config(config)?;
    rl.set_helper(Some(helper));

    let history_path = dirs_home().join(".keyva_history");
    let _ = rl.load_history(&history_path);

    loop {
        match rl.readline("keyva> ") {
            Ok(line) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }

                // Per-command help: "help <command>"
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

/// Print a response in the requested output mode.
fn print_output(resp: &Response, mode: OutputMode) {
    match mode {
        OutputMode::Human => resp.print(0),
        OutputMode::Json => {
            let json_val = resp.to_json();
            println!("{}", serde_json::to_string_pretty(&json_val).unwrap());
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

/// Split a line into words, respecting double-quoted strings.
fn shell_words(input: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;

    for ch in input.chars() {
        match ch {
            '"' => in_quotes = !in_quotes,
            ' ' | '\t' if !in_quotes => {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
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

  Credential Operations
    ISSUE <keyspace> [CLAIMS <json>] [META <json>] [TTL <secs>]
    VERIFY <keyspace> <token> [PAYLOAD <data>] [CHECKREV]
    REVOKE <keyspace> <id>
    REVOKE <keyspace> FAMILY <family_id>
    REFRESH <keyspace> <token>
    UPDATE <keyspace> <credential_id> META <json>
    INSPECT <keyspace> <credential_id>

  Key Management
    ROTATE <keyspace> [FORCE] [NOWAIT] [DRYRUN]
    JWKS <keyspace>
    KEYSTATE <keyspace>

  Operational
    HEALTH [<keyspace>]
    KEYS <keyspace> [COUNT <n>]
    SUSPEND <keyspace> <credential_id>
    UNSUSPEND <keyspace> <credential_id>
    SCHEMA <keyspace>

  Other
    help [<command>]   Show help (optionally for a specific command)
    quit/exit          Disconnect
"#
    );
}

fn print_command_help(cmd: &str) {
    match cmd.to_uppercase().as_str() {
        "ISSUE" => println!(
            r#"ISSUE <keyspace> [CLAIMS <json>] [META <json>] [TTL <secs>] [IDEMPOTENCY_KEY <key>]

  Issue a new credential in the given keyspace.

  CLAIMS     JSON object with JWT claims (JWT keyspaces only).
  META       JSON object with metadata to attach to the credential.
  TTL        Time-to-live in seconds (overrides keyspace default).
  IDEMPOTENCY_KEY  Prevents duplicate issuance for the same key within 5 minutes.

  Example:
    ISSUE tokens CLAIMS '{{"sub":"user123"}}' TTL 3600
"#
        ),
        "VERIFY" => println!(
            r#"VERIFY <keyspace> <token> [PAYLOAD <data>] [CHECKREV]

  Verify a credential (JWT token, API key, or HMAC signature).

  PAYLOAD    For HMAC keyspaces, the original message to verify against.
  CHECKREV   Also check if the credential has been revoked (slower).

  Example:
    VERIFY tokens eyJhbGciOi... CHECKREV
"#
        ),
        "REVOKE" => println!(
            r#"REVOKE <keyspace> <credential_id>
REVOKE <keyspace> FAMILY <family_id>

  Revoke a single credential by ID, or revoke an entire refresh token family.

  Example:
    REVOKE tokens cred_abc123
    REVOKE sessions FAMILY fam_xyz789
"#
        ),
        "REFRESH" => println!(
            r#"REFRESH <keyspace> <token>

  Exchange a refresh token for a new one (refresh token keyspaces only).
  The old token is consumed and a new token is returned.

  Example:
    REFRESH sessions rt_abc123...
"#
        ),
        "UPDATE" => println!(
            r#"UPDATE <keyspace> <credential_id> META <json>

  Update metadata on an existing credential. Merges with existing metadata.
  Immutable fields (if defined in meta_schema) cannot be changed.

  Example:
    UPDATE keys cred_abc123 META '{{"plan":"pro"}}'
"#
        ),
        "INSPECT" => println!(
            r#"INSPECT <keyspace> <credential_id>

  Retrieve full details about a credential including metadata, state, and timestamps.

  Example:
    INSPECT tokens cred_abc123
"#
        ),
        "ROTATE" => println!(
            r#"ROTATE <keyspace> [FORCE] [NOWAIT] [DRYRUN]

  Trigger key rotation for the keyspace. The old key enters drain mode.

  FORCE    Rotate even if the current key has not reached its rotation age.
  NOWAIT   Return immediately without waiting for rotation to complete.
  DRYRUN   Preview what would happen without making changes.

  Example:
    ROTATE tokens FORCE
"#
        ),
        "JWKS" => println!(
            r#"JWKS <keyspace>

  Return the JSON Web Key Set for a JWT keyspace. Includes all active
  and draining public keys.

  Example:
    JWKS tokens
"#
        ),
        "KEYSTATE" => println!(
            r#"KEYSTATE <keyspace>

  Show the current key ring state: active key, draining keys, and
  pre-staged keys with their creation timestamps and status.

  Example:
    KEYSTATE tokens
"#
        ),
        "HEALTH" => println!(
            r#"HEALTH [<keyspace>]

  Check server health. Optionally check a specific keyspace.
  Returns status, engine state, and per-keyspace credential counts.

  Example:
    HEALTH
    HEALTH tokens
"#
        ),
        "KEYS" => println!(
            r#"KEYS <keyspace> [CURSOR <cursor>] [PATTERN <glob>] [STATE <state>] [COUNT <n>]

  List credential IDs in a keyspace with optional filtering and pagination.

  CURSOR   Resume from a previous scan position.
  PATTERN  Filter by credential ID pattern (glob-style).
  STATE    Filter by state: active, suspended, revoked.
  COUNT    Maximum number of results to return (default: 100).

  Example:
    KEYS tokens COUNT 50
"#
        ),
        "SUSPEND" => println!(
            r#"SUSPEND <keyspace> <credential_id>

  Temporarily suspend a credential. Suspended credentials fail verification
  but are not permanently revoked and can be unsuspended.

  Example:
    SUSPEND tokens cred_abc123
"#
        ),
        "UNSUSPEND" => println!(
            r#"UNSUSPEND <keyspace> <credential_id>

  Reactivate a previously suspended credential.

  Example:
    UNSUSPEND tokens cred_abc123
"#
        ),
        "SCHEMA" => println!(
            r#"SCHEMA <keyspace>

  Display the metadata schema for a keyspace, including field types,
  required fields, defaults, and validation constraints.

  Example:
    SCHEMA tokens
"#
        ),
        "AUTH" => println!(
            r#"AUTH <token>

  Authenticate the current connection with a bearer token.
  Must be called before any other command if auth is enabled.

  Example:
    AUTH my-secret-token
"#
        ),
        "CONFIG" => println!(
            r#"CONFIG GET <key>
CONFIG SET <key> <value>

  Get or set runtime configuration values.

  Example:
    CONFIG GET max_connections
    CONFIG SET log_level debug
"#
        ),
        "SUBSCRIBE" => println!(
            r#"SUBSCRIBE <channel>

  Subscribe to real-time event notifications on a channel.

  Example:
    SUBSCRIBE keyspace:tokens
"#
        ),
        _ => println!("Unknown command: {cmd}. Type 'help' for all commands."),
    }
}
