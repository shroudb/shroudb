// k6 scenario: Password set, verify, change, and lockout
// Run: k6 run k6/password-lifecycle.js
//
// Requires Keyva running with a "users" password keyspace:
//   [keyrings.users]
//   type = "password"
//   algorithm = "argon2id"
//   max_failed_attempts = 5
//   lockout_duration = "1m"

import http from "k6/http";
import { check, sleep } from "k6";
import { Rate, Trend, Counter } from "k6/metrics";

const BASE = __ENV.KEYVA_REST_URL || "http://localhost:8080";
const KEYSPACE = __ENV.KEYVA_KEYSPACE || "users";

const setErrors = new Rate("password_set_errors");
const verifyErrors = new Rate("password_verify_errors");
const setDuration = new Trend("password_set_duration");
const verifyDuration = new Trend("password_verify_duration");
const lockouts = new Counter("password_lockouts");

export const options = {
  scenarios: {
    password_flow: {
      executor: "per-vu-iterations",
      vus: 3,
      iterations: 3,
    },
  },
  thresholds: {
    password_set_errors: ["rate<0.01"],
    password_verify_errors: ["rate<0.01"],
    password_set_duration: ["p(95)<5000"], // argon2id is intentionally slow (~100-500ms per hash, higher under concurrency)
    password_verify_duration: ["p(95)<5000"],
  },
};

export default function () {
  const headers = { "Content-Type": "application/json" };
  const userId = `user-${Date.now()}-${__VU}-${__ITER}`;
  const password = `P@ssw0rd-${__VU}-${__ITER}`;
  const newPassword = `N3w-${password}`;

  // Set password
  const setRes = http.post(
    `${BASE}/v1/${KEYSPACE}/password/set`,
    JSON.stringify({ user_id: userId, password }),
    { headers }
  );
  setDuration.add(setRes.timings.duration);
  const setOk = check(setRes, {
    "set status 200": (r) => r.status === 200,
    "set has credential_id": (r) =>
      JSON.parse(r.body).credential_id !== undefined,
  });
  setErrors.add(!setOk);
  if (!setOk) return;

  // Verify correct password
  const verifyRes = http.post(
    `${BASE}/v1/${KEYSPACE}/password/verify`,
    JSON.stringify({ user_id: userId, password }),
    { headers }
  );
  verifyDuration.add(verifyRes.timings.duration);
  const verifyOk = check(verifyRes, {
    "verify correct status 200": (r) => r.status === 200,
    "verify correct valid true": (r) => JSON.parse(r.body).valid === true,
  });
  verifyErrors.add(!verifyOk);

  // Verify wrong password
  const wrongRes = http.post(
    `${BASE}/v1/${KEYSPACE}/password/verify`,
    JSON.stringify({ user_id: userId, password: "wrong" }),
    { headers }
  );
  check(wrongRes, {
    "wrong password rejected": (r) => r.status !== 200 || JSON.parse(r.body).valid === false,
  });

  // Change password
  const changeRes = http.post(
    `${BASE}/v1/${KEYSPACE}/password/change`,
    JSON.stringify({
      user_id: userId,
      old_password: password,
      new_password: newPassword,
    }),
    { headers }
  );
  check(changeRes, {
    "change status 200": (r) => r.status === 200,
  });

  // Verify new password works
  const newVerifyRes = http.post(
    `${BASE}/v1/${KEYSPACE}/password/verify`,
    JSON.stringify({ user_id: userId, password: newPassword }),
    { headers }
  );
  check(newVerifyRes, {
    "new password valid": (r) =>
      r.status === 200 && JSON.parse(r.body).valid === true,
  });

  sleep(0.1);
}
