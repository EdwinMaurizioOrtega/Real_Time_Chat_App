use std::collections::HashMap; // Importa HashMap para almacenar usuarios.
use std::sync::{ // Importa tipos de sincronización.
                 atomic::{AtomicUsize, Ordering}, // Importa AtomicUsize para contar usuarios.
                 Arc, // Importa Arc para compartir datos entre threads.
};

use futures::{FutureExt, StreamExt}; // Importa tipos y funciones para trabajar con futures.
use sqlx::PgPool; // Importa PgPool para gestionar la conexión a la base de datos PostgreSQL.
use sqlx::postgres::PgPoolOptions; // Importa opciones específicas de PostgreSQL para PgPool.
use tokio::sync::{mpsc, RwLock}; // Importa tipos de sincronización de Tokio.
use tokio_stream::wrappers::UnboundedReceiverStream; // Importa UnboundedReceiverStream para manejar streams sin límite.
use warp::ws::{Message, WebSocket}; // Importa tipos relacionados con WebSocket de la biblioteca warp.
use warp::Filter; // Importa el tipo Filter para definir filtros en la aplicación warp.

static NEXT_USER_ID: AtomicUsize = AtomicUsize::new(1); // Define un contador atómico para asignar IDs a los usuarios.
static INDEX_HTML: &str = std::include_str!("../static/index.html"); // Carga el contenido HTML de index.html.

// Define un tipo alias para representar a los usuarios del chat.
type Users = Arc<RwLock<HashMap<usize, mpsc::UnboundedSender<Result<Message, warp::Error>>>>>;

// Función principal del programa, marcada como async para permitir el uso de await.
#[tokio::main]
async fn main() {
    // Establece la conexión a la base de datos PostgreSQL.
    let pool = PgPoolOptions::new()
        .connect("postgres://postgres:ededed@localhost:5432/califik")
        .await
        .expect("Failed to connect to database");

    // Inicializa el mapa de usuarios y crea un filtro para inyectar dependencias en las rutas.
    let users = Users::default();
    let users = warp::any().map(move || (users.clone(), pool.clone()));

    // Define una ruta para el chat WebSocket.
    let chat = warp::path("chat")
        .and(warp::ws())
        .and(users)
        .map(|ws: warp::ws::Ws, (users, pool)| {
            ws.on_upgrade(move |socket| user_connected(socket, users, pool))
        });

    // Define una ruta para la página de inicio.
    let index = warp::path::end().map(|| warp::reply::html(INDEX_HTML));

    // Combina las rutas de la página de inicio y el chat.
    let routes = index.or(chat);

    // Inicia el servidor warp en la dirección 127.0.0.1:8000.
    warp::serve(routes).run(([127, 0, 0, 1], 8000)).await;
}

// Función asincrónica para manejar la conexión de un usuario al chat.
async fn user_connected(ws: WebSocket, users: Users, pool: PgPool) {
    // Asigna un ID único al usuario.
    let my_id = NEXT_USER_ID.fetch_add(1, Ordering::Relaxed);

    // Muestra un mensaje en la consola sobre el nuevo usuario.
    eprintln!("new chat user: {}", my_id);

    // Separa el WebSocket en dos canales: envío y recepción.
    let (user_ws_tx, mut user_ws_rx) = ws.split();

    // Crea un canal sin límite para enviar mensajes al usuario.
    let (tx, rx) = mpsc::unbounded_channel();
    let rx = UnboundedReceiverStream::new(rx);

    // Crea una tarea para reenviar mensajes del canal al WebSocket del usuario.
    tokio::task::spawn(rx.forward(user_ws_tx).map(|result| {
        if let Err(e) = result {
            eprintln!("websocket send error: {}", e);
        }
    }));

    // Añade el canal de envío del usuario al mapa de usuarios.
    users.write().await.insert(my_id, tx);

    // Clona el mapa de usuarios para poder usarlo dentro del bucle de manejo de mensajes.
    let users2 = users.clone();

    // Maneja los mensajes recibidos del usuario mientras la conexión está activa.
    while let Some(result) = user_ws_rx.next().await {
        let msg = match result {
            Ok(msg) => msg,
            Err(e) => {
                eprintln!("websocket error(uid={}): {}", my_id, e);
                break;
            }
        };
        // Procesa el mensaje recibido.
        user_message(my_id, msg, &users, &pool).await;
    }

    // Llama a la función para manejar la desconexión del usuario.
    user_disconnected(my_id, &users2).await;
}

// Función asincrónica para procesar los mensajes de los usuarios.
async fn user_message(my_id: usize, msg: Message, users: &Users, pool: &PgPool) {
    // Verifica si el mensaje es de texto y lo convierte si es necesario.
    let msg = if let Ok(s) = msg.to_str() {
        s
    } else {
        return;
    };

    // Inserta el mensaje en la base de datos PostgreSQL.
    if let Err(err) = save_message_to_db(my_id, msg, &pool).await {
        eprintln!("Failed to save message to database: {}", err);
    }

    // Formatea el mensaje con el ID de usuario y lo muestra en la consola.
    let new_msg = format!("<User#{}>: {}", my_id, msg);
    println!("{}", new_msg);

    // Envía el mensaje a todos los usuarios conectados excepto al remitente.
    for (&uid, tx) in users.read().await.iter() {
        if my_id != uid {
            if let Err(_disconnected) = tx.send(Ok(Message::text(new_msg.clone()))) {
                // No se hace nada aquí, la función user_disconnected se encarga de ello.
            }
        }
    }
}

// Función asincrónica para manejar la desconexión de un usuario.
async fn user_disconnected(my_id: usize, users: &Users) {
    // Muestra un mensaje en la consola sobre el usuario desconectado.
    eprintln!("good bye user: {}", my_id);
    // Elimina al usuario del mapa de usuarios.
    users.write().await.remove(&my_id);
}

// Función asincrónica para guardar un mensaje en la base de datos PostgreSQL.
async fn save_message_to_db(user_id: usize, message: &str, pool: &PgPool) -> Result<(), sqlx::Error> {
    // Ejecuta una consulta SQL para insertar el mensaje en la base de datos.
    sqlx::query(
        r#"
        INSERT INTO messages (user_id, message)
        VALUES ($1, $2)
        "#,
    )
        .bind(user_id as i32)
        .bind(message)
        .execute(pool)
        .await?;

    Ok(())
}
