//! sans-server – Libreflip sans server daemon
//!
//! The design principle for this software is taken from Douglas Adams:
//!
//! > Don't Panic!
//!
//! Also something about towels.
//!
//! The daemon should never just stop. Every possible error needs to be wrapped
//! into a Result<_> and communicate them to the user interface. Any actual panic
//! is a condition in the operation that prevents communication with the UI. These
//! should however still be logged.

#[macro_use]
extern crate serde_json;

use actix_files as fs;
use actix_web::web::{self, Data};
use actix_web::{App, HttpRequest, HttpResponse, HttpServer, Result};

use handlebars::Handlebars;

use std::io;

/// Renders the "repository/about" subpage
pub async fn render_root(hb: Data<Handlebars>, req: HttpRequest) -> Result<HttpResponse> {
    let data = json!({
        "name": "Libreflip Sans"
    });
    let body = hb.render("index", &data).unwrap();

    Ok(HttpResponse::Ok().content_type("text/html").body(body))
}

#[actix_rt::main]
async fn main() -> io::Result<()> {
    HttpServer::new(|| {
        App::new()
            .service(fs::Files::new("/static", "static"))
            .service(web::resource("/").route(web::get().to(render_root)))
    })
    .bind("127.0.0.1:8080")?
    .run()
    .await
}
