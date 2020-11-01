#![warn(rust_2018_idioms)]
extern crate conduit;
extern crate route_recognizer as router;

#[macro_use]
extern crate tracing;

use std::collections::hash_map::{Entry, HashMap};
use std::error::Error;
use std::fmt;

use conduit::{box_error, Handler, HandlerResult, Method, RequestExt};
use router::{Match, Router};

#[derive(Default)]
pub struct RouteBuilder {
    routers: HashMap<Method, Router<WrappedHandler>>,
}

#[derive(Clone, Copy)]
pub struct RoutePattern(&'static str);

impl RoutePattern {
    pub fn pattern(&self) -> &str {
        self.0
    }
}

pub struct WrappedHandler {
    pattern: RoutePattern,
    handler: Box<dyn Handler>,
}

impl conduit::Handler for WrappedHandler {
    fn call(&self, request: &mut dyn RequestExt) -> HandlerResult {
        self.handler.call(request)
    }
}

#[derive(Debug)]
pub struct RouterError(String);

impl RouteBuilder {
    pub fn new() -> RouteBuilder {
        RouteBuilder {
            routers: HashMap::new(),
        }
    }

    #[instrument(level = "trace", skip(self))]
    #[allow(clippy::borrowed_box)]
    pub fn recognize<'a>(
        &'a self,
        method: &Method,
        path: &str,
    ) -> Result<Match<&WrappedHandler>, RouterError> {
        match self.routers.get(method) {
            Some(router) => router.recognize(path),
            None => Err(format!("No router found for {:?}", method)),
        }
        .map_err(RouterError)
    }

    #[instrument(level = "trace", skip(self, handler))]
    pub fn map<H: Handler>(
        &mut self,
        method: Method,
        pattern: &'static str,
        handler: H,
    ) -> &mut RouteBuilder {
        {
            let router = match self.routers.entry(method) {
                Entry::Occupied(e) => e.into_mut(),
                Entry::Vacant(e) => e.insert(Router::new()),
            };
            let wrapped_handler = WrappedHandler {
                pattern: RoutePattern(pattern),
                handler: Box::new(handler),
            };
            router.add(pattern, wrapped_handler);
        }
        self
    }

    pub fn get<H: Handler>(&mut self, pattern: &'static str, handler: H) -> &mut RouteBuilder {
        self.map(Method::GET, pattern, handler)
    }

    pub fn post<H: Handler>(&mut self, pattern: &'static str, handler: H) -> &mut RouteBuilder {
        self.map(Method::POST, pattern, handler)
    }

    pub fn put<H: Handler>(&mut self, pattern: &'static str, handler: H) -> &mut RouteBuilder {
        self.map(Method::PUT, pattern, handler)
    }

    pub fn delete<H: Handler>(&mut self, pattern: &'static str, handler: H) -> &mut RouteBuilder {
        self.map(Method::DELETE, pattern, handler)
    }

    pub fn head<H: Handler>(&mut self, pattern: &'static str, handler: H) -> &mut RouteBuilder {
        self.map(Method::HEAD, pattern, handler)
    }
}

impl conduit::Handler for RouteBuilder {
    #[instrument(level = "trace", skip(self, request))]
    fn call(&self, request: &mut dyn RequestExt) -> HandlerResult {
        let m = {
            let method = request.method();
            let path = request.path();

            match self.recognize(&method, path) {
                Ok(m) => m,
                Err(e) => {
                    info!("{}", e.0);
                    return Err(box_error(e));
                }
            }
        };

        let pattern = m.handler().pattern;
        debug!(pattern = pattern.0, "matching route handler found");

        {
            let extensions = request.mut_extensions();
            extensions.insert(pattern);
            extensions.insert(m.params().clone());
        }

        let span = trace_span!("handler", pattern = pattern.0);
        span.in_scope(|| m.handler().call(request))
    }
}

impl Error for RouterError {
    fn description(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RouterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

pub trait RequestParams<'a> {
    fn params(self) -> &'a router::Params;
}

pub fn params(req: &dyn RequestExt) -> &router::Params {
    req.extensions()
        .find::<router::Params>()
        .expect("Missing params")
}

impl<'a> RequestParams<'a> for &'a (dyn RequestExt + 'a) {
    fn params(self) -> &'a router::Params {
        params(self)
    }
}

#[cfg(test)]
mod tests {
    extern crate conduit_test;
    extern crate lazy_static;
    extern crate tracing_subscriber;

    use std::io;
    use std::net::SocketAddr;

    use {RequestParams, RouteBuilder, RoutePattern};

    use self::conduit_test::ResponseExt;
    use conduit::{
        Body, Extensions, Handler, HeaderMap, Host, Method, Response, Scheme, StatusCode, Version,
    };

    lazy_static::lazy_static! {
        static ref TRACING: () = {
            tracing_subscriber::FmtSubscriber::builder()
                .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
                .with_span_events(tracing_subscriber::fmt::format::FmtSpan::FULL)
                .with_thread_names(true)
                .init();
        };
    }

    struct RequestSentinel {
        method: Method,
        path: String,
        extensions: conduit::Extensions,
    }

    impl RequestSentinel {
        fn new(method: Method, path: &'static str) -> RequestSentinel {
            RequestSentinel {
                path: path.to_string(),
                extensions: Extensions::new(),
                method,
            }
        }
    }

    impl conduit::RequestExt for RequestSentinel {
        fn http_version(&self) -> Version {
            unimplemented!()
        }
        fn method(&self) -> &Method {
            &self.method
        }
        fn scheme(&self) -> Scheme {
            unimplemented!()
        }
        fn host(&self) -> Host<'_> {
            unimplemented!()
        }
        fn virtual_root(&self) -> Option<&str> {
            unimplemented!()
        }
        fn path(&self) -> &str {
            &self.path
        }
        fn path_mut(&mut self) -> &mut String {
            &mut self.path
        }
        fn query_string(&self) -> Option<&str> {
            unimplemented!()
        }
        fn remote_addr(&self) -> SocketAddr {
            unimplemented!()
        }
        fn content_length(&self) -> Option<u64> {
            unimplemented!()
        }
        fn headers(&self) -> &HeaderMap {
            unimplemented!()
        }
        fn body(&mut self) -> &mut dyn io::Read {
            unimplemented!()
        }
        fn extensions(&self) -> &Extensions {
            &self.extensions
        }
        fn mut_extensions(&mut self) -> &mut Extensions {
            &mut self.extensions
        }
    }

    #[test]
    fn basic_get() {
        lazy_static::initialize(&TRACING);

        let router = test_router();
        let mut req = RequestSentinel::new(Method::GET, "/posts/1");
        let res = router.call(&mut req).expect("No response");

        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(*res.into_cow(), b"1, GET, /posts/:id"[..]);
    }

    #[test]
    fn basic_post() {
        lazy_static::initialize(&TRACING);

        let router = test_router();
        let mut req = RequestSentinel::new(Method::POST, "/posts/10");
        let res = router.call(&mut req).expect("No response");

        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(*res.into_cow(), b"10, POST, /posts/:id"[..]);
    }

    #[test]
    fn nonexistent_route() {
        lazy_static::initialize(&TRACING);

        let router = test_router();
        let mut req = RequestSentinel::new(Method::POST, "/nonexistent");
        router.call(&mut req).err().expect("No response");
    }

    #[test]
    fn catch_all() {
        let mut router = RouteBuilder::new();
        router.get("/*", test_handler);

        let mut req = RequestSentinel::new(Method::GET, "/foo");
        let res = router.call(&mut req).expect("No response");
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(*res.into_cow(), b", GET, /*"[..]);
    }

    fn test_router() -> RouteBuilder {
        let mut router = RouteBuilder::new();
        router.post("/posts/:id", test_handler);
        router.get("/posts/:id", test_handler);
        router
    }

    fn test_handler(req: &mut dyn conduit::RequestExt) -> conduit::HttpResult {
        let res = vec![
            req.params().find("id").unwrap_or("").to_string(),
            format!("{:?}", req.method()),
            req.extensions()
                .find::<RoutePattern>()
                .unwrap()
                .pattern()
                .to_string(),
        ];

        let bytes = res.join(", ").into_bytes();
        Response::builder().body(Body::from_vec(bytes))
    }
}
