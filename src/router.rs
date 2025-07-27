use axum::{
    Router,
    http::Method,
    middleware::{from_extractor, map_response},
    routing::{delete, get, post},
};
use const_format::formatc;
use tower::ServiceBuilder;
use tower_http::{compression::CompressionLayer, cors::CorsLayer, services::ServeDir};

use crate::{
    IS_DEBUG,
    api::*,
    claude_code_state::ClaudeCodeState,
    claude_web_state::ClaudeWebState,
    gemini_state::GeminiState,
    middleware::{
        RequireAdminAuth, RequireBearerAuth, RequireQueryKeyAuth, RequireXApiKeyAuth,
        claude::{add_usage_info, apply_stop_sequences, to_oai},
    },
    services::{cookie_actor::CookieActorHandle, key_actor::KeyActorHandle},
};

/// RouterBuilder for the application
pub struct RouterBuilder {
    claude_web_state: ClaudeWebState,
    claude_code_state: ClaudeCodeState,
    cookie_actor_handle: CookieActorHandle,
    key_actor_handle: KeyActorHandle,
    gemini_state: GeminiState,
    inner: Router,
}

impl RouterBuilder {
    /// Creates a blank RouterBuilder instance
    /// Initializes the router with the provided application state
    ///
    /// # Arguments
    /// * `state` - The application state containing client information
    pub async fn new() -> Self {
        let cookie_handle = CookieActorHandle::start()
            .await
            .expect("Failed to start CookieActor");
        let claude_web_state = ClaudeWebState::new(cookie_handle.to_owned());
        let claude_code_state = ClaudeCodeState::new(cookie_handle.to_owned());
        let key_tx = KeyActorHandle::start()
            .await
            .expect("Failed to start KeyActorHandle");
        let gemini_state = GeminiState::new(key_tx.to_owned());
        RouterBuilder {
            claude_web_state,
            claude_code_state,
            cookie_actor_handle: cookie_handle,
            key_actor_handle: key_tx,
            gemini_state,
            inner: Router::new(),
        }
    }

    /// Creates a new RouterBuilder instance
    /// Sets up routes for API endpoints and static file serving
    pub fn with_default_setup(self) -> Self {
        self.route_claude_code_endpoints()
            .route_claude_web_endpoints()
            .route_admin_endpoints()
            .route_claude_web_oai_endpoints()
            .route_claude_code_oai_endpoints()
            .route_gemini_endpoints()
            .setup_static_serving()
            .with_tower_trace()
            .with_cors()
    }

    fn route_gemini_endpoints(mut self) -> Self {
        let router_gemini = Router::new()
            .route("/v1/v1beta/{*path}", post(api_post_gemini))
            .route("/v1/vertex/v1beta/{*path}", post(api_post_gemini))
            .layer(from_extractor::<RequireQueryKeyAuth>())
            .layer(CompressionLayer::new())
            .with_state(self.gemini_state.to_owned());
        let router_oai = Router::new()
            .route("/gemini/chat/completions", post(api_post_gemini_oai))
            .route("/gemini/vertex/chat/completions", post(api_post_gemini_oai))
            .layer(from_extractor::<RequireBearerAuth>())
            .layer(CompressionLayer::new())
            .with_state(self.gemini_state.to_owned());
        let router = router_gemini.merge(router_oai);
        self.inner = self.inner.merge(router);
        self
    }

    /// Sets up routes for v1 endpoints
    fn route_claude_web_endpoints(mut self) -> Self {
        let router = Router::new()
            .route("/v1/messages", post(api_claude_web))
            .layer(
                ServiceBuilder::new()
                    .layer(from_extractor::<RequireXApiKeyAuth>())
                    .layer(CompressionLayer::new())
                    .layer(map_response(add_usage_info))
                    .layer(map_response(apply_stop_sequences)),
            )
            .with_state(self.claude_web_state.to_owned().with_claude_format());
        self.inner = self.inner.merge(router);
        self
    }

    /// Sets up routes for v1 endpoints
    fn route_claude_code_endpoints(mut self) -> Self {
        let router = Router::new()
            .route("/code/v1/messages", post(api_claude_code))
            .layer(
                ServiceBuilder::new()
                    .layer(from_extractor::<RequireXApiKeyAuth>())
                    .layer(CompressionLayer::new())
                    .layer(map_response(add_usage_info)),
            )
            .with_state(self.claude_code_state.to_owned());
        self.inner = self.inner.merge(router);
        self
    }

    /// Sets up routes for API endpoints
    fn route_admin_endpoints(mut self) -> Self {
        let cookie_router = Router::new()
            .route("/cookies", get(api_get_cookies))
            .route("/cookie", delete(api_delete_cookie).post(api_post_cookie))
            .with_state(self.cookie_actor_handle.to_owned());
        let key_router = Router::new()
            .route("/key", post(api_post_key).delete(api_delete_key))
            .route("/keys", get(api_get_keys))
            .with_state(self.key_actor_handle.to_owned());
        let admin_router = Router::new()
            .route("/auth", get(api_auth))
            .route("/config", get(api_get_config).put(api_post_config));
        let router = Router::new()
            .nest(
                "/api",
                cookie_router
                    .merge(key_router)
                    .merge(admin_router)
                    .layer(from_extractor::<RequireAdminAuth>()),
            )
            .route("/api/version", get(api_version));
        self.inner = self.inner.merge(router);
        self
    }

    /// Sets up routes for OpenAI compatible endpoints
    fn route_claude_web_oai_endpoints(mut self) -> Self {
        let router = Router::new()
            .route("/v1/chat/completions", post(api_claude_web))
            .route("/v1/models", get(api_get_models))
            .layer(
                ServiceBuilder::new()
                    .layer(from_extractor::<RequireBearerAuth>())
                    .layer(CompressionLayer::new())
                    .layer(map_response(to_oai))
                    .layer(map_response(apply_stop_sequences)),
            )
            .with_state(self.claude_web_state.to_owned().with_openai_format());
        self.inner = self.inner.merge(router);
        self
    }

    /// Sets up routes for OpenAI compatible endpoints
    fn route_claude_code_oai_endpoints(mut self) -> Self {
        let router = Router::new()
            .route("/code/v1/chat/completions", post(api_claude_code))
            .route("/code/v1/models", get(api_get_models))
            .layer(
                ServiceBuilder::new()
                    .layer(from_extractor::<RequireBearerAuth>())
                    .layer(CompressionLayer::new())
                    .layer(map_response(to_oai)),
            )
            .with_state(self.claude_code_state.to_owned());
        self.inner = self.inner.merge(router);
        self
    }

    /// Sets up static file serving
    fn setup_static_serving(mut self) -> Self {
        if IS_DEBUG {
            self.inner = self.inner.fallback_service(ServeDir::new(formatc!(
                "{}/static",
                env!("CARGO_MANIFEST_DIR")
            )));
        } else {
            use include_dir::{Dir, include_dir};
            const INCLUDE_STATIC: Dir = include_dir!("$CARGO_MANIFEST_DIR/static");
            self.inner = self
                .inner
                .fallback_service(tower_serve_static::ServeDir::new(&INCLUDE_STATIC));
        }
        self
    }

    /// Adds CORS support to the router
    fn with_cors(mut self) -> Self {
        let cors = CorsLayer::new()
            .allow_origin(tower_http::cors::Any)
            .allow_methods([Method::GET, Method::POST, Method::DELETE])
            .allow_headers([
                axum::http::header::AUTHORIZATION,
                axum::http::header::CONTENT_TYPE,
            ]);

        self.inner = self.inner.layer(cors);
        self
    }

    fn with_tower_trace(mut self) -> Self {
        use tower_http::trace::TraceLayer;

        let layer = TraceLayer::new_for_http();

        self.inner = self.inner.layer(layer);
        self
    }

    /// Returns the configured router
    /// Finalizes the router configuration for use with axum
    pub fn build(self) -> Router {
        self.inner
    }
}
