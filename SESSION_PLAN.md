# ShrouDB Session — Plan

**Status:** Pre-build

HTTP session management middleware that composes ShrouDB primitives (JWT + refresh tokens + passwords) into a complete auth flow with cookie management. Ships as a library (`@shroudb/session` npm package), not a separate server.

---

## What Session Is

A thin middleware layer that sits between your web framework and ShrouDB. It owns the HTTP contract — cookies, CSRF, OAuth redirects — while delegating all credential operations to ShrouDB over TCP.

**Session is NOT a credential type.** It's a composition pattern over existing ShrouDB keyspaces:
- JWT keyspace → short-lived access tokens
- Refresh token keyspace → silent rotation with reuse detection
- Password keyspace → user authentication

**Session is NOT a server.** It's a middleware library you add to your existing app:

```typescript
import { shroudbSession } from '@shroudb/session';

app.use(shroudbSession({
  shroudb: 'shroudb://localhost:6399',
  jwt: { keyspace: 'sessions', ttl: '15m' },
  refresh: { keyspace: 'refresh', ttl: '30d' },
  passwords: { keyspace: 'users' },
  cookie: { name: 'sid', secure: true, sameSite: 'lax' },
  csrf: true,
}));
```

---

## Why a Library, Not a Service

A separate session service adds a network hop on every request and a deployment to manage. Session validation must be fast — it's on every HTTP request. A middleware library:

- Runs in-process — no extra latency beyond the ShrouDB TCP call
- No extra deployment — it's a dependency, not infrastructure
- Framework-native — integrates with your existing middleware stack
- Configurable per-app — different apps can have different cookie settings

The ShrouDB TCP call (VERIFY) is ~1ms. Adding a service hop would double that for no benefit.

---

## API Surface

### Middleware (automatic on every request)

The middleware runs before your route handlers. It:
1. Extracts the session cookie
2. VERIFYs the JWT against ShrouDB
3. If expired but refresh token present, REFRESHes silently (new cookies)
4. Injects user context into the request (e.g., `req.user`)
5. If no valid session, sets `req.user = null` (doesn't reject — let the route decide)

### Endpoints (mounted by the middleware)

| Endpoint | Method | What it does |
|----------|--------|-------------|
| `POST /auth/signup` | Create user (PASSWORD SET) → create session → set cookies |
| `POST /auth/login` | PASSWORD VERIFY → if valid, create session → set cookies |
| `POST /auth/logout` | REVOKE refresh token → clear cookies |
| `POST /auth/logout-all` | REVOKE FAMILY → clear cookies |
| `POST /auth/refresh` | REFRESH → new cookies (explicit refresh, usually automatic) |
| `POST /auth/password/change` | PASSWORD CHANGE → (session stays valid) |
| `POST /auth/password/reset/request` | Generate reset token (ISSUE) → call webhook/email handler |
| `POST /auth/password/reset/confirm` | VERIFY reset token → PASSWORD SET → REVOKE reset token |
| `GET /auth/csrf` | Return CSRF token |
| `GET /auth/session` | Return current user context (from cookie, no DB call if JWT is valid) |
| `GET /auth/oauth/:provider` | Redirect to OAuth provider |
| `GET /auth/oauth/:provider/callback` | Token exchange → create user if new → create session |

All endpoint paths are configurable (`prefix: '/auth'` by default).

---

## Session Lifecycle

### Signup
```
Client                  Session Middleware           ShrouDB
  │                          │                        │
  │  POST /auth/signup       │                        │
  │  {email, password}       │                        │
  │ ───────────────────────► │                        │
  │                          │  PASSWORD SET           │
  │                          │  (users, email, pass)   │
  │                          │ ──────────────────────► │
  │                          │  ◄───── {credential_id} │
  │                          │                        │
  │                          │  ISSUE JWT              │
  │                          │  (sessions, {sub: cid}) │
  │                          │ ──────────────────────► │
  │                          │  ◄───── {token}         │
  │                          │                        │
  │                          │  ISSUE refresh token    │
  │                          │  (refresh, {sub: cid})  │
  │                          │ ──────────────────────► │
  │                          │  ◄───── {token, fid}    │
  │                          │                        │
  │  Set-Cookie: sid=jwt     │                        │
  │  Set-Cookie: rt=refresh  │                        │
  │  ◄─────────────────────  │                        │
```

### Request Validation (every request)
```
Client                  Session Middleware           ShrouDB
  │                          │                        │
  │  GET /api/resource       │                        │
  │  Cookie: sid=jwt         │                        │
  │ ───────────────────────► │                        │
  │                          │  VERIFY JWT             │
  │                          │  (sessions, jwt)        │
  │                          │ ──────────────────────► │
  │                          │  ◄───── {claims}        │
  │                          │                        │
  │                          │  req.user = claims      │
  │                          │  next()                 │
  │  ◄───── response         │                        │
```

### Silent Refresh (JWT expired, refresh token valid)
```
Client                  Session Middleware           ShrouDB
  │                          │                        │
  │  GET /api/resource       │                        │
  │  Cookie: sid=expired     │                        │
  │  Cookie: rt=refresh      │                        │
  │ ───────────────────────► │                        │
  │                          │  VERIFY JWT → expired   │
  │                          │  REFRESH token          │
  │                          │ ──────────────────────► │
  │                          │  ◄── {new_token, fid}   │
  │                          │                        │
  │                          │  ISSUE new JWT          │
  │                          │ ──────────────────────► │
  │                          │  ◄───── {new_jwt}       │
  │                          │                        │
  │  Set-Cookie: sid=new_jwt │                        │
  │  Set-Cookie: rt=new_rt   │                        │
  │  ◄───── response         │                        │
```

---

## Cookie Strategy

| Cookie | Value | HttpOnly | Secure | SameSite | Path | Max-Age |
|--------|-------|----------|--------|----------|------|---------|
| `sid` | JWT access token | Yes | Yes (prod) | Lax | / | None (session) |
| `rt` | Refresh token | Yes | Yes (prod) | Strict | /auth/refresh | 30 days |
| `csrf` | CSRF token | No (JS needs to read it) | Yes | Strict | / | Session |

- `sid` is a session cookie (no Max-Age) — dies when browser closes unless "remember me" is set
- `rt` has a long Max-Age and restricted Path — only sent to the refresh endpoint
- `csrf` is readable by JavaScript (not HttpOnly) so the app can include it in request headers

**"Remember me":** Sets Max-Age on both `sid` and `rt`. The refresh token TTL in ShrouDB controls the actual session duration.

---

## CSRF Protection

Double-submit cookie pattern:
1. On session creation, set a `csrf` cookie with a random token
2. The client JavaScript reads the cookie and includes it in a `X-CSRF-Token` header
3. The middleware compares the cookie value with the header value
4. They match → request is legitimate (attacker can't read the cookie from another origin)

This works because:
- SameSite=Strict on the CSRF cookie prevents it from being sent on cross-origin requests
- Even if it were sent, the attacker can't read it to put it in the header
- No server-side token storage needed — the cookie IS the token

---

## OAuth Support

The middleware handles the OAuth redirect flow:

1. `GET /auth/oauth/github` → redirect to GitHub with `state` parameter
2. GitHub redirects back to `/auth/oauth/github/callback?code=...&state=...`
3. Middleware exchanges code for GitHub access token
4. Fetches user profile from GitHub API
5. Creates or links user in ShrouDB (PASSWORD SET with a random password, or skip if user exists)
6. Creates session (ISSUE JWT + refresh token)
7. Sets cookies, redirects to app

**Provider configuration:**
```typescript
shroudbSession({
  // ...
  oauth: {
    github: {
      clientId: process.env.GITHUB_CLIENT_ID,
      clientSecret: process.env.GITHUB_CLIENT_SECRET,
      scopes: ['user:email'],
    },
    google: {
      clientId: process.env.GOOGLE_CLIENT_ID,
      clientSecret: process.env.GOOGLE_CLIENT_SECRET,
      scopes: ['email', 'profile'],
    },
  },
});
```

---

## Framework Support

The initial release targets **Hono** (lightweight, runs everywhere — Node, Deno, Bun, Cloudflare Workers).

```typescript
import { Hono } from 'hono';
import { shroudbSession } from '@shroudb/session';

const app = new Hono();
app.use('*', shroudbSession({ /* config */ }));

app.get('/dashboard', (c) => {
  if (!c.get('user')) return c.redirect('/login');
  return c.json({ user: c.get('user') });
});
```

Future adapters: Express, Fastify, Next.js middleware, SvelteKit hooks.

---

## Package Structure

```
shroudb-session/
  package.json        — @shroudb/session
  src/
    index.ts          — exports middleware factory
    middleware.ts      — core middleware logic
    session.ts        — session create/validate/refresh/destroy
    cookies.ts        — cookie get/set/clear helpers
    csrf.ts           — CSRF token generation and validation
    oauth/
      index.ts        — provider registry
      github.ts       — GitHub OAuth flow
      google.ts       — Google OAuth flow
    types.ts          — SessionConfig, UserContext, etc.
  test/
    middleware.test.ts
    session.test.ts
    csrf.test.ts
    oauth.test.ts
```

**Dependencies:**
- `@shroudb/client` — ShrouDB TCP client (already built)
- `hono` — middleware target (peer dependency)
- Zero other runtime deps — cookie parsing is stdlib, OAuth is just HTTP calls

---

## Relationship to better-auth

For meterd specifically, `@shroudb/session` replaces better-auth:

| Concern | better-auth | @shroudb/session |
|---------|-------------|----------------|
| Password hashing | Internal (bcrypt) | ShrouDB (argon2id) |
| Session tokens | Internal (Postgres) | ShrouDB (JWT + refresh) |
| Cookie management | Internal | @shroudb/session middleware |
| OAuth | Built-in providers | @shroudb/session OAuth module |
| Email verification | Built-in | Application concern (use ShrouDB ISSUE for verification tokens) |
| Password reset | Built-in | @shroudb/session endpoint |
| Rate limiting | None | ShrouDB (per-credential lockout) |

The migration path for meterd:
1. Deploy ShrouDB
2. Add `@shroudb/session` to the Next.js app
3. Import existing passwords via `PASSWORD IMPORT`
4. Switch auth routes from better-auth to @shroudb/session
5. Remove better-auth dependency

---

## Build Order

1. **Core middleware** — session validation (VERIFY JWT), user context injection
2. **Login/signup/logout** — PASSWORD VERIFY/SET, ISSUE tokens, set/clear cookies
3. **Silent refresh** — REFRESH + new cookies
4. **CSRF** — double-submit cookie pattern
5. **Password change/reset** — PASSWORD CHANGE, reset token flow
6. **OAuth** — GitHub first, then Google
7. **"Remember me"** — configurable TTLs
8. **Framework adapters** — Express, Next.js, SvelteKit

---

## What This Plan Does NOT Cover

- **Multi-factor authentication.** TOTP/WebAuthn would be a separate module or ShrouDB keyspace type. Not in v1.
- **Role-based access control.** Session tells you WHO the user is. Authorization (what they can do) is the application's responsibility.
- **Email/SMS delivery.** Password reset generates a token; delivering it is the application's concern (webhook, Resend, SendGrid, etc.).
- **User profile management.** Session manages credentials and sessions, not user profiles. Profile data lives in your database.
