# Mock web shop

`examples/mockapp` is a small in-memory web shop to test against. It has a JSON login with session cookie, expiring
bearer tokens, a protected API with route parameters, varying latency, 1 % failing orders, a per-session rate limit
and a CSRF-protected form.

    cargo run --example mockapp                    # http://127.0.0.1:18080
    cargo run --example mockapp -- 0.0.0.0:9000    # another address

It accepts `user0`…`user999` with password `pw0`…`pw999`. `accounts.csv` has the first 500. Two ready test
definitions: `test.json` (session cookie) and `test-jwt.json` (bearer token from `/api/token`).

| Route | Behaviour |
|---|---|
| `POST /api/login` | JSON `{"username","password"}` → 200 and `sid` cookie, 401 on bad credentials |
| `POST /api/token` | same credentials → `{"access_token","token_type":"Bearer","expires_in":300}`, a JWT-shaped token that expires |
| `POST /api/token?via=header` | same, but 204 with the token only in `Authorization: Bearer …` and `Set-Cookie: jwt=…` |
| `GET /api/items`, `GET /api/items/{id}` | 5–40 ms latency, 404 for id ≥ 1000 |
| `POST /api/orders` | JSON `{"item","qty"}` → 201, 400 on bad input, 500 for 1 % |
| `GET /api/slow` | 300–800 ms |
| `POST /api/logout` | 204 |
| `GET /login`, `POST /login` | HTML form with a CSRF token; without it → 403 (see [limitations](../../docs/usage.md#known-limitations)) |

Every `/api/*` route except login and token answers 401 without a session cookie or valid bearer token, and 429
above 20 requests per second and session.
