#[macro_use]
extern crate log;

use actix_cors::Cors;
use actix_web::{
    delete, error, get, http::header, http::StatusCode, patch, post, web, App, HttpResponse,
    HttpResponseBuilder, HttpServer, Responder, HttpRequest
};
use anyhow::Result;
use derive_more::{Display, Error as DeriveError};
use listenfd::ListenFd;
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use std::env;

#[derive(Serialize, Deserialize, Debug)]
struct Todo {
    id: i64,
    title: String,
    completed: bool,
    order: i64,
}

#[derive(Deserialize)]
struct NewTodo {
    title: String,
    order: Option<i64>,
}

#[derive(Deserialize)]
struct UpdateTodo {
    title: Option<String>,
    completed: Option<bool>,
    order: Option<i64>,
}

#[derive(Serialize)]
struct TodoPresenter {
    #[serde(flatten)]
    todo: Todo,
    url: String,
}

struct TodosList {
    todos: Vec<Todo>,
    routing: RoutingService,
}

#[derive(Debug, Display, DeriveError)]
enum Error {
    #[display(fmt = "internal error")]
    InternalError,

    #[display(fmt = "bad request")]
    BadClientData,

    #[display(fmt = "timeout")]
    Timeout,

    #[display(fmt = "not found")]
    NotFound,
}

impl error::ResponseError for Error {
    fn error_response(&self) -> HttpResponse {
        HttpResponseBuilder::new(self.status_code())
            .set_header(header::CONTENT_TYPE, "text/html; charset=utf-8")
            .body(self.to_string())
    }

    fn status_code(&self) -> StatusCode {
        match *self {
            Error::InternalError => StatusCode::INTERNAL_SERVER_ERROR,
            Error::BadClientData => StatusCode::BAD_REQUEST,
            Error::Timeout => StatusCode::GATEWAY_TIMEOUT,
            Error::NotFound => StatusCode::NOT_FOUND,
        }
    }
}

impl From<sqlx::Error> for Error {
    fn from(error: sqlx::Error) -> Self {
        match error {
            sqlx::Error::RowNotFound => Error::NotFound,
            _ => Error::InternalError
        }
    }
}

impl Responder for TodosList {
    fn respond_to(self, _: &HttpRequest) -> HttpResponse {
        let routing = self.routing.clone();
        let result = self.todos.into_iter()
        .map(|todo| {
            let url = routing.todo_url(todo.id);
            TodoPresenter { todo, url }
        })
        .collect::<Vec<TodoPresenter>>();
        HttpResponse::Ok().json(result)
    }
}

impl Responder for TodoPresenter {
    fn respond_to(self, _: &HttpRequest) -> HttpResponse {
        HttpResponse::Ok().json(self)
    }
}

#[get("/todos")]
async fn todos_list_handler(
    pool: web::Data<PgPool>,
    routing: web::Data<RoutingService>,
) -> Result<TodosList, Error> {
    let todos = sqlx::query_as!(Todo, r#"SELECT * FROM todos ORDER BY id"#)
        .fetch_all(pool.get_ref())
        .await?;

    let routing = routing.get_ref().clone();
    Ok(TodosList { routing, todos })
}

#[get("/todos/{id:\\d+}")]
async fn todos_show_handler(
    id: web::Path<i64>,
    pool: web::Data<PgPool>,
    routing: web::Data<RoutingService>,
) -> Result<TodoPresenter, Error> {
    let todo = sqlx::query_as!(Todo, r#"SELECT * FROM todos WHERE id = $1"#, *id)
        .fetch_one(pool.get_ref())
        .await?;

    let url = routing.todo_url(*id);
    Ok(TodoPresenter { todo, url })
}

#[post("/todos")]
async fn create_todo_handler(
    pool: web::Data<PgPool>,
    todo: web::Json<NewTodo>,
    routing: web::Data<RoutingService>,
) -> Result<TodoPresenter, Error> {
    let title = &todo.title;
    let order = todo.order.unwrap_or(0);
    let todo = sqlx::query_as!(Todo, r#"INSERT INTO todos (title, "order") VALUES($1, $2) RETURNING id, title, completed, "order""#, title, order)
        .fetch_one(pool.get_ref())
        .await?;

    let url = routing.todo_url(todo.id);
    Ok(TodoPresenter { todo, url })
}

#[patch("/todos/{id:\\d+}")]
async fn patch_todo_handler(
    id: web::Path<i64>,
    pool: web::Data<PgPool>,
    update_todo: web::Json<UpdateTodo>,
    routing: web::Data<RoutingService>,
) -> Result<TodoPresenter, Error> {
    let mut todo = sqlx::query_as!(Todo, r#"SELECT * FROM todos WHERE id = $1"#, *id)
        .fetch_one(pool.get_ref())
        .await?;

    if let Some(title) = &update_todo.title {
        todo.title = title.clone();
    }
    if let Some(completed) = update_todo.completed {
        todo.completed = completed;
    }
    if let Some(order) = update_todo.order {
        todo.order = order;
    }
    let todo = sqlx::query_as!(Todo, r#"UPDATE todos SET title = $1, completed = $2, "order" = $3 WHERE id = $4 RETURNING id, title, completed, "order""#, todo.title, todo.completed, todo.order, todo.id)
        .fetch_one(pool.get_ref())
        .await?;

    let url = routing.todo_url(todo.id);
    Ok(TodoPresenter { todo, url })
}

#[delete("/todos")]
async fn delete_todos_handler(pool: web::Data<PgPool>) -> impl Responder {
    let result = sqlx::query!(r#"DELETE FROM todos"#)
        .execute(pool.get_ref())
        .await;

    match result {
        Ok(todo) => HttpResponse::NoContent().finish(),
        _ => HttpResponse::BadRequest().body("Error trying to delete a todo"),
    }
}

#[delete("/todos/{id:\\d+}")]
async fn delete_todo_handler(
    path: web::Path<i64>,
    pool: web::Data<PgPool>,
) -> Result<HttpResponse, Error> {
    let id: i64 = path.into_inner();
    let result = sqlx::query!(r#"DELETE FROM todos WHERE id = $1"#, id)
        .execute(pool.get_ref())
        .await?;

    Ok(HttpResponse::NoContent().finish())
}

#[derive(Debug, Clone)]
struct RoutingService {
    host: String,
    port: u16,
    scheme: String,
}

impl RoutingService {
    fn todo_url(&self, id: i64) -> String {
        // For production usage I would check if port is equal to 80 and don't insert port in such
        // case
        format!("{}://{}:{}/todos/{}", self.scheme, self.host, self.port, id)
    }
}

#[actix_web::main]
async fn main() -> Result<()> {
    env_logger::init();

    let mut listenfd = ListenFd::from_env();

    let database_url = env::var("DATABASE_URL").expect("DATABASE_URL is not set in .env file");
    let host = env::var("HOST").unwrap_or("127.0.0.1".to_owned());
    let port: u16 = env::var("PORT")
        .unwrap_or("8080".to_owned())
        .parse()
        .expect("PORT needs to be in 0-65535 range");
    let scheme = env::var("SCHEME").unwrap_or("http".to_owned());

    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await
        .unwrap();

    let routing_service = web::Data::new(RoutingService {
        host: host.clone(),
        port,
        scheme: scheme.clone(),
    });

    let mut server = HttpServer::new(move || {
        let cors = Cors::default()
            .allow_any_origin()
            .allow_any_header()
            .allow_any_method()
            .max_age(3600);
        App::new()
            /* .wrap(Logger::default())
            .wrap(Logger::new("%a %{User-Agent}i")) */
            .app_data(web::Data::new(pool.clone()))
            .app_data(routing_service.clone())
            .wrap(cors)
            .service(todos_list_handler)
            .service(create_todo_handler)
            .service(delete_todo_handler)
            .service(delete_todos_handler)
            .service(todos_show_handler)
            .service(patch_todo_handler)
    });

    server = match listenfd.take_tcp_listener(0)? {
        Some(listener) => server.listen(listener)?,
        None => server.bind(format!("{}:{}", host, port))?,
    };

    info!("Starting server");
    server.run().await?;

    Ok(())
}
