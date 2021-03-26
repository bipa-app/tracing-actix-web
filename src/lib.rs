//! `tracing-actix-web` provides [`TracingLogger`], a middleware to log request and response info
//! when using the [`actix-web`] framework.
//!
//! [`TracingLogger`] is designed as a drop-in replacement of [`actix-web`]'s [`Logger`].
//!
//! [`Logger`] is built on top of the [`log`] crate: you need to use regular expressions to parse
//! the request information out of the logged message.
//!
//! [`TracingLogger`] relies on [`tracing`], a modern instrumentation framework for structured
//! logging: all request information is captured as a machine-parsable set of key-value pairs.
//! It also enables propagation of context information to children spans.
//!
//! [`TracingLogger`]: struct.TracingLogger.html
//! [`actix-web`]: https://docs.rs/actix-web
//! [`Logger`]: https://docs.rs/actix-web/3.0.2/actix_web/middleware/struct.Logger.html
//! [`log`]: https://docs.rs/log
//! [`tracing`]: https://docs.rs/tracing
use actix_web::Error;
use actix_web::{
    dev::{Service, ServiceRequest, ServiceResponse, Transform},
    HttpMessage,
};
use futures::task::{Context, Poll};
use futures::{
    future::{ok, Ready},
    FutureExt,
};
use std::future::Future;
use std::pin::Pin;
use tracing_futures::Instrument;
use uuid::Uuid;

/// `TracingLogger` is a middleware to log request and response info in a structured format.
///
/// `TracingLogger` is designed as a drop-in replacement of [`actix-web`]'s [`Logger`].
///
/// [`Logger`] is built on top of the [`log`] crate: you need to use regular expressions to parse
/// the request information out of the logged message.
///
/// `TracingLogger` relies on [`tracing`], a modern instrumentation framework for structured
/// logging: all request information is captured as a machine-parsable set of key-value pairs.
/// It also enables propagation of context information to children spans.
///
/// ## Usage
///
/// Register `TracingLogger` as a middleware for your application using `.wrap` on `App`.
/// Add a `Subscriber` implementation to output logs to the console.
///
/// ```rust
/// use actix_web::middleware::Logger;
/// use actix_web::App;
/// use tracing::{Subscriber, subscriber::set_global_default};
/// use tracing_actix_web::TracingLogger;
/// use tracing_log::LogTracer;
/// use tracing_bunyan_formatter::{BunyanFormattingLayer, JsonStorageLayer};
/// use tracing_subscriber::{layer::SubscriberExt, EnvFilter, Registry};
///
/// /// Compose multiple layers into a `tracing`'s subscriber.
/// pub fn get_subscriber(
///     name: String,
///     env_filter: String
/// ) -> impl Subscriber + Send + Sync {
///     let env_filter = EnvFilter::try_from_default_env()
///         .unwrap_or(EnvFilter::new(env_filter));
///     let formatting_layer = BunyanFormattingLayer::new(
///         name.into(),
///         std::io::stdout
///     );
///     Registry::default()
///         .with(env_filter)
///         .with(JsonStorageLayer)
///         .with(formatting_layer)
/// }
///
/// /// Register a subscriber as global default to process span data.
/// ///
/// /// It should only be called once!
/// pub fn init_subscriber(subscriber: impl Subscriber + Send + Sync) {
///     LogTracer::init().expect("Failed to set logger");
///     set_global_default(subscriber).expect("Failed to set subscriber");
/// }
///
/// fn main() {
///     let subscriber = get_subscriber("app".into(), "info".into());
///     init_subscriber(subscriber);
///
///     let app = App::new().wrap(TracingLogger);
/// }
/// ```
///
/// [`actix-web`]: https://docs.rs/actix-web
/// [`Logger`]: https://docs.rs/actix-web/3.0.2/actix_web/middleware/struct.Logger.html
/// [`log`]: https://docs.rs/log
/// [`tracing`]: https://docs.rs/tracing
pub struct TracingLogger {
    service_name: String,
}

impl TracingLogger {
    pub fn new(service_name: String) -> TracingLogger {
        TracingLogger { service_name }
    }
}

impl<S, B> Transform<S> for TracingLogger
where
    S: Service<Request = ServiceRequest, Response = ServiceResponse<B>, Error = Error>,
    S::Future: 'static,
    B: 'static,
{
    type Request = ServiceRequest;
    type Response = ServiceResponse<B>;
    type Error = Error;
    type Transform = TracingLoggerMiddleware<S>;
    type InitError = ();
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        ok(TracingLoggerMiddleware {
            service_name: self.service_name.clone(),
            service,
        })
    }
}

#[doc(hidden)]
pub struct TracingLoggerMiddleware<S> {
    service_name: String,
    service: S,
}

impl<S, B> Service for TracingLoggerMiddleware<S>
where
    S: Service<Request = ServiceRequest, Response = ServiceResponse<B>, Error = Error>,
    S::Future: 'static,
    B: 'static,
{
    type Request = ServiceRequest;
    type Response = ServiceResponse<B>;
    type Error = Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>>>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.service.poll_ready(cx)
    }

    fn call(&mut self, req: ServiceRequest) -> Self::Future {
        let path = req.path();

        if path == "/healthz" {
            return Box::pin(self.service.call(req));
        }

        let user_agent = req
            .headers()
            .get("User-Agent")
            .and_then(|h| h.to_str().ok())
            .unwrap_or("");

        let route = req.match_pattern().unwrap_or(String::from(""));

        let span = tracing::info_span!(
            "Request",
            kind = "request",
            request_id = %Uuid::new_v4(),
            http.status_code = tracing::field::Empty,
            enduser.id = tracing::field::Empty,
            service.name = %self.service_name,
            http.user_agent = %user_agent,
            http.method = %req.method(),
            http.target = %path,
            http.route = %route,
        );

        req.extensions_mut().insert(span.clone());

        let fut = self
            .service
            .call(req)
            .instrument(span.clone())
            .inspect(move |outcome| {
                let status_code = match outcome {
                    Ok(response) => response.response().status(),
                    Err(error) => error.as_response_error().status_code(),
                };

                span.record("http.status_code", &status_code.as_u16());
            });

        Box::pin(fut)
    }
}
