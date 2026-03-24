// k6 scenario: HMAC sign and verify under load
// Run: k6 run k6/hmac-sign-verify.js
//
// Requires ShrouDB running with a "webhooks" HMAC keyspace and at least
// one signing key (ROTATE first).
//
// Note: HMAC ISSUE signs serde_json::to_vec(claims). VERIFY compares
// against payload.as_bytes(). The payload sent to VERIFY must be the
// exact JSON serialization that ISSUE signed.

import http from "k6/http";
import { check, sleep } from "k6";
import { Rate, Trend } from "k6/metrics";

const BASE = __ENV.SHROUDB_REST_URL || "http://localhost:8080";
const KEYSPACE = __ENV.SHROUDB_KEYSPACE || "webhooks";

const signErrors = new Rate("hmac_sign_errors");
const verifyErrors = new Rate("hmac_verify_errors");
const signDuration = new Trend("hmac_sign_duration");
const verifyDuration = new Trend("hmac_verify_duration");

export const options = {
  scenarios: {
    hmac_flow: {
      executor: "constant-arrival-rate",
      rate: 50,
      timeUnit: "1s",
      duration: "30s",
      preAllocatedVUs: 10,
      maxVUs: 30,
    },
  },
  thresholds: {
    hmac_sign_errors: ["rate<0.01"],
    hmac_verify_errors: ["rate<0.01"],
    hmac_sign_duration: ["p(95)<50"],
    hmac_verify_duration: ["p(95)<50"],
  },
};

export default function () {
  const headers = { "Content-Type": "application/json" };

  // The payload to sign. ISSUE serializes claims with serde_json which
  // sorts keys alphabetically (BTreeMap). VERIFY compares against the
  // exact bytes. We must send the same sorted serialization for verify.
  const payloadObj = {
    amount: Math.floor(Math.random() * 10000),
    event: "invoice.paid",
    ts: Date.now(),
  };
  // Keys are already in alphabetical order above to match serde_json output.

  // Sign via ISSUE — claims is the JSON value to sign
  const signRes = http.post(
    `${BASE}/v1/${KEYSPACE}/issue`,
    JSON.stringify({ claims: payloadObj }),
    { headers }
  );
  signDuration.add(signRes.timings.duration);
  const signOk = check(signRes, {
    "sign status 200": (r) => r.status === 200,
    "sign has signature": (r) => JSON.parse(r.body).signature !== undefined,
  });
  signErrors.add(!signOk);
  if (!signOk) return;

  const body = JSON.parse(signRes.body);

  // Verify — payload must match the exact JSON serialization that ISSUE signed.
  // ISSUE signs serde_json::to_vec(claims), which produces compact JSON.
  // We send the same object serialized with JSON.stringify (which is compact).
  const verifyRes = http.post(
    `${BASE}/v1/${KEYSPACE}/verify`,
    JSON.stringify({
      token: body.signature,
      payload: JSON.stringify(payloadObj),
    }),
    { headers }
  );
  verifyDuration.add(verifyRes.timings.duration);
  const verifyOk = check(verifyRes, {
    "verify status 200": (r) => r.status === 200,
  });
  verifyErrors.add(!verifyOk);
}
