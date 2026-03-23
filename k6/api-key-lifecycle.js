// k6 scenario: API key full lifecycle
// Run: k6 run k6/api-key-lifecycle.js
//
// Requires Keyva running with an "api-keys" keyspace:
//   cargo run -- --config config.example.toml

import http from "k6/http";
import { check, sleep } from "k6";
import { Rate, Trend } from "k6/metrics";

const BASE = __ENV.KEYVA_REST_URL || "http://localhost:8080";

const issueErrors = new Rate("issue_errors");
const verifyErrors = new Rate("verify_errors");
const issueDuration = new Trend("issue_duration");
const verifyDuration = new Trend("verify_duration");

export const options = {
  scenarios: {
    lifecycle: {
      executor: "ramping-vus",
      startVUs: 1,
      stages: [
        { duration: "10s", target: 10 },
        { duration: "30s", target: 10 },
        { duration: "10s", target: 0 },
      ],
    },
  },
  thresholds: {
    issue_errors: ["rate<0.01"],
    verify_errors: ["rate<0.01"],
    issue_duration: ["p(95)<200"],
    verify_duration: ["p(95)<50"],
  },
};

export default function () {
  // Issue
  const issueRes = http.post(
    `${BASE}/v1/service-keys/issue`,
    JSON.stringify({}),
    { headers: { "Content-Type": "application/json" } }
  );
  issueDuration.add(issueRes.timings.duration);

  const issueOk = check(issueRes, {
    "issue status 200": (r) => r.status === 200,
    "issue has api_key": (r) => JSON.parse(r.body).api_key !== undefined,
    "issue has credential_id": (r) =>
      JSON.parse(r.body).credential_id !== undefined,
  });
  issueErrors.add(!issueOk);

  if (!issueOk) return;

  const { api_key, credential_id } = JSON.parse(issueRes.body);

  // Verify
  const verifyRes = http.post(
    `${BASE}/v1/service-keys/verify`,
    JSON.stringify({ token: api_key }),
    { headers: { "Content-Type": "application/json" } }
  );
  verifyDuration.add(verifyRes.timings.duration);

  const verifyOk = check(verifyRes, {
    "verify status 200": (r) => r.status === 200,
    "verify credential matches": (r) =>
      JSON.parse(r.body).credential_id === credential_id,
  });
  verifyErrors.add(!verifyOk);

  // Inspect
  const inspectRes = http.get(
    `${BASE}/v1/service-keys/inspect/${credential_id}`
  );
  check(inspectRes, {
    "inspect status 200": (r) => r.status === 200,
  });

  sleep(0.1);
}
