# JWT Lifecycle in Rails — Before and After ShrouDB

## The Problem

Every Rails app that issues JWTs ends up building the same infrastructure:

- Generate and store signing keys
- Rotate keys without breaking existing tokens
- Serve a JWKS endpoint
- Verify tokens on every request
- Handle key expiry, revocation, and clock skew

This is typically spread across models, controllers, initializers, and background jobs — all custom, all fragile.

---

## Without ShrouDB

### Typical Rails JWT setup

**Gemfile:**
```ruby
gem "jwt"
gem "openssl"
```

**Key generation** (rake task or initializer):
```ruby
# lib/tasks/jwt.rake
namespace :jwt do
  task rotate: :environment do
    key = OpenSSL::PKey::EC.generate("prime256v1")
    kid = SecureRandom.uuid

    # Store in database
    SigningKey.create!(
      kid: kid,
      private_key: key.to_pem,
      public_key: key.public_key.to_pem,
      algorithm: "ES256",
      active: true,
      expires_at: 90.days.from_now
    )

    # Mark old keys as draining (but keep them for verification)
    SigningKey.where(active: true).where.not(kid: kid).update_all(active: false)
  end
end
```

**Token issuance:**
```ruby
# app/services/token_service.rb
class TokenService
  def self.issue(user)
    key_record = SigningKey.where(active: true).order(created_at: :desc).first!
    key = OpenSSL::PKey::EC.new(key_record.private_key)

    payload = {
      sub: user.id.to_s,
      iat: Time.now.to_i,
      exp: 15.minutes.from_now.to_i,
      jti: SecureRandom.uuid
    }

    JWT.encode(payload, key, "ES256", { kid: key_record.kid })
  end
end
```

**Token verification:**
```ruby
# app/middleware/jwt_auth.rb
class JwtAuth
  def initialize(app)
    @app = app
  end

  def call(env)
    token = extract_token(env)
    return unauthorized unless token

    begin
      # Decode header to find kid
      header = JWT.decode(token, nil, false).last
      kid = header["kid"]

      # Look up the key (active or draining)
      key_record = SigningKey.find_by(kid: kid)
      return unauthorized unless key_record

      key = OpenSSL::PKey::EC.new(key_record.public_key)
      payload = JWT.decode(token, key, true, {
        algorithm: "ES256",
        verify_expiration: true,
        leeway: 30
      }).first

      env["current_user_id"] = payload["sub"]
      @app.call(env)
    rescue JWT::DecodeError, JWT::ExpiredSignature => e
      unauthorized
    end
  end

  private

  def extract_token(env)
    auth = env["HTTP_AUTHORIZATION"]
    auth&.start_with?("Bearer ") ? auth[7..] : nil
  end

  def unauthorized
    [401, { "Content-Type" => "application/json" }, ['{"error":"unauthorized"}']]
  end
end
```

**JWKS endpoint:**
```ruby
# app/controllers/jwks_controller.rb
class JwksController < ApplicationController
  skip_before_action :authenticate!

  def index
    keys = SigningKey.where("expires_at > ?", Time.current).map do |record|
      key = OpenSSL::PKey::EC.new(record.public_key)
      {
        kty: "EC",
        crv: "P-256",
        kid: record.kid,
        use: "sig",
        alg: "ES256",
        x: Base64.urlsafe_encode64(key.public_key.to_bn(:uncompressed).to_s(2)[1..32], padding: false),
        y: Base64.urlsafe_encode64(key.public_key.to_bn(:uncompressed).to_s(2)[33..64], padding: false),
      }
    end

    render json: { keys: keys }
  end
end
```

**Key rotation** (cron or Sidekiq periodic):
```ruby
# app/jobs/key_rotation_job.rb
class KeyRotationJob < ApplicationJob
  def perform
    expiring = SigningKey.where(active: true).where("expires_at < ?", 7.days.from_now)
    return unless expiring.exists?

    Rake::Task["jwt:rotate"].invoke
    Rails.logger.info("JWT signing key rotated")
  end
end
```

**Migration:**
```ruby
class CreateSigningKeys < ActiveRecord::Migration[7.1]
  def change
    create_table :signing_keys do |t|
      t.string :kid, null: false, index: { unique: true }
      t.text :private_key, null: false  # encrypted at rest?
      t.text :public_key, null: false
      t.string :algorithm, null: false, default: "ES256"
      t.boolean :active, null: false, default: true
      t.datetime :expires_at, null: false
      t.timestamps
    end
  end
end
```

### What's wrong with this

- **Private keys in your database.** Even with application-level encryption, your DB backup now contains signing keys. Compromise the backup, forge any token.
- **Rotation is manual.** Someone has to write the cron job, handle the drain period, clean up expired keys. Get it wrong and you break all active sessions.
- **JWKS endpoint is hand-rolled.** The JWK coordinate extraction code above is brittle and easy to get wrong (and most examples on the internet are wrong for EC keys).
- **No revocation.** To revoke a token before expiry you need a blocklist — another table, another lookup on every request, another thing to expire.
- **Every app rebuilds this.** Switch from Rails to Phoenix? Start over. Run a second service? Build it again.
- **Verification hits the database.** Every request does a `SigningKey.find_by(kid:)` query.

---

## With ShrouDB

### Setup

```bash
# Start ShrouDB (one binary, zero dependencies)
docker run -d -p 6399:6399 -p 8080:8080 ghcr.io/shroudb/shroudb:latest
```

```toml
# config.toml
[keyspaces.auth-tokens]
type = "jwt"
algorithm = "ES256"
rotation_days = 90
drain_days = 30
default_ttl = "15m"
```

### Token issuance

```ruby
# app/services/token_service.rb
class TokenService
  def self.issue(user)
    response = HTTP.post("http://shroudb:8080/v1/auth-tokens/issue", json: {
      claims: { sub: user.id.to_s }
    })

    response.parse["token"]
  end
end
```

That's it. ShrouDB:
- Picks the active signing key
- Sets `iat`, `exp`, `kid` automatically
- Returns a signed JWT

### Token verification

```ruby
# app/middleware/jwt_auth.rb
class JwtAuth
  def initialize(app)
    @app = app
  end

  def call(env)
    token = extract_token(env)
    return unauthorized unless token

    response = HTTP.post("http://shroudb:8080/v1/auth-tokens/verify", json: {
      token: token
    })

    if response.status.ok?
      claims = response.parse["claims"]
      env["current_user_id"] = claims["sub"]
      @app.call(env)
    else
      unauthorized
    end
  end

  private

  def extract_token(env)
    auth = env["HTTP_AUTHORIZATION"]
    auth&.start_with?("Bearer ") ? auth[7..] : nil
  end

  def unauthorized
    [401, { "Content-Type" => "application/json" }, ['{"error":"unauthorized"}']]
  end
end
```

Or skip the middleware entirely and verify with the JWKS endpoint — standard JWT libraries can validate against it:

```ruby
# Alternative: verify locally using ShrouDB's JWKS endpoint
require "jwt"
require "net/http"
require "json"

class JwtAuth
  JWKS_URL = "http://shroudb:8080/v1/auth-tokens/jwks"
  JWKS_CACHE_TTL = 3600

  def initialize(app)
    @app = app
    @jwks = nil
    @jwks_fetched_at = 0
  end

  def call(env)
    token = extract_token(env)
    return unauthorized unless token

    jwks = fetch_jwks
    payload = JWT.decode(token, nil, true, {
      algorithms: ["ES256"],
      jwks: jwks
    }).first

    env["current_user_id"] = payload["sub"]
    @app.call(env)
  rescue JWT::DecodeError
    unauthorized
  end

  private

  def fetch_jwks
    if @jwks.nil? || Time.now.to_i - @jwks_fetched_at > JWKS_CACHE_TTL
      response = Net::HTTP.get(URI(JWKS_URL))
      @jwks = JSON.parse(response)
      @jwks_fetched_at = Time.now.to_i
    end
    @jwks
  end

  def extract_token(env)
    auth = env["HTTP_AUTHORIZATION"]
    auth&.start_with?("Bearer ") ? auth[7..] : nil
  end

  def unauthorized
    [401, { "Content-Type" => "application/json" }, ['{"error":"unauthorized"}']]
  end
end
```

### JWKS endpoint

Already built in. Point any JWT consumer at:

```
http://shroudb:8080/v1/auth-tokens/jwks
```

Returns a standards-compliant JWKS with cache headers. No code to write.

### Key rotation

Already built in. ShrouDB rotates automatically based on `rotation_days`. The lifecycle:

```
New key created (STAGED, 7 days before needed)
     ↓
Old key stops signing (DRAINING, new key becomes ACTIVE)
     ↓
Old key still verifies (drain_days = 30)
     ↓
Old key removed (RETIRED)
```

No cron jobs. No rake tasks. No coordination. Existing tokens signed by the old key continue to verify during the drain period.

### Revocation

```ruby
# Revoke a single token
HTTP.post("http://shroudb:8080/v1/auth-tokens/revoke", json: {
  credential_id: credential_id
})
```

No blocklist table. ShrouDB tracks revoked tokens in memory with TTL-based expiry.

---

## Side by Side

| | **Rails + jwt gem** | **Rails + ShrouDB** |
|---|---|---|
| **Key storage** | Database (private keys in your DB) | ShrouDB (encrypted WAL, mlock'd memory) |
| **Key rotation** | Manual cron job + custom drain logic | Automatic (`rotation_days = 90`) |
| **JWKS endpoint** | Hand-rolled controller + JWK math | Built-in (`/v1/{ks}/jwks`) |
| **Token issuance** | ~20 lines (load key, build payload, encode) | 3 lines (POST to ShrouDB) |
| **Token verification** | ~30 lines (find key by kid, decode, handle errors) | 5 lines (POST to ShrouDB) or local JWKS verify |
| **Revocation** | Build a blocklist table + middleware check | `POST /v1/{ks}/revoke` |
| **Clock skew** | Hope you remembered to add `leeway:` | Built-in (configurable, default 30s) |
| **Algorithm support** | Whatever you implement | ES256, ES384, RS256, RS384, RS512, EdDSA |
| **Multiple apps** | Rebuild for each app/language | Same ShrouDB instance, different keyspaces |
| **Lines of code** | ~150+ (service, middleware, controller, job, migration) | ~15 (two HTTP calls) |

### What you delete from your Rails app

- `SigningKey` model + migration
- `TokenService` (or equivalent)
- JWKS controller
- `KeyRotationJob`
- Key generation rake task
- The `jwt` and `openssl` gems (if only used for signing keys)

### What you keep

- Your user model (ShrouDB doesn't own users)
- Your business logic
- Your authorization rules (ShrouDB handles authentication primitives, not authorization)

---

## API Keys Too

The same ShrouDB instance handles API keys — no separate infrastructure:

```ruby
# Issue an API key for a user
response = HTTP.post("http://shroudb:8080/v1/service-keys/issue", json: {
  metadata: { user_id: user.id.to_s, plan: "pro" }
})
api_key = response.parse["api_key"]  # "sk_7Kj2mN..."

# Verify an incoming API key
response = HTTP.post("http://shroudb:8080/v1/service-keys/verify", json: {
  token: params[:api_key]
})
if response.status.ok?
  metadata = response.parse["metadata"]
  # metadata["user_id"], metadata["plan"], etc.
end
```

No `ApiKey` model. No SHA-256 hashing code. No prefix generation. One config block:

```toml
[keyspaces.service-keys]
type = "api_key"
prefix = "sk"
hash_algorithm = "sha256"
```

---

## Refresh Tokens Too

```ruby
# Issue a refresh token alongside the JWT
refresh = HTTP.post("http://shroudb:8080/v1/sessions/issue", json: {
  metadata: { sub: user.id.to_s }
})
refresh_token = refresh.parse["token"]

# Later: exchange for a new token
new_refresh = HTTP.post("http://shroudb:8080/v1/sessions/refresh", json: {
  token: refresh_token
})
# Returns new token; old one is consumed (single-use)
# Reuse of the old token → entire family revoked (theft detection)
```

```toml
[keyspaces.sessions]
type = "refresh_token"
token_ttl = "30d"
max_chain_length = 100
family_ttl = "90d"
```

Family-based reuse detection, chain limiting, and automatic revocation — built in.

---

## The Point

Rails is great at business logic. It shouldn't also be a credential management system. The `jwt` gem gives you encoding and decoding. Everything else — key storage, rotation, JWKS, revocation, drain periods — is on you.

ShrouDB is one process that handles all of it. Your Rails app calls it over HTTP and gets back tokens. When you add a Phoenix service next quarter, it calls the same ShrouDB instance. When you need API keys, you add a config block. When you need to rotate keys, it already happened.

```ruby
# Your entire JWT integration
HTTP.post("http://shroudb:8080/v1/auth-tokens/issue", json: { claims: { sub: user.id.to_s } })
HTTP.post("http://shroudb:8080/v1/auth-tokens/verify", json: { token: token })
```

Two HTTP calls. No gems. No migrations. No cron jobs.
