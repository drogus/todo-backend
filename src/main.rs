#[macro_use]
extern crate log;

use actix_cors::Cors;
use actix_web::{delete, get, patch, post, web, App, HttpResponse, HttpServer, Responder};
use anyhow::Result;
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

#[get("/todos")]
async fn todos_list_handler(
    pool: web::Data<PgPool>,
    routing: web::Data<RoutingService>,
) -> HttpResponse {
    let result = sqlx::query_as!(Todo, r#"SELECT * FROM todos ORDER BY id"#)
        .fetch_all(pool.get_ref())
        .await;

    match result {
        Ok(todos) => HttpResponse::Ok().json(
            todos
                .into_iter()
                .map(|todo| {
                    let url = routing.todo_url(todo.id);
                    TodoPresenter { todo, url }
                })
                .collect::<Vec<TodoPresenter>>(),
        ),
        _ => HttpResponse::BadRequest().body("Error trying to fetch todos"),
    }
}

#[get("/todos/{id:\\d+}")]
async fn todos_show_handler(
    path: web::Path<i64>,
    pool: web::Data<PgPool>,
    routing: web::Data<RoutingService>,
) -> HttpResponse {
    let id = path.into_inner();
    let result = sqlx::query_as!(Todo, r#"SELECT * FROM todos WHERE id = $1"#, id)
        .fetch_one(pool.get_ref())
        .await;

    match result {
        Ok(todo) => {
            let url = routing.todo_url(id);
            HttpResponse::Ok().json(TodoPresenter { todo, url })
        }
        _ => HttpResponse::BadRequest().body("Error trying to fetch a todo"),
    }
}

#[post("/todos")]
async fn create_todo_handler(
    pool: web::Data<PgPool>,
    new_todo: web::Json<NewTodo>,
    routing: web::Data<RoutingService>,
) -> impl Responder {
    let todo = new_todo.into_inner();
    let title = todo.title;
    let order = todo.order.unwrap_or(0);
    let result = sqlx::query_as!(Todo, r#"INSERT INTO todos (title, "order") VALUES($1, $2) RETURNING id, title, completed, "order""#, title, order)
        .fetch_one(pool.get_ref())
        .await;

    match result {
        Ok(todo) => {
            let url = routing.todo_url(todo.id);
            HttpResponse::Ok().json(TodoPresenter { todo, url })
        }
        _ => HttpResponse::BadRequest().body("Error trying to create new todo"),
    }
}

#[patch("/todos/{id:\\d+}")]
async fn patch_todo_handler(
    path: web::Path<i64>,
    pool: web::Data<PgPool>,
    update_todo: web::Json<UpdateTodo>,
    routing: web::Data<RoutingService>,
) -> impl Responder {
    let id = path.into_inner();
    let result = sqlx::query_as!(Todo, r#"SELECT * FROM todos WHERE id = $1"#, id)
        .fetch_one(pool.get_ref())
        .await;

    match result {
        Ok(mut todo) => {
            if let Some(title) = &update_todo.title {
                todo.title = title.clone();
            }
            if let Some(completed) = update_todo.completed {
                todo.completed = completed;
            }
            if let Some(order) = update_todo.order {
                todo.order = order;
            }
            let result = sqlx::query_as!(Todo, r#"UPDATE todos SET title = $1, completed = $2, "order" = $3 WHERE id = $4 RETURNING id, title, completed, "order""#, todo.title, todo.completed, todo.order, todo.id)
                .fetch_one(pool.get_ref())
                .await;
            match result {
                Ok(todo) => {
                    let url = routing.todo_url(todo.id);
                    HttpResponse::Ok().json(TodoPresenter { todo, url })
                }
                _ => HttpResponse::BadRequest().body("Error trying to create new todo"),
            }
        }
        _ => HttpResponse::BadRequest().body("Error trying to create new todo"),
    }
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
    routing: web::Data<RoutingService>,
) -> impl Responder {
    let id: i64 = path.into_inner();
    let result = sqlx::query!(r#"DELETE FROM todos WHERE id = $1"#, id)
        .execute(pool.get_ref())
        .await;

    match result {
        Ok(_query_result) => HttpResponse::NoContent().finish(),
        Err(e) => {
            dbg!(e);
            HttpResponse::BadRequest().body("Error trying to create new todo")
        }
    }
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
