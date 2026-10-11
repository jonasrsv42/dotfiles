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
        Route::new("GET", "/v1/admin/r0", echo, true),
        Route::new("GET", "/v1/admin/r1", echo, true),
        Route::new("GET", "/v1/admin/r2", echo, true),
        Route::new("GET", "/v1/admin/r3", echo, true),
        Route::new("GET", "/v1/admin/r4", echo, true),
        Route::new("GET", "/v1/admin/r5", echo, true),
        Route::new("GET", "/v1/admin/r6", echo, true),
        Route::new("GET", "/v1/admin/r7", echo, true),
        Route::new("GET", "/v1/admin/r8", echo, true),
        Route::new("GET", "/v1/admin/r9", echo, true),
        Route::new("GET", "/v1/admin/r10", echo, true),
        Route::new("GET", "/v1/admin/r11", echo, true),
        Route::new("GET", "/v1/admin/r12", echo, true),
        Route::new("GET", "/v1/admin/r13", echo, true),
        Route::new("GET", "/v1/admin/r14", echo, true),
        Route::new("GET", "/v1/admin/r15", echo, true),
        Route::new("GET", "/v1/admin/r16", echo, true),
        Route::new("GET", "/v1/admin/r17", echo, true),
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
