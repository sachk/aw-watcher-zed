use std::{collections::HashMap, sync::Arc};

use aw_client_rust::AwClient;
use chrono::{DateTime, Local, TimeDelta};
use clap::{value_parser, Arg, Command};
use serde_json::Value;
use tokio::sync::Mutex;
use tower_lsp::{jsonrpc, lsp_types::*, Client, LanguageServer, LspService, Server};

#[derive(Default, Debug)]
struct Event {
    uri: String,
    is_write: bool,
    language: Option<String>,
}

#[derive(Debug)]
struct CurrentFile {
    uri: String,
    timestamp: DateTime<Local>,
}

struct ActivityWatchLangaugeServer {
    client: Client,
    current_file: Mutex<CurrentFile>,
    aw_client: AwClient,
    bucket_id: String,
    file_languages: Mutex<HashMap<String, String>>,
}

impl ActivityWatchLangaugeServer {
    async fn send(&self, event: Event) {
        // if isWrite is false, and file has not changed since last heartbeat,
        // and it has been less than 1 second since the last heartbeat do nothing
        const INTERVAL: TimeDelta = TimeDelta::seconds(1);

        let mut current_file = self.current_file.lock().await;
        let now = Local::now();

        if event.uri == current_file.uri
            && now - current_file.timestamp < INTERVAL
            && event.is_write
        {
            return;
        }

        let mut data = serde_json::Map::new();
        data.insert("file".to_string(), Value::String(event.uri.clone()));
        match self.client.workspace_folders().await {
            Ok(folders) => {
                if let Some(folders) = folders {
                    // ActivityWatch's API only lets us report the first folder. I think Zed only ever reports one anyway
                    if let Some(folder) = folders.first() {
                        data.insert(
                            "project".to_string(),
                            Value::String(String::from(folder.uri.clone())),
                        );
                    }
                };
            }
            Err(e) => {
                println!("Error reported when fetching workspace folders: {e:#?}")
            }
        };
        let language = match event.language {
            Some(l) => Some(l),
            None => self.file_languages.lock().await.get(&event.uri).cloned(),
        };

        if let Some(language) = language {
            data.insert("language".to_string(), Value::String(language));
        }

        // Duration 0 because heartbeats https://docs.activitywatch.net/en/latest/buckets-and-events.html#id7
        // https://github.com/ActivityWatch/aw-watcher-vscode/blob/36093d4ac133f04363f144bdfefa4523f8e8f25f/src/extension.ts#L139
        let aw_event = aw_client_rust::Event::new(now.to_utc(), TimeDelta::zero(), data);

        const PULSETIME: f64 = 60_f64;
        if let Err(e) = self
            .aw_client
            .heartbeat(&self.bucket_id, &aw_event, PULSETIME)
            .await
        {
            println!("Recieved error trying to send a heartbeat to the server: {e:?}");
        }

        current_file.uri = event.uri;
        current_file.timestamp = now;
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for ActivityWatchLangaugeServer {
    async fn initialize(&self, _: InitializeParams) -> jsonrpc::Result<InitializeResult> {
        Ok(InitializeResult {
            server_info: Some(ServerInfo {
                name: env!("CARGO_PKG_NAME").to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::INCREMENTAL,
                )),
                ..Default::default()
            },
        })
    }

    async fn initialized(&self, _params: InitializedParams) {
        self.client
            .log_message(
                MessageType::INFO,
                "ActivityWatch language server initialized",
            )
            .await;
    }

    async fn shutdown(&self) -> jsonrpc::Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let event = Event {
            uri: params.text_document.uri[url::Position::BeforeUsername..].to_string(),
            is_write: false,
            language: Some(params.text_document.language_id.clone()),
        };

        self.file_languages
            .lock()
            .await
            .insert(event.uri.clone(), params.text_document.language_id);

        // TODO: keep tabs on whether or not to do this
        // self.send(event).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let event = Event {
            uri: params.text_document.uri[url::Position::BeforeUsername..].to_string(),
            is_write: false,
            language: None,
        };

        self.send(event).await;
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        let event = Event {
            uri: params.text_document.uri[url::Position::BeforeUsername..].to_string(),
            is_write: true,
            language: None,
        };

        self.send(event).await;
    }
}

#[tokio::main]
async fn main() {
    let matches = Command::new("activitywatch_ls")
        .version(env!("CARGO_PKG_VERSION"))
        .author("Sacha Korban <sk@sachk.com>")
        .about("A simple ActivityWatch language server watcher")
        .arg(
            Arg::new("host")
                .short('a')
                .long("host")
                .help("The host of the ActivityWatch server to connect to")
                .required(false)
                .default_value("localhost"),
        )
        .arg(
            Arg::new("port")
                .value_parser(value_parser!(u16))
                .short('p')
                .long("port")
                .help("The ActivityWatch server port to connect to on the host")
                .required(false)
                .default_value("5600"),
        )
        .get_matches();

    // Note that AwClient does not support https
    // TODO: this sucks and i hate the alternatives too lol
    let host: &String = matches.get_one("host").unwrap();
    let port: &u16 = matches.get_one("port").unwrap();

    let aw_client = match AwClient::new(host, *port, "aw-watcher-zed") {
        Ok(c) => c,
        Err(e) => {
            println!("Could not connect to ActivityWatch Server, recieved error {e:?}");
            return;
        }
    };

    let bucket_id = format!("test-client-bucket_{}", aw_client.hostname);
    if let Err(e) = aw_client
        .create_bucket_simple(&bucket_id, "app.editor.activity")
        .await
    {
        println!("Could not create ActivityWatch bucket, recieved error {e:?}");
        return;
    };

    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(|client| {
        Arc::new(ActivityWatchLangaugeServer {
            client,
            current_file: Mutex::new(CurrentFile {
                uri: String::new(),
                timestamp: Local::now(),
            }),
            aw_client,
            bucket_id,
            file_languages: Mutex::new(HashMap::new()),
        })
    });

    Server::new(stdin, stdout, socket).serve(service).await;
}
