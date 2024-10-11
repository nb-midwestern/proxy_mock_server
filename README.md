# Mock Api Interceptor

## How to use

- Install [Rust](https://rustup.rs/)
- copy `settings-example.json` to `settings.json`
- update values in `settings.json` to match needs
- run `cargo run`


### Settings

- `default_endpoint` - The endpoint that will be hit if you don't have a mock defined
- `endpoints` - Array of interceptor endpoints

### Hot Editing

The config json con be edited in the browsers at:

- <http://localhost:8000/mockserver/admin>

### Setting up the JSON

```json
{
  "default_endpoint": "https://localhost:5003",
  "endpoints": [
    {
      "method": "GET",
      "path": "/api/v1/endpoint/{id}",
      "status": 200,
      "content_type": "application/json",
      "payload": "{\"result\": \"Data created.\"}"
    },
  ]
}
```

`default_endpoint` - Endpoint route misses should be forward to

`endpoints` ---------- Endpoints that should be caught `GET` `POST` `PUT` etc

`method` -------------- Request type

`status` -------------- Response code

`content_type` ------ Response type `application/json` `text/plain` `text/html` etc

`payload` ------------ Response in JSON or as a string.

#### Using variables in path

Paths may contain variables such that can be used in response by placing the variable name in the path inside of `{}` and this can be used in the payload by using `{{}}`

```json
    {
      "method": "GET",
      "path": "/api/v1/message/{name}",
      "status": 200,
      "content_type": "text/plain",
      "payload": "Hello, {{name}}!"
    }
```
