# Employee identity lookup from a WASM plugin

Plugins execute after traffic completes, in the background trace workers. The host now provides an optional HTTP GET import; WASI and unrestricted sockets are not enabled. The actual employee lookup URL and JSON schema depend on your enterprise gateway. `/v1/whoami` below is an example, not a standard LLM API endpoint.

```toml
[[plugins]]
name = "employee-identity"
wasm_path = "plugins/employee-identity.wasm"
hooks = ["on_request_start"]
timeout_ms = 1000
http_get_urls = ["https://llm.example.com/v1/whoami"]
```

The allowlist compares parsed, normalized URLs exactly, including scheme, port, path, and query. It does not permit other paths on the same host. Redirects are returned to the plugin and never followed. HTTP is supported for internal services; use HTTPS when the connection needs TLS. Plugin lookup permissions are separate from proxy upstream permissions.

The WASM import is:

```c
// import module: "llmtrace", name: "http_get"
int32_t http_get(int32_t request_ptr, int32_t request_len,
                 int32_t output_ptr, int32_t output_capacity);
```

Both buffers belong to the plugin's exported linear memory. The request is UTF-8 JSON:

```json
{
  "url": "https://llm.example.com/v1/whoami",
  "headers": { "authorization": "Bearer employee-api-key" }
}
```

Read the credential from `HookInput.headers` in the request-start hook. Only send the credential header required by the configured identity endpoint. The host never supplies a credential automatically. `Host`, `Connection`, `Content-Length`, and `Transfer-Encoding` overrides are rejected.

The result is a positive byte count written into the output buffer, containing:

```json
{
  "status": 200,
  "body": "{\"id\":\"employee-123\",\"name\":\"Ada\"}"
}
```

`body` is a string. Parse it according to the upstream schema and check the HTTP status before using its identity. Return the existing hook output, for example:

```json
{
  "user_id": "employee-123",
  "user_name": "Ada",
  "custom_fields": { "identity_source": "gateway" }
}
```

Only return `session_key` when you have an actual conversation identifier. A username or API key groups an employee's unrelated conversations together and must not be used as a substitute. Existing hook exports, allocation, and packed output-pointer conventions remain unchanged. Legacy `metadata` is accepted as an alias of `custom_fields`.

The HTTP import returns `-1` for a denied URL, invalid buffer/request, timeout, transport failure, non-UTF-8 body, oversized response, or insufficient output capacity. Return a warning and leave identity unset on failure; do not fabricate a username. HTTP error responses themselves return the JSON envelope so the plugin can handle their status.

Limits per invocation:

- Four HTTP calls at most, sharing the plugin's wall-clock deadline.
- 16 KiB request JSON; 64 KiB response body and at most 64 KiB output capacity. JSON escaping adds envelope overhead, so a near-limit body can still exceed the output buffer.
- 128 MiB linear memory, one memory and instance, 100,000 table elements.
- Existing 64 KiB hook output limit and epoch timeout remain in force.

The host uses the Tokio runtime from the blocking trace worker; it does not block a live proxy handler. Each invocation receives a fresh instance. There is no shared identity cache or retry queue yet. Prefer a request-start-only lookup hook, give network lookups a realistic timeout, and monitor trace queue/persistence metrics. These permissions allow a trusted plugin to transmit captured credentials to the configured endpoint, so review the plugin and its exact URLs together.

The test `plugins::http::tests::http_host_looks_up_identity_without_following_redirects_and_times_out` exercises a real local HTTP server through the WASM import. The actual enterprise identity schema still needs to be supplied and implemented by the deployment's plugin.
