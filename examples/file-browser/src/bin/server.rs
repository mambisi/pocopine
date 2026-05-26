#[cfg(not(target_arch = "wasm32"))]
#[tokio::main]
async fn main() -> std::io::Result<()> {
    use file_browser_example as _;
    use pocopine_logging::init_default;
    use pocopine_server::{
        axum::{
            extract::Path,
            http::{
                header::{CONTENT_DISPOSITION, CONTENT_TYPE},
                HeaderMap, HeaderValue, StatusCode,
            },
            response::{IntoResponse, Response},
            routing::get,
            Router,
        },
        static_files,
        tower_http::services::ServeFile,
        Server,
    };
    use pocopine_storage::storage_tus_server_plugin;

    init_default().map_err(std::io::Error::other)?;

    async fn download_file(Path(file_id): Path<String>) -> Response {
        let path = match file_browser_example::download_file_path(&file_id) {
            Ok(path) => path,
            Err(err) => return (StatusCode::BAD_REQUEST, err.to_string()).into_response(),
        };
        let bytes = match tokio::fs::read(path).await {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return (StatusCode::NOT_FOUND, "file not found").into_response();
            }
            Err(err) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("read file: {err}"),
                )
                    .into_response();
            }
        };

        let mut headers = HeaderMap::new();
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static(file_browser_example::content_type_for_id(&file_id)),
        );
        match HeaderValue::from_str(&file_browser_example::content_disposition_for_id(&file_id)) {
            Ok(value) => {
                headers.insert(CONTENT_DISPOSITION, value);
            }
            Err(err) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("build download header: {err}"),
                )
                    .into_response();
            }
        }

        (StatusCode::OK, headers, bytes).into_response()
    }

    let storage = file_browser_example::storage_server().map_err(std::io::Error::other)?;
    let static_dir =
        std::env::var("POCOPINE_DIST").unwrap_or_else(|_| env!("CARGO_MANIFEST_DIR").to_string());
    let index_path = format!("{static_dir}/index.html");
    let static_service = static_files(&static_dir).fallback(ServeFile::new(index_path));
    let router = Router::new()
        .route("/api/files/download/:file_id", get(download_file))
        .fallback_service(static_service);

    let addr = std::env::var("PORT")
        .map(|port| format!("0.0.0.0:{port}"))
        .or_else(|_| std::env::var("POCOPINE_FILE_BROWSER_ADDR"))
        .unwrap_or_else(|_| "127.0.0.1:3024".to_string());
    tracing::info!(target: "pocopine.log", %addr, "serving file browser example");
    Server::new(router)
        .plugin(storage_tus_server_plugin(storage))
        .serve(addr.as_str())
        .await
}

#[cfg(target_arch = "wasm32")]
fn main() {}
