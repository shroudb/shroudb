// k6 scenario: Mixed realistic workload
// Run: k6 run k6/mixed-workload.js
//
// Simulates a real application's credential usage pattern:
// - 70% VERIFY (read-heavy, hot path)
// - 15% ISSUE (new credentials)
// - 10% INSPECT (metadata lookups)
// - 5% REVOKE (cleanup)
//
// This is the scenario closest to production behavior.

import http from "k6/http";
import { check, sleep } from "k6";
import { Rate, Trend, Counter } from "k6/metrics";
import { SharedArray } from "k6/data";

const BASE = __ENV.SHROUDB_REST_URL || "http://localhost:8080";

const errorRate = new Rate("errors");
const verifyLatency = new Trend("verify_latency");
const issueLatency = new Trend("issue_latency");
const opsCounter = new Counter("total_ops");

export const options = {
  scenarios: {
    mixed: {
      executor: "ramping-arrival-rate",
      startRate: 10,
      timeUnit: "1s",
      stages: [
        { duration: "10s", target: 50 },
        { duration: "30s", target: 100 },
        { duration: "10s", target: 200 },
        { duration: "30s", target: 200 },
        { duration: "10s", target: 0 },
      ],
      preAllocatedVUs: 50,
      maxVUs: 200,
    },
  },
  thresholds: {
    errors: ["rate<0.05"],
    verify_latency: ["p(99)<200"],
    issue_latency: ["p(99)<500"],
  },
};

// Shared pool of issued keys (populated during the test)
const keys = [];
const headers = { "Content-Type": "application/json" };

export default function () {
  const roll = Math.random();
  opsCounter.add(1);

  if (roll < 0.15 || keys.length === 0) {
    // ISSUE (15% — or always if no keys yet)
    const res = http.post(
      `${BASE}/v1/service-keys/issue`,
      JSON.stringify({ metadata: { vu: __VU, iter: __ITER } }),
      { headers }
    );
    issueLatency.add(res.timings.duration);

    const ok = check(res, {
      "issue ok": (r) => r.status === 200,
    });
    errorRate.add(!ok);

    if (ok) {
      const body = JSON.parse(res.body);
      keys.push({
        api_key: body.api_key,
        credential_id: body.credential_id,
      });
    }
  } else if (roll < 0.85) {
    // VERIFY (70%)
    const key = keys[Math.floor(Math.random() * keys.length)];
    if (!key) return;

    const res = http.post(
      `${BASE}/v1/service-keys/verify`,
      JSON.stringify({ token: key.api_key }),
      { headers }
    );
    verifyLatency.add(res.timings.duration);

    const ok = check(res, {
      "verify ok": (r) => r.status === 200 || r.status === 404,
    });
    errorRate.add(!ok);
  } else if (roll < 0.95) {
    // INSPECT (10%)
    const key = keys[Math.floor(Math.random() * keys.length)];
    if (!key) return;

    const res = http.get(
      `${BASE}/v1/service-keys/inspect/${key.credential_id}`
    );
    check(res, {
      "inspect ok": (r) => r.status === 200 || r.status === 404,
    });
  } else {
    // REVOKE (5%)
    const idx = Math.floor(Math.random() * keys.length);
    const key = keys[idx];
    if (!key) return;

    const res = http.post(
      `${BASE}/v1/service-keys/revoke`,
      JSON.stringify({ credential_id: key.credential_id }),
      { headers }
    );
    check(res, {
      "revoke ok": (r) => r.status === 200,
    });

    // Remove from pool so we don't verify revoked keys
    keys.splice(idx, 1);
  }

  sleep(0.01);
}
