//! A table: long, but one list of simple entries. Noted, not flagged.

pub struct Route {
    pub method: &'static str,
    pub path: &'static str,
    pub handler: fn(u32) -> u32,
    pub auth: bool,
}

fn echo(x: u32) -> u32 {
    x
}

pub fn routes() -> Vec<Route> {
    vec![
        Route::new("GET", "/v1/jobs", echo, true),
        Route::new("POST", "/v1/jobs", echo, true),
        Route::new("GET", "/v1/jobs/{id}", echo, true),
        Route::new("DELETE", "/v1/jobs/{id}", echo, true),
        Route::new("GET", "/v1/jobs/{id}/log", echo, true),
        Route::new("POST", "/v1/jobs/{id}/retry", echo, true),
        Route::new("GET", "/v1/queues", echo, false),
        Route::new("POST", "/v1/queues", echo, true),
        Route::new("GET", "/v1/queues/{name}", echo, false),
        Route::new("DELETE", "/v1/queues/{name}", echo, true),
        Route::new("GET", "/v1/workers", echo, false),
        Route::new("POST", "/v1/workers/{id}/drain", echo, true),
        Route::new("GET", "/v1/health", echo, false),
        Route::new("GET", "/v1/metrics", echo, false),
        Route::new("GET", "/v1/version", echo, false),
        Route {
            method: "GET",
            path: "/",
            handler: echo,
            auth: false,
        },
        Route {
            method: "GET",
            path: "/v1",
            handler: echo,
            auth: false,
        },
        Route {
            method: "OPTIONS",
            path: "/v1",
            handler: echo,
            auth: false,
        },
        Route {
            method: "HEAD",
            path: "/v1",
            handler: echo,
            auth: false,
        },
    ]
}

impl Route {
    fn new(method: &'static str, path: &'static str, handler: fn(u32) -> u32, auth: bool) -> Self {
        Route { method, path, handler, auth }
    }
}
