// k6 scenario: Refresh token rotation chain
// Run: k6 run k6/refresh-token-rotation.js
//
// Tests the refresh token lifecycle: issue → refresh → refresh → ...
// Verifies chain integrity and reuse detection under concurrent load.

import http from "k6/http";
import { check, sleep } from "k6";
import { Rate, Counter } from "k6/metrics";

const BASE = __ENV.SHROUDB_REST_URL || "http://localhost:8080";
const KEYSPACE = __ENV.SHROUDB_KEYSPACE || "sessions";

const refreshErrors = new Rate("refresh_errors");
const reuseDetections = new Counter("reuse_detections");

export const options = {
  scenarios: {
    refresh_chains: {
      executor: "per-vu-iterations",
      vus: 10,
      iterations: 5,
    },
  },
  thresholds: {
    refresh_errors: ["rate<0.05"],
  },
};

export default function () {
  const headers = { "Content-Type": "application/json" };

  // Issue initial token
  const issueRes = http.post(
    `${BASE}/v1/${KEYSPACE}/issue`,
    JSON.stringify({}),
    { headers }
  );

  const issueOk = check(issueRes, {
    "issue status 200": (r) => r.status === 200,
    "issue has token": (r) => JSON.parse(r.body).token !== undefined,
  });

  if (!issueOk) {
    refreshErrors.add(1);
    return;
  }

  let token = JSON.parse(issueRes.body).token;
  const familyId = JSON.parse(issueRes.body).family_id;

  // Rotate through 3 refreshes
  for (let i = 0; i < 3; i++) {
    const refreshRes = http.post(
      `${BASE}/v1/${KEYSPACE}/refresh`,
      JSON.stringify({ token }),
      { headers }
    );

    const refreshOk = check(refreshRes, {
      [`refresh ${i + 1} status 200`]: (r) => r.status === 200,
      [`refresh ${i + 1} new token`]: (r) =>
        JSON.parse(r.body).token !== undefined,
      [`refresh ${i + 1} same family`]: (r) =>
        JSON.parse(r.body).family_id === familyId,
    });

    if (!refreshOk) {
      refreshErrors.add(1);
      return;
    }

    const oldToken = token;
    token = JSON.parse(refreshRes.body).token;

    // Verify old token is consumed (should fail)
    const verifyOld = http.post(
      `${BASE}/v1/${KEYSPACE}/verify`,
      JSON.stringify({ token: oldToken }),
      { headers }
    );

    check(verifyOld, {
      "old token rejected": (r) => r.status !== 200,
    });

    sleep(0.05);
  }

  // Attempt reuse of the first token (should trigger family revocation)
  const firstToken = JSON.parse(issueRes.body).token;
  const reuseRes = http.post(
    `${BASE}/v1/${KEYSPACE}/refresh`,
    JSON.stringify({ token: firstToken }),
    { headers }
  );

  if (reuseRes.status !== 200) {
    reuseDetections.add(1);
  }

  check(reuseRes, {
    "reuse detected": (r) => r.status !== 200,
  });
}
