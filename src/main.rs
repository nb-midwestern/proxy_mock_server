use axum::{
    body::Body,
    extract::{Json, State},
    http::{Request, Response, StatusCode},
    response::{Html, IntoResponse},
    routing::get_service,
    Router,
};
use hyper::client::HttpConnector;
use hyper::Client;
use hyper_rustls::HttpsConnectorBuilder;
use matchit::Router as MatchItRouter;
use serde::{Deserialize, Serialize};
use std::{net::SocketAddr, sync::Arc};
use tokio::sync::RwLock;
use tower::ServiceBuilder;
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;
use tracing::{info, Level};

#[derive(Debug, Deserialize, Serialize, Clone)]
struct EndpointConfig {
    method: String,
    path: String,
    status: u16,
    content_type: String,
    payload: serde_json::Value,
}

#[derive(Debug, Deserialize, Serialize)]
struct Settings {
    default_endpoint: String,
    endpoints: Vec<EndpointConfig>,
}

#[derive(Clone)]
struct AppState {
    endpoints: Arc<RwLock<Vec<EndpointConfig>>>,
    router: Arc<RwLock<MatchItRouter<usize>>>, // For path matching
    default_endpoint: String,
    client: Client<hyper_rustls::HttpsConnector<HttpConnector>, Body>,
}
#[tokio::main]
async fn main() {
    // Set up logging
    // tracing_subscriber::fmt().with_max_level(Level::INFO).init();
    tracing_subscriber::fmt().with_max_level(Level::INFO).init();

    // Load settings
    let settings: Settings = {
        let file = std::fs::File::open("settings.json").expect("Failed to open settings.json");
        serde_json::from_reader(file).expect("Failed to parse settings.json")
    };

    // HTTPS client setup using HttpsConnectorBuilder
    let https = HttpsConnectorBuilder::new()
        .with_native_roots()
        .https_only()
        .enable_http1()
        .build();
    let client = Client::builder().build(https);

    // Shared application state
    let endpoints = Arc::new(RwLock::new(settings.endpoints.clone()));
    let router = build_router(&settings.endpoints);

    let app_state = AppState {
        endpoints,
        router,
        default_endpoint: settings.default_endpoint,
        client,
    };

    // Build the Axum router with logging middleware
    let app = Router::new()
        .route("/mockserver/admin", axum::routing::get(admin_page))
        .route(
            "/mockserver/admin/update",
            axum::routing::post(update_endpoints),
        )
        .nest_service(
            "/static",
            get_service(ServeDir::new("static")).handle_error(handle_error),
        )
        .fallback(handler)
        .with_state(app_state)
        .layer(ServiceBuilder::new().layer(TraceLayer::new_for_http()));

    // Run the server
    let addr = SocketAddr::from(([0, 0, 0, 0], 8000));
    println!("Listening on {}", addr);
    axum::Server::bind(&addr)
        .serve(app.into_make_service())
        .await
        .unwrap();
}

// Adjusted handler function
async fn handler(State(state): State<AppState>, req: Request<Body>) -> impl IntoResponse {
    match process_request(state, req).await {
        Ok(response) => response,
        Err(_) => Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .body(Body::from("Internal Server Error"))
            .unwrap(),
    }
}

async fn process_request(
    state: AppState,
    req: Request<Body>,
) -> Result<Response<Body>, hyper::Error> {
    let method = req.method().clone();
    let path = req.uri().path().to_string();

    tracing::info!("Processing request: {} {}", method, path);

    // Read the endpoints and router
    let endpoints = state.endpoints.read().await;
    let router = state.router.read().await;

    // Match the request path
    if let Ok(matched) = router.at(&path) {
        let idx = *matched.value;
        let endpoint = &endpoints[idx];

        if endpoint.method.eq_ignore_ascii_case(method.as_str()) {
            tracing::info!("Matched mock endpoint for path: {}", path);

            // Collect the path parameters
            let params = matched.params.clone();

            let body = if endpoint.content_type == "application/json" {
                // Inject parameters into the JSON payload
                let mut payload = endpoint.payload.clone();
                if let serde_json::Value::Object(ref mut map) = payload {
                    for (key, value) in params.iter() {
                        map.insert(
                            key.to_string().clone(),
                            serde_json::Value::String(value.to_string().clone()),
                        );
                    }
                }
                serde_json::to_string(&payload).unwrap()
            } else {
                // For other content types, perform placeholder replacement
                let mut body = match &endpoint.payload {
                    serde_json::Value::String(s) => s.clone(),
                    _ => endpoint.payload.to_string(),
                };
                for (key, value) in params.iter() {
                    let placeholder = format!("{{{{{}}}}}", key);
                    body = body.replace(&placeholder, value);
                }
                body
            };

            // Return the mocked response
            let response = Response::builder()
                .status(StatusCode::from_u16(endpoint.status).unwrap())
                .header("Content-Type", &endpoint.content_type)
                .body(Body::from(body))
                .unwrap();

            tracing::info!("Mocked response for {}: {}", path, endpoint.status);
            return Ok(response);
        }
    }

    // Proxy the request to the default endpoint
    tracing::info!(
        "Proxying request to default backend: {}",
        state.default_endpoint
    );
    match proxy_request(req, state.clone()).await {
        Ok(response) => {
            tracing::info!("Proxied response: {}", response.status());
            Ok(response)
        }
        Err(e) => {
            tracing::error!("Failed to proxy request: {}", e);
            Err(e)
        }
    }
}

async fn proxy_request(
    mut req: Request<Body>,
    state: AppState,
) -> Result<Response<Body>, hyper::Error> {
    // Construct the new URI for the default endpoint
    let uri = req.uri().clone();
    let query = uri.query().map(|q| format!("?{}", q)).unwrap_or_default();
    let new_uri_str = format!("{}{}{}", state.default_endpoint, uri.path(), query);
    let new_uri = new_uri_str
        .parse::<hyper::Uri>()
        .expect("Failed to parse new URI");
    *req.uri_mut() = new_uri.clone();

    tracing::info!("Forwarding request to: {}", new_uri);

    // Remove the `Host` header to prevent potential issues
    req.headers_mut().remove("host");

    // Forward the request
    match state.client.request(req).await {
        Ok(response) => {
            tracing::info!(
                "Received proxied response with status: {}",
                response.status()
            );
            Ok(response)
        }
        Err(e) => {
            tracing::error!("Error during proxy request: {}", e);
            Err(e)
        }
    }
}

fn build_router(endpoints: &[EndpointConfig]) -> Arc<RwLock<MatchItRouter<usize>>> {
    let mut router = MatchItRouter::new();
    for (idx, ep) in endpoints.iter().enumerate() {
        match router.insert(&ep.path, idx) {
            Ok(_) => tracing::debug!("Inserted route: {}", &ep.path),
            Err(e) => tracing::error!("Failed to insert route {}: {}", &ep.path, e),
        }
    }
    Arc::new(RwLock::new(router))
}

// Admin endpoint to update the endpoints dynamically
async fn update_endpoints(
    State(state): State<AppState>,
    Json(new_endpoints): Json<Vec<EndpointConfig>>,
) -> impl IntoResponse {
    // Update the endpoints and router
    {
        let mut endpoints = state.endpoints.write().await;
        *endpoints = new_endpoints.clone();
    }
    {
        let mut router = state.router.write().await;
        *router = MatchItRouter::new();
        for (idx, ep) in new_endpoints.iter().enumerate() {
            router.insert(&ep.path, idx).unwrap();
        }
    }

    // Assemble new Settings struct
    let settings = Settings {
        default_endpoint: state.default_endpoint.clone(),
        endpoints: new_endpoints.clone(),
    };

    // Write settings to settings.json
    if let Err(e) = write_settings_to_file(&settings) {
        tracing::error!("Failed to write settings to file: {}", e);
        return Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .body(Body::from("Failed to write settings to file"))
            .unwrap();
    }

    tracing::info!("Endpoints updated dynamically.");

    Response::builder()
        .status(StatusCode::OK)
        .body(Body::from("Endpoints updated"))
        .unwrap()
}

// Function to write settings to the JSON file
fn write_settings_to_file(settings: &Settings) -> std::io::Result<()> {
    let file = std::fs::File::create("settings.json")?;
    serde_json::to_writer_pretty(file, settings)?;
    Ok(())
}

// Admin page handler
async fn admin_page(State(state): State<AppState>) -> impl IntoResponse {
    // Read the current endpoint configurations
    let endpoints = state.endpoints.read().await;
    let endpoints_json = serde_json::to_string_pretty(&*endpoints).unwrap();

    // Build the HTML content
    let html_content = format!(
        r#"
        <!DOCTYPE html>
        <html>
        <head>
            <title>Mock Server Admin</title>
             <link rel="icon" href="/static/favicon.svg" type="image/x-icon">
            <!-- Include JSONEditor via CDN -->
            <link href="https://cdn.jsdelivr.net/npm/jsoneditor@9.5.6/dist/jsoneditor.min.css" rel="stylesheet" type="text/css">
            <script src="https://cdn.jsdelivr.net/npm/jsoneditor@9.5.6/dist/jsoneditor.min.js"></script>
            <!-- Include Toastify CSS and JS -->
            <link rel="stylesheet" type="text/css" href="https://cdn.jsdelivr.net/npm/toastify-js/src/toastify.min.css">
            <script type="text/javascript" src="https://cdn.jsdelivr.net/npm/toastify-js"></script>
            <style>
                /* Your custom styles here */
            </style>
        </head>
        <body>
            <h1>Mock Server Admin</h1>
            <div id="jsoneditor" style="height: 80vh; width: 100%;"></div>
            <button id="submit-button">Submit</button>
            <script>
                var container = document.getElementById('jsoneditor');
                var options = {{
                    mode: 'code',
                    modes: ['code', 'form', 'text', 'tree', 'view'],
                    onError: function (err) {{
                        Toastify({{
                            text: err.toString(),
                            duration: 3000,
                            close: true,
                            gravity: 'top',
                            position: 'right',
                            backgroundColor: '#F44336'
                        }}).showToast();
                    }}
                }};
                var editor = new JSONEditor(container, options);
                editor.set({json_data});
        
                function showToast(message, type) {{
                    Toastify({{
                        text: message,
                        duration: 3000,
                        close: true,
                        gravity: 'top',
                        position: 'right',
                        backgroundColor: type === 'success' ? '#4CAF50' : '#F44336'
                    }}).showToast();
                }}
        
                function submitForm() {{
                    try {{
                        var data = editor.get();
                        fetch('/mockserver/admin/update', {{  // Updated fetch URL
                            method: 'POST',
                            headers: {{
                                'Content-Type': 'application/json'
                            }},
                            body: JSON.stringify(data)
                        }})
                        .then(function(response) {{
                            if(response.ok) {{
                                showToast('Endpoints updated successfully', 'success');
                            }} else {{
                                showToast('Failed to update endpoints', 'error');
                            }}
                        }});
                    }} catch (err) {{
                        showToast('Invalid JSON data', 'error');
                    }}
                }}
        
                document.getElementById('submit-button').addEventListener('click', submitForm);
        
                document.addEventListener('keydown', function(event) {{
                    var key = event.key || event.keyCode;
                    if ((event.ctrlKey || event.metaKey) && (key === 's' || key === 'S' || key === 83)) {{
                        event.preventDefault();
                        submitForm();
                    }}
                }});
            </script>
        </body>
        </html>
        "#,
        json_data = endpoints_json
    );

    Html(html_content)
}

async fn handle_error(_err: std::io::Error) -> impl IntoResponse {
    (StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong..")
}
