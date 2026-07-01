use axum::{extract::Query, http::StatusCode, response::IntoResponse, routing::get, Json, Router};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use tracing_subscriber::EnvFilter;

#[derive(Serialize)]
struct HealthResponse {
    status: String,
}

#[derive(Deserialize)]
struct GreetParams {
    name: Option<String>,
}

#[derive(Serialize)]
struct StringResponse {
    string: String,
}

#[derive(Deserialize)]
struct CalcParams {
    a: Option<i64>,
    b: Option<i64>,
}

#[derive(Serialize)]
struct CalcResponse {
    result: i64,
    operation: String,
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "healthy".to_string(),
    })
}

async fn greet(Query(params): Query<GreetParams>) -> Result<Json<StringResponse>, impl IntoResponse> {
    let name = params.name.unwrap_or_default();

    if name.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "name parameter is required".to_string(),
            }),
        ));
    }

    if name.len() > 100 {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "name too long (max 100 characters)".to_string(),
            }),
        ));
    }

    let message = if name.to_lowercase() == "world" {
        "Hello, World! Welcome to the Rust coverage demo.".to_string()
    } else {
        format!("Hello, {}!", name)
    };

    Ok(Json(StringResponse { string: message }))
}

async fn calculate(
    Query(params): Query<CalcParams>,
) -> Result<Json<CalcResponse>, (StatusCode, Json<ErrorResponse>)> {
    let a = params.a.ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "parameter 'a' is required".to_string(),
            }),
        )
    })?;

    let b = params.b.ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "parameter 'b' is required".to_string(),
            }),
        )
    })?;

    let result = if a < 0 || b < 0 {
        0
    } else if a > 1_000_000 || b > 1_000_000 {
        a.saturating_add(b)
    } else {
        a + b
    };

    Ok(Json(CalcResponse {
        result,
        operation: format!("{} + {} = {}", a, b, result),
    }))
}

async fn useless() -> Json<StringResponse> {
    Json(StringResponse {
        string: "This is a useless route".to_string(),
    })
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    #[cfg(feature = "coverage")]
    let _coverage_handle = coverage_server::start_coverage_server().await;

    let app = Router::new()
        .route("/health", get(health))
        .route("/greet", get(greet))
        .route("/calculate", get(calculate))
        .route("/useless", get(useless));

    let port: u16 = std::env::var("APP_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8000);

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    tracing::info!("Example app listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
