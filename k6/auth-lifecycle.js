// k6 scenario: Auth server signup, login, session, refresh, change-password, logout
// Run: k6 run k6/auth-lifecycle.js
//
// Requires keyva-auth running (default: http://localhost:4001).
// Uses the "default" keyspace which is auto-created in dev mode.

import http from "k6/http";
import { check, sleep } from "k6";
import { Rate, Trend, Counter } from "k6/metrics";

const BASE = __ENV.AUTH_URL || "http://localhost:4001";
const KS = __ENV.AUTH_KEYSPACE || "default";

const signupErrors = new Rate("auth_signup_errors");
const loginErrors = new Rate("auth_login_errors");
const sessionErrors = new Rate("auth_session_errors");
const refreshErrors = new Rate("auth_refresh_errors");
const changeErrors = new Rate("auth_change_password_errors");
const logoutErrors = new Rate("auth_logout_errors");

const signupDuration = new Trend("auth_signup_duration");
const loginDuration = new Trend("auth_login_duration");
const sessionDuration = new Trend("auth_session_duration");
const refreshDuration = new Trend("auth_refresh_duration");

export const options = {
  scenarios: {
    auth_flow: {
      executor: "per-vu-iterations",
      vus: 3,
      iterations: 3,
    },
  },
  thresholds: {
    auth_signup_errors: ["rate<0.01"],
    auth_login_errors: ["rate<0.01"],
    auth_session_errors: ["rate<0.01"],
    auth_refresh_errors: ["rate<0.01"],
    auth_change_password_errors: ["rate<0.01"],
    auth_logout_errors: ["rate<0.01"],
    auth_signup_duration: ["p(95)<5000"], // argon2id hashing is intentionally slow
    auth_login_duration: ["p(95)<5000"],
    auth_session_duration: ["p(95)<100"],
    auth_refresh_duration: ["p(95)<500"],
  },
};

export default function () {
  const headers = { "Content-Type": "application/json" };
  const userId = `user-${Date.now()}-${__VU}-${__ITER}`;
  const password = `P@ss-${__VU}-${__ITER}-w0rd`;
  const newPassword = `N3w-${password}`;

  // 1. Signup
  const signupRes = http.post(
    `${BASE}/auth/${KS}/signup`,
    JSON.stringify({ user_id: userId, password }),
    { headers }
  );
  signupDuration.add(signupRes.timings.duration);
  const signupOk = check(signupRes, {
    "signup status 201": (r) => r.status === 201,
    "signup has access_token": (r) =>
      JSON.parse(r.body).access_token !== undefined,
    "signup has refresh_token": (r) =>
      JSON.parse(r.body).refresh_token !== undefined,
    "signup has user_id": (r) =>
      JSON.parse(r.body).user_id === userId,
    "signup sets access cookie": (r) =>
      r.headers["Set-Cookie"] !== undefined,
  });
  signupErrors.add(!signupOk);
  if (!signupOk) return;

  const signupBody = JSON.parse(signupRes.body);
  const accessToken = signupBody.access_token;
  const refreshToken = signupBody.refresh_token;

  // 2. Session — verify the access token
  const sessionRes = http.get(`${BASE}/auth/${KS}/session`, {
    headers: { Authorization: `Bearer ${accessToken}` },
  });
  sessionDuration.add(sessionRes.timings.duration);
  const sessionOk = check(sessionRes, {
    "session status 200": (r) => r.status === 200,
    "session has user_id": (r) =>
      JSON.parse(r.body).user_id === userId,
    "session has claims.sub": (r) =>
      JSON.parse(r.body).claims.sub === userId,
  });
  sessionErrors.add(!sessionOk);

  // 3. Session with invalid token — expect 401
  // Note: k6 auto-sends cookies from prior responses, so we use an explicit
  // invalid Bearer token to override the cookie-based auth.
  const badAuthRes = http.get(`${BASE}/auth/${KS}/session`, {
    headers: { Authorization: "Bearer invalid-token" },
  });
  check(badAuthRes, {
    "session with invalid token returns 401": (r) => r.status === 401,
  });

  // 4. Login with correct password
  const loginRes = http.post(
    `${BASE}/auth/${KS}/login`,
    JSON.stringify({ user_id: userId, password }),
    { headers }
  );
  loginDuration.add(loginRes.timings.duration);
  const loginOk = check(loginRes, {
    "login status 200": (r) => r.status === 200,
    "login has access_token": (r) =>
      JSON.parse(r.body).access_token !== undefined,
    "login has refresh_token": (r) =>
      JSON.parse(r.body).refresh_token !== undefined,
  });
  loginErrors.add(!loginOk);

  // 5. Login with wrong password — expect 401
  const wrongLoginRes = http.post(
    `${BASE}/auth/${KS}/login`,
    JSON.stringify({ user_id: userId, password: "wrong" }),
    { headers }
  );
  check(wrongLoginRes, {
    "wrong password returns 401": (r) => r.status === 401,
  });

  // 6. Refresh the token
  const refreshRes = http.post(`${BASE}/auth/${KS}/refresh`, null, {
    headers: { Authorization: `Bearer ${refreshToken}` },
  });
  refreshDuration.add(refreshRes.timings.duration);
  const refreshOk = check(refreshRes, {
    "refresh status 200": (r) => r.status === 200,
    "refresh has new access_token": (r) =>
      JSON.parse(r.body).access_token !== undefined,
    "refresh has new refresh_token": (r) =>
      JSON.parse(r.body).refresh_token !== undefined,
  });
  refreshErrors.add(!refreshOk);

  // 7. Verify the refreshed access token works for session
  if (refreshOk) {
    const newAccess = JSON.parse(refreshRes.body).access_token;
    const newSessionRes = http.get(`${BASE}/auth/${KS}/session`, {
      headers: { Authorization: `Bearer ${newAccess}` },
    });
    check(newSessionRes, {
      "refreshed token session valid": (r) => r.status === 200,
      "refreshed token has correct sub": (r) =>
        JSON.parse(r.body).user_id === userId,
    });
  }

  // 8. Change password
  if (loginOk) {
    const loginAccess = JSON.parse(loginRes.body).access_token;
    const changeRes = http.post(
      `${BASE}/auth/${KS}/change-password`,
      JSON.stringify({ old_password: password, new_password: newPassword }),
      {
        headers: {
          "Content-Type": "application/json",
          Authorization: `Bearer ${loginAccess}`,
        },
      }
    );
    const changeOk = check(changeRes, {
      "change-password status 200": (r) => r.status === 200,
    });
    changeErrors.add(!changeOk);

    // Verify login with new password works
    if (changeOk) {
      const newLoginRes = http.post(
        `${BASE}/auth/${KS}/login`,
        JSON.stringify({ user_id: userId, password: newPassword }),
        { headers }
      );
      check(newLoginRes, {
        "login with new password succeeds": (r) => r.status === 200,
      });
    }
  }

  // 9. Logout
  const logoutRes = http.post(`${BASE}/auth/${KS}/logout`, null, {
    headers: { Authorization: `Bearer ${refreshToken}` },
  });
  const logoutOk = check(logoutRes, {
    "logout status 200": (r) => r.status === 200,
  });
  logoutErrors.add(!logoutOk);

  // 10. Duplicate signup — expect 409
  const dupRes = http.post(
    `${BASE}/auth/${KS}/signup`,
    JSON.stringify({ user_id: userId, password: "anything" }),
    { headers }
  );
  check(dupRes, {
    "duplicate signup returns 409": (r) => r.status === 409,
  });

  sleep(0.1);
}
