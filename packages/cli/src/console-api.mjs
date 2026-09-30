// Calls to the Railyard console API. The bearer token goes in a header only.

export class ApiError extends Error {
  constructor(message, status) {
    super(message);
    this.status = status;
  }
}

export function consoleApi({ console: base, token, fetchImpl = fetch }) {
  const url = (p) => `${base.replace(/\/$/, "")}${p}`;
  async function call(method, path, body) {
    const res = await fetchImpl(url(path), {
      method,
      headers: { "content-type": "application/json", ...(token ? { authorization: `Bearer ${token}` } : {}) },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    const json = await res.json().catch(() => ({}));
    if (!res.ok) throw new ApiError(json.error_description || json.error || `Railyard answered ${res.status}`, res.status);
    return json;
  }
  return {
    deviceCode: () => call("POST", "/api/v1/device/code"),
    deviceToken: (deviceCode) =>
      fetchImpl(url("/api/v1/device/token"), { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ device_code: deviceCode }) }).then(async (r) => ({ status: r.status, body: await r.json().catch(() => ({})) })),
    recordDeploy: (d) => call("POST", "/api/v1/deploys", d),
    startLink: (repo, app) => call("POST", "/api/v1/github/link", { repo, app }),
    linkStatus: (id) => call("GET", `/api/v1/github/link/status?id=${encodeURIComponent(id)}`),
    ciSession: () => call("POST", "/api/v1/github/ci/session"),
    ciReport: (r) => call("POST", "/api/v1/github/ci/report", r),
    listDeploys: (app, branch) => call("GET", `/api/v1/deploys?app=${encodeURIComponent(app)}${branch ? `&branch=${encodeURIComponent(branch)}` : ""}`),
  };
}
